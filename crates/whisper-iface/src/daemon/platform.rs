//! macOS platform composition (`ARCHITECTURE.md` §5, D-0012) — compiled only with the `platform`
//! feature, so the default headless build is byte-for-byte unchanged. This is the composition
//! root that plugs `whisper-platform`'s adapters into the daemon:
//!
//! * [`cpal_live_source`] — a [`LiveSourceProvider`] backing `POST /v1/sessions {"source":"mic"}`
//!   with a CoreAudio [`CpalAudioSource`](whisper_platform::CpalAudioSource); it emits
//!   `platform.permission` / `audio.device_lost` WS events (the platform crate stays iface-free).
//! * [`pasteboard_sink_builder`] / [`platform_engine_builder`] — wire the D-0007 pasteboard sink
//!   in for `delivery = "paste"`.
//! * [`spawn_hotkey`] — the rdev hotkey listener, driving the same mic-session machinery.
//!
//! None of this runs in CI (every path needs a macOS TCC grant, D-0012); it is exercised via the
//! manual checklist (`docs/phase5-manual-verification.md`).

use std::sync::Arc;

use whisper_core::ports::{AudioSource, TextSink};
use whisper_core::session::CancellationToken;

use whisper_platform::{
    macos_pasteboard_sink, spawn_listener, AudioLevel, AutoGainConfig, CpalAudioSource,
    HotkeyCombo, HotkeyEvent, HotkeyMode, HotkeySession, SessionIntent,
};

use super::api::{ApiEvent, SessionRecord};
use super::config::{DeliveryMode, HotkeyConfig, HotkeyModeDto};
use super::state::{
    now_ms, AppState, AudioSourceFactory, CaptureControl, EnginePhase, Job, JobAudio,
    LiveSourceCtx, LiveSourceProvider, SinkKind,
};
use super::worker::{null_sink_builder, EngineBuilder, SinkBuilder};

/// Live mic capture backed by CoreAudio (D-0012). Each session builds a [`CpalAudioSource`] that
/// resamples to 16 kHz mono and boosts quiet input via AGC-lite (`agc`); permission denial and
/// mid-session device loss reach WS as `platform.permission` / `audio.device_lost`, and the
/// throttled post-gain input level reaches WS as `audio.level` (D-0013 level meter).
pub fn cpal_live_source(agc: AutoGainConfig) -> LiveSourceProvider {
    Box::new(move |ctx: LiveSourceCtx| {
        let LiveSourceCtx {
            session_id,
            events,
            stop,
        } = ctx;
        Box::new(move || {
            let sid = session_id.clone();
            let dl_events = events.clone();
            let on_device_lost = Box::new(move |reason: String| {
                let _ = dl_events.send(ApiEvent::AudioDeviceLost {
                    session_id: Some(sid.clone()),
                    reason,
                    ts_ms: now_ms(),
                });
            });
            let sid_lvl = session_id.clone();
            let lvl_events = events.clone();
            let on_level = Box::new(move |lvl: AudioLevel| {
                let _ = lvl_events.send(ApiEvent::AudioLevel {
                    session_id: sid_lvl.clone(),
                    rms: lvl.rms,
                    peak: lvl.peak,
                    ts_ms: now_ms(),
                });
            });
            match CpalAudioSource::open(stop, agc, on_device_lost, on_level) {
                Ok(source) => Ok(Box::new(source) as Box<dyn AudioSource>),
                Err(err) => {
                    if err.is_permission_denied() {
                        let _ = events.send(ApiEvent::PlatformPermission {
                            session_id: Some(session_id.clone()),
                            permission: "microphone".to_string(),
                            granted: false,
                            detail: Some(err.to_string()),
                            ts_ms: now_ms(),
                        });
                    }
                    Err(err.into())
                }
            }
        }) as AudioSourceFactory
    })
}

/// Delivery sink builder that injects via the pasteboard (D-0007) for `SinkKind::Paste`, falling
/// back to the null sink if the clipboard cannot be opened. `SinkKind::Null` is the null sink.
pub fn pasteboard_sink_builder() -> SinkBuilder {
    let null = null_sink_builder();
    Box::new(move |kind| match kind {
        SinkKind::Paste => match macos_pasteboard_sink() {
            Ok(sink) => Box::new(sink) as Box<dyn TextSink>,
            Err(err) => {
                eprintln!("whisperd: pasteboard sink unavailable, delivering to null: {err}");
                null(SinkKind::Null)
            }
        },
        SinkKind::Null => null(SinkKind::Null),
    })
}

/// Production engine set with the pasteboard delivery sink wired in (D-0012). Same STT/VAD/cleanup
/// as [`crate::production_engine_builder`], differing only in the delivery sink.
pub fn platform_engine_builder(
    stt_path: std::path::PathBuf,
    cleanup_path: Option<std::path::PathBuf>,
) -> EngineBuilder {
    super::production_engine_builder_with_sink(stt_path, cleanup_path, pasteboard_sink_builder())
}

/// Start a live mic session synchronously (from the hotkey thread — outside the tokio runtime).
/// Mirrors `POST /v1/sessions {"source":"mic"}` but uses `try_send` (no await). Returns the new
/// session id, or an error string if the engine is not ready / busy / has no capture backend.
pub fn start_live_session(state: &Arc<AppState>) -> Result<String, String> {
    match state.status.read().expect("status lock").phase {
        EnginePhase::Ready | EnginePhase::Degraded => {}
        other => return Err(format!("engine not ready ({})", other.as_str())),
    }
    if state.live_source.is_none() {
        return Err("no capture backend".to_string());
    }
    let session_id = state.new_session_id();
    if !state.try_begin(&session_id) {
        return Err("busy".to_string());
    }
    let token = CancellationToken::new();
    state.register_cancel(&session_id, token.clone());
    let control = CaptureControl::new();
    state.register_capture(&session_id, control.clone());
    let _ = state
        .store
        .upsert(&SessionRecord::running(session_id.clone(), now_ms()));

    let provider = state
        .live_source
        .as_ref()
        .expect("live_source presence checked above");
    let audio = JobAudio::Live(provider(LiveSourceCtx {
        session_id: session_id.clone(),
        events: state.events.clone(),
        stop: control.flag(),
    }));
    let sink = match state.config.read().expect("config lock").delivery {
        DeliveryMode::Paste => SinkKind::Paste,
        DeliveryMode::None => SinkKind::Null,
    };
    let job = Job {
        session_id: session_id.clone(),
        audio,
        cleanup_mode: None,
        token,
        sink,
    };
    if state.cmd_tx.try_send(job).is_err() {
        state.take_cancel(&session_id);
        state.take_capture(&session_id);
        state.end_active();
        return Err("engine worker channel full".to_string());
    }
    Ok(session_id)
}

/// Gracefully stop a live mic session (end capture, let the pipeline finish, D-0012).
pub fn stop_live_session(state: &Arc<AppState>, session_id: &str) {
    if let Some(control) = state.get_capture(session_id) {
        control.stop();
    }
}

/// Start the global hotkey listener + driver thread if `cfg.enabled` (D-0012). The rdev listener
/// feeds [`HotkeyEvent`]s to a driver that applies the mode and starts/stops mic sessions through
/// the same machinery as the HTTP API. Returns the driver thread handle (the rdev listener thread
/// is detached inside `spawn_listener`).
pub fn spawn_hotkey(
    state: Arc<AppState>,
    cfg: &HotkeyConfig,
) -> Option<std::thread::JoinHandle<()>> {
    if !cfg.enabled {
        return None;
    }
    let combo = match HotkeyCombo::parse(&cfg.combo) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("whisperd: invalid hotkey combo {:?}: {err}", cfg.combo);
            return None;
        }
    };
    let mode = match cfg.mode {
        HotkeyModeDto::PushToTalk => HotkeyMode::PushToTalk,
        HotkeyModeDto::Toggle => HotkeyMode::Toggle,
    };

    let (tx, rx) = std::sync::mpsc::channel::<HotkeyEvent>();
    // If the rdev listen loop cannot start (most commonly a missing Input Monitoring TCC grant),
    // whisper-platform would otherwise die silently and the hotkey would just appear broken to
    // the user (observed in real user testing) — surface it on stderr and as a WS event so a UI
    // shell can show it.
    let err_state = state.clone();
    let on_error: Box<dyn FnOnce(String) + Send> = Box::new(move |msg: String| {
        eprintln!(
            "whisperd: hotkey listener failed to start ({msg}). This usually means macOS \
             Input Monitoring permission is missing — grant it in System Settings › Privacy \
             & Security › Input Monitoring, then restart whisperd."
        );
        let _ = err_state.events.send(ApiEvent::PlatformPermission {
            session_id: None,
            permission: "input_monitoring".to_string(),
            granted: false,
            detail: Some(msg),
            ts_ms: now_ms(),
        });
    });
    // The rdev listener runs on its own (detached) thread inside `whisper-platform`.
    let _listener = spawn_listener(combo, tx, on_error);

    let handle = std::thread::Builder::new()
        .name("whisperd-hotkey-driver".to_string())
        .spawn(move || {
            let mut session = HotkeySession::new(mode);
            let mut current: Option<String> = None;
            for ev in rx {
                match session.apply(ev) {
                    Some(SessionIntent::Start) => match start_live_session(&state) {
                        Ok(id) => current = Some(id),
                        Err(err) => eprintln!("whisperd: hotkey start ignored: {err}"),
                    },
                    Some(SessionIntent::Stop) => {
                        if let Some(id) = current.take() {
                            stop_live_session(&state, &id);
                        }
                    }
                    None => {}
                }
            }
        })
        .expect("spawn hotkey driver thread");
    Some(handle)
}
