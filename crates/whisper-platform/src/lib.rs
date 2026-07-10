//! `whisper-platform` implements `whisper-core`'s ports for macOS: mic capture (cpal),
//! global hotkeys (rdev), and pasteboard text insertion (`ARCHITECTURE.md` §5, D-0006,
//! D-0007). Other OSes are ports-designed-for but not implemented (`ARCHITECTURE.md` §8).
//!
//! Not yet implemented: no real OS adapters exist yet — cpal/rdev/pasteboard bindings are
//! a later Phase-3 (opus) task. This crate currently only proves the downward dependency
//! on `whisper-core`'s port traits compiles and is reachable.

use whisper_core::ports::Vad;

/// Placeholder confirming the port surface is reachable from the platform crate.
pub fn placeholder_status() -> &'static str {
    "whisper-platform: not yet implemented"
}

/// Trivial stand-in for the future platform VAD wiring; not the real Silero adapter
/// (D-0008 lands later).
#[derive(Debug, Default, Clone, Copy)]
pub struct PlaceholderVad;

impl Vad for PlaceholderVad {
    fn is_speech(&mut self, _frame: &[f32]) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_status_reports_stub_state() {
        assert_eq!(
            placeholder_status(),
            "whisper-platform: not yet implemented"
        );
    }

    #[test]
    fn placeholder_vad_passes_everything_through() {
        let mut vad = PlaceholderVad;
        assert!(vad.is_speech(&[0.0_f32; 4]));
    }
}
