//! GUI wiring for `whisper-tray` (D-0013). Three concurrent concerns meet here:
//!
//! - the **eframe/egui main thread** owns the window and the tray icon (macOS
//!   requires both on the main thread) and polls tray/menu events each frame;
//! - a **WS listener thread** connects to `WS /v1/events`, parses frames, and
//!   forwards them to the main thread over an mpsc channel, reconnecting with
//!   backoff on drop;
//! - **blocking HTTP calls** (ureq) run inline on button presses — localhost
//!   latency is negligible for an MVP (see known limits in the manual doc).
//!
//! Everything here is GUI glue and is verified manually; the testable logic it
//! calls (state machine, parsing, client, icons, onboarding, dictionary,
//! theme, validation, save-report formatting) lives in the library modules.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tungstenite::Message;

use whisper_ux::api::{
    ApiEvent, CleanupMode, Config, DeliveryMode, DownloadPhase, HotkeyMode, Incoming, ModelEntry,
};
use whisper_ux::client::{backoff_delay, events_ws_url, Api, ApiError, HttpClient};
use whisper_ux::daemon_ctl::DaemonController;
use whisper_ux::dict_editor::DictEditor;
use whisper_ux::icons::{self, IconImage};
use whisper_ux::language::{self, LanguagePreset};
use whisper_ux::models_ui::ModelsPanel;
use whisper_ux::ollama_ui::{self, OllamaAttention, OllamaPanel};
use whisper_ux::onboarding::{self, OnboardingState};
use whisper_ux::overlay::{self, Overlay, OverlayPhase};
use whisper_ux::permissions_ui::{self, PermissionsPanel};
use whisper_ux::save_report::SaveReport;
use whisper_ux::shell_settings::{self, OverlayPosition, ShellSettings};
use whisper_ux::theme::{self, status_pill::PillState};
use whisper_ux::tray_state::{TrayInput, TrayState};
use whisper_ux::validation;

/// Messages the WS listener thread sends to the UI thread.
enum WsMsg {
    Frame(Incoming),
    Connected,
    Disconnected,
}

/// Which page the window shows.
#[derive(PartialEq, Eq)]
enum Screen {
    Onboarding,
    App,
}

/// Which of the two D-0016 platform permissions an onboarding step / banner row is about —
/// selects the live status field, the onboarding ack flag, and the deep-link URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Permission {
    InputMonitoring,
    Accessibility,
}

/// Sections of the main app screen, navigated via the left sidebar (D-0013 UX
/// pass; sidebar layout added in UX pass 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Status,
    Dictation,
    Audio,
    Cleanup,
    Models,
    Dictionary,
    Advanced,
}

const SECTIONS: [Section; 7] = [
    Section::Status,
    Section::Dictation,
    Section::Audio,
    Section::Cleanup,
    Section::Models,
    Section::Dictionary,
    Section::Advanced,
];

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Status => "Status",
            Section::Dictation => "Dictation",
            Section::Audio => "Audio",
            Section::Cleanup => "Cleanup",
            Section::Models => "Models",
            Section::Dictionary => "Dictionary",
            Section::Advanced => "Advanced",
        }
    }

    /// One-line muted description shown under the section heading (UX pass 2:
    /// "a section heading + one-line muted description under it").
    fn description(self) -> &'static str {
        match self {
            Section::Status => "Live engine state, input level and the last transcript.",
            Section::Dictation => "Dictation language, global hotkey and delivery.",
            Section::Audio => "Voice activity detection and automatic gain control.",
            Section::Cleanup => "LLM disfluency cleanup mode and the local Ollama model.",
            Section::Models => "Local speech-recognition model download and raw model path.",
            Section::Dictionary => "Deterministic substitutions applied before cleanup.",
            Section::Advanced => "Server bind/port, history and dictionary file locations.",
        }
    }
}

/// Handles to the dynamic tray menu items so their labels can be updated.
struct MenuHandles {
    start_stop: MenuItem,
    settings: MenuItem,
    status: MenuItem,
    quit: MenuItem,
}

pub struct ShellApp {
    // --- API access ---
    client: HttpClient,
    base_url: String,

    // --- daemon lifecycle (managed vs adopted) ---
    daemon: DaemonController,

    // --- live state from WS ---
    rx: Receiver<WsMsg>,
    tray_state: TrayState,
    connected: bool,
    daemon_phase: String,
    active_session: Option<String>,
    last_rms: f32,
    last_peak: f32,
    last_partial: String,

    // --- tray ---
    tray: Option<TrayIcon>,
    menu: Option<MenuHandles>,

    // --- window / screens ---
    screen: Screen,
    section: Section,
    onboarding: OnboardingState,
    onboarding_path: Option<PathBuf>,

    // --- model provisioning (D-0015): startup gate + Settings→Models + onboarding all read
    // this one shared snapshot, kept current by periodic `GET /v1/models` polls and live
    // `model.download` WS events. ---
    models: ModelsPanel,
    last_models_poll: std::time::Instant,

    // --- platform permissions (D-0016): Input Monitoring (hotkey) / Accessibility (paste)
    // grant status, polled the same way as `models` — feeds the persistent main-window banner
    // and the onboarding checklist steps. ---
    permissions: PermissionsPanel,
    last_permissions_poll: std::time::Instant,

    // --- text cleanup via a local Ollama server (D-0017): status polled with the models poll
    // (same 5 s cadence) and live-updated by `ollama.status` WS events; feeds the Settings →
    // Cleanup card, the onboarding step and the attention banner. Cleanup is optional, so this
    // never gates the app. ---
    ollama: OllamaPanel,

    // --- floating overlay (shell-local, D-0013) ---
    shell_settings: ShellSettings,
    shell_settings_path: Option<PathBuf>,
    overlay: Overlay,
    /// Latch so a post-capture `Processing` state keeps the overlay up only if a
    /// `Recording` session actually preceded it (not on startup model-loading).
    overlay_active: bool,

    // --- settings working copy ---
    cfg: Option<Config>,
    // `Option<String>` config fields need a free-typing text buffer that only
    // round-trips to `Option` on save (an empty box means "unset"), mirroring
    // how the daemon treats an absent field.
    token_input: String,
    dictionary_path_input: String,
    history_path_input: String,
    dict: DictEditor,
    status_msg: Option<String>,
    save_report: SaveReport,
    test_dictation_text: String,
    /// Whether the Dictation section's language picker shows the free-text
    /// "Other…" ISO-code field. Tracked separately from `cfg.language` so
    /// revealing the field never clobbers the current value (Task 1: "never
    /// clobber it to a preset"); derived from `cfg.language` on load.
    language_other_mode: bool,
}

impl ShellApp {
    pub fn new(cc: &eframe::CreationContext<'_>, base_url: String, token: Option<String>) -> Self {
        // D-0013 UX pass: a calm, rounded dark theme instead of raw egui defaults.
        theme::apply(&cc.egui_ctx);

        let client = HttpClient::new(base_url.clone(), token.clone());

        // Daemon lifecycle: if nothing answers /v1/health, spawn whisperd as a
        // child (managed mode) so launching the app is enough; otherwise adopt the
        // already-running daemon and never touch it (external mode). See
        // `daemon_ctl` and D-0014.
        let reachable = client.health().is_ok();
        let daemon = DaemonController::start(reachable);

        // WS listener: reconnect forever with backoff, waking the UI on each frame.
        let (tx, rx) = mpsc::channel();
        let ws_url = events_ws_url(&base_url, token.as_deref());
        let egui_ctx = cc.egui_ctx.clone();
        thread::Builder::new()
            .name("ws-listener".into())
            .spawn(move || ws_listen_loop(ws_url, tx, egui_ctx))
            .ok();

        // Build the tray icon (best-effort: a headless CI/build environment or a
        // missing status bar must not crash the shell).
        let (tray, menu) = match build_tray(TrayState::Idle) {
            Ok((t, m)) => (Some(t), Some(m)),
            Err(_) => (None, None),
        };

        let onboarding_path = onboarding::default_state_path();
        let onboarding = onboarding_path
            .as_ref()
            .map(|p| OnboardingState::load(p))
            .unwrap_or_default();

        let shell_settings_path = shell_settings::default_shell_settings_path();
        let shell_settings = shell_settings_path
            .as_ref()
            .map(|p| ShellSettings::load(p))
            .unwrap_or_default();
        let screen = if onboarding.completed {
            Screen::App
        } else {
            Screen::Onboarding
        };

        let mut app = Self {
            client,
            base_url,
            daemon,
            rx,
            tray_state: TrayState::Idle,
            connected: false,
            daemon_phase: "unknown".to_string(),
            active_session: None,
            last_rms: 0.0,
            last_peak: 0.0,
            last_partial: String::new(),
            tray,
            menu,
            screen,
            section: Section::Status,
            onboarding,
            onboarding_path,
            models: ModelsPanel::default(),
            last_models_poll: std::time::Instant::now(),
            permissions: PermissionsPanel::default(),
            last_permissions_poll: std::time::Instant::now(),
            ollama: OllamaPanel::default(),
            shell_settings,
            shell_settings_path,
            overlay: Overlay::new(),
            overlay_active: false,
            cfg: None,
            token_input: String::new(),
            dictionary_path_input: String::new(),
            history_path_input: String::new(),
            dict: DictEditor::default(),
            status_msg: None,
            save_report: SaveReport::default(),
            test_dictation_text: String::new(),
            language_other_mode: false,
        };
        // Best-effort initial sync so the UI reflects the daemon immediately.
        app.refresh_status();
        app.load_settings();
        app.models.refresh(&app.client);
        app.refresh_permissions();
        app.ollama.refresh(&app.client);
        app
    }

    // -- daemon sync ------------------------------------------------------

    fn refresh_status(&mut self) {
        match self.client.status() {
            Ok(s) => {
                self.daemon_phase = s.phase;
                self.active_session = s.active_session;
                if let Some(m) = &mut self.menu {
                    m.status.set_text(format!("Daemon: {}", self.daemon_phase));
                }
            }
            Err(e) => {
                self.daemon_phase = "unreachable".to_string();
                self.status_msg = Some(format!("status: {e}"));
            }
        }
    }

    fn load_settings(&mut self) {
        match self.client.get_config() {
            Ok(cfg) => {
                self.token_input = cfg.token.clone().unwrap_or_default();
                self.dictionary_path_input = cfg.dictionary_path.clone().unwrap_or_default();
                self.history_path_input = cfg.history.path.clone().unwrap_or_default();
                self.language_other_mode =
                    language::preset_for(&cfg.language) == LanguagePreset::Other;
                self.cfg = Some(cfg);
            }
            Err(e) => self.status_msg = Some(format!("load config: {e}")),
        }
        match self.client.get_dictionary() {
            Ok(v) => {
                if let Ok(d) = DictEditor::from_json_value(&v) {
                    self.dict = d;
                }
            }
            Err(e) => self.status_msg = Some(format!("load dictionary: {e}")),
        }
    }

    /// Fold the `Option<String>` working buffers back into `cfg` (empty input
    /// means "unset", matching the daemon's `Option::None`).
    fn fold_optional_strings(&self, cfg: &mut Config) {
        cfg.token = non_empty(&self.token_input);
        cfg.dictionary_path = non_empty(&self.dictionary_path_input);
        cfg.history.path = non_empty(&self.history_path_input);
    }

    fn save_settings(&mut self) {
        let Some(cfg) = self.cfg.clone() else {
            return;
        };
        let mut cfg = cfg;
        self.fold_optional_strings(&mut cfg);

        // Validate config ranges client-side (mirrors the daemon's `PUT
        // /v1/config` `validate_config`) so a bad slider value is flagged
        // inline instead of round-tripping to a 400.
        let cfg_errs = validation::validate(&cfg);
        if !cfg_errs.is_empty() {
            self.status_msg = Some(format!(
                "config invalid: {}",
                cfg_errs
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            return;
        }

        // Validate the dictionary client-side before sending.
        let errs = self.dict.validate();
        if !errs.is_empty() {
            self.status_msg = Some(format!(
                "dictionary invalid: {}",
                errs.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            return;
        }

        let mut messages = Vec::new();
        match self.client.put_config(&cfg) {
            Ok(resp) => {
                self.cfg = Some(resp.config);
                self.save_report = SaveReport::new(resp.applied, resp.staged);
            }
            Err(e) => messages.push(format!("config error: {e}")),
        }
        match self.client.put_dictionary(&self.dict.to_json_value()) {
            Ok(_) => messages.push("dictionary saved.".to_string()),
            Err(ApiError::Http { message, .. }) => {
                messages.push(format!("dictionary rejected: {message}"))
            }
            Err(e) => messages.push(format!("dictionary error: {e}")),
        }
        self.status_msg = if messages.is_empty() {
            None
        } else {
            Some(messages.join(" "))
        };
    }

    fn toggle_dictation(&mut self) {
        if let Some(id) = self.active_session.clone() {
            match self.client.stop_session(&id) {
                Ok(_) => self.active_session = None,
                Err(e) => self.status_msg = Some(format!("stop: {e}")),
            }
        } else {
            match self.client.start_mic_session() {
                Ok(acc) => self.active_session = Some(acc.session_id),
                Err(e) => self.status_msg = Some(format!("start: {e}")),
            }
        }
    }

    // -- model provisioning (D-0015) --------------------------------------

    fn refresh_models(&mut self) {
        self.models.refresh(&self.client);
        self.ollama.refresh(&self.client);
        self.last_models_poll = std::time::Instant::now();
        if self.models.required_satisfied() {
            self.mark_models_ready();
        }
    }

    /// Poll `GET /v1/models` every few seconds so the startup gate reappears if a required
    /// model is deleted out from under a running tray (D-0015: "the gate reappears whenever a
    /// required model goes missing"), without hammering the daemon every frame.
    fn poll_models_if_due(&mut self) {
        if self.last_models_poll.elapsed() >= std::time::Duration::from_secs(5) {
            self.refresh_models();
        }
    }

    fn mark_models_ready(&mut self) {
        if !self.onboarding.models_ready {
            self.onboarding.models_ready = true;
            self.persist_onboarding();
        }
    }

    // -- text cleanup via Ollama (D-0017) ----------------------------------

    /// Latch `onboarding.ollama_ready` once Ollama is running with the selected model present,
    /// or cleanup is off. Cheap; run every frame so a download finishing, a start, or "Off"
    /// all latch without per-site wiring.
    fn update_ollama_ready(&mut self) {
        if self.onboarding.ollama_ready {
            return;
        }
        let model = self.cfg.as_ref().map(|c| c.cleanup.model.as_str());
        if ollama_ui::onboarding_ready(&self.ollama, &self.models, model) {
            self.onboarding.ollama_ready = true;
            self.persist_onboarding();
        }
    }

    /// "Use" / "Off": GET config → set `cleanup.model` → PUT config (applied at runtime), then
    /// re-poll models so the `selected` flags follow. Returns whether the daemon accepted it.
    fn set_cleanup_model(&mut self, model: &str) -> bool {
        match ollama_ui::set_cleanup_model(&self.client, model) {
            Ok(_) => {
                if let Some(cfg) = &mut self.cfg {
                    cfg.cleanup.model = model.to_string();
                }
                self.status_msg = None;
                self.refresh_models();
                true
            }
            Err(e) => {
                self.status_msg = Some(format!("cleanup model: {e}"));
                false
            }
        }
    }

    fn start_ollama(&mut self) {
        self.ollama.start(&self.client);
    }

    /// `POST /v1/models/{id}/download`, with an optimistic local flip to "downloading" so the
    /// button reacts immediately (the first WS tick can lag behind a slow connection setup).
    fn start_model_download(&mut self, id: &str) {
        match self.client.start_model_download(id) {
            Ok(()) => self.models.mark_download_started(id),
            Err(e) => self.status_msg = Some(format!("download {id}: {e}")),
        }
    }

    fn cancel_model_download(&mut self, id: &str) {
        if let Err(e) = self.client.cancel_model_download(id) {
            self.status_msg = Some(format!("cancel download {id}: {e}"));
        }
    }

    // -- platform permissions (D-0016) ------------------------------------

    /// `GET /v1/platform/permissions`, then auto-latch the onboarding acks a live `"granted"`
    /// covers so the checklist doesn't ask the user to also click "I granted it" once the real
    /// grant has already landed.
    fn refresh_permissions(&mut self) {
        self.permissions.refresh(&self.client);
        self.last_permissions_poll = std::time::Instant::now();
        self.mark_permission_acks();
    }

    /// Poll `GET /v1/platform/permissions` every few seconds so the banner reacts to a grant
    /// (or a lost grant, e.g. after a reinstall) without requiring the user to hit "Re-check"
    /// (mirrors [`Self::poll_models_if_due`], D-0015's polling cadence).
    fn poll_permissions_if_due(&mut self) {
        if self.last_permissions_poll.elapsed() >= std::time::Duration::from_secs(5) {
            self.refresh_permissions();
        }
    }

    fn mark_permission_acks(&mut self) {
        let mut changed = false;
        if self.permissions.input_monitoring == "granted" && !self.onboarding.input_monitoring_ack {
            self.onboarding.input_monitoring_ack = true;
            changed = true;
        }
        if self.permissions.accessibility == "granted" && !self.onboarding.accessibility_ack {
            self.onboarding.accessibility_ack = true;
            changed = true;
        }
        if changed {
            self.persist_onboarding();
        }
    }

    /// The one shared "model download" row (D-0015): name, size, status (Present ✓ / Missing /
    /// Downloading NN% / Verifying / Error) and a Download/Retry/Cancel button. Rendered from
    /// three places — the startup gate, Settings → Models, and the onboarding checklist.
    fn ui_model_row(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(entry) = self.models.entry(id).cloned() else {
            ui.horizontal(|ui| {
                ui.label(id);
                ui.label(
                    egui::RichText::new("(not reported by daemon)")
                        .weak()
                        .small(),
                );
            });
            return;
        };
        let error = self.models.download_error(&entry.id).map(str::to_string);
        let phase = self.models.phase(&entry.id);

        theme::card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let label = model_display_name(&entry.id);
                    ui.label(egui::RichText::new(label).strong());
                    let mut caption = entry.size_label();
                    if entry.required {
                        caption.push_str(" · required");
                    }
                    ui.label(egui::RichText::new(caption).weak().small());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if entry.downloading {
                        let verb = download_verb(&entry, phase);
                        if ui.button("Cancel").clicked() {
                            self.cancel_model_download(&entry.id);
                        }
                        ui.label(verb);
                    } else if entry.present {
                        ui.colored_label(theme::ACCENT, "Present ✓");
                    } else if let Some(err) = &error {
                        if ui.button("Retry").clicked() {
                            self.start_model_download(&entry.id);
                        }
                        ui.colored_label(
                            egui::Color32::from_rgb(0xE0, 0x3B, 0x24),
                            format!("Error: {err}"),
                        );
                    } else {
                        if ui.button("Download").clicked() {
                            self.start_model_download(&entry.id);
                        }
                        ui.label("Missing");
                    }
                });
            });
        });
        ui.add_space(6.0);
    }

    /// `true` once at least one `GET /v1/models` has succeeded and a required model is
    /// missing — the startup-gate condition (D-0015). Gated on a successful fetch so a
    /// transient "daemon unreachable" moment never flashes the gate instead of the normal
    /// "is the daemon running?" messaging.
    fn models_gate_active(&self) -> bool {
        !self.models.entries.is_empty() && !self.models.required_satisfied()
    }

    /// Blocking "Download models" screen (D-0015 surface 1): shown instead of the sidebar/app
    /// whenever [`Self::models_gate_active`] is true, including reappearing after an
    /// accidental deletion. Only the required STT model gates the app (text cleanup is
    /// optional and lives in Settings → Cleanup); it gets its row via the shared [`Self::ui_model_row`] component; it disappears on its own the
    /// frame after the required model's `model.download` `done` event lands.
    fn ui_model_gate(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Download models");
            ui.label(
                egui::RichText::new(
                    "Whisper needs its speech-recognition model before it can dictate. \
                     Downloads are one-time, stored locally, and never uploaded anywhere.",
                )
                .weak(),
            );
        });
        ui.add_space(theme::CARD_GAP);
        let ids: Vec<String> = self.models.stt_entries().map(|m| m.id.clone()).collect();
        for id in ids {
            self.ui_model_row(ui, &id);
        }
        if let Some(err) = self.models.fetch_error.clone() {
            ui.colored_label(egui::Color32::from_rgb(0xE0, 0x3B, 0x24), err);
        }
        if ui.button("Refresh").clicked() {
            self.refresh_models();
        }
    }

    /// Restart the tray-managed daemon (SIGINT → wait → respawn). Only reachable
    /// from the UI when [`DaemonController::is_managed`]; refuses otherwise.
    fn restart_daemon(&mut self) {
        match self.daemon.restart() {
            Ok(()) => self.status_msg = Some("Restarting whisperd…".to_string()),
            Err(e) => self.status_msg = Some(format!("restart: {e}")),
        }
    }

    /// A "Restart whisperd" button, enabled only in managed mode; in external mode
    /// it is disabled with an explanatory tooltip (the tray does not own that
    /// daemon). Shared by the Status card and the staged-config notice.
    fn ui_restart_button(&mut self, ui: &mut egui::Ui) {
        if self.daemon.is_managed() {
            if ui.button("Restart whisperd").clicked() {
                self.restart_daemon();
            }
        } else {
            ui.add_enabled(false, egui::Button::new("Restart whisperd"))
                .on_disabled_hover_text("daemon not managed by this app");
        }
    }

    // -- WS + tray event draining ----------------------------------------

    fn drain_ws(&mut self) {
        let mut state_changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            let input = match &msg {
                WsMsg::Connected => {
                    self.connected = true;
                    Some(TrayInput::ConnectionRestored)
                }
                WsMsg::Disconnected => {
                    self.connected = false;
                    Some(TrayInput::ConnectionLost)
                }
                WsMsg::Frame(Incoming::Known(ev)) => {
                    self.absorb_event(ev);
                    Some(TrayInput::Event(ev))
                }
                WsMsg::Frame(_) => None,
            };
            if let Some(input) = input {
                let next = self.tray_state.next(input);
                if next != self.tray_state {
                    self.tray_state = next;
                    state_changed = true;
                }
            }
            if matches!(msg, WsMsg::Connected) {
                // Resync authoritative state on (re)connect.
                self.refresh_status();
            }
        }
        if state_changed {
            self.apply_tray_icon();
        }
    }

    /// Pull display-only payloads (levels, partials, session id) out of events.
    fn absorb_event(&mut self, ev: &ApiEvent) {
        // Model-download progress (D-0015) updates the shared snapshot live, independent of
        // the dictation-session events matched below.
        self.models.apply_event(ev);
        if matches!(ev, ApiEvent::ModelDownload { .. }) && self.models.required_satisfied() {
            self.mark_models_ready();
        }
        self.ollama.apply_event(ev);
        match ev {
            ApiEvent::SessionStarted { session_id, .. } => {
                self.active_session = Some(session_id.clone());
                self.last_partial.clear();
                self.overlay.reset();
            }
            ApiEvent::AudioLevel { rms, peak, .. } => {
                self.last_rms = *rms;
                self.last_peak = *peak;
                self.overlay.push_level(*rms, *peak);
            }
            ApiEvent::PartialTranscript { segment_text, .. } => {
                self.last_partial = segment_text.clone();
            }
            ApiEvent::Delivered { text, .. } | ApiEvent::Cleaned { text, .. } => {
                self.active_session = None;
                self.last_rms = 0.0;
                self.last_peak = 0.0;
                self.test_dictation_text = text.clone();
            }
            ApiEvent::Cancelled { .. } | ApiEvent::Failed { .. } => {
                self.active_session = None;
                self.last_rms = 0.0;
                self.last_peak = 0.0;
            }
            _ => {}
        }
    }

    fn drain_menu_events(&mut self, ctx: &egui::Context) {
        // Snapshot the ids and drop the borrow of `self.menu` before the loop,
        // which calls `&mut self` handlers.
        let (start_stop, settings, quit) = match self.menu.as_ref() {
            Some(menu) => (
                menu.start_stop.id().clone(),
                menu.settings.id().clone(),
                menu.quit.id().clone(),
            ),
            None => return,
        };
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == start_stop {
                self.toggle_dictation();
            } else if ev.id == settings {
                self.screen = Screen::App;
                self.section = Section::Status;
                show_window(ctx);
            } else if ev.id == quit {
                // process::exit skips Drop, so kill a managed daemon explicitly
                // (external daemons are left running).
                self.daemon.shutdown();
                std::process::exit(0);
            }
        }
    }

    fn apply_tray_icon(&mut self) {
        if let Some(tray) = &self.tray {
            if let Some(icon) = make_icon(self.tray_state) {
                let _ = tray.set_icon(Some(icon));
            }
        }
        if let Some(menu) = &self.menu {
            let label = if self.active_session.is_some() {
                "Stop dictation"
            } else {
                "Start dictation"
            };
            menu.start_stop.set_text(label);
        }
    }

    fn persist_onboarding(&self) {
        if let Some(p) = &self.onboarding_path {
            let _ = self.onboarding.save(p);
        }
    }

    fn persist_shell_settings(&self) {
        if let Some(p) = &self.shell_settings_path {
            let _ = self.shell_settings.save(p);
        }
    }

    /// Drive the floating overlay viewport (D-0013). Visibility follows the tray
    /// state machine: shown while `Recording` (live waveform), kept during the
    /// post-capture `Processing` tail (frozen/dimmed + "…"), hidden on
    /// `Idle`/`Error`. Hotkey sessions arrive via WS `session_started` like any
    /// other, so they light the overlay for free.
    ///
    /// Uses an *immediate* second viewport: state (the ring + eased bars) lives
    /// in `self.overlay` and is borrowed directly by the render closure, which a
    /// deferred viewport's `Send + Sync + 'static` bound would forbid without an
    /// `Arc<Mutex<_>>`. The overlay is small, so the immediate viewport's shared
    /// parent/child repaint cost is negligible, and it only repaints while
    /// visible.
    fn show_overlay(&mut self, ctx: &egui::Context) {
        if !self.shell_settings.overlay_enabled {
            self.overlay_active = false;
            return;
        }
        let phase = match self.tray_state {
            TrayState::Recording => {
                self.overlay_active = true;
                OverlayPhase::Recording
            }
            TrayState::Processing if self.overlay_active => OverlayPhase::Processing,
            _ => {
                // Not rendering this frame closes the viewport (hide on idle/error).
                self.overlay_active = false;
                return;
            }
        };

        // Recompute placement each show so a monitor/resolution change is picked
        // up; fall back to a sane size when the platform reports none.
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(overlay::FALLBACK_MONITOR);
        let pos = overlay::overlay_rect(
            monitor,
            overlay::OVERLAY_SIZE,
            self.shell_settings.overlay_position,
        );

        let builder = egui::ViewportBuilder::default()
            .with_title("Whisper overlay")
            .with_inner_size(overlay::OVERLAY_SIZE)
            .with_min_inner_size(egui::vec2(100.0, 28.0))
            .with_position(pos)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top()
            // Request the OS not to focus the overlay on show, and let clicks
            // fall through to whatever the user is typing in. Honoring of both is
            // platform-dependent (see the manual-verification note).
            .with_active(false)
            .with_mouse_passthrough(true);

        let t = ctx.input(|i| i.time) as f32;
        let opacity = self.shell_settings.overlay_opacity;
        let overlay_state = &mut self.overlay;

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("whisper-overlay"),
            builder,
            move |vctx, class| {
                // A backend without multi-viewport support would embed this in a
                // window inside the settings viewport; that can't be a borderless
                // floating capsule, so skip rendering rather than draw it wrong.
                if class == egui::ViewportClass::EmbeddedWindow {
                    return;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(vctx, |ui| {
                        overlay_state.paint(ui, phase, t, opacity);
                    });
                // Animate at ~60 fps, but only while the overlay is on screen.
                vctx.request_repaint_after(std::time::Duration::from_millis(16));
            },
        );
    }

    fn pill_state(&self) -> PillState {
        theme::status_pill::from_tray(self.connected, self.tray_state)
    }

    // -- rendering: shared chrome ------------------------------------------

    fn ui_recording_banner(&self, ui: &mut egui::Ui) {
        if self.tray_state == TrayState::Recording || self.active_session.is_some() {
            theme::card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("● Recording").color(egui::Color32::RED));
                    level_meter(ui, self.last_rms, self.last_peak, 180.0);
                });
                if !self.last_partial.is_empty() {
                    ui.label(format!("… {}", self.last_partial));
                }
            });
            ui.add_space(6.0);
        }
    }

    /// Persistent, actionable permission banner (D-0016): shown whenever Input Monitoring
    /// and/or Accessibility is not confirmed `"granted"`, naming the exact permission and
    /// deep-linking straight to its System Settings pane. This is the tray-side half of closing
    /// the silent-hotkey-failure gap — the daemon can detect the missing grant, but only the
    /// tray can put it in front of the user.
    fn ui_permissions_banner(&mut self, ui: &mut egui::Ui) {
        if !self.permissions.needs_attention() {
            return;
        }
        theme::card_frame().show(ui, |ui| {
            ui.label(
                egui::RichText::new("⚠ Permission needed")
                    .color(egui::Color32::from_rgb(0xF2, 0x9D, 0x1C))
                    .strong(),
            );
            if self.permissions.input_monitoring_needs_attention() {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Input Monitoring: {} — the global hotkey will not work.",
                        self.permissions.input_monitoring
                    ));
                    if ui.button("Open System Settings").clicked() {
                        permissions_ui::open_system_settings(
                            permissions_ui::INPUT_MONITORING_SETTINGS_URL,
                        );
                    }
                });
            }
            if self.permissions.accessibility_needs_attention() {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Accessibility: {} — paste into other apps will not work.",
                        self.permissions.accessibility
                    ));
                    if ui.button("Open System Settings").clicked() {
                        permissions_ui::open_system_settings(
                            permissions_ui::ACCESSIBILITY_SETTINGS_URL,
                        );
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(
                        "After granting, restart Whisper — the listener starts once at daemon startup.",
                    )
                    .weak()
                    .small(),
                );
                if ui.button("Re-check").clicked() {
                    self.refresh_permissions();
                }
            });
        });
        ui.add_space(6.0);
    }

    /// Actionable text-cleanup banner (D-0017), same placement/pattern as the permission banner:
    /// shown while cleanup is on and Ollama is missing/stopped or the selected model is not
    /// pulled, with the matching button. Cleanup is optional, so it only nags — never blocks.
    fn ui_ollama_banner(&mut self, ui: &mut egui::Ui) {
        let Some(model) = self.cfg.as_ref().map(|c| c.cleanup.model.clone()) else {
            return;
        };
        let Some(attention) = self.ollama.attention(&model, &self.models) else {
            return;
        };
        theme::card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("⚠ {}", attention.banner_text()))
                        .color(egui::Color32::from_rgb(0xF2, 0x9D, 0x1C)),
                );
                match &attention {
                    OllamaAttention::NotInstalled => {
                        if ui.button("Install Ollama").clicked() {
                            permissions_ui::open_system_settings(ollama_ui::OLLAMA_DOWNLOAD_URL);
                        }
                    }
                    OllamaAttention::NotRunning { can_start } => {
                        if ui
                            .add_enabled(*can_start, egui::Button::new("Start"))
                            .clicked()
                        {
                            self.start_ollama();
                        }
                    }
                    OllamaAttention::ModelMissing { id, .. } => {
                        if let Some(id) = id {
                            if ui.button("Download").clicked() {
                                self.start_model_download(id);
                            }
                        }
                    }
                }
            });
        });
        ui.add_space(6.0);
    }

    /// The persistent "restart to apply" notice (feedback item 2): stays
    /// visible on every section, not just the one that triggered the save,
    /// until the next save clears it.
    fn ui_staged_notice(&mut self, ui: &mut egui::Ui) {
        if let Some(notice) = self.save_report.staged_notice() {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(0xF2, 0x9D, 0x1C), notice);
                self.ui_restart_button(ui);
            });
            ui.add_space(4.0);
        }
        if let Some(applied) = self.save_report.applied_summary() {
            ui.label(egui::RichText::new(applied).weak().small());
        }
    }

    /// The sidebar's nav list: app name + status pill (top), then one row per
    /// [`Section`] with a selected-state accent bar/highlight and a hover
    /// effect (UX pass 2: proper left sidebar instead of a tab bar).
    fn ui_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Whisper").heading());
        ui.add_space(6.0);
        status_pill(ui, self.pill_state());
        ui.add_space(18.0);
        for section in SECTIONS {
            self.ui_nav_item(ui, section);
        }
    }

    fn ui_nav_item(&mut self, ui: &mut egui::Ui, section: Section) {
        let selected = self.section == section;
        let desired = egui::vec2(ui.available_width(), 32.0);
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
        if response.clicked() {
            self.section = section;
        }
        if ui.is_rect_visible(rect) {
            let radius = theme::CORNER_RADIUS as f32;
            if selected {
                ui.painter()
                    .rect_filled(rect, radius, egui::Color32::from_gray(40));
                let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
                ui.painter().rect_filled(bar, 0.0, theme::ACCENT);
            } else if response.hovered() {
                ui.painter()
                    .rect_filled(rect, radius, egui::Color32::from_gray(30));
            }
            let text_color = if selected {
                egui::Color32::WHITE
            } else {
                ui.visuals().text_color()
            };
            ui.painter().text(
                rect.left_center() + egui::vec2(16.0, 0.0),
                egui::Align2::LEFT_CENTER,
                section.label(),
                egui::FontId::proportional(14.0),
                text_color,
            );
        }
        ui.add_space(2.0);
    }

    // -- rendering: sections -------------------------------------------------

    /// Status dashboard (UX pass 2): a big level meter + status pill + primary
    /// action up top, session/process info laid out on a two-column grid below.
    fn ui_status_section(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                status_pill(ui, self.pill_state());
                ui.label(
                    egui::RichText::new(format!("daemon: {}", self.daemon_phase))
                        .weak()
                        .small(),
                );
            });
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Input level").weak().small());
            let width = ui.available_width();
            level_meter_sized(ui, self.last_rms, self.last_peak, width, 28.0);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let label = if self.active_session.is_some() {
                    "Stop dictation"
                } else {
                    "Start dictation"
                };
                if primary_button(ui, label).clicked() {
                    self.toggle_dictation();
                }
                self.ui_restart_button(ui);
            });
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            let mode = if self.daemon.is_managed() {
                "managed by this app"
            } else if self.daemon.is_external() {
                "external (started elsewhere)"
            } else {
                "not running"
            };
            egui::Grid::new("status_dashboard_grid")
                .num_columns(2)
                .spacing([32.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Session").strong());
                    ui.label(egui::RichText::new("Process").strong());
                    ui.end_row();

                    ui.vertical(|ui| {
                        let active = self.active_session.as_deref().unwrap_or("none");
                        ui.label(format!("active: {active}"));
                        if !self.last_partial.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("… {}", self.last_partial)).small(),
                            );
                        }
                    });
                    ui.vertical(|ui| {
                        ui.label(format!("mode: {mode}"));
                        ui.label(
                            egui::RichText::new(format!(
                                "log: {}",
                                self.daemon.log_path().display()
                            ))
                            .small(),
                        );
                    });
                    ui.end_row();
                });
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Last transcript");
            if self.test_dictation_text.is_empty() && self.last_partial.is_empty() {
                ui.label(egui::RichText::new("(nothing yet)").weak());
            } else if !self.last_partial.is_empty() {
                ui.label(format!("… {}", self.last_partial));
            } else {
                ui.label(&self.test_dictation_text);
            }
        });
    }

    fn ui_dictation_section(&mut self, cfg: &mut Config, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Language");
            self.ui_language_picker(cfg, ui);
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Global hotkey");
            labeled_toggle(ui, &mut cfg.hotkey.enabled, "Enabled");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Combo");
                ui.text_edit_singleline(&mut cfg.hotkey.combo);
            });
            ui.add_space(4.0);
            ui.label("Mode");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut cfg.hotkey.mode, HotkeyMode::PushToTalk, "Push to talk");
                ui.selectable_value(&mut cfg.hotkey.mode, HotkeyMode::Toggle, "Toggle");
            });
            ui.label(
                egui::RichText::new("Hotkey changes are staged until the daemon restarts.")
                    .weak()
                    .small(),
            );
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Delivery");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut cfg.delivery, DeliveryMode::None, "None (API/WS only)");
                ui.selectable_value(
                    &mut cfg.delivery,
                    DeliveryMode::Paste,
                    "Paste into focused app",
                );
            });
        });
        ui.add_space(theme::CARD_GAP);
        self.ui_overlay_card(ui);
    }

    /// Dictation-language picker (Task 1): preset buttons for cs/en/auto plus
    /// an "Other…" affordance that reveals a free-text ISO-code field. The
    /// preset shown is always derived from `cfg.language` so a value the
    /// daemon returns that isn't one of the three known codes round-trips
    /// byte-exact — clicking "Other…" only reveals the text field, it never
    /// overwrites `cfg.language`.
    fn ui_language_picker(&mut self, cfg: &mut Config, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let current = language::preset_for(&cfg.language);
            for preset in language::PRESETS {
                if ui
                    .selectable_label(
                        current == preset && !self.language_other_mode,
                        preset.label(),
                    )
                    .clicked()
                {
                    if let Some(code) = preset.code() {
                        cfg.language = code.to_string();
                    }
                    self.language_other_mode = false;
                }
            }
            if ui
                .selectable_label(self.language_other_mode, LanguagePreset::Other.label())
                .clicked()
            {
                self.language_other_mode = true;
            }
        });
        if self.language_other_mode {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("ISO code");
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.language)
                        .hint_text("e.g. de, sk, fr")
                        .desired_width(100.0),
                );
            });
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Applied live by the daemon — no restart needed.")
                .weak()
                .small(),
        );
    }

    /// "Floating overlay" card: enable toggle + Top/Bottom edge. Persisted to the
    /// shell-local settings file immediately on change (not daemon `/v1` config).
    fn ui_overlay_card(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Floating overlay");
            let mut changed = false;
            let mut enabled = self.shell_settings.overlay_enabled;
            if ui
                .horizontal(|ui| {
                    let r = toggle_switch(ui, &mut enabled);
                    ui.label("Show waveform while dictating");
                    r.changed()
                })
                .inner
            {
                self.shell_settings.overlay_enabled = enabled;
                changed = true;
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Position");
                let mut pos = self.shell_settings.overlay_position;
                changed |= ui
                    .selectable_value(&mut pos, OverlayPosition::Top, "Top")
                    .clicked();
                changed |= ui
                    .selectable_value(&mut pos, OverlayPosition::Bottom, "Bottom")
                    .clicked();
                self.shell_settings.overlay_position = pos;
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Opacity");
                let mut opacity = self.shell_settings.overlay_opacity;
                if ui
                    .add(egui::Slider::new(&mut opacity, 0.2..=1.0).show_value(false))
                    .changed()
                {
                    self.shell_settings.overlay_opacity = opacity;
                    changed = true;
                }
            });

            ui.label(
                egui::RichText::new("Always centered horizontally. Applies to the next session.")
                    .weak()
                    .small(),
            );

            if changed {
                self.persist_shell_settings();
            }
        });
    }

    fn ui_audio_section(&mut self, cfg: &mut Config, ui: &mut egui::Ui) {
        let errs = validation::validate(cfg);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Voice activity detection");
            ui.add(
                egui::Slider::new(&mut cfg.vad.threshold, 0.05..=0.95)
                    .step_by(0.01)
                    .text("Threshold"),
            );
            ui.label(
                egui::RichText::new("Lower = more sensitive (picks up quieter speech).")
                    .weak()
                    .small(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::DragValue::new(&mut cfg.vad.hangover_ms)
                    .range(50..=2000)
                    .suffix(" ms")
                    .prefix("Hangover: "),
            );
            ui.add_space(4.0);
            ui.add(
                egui::DragValue::new(&mut cfg.vad.max_silence_ms)
                    .range(50..=10_000)
                    .suffix(" ms")
                    .prefix("Max silence: "),
            );
            for e in &errs {
                if matches!(
                    e,
                    validation::ConfigError::VadThresholdRange(_)
                        | validation::ConfigError::VadMaxSilenceRange(_)
                        | validation::ConfigError::VadHangoverExceedsMaxSilence { .. }
                ) {
                    ui.colored_label(egui::Color32::from_rgb(0xE0, 0x3B, 0x24), e.to_string());
                }
            }
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Automatic gain control");
            labeled_toggle(ui, &mut cfg.audio.agc_enabled, "Enabled");
            ui.add_space(6.0);
            ui.add(
                egui::Slider::new(&mut cfg.audio.agc_target_peak, 0.1..=1.0)
                    .step_by(0.01)
                    .text("Target peak"),
            );
            ui.add(
                egui::Slider::new(&mut cfg.audio.agc_max_gain, 1.0..=24.0)
                    .step_by(0.5)
                    .text("Max gain"),
            );
            for e in &errs {
                if matches!(
                    e,
                    validation::ConfigError::AudioTargetPeakRange(_)
                        | validation::ConfigError::AudioMaxGainRange(_)
                ) {
                    ui.colored_label(egui::Color32::from_rgb(0xE0, 0x3B, 0x24), e.to_string());
                }
            }
            ui.label(
                egui::RichText::new("Audio changes are staged until the daemon restarts.")
                    .weak()
                    .small(),
            );
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Live level (for tuning)");
            level_meter(ui, self.last_rms, self.last_peak, 260.0);
        });
    }

    fn ui_cleanup_section(&mut self, cfg: &mut Config, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Cleanup");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut cfg.cleanup_mode, CleanupMode::Auto, "auto");
                ui.selectable_value(&mut cfg.cleanup_mode, CleanupMode::Always, "always");
                ui.selectable_value(&mut cfg.cleanup_mode, CleanupMode::Never, "never");
            });
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Text cleanup (Ollama)");
            self.ui_ollama_body(ui, cfg, false);
        });
    }

    /// Body of the "Text cleanup (Ollama)" card, shared by Settings → Cleanup and the onboarding
    /// step: server status + Install/Start, the "Off" option, the validated models, then the
    /// installed-but-unvalidated extras. `onboarding` adds the "Skip (no cleanup)" action.
    fn ui_ollama_body(&mut self, ui: &mut egui::Ui, cfg: &mut Config, onboarding: bool) {
        ui.label(self.ollama.status_label());
        if let Some(st) = self.ollama.status.clone() {
            if !st.installed {
                ui.horizontal(|ui| {
                    if ui.button("Install Ollama").clicked() {
                        permissions_ui::open_system_settings(ollama_ui::OLLAMA_DOWNLOAD_URL);
                    }
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(ollama_ui::BREW_INSTALL_HINT).monospace(),
                        )
                        .selectable(true),
                    );
                });
            } else if !st.running {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(st.can_start, egui::Button::new("Start"))
                        .clicked()
                    {
                        self.start_ollama();
                    }
                    if !st.can_start {
                        ui.label(
                            egui::RichText::new("Start it manually (the URL is not local).")
                                .weak()
                                .small(),
                        );
                    }
                });
            }
        }
        if let Some(err) = self.ollama.last_error.clone() {
            ui.colored_label(egui::Color32::from_rgb(0xE0, 0x3B, 0x24), err);
        }
        ui.add_space(6.0);

        if ui
            .selectable_label(!cfg.cleanup.enabled(), "Off (no cleanup)")
            .clicked()
            && cfg.cleanup.enabled()
            && self.set_cleanup_model("")
        {
            cfg.cleanup.model.clear();
        }
        ui.add_space(4.0);

        let validated: Vec<ModelEntry> = self
            .models
            .cleanup_validated()
            .into_iter()
            .cloned()
            .collect();
        for entry in &validated {
            self.ui_cleanup_model_row(ui, cfg, entry);
        }
        let extras: Vec<ModelEntry> = self
            .models
            .cleanup_unvalidated()
            .into_iter()
            .cloned()
            .collect();
        if !extras.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Installed in Ollama — not validated for Czech").strong());
            for entry in &extras {
                self.ui_cleanup_model_row(ui, cfg, entry);
            }
        }
        if onboarding {
            ui.add_space(6.0);
            if ui.button("Skip (no cleanup)").clicked() && self.set_cleanup_model("") {
                cfg.cleanup.model.clear();
                self.onboarding.ollama_ready = true;
                self.persist_onboarding();
            }
        }
    }

    /// One Ollama cleanup model: name, size, and Download / Cancel / Use / "In use ✓".
    fn ui_cleanup_model_row(&mut self, ui: &mut egui::Ui, cfg: &mut Config, entry: &ModelEntry) {
        let error = self.models.download_error(&entry.id).map(str::to_string);
        let phase = self.models.phase(&entry.id);
        let tag = entry.ollama_tag.clone().unwrap_or_default();
        theme::card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(model_display_name(&entry.id)).strong());
                    ui.label(
                        egui::RichText::new(format!("{tag} · {}", entry.size_label()))
                            .weak()
                            .small(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if entry.downloading {
                        if ui.button("Cancel").clicked() {
                            self.cancel_model_download(&entry.id);
                        }
                        ui.label(download_verb(entry, phase));
                    } else if entry.present {
                        if entry.selected {
                            ui.colored_label(theme::ACCENT, "In use ✓");
                        } else if ui.button("Use").clicked() && self.set_cleanup_model(&tag) {
                            cfg.cleanup.model = tag.clone();
                        }
                    } else {
                        if ui.button("Download").clicked() {
                            self.start_model_download(&entry.id);
                        }
                        if let Some(err) = &error {
                            ui.colored_label(
                                egui::Color32::from_rgb(0xE0, 0x3B, 0x24),
                                format!("Error: {err}"),
                            );
                        } else {
                            ui.label("Not downloaded");
                        }
                    }
                });
            });
        });
        ui.add_space(6.0);
    }

    fn ui_models_section(&mut self, cfg: &mut Config, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Models");
            ui.label(
                egui::RichText::new(
                    "In-app download, verified against a pinned checksum before use. This is \
                     the primary way to (re-)provision a model — e.g. after an accidental \
                     deletion.",
                )
                .weak()
                .small(),
            );
            if ui.button("Refresh").clicked() {
                self.refresh_models();
            }
        });
        ui.add_space(theme::CARD_GAP);
        let ids: Vec<String> = self.models.stt_entries().map(|m| m.id.clone()).collect();
        for id in ids {
            self.ui_model_row(ui, &id);
        }
        if let Some(err) = self.models.fetch_error.clone() {
            ui.colored_label(egui::Color32::from_rgb(0xE0, 0x3B, 0x24), err);
        }
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            egui::CollapsingHeader::new("Advanced: raw model paths")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "Power users only — point the daemon at a custom speech-recognition \
                             model file instead of the managed download.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.horizontal(|ui| {
                        ui.label("STT");
                        ui.text_edit_singleline(&mut cfg.models.stt);
                    });
                    ui.label(
                        egui::RichText::new(
                            "Model path changes are staged until the daemon restarts.",
                        )
                        .weak()
                        .small(),
                    );
                });
        });
    }

    fn ui_dictionary_section(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Dictionary");
            ui.label(
                egui::RichText::new(
                    "Deterministic substitutions applied before cleanup (Czech-safe, case-sensitive).",
                )
                .weak()
                .small(),
            );
            ui.add_space(6.0);
            let mut remove: Option<usize> = None;
            egui::Grid::new("dict_grid")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("from");
                    ui.label("to");
                    ui.label("");
                    ui.end_row();
                    for (i, pair) in self.dict.pairs.iter_mut().enumerate() {
                        ui.text_edit_singleline(&mut pair.from);
                        ui.text_edit_singleline(&mut pair.to);
                        if ui.button("✕").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
            if let Some(i) = remove {
                self.dict.remove(i);
            }
            if ui.button("+ Add row").clicked() {
                self.dict.add_blank();
            }
            for err in self.dict.validate() {
                ui.colored_label(egui::Color32::from_rgb(0xE0, 0x3B, 0x24), err.to_string());
            }
        });
    }

    fn ui_advanced_section(&mut self, cfg: &mut Config, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Server");
            ui.horizontal(|ui| {
                ui.label("Bind");
                ui.text_edit_singleline(&mut cfg.bind);
            });
            ui.horizontal(|ui| {
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut cfg.port).range(1..=65_535));
            });
            ui.horizontal(|ui| {
                ui.label("Token");
                ui.add(
                    egui::TextEdit::singleline(&mut self.token_input)
                        .password(true)
                        .hint_text("(none)"),
                );
            });
            ui.label(egui::RichText::new(&self.base_url).weak().small());
            ui.label(
                egui::RichText::new("Bind/port changes are staged until the daemon restarts.")
                    .weak()
                    .small(),
            );
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("History");
            labeled_toggle(
                ui,
                &mut cfg.history.enabled,
                "Enabled (opt-in transcript log)",
            );
            ui.horizontal(|ui| {
                ui.label("Path");
                ui.add(
                    egui::TextEdit::singleline(&mut self.history_path_input)
                        .hint_text("(default XDG data dir)"),
                );
            });
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Dictionary file");
            ui.horizontal(|ui| {
                ui.label("Path");
                ui.add(
                    egui::TextEdit::singleline(&mut self.dictionary_path_input)
                        .hint_text("(none — dictionary lives only in daemon memory)"),
                );
            });
        });
    }

    fn ui_onboarding(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Welcome to Whisper");
            ui.label("Local-first Czech/English dictation. Let's get set up.");
        });
        ui.add_space(theme::CARD_GAP);
        theme::card_frame().show(ui, |ui| {
            let daemon_ok = self.daemon_phase == "ready" || self.daemon_phase == "degraded";
            checklist_row(ui, daemon_ok, "1. Daemon is running");
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                if ui.button("Check daemon").clicked() {
                    self.refresh_status();
                    if daemon_ok {
                        self.onboarding.daemon_seen = true;
                    }
                    self.onboarding.daemon_seen |= self.daemon_phase != "unreachable";
                    self.persist_onboarding();
                }
                ui.label(format!("status: {}", self.daemon_phase));
            });

            // D-0015: the required speech-recognition model, downloaded in-app (the same
            // widget the Settings → Models card and the startup gate use). Test dictation
            // (step 5) needs this satisfied first.
            checklist_row(ui, self.onboarding.models_ready, "2. Required models");
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                if ui.button("Check models").clicked() {
                    self.refresh_models();
                }
            });
            {
                let ids: Vec<String> = self.models.stt_entries().map(|m| m.id.clone()).collect();
                for id in ids {
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.vertical(|ui| self.ui_model_row(ui, &id));
                    });
                }
            }

            // D-0017: optional text cleanup through a local Ollama. Skippable ("Skip (no cleanup)"
            // sets the model to "" and latches `ollama_ready`); it never blocks dictation.
            checklist_row(
                ui,
                self.onboarding.ollama_ready,
                "3. Text cleanup (Ollama, optional)",
            );
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.vertical(|ui| match self.cfg.take() {
                    Some(mut cfg) => {
                        self.ui_ollama_body(ui, &mut cfg, true);
                        self.cfg = Some(cfg);
                    }
                    None => {
                        ui.label("No config loaded — is the daemon running?");
                    }
                });
            });

            checklist_row(ui, self.onboarding.mic_ack, "4. Microphone permission");
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(
                    "Grant in System Settings → Privacy & Security → Microphone (for whisperd).",
                );
                if ui.button("I granted it").clicked() {
                    self.onboarding.mic_ack = true;
                    self.persist_onboarding();
                }
            });

            // D-0016: unlike the ack-only steps above, this reads the *real* grant from
            // `GET /v1/platform/permissions` (closing the "user ticked the box but the grant
            // never actually took" gap) — "Re-check" re-polls, and the manual "I granted it"
            // fallback only appears when the daemon can't say (`"unknown"`, e.g. an older
            // daemon build without this endpoint).
            self.ui_permission_onboarding_step(
                ui,
                "5. Input Monitoring permission (global hotkey)",
                permissions_ui::INPUT_MONITORING_SETTINGS_URL,
                Permission::InputMonitoring,
            );
            self.ui_permission_onboarding_step(
                ui,
                "6. Accessibility permission (paste into other apps)",
                permissions_ui::ACCESSIBILITY_SETTINGS_URL,
                Permission::Accessibility,
            );

            checklist_row(ui, self.onboarding.test_dictation_done, "7. Test dictation");
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                let label = if self.active_session.is_some() {
                    "Stop"
                } else {
                    "Start test"
                };
                if ui.button(label).clicked() {
                    self.toggle_dictation();
                }
            });
            if !self.test_dictation_text.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.label(format!("Heard: {}", self.test_dictation_text));
                });
                if !self.onboarding.test_dictation_done {
                    self.onboarding.test_dictation_done = true;
                    self.persist_onboarding();
                }
            }
        });

        ui.add_space(theme::CARD_GAP);
        ui.horizontal(|ui| {
            if ui.button("Finish").clicked() {
                self.onboarding.completed = true;
                self.persist_onboarding();
                self.screen = Screen::App;
                self.load_settings();
            }
            if ui.button("Skip for now").clicked() {
                self.onboarding.completed = true;
                self.persist_onboarding();
                self.screen = Screen::App;
            }
        });
    }

    /// One onboarding checklist row for a D-0016 platform permission: live status from the last
    /// `GET /v1/platform/permissions` poll, a "Re-check" button, a "Open System Settings"
    /// deep-link button, and — only while the status is `"unknown"` (the daemon can't say, e.g.
    /// an older build without this endpoint) — a manual "I granted it" fallback ack. A live
    /// `"granted"` reading auto-latches the ack (see [`Self::mark_permission_acks`]), so the
    /// fallback button never appears once the daemon can confirm the grant itself.
    fn ui_permission_onboarding_step(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        settings_url: &'static str,
        which: Permission,
    ) {
        let (status, done) = match which {
            Permission::InputMonitoring => (
                self.permissions.input_monitoring.clone(),
                self.onboarding.input_monitoring_ack,
            ),
            Permission::Accessibility => (
                self.permissions.accessibility.clone(),
                self.onboarding.accessibility_ack,
            ),
        };
        checklist_row(ui, done, label);
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.label(format!("status: {status}"));
            if ui.button("Re-check").clicked() {
                self.refresh_permissions();
            }
            if ui.button("Open System Settings").clicked() {
                permissions_ui::open_system_settings(settings_url);
            }
        });
        if status != "granted" {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label("Grant it, then Re-check.");
                if status == "unknown" && ui.button("I granted it").clicked() {
                    match which {
                        Permission::InputMonitoring => self.onboarding.input_monitoring_ack = true,
                        Permission::Accessibility => self.onboarding.accessibility_ack = true,
                    }
                    self.persist_onboarding();
                }
            });
        }
    }

    /// Content area for `Screen::App`: section heading + muted description
    /// (UX pass 2), Reload/Save row (Save is the primary accent-filled
    /// action), the staged-config notice, then the selected section.
    fn ui_app(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(self.section.label()).heading());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary_button(ui, "Save").clicked() {
                    self.save_settings();
                }
                if ui.button("Reload from daemon").clicked() {
                    self.load_settings();
                }
            });
        });
        ui.label(
            egui::RichText::new(self.section.description())
                .weak()
                .small(),
        );
        ui.add_space(10.0);
        self.ui_staged_notice(ui);

        if self.cfg.is_none() {
            ui.label("No config loaded — is the daemon running?");
            return;
        }

        // Work on a temporary so the borrow of self.cfg doesn't collide with
        // helper methods; write back at the end.
        let mut cfg = self.cfg.take().unwrap();
        match self.section {
            Section::Status => self.ui_status_section(ui),
            Section::Dictation => self.ui_dictation_section(&mut cfg, ui),
            Section::Audio => self.ui_audio_section(&mut cfg, ui),
            Section::Cleanup => self.ui_cleanup_section(&mut cfg, ui),
            Section::Models => self.ui_models_section(&mut cfg, ui),
            Section::Dictionary => self.ui_dictionary_section(ui),
            Section::Advanced => self.ui_advanced_section(&mut cfg, ui),
        }
        self.cfg = Some(cfg);
    }
}

impl eframe::App for ShellApp {
    // eframe's default clear color is opaque (`visuals.panel_fill`) and is used
    // to clear every viewport's swapchain surface each frame, including the
    // overlay viewport — even though its `ViewportBuilder` sets
    // `.with_transparent(true)`. An opaque clear paints a solid square behind
    // the overlay before the rounded capsule is drawn on top of it, so make the
    // clear itself transparent. This is safe for the main settings window too:
    // its content is drawn inside eframe's default `CentralPanel`, which paints
    // its own opaque `panel_fill` rect covering the whole window regardless of
    // the clear color, so this only actually matters for the overlay viewport.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    // Non-painting work (eframe 0.35 splits `logic` from `ui`): drain background
    // channels and handle the window-close request here, where the `Context` is
    // available.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_ws();
        self.drain_menu_events(ctx);
        self.poll_models_if_due();
        self.poll_permissions_if_due();
        self.update_ollama_ready();

        // Drive the floating overlay viewport here (not in `ui()`): `logic` runs
        // every frame regardless of the main window's visibility, while `ui` is
        // skipped whenever the window is hidden (close-to-tray steady state) —
        // and the overlay must still appear while dictating with no window open.
        self.show_overlay(ctx);

        // Close-to-tray: hide the window instead of quitting (D-0013 §6).
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // Keep repainting so background WS/tray messages are drained even when
        // the window is idle or hidden.
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Startup gate (D-0015): once past onboarding, a missing *required* model blocks
        // normal use — reappears any time `GET /v1/models` (polled every few seconds, plus
        // live `model.download` events) shows it missing again, e.g. after an accidental
        // deletion.
        let gate_active = self.screen == Screen::App && self.models_gate_active();

        // Onboarding/the gate have no section nav, so they keep a slim top bar instead of the
        // sidebar (UX pass 2 restyles it lightly; the flow is unchanged).
        if self.screen == Screen::Onboarding || gate_active {
            egui::Panel::top("top_onboarding").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Whisper").heading());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        status_pill(ui, self.pill_state());
                    });
                });
            });
        } else {
            // Left sidebar: app name + status pill + section nav (UX pass 2:
            // replaces the old top tab bar).
            egui::Panel::left("sidebar")
                .exact_size(150.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(theme::sidebar_frame())
                .show(ui, |ui| self.ui_sidebar(ui));
        }

        egui::CentralPanel::default()
            .frame(theme::content_frame())
            .show(ui, |ui| {
                self.ui_recording_banner(ui);
                // Onboarding has its own inline live-status + deep-link flow per step (below);
                // showing the summary banner there too would just duplicate it.
                if self.screen != Screen::Onboarding {
                    self.ui_permissions_banner(ui);
                    self.ui_ollama_banner(ui);
                }
                egui::ScrollArea::vertical().show(ui, |ui| match self.screen {
                    Screen::Onboarding => self.ui_onboarding(ui),
                    Screen::App if gate_active => self.ui_model_gate(ui),
                    Screen::App => self.ui_app(ui),
                });
                if let Some(msg) = &self.status_msg {
                    ui.separator();
                    ui.label(egui::RichText::new(msg).weak().small());
                }
            });
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Trim `s`; an empty result becomes `None` (the daemon's "unset" convention
/// for `Option<String>` config fields).
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// "Verifying…" / "Downloading NN%"; "Downloading…" while the total is unknown (an Ollama pull's
/// status-only lines report `total == 0`) so the label never shows a bogus 0%.
fn download_verb(entry: &ModelEntry, phase: Option<DownloadPhase>) -> String {
    if phase == Some(DownloadPhase::Verifying) {
        "Verifying…".to_string()
    } else {
        entry.progress_percent().map_or_else(
            || "Downloading…".to_string(),
            |p| format!("Downloading {p}%"),
        )
    }
}

/// A short human label for a catalog model id (D-0015), so the three model-download surfaces
/// don't show the raw `stt-large-v3-turbo` id verbatim.
fn model_display_name(id: &str) -> &str {
    match id {
        "stt-large-v3-turbo" => "Speech recognition (Whisper large-v3-turbo)",
        "cleanup-qwen3-vl-8b-instruct" => "Qwen3-VL 8B Instruct (default)",
        "cleanup-qwen3-8b" => "Qwen3 8B",
        other => other.strip_prefix("ollama:").unwrap_or(other),
    }
}

fn checklist_row(ui: &mut egui::Ui, done: bool, label: &str) {
    let mark = if done { "☑" } else { "☐" };
    ui.label(format!("{mark} {label}"));
}

fn show_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// Colored-dot + label status pill (D-0013 UX pass, feedback item 1).
fn status_pill(ui: &mut egui::Ui, state: PillState) {
    ui.horizontal(|ui| {
        let (rect, _response) =
            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            ui.painter()
                .circle_filled(rect.center(), 5.0, state.color());
        }
        ui.label(state.label());
    });
}

/// Smooth horizontal level meter: an RMS fill banded quiet/good/hot, plus a
/// peak tick mark (D-0013 UX pass, feedback item 1: "not a raw number").
/// Standard 14px-tall bar; see [`level_meter_sized`] for the Status
/// dashboard's larger meter (UX pass 2).
fn level_meter(ui: &mut egui::Ui, rms: f32, peak: f32, width: f32) {
    level_meter_sized(ui, rms, peak, width, 14.0);
}

/// [`level_meter`] with a configurable bar height (UX pass 2: "big level
/// meter" on the Status dashboard).
fn level_meter_sized(ui: &mut egui::Ui, rms: f32, peak: f32, width: f32, height: f32) {
    let rms = rms.clamp(0.0, 1.0);
    let peak = peak.clamp(0.0, 1.0);
    let desired_size = egui::vec2(width.min(ui.available_width()), height);
    let (rect, _response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = height * 0.5;
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            ui.visuals().extreme_bg_color,
            egui::Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        if rms > 0.0 {
            let fill =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * rms, rect.height()));
            painter.rect(
                fill,
                radius,
                theme::level::color_for_rms(rms),
                egui::Stroke::NONE,
                egui::StrokeKind::Inside,
            );
        }
        if peak > 0.0 {
            let x = rect.left() + rect.width() * peak;
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(2.0, egui::Color32::WHITE),
            );
        }
    }
    ui.label(
        egui::RichText::new(format!("rms {rms:.2} · peak {peak:.2}"))
            .weak()
            .small(),
    );
}

/// A primary, accent-filled action button (white text on [`theme::ACCENT`],
/// rounded) for the shell's single most important action per screen (UX pass
/// 2: "Save, Start dictation"). Secondary buttons stay the default subtle
/// style.
fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(egui::Color32::WHITE))
            .fill(theme::ACCENT)
            .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS)),
    )
}

/// Animated toggle switch (egui has no built-in one); the canonical ~15-line
/// pattern from the egui demo library, themed with the shell's accent color
/// via `interact_selectable`.
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = ui.spacing().interact_size.y * egui::vec2(1.8, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        ui.painter().rect(
            rect,
            radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter().circle(
            center,
            0.75 * radius,
            egui::Color32::WHITE,
            visuals.fg_stroke,
        );
    }

    response
}

/// A toggle switch with a trailing label, laid out like a checkbox row.
fn labeled_toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) {
    ui.horizontal(|ui| {
        toggle_switch(ui, on);
        ui.label(label);
    });
}

/// Convert a rasterized [`IconImage`] into a tray-icon [`Icon`].
fn make_icon(state: TrayState) -> Option<Icon> {
    let IconImage {
        rgba,
        width,
        height,
        ..
    } = icons::render(state);
    Icon::from_rgba(rgba, width, height).ok()
}

/// Build the NSStatusItem with its menu. Returns an error on headless
/// environments where a status bar cannot be created.
fn build_tray(state: TrayState) -> Result<(TrayIcon, MenuHandles), Box<dyn std::error::Error>> {
    let menu = Menu::new();
    let start_stop = MenuItem::new("Start dictation", true, None);
    let settings = MenuItem::new("Settings…", true, None);
    let status = MenuItem::new("Daemon: connecting…", false, None);
    let quit = MenuItem::new("Quit", true, None);

    menu.append(&start_stop)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&settings)?;
    menu.append(&status)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let image = icons::render(state);
    let icon = Icon::from_rgba(image.rgba.clone(), image.width, image.height)?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Whisper — local dictation")
        .with_icon(icon)
        .with_icon_as_template(image.template)
        .build()?;

    Ok((
        tray,
        MenuHandles {
            start_stop,
            settings,
            status,
            quit,
        },
    ))
}

/// The WS listener loop: connect, forward frames, reconnect with backoff.
fn ws_listen_loop(url: String, tx: mpsc::Sender<WsMsg>, ctx: egui::Context) {
    let mut attempt: u32 = 0;
    loop {
        match tungstenite::connect(&url) {
            Ok((mut socket, _resp)) => {
                attempt = 0;
                let _ = tx.send(WsMsg::Connected);
                ctx.request_repaint();
                loop {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            let _ =
                                tx.send(WsMsg::Frame(whisper_ux::api::parse_event(text.as_str())));
                            ctx.request_repaint();
                        }
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
                let _ = tx.send(WsMsg::Disconnected);
                ctx.request_repaint();
            }
            Err(_) => {
                let _ = tx.send(WsMsg::Disconnected);
                ctx.request_repaint();
            }
        }
        thread::sleep(backoff_delay(attempt));
        attempt = attempt.saturating_add(1);
    }
}
