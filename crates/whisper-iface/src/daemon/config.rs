//! `whisperd` configuration (`ARCHITECTURE.md` §6 Storage, D-0010): a TOML file with model
//! paths, bind/port, optional bearer token, cleanup mode, and language. The same struct is
//! the `GET/PUT /v1/config` JSON body, so the wire contract and the on-disk format never drift.
//!
//! Field order is load-bearing for TOML serialization: scalar fields must precede the
//! `[models]` / `[history]` tables (TOML requires tables after values).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Optional JSON dictionary file to seed `/v1/dictionary` from at startup (D-0010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_path: Option<PathBuf>,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub history: HistoryConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: default_port(),
            token: None,
            cleanup_mode: CleanupModeDto::Auto,
            language: default_language(),
            dictionary_path: None,
            models: ModelsConfig::default(),
            history: HistoryConfig::default(),
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
}
