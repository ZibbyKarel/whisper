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

use whisper_core::error::SttError;
use whisper_core::ports::{CleanupEngine, NoopVad, SttEngine, Vad};
use whisper_core::types::{CleanupCtx, SttOpts, TranscriptRaw};

use whisper_iface::daemon::config::Config;
use whisper_iface::daemon::state::{AppState, EnginePhase};
use whisper_iface::daemon::worker::{BuildCtx, EngineBuilder, ResidentEngines};
use whisper_iface::{build_router, build_state};

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
            cleanup_model_label: None,
            cleanup_load_error: None,
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
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
            cleanup_model_label: Some("fake-cleanup".to_string()),
            cleanup_load_error: None,
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
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
            cleanup_model_label: None,
            cleanup_load_error: None,
            vad_builder: Box::new(|| Ok(Box::new(NoopVad::default()) as Box<dyn Vad>)),
        })
    });
    (builder, started_rx, release_tx)
}

/// Build a daemon state + router with the given config and engine builder, then block until the
/// worker reports the engines are ready (fake engines load instantly).
pub fn harness(config: Config, builder: EngineBuilder) -> (Arc<AppState>, Router) {
    let state = build_state(config, None, builder).expect("build state");
    // Wait for the worker thread to finish its (instant) model-load phase.
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
    let router = build_router(state.clone());
    (state, router)
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
