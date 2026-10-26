//! `whisper-ux`: the replaceable Wispr Flow-style UX shell (D-0013).
//!
//! The shell is a **client** of the whisperd `/v1` Interface Layer and nothing
//! more — no dependency on `whisper-core`, `whisper-iface`, or `whisper-platform`
//! (constitution: UX plugs in over HTTP/WS and is deletable). Every wire type is
//! mirrored locally in [`api`].
//!
//! This library crate holds all the GUI-free logic so it is unit-tested with
//! `cargo test -p whisper-ux` without launching a window, a tray, or a daemon.
//! The `whisper-tray` binary ([`main.rs`](../src/main.rs)) is the thin GUI shell
//! on top.

pub mod api;
pub mod client;
pub mod daemon_ctl;
pub mod dict_editor;
pub mod icons;
pub mod language;
pub mod models_ui;
pub mod ollama_ui;
pub mod onboarding;
pub mod overlay;
pub mod permissions_ui;
pub mod save_report;
pub mod shell_settings;
pub mod theme;
pub mod tray_state;
pub mod validation;
