//! Shell-local UX preferences (D-0013). These are concerns of the *tray shell*
//! itself — the floating overlay's placement and whether it shows at all — and
//! deliberately live outside the daemon `/v1` config, which owns engine
//! behavior. Persisted as a tiny JSON file next to the onboarding state
//! (`<config_dir>/whisper-tray/shell.json`).
//!
//! Persistence takes an explicit path so it is unit-tested against a temp file,
//! never the real config dir. Load is tolerant: a missing or malformed file
//! yields defaults rather than blocking the shell (these settings are advisory
//! UI state, not authoritative engine config).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the floating overlay sits on screen. Always horizontally centered; this
/// only picks the vertical edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayPosition {
    /// A small margin below the menu bar.
    Top,
    /// Comfortably above the Dock (the default, matching Wispr Flow).
    #[default]
    Bottom,
}

/// Persisted shell-local preferences. `#[serde(default)]` at the container level
/// fills any missing field from [`Default`], so an older/partial file (or a
/// newer daemon adding fields) still loads with sensible values — and, crucially,
/// a missing `overlay_enabled` deserializes to `true`, not `false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellSettings {
    /// Show the floating waveform overlay while dictating.
    pub overlay_enabled: bool,
    /// Which screen edge the overlay hugs.
    pub overlay_position: OverlayPosition,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            overlay_enabled: true,
            overlay_position: OverlayPosition::Bottom,
        }
    }
}

impl ShellSettings {
    /// Load from `path`; a missing *or* malformed file yields defaults (these are
    /// advisory UI settings, so a corrupt file must never block the shell).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist to `path`, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }
}

/// Default persistence path: `<config_dir>/whisper-tray/shell.json`. Returns
/// `None` if the platform has no config dir.
#[must_use]
pub fn default_shell_settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("whisper-tray").join("shell.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "whisper-ux-shell-test-{}-{name}",
            std::process::id()
        ));
        p.push("shell.json");
        p
    }

    #[test]
    fn defaults_enable_overlay_at_bottom() {
        let s = ShellSettings::default();
        assert!(s.overlay_enabled);
        assert_eq!(s.overlay_position, OverlayPosition::Bottom);
    }

    #[test]
    fn missing_file_loads_default() {
        let p = temp_path("missing");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
        let s = ShellSettings::load(&p);
        assert_eq!(s, ShellSettings::default());
    }

    #[test]
    fn round_trips_through_disk() {
        let p = temp_path("roundtrip");
        let s = ShellSettings {
            overlay_enabled: false,
            overlay_position: OverlayPosition::Top,
        };
        s.save(&p).unwrap();
        assert_eq!(ShellSettings::load(&p), s);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn malformed_file_falls_back_to_default() {
        let p = temp_path("malformed");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(ShellSettings::load(&p), ShellSettings::default());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn partial_file_keeps_true_default_for_missing_enabled() {
        // Container-level `#[serde(default)]` must source the missing field from
        // `ShellSettings::default()` (true), not `bool::default()` (false).
        let p = temp_path("partial");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"overlay_position":"top"}"#).unwrap();
        let s = ShellSettings::load(&p);
        assert!(s.overlay_enabled);
        assert_eq!(s.overlay_position, OverlayPosition::Top);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn position_serializes_lowercase() {
        let s = ShellSettings {
            overlay_enabled: true,
            overlay_position: OverlayPosition::Top,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""overlay_position":"top""#), "got {json}");
    }
}
