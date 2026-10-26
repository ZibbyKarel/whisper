//! Pipeline orchestrator (`ARCHITECTURE.md` §3.2): drives
//! `AudioSource -> Vad -> SttEngine -> dictionary pass -> cleanup gate -> CleanupEngine? ->
//! TextSink`, running the [`SessionFsm`](crate::session::SessionFsm) alongside it and
//! emitting [`PipelineEvent`]s through a subscriber channel — the seam `whisper-iface`
//! will bridge to WebSocket events.
//!
//! The cleanup gate (`ARCHITECTURE.md` §3.2, "cleanup gate", D-0005) is
//! [`CleanupMode`]-driven: `Always`/`Never` are explicit overrides, and the default `Auto`
//! calls [`crate::skip_gate`]'s marker heuristic on the dictionary-passed text to decide
//! per session.

use std::sync::mpsc::{self, Receiver, Sender};

use crate::dictionary::Dictionary;
use crate::error::{PipelineError, SttError};
use crate::ports::{
    AudioSource, CleanupEngine, DeliverOutcome, SttEngine, TextSink, Vad, VadEvent,
};
use crate::session::{CancellationToken, FailureInfo, SessionFsm, SessionState};
use crate::skip_gate::SkipGateConfig;
use crate::types::{AudioCfg, CleanupCtx, SttOpts};

/// How the cleanup stage is gated for a session (`ARCHITECTURE.md` §3.2, D-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CleanupMode {
    /// Always attempt cleanup (if a [`CleanupEngine`] is configured).
    Always,
    /// Never attempt cleanup; the dictionary-passed text is delivered as-is.
    Never,
    /// Run the skip-gate heuristic ([`crate::skip_gate::needs_cleanup`]) on the
    /// dictionary-passed text and only attempt cleanup when it says the text needs it.
    /// This is the default: it keeps the ~40% clean-dictation case on the fast path
    /// (`ARCHITECTURE.md` §3.4) without ever missing an explicit override.
    #[default]
    Auto,
}

/// Pipeline behavior knobs.
#[derive(Debug, Clone, Default)]
pub struct PipelineConfig {
    /// Cleanup gating mode; defaults to [`CleanupMode::Auto`].
    pub cleanup_mode: CleanupMode,
    /// Marker lists/thresholds the `Auto` mode's skip-gate heuristic uses; defaults to
    /// [`SkipGateConfig::default`]. Ignored in `Always`/`Never` mode.
    pub skip_gate: SkipGateConfig,
}

/// Events emitted by the pipeline as a session progresses. This is the seam
/// `whisper-iface` bridges to `WS /v1/events` (`ARCHITECTURE.md` §4).
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineEvent {
    SessionStarted,
    /// One VAD segment finished decoding during recording (`ARCHITECTURE.md` §3.2,
    /// per-VAD-segment decoding). Emitted as segments close, before the joined
    /// [`RawTranscript`](PipelineEvent::RawTranscript). This is the v1 partial: segment-
    /// granular, not word-level.
    PartialTranscript {
        segment_text: String,
    },
    RawTranscript {
        text: String,
        lang: String,
    },
    DictionaryApplied {
        text: String,
    },
    CleanupSkipped {
        reason: CleanupSkipReason,
    },
    Cleaned {
        text: String,
    },
    /// Injection was suppressed and the text left on the clipboard only (Secure Input active,
    /// D-0007). Emitted just before [`Delivered`](PipelineEvent::Delivered) — the session still
    /// completes; this only surfaces the clipboard-only fallback (`ARCHITECTURE.md` §5).
    SecureInputBlocked {
        owner: Option<String>,
    },
    Delivered {
        text: String,
    },
    Cancelled {
        partial_text: Option<String>,
    },
    Failed {
        error: String,
        partial_text: Option<String>,
    },
}

/// Why the cleanup stage did not run for a session (`ARCHITECTURE.md` §3.2 cleanup gate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupSkipReason {
    /// [`CleanupMode::Never`]: cleanup is off for this session/profile.
    ModeDisabled,
    /// [`CleanupMode::Auto`]: the skip-gate heuristic
    /// ([`crate::skip_gate::SkipGateConfig::needs_cleanup`]) found no disfluency/self-
    /// correction markers in the dictionary-passed text — the fast path
    /// (`ARCHITECTURE.md` §3.4).
    GateClean,
    /// Cleanup was wanted (`Always`, or `Auto` with the gate saying it's needed) but no
    /// [`CleanupEngine`] is configured — degrade gracefully rather than fail the session
    /// (`ARCHITECTURE.md` §3.2, "the engine never dies with a session").
    NoEngineConfigured,
    /// The [`CleanupEngine`] ran but returned an error (e.g. the Ollama server is down) —
    /// the dictionary-passed text is delivered instead; a dictation never fails because
    /// cleanup did.
    EngineFailed,
}

/// Drives one dictation session end to end through the ports it is constructed with.
pub struct Pipeline {
    audio: Box<dyn AudioSource>,
    vad: Box<dyn Vad>,
    stt: Box<dyn SttEngine>,
    dictionary: Dictionary,
    cleanup: Option<Box<dyn CleanupEngine>>,
    sink: Box<dyn TextSink>,
    config: PipelineConfig,
    subscribers: Vec<Sender<PipelineEvent>>,
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        audio: Box<dyn AudioSource>,
        vad: Box<dyn Vad>,
        stt: Box<dyn SttEngine>,
        dictionary: Dictionary,
        cleanup: Option<Box<dyn CleanupEngine>>,
        sink: Box<dyn TextSink>,
        config: PipelineConfig,
    ) -> Self {
        Self {
            audio,
            vad,
            stt,
            dictionary,
            cleanup,
            sink,
            config,
            subscribers: Vec::new(),
        }
    }

    /// Subscribe to pipeline events for the lifetime of this `Pipeline`. Each call
    /// returns an independent receiver; every subscriber gets every event.
    pub fn subscribe(&mut self) -> Receiver<PipelineEvent> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.push(tx);
        rx
    }

    fn emit(&self, event: PipelineEvent) {
        for tx in &self.subscribers {
            // A dropped receiver just stops getting events; it must not abort the session.
            let _ = tx.send(event.clone());
        }
    }

    /// Run one full dictation session: `Idle -> Recording -> Transcribing -> Cleaning ->
    /// Delivering -> Idle`, or an early `Cancelled`/`Failed` exit. Cancellation is checked
    /// between every stage via `token`.
    pub fn run(
        &mut self,
        audio_cfg: AudioCfg,
        cleanup_ctx: &CleanupCtx,
        token: &CancellationToken,
    ) -> SessionState {
        let mut fsm = SessionFsm::new();
        fsm.transition(SessionState::Recording)
            .expect("Idle->Recording is always valid");
        self.emit(PipelineEvent::SessionStarted);
        // Warm the cleanup backend while the user is still speaking.
        if let Some(engine) = self.cleanup.as_ref().filter(|e| e.is_enabled()) {
            engine.prepare();
        }

        let mut stream = match self.audio.start(audio_cfg) {
            Ok(stream) => stream,
            Err(err) => return self.fail(&mut fsm, PipelineError::from(err), None),
        };

        // Streaming VAD segmentation with per-segment decoding (`ARCHITECTURE.md` §3.2,
        // D-0008): frames flow through the VAD, and each segment is decoded as it closes —
        // during the capture loop — so whisper never sees the multi-second silence that
        // triggers the "Titulky vytvořil JohnyX." hallucination, and earlier segments'
        // decode overlaps recording.
        let mut segment_texts: Vec<String> = Vec::new();
        let mut current: Vec<f32> = Vec::new(); // PCM of the currently-open segment
        let mut lang: Option<String> = None;
        let mut device_error = None;
        loop {
            match stream.next_frame() {
                Ok(Some(frame)) => {
                    let events = self.vad.push(&frame);
                    if let Err(err) =
                        self.drain_vad_events(events, &mut current, &mut segment_texts, &mut lang)
                    {
                        // Segment decode failed mid-recording: fail carrying what decoded.
                        let partial = Self::join_nonempty(&segment_texts);
                        return self.fail(&mut fsm, PipelineError::from(err), partial);
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    device_error = Some(err);
                    break;
                }
            }
        }

        if let Some(err) = device_error {
            // Best-effort: already-decoded segments plus a decode of the open segment's
            // buffer ("raw transcript beats nothing", `ARCHITECTURE.md` §3.2).
            let mut parts = segment_texts.clone();
            if !current.is_empty() {
                if let Ok(t) = self.stt.transcribe(&current, &SttOpts::default()) {
                    parts.push(t.text);
                }
            }
            let partial_text = Self::join_nonempty(&parts);
            return self.fail(&mut fsm, PipelineError::from(err), partial_text);
        }

        // End of stream: close any segment the VAD still holds open.
        let flush_events = self.vad.flush();
        if let Err(err) =
            self.drain_vad_events(flush_events, &mut current, &mut segment_texts, &mut lang)
        {
            let partial = Self::join_nonempty(&segment_texts);
            return self.fail(&mut fsm, PipelineError::from(err), partial);
        }

        if token.is_cancelled() {
            return self.cancel(&mut fsm, Self::join_nonempty(&segment_texts));
        }

        fsm.transition(SessionState::Transcribing)
            .expect("Recording->Transcribing is always valid here");
        // Final text = segments joined with a single space (formatting pass handles
        // punctuation/casing joins later, `ARCHITECTURE.md` §3.2). An all-silence session
        // yields no segments and an empty transcript — a clean result, not an error.
        let raw_text = segment_texts.join(" ");
        self.emit(PipelineEvent::RawTranscript {
            text: raw_text.clone(),
            lang: lang.clone().unwrap_or_default(),
        });

        let dict_text = self.dictionary.apply(&raw_text);
        self.emit(PipelineEvent::DictionaryApplied {
            text: dict_text.clone(),
        });

        if token.is_cancelled() {
            return self.cancel(&mut fsm, Some(dict_text));
        }

        fsm.transition(SessionState::Cleaning)
            .expect("Transcribing->Cleaning is always valid here");
        // Cleanup gate (`ARCHITECTURE.md` §3.2, D-0005): `Always`/`Never` are explicit
        // overrides; `Auto` asks the skip-gate heuristic about the dictionary-passed text.
        let wants_cleanup = match self.config.cleanup_mode {
            CleanupMode::Always => true,
            CleanupMode::Never => false,
            CleanupMode::Auto => self.config.skip_gate.needs_cleanup(&dict_text),
        };
        let final_text = if wants_cleanup {
            match self.cleanup.as_ref().filter(|e| e.is_enabled()) {
                Some(engine) => match engine.clean(&dict_text, cleanup_ctx) {
                    Ok(cleaned) => {
                        self.emit(PipelineEvent::Cleaned {
                            text: cleaned.clone(),
                        });
                        cleaned
                    }
                    Err(err) => {
                        // Degrade, never fail: deliver the dictionary-passed text.
                        eprintln!(
                            "whisper-core: cleanup failed, delivering dictionary text: {err}"
                        );
                        self.emit(PipelineEvent::CleanupSkipped {
                            reason: CleanupSkipReason::EngineFailed,
                        });
                        dict_text
                    }
                },
                None => {
                    self.emit(PipelineEvent::CleanupSkipped {
                        reason: CleanupSkipReason::NoEngineConfigured,
                    });
                    dict_text
                }
            }
        } else {
            let reason = match self.config.cleanup_mode {
                CleanupMode::Never => CleanupSkipReason::ModeDisabled,
                CleanupMode::Auto => CleanupSkipReason::GateClean,
                CleanupMode::Always => unreachable!("Always always sets wants_cleanup = true"),
            };
            self.emit(PipelineEvent::CleanupSkipped { reason });
            dict_text
        };

        if token.is_cancelled() {
            return self.cancel(&mut fsm, Some(final_text));
        }

        fsm.transition(SessionState::Delivering)
            .expect("Cleaning->Delivering is always valid here");
        match self.sink.deliver(&final_text) {
            Ok(outcome) => {
                if let DeliverOutcome::ClipboardOnly { owner } = outcome {
                    // Secure Input suppressed injection (D-0007): surface it, then still deliver
                    // — the text is on the clipboard, so the session completes, not fails.
                    self.emit(PipelineEvent::SecureInputBlocked { owner });
                }
                self.emit(PipelineEvent::Delivered {
                    text: final_text.clone(),
                });
                fsm.transition(SessionState::Idle)
                    .expect("Delivering->Idle is always valid here");
                fsm.state().clone()
            }
            Err(err) => self.fail(&mut fsm, PipelineError::from(err), Some(final_text)),
        }
    }

    /// Apply a batch of [`VadEvent`]s: accumulate `SpeechFrame` PCM into the open segment,
    /// and on `SpeechEnd` decode that segment, emit its [`PipelineEvent::PartialTranscript`]
    /// and record it. `SpeechStart` resets the open-segment buffer.
    fn drain_vad_events(
        &self,
        events: Vec<VadEvent>,
        current: &mut Vec<f32>,
        segment_texts: &mut Vec<String>,
        lang: &mut Option<String>,
    ) -> Result<(), SttError> {
        for event in events {
            match event {
                VadEvent::SpeechStart => current.clear(),
                VadEvent::SpeechFrame(frame) => current.extend_from_slice(&frame),
                VadEvent::SpeechEnd => {
                    if !current.is_empty() {
                        let transcript = self.stt.transcribe(current, &SttOpts::default())?;
                        if lang.is_none() {
                            *lang = Some(transcript.lang.clone());
                        }
                        self.emit(PipelineEvent::PartialTranscript {
                            segment_text: transcript.text.clone(),
                        });
                        segment_texts.push(transcript.text);
                    }
                    current.clear();
                }
            }
        }
        Ok(())
    }

    /// Join decoded segments with single spaces, or `None` if there are none — used for the
    /// best-text-so-far carried on `Cancelled`/`Failed` (`ARCHITECTURE.md` §3.2).
    fn join_nonempty(segment_texts: &[String]) -> Option<String> {
        if segment_texts.is_empty() {
            None
        } else {
            Some(segment_texts.join(" "))
        }
    }

    fn fail(
        &self,
        fsm: &mut SessionFsm,
        err: PipelineError,
        partial_text: Option<String>,
    ) -> SessionState {
        let info = FailureInfo {
            error: err.to_string(),
            partial_text: partial_text.clone(),
        };
        let _ = fsm.transition(SessionState::Failed(info));
        self.emit(PipelineEvent::Failed {
            error: err.to_string(),
            partial_text,
        });
        fsm.state().clone()
    }

    fn cancel(&self, fsm: &mut SessionFsm, partial_text: Option<String>) -> SessionState {
        let _ = fsm.transition(SessionState::Cancelled);
        self.emit(PipelineEvent::Cancelled { partial_text });
        fsm.state().clone()
    }
}
