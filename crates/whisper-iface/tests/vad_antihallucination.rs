//! Anti-hallucination proof (D-0008): the real Silero VAD + real turbo model, driven
//! through the core [`Pipeline`], must transcribe a silence-padded clip identically to the
//! trimmed one — whisper never sees the multi-second trailing silence that produced the
//! measured "Titulky vytvořil JohnyX." hallucination (`docs/stt-real-validation.md`).
//!
//! Marked `#[ignore]` (needs the model): run with
//! `cargo test -p whisper-iface -- --ignored`.
//!
//! The padded WAV is generated in-test with `hound` and never committed (fixtures/ is
//! read-only; the padded speech samples are bit-identical to the original, only the trailing
//! digital silence differs).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use whisper_core::dictionary::Dictionary;
use whisper_core::error::SttError;
use whisper_core::pipeline::{Pipeline, PipelineConfig};
use whisper_core::ports::SttEngine;
use whisper_core::session::{CancellationToken, SessionState};
use whisper_core::test_support::{CaptureSink, FixtureWavSource};
use whisper_core::types::{AudioCfg, CleanupCtx, SttOpts, TranscriptRaw};
use whisper_core::vad::VadConfig;

use whisper_iface::{SileroVad, WhisperCppEngine};

/// Serializes model loading across the real-model test files (see `e2e_pipeline.rs`).
static MODEL_LOAD_GUARD: Mutex<()> = Mutex::new(());

fn serialize_model_load() -> MutexGuard<'static, ()> {
    MODEL_LOAD_GUARD.lock().unwrap_or_else(|p| p.into_inner())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn model_path() -> PathBuf {
    workspace_root().join("models/ggml-large-v3-turbo-q5_0.bin")
}

/// Shares one resident whisper model across several pipelines (D-0004 / §3.4: models stay
/// resident). `WhisperCppEngine::transcribe` is `&self`, so an `Arc` is enough.
struct SharedStt(Arc<WhisperCppEngine>);

impl SttEngine for SharedStt {
    fn transcribe(&self, pcm: &[f32], opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        self.0.transcribe(pcm, opts)
    }
}

/// Read a WAV as 16 kHz mono `i16`, preserving the exact integer sample values (downmixing
/// multi-channel input). Used so the padded clip's speech samples are bit-identical to the
/// source — only the appended silence differs.
fn read_wav_mono_i16(path: &Path) -> (hound::WavSpec, Vec<i16>) {
    let reader = hound::WavReader::open(path).expect("open source wav");
    let spec = reader.spec();
    assert_eq!(
        spec.sample_format,
        hound::SampleFormat::Int,
        "fixture is PCM"
    );
    let raw: Vec<i16> = reader
        .into_samples::<i16>()
        .map(|s| s.expect("read i16 sample"))
        .collect();
    let mono: Vec<i16> = if spec.channels > 1 {
        raw.chunks(spec.channels as usize)
            .map(|c| (c.iter().map(|&s| s as i32).sum::<i32>() / c.len() as i32) as i16)
            .collect()
    } else {
        raw
    };
    let mono_spec = hound::WavSpec {
        channels: 1,
        sample_rate: spec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    (mono_spec, mono)
}

fn write_wav_i16(path: &Path, spec: hound::WavSpec, samples: &[i16]) {
    let mut writer = hound::WavWriter::create(path, spec).expect("create temp wav");
    for &s in samples {
        writer.write_sample(s).expect("write sample");
    }
    writer.finalize().expect("finalize temp wav");
}

/// Run the full pipeline (Silero VAD + shared whisper) over a WAV; return `(state, text)`.
fn run_pipeline(wav: &Path, stt: SharedStt) -> (SessionState, String) {
    let vad = SileroVad::new(VadConfig::default()).expect("build Silero VAD");
    let sink = CaptureSink::default();
    let mut pipeline = Pipeline::new(
        Box::new(FixtureWavSource::new(wav)),
        Box::new(vad),
        Box::new(stt),
        Dictionary::empty(),
        None,
        Box::new(sink.clone()),
        PipelineConfig {
            cleanup_enabled: false,
        },
    );
    let token = CancellationToken::new();
    let state = pipeline.run(AudioCfg::default(), &CleanupCtx::default(), &token);
    (state, sink.captured().unwrap_or_default())
}

#[test]
#[ignore = "requires the turbo model; run with `cargo test -- --ignored`"]
fn silence_padding_does_not_change_transcript() {
    let _guard = serialize_model_load();
    let model = model_path();
    assert!(model.exists(), "missing model {}", model.display());

    let source = workspace_root().join("fixtures/audio/cs-03.wav");
    let (spec, samples) = read_wav_mono_i16(&source);

    // Padded variant: 4 s of digital silence appended (generated here, never committed).
    let pad_len = spec.sample_rate as usize * 4;
    let mut padded_samples = samples.clone();
    padded_samples.extend(std::iter::repeat_n(0i16, pad_len));
    let padded_path = std::env::temp_dir().join(format!("cs-03-padded-{}.wav", std::process::id()));
    write_wav_i16(&padded_path, spec, &padded_samples);

    let engine = Arc::new(WhisperCppEngine::load(&model).expect("load turbo model"));

    // Baseline: the original clip (VAD trims its natural silence).
    let (trimmed_state, trimmed_text) = run_pipeline(&source, SharedStt(Arc::clone(&engine)));
    // Padded: original + 4 s silence (VAD must trim it away).
    let (padded_state, padded_text) = run_pipeline(&padded_path, SharedStt(Arc::clone(&engine)));

    let _ = std::fs::remove_file(&padded_path);

    assert_eq!(trimmed_state, SessionState::Idle);
    assert_eq!(padded_state, SessionState::Idle);

    println!("trimmed: {trimmed_text:?}");
    println!("padded : {padded_text:?}");

    // The point of D-0008: the appended silence must not add the canonical hallucination.
    for (label, text) in [("trimmed", &trimmed_text), ("padded", &padded_text)] {
        assert!(
            !text.contains("Titulky"),
            "{label} output contains the Titulky/JohnyX hallucination: {text:?}"
        );
        assert!(
            !text.contains("JohnyX"),
            "{label} output contains the JohnyX hallucination: {text:?}"
        );
    }

    // Silence changes nothing: the two transcripts are identical.
    assert_eq!(
        trimmed_text.trim(),
        padded_text.trim(),
        "padding with silence changed the transcript"
    );
    // Sanity: the clip actually transcribed to something.
    assert!(
        !trimmed_text.trim().is_empty(),
        "expected non-empty transcript for cs-03"
    );
}

#[test]
#[ignore = "requires the turbo model; run with `cargo test -- --ignored`"]
fn pure_silence_yields_empty_text_and_clean_idle_exit() {
    let _guard = serialize_model_load();
    let model = model_path();
    assert!(model.exists(), "missing model {}", model.display());

    // 4 s of pure digital silence at 16 kHz mono.
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let silence = vec![0i16; spec.sample_rate as usize * 4];
    let silence_path =
        std::env::temp_dir().join(format!("pure-silence-{}.wav", std::process::id()));
    write_wav_i16(&silence_path, spec, &silence);

    let engine = Arc::new(WhisperCppEngine::load(&model).expect("load turbo model"));
    let (state, text) = run_pipeline(&silence_path, SharedStt(engine));

    let _ = std::fs::remove_file(&silence_path);

    // Silence in, empty text out, clean Idle exit — not an error, no hallucination.
    assert_eq!(
        state,
        SessionState::Idle,
        "silence must not fail the session"
    );
    assert!(
        text.trim().is_empty(),
        "pure silence should transcribe to nothing, got {text:?}"
    );
}
