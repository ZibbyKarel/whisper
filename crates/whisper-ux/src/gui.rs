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
//! calls (state machine, parsing, client, icons, onboarding, dictionary) lives in
//! the library modules.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tungstenite::Message;

use whisper_ux::api::{ApiEvent, Config, DeliveryMode, HotkeyMode, Incoming};
use whisper_ux::client::{backoff_delay, events_ws_url, Api, ApiError, HttpClient};
use whisper_ux::dict_editor::DictEditor;
use whisper_ux::icons::{self, IconImage};
use whisper_ux::onboarding::{self, OnboardingState};
use whisper_ux::tray_state::{TrayInput, TrayState};

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
    Settings,
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
    onboarding: OnboardingState,
    onboarding_path: Option<PathBuf>,

    // --- settings working copy ---
    cfg: Option<Config>,
    port_str: String,
    dict: DictEditor,
    status_msg: Option<String>,
    save_report: Option<String>,
    test_dictation_text: String,
}

impl ShellApp {
    pub fn new(cc: &eframe::CreationContext<'_>, base_url: String, token: Option<String>) -> Self {
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
            Screen::Settings
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
            onboarding,
            onboarding_path,
            cfg: None,
            port_str: String::new(),
            dict: DictEditor::default(),
            status_msg: None,
            save_report: None,
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
                self.port_str = cfg.port.to_string();
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

    fn save_settings(&mut self) {
        let Some(cfg) = self.cfg.clone() else {
            return;
        };
        let mut cfg = cfg;
        if let Ok(p) = self.port_str.parse::<u16>() {
            cfg.port = p;
        }

        // Validate the dictionary client-side before sending.
        let errs = self.dict.validate();
        if !errs.is_empty() {
            self.save_report = Some(format!(
                "dictionary invalid: {}",
                errs.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            return;
        }

        let mut report = String::new();
        match self.client.put_config(&cfg) {
            Ok(resp) => {
                self.cfg = Some(resp.config);
                if !resp.applied.is_empty() {
                    report.push_str(&format!("applied: {}. ", resp.applied.join(", ")));
                }
                if !resp.staged.is_empty() {
                    report.push_str(&format!(
                        "staged (needs daemon restart): {}. ",
                        resp.staged.join(", ")
                    ));
                }
            }
            Err(e) => report.push_str(&format!("config error: {e}. ")),
        }
        match self.client.put_dictionary(&self.dict.to_json_value()) {
            Ok(_) => report.push_str("dictionary saved."),
            Err(ApiError::Http { message, .. }) => {
                report.push_str(&format!("dictionary rejected: {message}"))
            }
            Err(e) => report.push_str(&format!("dictionary error: {e}")),
        }
        self.save_report = Some(report);
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
                self.screen = Screen::Settings;
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

    // -- rendering --------------------------------------------------------

    fn ui_recording_banner(&self, ui: &mut egui::Ui) {
        if self.tray_state == TrayState::Recording || self.active_session.is_some() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("● Recording").color(egui::Color32::RED));
                    // Live level meter from the audio.level event. Empty when the
                    // daemon does not emit it (D-0013 §4).
                    let meter = egui::ProgressBar::new(self.last_rms.clamp(0.0, 1.0))
                        .desired_width(180.0)
                        .text(format!(
                            "rms {:.2} · peak {:.2}",
                            self.last_rms, self.last_peak
                        ));
                    ui.add(meter);
                });
                if !self.last_partial.is_empty() {
                    ui.label(format!("… {}", self.last_partial));
                }
            });
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.horizontal(|ui| {
            if ui.button("Reload from daemon").clicked() {
                self.load_settings();
            }
            if ui.button("Save").clicked() {
                self.save_settings();
            }
        });
        ui.separator();

        if self.cfg.is_none() {
            ui.label("No config loaded — is the daemon running?");
            return;
        }

        // Work on a temporary so the borrow of self.cfg doesn't collide with
        // helper methods; write back at the end.
        let mut cfg = self.cfg.take().unwrap();

        egui::Grid::new("cfg_grid").num_columns(2).show(ui, |ui| {
            ui.label("Language");
            ui.horizontal(|ui| {
                lang_radio(ui, &mut cfg.language, "cs", "Czech");
                lang_radio(ui, &mut cfg.language, "en", "English");
                lang_radio(ui, &mut cfg.language, "auto", "Auto");
            });
            ui.end_row();

            ui.label("Cleanup mode");
            ui.horizontal(|ui| {
                use whisper_ux::api::CleanupMode as CM;
                ui.selectable_value(&mut cfg.cleanup_mode, CM::Always, "always");
                ui.selectable_value(&mut cfg.cleanup_mode, CM::Never, "never");
                ui.selectable_value(&mut cfg.cleanup_mode, CM::Auto, "auto");
            });
            ui.end_row();

            ui.label("Delivery");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut cfg.delivery, DeliveryMode::None, "none");
                ui.selectable_value(&mut cfg.delivery, DeliveryMode::Paste, "paste");
            });
            ui.end_row();

            ui.label("Hotkey");
            ui.horizontal(|ui| {
                ui.checkbox(&mut cfg.hotkey.enabled, "enabled");
                ui.text_edit_singleline(&mut cfg.hotkey.combo);
            });
            ui.end_row();

            ui.label("Hotkey mode");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut cfg.hotkey.mode, HotkeyMode::PushToTalk, "push-to-talk");
                ui.selectable_value(&mut cfg.hotkey.mode, HotkeyMode::Toggle, "toggle");
            });
            ui.end_row();

            ui.label("Daemon URL");
            ui.label(&self.base_url);
            ui.end_row();

            ui.label("Port");
            ui.text_edit_singleline(&mut self.port_str);
            ui.end_row();
        });
        ui.label(
            egui::RichText::new("Hotkey and port changes are staged until the daemon restarts.")
                .weak()
                .small(),
        );

        ui.separator();
        ui.collapsing("Dictionary", |ui| self.ui_dictionary(ui));

        self.cfg = Some(cfg);

        if let Some(report) = &self.save_report {
            ui.separator();
            ui.label(report);
        }
    }

    fn ui_dictionary(&mut self, ui: &mut egui::Ui) {
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
    }

    fn ui_onboarding(&mut self, ui: &mut egui::Ui) {
        ui.heading("Welcome to Whisper");
        ui.label("Local-first Czech/English dictation. Let's get set up.");
        ui.separator();

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
            ui.label("Grant in System Settings → Privacy & Security → Microphone (for whisperd).");
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
            ui.label("Grant in System Settings → Privacy & Security → Accessibility (for paste).");
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

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Finish").clicked() {
                self.onboarding.completed = true;
                self.persist_onboarding();
                self.screen = Screen::Settings;
                self.load_settings();
            }
            if ui.button("Skip for now").clicked() {
                self.onboarding.completed = true;
                self.persist_onboarding();
                self.screen = Screen::Settings;
            }
        });
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
                ui.selectable_value(&mut self.screen, Screen::Settings, "Settings");
                ui.selectable_value(&mut self.screen, Screen::Onboarding, "Onboarding");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dot = if self.connected { "🟢" } else { "🔴" };
                    ui.label(format!("{dot} {}", self.tray_state.label()));
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            self.ui_recording_banner(ui);
            ui.add_space(6.0);
            egui::ScrollArea::vertical().show(ui, |ui| match self.screen {
                Screen::Onboarding => self.ui_onboarding(ui),
                Screen::Settings => self.ui_settings(ui),
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

fn lang_radio(ui: &mut egui::Ui, current: &mut String, value: &str, label: &str) {
    if ui.selectable_label(current == value, label).clicked() {
        *current = value.to_string();
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
