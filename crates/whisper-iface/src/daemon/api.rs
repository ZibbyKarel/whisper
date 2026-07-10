//! The `/v1` JSON contract (`ARCHITECTURE.md` §4, D-0009): every request body, response
//! body, WebSocket event, and error envelope lives here so the public interface is defined
//! in one place. All text fields are UTF-8 `String`s — Czech diacritics round-trip byte-exact
//! through serde (`ARCHITECTURE.md` §3.3).

use serde::{Deserialize, Serialize};

use whisper_core::pipeline::{CleanupMode, CleanupSkipReason};

/// Cleanup gating mode on the wire (`CleanupMode` in the core, D-0005). Defaults to `Auto`, the
/// skip-gate default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupModeDto {
    Always,
    Never,
    #[default]
    Auto,
}

impl From<CleanupModeDto> for CleanupMode {
    fn from(m: CleanupModeDto) -> Self {
        match m {
            CleanupModeDto::Always => CleanupMode::Always,
            CleanupModeDto::Never => CleanupMode::Never,
            CleanupModeDto::Auto => CleanupMode::Auto,
        }
    }
}

impl From<CleanupMode> for CleanupModeDto {
    fn from(m: CleanupMode) -> Self {
        match m {
            CleanupMode::Always => CleanupModeDto::Always,
            CleanupMode::Never => CleanupModeDto::Never,
            CleanupMode::Auto => CleanupModeDto::Auto,
        }
    }
}

/// Map a core [`CleanupSkipReason`] to its stable wire string.
pub fn skip_reason_str(reason: CleanupSkipReason) -> &'static str {
    match reason {
        CleanupSkipReason::ModeDisabled => "mode_disabled",
        CleanupSkipReason::GateClean => "gate_clean",
        CleanupSkipReason::NoEngineConfigured => "no_engine_configured",
    }
}

/// Body for `POST /v1/transcribe` and `POST /v1/sessions`. Exactly one audio source must be
/// given: inline base64 WAV bytes (`audio_b64`) or a local WAV `path` (`ARCHITECTURE.md` §4:
/// headless audio for Phase 4; the mic port arrives in Phase 5).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioRequest {
    #[serde(default)]
    pub audio_b64: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// Per-request override of the configured cleanup gating mode.
    #[serde(default)]
    pub cleanup_mode: Option<CleanupModeDto>,
}

/// Terminal state of a session (`ARCHITECTURE.md` §3.2 FSM exits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatusDto {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Whether the LLM cleanup pass ran, and if not, why (`ARCHITECTURE.md` §3.2 cleanup gate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupInfo {
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

/// Per-phase latencies in milliseconds (`ARCHITECTURE.md` §3.4 latency budget). `stt_ms`
/// spans capture+VAD+decode up to the raw transcript; `cleanup_ms` the gate/LLM pass;
/// `deliver_ms` the sink hand-off; `total_ms` the whole session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyInfo {
    pub stt_ms: u64,
    pub cleanup_ms: u64,
    pub deliver_ms: u64,
    pub total_ms: u64,
}

/// The authoritative, persisted result of a session (`ARCHITECTURE.md` §4: retrievable via
/// `GET /v1/sessions/{id}` even after a WS client disconnects). Also the `POST /v1/transcribe`
/// response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub status: SessionStatusDto,
    /// Final delivered text on success; best partial text on cancel/failure.
    pub text: String,
    pub lang: String,
    pub cleanup: CleanupInfo,
    pub latency: LatencyInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at_ms: u128,
}

impl SessionRecord {
    /// A freshly admitted, not-yet-run session placeholder.
    pub fn running(session_id: String, created_at_ms: u128) -> Self {
        Self {
            session_id,
            status: SessionStatusDto::Running,
            text: String::new(),
            lang: String::new(),
            cleanup: CleanupInfo {
                applied: false,
                skip_reason: None,
            },
            latency: LatencyInfo::default(),
            error: None,
            created_at_ms,
        }
    }
}

/// `POST /v1/sessions` acknowledgement — the id to poll and to stop/cancel.
#[derive(Debug, Clone, Serialize)]
pub struct SessionAccepted {
    pub session_id: String,
    pub status: SessionStatusDto,
}

/// `GET /v1/health` body.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: String,
}

/// Readiness of one resident model (`ARCHITECTURE.md` §3.2 model lifecycle).
#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `GET /v1/status` body: engine phase, resident models, active session, and effective
/// profile (`ARCHITECTURE.md` §4).
#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    /// `loading` | `ready` | `degraded` (cleanup off) | `unavailable` (STT failed).
    pub phase: String,
    pub version: String,
    pub uptime_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session: Option<String>,
    pub stt: ModelStatus,
    pub cleanup: ModelStatus,
    pub cleanup_mode: CleanupModeDto,
    pub language: String,
    pub history_enabled: bool,
}

/// Result of a `PUT /v1/config`: which fields took effect immediately vs. which are staged
/// for the next daemon restart / model reload (`ARCHITECTURE.md` §4: "what requires reload,
/// return clearly").
#[derive(Debug, Clone, Serialize)]
pub struct ConfigUpdateResponse {
    pub applied: Vec<String>,
    pub staged: Vec<String>,
    pub config: super::config::Config,
}

/// One event on `WS /v1/events` (`ARCHITECTURE.md` §4). Best-effort: a lagging client may miss
/// events; the authoritative result is `GET /v1/sessions/{id}`. Tagged by `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiEvent {
    ModelLoading {
        model: String,
        ts_ms: u128,
    },
    ModelReady {
        model: String,
        ts_ms: u128,
    },
    ModelLoadFailed {
        model: String,
        error: String,
        ts_ms: u128,
    },
    SessionStarted {
        session_id: String,
        ts_ms: u128,
    },
    PartialTranscript {
        session_id: String,
        segment_text: String,
        ts_ms: u128,
    },
    RawTranscript {
        session_id: String,
        text: String,
        lang: String,
        ts_ms: u128,
    },
    DictionaryApplied {
        session_id: String,
        text: String,
        ts_ms: u128,
    },
    CleanupSkipped {
        session_id: String,
        reason: String,
        ts_ms: u128,
    },
    Cleaned {
        session_id: String,
        text: String,
        ts_ms: u128,
    },
    Delivered {
        session_id: String,
        text: String,
        ts_ms: u128,
    },
    Cancelled {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_text: Option<String>,
        ts_ms: u128,
    },
    Failed {
        session_id: String,
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_text: Option<String>,
        ts_ms: u128,
    },
}

/// Structured error envelope returned for every non-2xx response (`ARCHITECTURE.md` §3:
/// "structured busy error", §3.2 model-load rejection). `code` is a stable machine string.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}
