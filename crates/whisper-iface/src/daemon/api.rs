//! The `/v1` JSON contract (`ARCHITECTURE.md` §4, D-0009): every request body, response
//! body, WebSocket event, and error envelope lives here so the public interface is defined
//! in one place. All text fields are UTF-8 `String`s — Czech diacritics round-trip byte-exact
//! through serde (`ARCHITECTURE.md` §3.3).

use serde::{Deserialize, Serialize};

use whisper_core::pipeline::{CleanupMode, CleanupSkipReason};

use super::models::DownloadPhase;

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
        CleanupSkipReason::EngineFailed => "engine_failed",
    }
}

/// Live capture source (`ARCHITECTURE.md` §5, D-0012). Only `mic` exists in v1; it selects a live
/// microphone session instead of inline/file audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Mic,
}

/// Body for `POST /v1/transcribe` and `POST /v1/sessions`. Provide exactly one audio input:
/// inline base64 WAV bytes (`audio_b64`), a local WAV `path` (`ARCHITECTURE.md` §4, headless), or
/// `"source":"mic"` for a live capture session (`POST /v1/sessions` only, D-0012).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioRequest {
    #[serde(default)]
    pub audio_b64: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// Live capture source; mutually exclusive with `audio_b64`/`path`.
    #[serde(default)]
    pub source: Option<SourceKind>,
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
    pub created_at_ms: u64,
}

impl SessionRecord {
    /// A freshly admitted, not-yet-run session placeholder.
    pub fn running(session_id: String, created_at_ms: u64) -> Self {
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

/// `GET /v1/platform/permissions` body (`ARCHITECTURE.md` §5, D-0016): current macOS TCC grant
/// status for the two permissions the hotkey/paste path needs, so a UI can detect a missing
/// grant *proactively* (e.g. after an ad-hoc-signed reinstall drops it) instead of waiting for
/// a downstream failure that may never surface (`rdev::listen` can return `Ok` with the grant
/// missing — see D-0016). Under a non-`platform` build both fields are `"unknown"` — there is no
/// macOS adapter to query.
#[derive(Debug, Clone, Serialize)]
pub struct PlatformPermissionsDto {
    /// `"granted" | "denied" | "unknown"` — Input Monitoring (the global hotkey listener).
    pub input_monitoring: String,
    /// `"granted" | "denied" | "unknown"` — Accessibility (synthesized paste).
    pub accessibility: String,
}

/// `GET /v1/status` body: engine phase, resident models, active session, and effective
/// profile (`ARCHITECTURE.md` §4).
#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    /// `loading` | `ready` | `degraded` (cleanup off) | `unavailable` (STT failed).
    pub phase: String,
    pub version: String,
    pub uptime_ms: u64,
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

// ---------------------------------------------------------------------------
// Model provisioning (D-0015): GET /v1/models, POST/DELETE /v1/models/{id}/download
// ---------------------------------------------------------------------------

/// In-flight download progress, embedded in [`ModelEntryDto`] and the `model.download` WS
/// event.
#[derive(Debug, Clone, Serialize)]
pub struct ModelProgressDto {
    pub downloaded: u64,
    pub total: u64,
}

/// One catalog model's provisioning status (`GET /v1/models` D-0015 design doc). Deliberately
/// does not expose `url`/`sha256`/`license` on the wire — those are provisioning internals, not
/// part of the operator-facing contract.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntryDto {
    pub id: String,
    /// `"stt"` | `"cleanup"`.
    pub kind: String,
    pub filename: String,
    pub required: bool,
    pub size_bytes: u64,
    pub present: bool,
    /// The resolved on-disk target path (`<data_dir>/models/<filename>`), present or not.
    pub path: String,
    pub downloading: bool,
    pub progress: Option<ModelProgressDto>,
}

/// `GET /v1/models` body (D-0015 design doc).
#[derive(Debug, Clone, Serialize)]
pub struct ModelsListResponse {
    pub models: Vec<ModelEntryDto>,
}

/// One event on `WS /v1/events` (`ARCHITECTURE.md` §4). Best-effort: a lagging client may miss
/// events; the authoritative result is `GET /v1/sessions/{id}`. Tagged by `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiEvent {
    ModelLoading {
        model: String,
        ts_ms: u64,
    },
    ModelReady {
        model: String,
        ts_ms: u64,
    },
    ModelLoadFailed {
        model: String,
        error: String,
        ts_ms: u64,
    },
    SessionStarted {
        session_id: String,
        ts_ms: u64,
    },
    PartialTranscript {
        session_id: String,
        segment_text: String,
        ts_ms: u64,
    },
    RawTranscript {
        session_id: String,
        text: String,
        lang: String,
        ts_ms: u64,
    },
    DictionaryApplied {
        session_id: String,
        text: String,
        ts_ms: u64,
    },
    CleanupSkipped {
        session_id: String,
        reason: String,
        ts_ms: u64,
    },
    Cleaned {
        session_id: String,
        text: String,
        ts_ms: u64,
    },
    Delivered {
        session_id: String,
        text: String,
        ts_ms: u64,
    },
    Cancelled {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_text: Option<String>,
        ts_ms: u64,
    },
    Failed {
        session_id: String,
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_text: Option<String>,
        ts_ms: u64,
    },
    // The three platform events below keep the dotted names `ARCHITECTURE.md` §4 lists
    // verbatim (`secure_input.blocked`, `audio.device_lost`, `platform.permission`); the
    // per-session lifecycle events above predate this and stay in their established
    // snake_case form to avoid breaking the existing contract.
    /// Injection was suppressed and text left on the clipboard (Secure Input active, D-0007).
    /// The session still completes; `owner` names the blocking app when known.
    #[serde(rename = "secure_input.blocked")]
    SecureInputBlocked {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
        ts_ms: u64,
    },
    /// The capture device dropped mid-session (e.g. a Bluetooth mic, `ARCHITECTURE.md` §3.1).
    /// Emitted alongside the session's `failed` event, which carries the partial transcript.
    #[serde(rename = "audio.device_lost")]
    AudioDeviceLost {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        reason: String,
        ts_ms: u64,
    },
    /// Throttled (~10 Hz) input-level telemetry for a live mic session (`ARCHITECTURE.md` §5,
    /// D-0013 level meter). `rms`/`peak` are post-gain linear amplitude in `[0.0, 1.0]`.
    #[serde(rename = "audio.level")]
    AudioLevel {
        session_id: String,
        rms: f32,
        peak: f32,
        ts_ms: u64,
    },
    /// A platform permission the engine needs is missing (`ARCHITECTURE.md` §5: surfaced as an
    /// event, never a blocking dialog in core). `permission` is e.g. `"microphone"`.
    #[serde(rename = "platform.permission")]
    PlatformPermission {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        permission: String,
        granted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        ts_ms: u64,
    },
    /// Model provisioning progress (D-0015 design doc): one catalog model's download, from the
    /// first `downloading` tick through `verifying` to a terminal `done`/`error`. `error` is
    /// only present when `phase == "error"` (network failure, checksum mismatch, or a user
    /// cancel, all surfaced as `error` with a distinguishing message).
    #[serde(rename = "model.download")]
    ModelDownload {
        id: String,
        downloaded: u64,
        total: u64,
        phase: DownloadPhase,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        ts_ms: u64,
    },
    /// The local Ollama server's state changed (D-0017): emitted when a `POST /v1/ollama/start`
    /// finishes, whether or not the server came up.
    #[serde(rename = "ollama.status")]
    OllamaStatus {
        installed: bool,
        running: bool,
        version: Option<String>,
        url: String,
        can_start: bool,
        ts_ms: u64,
    },
}

/// `GET /v1/ollama` body (D-0017): whether a local Ollama is installed/running at the configured
/// `cleanup.ollama_url`, and whether the daemon may start it (installed, not running, loopback URL).
#[derive(Debug, Clone, Serialize)]
pub struct OllamaStatusDto {
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
    pub url: String,
    pub can_start: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_level_event_wire_shape() {
        let ev = ApiEvent::AudioLevel {
            session_id: "sess-1".to_string(),
            rms: 0.25,
            peak: 0.5,
            ts_ms: 42,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "audio.level");
        assert_eq!(v["session_id"], "sess-1");
        assert_eq!(v["rms"], 0.25);
        assert_eq!(v["peak"], 0.5);
        assert_eq!(v["ts_ms"], 42);
        // Exactly the five documented keys, no more.
        assert_eq!(v.as_object().unwrap().len(), 5);
    }

    #[test]
    fn model_download_event_wire_shape() {
        let ev = ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 100,
            total: 574_041_195,
            phase: DownloadPhase::Downloading,
            error: None,
            ts_ms: 42,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "model.download");
        assert_eq!(v["id"], "stt-large-v3-turbo");
        assert_eq!(v["downloaded"], 100);
        assert_eq!(v["total"], 574_041_195_u64);
        assert_eq!(v["phase"], "downloading");
        // `error` is omitted (not null) when absent.
        assert!(v.get("error").is_none());
        // type, id, downloaded, total, phase, ts_ms — exactly six keys, no more.
        assert_eq!(v.as_object().unwrap().len(), 6);

        let err_ev = ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 0,
            total: 574_041_195,
            phase: DownloadPhase::Error,
            error: Some("integrity check failed".to_string()),
            ts_ms: 43,
        };
        let ev_v = serde_json::to_value(&err_ev).unwrap();
        assert_eq!(ev_v["phase"], "error");
        assert_eq!(ev_v["error"], "integrity check failed");
    }

    #[test]
    fn ollama_status_dto_wire_shape() {
        // Fixture mirrored in `whisper-ux/src/api.rs` (Task 7): drift must fail both sides.
        let running = OllamaStatusDto {
            installed: true,
            running: true,
            version: Some("0.12.3".to_string()),
            url: "http://127.0.0.1:11434".to_string(),
            can_start: false,
        };
        assert_eq!(
            serde_json::to_value(&running).unwrap(),
            serde_json::json!({
                "installed": true,
                "running": true,
                "version": "0.12.3",
                "url": "http://127.0.0.1:11434",
                "can_start": false
            })
        );
        // `version` is an explicit null when the server is not answering.
        let stopped = OllamaStatusDto {
            installed: true,
            running: false,
            version: None,
            url: "http://127.0.0.1:11434".to_string(),
            can_start: true,
        };
        let v = serde_json::to_value(&stopped).unwrap();
        assert!(v["version"].is_null());
        assert_eq!(v["can_start"], true);
        assert_eq!(v.as_object().unwrap().len(), 5);
    }

    #[test]
    fn ollama_status_event_wire_shape() {
        let ev = ApiEvent::OllamaStatus {
            installed: true,
            running: true,
            version: Some("0.12.3".to_string()),
            url: "http://127.0.0.1:11434".to_string(),
            can_start: false,
            ts_ms: 42,
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({
                "type": "ollama.status",
                "installed": true,
                "running": true,
                "version": "0.12.3",
                "url": "http://127.0.0.1:11434",
                "can_start": false,
                "ts_ms": 42
            })
        );
    }

    #[test]
    fn platform_permissions_wire_shape() {
        // Fixture mirrored in `whisper-ux/src/api.rs` (contract-mirror discipline, HANDOFF item
        // 8) — a drift here must fail the mirrored test on the ux side too.
        let dto = PlatformPermissionsDto {
            input_monitoring: "denied".to_string(),
            accessibility: "granted".to_string(),
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["input_monitoring"], "denied");
        assert_eq!(v["accessibility"], "granted");
        // Exactly the two documented keys, no more.
        assert_eq!(v.as_object().unwrap().len(), 2);
    }
}
