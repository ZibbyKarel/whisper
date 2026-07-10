//! End-to-end proof: the real turbo model driven through the core [`Pipeline`]
//! (`ARCHITECTURE.md` §3.2) with the shared fixture adapters — `FixtureWavSource` +
//! `WhisperCppEngine` + empty dictionary + no cleanup + `CaptureSink`.
//!
//! Marked `#[ignore]` (needs the model): run with
//! `cargo test -p whisper-iface -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use whisper_core::dictionary::Dictionary;
use whisper_core::pipeline::{Pipeline, PipelineConfig};
use whisper_core::ports::NoopVad;
use whisper_core::session::{CancellationToken, SessionState};
use whisper_core::test_support::{CaptureSink, FixtureWavSource};
use whisper_core::types::{AudioCfg, CleanupCtx};

use whisper_iface::{transcribe_file, WhisperCppEngine};

/// Serializes the model-loading tests: two whisper.cpp Metal contexts initializing on
/// parallel test threads can transiently fail GPU init, and matches the "one active session
/// per engine" model (`ARCHITECTURE.md` §3.2). Held for each test's duration.
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

#[test]
#[ignore = "requires the turbo model; run with `cargo test -- --ignored`"]
fn real_model_through_pipeline_transcribes_czech() {
    let _guard = serialize_model_load();
    let model = model_path();
    assert!(model.exists(), "missing model {}", model.display());

    let cs01 = workspace_root().join("fixtures/audio/cs-01.wav");
    let stt = WhisperCppEngine::load(&model).expect("load turbo model");
    let sink = CaptureSink::default();

    let mut pipeline = Pipeline::new(
        Box::new(FixtureWavSource::new(&cs01)),
        Box::new(NoopVad::default()),
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

    assert_eq!(state, SessionState::Idle);
    let delivered = sink.captured().expect("pipeline delivered text");
    // Diacritics are load-bearing: the composed Czech word must survive the whole path.
    assert!(
        delivered.contains("žluťoučký"),
        "pipeline output lost Czech diacritics: {delivered:?}"
    );
}

#[test]
#[ignore = "requires the turbo model; run with `cargo test -- --ignored`"]
fn transcribe_file_helper_returns_czech_text() {
    let _guard = serialize_model_load();
    let model = model_path();
    assert!(model.exists(), "missing model {}", model.display());
    let cs01 = workspace_root().join("fixtures/audio/cs-01.wav");

    let text = transcribe_file(&model, &cs01, "cs").expect("transcribe_file");
    assert!(
        text.contains("žluťoučký"),
        "transcribe_file lost diacritics: {text:?}"
    );
}
