//! `whisper-iface` hosts the `whisperd` daemon: localhost HTTP (control) + WebSocket
//! (events) + CLI plumbing over `whisper-core` (`ARCHITECTURE.md` §4 "Interface Layer",
//! D-0009).
//!
//! Not yet implemented: no HTTP/WS server exists yet — that is a later Phase-3 task. This
//! crate currently only proves the downward dependency on `whisper-core` compiles and is
//! reachable.

use whisper_core::pipeline::PipelineConfig;

/// Placeholder for the future `/v1/health` handler.
pub fn placeholder_status() -> &'static str {
    "whisper-iface: not yet implemented"
}

/// Smoke-checks that `whisper-core`'s pipeline configuration is reachable from the
/// interface layer; real HTTP/WS wiring lands in a later Phase-3 task.
pub fn default_pipeline_config() -> PipelineConfig {
    PipelineConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_status_reports_stub_state() {
        assert_eq!(placeholder_status(), "whisper-iface: not yet implemented");
    }

    #[test]
    fn default_pipeline_config_has_cleanup_off() {
        assert!(!default_pipeline_config().cleanup_enabled);
    }
}
