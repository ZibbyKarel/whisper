//! Production VAD adapter: Silero V5 (via the `voice_activity_detector` crate) behind
//! `whisper_core::ports::Vad` (D-0008, `ARCHITECTURE.md` §3.2).
//!
//! Adapter placement per `ARCHITECTURE.md` §3.1: the ONNX-runtime dependency is a heavy
//! native dep, so it lives in `whisper-iface`, never in the pure core. This needs no OS
//! specifics, so it stays out of `whisper-platform`.
//!
//! Licensing: `voice_activity_detector` is MIT; it bundles the Silero VAD V5 ONNX model
//! (also MIT) as embedded bytes, so there is no model download and **no network at runtime**
//! (D-0008 / mission local-first constraint). ONNX inference runs through the `ort` crate.
//!
//! The measured requirement it satisfies (D-0008): whisper hallucinates
//! "Titulky vytvořil JohnyX." when fed multi-second trailing silence. This adapter emits the
//! silence-trimmed segment stream defined by [`whisper_core::vad::Segmenter`], so STT only
//! ever sees speech (plus a bounded hangover).

use voice_activity_detector::VoiceActivityDetector;
use whisper_core::ports::{Vad, VadEvent};
use whisper_core::vad::{Segmenter, VadConfig};

/// Errors constructing the Silero VAD adapter.
#[derive(Debug, thiserror::Error)]
pub enum VadError {
    #[error("unsupported VAD sample rate {0} Hz (Silero V5 supports 8000 or 16000)")]
    UnsupportedSampleRate(u32),
    #[error("failed to build Silero VAD: {0}")]
    Build(String),
}

/// Streaming Silero VAD segmenter. Buffers incoming PCM into the fixed Silero window
/// (512 samples @ 16 kHz, 256 @ 8 kHz), scores each window, and drives a
/// [`Segmenter`] to produce the trimmed [`VadEvent`] stream.
pub struct SileroVad {
    detector: VoiceActivityDetector,
    segmenter: Segmenter,
    threshold: f32,
    window: usize,
    /// Samples not yet forming a full Silero window.
    buf: Vec<f32>,
}

impl SileroVad {
    /// Build a Silero VAD for the given config. Loads the embedded Silero V5 model — no
    /// network. Fails only on an unsupported sample rate or ONNX-session init error.
    pub fn new(cfg: VadConfig) -> Result<Self, VadError> {
        let window = match cfg.sample_rate_hz {
            16_000 => 512usize,
            8_000 => 256usize,
            other => return Err(VadError::UnsupportedSampleRate(other)),
        };
        let detector = VoiceActivityDetector::builder()
            .sample_rate(cfg.sample_rate_hz as i64)
            .chunk_size(window)
            .build()
            .map_err(|e| VadError::Build(e.to_string()))?;
        Ok(Self {
            detector,
            segmenter: Segmenter::new(&cfg),
            threshold: cfg.speech_threshold,
            window,
            buf: Vec::new(),
        })
    }

    /// Score and segment every full Silero window currently buffered.
    fn process_full_windows(&mut self, out: &mut Vec<VadEvent>) {
        while self.buf.len() >= self.window {
            let chunk: Vec<f32> = self.buf.drain(..self.window).collect();
            let prob = self.detector.predict(chunk.iter().copied());
            let is_speech = prob >= self.threshold;
            out.extend(self.segmenter.push_window(chunk, is_speech));
        }
    }
}

impl Vad for SileroVad {
    fn push(&mut self, frame: &[f32]) -> Vec<VadEvent> {
        self.buf.extend_from_slice(frame);
        let mut out = Vec::new();
        self.process_full_windows(&mut out);
        out
    }

    fn flush(&mut self) -> Vec<VadEvent> {
        let mut out = Vec::new();
        self.process_full_windows(&mut out);
        // Score any residual (< one window) by zero-padding for the model, but hand the
        // Segmenter only the real samples so no synthetic silence enters a segment.
        if !self.buf.is_empty() {
            let residual: Vec<f32> = std::mem::take(&mut self.buf);
            let mut padded = residual.clone();
            padded.resize(self.window, 0.0);
            let prob = self.detector.predict(padded);
            let is_speech = prob >= self.threshold;
            out.extend(self.segmenter.push_window(residual, is_speech));
        }
        out.extend(self.segmenter.flush());
        out
    }
}
