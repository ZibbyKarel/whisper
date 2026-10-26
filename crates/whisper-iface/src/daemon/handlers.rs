//! axum request handlers for the `/v1` control API (`ARCHITECTURE.md` §4). Handlers are the
//! async side of the D-0011 actor: they validate input, decode audio, admit the session behind
//! the single-active gate, and either await the worker's reply (`/v1/transcribe`) or return an
//! id to poll (`/v1/sessions`). They never touch an engine directly.

use std::io::Cursor;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use tokio::sync::oneshot;

use whisper_core::dictionary::Dictionary;
use whisper_core::session::CancellationToken;

use crate::cleanup::CleanupHealth;
use crate::ollama::{is_loopback_url, normalize_tag, InstalledModel, OllamaClient};

use super::api::{
    ApiEvent, AudioRequest, ConfigUpdateResponse, ErrorBody, ErrorEnvelope, HealthResponse,
    ModelEntryDto, ModelProgressDto, ModelStatus, ModelsListResponse, OllamaStatusDto,
    PlatformPermissionsDto, SessionAccepted, SessionRecord, SessionStatusDto, SourceKind,
    StatusResponse,
};
use super::config::{Config, DeliveryMode};
use super::models::{self, DownloadProgress};
use super::ollama as ollama_probe;
use super::state::{
    now_ms, AppState, CaptureControl, EnginePhase, Job, JobAudio, LiveSourceCtx, SinkKind,
};

/// Structured error that renders as the `/v1` [`ErrorEnvelope`] with the right status code.
pub struct ApiErr {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiErr {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    /// `409` structured busy error — the single-active-session semantics (`ARCHITECTURE.md` §3).
    pub fn busy() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "busy",
            "a session is already active; one active session per engine",
        )
    }
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", what)
    }
    /// `409` — a download for this model id is already in flight (D-0015: one active download
    /// per id).
    pub fn download_busy(id: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "download_busy",
            format!("model {id} is already downloading"),
        )
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", msg)
    }
    /// `401` — a bearer token is configured but missing/incorrect (`ARCHITECTURE.md` §4).
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code.to_string(),
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: state.version.clone(),
    })
}

pub async fn status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let s = state.status.read().expect("status lock").clone();
    let cfg = state.config.read().expect("config lock").clone();
    // Cleanup readiness is live (the engine updates it after every warm-up / utterance), so the
    // phase is derived here rather than fixed once at startup (D-0017).
    let model = state
        .cleanup_model
        .read()
        .expect("cleanup model lock")
        .clone();
    let health = state
        .cleanup_health
        .read()
        .expect("cleanup health lock")
        .clone();
    let enabled = !model.trim().is_empty();
    // A pending first warm-up (not ready, no error) is not a failure: STT already works.
    let degraded = enabled && !health.ready && health.error.is_some();
    let phase = if s.phase == EnginePhase::Ready && degraded {
        EnginePhase::Degraded
    } else {
        s.phase
    };
    Json(StatusResponse {
        phase: phase.as_str().to_string(),
        version: state.version.clone(),
        uptime_ms: state.started.elapsed().as_millis() as u64,
        active_session: state.active_session(),
        stt: ModelStatus {
            ready: s.stt_ready,
            model: s.stt_model,
            error: s.stt_error,
        },
        cleanup: ModelStatus {
            ready: enabled && health.ready,
            model: enabled.then_some(model),
            error: if enabled { health.error } else { None },
        },
        cleanup_mode: cfg.cleanup_mode,
        language: cfg.language,
        history_enabled: state.store.history_enabled(),
    })
}

/// `GET /v1/platform/permissions` (`ARCHITECTURE.md` §5, D-0016): the current macOS TCC grant
/// status for Input Monitoring (hotkey) and Accessibility (paste), queried fresh on every call
/// (both are cheap parameterless OS queries — no caching needed). Lets a UI detect a missing
/// grant proactively instead of waiting for a downstream failure that, for Input Monitoring,
/// may never surface as an error at all (see D-0016).
pub async fn platform_permissions() -> Json<PlatformPermissionsDto> {
    Json(current_platform_permissions())
}

#[cfg(feature = "platform")]
fn current_platform_permissions() -> PlatformPermissionsDto {
    PlatformPermissionsDto {
        input_monitoring: access_str(whisper_platform::input_monitoring_access()),
        accessibility: access_str(whisper_platform::accessibility_access()),
    }
}

#[cfg(feature = "platform")]
fn access_str(a: whisper_platform::Access) -> String {
    match a {
        whisper_platform::Access::Granted => "granted",
        whisper_platform::Access::Denied => "denied",
        whisper_platform::Access::Unknown => "unknown",
    }
    .to_string()
}

/// Headless build: no macOS adapter to query, so both fields are `"unknown"` rather than a
/// hard error — the endpoint still answers, it just cannot say more (`ARCHITECTURE.md` §5).
#[cfg(not(feature = "platform"))]
fn current_platform_permissions() -> PlatformPermissionsDto {
    PlatformPermissionsDto {
        input_monitoring: "unknown".to_string(),
        accessibility: "unknown".to_string(),
    }
}

/// One-shot transcription: blocks until the pipeline finishes and returns the final record
/// (`ARCHITECTURE.md` §4, the embeddable primitive).
pub async fn transcribe(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AudioRequest>,
) -> Result<Json<SessionRecord>, ApiErr> {
    ensure_stt_ready(&state)?;
    // One-shot transcribe is a headless primitive; a live mic session (with its start/stop
    // lifecycle) belongs on `POST /v1/sessions` (D-0012).
    if req.source.is_some() {
        return Err(ApiErr::bad_request(
            "mic source is only supported on POST /v1/sessions",
        ));
    }
    let pcm = decode_audio(&req)?;

    let session_id = state.new_session_id();
    if !state.try_begin(&session_id) {
        return Err(ApiErr::busy());
    }

    let token = CancellationToken::new();
    state.register_cancel(&session_id, token.clone());
    let (tx, rx) = oneshot::channel();
    state.register_waiter(&session_id, tx);
    let _ = state
        .store
        .upsert(&SessionRecord::running(session_id.clone(), now_ms()));

    let job = Job {
        session_id: session_id.clone(),
        audio: JobAudio::Pcm(pcm),
        cleanup_mode: req.cleanup_mode.map(Into::into),
        token,
        sink: sink_kind(&state),
    };
    if state.cmd_tx.send(job).await.is_err() {
        release(&state, &session_id);
        return Err(ApiErr::internal("engine worker unavailable"));
    }

    match rx.await {
        Ok(record) => Ok(Json(record)),
        Err(_) => Err(ApiErr::internal("session result was dropped")),
    }
}

/// Start an asynchronous session: returns an id immediately; events stream over `WS /v1/events`
/// and the authoritative result is retrievable via `GET /v1/sessions/{id}`.
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AudioRequest>,
) -> Result<(StatusCode, Json<SessionAccepted>), ApiErr> {
    ensure_stt_ready(&state)?;
    let is_mic = matches!(req.source, Some(SourceKind::Mic));
    if is_mic && state.live_source.is_none() {
        // No capture backend compiled in (headless build) — reject before admitting.
        return Err(ApiErr::bad_request(
            "mic source requires the daemon to be built with the 'platform' feature",
        ));
    }
    // Decode headless audio up front (before admission) so a bad request doesn't take the gate.
    let pcm = if is_mic {
        None
    } else {
        Some(decode_audio(&req)?)
    };

    let session_id = state.new_session_id();
    if !state.try_begin(&session_id) {
        return Err(ApiErr::busy());
    }

    let token = CancellationToken::new();
    state.register_cancel(&session_id, token.clone());
    let _ = state
        .store
        .upsert(&SessionRecord::running(session_id.clone(), now_ms()));

    let audio = if is_mic {
        // Live capture: register a graceful-stop control and build the source factory (which the
        // worker runs on its thread). `platform.permission`/`audio.device_lost` events originate
        // inside the factory/capture and reach WS directly (D-0012).
        let control = CaptureControl::new();
        state.register_capture(&session_id, control.clone());
        let provider = state
            .live_source
            .as_ref()
            .expect("live_source presence checked above");
        JobAudio::Live(provider(LiveSourceCtx {
            session_id: session_id.clone(),
            events: state.events.clone(),
            stop: control.flag(),
        }))
    } else {
        JobAudio::Pcm(pcm.expect("non-mic pcm decoded above"))
    };

    let job = Job {
        session_id: session_id.clone(),
        audio,
        cleanup_mode: req.cleanup_mode.map(Into::into),
        token,
        sink: sink_kind(&state),
    };
    if state.cmd_tx.send(job).await.is_err() {
        release(&state, &session_id);
        return Err(ApiErr::internal("engine worker unavailable"));
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(SessionAccepted {
            session_id,
            status: SessionStatusDto::Running,
        }),
    ))
}

pub async fn stop_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, ApiErr> {
    // A live mic session stops *gracefully*: end capture and let the pipeline finish STT/cleanup/
    // delivery (D-0012). A headless PCM session has no capture to stop, so `stop` == `cancel`.
    if let Some(control) = state.get_capture(&id) {
        control.stop();
        return current_record(&state, &id);
    }
    signal_cancel(&state, &id)
}

pub async fn cancel_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, ApiErr> {
    // For a live session, also end capture so the pipeline reaches the cancel checkpoint (it is
    // checked right after the capture loop; otherwise `next_frame` would block forever).
    if let Some(control) = state.get_capture(&id) {
        control.stop();
    }
    signal_cancel(&state, &id)
}

/// `cancel` (and `stop` on a headless session) set the cancellation token; the pipeline observes
/// it at the next stage boundary (`ARCHITECTURE.md` §3.2). Returns the current stored record.
fn signal_cancel(state: &AppState, id: &str) -> Result<Json<SessionRecord>, ApiErr> {
    if let Some(token) = state.get_cancel(id) {
        token.cancel();
    }
    current_record(state, id)
}

/// The current stored record for `id`, or 404.
fn current_record(state: &AppState, id: &str) -> Result<Json<SessionRecord>, ApiErr> {
    match state.store.get(id) {
        Some(record) => Ok(Json(record)),
        None => Err(ApiErr::not_found(format!("unknown session {id}"))),
    }
}

/// The delivery sink kind for a new session, from `config.delivery` (`ARCHITECTURE.md` §5,
/// D-0012). Without the `platform` feature the worker's sink builder maps `Paste` to `Null`.
fn sink_kind(state: &AppState) -> SinkKind {
    match state.config.read().expect("config lock").delivery {
        DeliveryMode::Paste => SinkKind::Paste,
        DeliveryMode::None => SinkKind::Null,
    }
}

pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, ApiErr> {
    match state.store.get(&id) {
        Some(record) => Ok(Json(record)),
        None => Err(ApiErr::not_found(format!("unknown session {id}"))),
    }
}

pub async fn get_config(State(state): State<Arc<AppState>>) -> Json<Config> {
    Json(state.config.read().expect("config lock").clone())
}

/// Replace the configuration. Runtime-safe fields (cleanup mode, language, token) take effect
/// immediately; infra fields (port, bind, model paths, history) are staged for the next restart
/// and reported as such (`ARCHITECTURE.md` §4).
pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(new): Json<Config>,
) -> Result<Json<ConfigUpdateResponse>, ApiErr> {
    validate_config(&new)?;
    let mut applied = Vec::new();
    let mut staged = Vec::new();
    {
        let mut cur = state.config.write().expect("config lock");
        if new.cleanup_mode != cur.cleanup_mode {
            applied.push("cleanup_mode".to_string());
        }
        if new.language != cur.language {
            *state.lang.write().expect("lang lock") = new.language.clone();
            applied.push("language".to_string());
        }
        if new.vad != cur.vad {
            // The VAD is rebuilt per session from the shared tuning, so an edit is applied to the
            // next session (no restart) — hence `applied`, not `staged` (D-0008).
            *state.vad.write().expect("vad lock") = new.vad.to_vad_config();
            applied.push("vad".to_string());
        }
        if new.cleanup.model != cur.cleanup.model {
            // The engine reads the shared tag per utterance, so a swap needs no restart. Health
            // described the old model; reset it so status does not report stale readiness.
            *state.cleanup_model.write().expect("cleanup model lock") = new.cleanup.model.clone();
            *state.cleanup_health.write().expect("cleanup health lock") = CleanupHealth::default();
            applied.push("cleanup.model".to_string());
        }
        if new.cleanup.ollama_url != cur.cleanup.ollama_url
            || new.cleanup.keep_alive != cur.cleanup.keep_alive
        {
            // The Ollama client and keep-alive are fixed when the engine is built.
            staged.push("cleanup".to_string());
        }
        if new.audio != cur.audio {
            // The capture stream (and its AGC) is built once at startup; a change needs a restart.
            staged.push("audio".to_string());
        }
        if new.token != cur.token {
            applied.push("token".to_string());
        }
        if new.delivery != cur.delivery {
            // Read per-session from config at admission time, so it takes effect next session.
            applied.push("delivery".to_string());
        }
        if new.hotkey != cur.hotkey {
            // The hotkey listener is started once at daemon startup; a change needs a restart.
            staged.push("hotkey".to_string());
        }
        if new.port != cur.port {
            staged.push("port".to_string());
        }
        if new.bind != cur.bind {
            staged.push("bind".to_string());
        }
        if new.models != cur.models {
            staged.push("models".to_string());
        }
        if new.history != cur.history {
            staged.push("history".to_string());
        }
        if new.dictionary_path != cur.dictionary_path {
            staged.push("dictionary_path".to_string());
        }
        *cur = new;
    }

    // Persist the full desired config so a restart applies the staged fields.
    let cfg = state.config.read().expect("config lock").clone();
    if let Some(path) = &state.config_path {
        let toml = cfg
            .to_toml_string()
            .map_err(|e| ApiErr::internal(format!("serialize config: {e}")))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ApiErr::internal(format!("create config dir {}: {e}", parent.display()))
                })?;
            }
        }
        std::fs::write(path, toml)
            .map_err(|e| ApiErr::internal(format!("write config {}: {e}", path.display())))?;
    }

    Ok(Json(ConfigUpdateResponse {
        applied,
        staged,
        config: cfg,
    }))
}

pub async fn get_dictionary(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.dictionary.read().expect("dict lock").json.clone())
}

/// Replace the user dictionary (D-0010 public contract `{"pairs":[{"from","to"}]}`). Validated
/// by the core parser before it is accepted, and applied to the next session.
pub async fn put_dictionary(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    Dictionary::from_json_str(&body.to_string())
        .map_err(|e| ApiErr::bad_request(format!("invalid dictionary: {e}")))?;

    let path = {
        let mut dict = state.dictionary.write().expect("dict lock");
        dict.json = body.clone();
        dict.path.clone()
    };
    if let Some(path) = path {
        let pretty = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ApiErr::internal(format!("create dictionary dir {}: {e}", parent.display()))
                })?;
            }
        }
        std::fs::write(&path, pretty)
            .map_err(|e| ApiErr::internal(format!("write dictionary {}: {e}", path.display())))?;
    }
    Ok(Json(body))
}

// ---------------------------------------------------------------------------
// Model provisioning (D-0015): GET /v1/models, POST/DELETE /v1/models/{id}/download
// ---------------------------------------------------------------------------

/// `GET /v1/models`: the compiled-in catalog plus each entry's live presence/download state
/// (Ollama presence from `/api/tags`), then installed non-catalog Ollama models (D-0017).
pub async fn list_models(State(state): State<Arc<AppState>>) -> Json<ModelsListResponse> {
    let (url, selected_tag) = {
        let cfg = state.config.read().expect("config lock");
        (cfg.cleanup.ollama_url.clone(), cfg.cleanup.model.clone())
    };
    // `tags()` is blocking I/O with short timeouts; Ollama down is not an error here — its
    // models are simply not present (D-0017).
    let installed = tokio::task::spawn_blocking(move || OllamaClient::new(url).tags())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let is_selected = |tag: &str| {
        !selected_tag.trim().is_empty() && normalize_tag(tag) == normalize_tag(&selected_tag)
    };

    let download_state = |id: &str| match state.model_downloads.get(id) {
        Some(handle) => {
            let p = handle.snapshot();
            (
                true,
                Some(ModelProgressDto {
                    downloaded: p.downloaded,
                    total: p.total,
                }),
            )
        }
        None => (false, None),
    };

    let mut models: Vec<ModelEntryDto> = models::catalog()
        .iter()
        .map(|m| {
            let (downloading, progress) = download_state(m.id);
            let file = m.file();
            let tag = m.ollama_tag();
            ModelEntryDto {
                id: m.id.to_string(),
                kind: m.kind.as_str().to_string(),
                source: if tag.is_some() { "ollama" } else { "file" }.to_string(),
                ollama_tag: tag.map(str::to_string),
                validated: m.validated,
                selected: tag.is_some_and(is_selected),
                filename: file.map(|f| f.filename.to_string()),
                required: m.required,
                size_bytes: m.size_bytes(),
                present: match tag {
                    Some(t) => installed
                        .iter()
                        .any(|i| normalize_tag(&i.name) == normalize_tag(t)),
                    None => models::is_present(m, &state.data_dir),
                },
                path: models::target_path(m, &state.data_dir).map(|p| p.display().to_string()),
                downloading,
                progress,
            }
        })
        .collect();

    // Installed Ollama models outside the catalog: selectable but not validated.
    for inst in &installed {
        let in_catalog = models::catalog()
            .iter()
            .filter_map(|m| m.ollama_tag())
            .any(|t| normalize_tag(t) == normalize_tag(&inst.name));
        if in_catalog || is_embedding_model(inst) {
            continue;
        }
        models.push(ModelEntryDto {
            id: format!("ollama:{}", inst.name),
            kind: models::ModelKind::Cleanup.as_str().to_string(),
            source: "ollama".to_string(),
            ollama_tag: Some(inst.name.clone()),
            validated: false,
            selected: is_selected(&inst.name),
            filename: None,
            required: false,
            size_bytes: inst.size,
            present: true,
            path: None,
            downloading: false,
            progress: None,
        });
    }
    Json(ModelsListResponse { models })
}

/// Embedding-only models cannot do chat cleanup, so they are not offered as extras.
fn is_embedding_model(m: &InstalledModel) -> bool {
    m.name.to_ascii_lowercase().contains("embed")
        || (!m.families.is_empty()
            && m.families.iter().all(|f| {
                let f = f.to_ascii_lowercase();
                f.contains("embed") || f.contains("bert")
            }))
}

/// `POST /v1/models/{id}/download`: start a download (file) or pull (Ollama) in the background
/// (`202 Accepted`); `409` if this model is already downloading or is an already-installed
/// `ollama:<tag>` extra, `404` for an unknown id.
pub async fn start_model_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiErr> {
    if id.starts_with("ollama:") {
        return Err(ApiErr::new(
            StatusCode::CONFLICT,
            "already_present",
            format!("model {id} is already installed in Ollama"),
        ));
    }
    let model =
        models::find(&id).ok_or_else(|| ApiErr::not_found(format!("unknown model {id}")))?;
    let handle = state
        .model_downloads
        .begin(model.id)
        .ok_or_else(|| ApiErr::download_busy(&id))?;

    let data_dir = state.data_dir.clone();
    let ollama_url = state
        .config
        .read()
        .expect("config lock")
        .cleanup
        .ollama_url
        .clone();
    let events = state.events.clone();
    let registry = state.model_downloads.clone();
    let model_id = model.id.to_string();

    tokio::spawn(async move {
        let progress_events = events.clone();
        let progress_id = model_id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let on_progress = move |p: DownloadProgress| {
                let _ = progress_events.send(model_download_event(&progress_id, &p));
            };
            if model.ollama_tag().is_some() {
                models::run_pull(model, &OllamaClient::new(ollama_url), &handle, on_progress);
            } else {
                models::run_download(model, &data_dir, &handle, on_progress);
            }
        })
        .await;
        // Terminal (done/error/cancel) — free the slot so a retry can begin fresh.
        registry.end(&model_id);
    });

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /v1/models/{id}/download`: cancel an in-flight download or pull (file: aborts and
/// deletes `.part`; Ollama: drops the pull stream, Ollama keeps its partial blobs so a retry
/// resumes); `404` for an unknown model id, no-op (`204`) if nothing is in flight. There is
/// deliberately no removal path for Ollama models (D-0017).
pub async fn cancel_model_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiErr> {
    if models::find(&id).is_none() {
        return Err(ApiErr::not_found(format!("unknown model {id}")));
    }
    state.model_downloads.cancel(&id);
    Ok(StatusCode::NO_CONTENT)
}

fn model_download_event(id: &str, p: &DownloadProgress) -> ApiEvent {
    ApiEvent::ModelDownload {
        id: id.to_string(),
        downloaded: p.downloaded,
        total: p.total,
        phase: p.phase,
        error: p.error.clone(),
        ts_ms: now_ms(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Validate the tunable ranges of an incoming config before it is applied (`PUT /v1/config`).
/// Out-of-range values are a structured `400`, so a bad edit never destabilises the VAD/capture.
fn validate_config(cfg: &Config) -> Result<(), ApiErr> {
    let v = &cfg.vad;
    if !(0.05..=0.95).contains(&v.threshold) {
        return Err(ApiErr::bad_request(format!(
            "vad.threshold {} out of range [0.05, 0.95]",
            v.threshold
        )));
    }
    if !(50..=10_000).contains(&v.max_silence_ms) {
        return Err(ApiErr::bad_request(format!(
            "vad.max_silence_ms {} out of range [50, 10000]",
            v.max_silence_ms
        )));
    }
    if v.hangover_ms > v.max_silence_ms {
        return Err(ApiErr::bad_request(format!(
            "vad.hangover_ms {} must be <= vad.max_silence_ms {}",
            v.hangover_ms, v.max_silence_ms
        )));
    }
    let a = &cfg.audio;
    if !(0.1..=1.0).contains(&a.agc_target_peak) {
        return Err(ApiErr::bad_request(format!(
            "audio.agc_target_peak {} out of range [0.1, 1.0]",
            a.agc_target_peak
        )));
    }
    if !(1.0..=64.0).contains(&a.agc_max_gain) {
        return Err(ApiErr::bad_request(format!(
            "audio.agc_max_gain {} out of range [1.0, 64.0]",
            a.agc_max_gain
        )));
    }
    Ok(())
}

fn ensure_stt_ready(state: &AppState) -> Result<(), ApiErr> {
    match state.status.read().expect("status lock").phase {
        EnginePhase::Ready | EnginePhase::Degraded => Ok(()),
        EnginePhase::Loading => Err(ApiErr::unavailable("models are still loading")),
        EnginePhase::Unavailable => Err(ApiErr::unavailable("stt model failed to load")),
    }
}

/// Undo an admission whose job could not be dispatched.
fn release(state: &AppState, session_id: &str) {
    state.take_waiter(session_id);
    state.take_cancel(session_id);
    state.take_capture(session_id);
    state.end_active();
}

/// Decode the request's audio into 16 kHz mono f32 PCM. Exactly one of `audio_b64` (base64 WAV)
/// or `path` (local WAV file) must be present.
fn decode_audio(req: &AudioRequest) -> Result<Vec<f32>, ApiErr> {
    match (&req.audio_b64, &req.path) {
        (Some(_), Some(_)) => Err(ApiErr::bad_request(
            "provide exactly one of audio_b64 or path, not both",
        )),
        (Some(b64), None) => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| ApiErr::bad_request(format!("audio_b64 is not valid base64: {e}")))?;
            wav_bytes_to_mono_f32(&bytes)
                .map_err(|e| ApiErr::bad_request(format!("audio_b64 is not a valid WAV: {e}")))
        }
        (None, Some(path)) => crate::stt::read_wav_mono_f32(std::path::Path::new(path))
            .map_err(|e| ApiErr::bad_request(format!("read wav {path}: {e}"))),
        (None, None) => Err(ApiErr::bad_request("missing audio_b64 or path")),
    }
}

/// Decode in-memory WAV bytes to 16 kHz mono f32, downmixing multi-channel input (mirrors
/// [`crate::stt::read_wav_mono_f32`] for the base64 path).
fn wav_bytes_to_mono_f32(bytes: &[u8]) -> Result<Vec<f32>, hound::Error> {
    let reader = hound::WavReader::new(Cursor::new(bytes))?;
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

// ---------------------------------------------------------------------------
// Local Ollama (D-0017): GET /v1/ollama, POST /v1/ollama/start
// ---------------------------------------------------------------------------

/// Probe the configured Ollama URL (blocking: a 2 s version request plus filesystem checks).
fn ollama_status_blocking(url: &str) -> OllamaStatusDto {
    let probe = ollama_probe::probe(&OllamaClient::new(url));
    OllamaStatusDto {
        can_start: probe.installed && !probe.running && is_loopback_url(url),
        installed: probe.installed,
        running: probe.running,
        version: probe.version,
        url: url.to_string(),
    }
}

fn ollama_url(state: &AppState) -> String {
    state
        .config
        .read()
        .expect("config lock")
        .cleanup
        .ollama_url
        .clone()
}

/// `GET /v1/ollama`: installed / running / version at `cleanup.ollama_url`, plus `can_start`.
pub async fn get_ollama(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OllamaStatusDto>, ApiErr> {
    let url = ollama_url(&state);
    let dto = tokio::task::spawn_blocking(move || ollama_status_blocking(&url))
        .await
        .map_err(|e| ApiErr::internal(format!("ollama probe: {e}")))?;
    Ok(Json(dto))
}

/// `POST /v1/ollama/start`: launch a local Ollama (`202`, started in the background; the result
/// arrives as an `ollama.status` WS event) or `409` when it cannot/need not be started.
pub async fn start_ollama(State(state): State<Arc<AppState>>) -> Result<StatusCode, ApiErr> {
    let url = ollama_url(&state);
    let probe_url = url.clone();
    let dto = tokio::task::spawn_blocking(move || ollama_status_blocking(&probe_url))
        .await
        .map_err(|e| ApiErr::internal(format!("ollama probe: {e}")))?;
    if !dto.can_start {
        return Err(ApiErr::new(
            StatusCode::CONFLICT,
            "ollama_not_startable",
            "ollama is not installed locally, is already running, or the configured url is not loopback",
        ));
    }

    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let client = OllamaClient::new(url.as_str());
            let path_env = std::env::var("PATH").ok();
            let outcome = match ollama_probe::find_installation(path_env.as_deref(), |p| p.exists())
            {
                Some(inst) => ollama_probe::start(&inst, &client),
                None => Err("ollama is not installed".to_string()),
            };
            if let Err(e) = outcome {
                eprintln!("whisperd: ollama start failed: {e}");
            }
            ollama_status_blocking(&url)
        })
        .await;
        if let Ok(dto) = result {
            let _ = state.events.send(ApiEvent::OllamaStatus {
                installed: dto.installed,
                running: dto.running,
                version: dto.version,
                url: dto.url,
                can_start: dto.can_start,
                ts_ms: now_ms(),
            });
        }
    });

    Ok(StatusCode::ACCEPTED)
}
