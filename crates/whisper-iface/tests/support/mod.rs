//! Shared harness for the fast `whisperd` contract tests: fake engines (no models, no mic, no
//! network) that drive the real actor/pipeline, plus HTTP helpers over `tower::oneshot` so the
//! router is exercised without binding a socket.

#![allow(dead_code)]

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use std::sync::atomic::{AtomicBool, Ordering};

use whisper_core::error::{AudioError, SttError};
use whisper_core::ports::{AudioSource, AudioStream, CleanupEngine, NoopVad, SttEngine, Vad};
use whisper_core::types::{AudioCfg, AudioFrame, CleanupCtx, SttOpts, TranscriptRaw};

use whisper_iface::daemon::api::ApiEvent;
use whisper_iface::daemon::config::Config;
use whisper_iface::daemon::state::{
    AppState, AudioSourceFactory, EnginePhase, LiveSourceCtx, LiveSourceProvider,
};
use whisper_iface::daemon::worker::{null_sink_builder, BuildCtx, EngineBuilder, ResidentEngines};
use whisper_iface::{build_router, build_state, build_state_with_data_dir};

/// The canonical Czech pangram — every diacritic in one sentence. Fake STT returns it so tests
/// can assert byte-exact UTF-8 round-trips through the whole API (`ARCHITECTURE.md` §3.3).
pub const CZECH_TEXT: &str = "Příliš žluťoučký kůň úpěl ďábelské ódy.";

/// STT stub that ignores its PCM and returns a fixed Czech transcript.
struct FakeStt {
    text: String,
}

impl SttEngine for FakeStt {
    fn transcribe(&self, _pcm: &[f32], _opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        Ok(TranscriptRaw {
            text: self.text.clone(),
            lang: "cs".to_string(),
            segments: Vec::new(),
        })
    }
}

/// STT stub that blocks in `transcribe` until released — used to hold a session "active" so a
/// concurrent request can observe the `409 busy` path.
pub struct GateStt {
    text: String,
    started: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl SttEngine for GateStt {
    fn transcribe(&self, _pcm: &[f32], _opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        let _ = self.started.send(());
        // Block until the test releases us.
        let _ = self.release.lock().unwrap().recv();
        Ok(TranscriptRaw {
            text: self.text.clone(),
            lang: "cs".to_string(),
            segments: Vec::new(),
        })
    }
}

/// Identity cleanup: emits a `Cleaned` event (so `cleanup.applied` is true) without altering the
/// text — enough to test the cleanup-applied contract path.
struct IdentityCleanup;

impl CleanupEngine for IdentityCleanup {
    fn clean(
        &self,
        text: &str,
        _ctx: &CleanupCtx,
    ) -> Result<String, whisper_core::error::CleanupError> {
        Ok(text.to_string())
    }
}

/// Builder producing a fake STT (fixed Czech text), a pass-through VAD, and no cleanup.
pub fn fake_builder() -> EngineBuilder {
    Box::new(|_ctx: BuildCtx| {
        Ok(ResidentEngines {
            stt: Rc::new(FakeStt {
                text: CZECH_TEXT.to_string(),
            }),
            stt_model_label: "fake-stt".to_string(),
            cleanup: None,
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
            sink_builder: null_sink_builder(),
        })
    })
}

/// Like [`fake_builder`] but with an identity cleanup engine, so `CleanupMode::Always` exercises
/// the cleanup-applied path.
pub fn fake_builder_with_cleanup() -> EngineBuilder {
    Box::new(|_ctx: BuildCtx| {
        Ok(ResidentEngines {
            stt: Rc::new(FakeStt {
                text: CZECH_TEXT.to_string(),
            }),
            stt_model_label: "fake-stt".to_string(),
            cleanup: Some(Rc::new(IdentityCleanup)),
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
            sink_builder: null_sink_builder(),
        })
    })
}

/// Builder whose STT blocks until the returned `release` sender is used. `started` fires when the
/// blocking transcribe is entered, so a test can be sure the session is active before probing.
pub fn gate_builder() -> (
    EngineBuilder,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let builder: EngineBuilder = Box::new(move |_ctx: BuildCtx| {
        Ok(ResidentEngines {
            stt: Rc::new(GateStt {
                text: CZECH_TEXT.to_string(),
                started: started_tx,
                release: Mutex::new(release_rx),
            }),
            stt_model_label: "gate-stt".to_string(),
            cleanup: None,
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
            sink_builder: null_sink_builder(),
        })
    });
    (builder, started_rx, release_tx)
}

/// A fake live capture source (no CoreAudio): yields tiny frames until the shared `stop` flag is
/// set (graceful stop), then reports end-of-stream. A safety cap guarantees termination.
struct FakeMicSource {
    stop: std::sync::Arc<AtomicBool>,
}

impl AudioSource for FakeMicSource {
    fn start(&self, _cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
        Ok(Box::new(FakeMicStream {
            stop: self.stop.clone(),
            emitted: 0,
        }))
    }
}

struct FakeMicStream {
    stop: std::sync::Arc<AtomicBool>,
    emitted: usize,
}

impl AudioStream for FakeMicStream {
    fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError> {
        if self.stop.load(Ordering::SeqCst) && self.emitted > 0 {
            return Ok(None);
        }
        if self.emitted > 5000 {
            return Ok(None); // safety cap so a test that never stops still terminates
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.emitted += 1;
        Ok(Some(vec![0.0_f32; 160]))
    }
}

/// A live-source provider that builds [`FakeMicSource`]s — injects the mic path into the actor
/// without touching hardware (D-0012 testability).
pub fn fake_live_source() -> LiveSourceProvider {
    Box::new(|ctx: LiveSourceCtx| {
        Box::new(move || Ok(Box::new(FakeMicSource { stop: ctx.stop }) as Box<dyn AudioSource>))
            as AudioSourceFactory
    })
}

/// A live-source provider whose factory emits a `platform.permission` denial and fails — models
/// the microphone-permission-denied path (D-0012).
pub fn fake_live_source_denied() -> LiveSourceProvider {
    Box::new(|ctx: LiveSourceCtx| {
        Box::new(move || {
            let _ = ctx.events.send(ApiEvent::PlatformPermission {
                session_id: Some(ctx.session_id.clone()),
                permission: "microphone".to_string(),
                granted: false,
                detail: Some("test denial".to_string()),
                ts_ms: 0,
            });
            Err(AudioError::StartFailed(
                "microphone permission denied: test".to_string(),
            ))
        }) as AudioSourceFactory
    })
}

/// A capturing delivery sink builder: records delivered text so a test can assert the paste path
/// ran (stands in for the real pasteboard sink, which needs hardware). Returns the builder plus a
/// handle to read what was delivered.
pub fn capture_sink_builder() -> (
    whisper_iface::daemon::worker::SinkBuilder,
    std::sync::Arc<Mutex<Option<String>>>,
) {
    let captured = std::sync::Arc::new(Mutex::new(None));
    let handle = captured.clone();
    let builder: whisper_iface::daemon::worker::SinkBuilder = Box::new(move |kind| match kind {
        whisper_iface::daemon::state::SinkKind::Paste => Box::new(CaptureDeliverySink {
            captured: captured.clone(),
        })
            as Box<dyn whisper_core::ports::TextSink>,
        whisper_iface::daemon::state::SinkKind::Null => {
            Box::new(NullDeliverySink) as Box<dyn whisper_core::ports::TextSink>
        }
    });
    (builder, handle)
}

struct CaptureDeliverySink {
    captured: std::sync::Arc<Mutex<Option<String>>>,
}
impl whisper_core::ports::TextSink for CaptureDeliverySink {
    fn deliver(
        &self,
        text: &str,
    ) -> Result<whisper_core::ports::DeliverOutcome, whisper_core::error::DeliverError> {
        *self.captured.lock().unwrap() = Some(text.to_string());
        Ok(whisper_core::ports::DeliverOutcome::Injected)
    }
}

struct NullDeliverySink;
impl whisper_core::ports::TextSink for NullDeliverySink {
    fn deliver(
        &self,
        _text: &str,
    ) -> Result<whisper_core::ports::DeliverOutcome, whisper_core::error::DeliverError> {
        Ok(whisper_core::ports::DeliverOutcome::Injected)
    }
}

/// A sink builder whose `Paste` sink always reports a Secure-Input clipboard-only delivery with a
/// fixed owner — models the D-0007 fallback so the daemon's `secure_input.blocked` WS mapping can
/// be asserted without hardware.
pub fn secure_input_sink_builder(owner: &str) -> whisper_iface::daemon::worker::SinkBuilder {
    let owner = owner.to_string();
    Box::new(move |_kind| {
        Box::new(SecureInputDeliverySink {
            owner: owner.clone(),
        }) as Box<dyn whisper_core::ports::TextSink>
    })
}

struct SecureInputDeliverySink {
    owner: String,
}
impl whisper_core::ports::TextSink for SecureInputDeliverySink {
    fn deliver(
        &self,
        _text: &str,
    ) -> Result<whisper_core::ports::DeliverOutcome, whisper_core::error::DeliverError> {
        Ok(whisper_core::ports::DeliverOutcome::ClipboardOnly {
            owner: Some(self.owner.clone()),
        })
    }
}

/// A [`fake_builder`]-style engine builder that also installs a custom sink builder (for delivery
/// tests). STT returns the fixed Czech text, VAD passes through, no cleanup.
pub fn fake_builder_with_sink(
    sink_builder: whisper_iface::daemon::worker::SinkBuilder,
) -> EngineBuilder {
    Box::new(move |_ctx: BuildCtx| {
        Ok(ResidentEngines {
            stt: Rc::new(FakeStt {
                text: CZECH_TEXT.to_string(),
            }),
            stt_model_label: "fake-stt".to_string(),
            cleanup: None,
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
            sink_builder,
        })
    })
}

/// Build a daemon state + router with the given config and engine builder, then block until the
/// worker reports the engines are ready (fake engines load instantly).
pub fn harness(config: Config, builder: EngineBuilder) -> (Arc<AppState>, Router) {
    harness_with_live(config, builder, None)
}

/// Like [`harness`] but with a live-capture provider injected — used to drive the mic session
/// path headlessly (no CoreAudio) via [`fake_live_source`].
pub fn harness_with_live(
    config: Config,
    builder: EngineBuilder,
    live_source: Option<LiveSourceProvider>,
) -> (Arc<AppState>, Router) {
    harness_with_config_path(config, builder, None, live_source)
}

/// Like [`harness`] but with an explicit `config_path` — used to test `PUT /v1/config` persistence
/// (e.g. that a missing parent directory is created rather than a 500).
pub fn harness_with_config_path(
    config: Config,
    builder: EngineBuilder,
    config_path: Option<std::path::PathBuf>,
    live_source: Option<LiveSourceProvider>,
) -> (Arc<AppState>, Router) {
    let state = build_state(config, config_path, builder, live_source).expect("build state");
    wait_ready(&state);
    let router = build_router(state.clone());
    (state, router)
}

/// Like [`harness`] but with an explicit model-provisioning `data_dir` (D-0015) — used by the
/// model-download contract/API tests so they never touch the real app-support directory.
pub fn harness_with_data_dir(
    config: Config,
    builder: EngineBuilder,
    data_dir: std::path::PathBuf,
) -> (Arc<AppState>, Router) {
    let state = build_state_with_data_dir(config, None, builder, None, data_dir)
        .expect("build state with data dir");
    wait_ready(&state);
    let router = build_router(state.clone());
    (state, router)
}

/// Wait for the worker thread to finish its (instant, fake-engine) model-load phase.
fn wait_ready(state: &Arc<AppState>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let phase = state.status.read().unwrap().phase;
        if phase != EnginePhase::Loading {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "engines never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A process-unique temp dir under the OS temp root, auto-cleaned on drop — the `data_dir` for
/// model-provisioning tests (D-0015). Not a `tempfile` dependency: this workspace already
/// hand-rolls the same pattern in a couple of places.
pub struct TempDataDir(pub std::path::PathBuf);

impl TempDataDir {
    pub fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "whisper-iface-data-dir-test-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A tiny valid 16 kHz mono WAV (base64) of near-silence. Content is irrelevant — the fake STT
/// ignores it — but it must be a parseable WAV with at least one non-empty frame.
pub fn tiny_wav_b64() -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(tiny_wav_bytes())
}

/// The same WAV as raw bytes, for writing to a temp file (the `path` input variant).
pub fn tiny_wav_bytes() -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
        for _ in 0..3200 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
    }
    buf.into_inner()
}

/// Perform an HTTP request against the router via `tower::oneshot` and return the status plus the
/// parsed JSON body.
pub async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    call_with_headers(router, method, uri, body, &[]).await
}

/// Like [`call`] but with extra request headers (e.g. Authorization).
pub async fn call_with_headers(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let request = match body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}
