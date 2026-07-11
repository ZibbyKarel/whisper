//! Shared "model download" state (D-0015) behind the three UX surfaces that need it: the
//! startup gate (blocks normal use while a required model is missing), Settings → Models
//! (the primary re-download surface), and the onboarding checklist. All three read the same
//! [`ModelsPanel`] snapshot, kept current by [`ModelsPanel::refresh`] (`GET /v1/models`) and
//! live-updated between polls by [`ModelsPanel::apply_event`] (`model.download` WS events).
//!
//! Rendering (`egui::Ui`) stays in `gui.rs`, alongside the HTTP calls the download/cancel
//! buttons make — this module is the GUI-free, unit-tested half (`ARCHITECTURE.md`/D-0013:
//! the lib holds logic; the binary wires the window).

use std::collections::HashMap;

use crate::api::{ApiEvent, DownloadPhase, ModelEntry, ModelProgress};
use crate::client::Api;

/// Last known `GET /v1/models` snapshot, plus any download errors surfaced by WS events
/// (`GET /v1/models` itself carries no error detail — only the WS `model.download` event
/// does, per the D-0015 design doc's error table).
#[derive(Debug, Clone, Default)]
pub struct ModelsPanel {
    pub entries: Vec<ModelEntry>,
    /// Set when the last `refresh()` call failed (e.g. daemon unreachable).
    pub fetch_error: Option<String>,
    /// Per-model id: the last `model.download` error message, if the most recent attempt
    /// ended in `phase == error`. Cleared when a new download starts.
    download_errors: HashMap<String, String>,
    /// Per-model id: the most recent `model.download` phase, so the UI can distinguish
    /// "Downloading NN%" from "Verifying" (both have `downloading == true` on the plain
    /// `GET /v1/models` shape, which has no `phase` field of its own).
    download_phase: HashMap<String, DownloadPhase>,
}

impl ModelsPanel {
    /// `GET /v1/models`. Best-effort: on failure, `entries` is left as the last known
    /// snapshot (stale-but-present beats blank) and `fetch_error` is set.
    pub fn refresh(&mut self, api: &dyn Api) {
        match api.list_models() {
            Ok(resp) => {
                self.entries = resp.models;
                self.fetch_error = None;
            }
            Err(e) => self.fetch_error = Some(e.to_string()),
        }
    }

    /// Fold a WS event into the cached snapshot so the UI updates live between polls.
    /// Non-`model.download` events are ignored (callers may pass every event through).
    pub fn apply_event(&mut self, ev: &ApiEvent) {
        let ApiEvent::ModelDownload {
            id,
            downloaded,
            total,
            phase,
            error,
            ..
        } = ev
        else {
            return;
        };
        self.download_phase.insert(id.clone(), *phase);
        match phase {
            DownloadPhase::Downloading | DownloadPhase::Verifying => {
                self.download_errors.remove(id);
                if let Some(entry) = self.entry_mut(id) {
                    entry.downloading = true;
                    entry.progress = Some(ModelProgress {
                        downloaded: *downloaded,
                        total: *total,
                    });
                }
            }
            DownloadPhase::Done => {
                self.download_errors.remove(id);
                if let Some(entry) = self.entry_mut(id) {
                    entry.downloading = false;
                    entry.present = true;
                    entry.progress = None;
                }
            }
            DownloadPhase::Error => {
                // A user-initiated cancel (`DELETE /v1/models/{id}/download`) reports as
                // `phase=error, error="cancelled"` on the wire (D-0015's fixed phase set has
                // no dedicated "cancelled" value) — but it isn't a *failure* the tray should
                // flag in red; the daemon already deleted `.part`, so the row should just
                // read "Missing" again, same as if the download had never started.
                if error.as_deref() == Some("cancelled") {
                    self.download_errors.remove(id);
                } else {
                    self.download_errors
                        .insert(id.clone(), error.clone().unwrap_or_default());
                }
                if let Some(entry) = self.entry_mut(id) {
                    entry.downloading = false;
                    entry.progress = None;
                }
            }
        }
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut ModelEntry> {
        self.entries.iter_mut().find(|m| m.id == id)
    }

    /// The catalog entry for `id`, if the last refresh saw it.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&ModelEntry> {
        self.entries.iter().find(|m| m.id == id)
    }

    /// The last download error for `id` (cleared once a new attempt starts/succeeds).
    #[must_use]
    pub fn download_error(&self, id: &str) -> Option<&str> {
        self.download_errors.get(id).map(String::as_str)
    }

    /// The most recent `model.download` phase seen for `id` — lets the UI show "Verifying"
    /// distinctly from "Downloading NN%" (both report `downloading == true` on the plain
    /// `GET /v1/models` snapshot).
    #[must_use]
    pub fn phase(&self, id: &str) -> Option<DownloadPhase> {
        self.download_phase.get(id).copied()
    }

    /// Optimistic UI update right after a successful `POST /v1/models/{id}/download`, so the
    /// row flips to "Downloading" immediately instead of waiting for the first WS tick (which
    /// only fires once the HTTP response headers for the (potentially large) download land).
    /// A no-op if `id` isn't a known catalog entry.
    pub fn mark_download_started(&mut self, id: &str) {
        self.download_errors.remove(id);
        self.download_phase
            .insert(id.to_string(), DownloadPhase::Downloading);
        if let Some(entry) = self.entry_mut(id) {
            entry.downloading = true;
            entry.progress = Some(ModelProgress {
                downloaded: 0,
                total: entry.size_bytes,
            });
        }
    }

    /// Every catalog model marked `required` that is not yet `present`.
    #[must_use]
    pub fn missing_required(&self) -> Vec<&ModelEntry> {
        self.entries
            .iter()
            .filter(|m| m.required && !m.present)
            .collect()
    }

    /// `true` once at least one successful refresh has happened AND every required model is
    /// present — the startup-gate / onboarding-checklist condition (D-0015 design doc).
    #[must_use]
    pub fn required_satisfied(&self) -> bool {
        !self.entries.is_empty() && self.missing_required().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ApiError;

    fn entry(id: &str, required: bool, present: bool) -> ModelEntry {
        ModelEntry {
            id: id.to_string(),
            kind: if id.starts_with("stt") {
                "stt".to_string()
            } else {
                "cleanup".to_string()
            },
            filename: format!("{id}.bin"),
            required,
            size_bytes: 1_000,
            present,
            path: format!("/data/{id}.bin"),
            downloading: false,
            progress: None,
        }
    }

    /// Fake [`Api`] returning a fixed model list (or a transport error), so `ModelsPanel` is
    /// unit-tested without a daemon (mirrors the `Api` trait's existing test seam).
    struct FakeApi(Result<Vec<ModelEntry>, ApiError>);

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
            self.0
                .clone()
                .map(|models| crate::api::ModelsListResponse { models })
        }
        fn start_model_download(&self, _id: &str) -> Result<(), ApiError> {
            unimplemented!()
        }
        fn cancel_model_download(&self, _id: &str) -> Result<(), ApiError> {
            unimplemented!()
        }
    }

    #[test]
    fn refresh_populates_entries_on_success() {
        let mut panel = ModelsPanel::default();
        let api = FakeApi(Ok(vec![entry("stt-large-v3-turbo", true, false)]));
        panel.refresh(&api);
        assert_eq!(panel.entries.len(), 1);
        assert!(panel.fetch_error.is_none());
    }

    #[test]
    fn refresh_keeps_stale_entries_and_sets_error_on_failure() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, true)],
            ..Default::default()
        };
        let api = FakeApi(Err(ApiError::Transport("no daemon".to_string())));
        panel.refresh(&api);
        assert_eq!(
            panel.entries.len(),
            1,
            "stale snapshot is kept, not cleared"
        );
        assert!(panel.fetch_error.is_some());
    }

    #[test]
    fn missing_required_lists_only_required_and_absent() {
        let panel = ModelsPanel {
            entries: vec![
                entry("stt-large-v3-turbo", true, false),
                entry("cleanup-qwen3-8b", false, false),
            ],
            ..Default::default()
        };
        let missing = panel.missing_required();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "stt-large-v3-turbo");
    }

    #[test]
    fn required_satisfied_false_before_first_refresh() {
        // Empty entries must never read as "satisfied" — that would let the startup gate
        // flash past before the daemon has actually been asked.
        let panel = ModelsPanel::default();
        assert!(!panel.required_satisfied());
    }

    #[test]
    fn required_satisfied_true_once_all_required_present() {
        let panel = ModelsPanel {
            entries: vec![
                entry("stt-large-v3-turbo", true, true),
                entry("cleanup-qwen3-8b", false, false),
            ],
            ..Default::default()
        };
        assert!(panel.required_satisfied());
    }

    #[test]
    fn apply_event_updates_progress_then_marks_present_on_done() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };

        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 100,
            total: 574_041_195,
            phase: DownloadPhase::Downloading,
            error: None,
            ts_ms: 1,
        });
        let e = panel.entry("stt-large-v3-turbo").unwrap();
        assert!(e.downloading);
        assert_eq!(e.progress.unwrap().downloaded, 100);
        assert!(!e.present);

        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 574_041_195,
            total: 574_041_195,
            phase: DownloadPhase::Done,
            error: None,
            ts_ms: 2,
        });
        let e = panel.entry("stt-large-v3-turbo").unwrap();
        assert!(!e.downloading);
        assert!(e.present);
        assert!(e.progress.is_none());
        assert!(panel.required_satisfied());
    }

    #[test]
    fn apply_event_records_error_message_and_clears_downloading() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };
        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 0,
            total: 574_041_195,
            phase: DownloadPhase::Error,
            error: Some("integrity check failed".to_string()),
            ts_ms: 3,
        });
        let e = panel.entry("stt-large-v3-turbo").unwrap();
        assert!(!e.downloading);
        assert!(!e.present, "an error must never flip present");
        assert_eq!(
            panel.download_error("stt-large-v3-turbo"),
            Some("integrity check failed")
        );
    }

    #[test]
    fn apply_event_treats_a_user_cancel_as_missing_not_an_error() {
        // A cancel reports on the wire as phase=error, error="cancelled" (the fixed phase set
        // has no dedicated "cancelled" value) — the tray must not show a red error banner for
        // a user-initiated action; the row should just read "Missing" again.
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };
        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 12_345,
            total: 574_041_195,
            phase: DownloadPhase::Error,
            error: Some("cancelled".to_string()),
            ts_ms: 4,
        });
        let e = panel.entry("stt-large-v3-turbo").unwrap();
        assert!(!e.downloading);
        assert!(!e.present);
        assert_eq!(
            panel.download_error("stt-large-v3-turbo"),
            None,
            "a cancel must not surface as a download error"
        );
    }

    #[test]
    fn apply_event_cancel_after_a_real_error_clears_the_stale_error() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };
        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 0,
            total: 574_041_195,
            phase: DownloadPhase::Error,
            error: Some("integrity check failed".to_string()),
            ts_ms: 1,
        });
        assert!(panel.download_error("stt-large-v3-turbo").is_some());

        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 0,
            total: 574_041_195,
            phase: DownloadPhase::Error,
            error: Some("cancelled".to_string()),
            ts_ms: 2,
        });
        assert_eq!(panel.download_error("stt-large-v3-turbo"), None);
    }

    #[test]
    fn phase_distinguishes_verifying_from_downloading() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };
        panel.apply_event(&ApiEvent::ModelDownload {
            id: "stt-large-v3-turbo".to_string(),
            downloaded: 574_041_195,
            total: 574_041_195,
            phase: DownloadPhase::Verifying,
            error: None,
            ts_ms: 1,
        });
        assert_eq!(
            panel.phase("stt-large-v3-turbo"),
            Some(DownloadPhase::Verifying)
        );
        assert!(panel.entry("stt-large-v3-turbo").unwrap().downloading);
    }

    #[test]
    fn mark_download_started_flips_the_row_optimistically() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };
        panel.mark_download_started("stt-large-v3-turbo");
        let e = panel.entry("stt-large-v3-turbo").unwrap();
        assert!(e.downloading);
        assert_eq!(e.progress.unwrap().downloaded, 0);
        assert_eq!(
            panel.phase("stt-large-v3-turbo"),
            Some(DownloadPhase::Downloading)
        );
    }

    #[test]
    fn mark_download_started_is_a_no_op_for_unknown_id() {
        let mut panel = ModelsPanel::default();
        panel.mark_download_started("no-such-model");
        assert!(panel.entries.is_empty());
    }

    #[test]
    fn apply_event_ignores_unrelated_events() {
        let mut panel = ModelsPanel {
            entries: vec![entry("stt-large-v3-turbo", true, false)],
            ..Default::default()
        };
        panel.apply_event(&ApiEvent::AudioLevel {
            session_id: "s1".to_string(),
            rms: 0.1,
            peak: 0.2,
            ts_ms: 1,
        });
        assert!(!panel.entry("stt-large-v3-turbo").unwrap().downloading);
    }

    #[test]
    fn apply_event_for_unknown_id_is_a_safe_no_op() {
        let mut panel = ModelsPanel::default();
        panel.apply_event(&ApiEvent::ModelDownload {
            id: "no-such-model".to_string(),
            downloaded: 0,
            total: 0,
            phase: DownloadPhase::Downloading,
            error: None,
            ts_ms: 1,
        });
        assert!(panel.entries.is_empty());
    }
}
