//! `whisper-platform` implements `whisper-core`'s ports for macOS: mic capture (cpal),
//! global hotkeys (rdev), and pasteboard text insertion (`ARCHITECTURE.md` §5, D-0006,
//! D-0007). Other OSes are ports-designed-for but not implemented (`ARCHITECTURE.md` §8).
//!
//! Not yet implemented: no real OS adapters exist yet — cpal/rdev/pasteboard bindings are
//! a later Phase-3 (opus) task. This crate currently only proves the downward dependency
//! on `whisper-core`'s port traits compiles and is reachable.

use whisper_core::ports::{NoopVad, Vad, VadEvent};

/// Placeholder confirming the port surface is reachable from the platform crate.
pub fn placeholder_status() -> &'static str {
    "whisper-platform: not yet implemented"
}

/// Trivial stand-in confirming the streaming [`Vad`] port is reachable from the platform
/// crate; the real Silero adapter (D-0008) lives in `whisper-iface`. Delegates to the core
/// pass-through [`NoopVad`] (single segment over the whole stream).
#[derive(Debug, Default)]
pub struct PlaceholderVad(NoopVad);

impl Vad for PlaceholderVad {
    fn push(&mut self, frame: &[f32]) -> Vec<VadEvent> {
        self.0.push(frame)
    }

    fn flush(&mut self) -> Vec<VadEvent> {
        self.0.flush()
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
    fn placeholder_vad_passes_everything_through_as_one_segment() {
        let mut vad = PlaceholderVad::default();
        let events = vad.push(&[0.0_f32; 4]);
        assert_eq!(events.first(), Some(&VadEvent::SpeechStart));
        assert_eq!(vad.flush(), vec![VadEvent::SpeechEnd]);
    }
}
