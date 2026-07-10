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

/// One event from a streaming [`Vad`] segmenter (`ARCHITECTURE.md` §3.2, D-0008).
///
/// A speech utterance is delimited by a `SpeechStart` .. `SpeechEnd` pair; every
/// `SpeechFrame` in between carries PCM that belongs to the segment. Leading silence and
/// long trailing silence are dropped by the segmenter, so the concatenation of a segment's
/// `SpeechFrame`s is what STT decodes — whisper must never see multi-second silence
/// windows (the measured "Titulky vytvořil JohnyX." hallucination, D-0008).
#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    /// A new speech segment has opened.
    SpeechStart,
    /// PCM belonging to the currently-open segment.
    SpeechFrame(AudioFrame),
    /// The open speech segment has closed; decode it now (`ARCHITECTURE.md` §3.2,
    /// per-VAD-segment decoding).
    SpeechEnd,
}

/// Streaming voice-activity segmenter (`ARCHITECTURE.md` §3.2, D-0008). Frames are pushed
/// in; [`VadEvent`]s come out. The real implementation (Silero, in `whisper-iface`) gates
/// speech and trims silence; [`NoopVad`] is a pass-through used by hardware-free tests.
///
/// Contract: between a `SpeechStart` and its matching `SpeechEnd` an implementation may
/// emit any number of `SpeechFrame`s; `SpeechStart`/`SpeechEnd` are always balanced across
/// the full `push`* / `flush` sequence. [`flush`](Vad::flush) closes any segment still open
/// at end-of-stream.
pub trait Vad {
    /// Feed one frame of 16 kHz mono PCM; returns any events it triggered.
    fn push(&mut self, frame: &[f32]) -> Vec<VadEvent>;
    /// Signal end-of-stream, flushing any open segment (final `SpeechEnd`).
    fn flush(&mut self) -> Vec<VadEvent>;
}

/// Pass-through [`Vad`]: treats the whole stream as a single speech segment (one
/// `SpeechStart` on the first frame, a `SpeechFrame` per pushed frame, one `SpeechEnd` at
/// [`flush`](Vad::flush)). Used by unit/integration tests so the pipeline can be exercised
/// without the Silero model; production uses the Silero adapter in `whisper-iface`.
#[derive(Debug, Default, Clone)]
pub struct NoopVad {
    started: bool,
}

impl Vad for NoopVad {
    fn push(&mut self, frame: &[f32]) -> Vec<VadEvent> {
        let mut out = Vec::with_capacity(2);
        if !self.started {
            self.started = true;
            out.push(VadEvent::SpeechStart);
        }
        out.push(VadEvent::SpeechFrame(frame.to_vec()));
        out
    }

    fn flush(&mut self) -> Vec<VadEvent> {
        if self.started {
            self.started = false;
            vec![VadEvent::SpeechEnd]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_vad_emits_one_segment_over_the_whole_stream() {
        let mut vad = NoopVad::default();
        assert_eq!(
            vad.push(&[0.0, 0.5]),
            vec![VadEvent::SpeechStart, VadEvent::SpeechFrame(vec![0.0, 0.5])]
        );
        // No second SpeechStart on later frames.
        assert_eq!(vad.push(&[-0.5]), vec![VadEvent::SpeechFrame(vec![-0.5])]);
        assert_eq!(vad.flush(), vec![VadEvent::SpeechEnd]);
        // Idempotent flush once the segment is closed.
        assert_eq!(vad.flush(), Vec::<VadEvent>::new());
    }
}
