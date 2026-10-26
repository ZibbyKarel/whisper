//! Shared daemon state (`ARCHITECTURE.md` §4, D-0011). Everything the async axum handlers
//! and the synchronous engine worker thread need to see is here behind `Arc<AppState>`.
//!
//! The actor seam (D-0011): the `!Send` engines live on one worker thread; handlers never
//! touch them. They speak to the worker through `cmd_tx` (a bounded tokio channel) and observe
//! results through the `store`, `waiters` (one-shot replies for the blocking `/v1/transcribe`),
//! and the `events` broadcast (fan-out for `WS /v1/events`). The single-active-session invariant
//! is a `Mutex<Option<String>>` gate (`active`), which yields the structured `409 busy` of §3.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, oneshot};

use whisper_core::error::AudioError;
use whisper_core::pipeline::CleanupMode;
use whisper_core::ports::AudioSource;
use whisper_core::session::CancellationToken;
use whisper_core::vad::VadConfig;

use super::api::{ApiEvent, SessionRecord};
use super::config::Config;
use super::models::DownloadRegistry;
use super::store::SessionStore;
use crate::cleanup::CleanupHealth;

/// Unix-epoch milliseconds, the timestamp unit used across the `/v1` contract.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A live capture factory built on the worker thread (D-0012). It is `Send` (it crosses into the
/// worker over the command channel) but the [`AudioSource`] it produces — and the cpal stream
/// *that* produces — stay on the worker thread. Eager-opening inside the factory lets a
/// permission/device error surface where the worker can finalize the session cleanly.
pub type AudioSourceFactory = Box<dyn FnOnce() -> Result<Box<dyn AudioSource>, AudioError> + Send>;

/// The audio a [`Job`] runs on: pre-decoded PCM (headless `audio_b64`/`path`, Phase 4) or a live
/// mic source built on the worker thread (`{"source":"mic"}`, D-0012).
pub enum JobAudio {
    Pcm(Vec<f32>),
    Live(AudioSourceFactory),
}

/// Which [`TextSink`](whisper_core::ports::TextSink) delivers a session's final text. `Paste`
/// only differs from `Null` when the daemon was built with the `platform` feature and a
/// pasteboard sink is available; otherwise it degrades to `Null` (`delivery = "none"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    Null,
    Paste,
}

/// A unit of work sent to the engine worker thread. Carries owned audio (or a live source
/// factory) so the worker never touches the network or filesystem for headless input.
pub struct Job {
    pub session_id: String,
    pub audio: JobAudio,
    /// Per-request cleanup override; `None` means "use the configured mode".
    pub cleanup_mode: Option<CleanupMode>,
    pub token: CancellationToken,
    /// Which delivery sink to build for this session (`ARCHITECTURE.md` §5, D-0012).
    pub sink: SinkKind,
}

/// Graceful stop signal for a live capture session (D-0012). Cloned into the capture source so a
/// `POST /v1/sessions/{id}/stop` ends recording *and lets the pipeline finish* (STT/cleanup/
/// deliver) — as opposed to `cancel`, which discards via the [`CancellationToken`].
#[derive(Clone, Default)]
pub struct CaptureControl(Arc<AtomicBool>);

impl CaptureControl {
    pub fn new() -> Self {
        Self::default()
    }
    /// Signal the capture to stop; the source drains buffered audio, then reports end-of-stream.
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// The shared flag to hand to the capture source.
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

/// Context passed to a [`LiveSourceProvider`] to build a mic capture factory for one session.
pub struct LiveSourceCtx {
    pub session_id: String,
    /// WS event sink, so the capture can emit `audio.device_lost` / `platform.permission`.
    pub events: broadcast::Sender<super::api::ApiEvent>,
    /// Shared graceful-stop flag (see [`CaptureControl`]).
    pub stop: Arc<AtomicBool>,
}

/// Seam producing a live mic [`AudioSourceFactory`] (D-0012). `Some` only when the daemon was
/// composed with a capture backend (the `platform` feature, or a test fake); `None` ⇒ mic
/// sessions are rejected. Keeping it here (not in a handler `cfg`) lets contract tests inject a
/// fake capture backend and drive the whole live path headlessly.
pub type LiveSourceProvider = Box<dyn Fn(LiveSourceCtx) -> AudioSourceFactory + Send + Sync>;

/// Engine phase for `GET /v1/status` (`ARCHITECTURE.md` §3.2 model lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePhase {
    Loading,
    Ready,
    /// STT up, cleanup (Ollama) unhealthy — the pipeline degrades to dictionary text (`§3.2`).
    Degraded,
    /// STT failed to load — sessions are rejected with a structured error.
    Unavailable,
}

impl EnginePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            EnginePhase::Loading => "loading",
            EnginePhase::Ready => "ready",
            EnginePhase::Degraded => "degraded",
            EnginePhase::Unavailable => "unavailable",
        }
    }
}

/// Resident-model readiness snapshot for `/v1/status`.
#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub phase: EnginePhase,
    pub stt_ready: bool,
    pub stt_model: Option<String>,
    pub stt_error: Option<String>,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            phase: EnginePhase::Loading,
            stt_ready: false,
            stt_model: None,
            stt_error: None,
        }
    }
}

/// The `/v1/dictionary` state (D-0010 public contract): the raw JSON document plus the file it
/// is persisted to (if any). The worker rebuilds a `whisper_core::dictionary::Dictionary` from
/// this JSON per job, so edits take effect on the next session.
pub struct DictionaryState {
    pub json: serde_json::Value,
    pub path: Option<PathBuf>,
}

impl DictionaryState {
    pub fn empty() -> Self {
        Self {
            json: serde_json::json!({ "pairs": [] }),
            path: None,
        }
    }
}

/// Everything shared between the axum handlers and the engine worker.
pub struct AppState {
    pub started: Instant,
    pub version: String,
    pub events: broadcast::Sender<ApiEvent>,
    pub cmd_tx: mpsc::Sender<Job>,
    /// Single-active-session gate: `Some(session_id)` while a session runs; the source of the
    /// `409 busy` decision (`ARCHITECTURE.md` §3, D-0011).
    pub active: Mutex<Option<String>>,
    /// One-shot reply channels for blocking `/v1/transcribe` callers awaiting a final record.
    pub waiters: Mutex<HashMap<String, oneshot::Sender<SessionRecord>>>,
    /// Per-session cancellation tokens for `stop`/`cancel`.
    pub cancels: Mutex<HashMap<String, CancellationToken>>,
    /// Per-live-session graceful-stop controls (D-0012). Present only for mic sessions.
    pub captures: Mutex<HashMap<String, CaptureControl>>,
    /// Capture backend (`Some` with the `platform` feature or a test fake, else mic → 409/400).
    pub live_source: Option<LiveSourceProvider>,
    pub config: RwLock<Config>,
    pub config_path: Option<PathBuf>,
    pub dictionary: RwLock<DictionaryState>,
    /// Effective STT language, shared with the STT adapter wrapper so `/v1/config` language
    /// edits reach per-segment decoding without rebuilding the model.
    pub lang: std::sync::Arc<RwLock<String>>,
    /// Effective VAD tuning, shared with the per-session `vad_builder` so `/v1/config` `[vad]`
    /// edits reach the next session's VAD without a restart (D-0008).
    pub vad: std::sync::Arc<RwLock<VadConfig>>,
    pub store: SessionStore,
    pub status: RwLock<EngineStatus>,
    /// Effective cleanup model tag (empty = cleanup off), shared with the Ollama cleanup engine
    /// so a `/v1/config` model edit applies to the next utterance (D-0017).
    pub cleanup_model: Arc<RwLock<String>>,
    /// Live cleanup readiness written by the cleanup engine; `/v1/status` reads it per request.
    pub cleanup_health: Arc<RwLock<CleanupHealth>>,
    /// App-support data dir (`ARCHITECTURE.md` §6, D-0014) — the root that model provisioning
    /// (D-0015) downloads into (`data_dir/models/<filename>`), mirroring `resolve_model_paths`'s
    /// rebase target.
    pub data_dir: PathBuf,
    /// In-flight model downloads (D-0015): one active download per catalog model id.
    pub model_downloads: Arc<DownloadRegistry>,
    id_counter: AtomicU64,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: String,
        events: broadcast::Sender<ApiEvent>,
        cmd_tx: mpsc::Sender<Job>,
        config: Config,
        config_path: Option<PathBuf>,
        dictionary: DictionaryState,
        lang: std::sync::Arc<RwLock<String>>,
        vad: std::sync::Arc<RwLock<VadConfig>>,
        store: SessionStore,
        live_source: Option<LiveSourceProvider>,
        data_dir: PathBuf,
    ) -> Self {
        let cleanup_model = Arc::new(RwLock::new(config.cleanup.model.clone()));
        Self {
            started: Instant::now(),
            version,
            events,
            cmd_tx,
            active: Mutex::new(None),
            waiters: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            captures: Mutex::new(HashMap::new()),
            live_source,
            config: RwLock::new(config),
            config_path,
            dictionary: RwLock::new(dictionary),
            lang,
            vad,
            store,
            status: RwLock::new(EngineStatus::default()),
            cleanup_model,
            cleanup_health: Arc::new(RwLock::new(CleanupHealth::default())),
            data_dir,
            model_downloads: Arc::new(DownloadRegistry::default()),
            id_counter: AtomicU64::new(0),
        }
    }

    /// Emit an event to all WS subscribers. Errors (no subscribers) are ignored — events are
    /// best-effort by contract (`ARCHITECTURE.md` §4).
    pub fn emit(&self, event: ApiEvent) {
        let _ = self.events.send(event);
    }

    /// Mint a process-unique session id.
    pub fn new_session_id(&self) -> String {
        let n = self.id_counter.fetch_add(1, Ordering::Relaxed);
        format!("sess-{}-{}", now_ms(), n)
    }

    /// Try to become the single active session. Returns `false` if one is already active
    /// (the caller then responds `409 busy`).
    pub fn try_begin(&self, session_id: &str) -> bool {
        let mut active = self.active.lock().expect("active mutex");
        if active.is_some() {
            false
        } else {
            *active = Some(session_id.to_string());
            true
        }
    }

    /// Clear the active-session gate (called by the worker after a record is finalized, so a
    /// new session is only admitted once the result is retrievable).
    pub fn end_active(&self) {
        *self.active.lock().expect("active mutex") = None;
    }

    pub fn active_session(&self) -> Option<String> {
        self.active.lock().expect("active mutex").clone()
    }

    pub fn register_waiter(&self, session_id: &str, tx: oneshot::Sender<SessionRecord>) {
        self.waiters
            .lock()
            .expect("waiters mutex")
            .insert(session_id.to_string(), tx);
    }

    pub fn take_waiter(&self, session_id: &str) -> Option<oneshot::Sender<SessionRecord>> {
        self.waiters
            .lock()
            .expect("waiters mutex")
            .remove(session_id)
    }

    pub fn register_cancel(&self, session_id: &str, token: CancellationToken) {
        self.cancels
            .lock()
            .expect("cancels mutex")
            .insert(session_id.to_string(), token);
    }

    pub fn get_cancel(&self, session_id: &str) -> Option<CancellationToken> {
        self.cancels
            .lock()
            .expect("cancels mutex")
            .get(session_id)
            .cloned()
    }

    pub fn take_cancel(&self, session_id: &str) -> Option<CancellationToken> {
        self.cancels
            .lock()
            .expect("cancels mutex")
            .remove(session_id)
    }

    pub fn register_capture(&self, session_id: &str, control: CaptureControl) {
        self.captures
            .lock()
            .expect("captures mutex")
            .insert(session_id.to_string(), control);
    }

    pub fn get_capture(&self, session_id: &str) -> Option<CaptureControl> {
        self.captures
            .lock()
            .expect("captures mutex")
            .get(session_id)
            .cloned()
    }

    pub fn take_capture(&self, session_id: &str) -> Option<CaptureControl> {
        self.captures
            .lock()
            .expect("captures mutex")
            .remove(session_id)
    }
}
