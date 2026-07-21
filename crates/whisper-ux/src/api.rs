//! Local mirror of the whisperd `/v1` JSON contract (`ARCHITECTURE.md` §4, D-0009).
//!
//! These `serde` structs intentionally duplicate the shapes defined in
//! `whisper-iface/src/daemon/{api.rs,config.rs}`. The UX shell is a replaceable
//! client (constitution: UX talks ONLY to the Interface Layer over HTTP/WS) and
//! must NOT depend on any engine/interface crate, so the wire types are copied
//! here rather than imported. Keep field names and `#[serde(...)]` attributes in
//! sync with the daemon; the contract tests over there are authoritative.
//!
//! All text fields are UTF-8 `String`s: Czech diacritics round-trip byte-exact.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config (GET/PUT /v1/config) — mirror of daemon/config.rs
// ---------------------------------------------------------------------------

/// Cleanup gating mode (mirror of `CleanupModeDto`, D-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupMode {
    Always,
    Never,
    #[default]
    Auto,
}

/// Delivery of a session's final text (mirror of `DeliveryMode`, D-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryMode {
    #[default]
    None,
    Paste,
}

/// Hotkey trigger mode (mirror of `HotkeyModeDto`, D-0007/D-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    #[default]
    PushToTalk,
    Toggle,
}

/// Resident model paths (mirror of `ModelsConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    pub stt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<String>,
}

/// Global hotkey configuration (mirror of `HotkeyConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub enabled: bool,
    pub combo: String,
    pub mode: HotkeyMode,
}

/// Opt-in transcript history (mirror of `HistoryConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// VAD sensitivity tuning (mirror of `VadTuning`, `[vad]` in config.toml, D-0008).
/// `Copy`/`f32` fields mean this (and therefore [`Config`]) cannot derive `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VadTuning {
    pub threshold: f32,
    pub hangover_ms: u32,
    pub max_silence_ms: u32,
}

impl Default for VadTuning {
    fn default() -> Self {
        Self {
            threshold: 0.35,
            hangover_ms: 300,
            max_silence_ms: 1_000,
        }
    }
}

/// Capture input-gain tuning (mirror of `AudioTuning`, `[audio]` in config.toml).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioTuning {
    pub agc_enabled: bool,
    pub agc_target_peak: f32,
    pub agc_max_gain: f32,
}

impl Default for AudioTuning {
    fn default() -> Self {
        Self {
            agc_enabled: true,
            agc_target_peak: 0.7,
            agc_max_gain: 12.0,
        }
    }
}

/// The full daemon configuration and the `/v1/config` body (mirror of `Config`).
///
/// `#[serde(default)]` mirrors the daemon so a config returned by a newer daemon
/// with extra fields still deserializes (unknown fields are ignored by serde by
/// default), and a partial value we send is filled in by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub cleanup_mode: CleanupMode,
    pub language: String,
    pub delivery: DeliveryMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary_path: Option<String>,
    pub models: ModelsConfig,
    pub history: HistoryConfig,
    pub hotkey: HotkeyConfig,
    pub vad: VadTuning,
    pub audio: AudioTuning,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: 8737,
            token: None,
            cleanup_mode: CleanupMode::Auto,
            language: "cs".to_string(),
            delivery: DeliveryMode::None,
            dictionary_path: None,
            models: ModelsConfig {
                stt: "models/ggml-large-v3-turbo-q5_0.bin".to_string(),
                cleanup: Some("models/Qwen3-8B-Q4_K_M.gguf".to_string()),
            },
            history: HistoryConfig::default(),
            hotkey: HotkeyConfig {
                // Daemon default is moving to a modifier-only combo with the
                // hotkey enabled out of the box (parallel change in
                // whisper-iface); mirrored here so the shell's own defaults
                // (used before the first `GET /v1/config`) stay in sync.
                enabled: true,
                combo: "Ctrl+Alt".to_string(),
                mode: HotkeyMode::PushToTalk,
            },
            vad: VadTuning::default(),
            audio: AudioTuning::default(),
        }
    }
}

/// `PUT /v1/config` response (mirror of `ConfigUpdateResponse`): which fields took
/// effect now vs. which are staged for the next daemon restart/reload.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigUpdateResponse {
    pub applied: Vec<String>,
    pub staged: Vec<String>,
    pub config: Config,
}

// ---------------------------------------------------------------------------
// Health / status
// ---------------------------------------------------------------------------

/// `GET /v1/health` body (mirror of `HealthResponse`).
#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Readiness of one resident model (mirror of `ModelStatus`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelStatus {
    pub ready: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `GET /v1/status` body (mirror of `StatusResponse`).
#[derive(Debug, Clone, Deserialize)]
pub struct StatusResponse {
    /// `loading` | `ready` | `degraded` | `unavailable`.
    pub phase: String,
    pub version: String,
    #[serde(default)]
    pub uptime_ms: u64,
    #[serde(default)]
    pub active_session: Option<String>,
    #[serde(default)]
    pub stt: ModelStatus,
    #[serde(default)]
    pub cleanup: ModelStatus,
    #[serde(default)]
    pub cleanup_mode: CleanupMode,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub history_enabled: bool,
}

// ---------------------------------------------------------------------------
// Model provisioning (GET /v1/models, POST/DELETE /v1/models/{id}/download) — D-0015
// mirror of daemon/models.rs + daemon/api.rs
// ---------------------------------------------------------------------------

/// In-flight download progress (mirror of `ModelProgressDto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ModelProgress {
    pub downloaded: u64,
    pub total: u64,
}

/// One catalog model's provisioning status (mirror of `ModelEntryDto`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    /// `"stt"` | `"cleanup"`.
    pub kind: String,
    pub filename: String,
    pub required: bool,
    pub size_bytes: u64,
    pub present: bool,
    pub path: String,
    pub downloading: bool,
    #[serde(default)]
    pub progress: Option<ModelProgress>,
}

impl ModelEntry {
    /// Human-readable size, e.g. `"547 MB"` / `"4.7 GB"` — used by every model-download
    /// surface (startup gate, Settings → Models, onboarding) so formatting stays consistent.
    #[must_use]
    pub fn size_label(&self) -> String {
        format_bytes(self.size_bytes)
    }

    /// Progress as a rounded percentage in `0..=100`, or `None` when not downloading or the
    /// daemon has not reported a total yet.
    #[must_use]
    pub fn progress_percent(&self) -> Option<u8> {
        let p = self.progress?;
        if p.total == 0 {
            return None;
        }
        Some(((p.downloaded as f64 / p.total as f64) * 100.0).round() as u8)
    }
}

fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

/// `GET /v1/models` body (mirror of `ModelsListResponse`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelsListResponse {
    pub models: Vec<ModelEntry>,
}

// ---------------------------------------------------------------------------
// Platform permissions (GET /v1/platform/permissions) — mirror of daemon/api.rs, D-0016
// ---------------------------------------------------------------------------

/// Current macOS TCC grant status (mirror of `PlatformPermissionsDto`). `"granted" | "denied" |
/// "unknown"` — `"unknown"` on a headless daemon build, or before the first successful poll.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PlatformPermissions {
    pub input_monitoring: String,
    pub accessibility: String,
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Terminal/liveness state of a session (mirror of `SessionStatusDto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// `POST /v1/sessions` acknowledgement (mirror of `SessionAccepted`).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionAccepted {
    pub session_id: String,
    pub status: SessionStatus,
}

/// Whether the LLM cleanup pass ran (mirror of `CleanupInfo`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CleanupInfo {
    pub applied: bool,
    #[serde(default)]
    pub skip_reason: Option<String>,
}

/// Per-phase latencies in ms (mirror of `LatencyInfo`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LatencyInfo {
    pub stt_ms: u64,
    pub cleanup_ms: u64,
    pub deliver_ms: u64,
    pub total_ms: u64,
}

/// The persisted result of a session (mirror of `SessionRecord`), also the
/// `stop`/`cancel`/`transcribe` response body.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub status: SessionStatus,
    pub text: String,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub cleanup: CleanupInfo,
    #[serde(default)]
    pub latency: LatencyInfo,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub created_at_ms: u64,
}

// ---------------------------------------------------------------------------
// Dictionary (GET/PUT /v1/dictionary) — public contract D-0010
// ---------------------------------------------------------------------------

/// One substitution rule (mirror of core `DictionaryPair`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryPair {
    pub from: String,
    pub to: String,
}

/// The `/v1/dictionary` body: `{"pairs":[{"from","to"}]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryFile {
    #[serde(default)]
    pub pairs: Vec<DictionaryPair>,
}

// ---------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------

/// Structured error body returned for every non-2xx response (mirror of
/// `ErrorEnvelope`).
#[derive(Debug, Clone, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// Phase of a model download (mirror of `DownloadPhase`, D-0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    Downloading,
    Verifying,
    Done,
    Error,
}

// ---------------------------------------------------------------------------
// WS /v1/events — mirror of `ApiEvent` (deserialize side only)
// ---------------------------------------------------------------------------

/// One event on `WS /v1/events` (mirror of `ApiEvent`). The `type` tag strings
/// are byte-for-byte those the daemon emits, including the three dotted platform
/// names. `audio.level` is the live-metering event added alongside this shell
/// (D-0013 consequence); the daemon side is implemented by a parallel work item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
        #[serde(default)]
        partial_text: Option<String>,
        ts_ms: u64,
    },
    Failed {
        session_id: String,
        error: String,
        #[serde(default)]
        partial_text: Option<String>,
        ts_ms: u64,
    },
    #[serde(rename = "secure_input.blocked")]
    SecureInputBlocked {
        session_id: String,
        #[serde(default)]
        owner: Option<String>,
        ts_ms: u64,
    },
    #[serde(rename = "audio.device_lost")]
    AudioDeviceLost {
        #[serde(default)]
        session_id: Option<String>,
        reason: String,
        ts_ms: u64,
    },
    #[serde(rename = "platform.permission")]
    PlatformPermission {
        #[serde(default)]
        session_id: Option<String>,
        permission: String,
        granted: bool,
        #[serde(default)]
        detail: Option<String>,
        ts_ms: u64,
    },
    /// Live input level (RMS/peak in 0.0..=1.0). Added for the UX level meter
    /// (D-0013). Fields mirror the agreed shape
    /// `{"type":"audio.level","session_id","rms","peak","ts_ms"}`.
    #[serde(rename = "audio.level")]
    AudioLevel {
        session_id: String,
        rms: f32,
        peak: f32,
        ts_ms: u64,
    },
    /// Model provisioning progress (mirror of the daemon's `model.download`, D-0015). `error`
    /// is only present when `phase == Error`.
    #[serde(rename = "model.download")]
    ModelDownload {
        id: String,
        downloaded: u64,
        total: u64,
        phase: DownloadPhase,
        #[serde(default)]
        error: Option<String>,
        ts_ms: u64,
    },
}

/// Result of decoding one WS text frame. Tolerant by design: an unknown `type`
/// (e.g. an event a newer daemon adds) yields [`Incoming::Unknown`] rather than
/// breaking the stream, and a non-event frame yields [`Incoming::Unparseable`].
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Known(ApiEvent),
    Unknown(String),
    Unparseable,
}

/// Parse one WS text frame into an [`Incoming`]. Never panics.
pub fn parse_event(text: &str) -> Incoming {
    match serde_json::from_str::<ApiEvent>(text) {
        Ok(ev) => Incoming::Known(ev),
        Err(_) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => match v.get("type").and_then(|t| t.as_str()) {
                Some(t) => Incoming::Unknown(t.to_string()),
                None => Incoming::Unparseable,
            },
            Err(_) => Incoming::Unparseable,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures copied from the real shapes in daemon/api.rs so a drift in the
    // contract fails here loudly.
    const SESSION_STARTED: &str = r#"{"type":"session_started","session_id":"s1","ts_ms":1000}"#;
    const MODEL_LOADING: &str = r#"{"type":"model_loading","model":"large-v3-turbo","ts_ms":1}"#;
    const MODEL_READY: &str = r#"{"type":"model_ready","model":"large-v3-turbo","ts_ms":2}"#;
    const MODEL_LOAD_FAILED: &str =
        r#"{"type":"model_load_failed","model":"qwen3","error":"OOM","ts_ms":3}"#;
    const PARTIAL: &str = r#"{"type":"partial_transcript","session_id":"s1","segment_text":"příliš žluťoučký","ts_ms":4}"#;
    const RAW: &str = r#"{"type":"raw_transcript","session_id":"s1","text":"příliš žluťoučký kůň","lang":"cs","ts_ms":5}"#;
    const DICT_APPLIED: &str =
        r#"{"type":"dictionary_applied","session_id":"s1","text":"package.json","ts_ms":6}"#;
    const CLEANUP_SKIPPED: &str =
        r#"{"type":"cleanup_skipped","session_id":"s1","reason":"gate_clean","ts_ms":7}"#;
    const CLEANED: &str = r#"{"type":"cleaned","session_id":"s1","text":"hotový text","ts_ms":8}"#;
    const DELIVERED: &str =
        r#"{"type":"delivered","session_id":"s1","text":"hotový text","ts_ms":9}"#;
    const CANCELLED: &str =
        r#"{"type":"cancelled","session_id":"s1","partial_text":"půl","ts_ms":10}"#;
    const FAILED: &str = r#"{"type":"failed","session_id":"s1","error":"boom","ts_ms":11}"#;
    const SECURE_INPUT: &str =
        r#"{"type":"secure_input.blocked","session_id":"s1","owner":"Terminal","ts_ms":12}"#;
    const DEVICE_LOST: &str = r#"{"type":"audio.device_lost","reason":"unplugged","ts_ms":13}"#;
    const PERMISSION: &str =
        r#"{"type":"platform.permission","permission":"microphone","granted":false,"ts_ms":14}"#;
    const AUDIO_LEVEL: &str =
        r#"{"type":"audio.level","session_id":"s1","rms":0.12,"peak":0.4,"ts_ms":15}"#;
    const MODEL_DOWNLOAD: &str = r#"{"type":"model.download","id":"stt-large-v3-turbo","downloaded":100,"total":574041195,"phase":"downloading","ts_ms":16}"#;
    const MODEL_DOWNLOAD_ERROR: &str = r#"{"type":"model.download","id":"stt-large-v3-turbo","downloaded":0,"total":574041195,"phase":"error","error":"integrity check failed","ts_ms":17}"#;

    #[test]
    fn every_event_variant_parses() {
        let all = [
            SESSION_STARTED,
            MODEL_LOADING,
            MODEL_READY,
            MODEL_LOAD_FAILED,
            PARTIAL,
            RAW,
            DICT_APPLIED,
            CLEANUP_SKIPPED,
            CLEANED,
            DELIVERED,
            CANCELLED,
            FAILED,
            SECURE_INPUT,
            DEVICE_LOST,
            PERMISSION,
            AUDIO_LEVEL,
            MODEL_DOWNLOAD,
            MODEL_DOWNLOAD_ERROR,
        ];
        for fixture in all {
            assert!(
                matches!(parse_event(fixture), Incoming::Known(_)),
                "should parse as a known event: {fixture}"
            );
        }
    }

    #[test]
    fn partial_transcript_keeps_czech_diacritics() {
        match parse_event(PARTIAL) {
            Incoming::Known(ApiEvent::PartialTranscript { segment_text, .. }) => {
                assert_eq!(segment_text, "příliš žluťoučký");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn audio_level_fields_decoded() {
        match parse_event(AUDIO_LEVEL) {
            Incoming::Known(ApiEvent::AudioLevel { rms, peak, .. }) => {
                assert!((rms - 0.12).abs() < 1e-6);
                assert!((peak - 0.4).abs() < 1e-6);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_type_is_tolerated() {
        let ev = r#"{"type":"future_event","session_id":"s1","ts_ms":1}"#;
        assert_eq!(
            parse_event(ev),
            Incoming::Unknown("future_event".to_string())
        );
    }

    #[test]
    fn non_event_frame_is_unparseable() {
        assert_eq!(parse_event("not json"), Incoming::Unparseable);
        assert_eq!(parse_event(r#"{"no":"type"}"#), Incoming::Unparseable);
    }

    #[test]
    fn model_download_event_fields_decoded() {
        match parse_event(MODEL_DOWNLOAD) {
            Incoming::Known(ApiEvent::ModelDownload {
                id,
                downloaded,
                total,
                phase,
                error,
                ..
            }) => {
                assert_eq!(id, "stt-large-v3-turbo");
                assert_eq!(downloaded, 100);
                assert_eq!(total, 574_041_195);
                assert_eq!(phase, DownloadPhase::Downloading);
                assert!(error.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn model_download_error_event_carries_the_message() {
        match parse_event(MODEL_DOWNLOAD_ERROR) {
            Incoming::Known(ApiEvent::ModelDownload { phase, error, .. }) => {
                assert_eq!(phase, DownloadPhase::Error);
                assert_eq!(error.as_deref(), Some("integrity check failed"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn models_list_response_parses_the_daemon_shape() {
        // Fixture copied from the real `GET /v1/models` shape in daemon/api.rs.
        let json = r#"{"models":[
            {"id":"stt-large-v3-turbo","kind":"stt","filename":"ggml-large-v3-turbo-q5_0.bin","required":true,"size_bytes":574041195,"present":false,"path":"/data/models/ggml-large-v3-turbo-q5_0.bin","downloading":true,"progress":{"downloaded":1000,"total":574041195}},
            {"id":"cleanup-qwen3-8b","kind":"cleanup","filename":"Qwen3-8B-Q4_K_M.gguf","required":false,"size_bytes":5027783488,"present":true,"path":"/data/models/Qwen3-8B-Q4_K_M.gguf","downloading":false,"progress":null}
        ]}"#;
        let resp: ModelsListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.models.len(), 2);
        let stt = &resp.models[0];
        assert!(stt.required);
        assert!(!stt.present);
        assert!(stt.downloading);
        assert_eq!(
            stt.progress,
            Some(ModelProgress {
                downloaded: 1000,
                total: 574_041_195
            })
        );
        assert_eq!(stt.progress_percent(), Some(0));
        assert_eq!(stt.size_label(), "574 MB");

        let cleanup = &resp.models[1];
        assert!(cleanup.present);
        assert!(!cleanup.downloading);
        assert_eq!(cleanup.progress, None);
        assert_eq!(cleanup.progress_percent(), None);
    }

    #[test]
    fn size_label_formats_mb_and_gb() {
        let mut m = ModelEntry {
            id: "x".to_string(),
            kind: "stt".to_string(),
            filename: "x.bin".to_string(),
            required: true,
            size_bytes: 547_000_000,
            present: false,
            path: String::new(),
            downloading: false,
            progress: None,
        };
        assert_eq!(m.size_label(), "547 MB");
        m.size_bytes = 5_027_783_488;
        assert_eq!(m.size_label(), "5.0 GB");
    }

    #[test]
    fn config_round_trips_with_czech_language() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(back.language, "cs");
    }

    #[test]
    fn config_from_partial_daemon_json_fills_defaults() {
        // A daemon could omit fields we do not know; serde defaults cover them.
        let cfg: Config = serde_json::from_str(r#"{"port":9000,"language":"en"}"#).unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.language, "en");
        assert_eq!(cfg.cleanup_mode, CleanupMode::Auto);
    }

    #[test]
    fn config_ignores_unknown_future_fields() {
        // Forward-compatible: a newer daemon field must not break the shell.
        let cfg: Config = serde_json::from_str(r#"{"port":8737,"some_new_field":true}"#).unwrap();
        assert_eq!(cfg.port, 8737);
    }

    #[test]
    fn config_default_hotkey_matches_daemon_modifier_only_combo() {
        // Kept in sync with the daemon's incoming default (see whisper-iface
        // config.rs `default_hotkey_combo`): "Ctrl+Alt", enabled true.
        let cfg = Config::default();
        assert!(cfg.hotkey.enabled);
        assert_eq!(cfg.hotkey.combo, "Ctrl+Alt");
    }

    #[test]
    fn config_default_vad_and_audio_match_daemon() {
        let cfg = Config::default();
        assert_eq!(cfg.vad.threshold, 0.35);
        assert_eq!(cfg.vad.hangover_ms, 300);
        assert_eq!(cfg.vad.max_silence_ms, 1_000);
        assert!(cfg.audio.agc_enabled);
        assert_eq!(cfg.audio.agc_target_peak, 0.7);
        assert_eq!(cfg.audio.agc_max_gain, 12.0);
    }

    #[test]
    fn config_vad_audio_round_trip_and_partial_json_fills_defaults() {
        let json = r#"{"vad":{"threshold":0.42,"hangover_ms":250,"max_silence_ms":800},"audio":{"agc_enabled":false,"agc_target_peak":0.5,"agc_max_gain":8.0}}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.vad.threshold, 0.42);
        assert_eq!(cfg.vad.hangover_ms, 250);
        assert_eq!(cfg.vad.max_silence_ms, 800);
        assert!(!cfg.audio.agc_enabled);
        assert_eq!(cfg.audio.agc_target_peak, 0.5);
        assert_eq!(cfg.audio.agc_max_gain, 8.0);

        // Round-trip through serialize/deserialize.
        let back: Config = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn config_missing_vad_audio_fields_fill_defaults() {
        // A daemon predating this shell version still deserializes cleanly.
        let cfg: Config = serde_json::from_str(r#"{"port":8737}"#).unwrap();
        assert_eq!(cfg.vad, VadTuning::default());
        assert_eq!(cfg.audio, AudioTuning::default());
    }

    #[test]
    fn platform_permissions_parses_the_daemon_shape() {
        // Fixture copied from the real `GET /v1/platform/permissions` shape in daemon/api.rs
        // (contract-mirror discipline, HANDOFF item 8).
        let json = r#"{"input_monitoring":"denied","accessibility":"granted"}"#;
        let p: PlatformPermissions = serde_json::from_str(json).unwrap();
        assert_eq!(p.input_monitoring, "denied");
        assert_eq!(p.accessibility, "granted");
    }
}
