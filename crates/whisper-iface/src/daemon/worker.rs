//! The engine worker thread — the actor that owns the `!Send` engines and the `Pipeline`
//! (D-0011). This is the ONLY thread that touches whisper.cpp; the async axum side
//! never does. It receives [`Job`]s over a channel, runs one at a time (the single-active-session
//! invariant of `ARCHITECTURE.md` §3.2), and finalizes an authoritative [`SessionRecord`].
//!
//! Why a thread and not a task: the whisper context is `!Send`/`!Sync`, and the engines are
//! shared via `Rc`; pinning them to one OS thread is the constraint D-0011 is built around. Engines are constructed *on this thread* by the
//! `builder` closure — they never cross a thread boundary.
//!
//! Per job, a fresh `Pipeline` is assembled from the resident engines (shared cheaply via `Rc`)
//! plus a per-job `VecAudioSource`, a fresh VAD (segmenter state must not leak between sessions),
//! and the current dictionary/cleanup-mode. A short-lived forwarder thread drains the pipeline's
//! synchronous event channel live, fans each event out to the WS broadcast, and builds the record.

use std::rc::Rc;
use std::sync::{Arc, RwLock, Weak};
use std::time::Instant;

use tokio::sync::mpsc;

use whisper_core::dictionary::Dictionary;
use whisper_core::error::{AudioError, CleanupError, DeliverError, SttError};
use whisper_core::pipeline::{CleanupMode, Pipeline, PipelineConfig, PipelineEvent};
use whisper_core::ports::{
    AudioSource, AudioStream, CleanupEngine, DeliverOutcome, SttEngine, TextSink, Vad,
};
use whisper_core::skip_gate::SkipGateConfig;
use whisper_core::types::{AudioCfg, AudioFrame, CleanupCtx, SttOpts, TranscriptRaw};

use super::api::{
    skip_reason_str, ApiEvent, CleanupInfo, LatencyInfo, SessionRecord, SessionStatusDto,
};
use super::state::{now_ms, AppState, EnginePhase, Job, JobAudio, SinkKind};
use crate::cleanup::CleanupHealth;

/// Context handed to the engine builder so it can wire runtime-configurable knobs into the
/// resident engines without rebuilding them later (e.g. STT language, D-0004 `-l cs`).
pub struct BuildCtx {
    /// Shared effective language; the STT wrapper reads it per decode so `/v1/config` language
    /// edits take effect on the next session.
    pub lang: Arc<RwLock<String>>,
    /// Shared VAD tuning; the per-session `vad_builder` reads it each session, so a `/v1/config`
    /// `[vad]` edit takes effect on the next session without a restart (D-0008).
    pub vad: Arc<RwLock<whisper_core::vad::VadConfig>>,
    /// Shared cleanup model tag (empty = off); the Ollama cleanup engine reads it per utterance
    /// so a `/v1/config` model edit applies without a restart (D-0017).
    pub cleanup_model: Arc<RwLock<String>>,
    /// Live cleanup readiness the engine writes and `/v1/status` reads.
    pub cleanup_health: Arc<RwLock<CleanupHealth>>,
}

/// The resident engines the worker keeps loaded for the process lifetime. STT is required;
/// cleanup is optional (D-0005). `Rc` lets a fresh per-job `Pipeline` share the same loaded
/// models without reloading — sound because everything stays on the single worker thread.
pub struct ResidentEngines {
    pub stt: Rc<dyn SttEngine>,
    pub stt_model_label: String,
    pub cleanup: Option<Rc<dyn CleanupEngine>>,
    /// Builds a fresh VAD per session (segmenter/buffer state must start clean each time).
    pub vad_builder: Box<dyn Fn() -> Result<Box<dyn Vad>, String>>,
    /// Builds the delivery sink for a session's [`SinkKind`] (D-0012). The default (headless)
    /// builder ignores the kind and always returns [`NullSink`]; the `platform` builder maps
    /// `Paste` to a pasteboard sink. Kept in the engine set so the sink is constructed on the
    /// worker thread (a `PasteboardSink` is `!Send`).
    pub sink_builder: SinkBuilder,
}

/// Builds a per-session delivery [`TextSink`] from a [`SinkKind`] (D-0012). `Send` because it is
/// captured by the `Send` [`EngineBuilder`] closure and moved to the worker thread; the sink it
/// *produces* need not be `Send` (it is built and used only on that thread).
pub type SinkBuilder = Box<dyn Fn(SinkKind) -> Box<dyn TextSink> + Send>;

/// The headless default sink builder: delivery means "available via the API", so every kind maps
/// to [`NullSink`]. Used by [`crate::production_engine_builder`] and the contract-test fakes.
pub fn null_sink_builder() -> SinkBuilder {
    Box::new(|_kind| Box::new(NullSink) as Box<dyn TextSink>)
}

/// Closure that constructs the resident engines *on the worker thread*. Must be `Send` (it is
/// moved to the thread); its output is used only on that thread, so the engines may be `!Send`.
pub type EngineBuilder = Box<dyn FnOnce(BuildCtx) -> Result<ResidentEngines, String> + Send>;

/// Spawn the engine worker thread. It holds only a `Weak<AppState>`, so once the daemon drops
/// all strong references the command channel closes and this thread exits cleanly. `cmd_rx` is
/// the receive half of [`AppState::cmd_tx`].
pub fn spawn_weak(
    state: Weak<AppState>,
    mut cmd_rx: mpsc::Receiver<Job>,
    builder: EngineBuilder,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("whisperd-engine".to_string())
        .spawn(move || {
            // Model-load phase: needs the state briefly to emit lifecycle events.
            let engines = {
                let Some(state) = state.upgrade() else { return };
                let lang = state.lang.clone();
                let vad = state.vad.clone();
                let cleanup_model = state.cleanup_model.clone();
                let cleanup_health = state.cleanup_health.clone();
                state.emit(ApiEvent::ModelLoading {
                    model: "stt".to_string(),
                    ts_ms: now_ms(),
                });
                match builder(BuildCtx {
                    lang,
                    vad,
                    cleanup_model,
                    cleanup_health,
                }) {
                    Ok(engines) => {
                        update_status_ready(&state, &engines);
                        Some(engines)
                    }
                    Err(err) => {
                        set_stt_unavailable(&state, &err);
                        state.emit(ApiEvent::ModelLoadFailed {
                            model: "stt".to_string(),
                            error: err,
                            ts_ms: now_ms(),
                        });
                        None
                    }
                }
            };

            // Blocking receive on a plain OS thread (no async context here) — the actor loop.
            while let Some(job) = cmd_rx.blocking_recv() {
                let Some(state) = state.upgrade() else { break };
                match &engines {
                    Some(engines) => process_job(&state, engines, job),
                    None => finalize_unavailable(&state, job),
                }
            }
        })
        .expect("spawn engine worker thread")
}

fn update_status_ready(state: &AppState, engines: &ResidentEngines) {
    let mut status = state.status.write().expect("status lock");
    status.stt_ready = true;
    status.stt_model = Some(engines.stt_model_label.clone());
    // Cleanup health is live (the `status` handler derives `Degraded` from it), so STT being up
    // is all the worker decides here.
    status.phase = EnginePhase::Ready;
    drop(status);

    state.emit(ApiEvent::ModelReady {
        model: "stt".to_string(),
        ts_ms: now_ms(),
    });
}

fn set_stt_unavailable(state: &AppState, err: &str) {
    let mut status = state.status.write().expect("status lock");
    status.phase = EnginePhase::Unavailable;
    status.stt_ready = false;
    status.stt_error = Some(err.to_string());
}

/// Reject a job when no STT engine is loaded: emit a `Failed` record so the caller still gets a
/// structured result and the active gate is released.
fn finalize_unavailable(state: &AppState, job: Job) {
    let record = SessionRecord {
        session_id: job.session_id.clone(),
        status: SessionStatusDto::Failed,
        text: String::new(),
        lang: String::new(),
        cleanup: CleanupInfo {
            applied: false,
            skip_reason: None,
        },
        latency: LatencyInfo::default(),
        error: Some("stt model unavailable".to_string()),
        created_at_ms: now_ms(),
    };
    state.emit(ApiEvent::Failed {
        session_id: job.session_id.clone(),
        error: "stt model unavailable".to_string(),
        partial_text: None,
        ts_ms: now_ms(),
    });
    finalize(state, &job.session_id, record);
}

/// Run one session end to end on the worker thread.
fn process_job(state: &AppState, engines: &ResidentEngines, job: Job) {
    let cfg_mode: CleanupMode = state
        .config
        .read()
        .expect("config lock")
        .cleanup_mode
        .into();
    let cleanup_mode = job.cleanup_mode.unwrap_or(cfg_mode);
    let lang = state.lang.read().expect("lang lock").clone();

    let dictionary = current_dictionary(state);

    let vad = match (engines.vad_builder)() {
        Ok(v) => v,
        Err(err) => {
            let record = failed_record(&job.session_id, format!("vad init: {err}"));
            state.emit(ApiEvent::Failed {
                session_id: job.session_id.clone(),
                error: format!("vad init: {err}"),
                partial_text: None,
                ts_ms: now_ms(),
            });
            finalize(state, &job.session_id, record);
            return;
        }
    };

    let cleanup: Option<Box<dyn CleanupEngine>> = engines
        .cleanup
        .clone()
        .map(|c| Box::new(RcCleanup(c)) as Box<dyn CleanupEngine>);

    // Build the audio source. For a live mic session the factory eager-opens the device here, so
    // a permission/device failure is finalized cleanly (the factory already emitted
    // `platform.permission` for a denial, D-0012).
    let source: Box<dyn AudioSource> = match job.audio {
        JobAudio::Pcm(pcm) => Box::new(VecAudioSource::new(pcm)),
        JobAudio::Live(factory) => match factory() {
            Ok(source) => source,
            Err(err) => {
                let msg = err.to_string();
                let record = failed_record(&job.session_id, msg.clone());
                state.emit(ApiEvent::Failed {
                    session_id: job.session_id.clone(),
                    error: msg,
                    partial_text: None,
                    ts_ms: now_ms(),
                });
                finalize(state, &job.session_id, record);
                return;
            }
        },
    };

    let sink = (engines.sink_builder)(job.sink);

    let mut pipeline = Pipeline::new(
        source,
        vad,
        Box::new(RcStt(engines.stt.clone())),
        dictionary,
        cleanup,
        sink,
        PipelineConfig {
            cleanup_mode,
            skip_gate: SkipGateConfig::default(),
        },
    );

    // Subscribe before running, then drain the synchronous event channel on a helper thread so
    // WS clients see partials live (the channel `Receiver` is `Send`; the `Pipeline` is not).
    let event_rx = pipeline.subscribe();
    let session_id = job.session_id.clone();
    let events = state.events.clone();
    let forwarder = std::thread::spawn(move || forward_events(event_rx, session_id, events));

    let cleanup_ctx = CleanupCtx { lang };
    let _final_state = pipeline.run(AudioCfg::default(), &cleanup_ctx, &job.token);
    // Dropping the pipeline closes the event sender, ending the forwarder loop.
    drop(pipeline);
    let record = forwarder.join().expect("event forwarder thread");

    finalize(state, &job.session_id, record);
}

/// Store the record, wake any blocking `/v1/transcribe` waiter, drop the cancellation token, and
/// release the active-session gate — in that order, so a new session is only admitted once the
/// result is durably retrievable via `GET /v1/sessions/{id}`.
fn finalize(state: &AppState, session_id: &str, record: SessionRecord) {
    if let Err(err) = state.store.upsert(&record) {
        // History write failed; the in-memory copy still succeeded. Surface on stderr only.
        eprintln!("whisperd: history write failed for {session_id}: {err}");
    }
    if let Some(tx) = state.take_waiter(session_id) {
        let _ = tx.send(record);
    }
    let _ = state.take_cancel(session_id);
    let _ = state.take_capture(session_id);
    state.end_active();
}

fn current_dictionary(state: &AppState) -> Dictionary {
    let json = state
        .dictionary
        .read()
        .expect("dictionary lock")
        .json
        .clone();
    // The JSON was validated at PUT time; if it somehow fails to parse, fall back to empty
    // rather than failing the session (`ARCHITECTURE.md` §3.2: never die with a session).
    Dictionary::from_json_str(&json.to_string()).unwrap_or_else(|_| Dictionary::empty())
}

fn failed_record(session_id: &str, error: String) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        status: SessionStatusDto::Failed,
        text: String::new(),
        lang: String::new(),
        cleanup: CleanupInfo {
            applied: false,
            skip_reason: None,
        },
        latency: LatencyInfo::default(),
        error: Some(error),
        created_at_ms: now_ms(),
    }
}

/// Drain the pipeline's event channel until it closes, fanning each event out to the WS
/// broadcast (best-effort) and accumulating the authoritative [`SessionRecord`] with per-phase
/// latencies (`ARCHITECTURE.md` §3.4).
fn forward_events(
    rx: std::sync::mpsc::Receiver<PipelineEvent>,
    session_id: String,
    events: tokio::sync::broadcast::Sender<ApiEvent>,
) -> SessionRecord {
    let start = Instant::now();
    let created_at_ms = now_ms();
    let mut record = SessionRecord::running(session_id.clone(), created_at_ms);

    let mut t_raw: Option<u64> = None;
    let mut t_cleanup: Option<u64> = None;
    let mut t_deliver: Option<u64> = None;

    for event in rx {
        let elapsed = start.elapsed().as_millis() as u64;
        let ts_ms = now_ms();
        match event {
            PipelineEvent::SessionStarted => {
                let _ = events.send(ApiEvent::SessionStarted {
                    session_id: session_id.clone(),
                    ts_ms,
                });
            }
            PipelineEvent::PartialTranscript { segment_text } => {
                let _ = events.send(ApiEvent::PartialTranscript {
                    session_id: session_id.clone(),
                    segment_text,
                    ts_ms,
                });
            }
            PipelineEvent::RawTranscript { text, lang } => {
                record.lang = lang.clone();
                t_raw = Some(elapsed);
                let _ = events.send(ApiEvent::RawTranscript {
                    session_id: session_id.clone(),
                    text,
                    lang,
                    ts_ms,
                });
            }
            PipelineEvent::DictionaryApplied { text } => {
                let _ = events.send(ApiEvent::DictionaryApplied {
                    session_id: session_id.clone(),
                    text,
                    ts_ms,
                });
            }
            PipelineEvent::CleanupSkipped { reason } => {
                record.cleanup = CleanupInfo {
                    applied: false,
                    skip_reason: Some(skip_reason_str(reason).to_string()),
                };
                t_cleanup = Some(elapsed);
                let _ = events.send(ApiEvent::CleanupSkipped {
                    session_id: session_id.clone(),
                    reason: skip_reason_str(reason).to_string(),
                    ts_ms,
                });
            }
            PipelineEvent::Cleaned { text } => {
                record.cleanup = CleanupInfo {
                    applied: true,
                    skip_reason: None,
                };
                t_cleanup = Some(elapsed);
                let _ = events.send(ApiEvent::Cleaned {
                    session_id: session_id.clone(),
                    text,
                    ts_ms,
                });
            }
            PipelineEvent::SecureInputBlocked { owner } => {
                let _ = events.send(ApiEvent::SecureInputBlocked {
                    session_id: session_id.clone(),
                    owner,
                    ts_ms,
                });
            }
            PipelineEvent::Delivered { text } => {
                record.text = text.clone();
                record.status = SessionStatusDto::Completed;
                t_deliver = Some(elapsed);
                let _ = events.send(ApiEvent::Delivered {
                    session_id: session_id.clone(),
                    text,
                    ts_ms,
                });
            }
            PipelineEvent::Cancelled { partial_text } => {
                record.text = partial_text.clone().unwrap_or_default();
                record.status = SessionStatusDto::Cancelled;
                let _ = events.send(ApiEvent::Cancelled {
                    session_id: session_id.clone(),
                    partial_text,
                    ts_ms,
                });
            }
            PipelineEvent::Failed {
                error,
                partial_text,
            } => {
                record.text = partial_text.clone().unwrap_or_default();
                record.status = SessionStatusDto::Failed;
                record.error = Some(error.clone());
                let _ = events.send(ApiEvent::Failed {
                    session_id: session_id.clone(),
                    error,
                    partial_text,
                    ts_ms,
                });
            }
        }
    }

    let total = start.elapsed().as_millis() as u64;
    let stt_ms = t_raw.unwrap_or(total);
    let cleanup_ms = t_cleanup.map(|t| t.saturating_sub(stt_ms)).unwrap_or(0);
    let deliver_ms = t_deliver
        .map(|t| t.saturating_sub(t_cleanup.unwrap_or(stt_ms)))
        .unwrap_or(0);
    record.latency = LatencyInfo {
        stt_ms,
        cleanup_ms,
        deliver_ms,
        total_ms: total,
    };
    record
}

// ---------------------------------------------------------------------------
// Per-job / resident adapters
// ---------------------------------------------------------------------------

/// A [`Vad`]-agnostic [`AudioSource`] that replays an owned PCM buffer as fixed-size frames.
/// One job's audio, already decoded from the request (`ARCHITECTURE.md` §4 headless input).
struct VecAudioSource {
    pcm: std::sync::Mutex<Option<Vec<f32>>>,
}

impl VecAudioSource {
    fn new(pcm: Vec<f32>) -> Self {
        Self {
            pcm: std::sync::Mutex::new(Some(pcm)),
        }
    }
}

const FRAME_LEN: usize = 1600; // 100 ms @ 16 kHz mono, matching the fixture source.

impl AudioSource for VecAudioSource {
    fn start(&self, _cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
        let pcm =
            self.pcm.lock().expect("pcm mutex").take().ok_or_else(|| {
                AudioError::StartFailed("audio source already consumed".to_string())
            })?;
        Ok(Box::new(VecAudioStream { pcm, pos: 0 }))
    }
}

struct VecAudioStream {
    pcm: Vec<f32>,
    pos: usize,
}

impl AudioStream for VecAudioStream {
    fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError> {
        if self.pos >= self.pcm.len() {
            return Ok(None);
        }
        let end = (self.pos + FRAME_LEN).min(self.pcm.len());
        let frame = self.pcm[self.pos..end].to_vec();
        self.pos = end;
        Ok(Some(frame))
    }
}

/// Headless [`TextSink`]: delivery means "made available via the API", so there is nothing to
/// inject. The final text is carried on the `Delivered` event, not read back from here (platform
/// text injection arrives with `whisper-platform` in Phase 5).
struct NullSink;

impl TextSink for NullSink {
    fn deliver(&self, _text: &str) -> Result<DeliverOutcome, DeliverError> {
        Ok(DeliverOutcome::Injected)
    }
}

/// Share a resident STT engine across per-job pipelines without reloading the model. `Rc` is
/// sound here because every clone stays on the single worker thread.
struct RcStt(Rc<dyn SttEngine>);

impl SttEngine for RcStt {
    fn transcribe(&self, pcm: &[f32], opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        self.0.transcribe(pcm, opts)
    }
}

/// Share a resident cleanup engine across per-job pipelines (see [`RcStt`]).
struct RcCleanup(Rc<dyn CleanupEngine>);

impl CleanupEngine for RcCleanup {
    fn clean(&self, text: &str, ctx: &CleanupCtx) -> Result<String, CleanupError> {
        self.0.clean(text, ctx)
    }
}

/// Wrap an STT engine so it injects the configured language (D-0004 `-l cs`) when the pipeline
/// passes no hint — the pipeline calls STT with `SttOpts::default()`, so this is how a
/// `/v1/config` `language` edit reaches decoding without touching the core.
pub struct LangInjectingStt {
    inner: Box<dyn SttEngine>,
    lang: Arc<RwLock<String>>,
}

impl LangInjectingStt {
    pub fn new(inner: Box<dyn SttEngine>, lang: Arc<RwLock<String>>) -> Self {
        Self { inner, lang }
    }
}

impl SttEngine for LangInjectingStt {
    fn transcribe(&self, pcm: &[f32], opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        let hint = opts.lang_hint.clone().or_else(|| {
            let lang = self.lang.read().expect("lang lock").clone();
            if lang.is_empty() {
                None
            } else {
                Some(lang)
            }
        });
        self.inner.transcribe(pcm, &SttOpts { lang_hint: hint })
    }
}
