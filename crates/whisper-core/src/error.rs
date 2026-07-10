//! Typed errors for whisper-core's ports and the pipeline that drives them
//! (`ARCHITECTURE.md` §3.1/§3.2).

use thiserror::Error;

/// Errors surfaced by an [`crate::ports::AudioSource`]/[`crate::ports::AudioStream`].
///
/// `DeviceLost` is the typed error the mission calls out explicitly: a mic disconnect
/// (e.g. Bluetooth drop) mid-recording. The pipeline turns it into a `Failed` session
/// state carrying any partial transcript already produced.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AudioError {
    #[error("audio device lost: {reason}")]
    DeviceLost { reason: String },
    #[error("failed to start audio source: {0}")]
    StartFailed(String),
}

/// Errors surfaced by an [`crate::ports::SttEngine`].
///
/// `ModelLoadFailed` is kept distinct from `Failed` because model-lifecycle failures are
/// first-class in `ARCHITECTURE.md` §3.2 (`model.load_failed`): a missing/corrupt GGUF or
/// OOM at load time is reported differently from a per-utterance decode failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SttError {
    #[error("model load failed: {0}")]
    ModelLoadFailed(String),
    #[error("transcription failed: {0}")]
    Failed(String),
}

/// Errors surfaced by a [`crate::ports::CleanupEngine`].
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CleanupError {
    #[error("cleanup failed: {0}")]
    Failed(String),
}

/// Errors surfaced by a [`crate::ports::TextSink`].
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeliverError {
    #[error("delivery failed: {0}")]
    Failed(String),
}

/// Errors from loading a dictionary substitution file (`ARCHITECTURE.md` §3.2, "dictionary
/// pass").
#[derive(Debug, Error)]
pub enum DictionaryError {
    #[error("invalid dictionary JSON: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Union of stage errors the pipeline orchestrator can encounter; used to build the
/// `Failed` session state's error message.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Stt(#[from] SttError),
    #[error(transparent)]
    Cleanup(#[from] CleanupError),
    #[error(transparent)]
    Deliver(#[from] DeliverError),
}

/// Rejected session-state transition (`ARCHITECTURE.md` §3.2 FSM).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid session transition: {from} -> {attempted}")]
pub struct FsmError {
    pub from: String,
    pub attempted: String,
}
