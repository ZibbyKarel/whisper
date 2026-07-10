//! `whisper-tray`: the Wispr Flow-style menu-bar shell (D-0013). A thin GUI over
//! the whisperd `/v1` API — all logic lives in the `whisper_ux` library so it is
//! unit-tested without a display; this binary only wires the tray, the egui
//! window, and the WS listener thread together.
//!
//! GUI behavior (tray click-through, window rendering, TCC prompts) is verified
//! manually — see `docs/phase6-manual-verification.md`. This file is not covered
//! by automated tests.

mod gui;

use std::env;

fn main() -> eframe::Result<()> {
    // Connection settings come from the environment so the shell needs no config
    // file of its own (it is a client; the daemon owns config). Defaults match
    // the daemon's D-0010 bind/port.
    let base_url = env::var("WHISPERD_URL").unwrap_or_else(|_| "http://127.0.0.1:8737".to_string());
    let token = env::var("WHISPERD_TOKEN").ok().filter(|t| !t.is_empty());

    let mut native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Whisper")
            .with_inner_size([560.0, 640.0])
            .with_min_inner_size([420.0, 480.0]),
        ..Default::default()
    };

    // macOS: run as an accessory (no Dock icon, menu-bar only) — the primary
    // surface is the tray, per Wispr Flow (D-0013 §6). Compile-time only until
    // verified on a real desktop.
    #[cfg(target_os = "macos")]
    {
        native_options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }));
    }

    eframe::run_native(
        "Whisper",
        native_options,
        Box::new(move |cc| Ok(Box::new(gui::ShellApp::new(cc, base_url, token)))),
    )
}
