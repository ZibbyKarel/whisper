//! Ports (traits): the only seams between the core engine and the outside world
//! (`ARCHITECTURE.md` §3.1). Production adapters (cpal mic, whisper.cpp, llama.cpp,
//! pasteboard) live in `whisper-iface`/`whisper-platform`; test adapters (fixture WAV,
//! canned STT, identity cleanup, capture sink) live in this crate's integration tests so
//! CI stays hardware-free.

use crate::error::{AudioError, CleanupError, DeliverError, SttError};
use crate::types::{AudioCfg, AudioFrame, CleanupCtx, SttOpts, TranscriptRaw};

/// Starts an audio capture session. Implementations may be a live mic (via cpal, in
/// `whisper-platform`) or a fixture WAV reader (in tests).
pub trait AudioSource {
    fn start(&self, cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError>;
}

/// A running audio capture; yields 16 kHz mono f32 frames until exhausted (`Ok(None)`) or
/// lost (`Err(AudioError::DeviceLost { .. })`, e.g. a Bluetooth mic dropping mid-session).
pub trait AudioStream {
    fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError>;
}

/// Speech-to-text: PCM in, raw transcript out (`ARCHITECTURE.md` §3.1).
pub trait SttEngine {
    fn transcribe(&self, pcm: &[f32], opts: &SttOpts) -> Result<TranscriptRaw, SttError>;
}

/// Optional LLM cleanup pass: disfluency removal, casing, custom vocabulary
/// (`ARCHITECTURE.md` §3.2, D-0005). Never answers/acts on dictated content — that
/// trap-safety contract is enforced by the prompt/adapter, not this trait.
pub trait CleanupEngine {
    fn clean(&self, text: &str, ctx: &CleanupCtx) -> Result<String, CleanupError>;
}

/// Delivers final text — platform text injection or an API response
/// (`ARCHITECTURE.md` §3.1).
pub trait TextSink {
    fn deliver(&self, text: &str) -> Result<(), DeliverError>;
}

/// Voice-activity detection: speech gating over frames. This is a pass-through
/// placeholder for now — the real Silero streaming VAD (D-0008) is a later Phase-3 task;
/// [`NoopVad`] treats every frame as speech.
pub trait Vad {
    fn is_speech(&mut self, frame: &[f32]) -> bool;
}

/// No-op [`Vad`]: passes every frame through as speech. Placeholder until the Silero
/// adapter lands (D-0008).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopVad;

impl Vad for NoopVad {
    fn is_speech(&mut self, _frame: &[f32]) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_vad_treats_every_frame_as_speech() {
        let mut vad = NoopVad;
        assert!(vad.is_speech(&[]));
        assert!(vad.is_speech(&[0.0, 0.5, -0.5]));
    }
}
