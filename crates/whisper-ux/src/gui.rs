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

use whisper_ux::api::{ApiEvent, CleanupMode, Config, DeliveryMode, HotkeyMode, Incoming};
use whisper_ux::client::{backoff_delay, events_ws_url, Api, ApiError, HttpClient};
use whisper_ux::dict_editor::DictEditor;
use whisper_ux::icons::{self, IconImage};
use whisper_ux::onboarding::{self, OnboardingState};
use whisper_ux::save_report::SaveReport;
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

/// Sections of the main app screen (left sidebar / tab bar, D-0013 UX pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Status,
    Dictation,
    Audio,
    Cleanup,
    Dictionary,
    Advanced,
}

const SECTIONS: [Section; 6] = [
    Section::Status,
    Section::Dictation,
    Section::Audio,
    Section::Cleanup,
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
            Section::Dictionary => "Dictionary",
            Section::Advanced => "Advanced",
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

    // --- settings working copy ---
    cfg: Option<Config>,
    // `Option<String>` config fields need a free-typing text buffer that only
    // round-trips to `Option` on save (an empty box means "unset"), mirroring
    // how the daemon treats an absent field.
    token_input: String,
    dictionary_path_input: String,
    history_path_input: String,
    cleanup_model_input: String,
    dict: DictEditor,
    status_msg: Option<String>,
    save_report: SaveReport,
    test_dictation_text: String,
}

impl ShellApp {
    pub fn new(cc: &eframe::CreationContext<'_>, base_url: String, token: Option<String>) -> Self {
        // D-0013 UX pass: a calm, rounded dark theme instead of raw egui defaults.
        theme::apply(&cc.egui_ctx);

        let client = HttpClient::new(base_url.clone(), token.clone());

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
        let screen = if onboarding.completed {
            Screen::App
        } else {
            Screen::Onboarding
        };

        let mut app = Self {
            client,
            base_url,
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
            cfg: None,
            token_input: String::new(),
            dictionary_path_input: String::new(),
            history_path_input: String::new(),
            cleanup_model_input: String::new(),
            dict: DictEditor::default(),
            status_msg: None,
            save_report: SaveReport::default(),
            test_dictation_text: String::new(),
        };
        // Best-effort initial sync so the UI reflects the daemon immediately.
        app.refresh_status();
        app.load_settings();
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
                self.cleanup_model_input = cfg.models.cleanup.clone().unwrap_or_default();
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
        cfg.models.cleanup = non_empty(&self.cleanup_model_input);
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
        match ev {
            ApiEvent::SessionStarted { session_id, .. } => {
                self.active_session = Some(session_id.clone());
                self.last_partial.clear();
            }
            ApiEvent::AudioLevel { rms, peak, .. } => {
                self.last_rms = *rms;
                self.last_peak = *peak;
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

    /// The persistent "restart to apply" notice (feedback item 2): stays
    /// visible on every section, not just the one that triggered the save,
    /// until the next save clears it.
    fn ui_staged_notice(&self, ui: &mut egui::Ui) {
        if let Some(notice) = self.save_report.staged_notice() {
            ui.colored_label(egui::Color32::from_rgb(0xF2, 0x9D, 0x1C), notice);
            ui.add_space(4.0);
        }
        if let Some(applied) = self.save_report.applied_summary() {
            ui.label(egui::RichText::new(applied).weak().small());
        }
    }

    fn ui_section_nav(&mut self, ui: &mut egui::Ui) {
        for section in SECTIONS {
            let selected = self.section == section;
            if ui.selectable_label(selected, section.label()).clicked() {
                self.section = section;
            }
        }
    }

    // -- rendering: sections -------------------------------------------------

    fn ui_status_section(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.heading("Engine");
            ui.horizontal(|ui| {
                status_pill(ui, self.pill_state());
                ui.label(format!("daemon: {}", self.daemon_phase));
            });
            ui.add_space(6.0);
            ui.label("Input level");
            level_meter(ui, self.last_rms, self.last_peak, 260.0);
            ui.add_space(6.0);
            let label = if self.active_session.is_some() {
                "Stop dictation"
            } else {
                "Start dictation"
            };
            if ui.button(label).clicked() {
                self.toggle_dictation();
            }
        });
        ui.add_space(8.0);
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
        ui.add_space(8.0);
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
        ui.add_space(8.0);
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
        ui.add_space(8.0);
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
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Language");
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.language)
                        .hint_text("cs / en / auto")
                        .desired_width(120.0),
                );
            });
        });
        ui.add_space(8.0);
        theme::card_frame().show(ui, |ui| {
            ui.heading("Models");
            ui.horizontal(|ui| {
                ui.label("STT");
                ui.text_edit_singleline(&mut cfg.models.stt);
            });
            ui.horizontal(|ui| {
                ui.label("Cleanup LLM");
                ui.text_edit_singleline(&mut self.cleanup_model_input);
            });
            ui.label(
                egui::RichText::new("Empty cleanup path disables the LLM cleanup stage.")
                    .weak()
                    .small(),
            );
            ui.label(
                egui::RichText::new("Model changes are staged until the daemon restarts.")
                    .weak()
                    .small(),
            );
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
        ui.add_space(8.0);
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
        ui.add_space(8.0);
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
        ui.add_space(8.0);
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

            checklist_row(ui, self.onboarding.mic_ack, "2. Microphone permission");
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

            checklist_row(
                ui,
                self.onboarding.accessibility_ack,
                "3. Accessibility permission",
            );
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(
                    "Grant in System Settings → Privacy & Security → Accessibility (for paste).",
                );
                if ui.button("I granted it").clicked() {
                    self.onboarding.accessibility_ack = true;
                    self.persist_onboarding();
                }
            });

            checklist_row(ui, self.onboarding.test_dictation_done, "4. Test dictation");
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

        ui.add_space(8.0);
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

    fn ui_app(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Reload from daemon").clicked() {
                self.load_settings();
            }
            if ui.button("Save").clicked() {
                self.save_settings();
            }
        });
        ui.add_space(4.0);
        self.ui_staged_notice(ui);

        if self.cfg.is_none() {
            ui.label("No config loaded — is the daemon running?");
            return;
        }

        ui.horizontal_wrapped(|ui| self.ui_section_nav(ui));
        ui.add_space(8.0);

        // Work on a temporary so the borrow of self.cfg doesn't collide with
        // helper methods; write back at the end.
        let mut cfg = self.cfg.take().unwrap();
        match self.section {
            Section::Status => self.ui_status_section(ui),
            Section::Dictation => self.ui_dictation_section(&mut cfg, ui),
            Section::Audio => self.ui_audio_section(&mut cfg, ui),
            Section::Cleanup => self.ui_cleanup_section(&mut cfg, ui),
            Section::Dictionary => self.ui_dictionary_section(ui),
            Section::Advanced => self.ui_advanced_section(&mut cfg, ui),
        }
        self.cfg = Some(cfg);
    }
}

impl eframe::App for ShellApp {
    // Non-painting work (eframe 0.35 splits `logic` from `ui`): drain background
    // channels and handle the window-close request here, where the `Context` is
    // available.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_ws();
        self.drain_menu_events(ctx);

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
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Whisper");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    status_pill(ui, self.pill_state());
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            self.ui_recording_banner(ui);
            egui::ScrollArea::vertical().show(ui, |ui| match self.screen {
                Screen::Onboarding => self.ui_onboarding(ui),
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
fn level_meter(ui: &mut egui::Ui, rms: f32, peak: f32, width: f32) {
    let rms = rms.clamp(0.0, 1.0);
    let peak = peak.clamp(0.0, 1.0);
    let desired_size = egui::vec2(width.min(ui.available_width()), 14.0);
    let (rect, _response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(
            rect,
            7.0,
            ui.visuals().extreme_bg_color,
            egui::Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        if rms > 0.0 {
            let fill =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * rms, rect.height()));
            painter.rect(
                fill,
                7.0,
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
