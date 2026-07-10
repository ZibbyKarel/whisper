//! First-run onboarding state (D-0013 §5). A tiny JSON file records which
//! checklist steps the user has cleared so onboarding is shown only until it is
//! complete. Persistence takes an explicit path so it is unit-tested against a
//! temp file, never the real config dir.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted onboarding progress. `#[serde(default)]` keeps it forward-compatible
/// if steps are added later.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardingState {
    /// The user has finished (or explicitly dismissed) onboarding.
    pub completed: bool,
    /// `GET /v1/health` succeeded at least once.
    pub daemon_seen: bool,
    /// The user confirmed the microphone permission step.
    pub mic_ack: bool,
    /// The user confirmed the accessibility permission step.
    pub accessibility_ack: bool,
    /// A test dictation produced text at least once.
    pub test_dictation_done: bool,
}

impl OnboardingState {
    /// All checklist steps cleared.
    #[must_use]
    pub fn all_steps_done(&self) -> bool {
        self.daemon_seen && self.mic_ack && self.accessibility_ack && self.test_dictation_done
    }

    /// Load from `path`; a missing file yields the default (fresh) state. A
    /// malformed file also yields the default rather than blocking first-run —
    /// onboarding state is advisory, not authoritative.
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

/// Default persistence path: `<config_dir>/whisper-tray/state.json` (D-0013 §5).
/// Returns `None` if the platform has no config dir.
#[must_use]
pub fn default_state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("whisper-tray").join("state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("whisper-ux-test-{}-{name}", std::process::id()));
        p.push("state.json");
        p
    }

    #[test]
    fn missing_file_loads_default() {
        let p = temp_path("missing");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
        let s = OnboardingState::load(&p);
        assert_eq!(s, OnboardingState::default());
        assert!(!s.completed);
    }

    #[test]
    fn round_trips_through_disk() {
        let p = temp_path("roundtrip");
        let s = OnboardingState {
            completed: true,
            daemon_seen: true,
            mic_ack: true,
            accessibility_ack: true,
            test_dictation_done: true,
        };
        s.save(&p).unwrap();
        let back = OnboardingState::load(&p);
        assert_eq!(s, back);
        assert!(back.all_steps_done());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn malformed_file_falls_back_to_default() {
        let p = temp_path("malformed");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(OnboardingState::load(&p), OnboardingState::default());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn all_steps_done_requires_every_step() {
        let mut s = OnboardingState {
            daemon_seen: true,
            mic_ack: true,
            accessibility_ack: true,
            test_dictation_done: false,
            completed: false,
        };
        assert!(!s.all_steps_done());
        s.test_dictation_done = true;
        assert!(s.all_steps_done());
    }
}
