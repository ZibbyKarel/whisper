//! `whisper-iface` hosts the `whisperd` daemon: localhost HTTP (control) + WebSocket
//! (events) + CLI plumbing over `whisper-core` (`ARCHITECTURE.md` §4 "Interface Layer",
//! D-0009/D-0011).
//!
//! The [`daemon`] module implements the `/v1` control API and `WS /v1/events` fan-out; the
//! `!Send` engines are pinned to one worker thread behind an actor (D-0011). Production STT/VAD/
//! cleanup adapters live in [`stt`]/[`vad`]/[`cleanup`] (`ARCHITECTURE.md` §3.1: heavy native
//! deps here, not in the pure core).

pub mod cleanup;
pub mod daemon;
pub mod ollama;
pub mod stt;
pub mod vad;

pub use cleanup::{CleanupHealth, OllamaCleanupEngine};
pub use daemon::{
    build_router, build_state, build_state_with_data_dir, production_engine_builder, Daemon,
};
pub use stt::{transcribe_file, WhisperCppEngine};
pub use vad::{SileroVad, VadError};

use whisper_core::pipeline::PipelineConfig;

/// Default pipeline configuration (`CleanupMode::Auto` skip-gate, D-0005).
pub fn default_pipeline_config() -> PipelineConfig {
    PipelineConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use whisper_core::pipeline::CleanupMode;

    #[test]
    fn default_pipeline_config_uses_auto_cleanup_mode() {
        assert_eq!(default_pipeline_config().cleanup_mode, CleanupMode::Auto);
    }
}
