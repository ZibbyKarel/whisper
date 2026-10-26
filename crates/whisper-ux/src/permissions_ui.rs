//! Shared platform-permission state (D-0016): Input Monitoring (hotkey) and Accessibility
//! (paste) grant status, kept current by polling `GET /v1/platform/permissions` on the same
//! cadence as the models poll (`gui.rs`). Feeds both the persistent main-window banner and the
//! onboarding checklist steps — GUI-free/unit-tested half (`ARCHITECTURE.md`/D-0013: the lib
//! holds logic, the binary wires the window).

use crate::api::PlatformPermissions;
use crate::client::Api;

/// System Settings deep-link for the Input Monitoring pane (`x-apple.systempreferences:`
/// scheme, opened via the `open` command).
pub const INPUT_MONITORING_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";
/// System Settings deep-link for the Accessibility pane.
pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// Last known `GET /v1/platform/permissions` snapshot. Defaults to `"unknown"` for both fields
/// (never defaults to `"granted"`: an unpolled/unreachable daemon must never read as "fine").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsPanel {
    pub input_monitoring: String,
    pub accessibility: String,
    /// Set when the last `refresh()` call failed (e.g. daemon unreachable, or an older daemon
    /// build without this endpoint — treated the same as "unknown", not a hard error).
    pub fetch_error: Option<String>,
}

impl Default for PermissionsPanel {
    fn default() -> Self {
        Self {
            input_monitoring: "unknown".to_string(),
            accessibility: "unknown".to_string(),
            fetch_error: None,
        }
    }
}

impl PermissionsPanel {
    /// `GET /v1/platform/permissions`. Best-effort: on failure the last known snapshot is kept
    /// (stale-but-present beats blank) and `fetch_error` is set.
    pub fn refresh(&mut self, api: &dyn Api) {
        match api.platform_permissions() {
            Ok(PlatformPermissions {
                input_monitoring,
                accessibility,
            }) => {
                self.input_monitoring = input_monitoring;
                self.accessibility = accessibility;
                self.fetch_error = None;
            }
            Err(e) => self.fetch_error = Some(e.to_string()),
        }
    }

    /// Input Monitoring needs attention: anything other than `"granted"` — this covers both a
    /// confirmed missing grant (`"denied"`) and the ambiguous `"unknown"` (headless daemon, or
    /// before the first successful poll), since the silent failure this closes is exactly
    /// "looks like it started fine but isn't receiving events" (`ARCHITECTURE.md` §5, D-0016).
    #[must_use]
    pub fn input_monitoring_needs_attention(&self) -> bool {
        self.input_monitoring != "granted"
    }

    #[must_use]
    pub fn accessibility_needs_attention(&self) -> bool {
        self.accessibility != "granted"
    }

    /// Either grant needs attention — drives the persistent main-window banner.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.input_monitoring_needs_attention() || self.accessibility_needs_attention()
    }
}

/// Open a macOS System Settings pane via the `open` command (e.g.
/// [`INPUT_MONITORING_SETTINGS_URL`]). Best-effort: a spawn failure (missing `open`, or a
/// non-macOS dev environment) is swallowed — the banner/onboarding text already spells out the
/// manual System Settings path as a fallback.
pub fn open_system_settings(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ApiError;

    struct FakeApi(Result<PlatformPermissions, ApiError>);

    impl Api for FakeApi {
        fn health(&self) -> Result<crate::api::HealthResponse, ApiError> {
            unimplemented!()
        }
        fn status(&self) -> Result<crate::api::StatusResponse, ApiError> {
            unimplemented!()
        }
        fn get_config(&self) -> Result<crate::api::Config, ApiError> {
            unimplemented!()
        }
        fn put_config(
            &self,
            _cfg: &crate::api::Config,
        ) -> Result<crate::api::ConfigUpdateResponse, ApiError> {
            unimplemented!()
        }
        fn get_dictionary(&self) -> Result<serde_json::Value, ApiError> {
            unimplemented!()
        }
        fn put_dictionary(&self, _body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
            unimplemented!()
        }
        fn start_mic_session(&self) -> Result<crate::api::SessionAccepted, ApiError> {
            unimplemented!()
        }
        fn stop_session(&self, _id: &str) -> Result<crate::api::SessionRecord, ApiError> {
            unimplemented!()
        }
        fn list_models(&self) -> Result<crate::api::ModelsListResponse, ApiError> {
            unimplemented!()
        }
        fn start_model_download(&self, _id: &str) -> Result<(), ApiError> {
            unimplemented!()
        }
        fn cancel_model_download(&self, _id: &str) -> Result<(), ApiError> {
            unimplemented!()
        }
        fn platform_permissions(&self) -> Result<PlatformPermissions, ApiError> {
            self.0.clone()
        }
        fn ollama_status(&self) -> Result<crate::api::OllamaStatus, ApiError> {
            unimplemented!()
        }
        fn start_ollama(&self) -> Result<(), ApiError> {
            unimplemented!()
        }
    }

    fn permissions(input_monitoring: &str, accessibility: &str) -> PlatformPermissions {
        PlatformPermissions {
            input_monitoring: input_monitoring.to_string(),
            accessibility: accessibility.to_string(),
        }
    }

    #[test]
    fn default_is_unknown_not_granted() {
        let p = PermissionsPanel::default();
        assert_eq!(p.input_monitoring, "unknown");
        assert_eq!(p.accessibility, "unknown");
        assert!(p.needs_attention(), "unpolled state must not read as fine");
    }

    #[test]
    fn refresh_populates_fields_on_success() {
        let mut panel = PermissionsPanel::default();
        let api = FakeApi(Ok(permissions("granted", "granted")));
        panel.refresh(&api);
        assert_eq!(panel.input_monitoring, "granted");
        assert_eq!(panel.accessibility, "granted");
        assert!(panel.fetch_error.is_none());
        assert!(!panel.needs_attention());
    }

    #[test]
    fn refresh_keeps_stale_snapshot_and_sets_error_on_failure() {
        let mut panel = PermissionsPanel {
            input_monitoring: "granted".to_string(),
            accessibility: "granted".to_string(),
            fetch_error: None,
        };
        let api = FakeApi(Err(ApiError::Transport("no daemon".to_string())));
        panel.refresh(&api);
        assert_eq!(panel.input_monitoring, "granted", "stale value kept");
        assert!(panel.fetch_error.is_some());
    }

    #[test]
    fn denied_input_monitoring_needs_attention_but_not_accessibility() {
        let panel = PermissionsPanel {
            input_monitoring: "denied".to_string(),
            accessibility: "granted".to_string(),
            fetch_error: None,
        };
        assert!(panel.input_monitoring_needs_attention());
        assert!(!panel.accessibility_needs_attention());
        assert!(panel.needs_attention());
    }

    #[test]
    fn both_granted_needs_no_attention() {
        let panel = PermissionsPanel {
            input_monitoring: "granted".to_string(),
            accessibility: "granted".to_string(),
            fetch_error: None,
        };
        assert!(!panel.needs_attention());
    }

    #[test]
    fn deep_link_urls_use_the_documented_system_preferences_scheme() {
        assert!(INPUT_MONITORING_SETTINGS_URL.starts_with("x-apple.systempreferences:"));
        assert!(INPUT_MONITORING_SETTINGS_URL.ends_with("Privacy_ListenEvent"));
        assert!(ACCESSIBILITY_SETTINGS_URL.starts_with("x-apple.systempreferences:"));
        assert!(ACCESSIBILITY_SETTINGS_URL.ends_with("Privacy_Accessibility"));
    }
}
