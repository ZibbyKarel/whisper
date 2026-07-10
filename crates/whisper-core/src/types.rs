//! Data types passed across the port boundaries (`ARCHITECTURE.md` §3.1).

/// Audio capture configuration. Production capture is 48 kHz downsampled to 16 kHz mono
/// (`ARCHITECTURE.md` §3.2); ports always deal in 16 kHz mono f32 frames, so this struct
/// exists for adapters that need to know the target format up front (e.g. to configure a
/// resampler) rather than to negotiate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCfg {
    pub sample_rate_hz: u32,
    pub channels: u16,
}

impl Default for AudioCfg {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            channels: 1,
        }
    }
}

/// One chunk of 16 kHz mono PCM samples in `[-1.0, 1.0]`.
pub type AudioFrame = Vec<f32>;

/// Options passed to an [`crate::ports::SttEngine`] call.
#[derive(Debug, Clone, Default)]
pub struct SttOpts {
    /// BCP-47-ish language hint (e.g. `"cs"`); default profile always passes `Some("cs")`
    /// in production (`ARCHITECTURE.md` §3.3), but the port itself stays generic.
    pub lang_hint: Option<String>,
}

/// One decoded segment of speech, as produced by per-VAD-segment decoding
/// (`ARCHITECTURE.md` §3.2).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Segment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Raw STT output before the dictionary/cleanup passes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TranscriptRaw {
    pub text: String,
    pub lang: String,
    pub segments: Vec<Segment>,
}

/// Context handed to a [`crate::ports::CleanupEngine`] alongside the text to clean.
#[derive(Debug, Clone, Default)]
pub struct CleanupCtx {
    pub lang: String,
}
