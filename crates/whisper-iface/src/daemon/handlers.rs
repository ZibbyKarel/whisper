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

use super::api::{
    AudioRequest, ConfigUpdateResponse, ErrorBody, ErrorEnvelope, HealthResponse, ModelStatus,
    SessionAccepted, SessionRecord, SessionStatusDto, StatusResponse,
};
use super::config::Config;
use super::state::{now_ms, AppState, EnginePhase, Job};

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
    Json(StatusResponse {
        phase: s.phase.as_str().to_string(),
        version: state.version.clone(),
        uptime_ms: state.started.elapsed().as_millis(),
        active_session: state.active_session(),
        stt: ModelStatus {
            ready: s.stt_ready,
            model: s.stt_model,
            error: s.stt_error,
        },
        cleanup: ModelStatus {
            ready: s.cleanup_ready,
            model: s.cleanup_model,
            error: s.cleanup_error,
        },
        cleanup_mode: cfg.cleanup_mode,
        language: cfg.language,
        history_enabled: state.store.history_enabled(),
    })
}

/// One-shot transcription: blocks until the pipeline finishes and returns the final record
/// (`ARCHITECTURE.md` §4, the embeddable primitive).
pub async fn transcribe(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AudioRequest>,
) -> Result<Json<SessionRecord>, ApiErr> {
    ensure_stt_ready(&state)?;
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
        pcm,
        cleanup_mode: req.cleanup_mode.map(Into::into),
        token,
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
    let pcm = decode_audio(&req)?;

    let session_id = state.new_session_id();
    if !state.try_begin(&session_id) {
        return Err(ApiErr::busy());
    }

    let token = CancellationToken::new();
    state.register_cancel(&session_id, token.clone());
    let _ = state
        .store
        .upsert(&SessionRecord::running(session_id.clone(), now_ms()));

    let job = Job {
        session_id: session_id.clone(),
        pcm,
        cleanup_mode: req.cleanup_mode.map(Into::into),
        token,
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
    signal_cancel(&state, &id)
}

pub async fn cancel_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, ApiErr> {
    signal_cancel(&state, &id)
}

/// Both `stop` and `cancel` set the session's cancellation token; the pipeline observes it at
/// the next stage boundary (`ARCHITECTURE.md` §3.2). Returns the current stored record.
fn signal_cancel(state: &AppState, id: &str) -> Result<Json<SessionRecord>, ApiErr> {
    if let Some(token) = state.get_cancel(id) {
        token.cancel();
    }
    match state.store.get(id) {
        Some(record) => Ok(Json(record)),
        None => Err(ApiErr::not_found(format!("unknown session {id}"))),
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
        if new.token != cur.token {
            applied.push("token".to_string());
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
        std::fs::write(&path, pretty)
            .map_err(|e| ApiErr::internal(format!("write dictionary {}: {e}", path.display())))?;
    }
    Ok(Json(body))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
