//! Silence-trimming speech segmenter (`ARCHITECTURE.md` §3.2, D-0008).
//!
//! [`Segmenter`] is the model-free half of streaming VAD: given a per-window speech/silence
//! decision (the Silero probability threshold lives in the adapter), it produces the
//! balanced [`VadEvent`] stream that the pipeline decodes per segment. It exists in the pure
//! core so the anti-hallucination trimming logic is unit-tested without the ONNX model.
//!
//! The measured requirement (D-0008): whisper must never see multi-second silence, or it
//! hallucinates "Titulky vytvořil JohnyX." on Czech. The segmenter enforces this by
//! - dropping leading silence (before any speech in a segment),
//! - keeping at most [`VadConfig::hangover_ms`] of trailing silence in a closed segment, and
//! - closing a segment once [`VadConfig::max_silence_ms`] of contiguous silence accrues,
//!   so the long tail after an utterance is discarded rather than decoded.
//!
//! Short internal pauses (< `max_silence_ms`) are preserved so a sentence is not fragmented.

use crate::ports::VadEvent;
use crate::types::AudioFrame;

/// Tunable VAD parameters (`ARCHITECTURE.md` §3.2, D-0008). Defaults are the Phase-3
/// starting point; they need tuning on real recordings (D-0008 consequences).
#[derive(Debug, Clone, Copy)]
pub struct VadConfig {
    /// Sample rate of the frames the VAD sees. Silero V5 supports 8 kHz / 16 kHz; the
    /// pipeline runs at 16 kHz mono (`ARCHITECTURE.md` §3.2).
    pub sample_rate_hz: u32,
    /// Silero speech-probability threshold in `[0.0, 1.0]`; at/above is speech. Used by the
    /// adapter, carried here so all VAD tunables live in one struct. Default 0.35 (lowered from
    /// Silero's 0.5) so quiet mics/soft speech still trigger; digital silence still scores ~0.0,
    /// so the D-0008 anti-hallucination trimming is unaffected.
    pub speech_threshold: f32,
    /// Trailing silence kept in a closed segment so the last word is not clipped (~300 ms).
    /// Bounds the silence whisper sees at a segment's end — always < 1 s (D-0008).
    pub hangover_ms: u32,
    /// Contiguous silence that closes the open segment (~1 s, D-0008). The silence beyond
    /// [`Self::hangover_ms`] is discarded, never decoded.
    pub max_silence_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            speech_threshold: 0.35,
            hangover_ms: 300,
            max_silence_ms: 1_000,
        }
    }
}

impl VadConfig {
    fn ms_to_samples(&self, ms: u32) -> usize {
        (self.sample_rate_hz as usize * ms as usize) / 1_000
    }
}

/// Turns per-window speech/silence decisions into a balanced [`VadEvent`] stream with
/// silence trimming (D-0008). Model-agnostic: the caller supplies `is_speech`.
#[derive(Debug)]
pub struct Segmenter {
    hangover_samples: usize,
    max_silence_samples: usize,
    in_speech: bool,
    /// Silence windows buffered inside an open segment (an as-yet-unresolved pause).
    pending: Vec<AudioFrame>,
    pending_samples: usize,
}

impl Segmenter {
    pub fn new(cfg: &VadConfig) -> Self {
        Self {
            hangover_samples: cfg.ms_to_samples(cfg.hangover_ms),
            max_silence_samples: cfg.ms_to_samples(cfg.max_silence_ms).max(1),
            in_speech: false,
            pending: Vec::new(),
            pending_samples: 0,
        }
    }

    /// Feed one window of PCM plus its speech decision; returns triggered events.
    pub fn push_window(&mut self, window: AudioFrame, is_speech: bool) -> Vec<VadEvent> {
        let mut out = Vec::new();
        if is_speech {
            if !self.in_speech {
                self.in_speech = true;
                out.push(VadEvent::SpeechStart);
            } else if !self.pending.is_empty() {
                // A pause shorter than max_silence resolved back to speech: the buffered
                // silence was an internal gap, keep it so the utterance stays contiguous.
                for f in self.pending.drain(..) {
                    out.push(VadEvent::SpeechFrame(f));
                }
                self.pending_samples = 0;
            }
            out.push(VadEvent::SpeechFrame(window));
        } else if self.in_speech {
            self.pending_samples += window.len();
            self.pending.push(window);
            if self.pending_samples >= self.max_silence_samples {
                self.close_segment(&mut out);
            }
        }
        // Silence while not in a segment is leading silence: dropped.
        out
    }

    /// End-of-stream: close any open segment (bounded trailing silence + `SpeechEnd`).
    pub fn flush(&mut self) -> Vec<VadEvent> {
        let mut out = Vec::new();
        if self.in_speech {
            self.close_segment(&mut out);
        }
        out
    }

    /// Emit up to `hangover_samples` of buffered trailing silence, then `SpeechEnd`, and
    /// discard the rest — the long silence tail never reaches STT (D-0008).
    fn close_segment(&mut self, out: &mut Vec<VadEvent>) {
        let mut acc = 0usize;
        for f in self.pending.drain(..) {
            if acc >= self.hangover_samples {
                break; // remaining buffered silence is dropped (drain clears it anyway)
            }
            acc += f.len();
            out.push(VadEvent::SpeechFrame(f));
        }
        self.pending.clear();
        self.pending_samples = 0;
        self.in_speech = false;
        out.push(VadEvent::SpeechEnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 100-sample windows keep the arithmetic legible: at 16 kHz, hangover 300 ms = 4800
    // samples = 48 windows, max_silence 1000 ms = 16000 samples = 160 windows.
    fn cfg() -> VadConfig {
        VadConfig::default()
    }

    fn speech_frames(events: &[VadEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, VadEvent::SpeechFrame(_)))
            .count()
    }

    #[test]
    fn leading_silence_is_dropped() {
        let mut seg = Segmenter::new(&cfg());
        // 10 windows of silence before any speech: no events.
        let mut events = Vec::new();
        for _ in 0..10 {
            events.extend(seg.push_window(vec![0.0; 100], false));
        }
        assert!(events.is_empty(), "leading silence must produce no events");
    }

    #[test]
    fn single_utterance_opens_and_closes_once() {
        let mut seg = Segmenter::new(&cfg());
        let mut events = Vec::new();
        for _ in 0..20 {
            events.extend(seg.push_window(vec![0.5; 100], true));
        }
        events.extend(seg.flush());
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == VadEvent::SpeechStart)
                .count(),
            1
        );
        assert_eq!(
            events.iter().filter(|e| **e == VadEvent::SpeechEnd).count(),
            1
        );
        assert_eq!(speech_frames(&events), 20);
        assert_eq!(*events.first().unwrap(), VadEvent::SpeechStart);
        assert_eq!(*events.last().unwrap(), VadEvent::SpeechEnd);
    }

    #[test]
    fn long_trailing_silence_is_trimmed_to_hangover() {
        // The measured D-0008 case: speech then a multi-second silence tail. The segment
        // must close with at most `hangover` worth of silence, discarding the rest.
        let cfg = cfg();
        let mut seg = Segmenter::new(&cfg);
        let win = 100usize;
        // 5 windows of speech.
        for _ in 0..5 {
            let _ = seg.push_window(vec![0.5; win], true);
        }
        // 400 windows of silence = 40000 samples = 2.5 s (well past max_silence 1 s).
        let mut tail = Vec::new();
        for _ in 0..400 {
            tail.extend(seg.push_window(vec![0.0; win], false));
        }
        // Exactly one SpeechEnd (segment closed by max_silence, not by flush).
        assert_eq!(
            tail.iter().filter(|e| **e == VadEvent::SpeechEnd).count(),
            1
        );
        // Trailing silence emitted is bounded by hangover (48 windows), never the 400.
        let hangover_windows = cfg.hangover_ms as usize * cfg.sample_rate_hz as usize / 1000 / win; // 48
        assert!(
            speech_frames(&tail) <= hangover_windows + 1,
            "trailing silence not trimmed: {} silence frames survived",
            speech_frames(&tail)
        );
        // flush now is a no-op: the segment already closed.
        assert!(seg.flush().is_empty());
    }

    #[test]
    fn short_internal_pause_keeps_one_segment() {
        // A pause shorter than max_silence (1 s) must NOT split the utterance.
        let mut seg = Segmenter::new(&cfg());
        let win = 100usize;
        let mut events = Vec::new();
        for _ in 0..5 {
            events.extend(seg.push_window(vec![0.5; win], true));
        }
        // 100 windows of silence = 10000 samples = 0.625 s < 1 s max_silence.
        for _ in 0..100 {
            events.extend(seg.push_window(vec![0.0; win], false));
        }
        // Speech resumes.
        for _ in 0..5 {
            events.extend(seg.push_window(vec![0.5; win], true));
        }
        events.extend(seg.flush());
        // Still exactly one segment, and the internal pause was preserved as SpeechFrames.
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == VadEvent::SpeechStart)
                .count(),
            1
        );
        assert_eq!(
            events.iter().filter(|e| **e == VadEvent::SpeechEnd).count(),
            1
        );
        assert_eq!(speech_frames(&events), 110, "internal pause should be kept");
    }
}
