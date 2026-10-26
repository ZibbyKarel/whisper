//! Pure tray-icon state machine (D-0013 §2). WS events + connection signals map
//! to a small visual state; kept side-effect-free so the mapping is unit-tested
//! without a tray, a window, or a daemon.

use crate::api::ApiEvent;

/// The visual state of the menu-bar icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayState {
    /// Ready, no active session.
    #[default]
    Idle,
    /// A live capture session is running.
    Recording,
    /// Models loading (or post-capture STT/cleanup in progress).
    Processing,
    /// The daemon connection is lost, or a model failed to load.
    Error,
}

/// Inputs that can drive the state machine: a WS event, or a change in the WS
/// connection itself (the transport layer, not an event on it).
#[derive(Debug, Clone, Copy)]
pub enum TrayInput<'a> {
    Event(&'a ApiEvent),
    /// The WS connection dropped (reconnect with backoff is the caller's job).
    ConnectionLost,
    /// The WS connection was (re)established; state resyncs via `GET /v1/status`.
    ConnectionRestored,
}

impl TrayState {
    /// Fold one input into the next state. Pure: no I/O, no panics.
    ///
    /// Mapping (D-0013 §2):
    /// - `session_started` → Recording
    /// - `cleaned` / `delivered` / `cancelled` / `failed` → Idle (session over)
    /// - `raw_transcript` / `dictionary_applied` / `cleanup_skipped` → Processing
    ///   (capture ended, the pipeline is finishing) — only while Recording, so a
    ///   stray event never revives a finished session.
    /// - `model_loading` → Processing; `model_ready` → Idle; `model_load_failed` → Error
    /// - `ConnectionLost` → Error; `ConnectionRestored` → Idle (then resync)
    /// - anything else (partial transcripts, audio levels, platform notices) → unchanged
    #[must_use]
    pub fn next(self, input: TrayInput<'_>) -> TrayState {
        match input {
            TrayInput::ConnectionLost => TrayState::Error,
            TrayInput::ConnectionRestored => TrayState::Idle,
            TrayInput::Event(ev) => self.on_event(ev),
        }
    }

    fn on_event(self, ev: &ApiEvent) -> TrayState {
        match ev {
            ApiEvent::SessionStarted { .. } => TrayState::Recording,

            ApiEvent::Cleaned { .. }
            | ApiEvent::Delivered { .. }
            | ApiEvent::Cancelled { .. }
            | ApiEvent::Failed { .. } => TrayState::Idle,

            ApiEvent::RawTranscript { .. }
            | ApiEvent::DictionaryApplied { .. }
            | ApiEvent::CleanupSkipped { .. } => {
                // Post-capture pipeline work: only meaningful during a session.
                if self == TrayState::Recording {
                    TrayState::Processing
                } else {
                    self
                }
            }

            ApiEvent::ModelLoading { .. } => TrayState::Processing,
            ApiEvent::ModelReady { .. } => TrayState::Idle,
            ApiEvent::ModelLoadFailed { .. } => TrayState::Error,

            // Live-metering / partials / platform notices don't change the icon;
            // they feed the settings window instead. Model downloads (D-0015) are
            // background provisioning, not a dictation-session phase — the startup gate /
            // Settings → Models card show their progress, not the tray icon.
            ApiEvent::PartialTranscript { .. }
            | ApiEvent::AudioLevel { .. }
            | ApiEvent::SecureInputBlocked { .. }
            | ApiEvent::AudioDeviceLost { .. }
            | ApiEvent::PlatformPermission { .. }
            | ApiEvent::ModelDownload { .. }
            | ApiEvent::OllamaStatus { .. } => self,
        }
    }

    /// Human-readable label for the tray menu / accessibility.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TrayState::Idle => "Idle",
            TrayState::Recording => "Recording",
            TrayState::Processing => "Processing",
            TrayState::Error => "Error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(json: &str) -> ApiEvent {
        match crate::api::parse_event(json) {
            crate::api::Incoming::Known(e) => e,
            other => panic!("fixture must be a known event: {other:?}"),
        }
    }

    #[test]
    fn session_started_enters_recording() {
        let s = TrayState::Idle.next(TrayInput::Event(&ev(
            r#"{"type":"session_started","session_id":"s","ts_ms":1}"#,
        )));
        assert_eq!(s, TrayState::Recording);
    }

    #[test]
    fn delivered_returns_to_idle() {
        let s = TrayState::Recording.next(TrayInput::Event(&ev(
            r#"{"type":"delivered","session_id":"s","text":"ahoj","ts_ms":2}"#,
        )));
        assert_eq!(s, TrayState::Idle);
    }

    #[test]
    fn cancelled_and_failed_return_to_idle() {
        for j in [
            r#"{"type":"cancelled","session_id":"s","ts_ms":1}"#,
            r#"{"type":"failed","session_id":"s","error":"x","ts_ms":1}"#,
        ] {
            assert_eq!(
                TrayState::Recording.next(TrayInput::Event(&ev(j))),
                TrayState::Idle
            );
        }
    }

    #[test]
    fn raw_transcript_moves_recording_to_processing() {
        let s = TrayState::Recording.next(TrayInput::Event(&ev(
            r#"{"type":"raw_transcript","session_id":"s","text":"t","lang":"cs","ts_ms":1}"#,
        )));
        assert_eq!(s, TrayState::Processing);
    }

    #[test]
    fn raw_transcript_when_idle_does_not_revive() {
        let s = TrayState::Idle.next(TrayInput::Event(&ev(
            r#"{"type":"raw_transcript","session_id":"s","text":"t","lang":"cs","ts_ms":1}"#,
        )));
        assert_eq!(s, TrayState::Idle);
    }

    #[test]
    fn model_lifecycle() {
        assert_eq!(
            TrayState::Idle.next(TrayInput::Event(&ev(
                r#"{"type":"model_loading","model":"m","ts_ms":1}"#
            ))),
            TrayState::Processing
        );
        assert_eq!(
            TrayState::Processing.next(TrayInput::Event(&ev(
                r#"{"type":"model_ready","model":"m","ts_ms":1}"#
            ))),
            TrayState::Idle
        );
        assert_eq!(
            TrayState::Idle.next(TrayInput::Event(&ev(
                r#"{"type":"model_load_failed","model":"m","error":"OOM","ts_ms":1}"#
            ))),
            TrayState::Error
        );
    }

    #[test]
    fn connection_signals() {
        assert_eq!(
            TrayState::Recording.next(TrayInput::ConnectionLost),
            TrayState::Error
        );
        assert_eq!(
            TrayState::Error.next(TrayInput::ConnectionRestored),
            TrayState::Idle
        );
    }

    #[test]
    fn model_download_does_not_change_tray_state() {
        let idle = TrayState::Idle;
        assert_eq!(
            idle.next(TrayInput::Event(&ev(
                r#"{"type":"model.download","id":"stt-large-v3-turbo","downloaded":1,"total":2,"phase":"downloading","ts_ms":1}"#
            ))),
            TrayState::Idle
        );
        let rec = TrayState::Recording;
        assert_eq!(
            rec.next(TrayInput::Event(&ev(
                r#"{"type":"model.download","id":"stt-large-v3-turbo","downloaded":1,"total":2,"phase":"error","error":"boom","ts_ms":1}"#
            ))),
            TrayState::Recording
        );
    }

    #[test]
    fn partials_and_levels_do_not_change_state() {
        let rec = TrayState::Recording;
        assert_eq!(
            rec.next(TrayInput::Event(&ev(
                r#"{"type":"partial_transcript","session_id":"s","segment_text":"č","ts_ms":1}"#
            ))),
            TrayState::Recording
        );
        assert_eq!(
            rec.next(TrayInput::Event(&ev(
                r#"{"type":"audio.level","session_id":"s","rms":0.1,"peak":0.2,"ts_ms":1}"#
            ))),
            TrayState::Recording
        );
    }
}
