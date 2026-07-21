//! `whisper-platform` implements `whisper-core`'s ports for macOS (`ARCHITECTURE.md` §5,
//! D-0006/D-0007/D-0012): mic capture (cpal → 16 kHz mono), a global hotkey listener (rdev,
//! push-to-talk + toggle), and pasteboard text insertion (save → set → Cmd-V → restore, NFC
//! read-back, Secure Input detect-never-bypass). Other OSes are ports-designed-for but not
//! implemented (`ARCHITECTURE.md` §8).
//!
//! Layering: this crate depends only on `whisper-core`'s port traits — never on
//! `whisper-iface` (D-0012). Events it must surface upward (device loss) are delivered through
//! caller-supplied callbacks, so the daemon owns the wire format, not this crate.
//!
//! Hardware paths (opening a device, synthesizing keys, listening for global input) require
//! macOS TCC grants that CI cannot provide (D-0012); they live behind `#[cfg(target_os =
//! "macos")]` and are only exercised by `#[ignore]` tests + the manual checklist in
//! `docs/phase5-manual-verification.md`. All decision logic (downmix/resample math, the NFC
//! pasteboard round-trip, the Secure-Input gate, hotkey combo parsing and the push-to-talk /
//! toggle state machines) is factored behind trait/data seams and unit-tested without hardware.

pub mod audio;
pub mod hotkey;
pub mod paste;
pub mod permission;

pub use audio::{
    downmix_to_mono, AudioLevel, AutoGain, AutoGainConfig, CaptureError, LevelMeter,
    StreamResampler, TARGET_SAMPLE_RATE_HZ,
};
pub use hotkey::{
    ComboTracker, HotkeyCombo, HotkeyEvent, HotkeyMode, HotkeySession, SessionIntent,
};
pub use paste::{Clipboard, KeySynth, PasteboardSink, SecureInput, DEFAULT_RESTORE_DELAY};
// Permission preflight (D-0016): unlike the hardware adapters above, these are exported
// unconditionally — a non-macOS build gets the `Access::Unknown`/`false` stub (see
// `permission.rs`), so a caller (e.g. `whisper-iface`'s `/v1/platform/permissions`) can call
// them the same way regardless of host OS.
pub use permission::{
    accessibility_access, input_monitoring_access, request_accessibility, request_input_monitoring,
    Access,
};

#[cfg(target_os = "macos")]
pub use audio::CpalAudioSource;
#[cfg(target_os = "macos")]
pub use hotkey::spawn_listener;
#[cfg(target_os = "macos")]
pub use paste::{macos_pasteboard_sink, secure_input_enabled};
