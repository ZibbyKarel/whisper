//! Shared Ollama cleanup state (D-0017): is a local Ollama installed/running, and does the
//! selected cleanup model exist in it? Kept current by polling `GET /v1/ollama` on the same
//! cadence as the models poll (`gui.rs`) and by the `ollama.status` WS event. Feeds the
//! Settings → Cleanup card, the onboarding step and the main-window attention banner — the
//! GUI-free/unit-tested half (`ARCHITECTURE.md`/D-0013: the lib holds logic, the binary wires
//! the window). Cleanup is optional, so nothing here ever blocks the app.

use crate::api::{ApiEvent, Config, OllamaStatus};
use crate::client::{Api, ApiError};
use crate::models_ui::ModelsPanel;

/// Where "Install Ollama" sends the user (opened via `permissions_ui::open_system_settings`).
pub const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";
/// Selectable install hint shown next to the "Install Ollama" button.
pub const BREW_INSTALL_HINT: &str = "brew install ollama";

/// What is wrong with the cleanup setup, most fundamental problem first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OllamaAttention {
    NotInstalled,
    NotRunning {
        can_start: bool,
    },
    /// The configured model is not pulled; `id` is the `/v1/models` entry to download, when the
    /// daemon lists one for this tag.
    ModelMissing {
        tag: String,
        id: Option<String>,
    },
}

impl OllamaAttention {
    /// Banner copy (exact strings, D-0017 plan).
    #[must_use]
    pub fn banner_text(&self) -> String {
        match self {
            Self::NotInstalled => "Text cleanup needs Ollama — not installed.".to_string(),
            Self::NotRunning { .. } => "Text cleanup: Ollama is not running.".to_string(),
            Self::ModelMissing { tag, .. } => {
                format!("Text cleanup: model {tag} is not downloaded.")
            }
        }
    }
}

/// Last known `GET /v1/ollama` snapshot.
#[derive(Debug, Clone, Default)]
pub struct OllamaPanel {
    /// `None` until the first successful poll — an unpolled/unreachable daemon never claims
    /// "not installed".
    pub status: Option<OllamaStatus>,
    /// The last failed `refresh()`/`start()` message.
    pub last_error: Option<String>,
}

impl OllamaPanel {
    /// `GET /v1/ollama`. Best-effort: on failure the last snapshot is kept and `last_error` set.
    pub fn refresh(&mut self, api: &dyn Api) {
        match api.ollama_status() {
            Ok(st) => {
                self.status = Some(st);
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    /// `POST /v1/ollama/start` (`202`). The `ollama.status` event follows once the server is up
    /// (or has failed to come up), so no refresh is needed here; a failure lands in `last_error`.
    pub fn start(&mut self, api: &dyn Api) {
        match api.start_ollama() {
            Ok(()) => self.last_error = None,
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    /// Fold a WS event in; everything but `ollama.status` is ignored.
    pub fn apply_event(&mut self, ev: &ApiEvent) {
        if let ApiEvent::OllamaStatus {
            installed,
            running,
            version,
            url,
            can_start,
            ..
        } = ev
        {
            self.status = Some(OllamaStatus {
                installed: *installed,
                running: *running,
                version: version.clone(),
                url: url.clone(),
                can_start: *can_start,
            });
            self.last_error = None;
        }
    }

    /// Status line for the card: "Ollama not installed" / "Ollama installed — not running" /
    /// "Ollama running (v{version})".
    #[must_use]
    pub fn status_label(&self) -> String {
        match &self.status {
            None => "Checking Ollama…".to_string(),
            Some(s) if !s.installed => "Ollama not installed".to_string(),
            Some(s) if !s.running => "Ollama installed — not running".to_string(),
            Some(OllamaStatus {
                version: Some(v), ..
            }) => format!("Ollama running (v{v})"),
            Some(_) => "Ollama running".to_string(),
        }
    }

    /// The server itself needs the user (not installed / not running) while cleanup is on.
    /// Cleanup off (`cleanup_model == ""`) never needs attention.
    #[must_use]
    pub fn needs_attention(&self, cleanup_model: &str) -> bool {
        self.attention(cleanup_model, &ModelsPanel::default())
            .is_some()
    }

    /// Full attention check including a missing model, judged from the shared `/v1/models`
    /// snapshot (only once it has been fetched, and only while Ollama is running — the tag list
    /// cannot be known otherwise).
    #[must_use]
    pub fn attention(&self, cleanup_model: &str, models: &ModelsPanel) -> Option<OllamaAttention> {
        let model = cleanup_model.trim();
        if model.is_empty() {
            return None;
        }
        let status = self.status.as_ref()?;
        if !status.installed {
            return Some(OllamaAttention::NotInstalled);
        }
        if !status.running {
            return Some(OllamaAttention::NotRunning {
                can_start: status.can_start,
            });
        }
        if models.entries.is_empty() {
            return None;
        }
        match models.selected_cleanup() {
            Some(e) if e.present => None,
            selected => Some(OllamaAttention::ModelMissing {
                tag: model.to_string(),
                id: selected.map(|e| e.id.clone()),
            }),
        }
    }
}

/// Onboarding "Text cleanup" step is satisfied: the user chose Off (`cleanup.model == ""`), or
/// Ollama is running and the selected model is present. `None` (config not loaded yet) is never
/// ready.
#[must_use]
pub fn onboarding_ready(
    ollama: &OllamaPanel,
    models: &ModelsPanel,
    cleanup_model: Option<&str>,
) -> bool {
    let Some(model) = cleanup_model else {
        return false;
    };
    if model.trim().is_empty() {
        return true;
    }
    ollama.status.as_ref().is_some_and(|s| s.running)
        && models.selected_cleanup().is_some_and(|e| e.present)
}

/// "Use" / "Off": `GET /v1/config` → set `cleanup.model` → `PUT /v1/config` (applied at runtime
/// by the daemon). `model == ""` turns cleanup off. Returns the daemon's resulting config.
pub fn set_cleanup_model(api: &dyn Api, model: &str) -> Result<Config, ApiError> {
    let mut cfg = api.get_config()?;
    cfg.cleanup.model = model.to_string();
    Ok(api.put_config(&cfg)?.config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ConfigUpdateResponse, ModelEntry};
    use std::cell::RefCell;

    struct FakeApi {
        status: Result<OllamaStatus, ApiError>,
        start: Result<(), ApiError>,
        put: RefCell<Vec<Config>>,
    }

    impl FakeApi {
        fn new(status: Result<OllamaStatus, ApiError>) -> Self {
            Self {
                status,
                start: Ok(()),
                put: RefCell::new(Vec::new()),
            }
        }
    }

    impl Api for FakeApi {
        fn health(&self) -> Result<crate::api::HealthResponse, ApiError> {
            unimplemented!()
        }
        fn status(&self) -> Result<crate::api::StatusResponse, ApiError> {
            unimplemented!()
        }
        fn get_config(&self) -> Result<Config, ApiError> {
            Ok(Config {
                language: "cs".to_string(),
                ..Config::default()
            })
        }
        fn put_config(&self, cfg: &Config) -> Result<ConfigUpdateResponse, ApiError> {
            self.put.borrow_mut().push(cfg.clone());
            Ok(ConfigUpdateResponse {
                applied: vec!["cleanup.model".to_string()],
                staged: Vec::new(),
                config: cfg.clone(),
            })
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
        fn platform_permissions(&self) -> Result<crate::api::PlatformPermissions, ApiError> {
            unimplemented!()
        }
        fn ollama_status(&self) -> Result<OllamaStatus, ApiError> {
            self.status.clone()
        }
        fn start_ollama(&self) -> Result<(), ApiError> {
            self.start.clone()
        }
    }

    fn status(installed: bool, running: bool) -> OllamaStatus {
        OllamaStatus {
            installed,
            running,
            version: running.then(|| "0.12.3".to_string()),
            url: "http://127.0.0.1:11434".to_string(),
            can_start: installed && !running,
        }
    }

    fn panel(installed: bool, running: bool) -> OllamaPanel {
        OllamaPanel {
            status: Some(status(installed, running)),
            last_error: None,
        }
    }

    fn cleanup_entry(id: &str, tag: &str, present: bool, selected: bool) -> ModelEntry {
        ModelEntry {
            id: id.to_string(),
            kind: "cleanup".to_string(),
            source: "ollama".to_string(),
            ollama_tag: Some(tag.to_string()),
            validated: true,
            selected,
            filename: None,
            required: false,
            size_bytes: 6_000_000_000,
            present,
            path: None,
            downloading: false,
            progress: None,
        }
    }

    fn models_with(entries: Vec<ModelEntry>) -> ModelsPanel {
        let mut m = ModelsPanel::default();
        m.entries = entries;
        m
    }

    const MODEL: &str = "qwen3-vl:8b-instruct";

    #[test]
    fn needs_attention_when_not_installed() {
        assert!(panel(false, false).needs_attention(MODEL));
        assert_eq!(
            panel(false, false).attention(MODEL, &ModelsPanel::default()),
            Some(OllamaAttention::NotInstalled)
        );
    }

    #[test]
    fn needs_attention_when_installed_but_not_running() {
        let p = panel(true, false);
        assert!(p.needs_attention(MODEL));
        assert_eq!(
            p.attention(MODEL, &ModelsPanel::default()),
            Some(OllamaAttention::NotRunning { can_start: true })
        );
    }

    #[test]
    fn needs_attention_false_when_model_off() {
        assert!(!panel(false, false).needs_attention(""));
        assert!(!panel(true, false).needs_attention("  "));
    }

    #[test]
    fn needs_attention_false_before_first_poll() {
        assert!(!OllamaPanel::default().needs_attention(MODEL));
    }

    #[test]
    fn needs_attention_when_running_but_model_missing() {
        let p = panel(true, true);
        let models = models_with(vec![cleanup_entry(
            "cleanup-qwen3-vl-8b-instruct",
            MODEL,
            false,
            true,
        )]);
        let att = p.attention(MODEL, &models).unwrap();
        assert_eq!(
            att,
            OllamaAttention::ModelMissing {
                tag: MODEL.to_string(),
                id: Some("cleanup-qwen3-vl-8b-instruct".to_string())
            }
        );
        assert_eq!(
            att.banner_text(),
            "Text cleanup: model qwen3-vl:8b-instruct is not downloaded."
        );
    }

    #[test]
    fn model_unlisted_counts_as_missing_without_download_id() {
        let p = panel(true, true);
        let models = models_with(vec![cleanup_entry(
            "cleanup-qwen3-8b",
            "qwen3:8b",
            true,
            false,
        )]);
        assert_eq!(
            p.attention("gemma3:12b", &models),
            Some(OllamaAttention::ModelMissing {
                tag: "gemma3:12b".to_string(),
                id: None
            })
        );
    }

    #[test]
    fn running_with_present_model_needs_no_attention() {
        let p = panel(true, true);
        let models = models_with(vec![cleanup_entry(
            "cleanup-qwen3-vl-8b-instruct",
            MODEL,
            true,
            true,
        )]);
        assert_eq!(p.attention(MODEL, &models), None);
        // Models not fetched yet: don't guess.
        assert_eq!(p.attention(MODEL, &ModelsPanel::default()), None);
    }

    #[test]
    fn banner_and_status_copy_is_exact() {
        assert_eq!(
            OllamaAttention::NotInstalled.banner_text(),
            "Text cleanup needs Ollama — not installed."
        );
        assert_eq!(
            OllamaAttention::NotRunning { can_start: false }.banner_text(),
            "Text cleanup: Ollama is not running."
        );
        assert_eq!(panel(false, false).status_label(), "Ollama not installed");
        assert_eq!(
            panel(true, false).status_label(),
            "Ollama installed — not running"
        );
        assert_eq!(panel(true, true).status_label(), "Ollama running (v0.12.3)");
    }

    #[test]
    fn refresh_sets_status_and_keeps_stale_on_failure() {
        let mut p = OllamaPanel::default();
        p.refresh(&FakeApi::new(Ok(status(true, true))));
        assert!(p.status.as_ref().unwrap().running);
        p.refresh(&FakeApi::new(Err(ApiError::Transport("down".to_string()))));
        assert!(p.status.as_ref().unwrap().running, "stale snapshot kept");
        assert!(p.last_error.is_some());
    }

    #[test]
    fn start_records_error_from_409() {
        let mut p = panel(true, false);
        let mut api = FakeApi::new(Ok(status(true, false)));
        api.start = Err(ApiError::Http {
            status: 409,
            code: "ollama_not_startable".to_string(),
            message: "not startable".to_string(),
        });
        p.start(&api);
        assert!(p
            .last_error
            .as_deref()
            .unwrap()
            .contains("ollama_not_startable"));
        api.start = Ok(());
        p.start(&api);
        assert!(p.last_error.is_none());
    }

    #[test]
    fn ollama_status_event_replaces_snapshot() {
        let mut p = panel(true, false);
        p.apply_event(&ApiEvent::OllamaStatus {
            installed: true,
            running: true,
            version: Some("0.12.3".to_string()),
            url: "http://127.0.0.1:11434".to_string(),
            can_start: false,
            ts_ms: 1,
        });
        assert!(p.status.as_ref().unwrap().running);
        // Unrelated events are ignored.
        p.apply_event(&ApiEvent::Cleaned {
            session_id: "s".to_string(),
            text: "příliš žluťoučký kůň".to_string(),
            ts_ms: 2,
        });
        assert!(p.status.as_ref().unwrap().running);
    }

    #[test]
    fn onboarding_ready_when_off_or_running_with_model() {
        let models = models_with(vec![cleanup_entry(
            "cleanup-qwen3-vl-8b-instruct",
            MODEL,
            true,
            true,
        )]);
        // Off needs nothing from Ollama.
        assert!(onboarding_ready(
            &OllamaPanel::default(),
            &ModelsPanel::default(),
            Some("")
        ));
        assert!(onboarding_ready(&panel(true, true), &models, Some(MODEL)));
        assert!(!onboarding_ready(&panel(true, false), &models, Some(MODEL)));
        assert!(!onboarding_ready(
            &panel(true, true),
            &ModelsPanel::default(),
            Some(MODEL)
        ));
        assert!(!onboarding_ready(&panel(true, true), &models, None));
    }

    #[test]
    fn set_cleanup_model_puts_the_changed_config_and_off_is_empty() {
        let api = FakeApi::new(Ok(status(true, true)));
        let cfg = set_cleanup_model(&api, "qwen3:8b").unwrap();
        assert_eq!(cfg.cleanup.model, "qwen3:8b");
        let cfg = set_cleanup_model(&api, "").unwrap();
        assert!(!cfg.cleanup.enabled());
        let puts = api.put.borrow();
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[0].cleanup.model, "qwen3:8b");
        assert_eq!(puts[0].language, "cs", "other fields round-trip untouched");
        assert_eq!(puts[1].cleanup.model, "");
    }
}
