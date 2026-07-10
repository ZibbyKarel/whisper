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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, oneshot};

use whisper_core::pipeline::CleanupMode;
use whisper_core::session::CancellationToken;

use super::api::{ApiEvent, SessionRecord};
use super::config::Config;
use super::store::SessionStore;

/// Unix-epoch milliseconds, the timestamp unit used across the `/v1` contract.
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// A unit of work sent to the engine worker thread. Carries owned PCM (already decoded from
/// the request's `audio_b64`/`path`) so the worker never touches the network or filesystem.
pub struct Job {
    pub session_id: String,
    pub pcm: Vec<f32>,
    /// Per-request cleanup override; `None` means "use the configured mode".
    pub cleanup_mode: Option<CleanupMode>,
    pub token: CancellationToken,
}

/// Engine phase for `GET /v1/status` (`ARCHITECTURE.md` §3.2 model lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePhase {
    Loading,
    Ready,
    /// STT up, cleanup LLM failed to load — the pipeline runs cleanup-off (`§3.2`).
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
    pub cleanup_ready: bool,
    pub cleanup_model: Option<String>,
    pub cleanup_error: Option<String>,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            phase: EnginePhase::Loading,
            stt_ready: false,
            stt_model: None,
            stt_error: None,
            cleanup_ready: false,
            cleanup_model: None,
            cleanup_error: None,
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
    pub config: RwLock<Config>,
    pub config_path: Option<PathBuf>,
    pub dictionary: RwLock<DictionaryState>,
    /// Effective STT language, shared with the STT adapter wrapper so `/v1/config` language
    /// edits reach per-segment decoding without rebuilding the model.
    pub lang: std::sync::Arc<RwLock<String>>,
    pub store: SessionStore,
    pub status: RwLock<EngineStatus>,
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
        store: SessionStore,
    ) -> Self {
        Self {
            started: Instant::now(),
            version,
            events,
            cmd_tx,
            active: Mutex::new(None),
            waiters: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            config: RwLock::new(config),
            config_path,
            dictionary: RwLock::new(dictionary),
            lang,
            store,
            status: RwLock::new(EngineStatus::default()),
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
}
