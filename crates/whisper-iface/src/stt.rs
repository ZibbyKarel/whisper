//! Production STT adapter: whisper.cpp (via the `whisper-rs` bindings) behind
//! `whisper_core::ports::SttEngine`.
//!
//! Adapter placement per `ARCHITECTURE.md` §3.1: heavy native deps live in `whisper-iface`,
//! never in the pure core. Model choice and profile are D-0004 (large-v3-turbo q5_0, `-l
//! cs`). This adapter is the NFC-normalization boundary for the whole pipeline
//! (`ARCHITECTURE.md` §3.3): whisper.cpp can emit decomposed (NFD) sequences, so Czech
//! diacritics are normalized to NFC here, once, before any downstream text handling.

use std::path::Path;

use unicode_normalization::UnicodeNormalization;
use whisper_core::error::SttError;
use whisper_core::ports::SttEngine;
use whisper_core::types::{Segment, SttOpts, TranscriptRaw};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Default language when `SttOpts::lang_hint` is unset — Czech-first (`ARCHITECTURE.md`
/// §3.3, D-0004: single pass with `-l cs`, including for code-switching).
const DEFAULT_LANG: &str = "cs";

/// whisper.cpp STT engine holding a model resident for the process lifetime.
///
/// The [`WhisperContext`] (the loaded weights) is created once in [`WhisperCppEngine::load`]
/// and reused across calls (D-0004 / §3.4: models stay resident between sessions). Each
/// `transcribe` spins a fresh decode state, which is cheap relative to model load.
pub struct WhisperCppEngine {
    ctx: WhisperContext,
}

impl WhisperCppEngine {
    /// Load a GGML model from `model_path` and keep it resident. The path is a parameter —
    /// the default (`models/ggml-large-v3-turbo-q5_0.bin`) comes from config, never hardcoded
    /// here.
    ///
    /// Load failures map to [`SttError::ModelLoadFailed`] (distinct from decode failures),
    /// matching the `model.load_failed` semantics in `ARCHITECTURE.md` §3.2.
    pub fn load(model_path: &Path) -> Result<Self, SttError> {
        let path = model_path.to_str().ok_or_else(|| {
            SttError::ModelLoadFailed(format!(
                "model path is not valid UTF-8: {}",
                model_path.display()
            ))
        })?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(true);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| SttError::ModelLoadFailed(format!("{}: {e}", model_path.display())))?;
        Ok(Self { ctx })
    }
}

impl SttEngine for WhisperCppEngine {
    fn transcribe(&self, pcm: &[f32], opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        let lang = opts.lang_hint.as_deref().unwrap_or(DEFAULT_LANG);

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| SttError::Failed(format!("create_state: {e}")))?;

        // Greedy sampling with the native print/timestamp flags off, mirroring the
        // `whisper-cli -nt -np` invocation as closely as the bindings allow. (Note:
        // whisper-cli actually defaults to beam search, beam_size 5; on our fixtures beam
        // and greedy produce byte-identical output, so greedy is kept per spec — see the
        // parity test for the measured equivalence.)
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(lang));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, pcm)
            .map_err(|e| SttError::Failed(format!("decode: {e}")))?;

        let n = state.full_n_segments();
        let mut text = String::new();
        let mut segments = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let seg = state
                .get_segment(i)
                .ok_or_else(|| SttError::Failed(format!("missing segment {i}")))?;
            let raw = seg
                .to_str()
                .map_err(|e| SttError::Failed(format!("segment {i} utf8: {e}")))?;
            // NFC boundary (§3.3): normalize each piece so diacritics are byte-stable.
            let piece: String = raw.nfc().collect();
            segments.push(Segment {
                text: piece.trim().to_string(),
                // whisper timestamps are centiseconds; the port speaks milliseconds.
                start_ms: seg.start_timestamp().max(0) as u64 * 10,
                end_ms: seg.end_timestamp().max(0) as u64 * 10,
            });
            text.push_str(&piece);
        }
        // Normalize the joined string too, so a composed sequence split across a segment
        // boundary still lands in NFC.
        let text: String = text.nfc().collect();

        Ok(TranscriptRaw {
            text,
            lang: lang.to_string(),
            segments,
        })
    }
}

/// Read a WAV file as 16 kHz mono f32 PCM in `[-1.0, 1.0]`, downmixing multi-channel input.
/// Shared by [`transcribe_file`] and the parity tests.
pub fn read_wav_mono_f32(path: &Path) -> Result<Vec<f32>, hound::Error> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .into_samples::<i16>()
            .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => reader.into_samples::<f32>().collect::<Result<_, _>>()?,
    };
    if spec.channels > 1 {
        Ok(samples
            .chunks(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect())
    } else {
        Ok(samples)
    }
}

/// Minimal end-to-end helper: load `model_path`, read a WAV at `wav_path`, and return its
/// NFC-normalized transcript. Used by the real-model integration tests as a one-shot proof
/// of the adapter (`ARCHITECTURE.md` §4, `/v1/transcribe` primitive).
pub fn transcribe_file(model_path: &Path, wav_path: &Path, lang: &str) -> Result<String, SttError> {
    let engine = WhisperCppEngine::load(model_path)?;
    let pcm = read_wav_mono_f32(wav_path)
        .map_err(|e| SttError::Failed(format!("read wav {}: {e}", wav_path.display())))?;
    let opts = SttOpts {
        lang_hint: Some(lang.to_string()),
    };
    Ok(engine.transcribe(&pcm, &opts)?.text)
}
