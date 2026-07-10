//! `whisperd` configuration (`ARCHITECTURE.md` §6 Storage, D-0010): a TOML file with model
//! paths, bind/port, optional bearer token, cleanup mode, and language. The same struct is
//! the `GET/PUT /v1/config` JSON body, so the wire contract and the on-disk format never drift.
//!
//! Field order is load-bearing for TOML serialization: scalar fields must precede the
//! `[models]` / `[history]` tables (TOML requires tables after values).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use whisper_core::vad::VadConfig;

use super::api::CleanupModeDto;

/// Default HTTP port (`ARCHITECTURE.md` §4 note: pick a sensible default). 8737 spells
/// "TSER"-ish and is well outside the ephemeral range.
pub const DEFAULT_PORT: u16 = 8737;

fn default_bind() -> String {
    // Local-first hard constraint: never bind a routable interface by default.
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_language() -> String {
    "cs".to_string()
}

fn default_stt_model() -> PathBuf {
    PathBuf::from("models/ggml-large-v3-turbo-q5_0.bin")
}

fn default_cleanup_model() -> Option<PathBuf> {
    Some(PathBuf::from("models/Qwen3-8B-Q4_K_M.gguf"))
}

/// Resident model paths (D-0004 STT, D-0005 cleanup LLM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    #[serde(default = "default_stt_model")]
    pub stt: PathBuf,
    /// `None` disables the cleanup stage entirely (a supported profile, D-0005).
    #[serde(
        default = "default_cleanup_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub cleanup: Option<PathBuf>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            stt: default_stt_model(),
            cleanup: default_cleanup_model(),
        }
    }
}

/// How a session's final text is delivered (`ARCHITECTURE.md` §5, D-0012). `paste` requires the
/// daemon to be built with the `platform` feature; without it, `paste` degrades to `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryMode {
    /// Text is only returned via the API / WS events (the Phase-4 default).
    #[default]
    None,
    /// Inject into the focused app via pasteboard + Cmd-V (D-0007).
    Paste,
}

/// Hotkey trigger mode on the wire (`HotkeyMode` in `whisper-platform`, D-0007/D-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyModeDto {
    /// Hold to talk; release stops.
    #[default]
    PushToTalk,
    /// Press to start, press again to stop.
    Toggle,
}

/// The default combo is modifier-only (`Ctrl+Alt`, no `Space`): rdev listens without suppressing
/// events, so a combo that includes a printable key like `Space` leaks a literal character into
/// whatever text field has focus every time push-to-talk is pressed (a real user-testing hit).
/// Modifier-only combos have no printable representation, so nothing leaks.
fn default_hotkey_combo() -> String {
    "Ctrl+Alt".to_string()
}

fn default_hotkey_enabled() -> bool {
    // The hotkey only exists in `--features platform` builds (headless builds ignore this
    // field), and a dictation app whose hotkey is off by default fails the out-of-box test (a
    // real user-testing hit: the daemon started but nothing happened on the documented combo).
    true
}

/// Global hotkey configuration (`ARCHITECTURE.md` §5, D-0012). Effective only with the `platform`
/// feature; the listener starts at daemon startup, so changes are staged for the next restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    #[serde(default = "default_hotkey_enabled")]
    pub enabled: bool,
    #[serde(default = "default_hotkey_combo")]
    pub combo: String,
    #[serde(default)]
    pub mode: HotkeyModeDto,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled: default_hotkey_enabled(),
            combo: default_hotkey_combo(),
            mode: HotkeyModeDto::default(),
        }
    }
}

fn default_vad_threshold() -> f32 {
    // Lowered from Silero's 0.5 to 0.35 so quiet mics/soft speech still trigger speech detection
    // (the measured "must speak loudly" failure). Digital silence still scores ~0.0, so the D-0008
    // anti-hallucination trimming is unaffected (verified by `vad_antihallucination.rs`).
    0.35
}

fn default_hangover_ms() -> u32 {
    300
}

fn default_max_silence_ms() -> u32 {
    1_000
}

/// VAD sensitivity tuning (`[vad]` in config.toml, D-0008). Mapped to
/// [`whisper_core::vad::VadConfig`] when a session's VAD is built, so an edit takes effect on the
/// next session (reported `applied` by `PUT /v1/config`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VadTuning {
    /// Silero speech-probability threshold in `[0.0, 1.0]`; at/above is speech. Lower = more
    /// sensitive (quieter speech detected). Validated to `[0.05, 0.95]` by `PUT /v1/config`.
    #[serde(default = "default_vad_threshold")]
    pub threshold: f32,
    /// Trailing silence kept in a closed segment (~300 ms) so the last word is not clipped.
    #[serde(default = "default_hangover_ms")]
    pub hangover_ms: u32,
    /// Contiguous silence that closes an open segment (~1 s, D-0008); silence beyond the hangover
    /// is never decoded.
    #[serde(default = "default_max_silence_ms")]
    pub max_silence_ms: u32,
}

impl Default for VadTuning {
    fn default() -> Self {
        Self {
            threshold: default_vad_threshold(),
            hangover_ms: default_hangover_ms(),
            max_silence_ms: default_max_silence_ms(),
        }
    }
}

impl VadTuning {
    /// Project onto the core [`VadConfig`] (the pipeline's fixed 16 kHz working rate is kept).
    pub fn to_vad_config(&self) -> VadConfig {
        VadConfig {
            speech_threshold: self.threshold,
            hangover_ms: self.hangover_ms,
            max_silence_ms: self.max_silence_ms,
            ..VadConfig::default()
        }
    }
}

fn default_agc_enabled() -> bool {
    true
}

fn default_agc_target_peak() -> f32 {
    0.7
}

fn default_agc_max_gain() -> f32 {
    12.0
}

/// Capture input-gain tuning (`[audio]` in config.toml). Boost-only AGC-lite applied in the
/// capture path (`whisper-platform`) so a quiet mic is lifted before the VAD/STT see it. The
/// capture stream is built at daemon startup, so edits are *staged* for the next restart.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioTuning {
    /// Enable AGC-lite. On by default.
    #[serde(default = "default_agc_enabled")]
    pub agc_enabled: bool,
    /// Peak the gain chases toward, in `(0.0, 1.0]`. Validated by `PUT /v1/config`.
    #[serde(default = "default_agc_target_peak")]
    pub agc_target_peak: f32,
    /// Upper bound on the gain multiplier (lower bound is a fixed 1×). Validated by `PUT`.
    #[serde(default = "default_agc_max_gain")]
    pub agc_max_gain: f32,
}

impl Default for AudioTuning {
    fn default() -> Self {
        Self {
            agc_enabled: default_agc_enabled(),
            agc_target_peak: default_agc_target_peak(),
            agc_max_gain: default_agc_max_gain(),
        }
    }
}

/// Opt-in transcript history (D-0010: strictly opt-in SQLite).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Override the SQLite path; defaults to the XDG data dir at runtime when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// The full daemon configuration. Also the `/v1/config` JSON body.
// `Eq` is intentionally not derived: `[vad]`/`[audio]` carry `f32` tunables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional bearer token; when set (non-empty) every `/v1/*` request except `/v1/health`
    /// must carry `Authorization: Bearer <token>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub cleanup_mode: CleanupModeDto,
    #[serde(default = "default_language")]
    pub language: String,
    /// How final text is delivered (`ARCHITECTURE.md` §5, D-0012). Default `none` keeps the
    /// headless contract unchanged; `paste` needs the `platform` feature.
    #[serde(default)]
    pub delivery: DeliveryMode,
    /// Optional JSON dictionary file to seed `/v1/dictionary` from at startup (D-0010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_path: Option<PathBuf>,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub hotkey: HotkeyConfig,
    #[serde(default)]
    pub vad: VadTuning,
    #[serde(default)]
    pub audio: AudioTuning,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: default_port(),
            token: None,
            cleanup_mode: CleanupModeDto::Auto,
            language: default_language(),
            delivery: DeliveryMode::default(),
            dictionary_path: None,
            models: ModelsConfig::default(),
            history: HistoryConfig::default(),
            hotkey: HotkeyConfig::default(),
            vad: VadTuning::default(),
            audio: AudioTuning::default(),
        }
    }
}

impl Config {
    /// Parse a TOML config string. Missing fields fall back to defaults.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Serialize back to TOML for persisting after a `PUT /v1/config`.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Load from `path`, or return defaults if the file does not exist. A malformed file is a
    /// hard error (fail fast rather than silently run on defaults).
    pub fn load_or_default(path: &std::path::Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                Config::from_toml_str(&s).map_err(|e| format!("parse {}: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let cfg = Config::default();
        let toml = cfg.to_toml_string().unwrap();
        let back = Config::from_toml_str(&toml).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let cfg = Config::from_toml_str("port = 9000\nlanguage = \"cs\"\n").unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.bind, "127.0.0.1");
        assert_eq!(cfg.cleanup_mode, CleanupModeDto::Auto);
        assert_eq!(cfg.models.stt, default_stt_model());
    }

    #[test]
    fn language_field_carries_czech_value() {
        let cfg = Config::from_toml_str("language = \"cs\"\n").unwrap();
        assert_eq!(cfg.language, "cs");
    }

    #[test]
    fn delivery_defaults_to_none_and_parses_paste() {
        assert_eq!(Config::default().delivery, DeliveryMode::None);
        let cfg = Config::from_toml_str("delivery = \"paste\"\n").unwrap();
        assert_eq!(cfg.delivery, DeliveryMode::Paste);
    }

    #[test]
    fn vad_defaults_and_parses() {
        let def = Config::default().vad;
        assert_eq!(def.threshold, 0.35);
        assert_eq!(def.hangover_ms, 300);
        assert_eq!(def.max_silence_ms, 1_000);

        let cfg = Config::from_toml_str(
            "[vad]\nthreshold = 0.42\nhangover_ms = 250\nmax_silence_ms = 800\n",
        )
        .unwrap();
        assert_eq!(cfg.vad.threshold, 0.42);
        assert_eq!(cfg.vad.hangover_ms, 250);
        assert_eq!(cfg.vad.max_silence_ms, 800);

        // Projection onto the core VadConfig keeps the 16 kHz working rate.
        let vc = cfg.vad.to_vad_config();
        assert_eq!(vc.speech_threshold, 0.42);
        assert_eq!(vc.sample_rate_hz, 16_000);
    }

    #[test]
    fn audio_agc_defaults_and_parses() {
        let def = Config::default().audio;
        assert!(def.agc_enabled);
        assert_eq!(def.agc_target_peak, 0.7);
        assert_eq!(def.agc_max_gain, 12.0);

        let cfg = Config::from_toml_str(
            "[audio]\nagc_enabled = false\nagc_target_peak = 0.6\nagc_max_gain = 8.0\n",
        )
        .unwrap();
        assert!(!cfg.audio.agc_enabled);
        assert_eq!(cfg.audio.agc_target_peak, 0.6);
        assert_eq!(cfg.audio.agc_max_gain, 8.0);
    }

    #[test]
    fn hotkey_defaults_and_parses() {
        let def = Config::default().hotkey;
        // Default is on (out-of-box the hotkey should just work) with a modifier-only combo (no
        // Space) so push-to-talk never leaks a literal character into the focused field.
        assert!(def.enabled);
        assert_eq!(def.combo, "Ctrl+Alt");
        assert_eq!(def.mode, HotkeyModeDto::PushToTalk);

        let cfg = Config::from_toml_str(
            "[hotkey]\nenabled = true\ncombo = \"Cmd+Alt+D\"\nmode = \"toggle\"\n",
        )
        .unwrap();
        assert!(cfg.hotkey.enabled);
        assert_eq!(cfg.hotkey.combo, "Cmd+Alt+D");
        assert_eq!(cfg.hotkey.mode, HotkeyModeDto::Toggle);
    }
}
