//! Test-support adapters (feature `test-support`): a fixture WAV [`AudioSource`] and a
//! capturing [`TextSink`], shared across the workspace so production adapters can be driven
//! through the real [`crate::pipeline::Pipeline`] without copy-pasting harness code
//! (`ARCHITECTURE.md` §3.1, "Test adapters"). This module carries no OS/hardware
//! dependencies — it reads WAV files with `hound` and is gated behind a feature so the
//! default `whisper-core` build stays lean.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error::{AudioError, DeliverError};
use crate::ports::{AudioSource, AudioStream, DeliverOutcome, TextSink};
use crate::types::{AudioCfg, AudioFrame};

const FRAME_LEN: usize = 1600; // 100 ms @ 16 kHz mono

/// Reads a fixture WAV via `hound` and yields it as fixed-size 16 kHz mono f32 frames,
/// downmixing multi-channel input.
pub struct FixtureWavSource {
    path: PathBuf,
}

impl FixtureWavSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AudioSource for FixtureWavSource {
    fn start(&self, _cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
        let reader = hound::WavReader::open(&self.path)
            .map_err(|e| AudioError::StartFailed(e.to_string()))?;
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => reader
                .into_samples::<i16>()
                .map(|s| s.unwrap_or(0) as f32 / i16::MAX as f32)
                .collect(),
            hound::SampleFormat::Float => reader
                .into_samples::<f32>()
                .map(|s| s.unwrap_or(0.0))
                .collect(),
        };
        let mono: Vec<f32> = if spec.channels > 1 {
            samples
                .chunks(spec.channels as usize)
                .map(|c| c.iter().sum::<f32>() / c.len() as f32)
                .collect()
        } else {
            samples
        };
        Ok(Box::new(FixtureWavStream {
            samples: mono,
            pos: 0,
        }))
    }
}

struct FixtureWavStream {
    samples: Vec<f32>,
    pos: usize,
}

impl AudioStream for FixtureWavStream {
    fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError> {
        if self.pos >= self.samples.len() {
            return Ok(None);
        }
        let end = (self.pos + FRAME_LEN).min(self.samples.len());
        let frame = self.samples[self.pos..end].to_vec();
        self.pos = end;
        Ok(Some(frame))
    }
}

/// A [`TextSink`] that records the last delivered text for assertions.
#[derive(Clone, Default)]
pub struct CaptureSink {
    captured: Arc<Mutex<Option<String>>>,
}

impl CaptureSink {
    /// The most recently delivered text, if any.
    pub fn captured(&self) -> Option<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl TextSink for CaptureSink {
    fn deliver(&self, text: &str) -> Result<DeliverOutcome, DeliverError> {
        *self.captured.lock().unwrap() = Some(text.to_string());
        Ok(DeliverOutcome::Injected)
    }
}
