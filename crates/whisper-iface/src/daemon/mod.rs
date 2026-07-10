//! `whisperd` — the localhost HTTP (control) + WebSocket (events) daemon over `whisper-core`
//! (`ARCHITECTURE.md` §4, D-0009/D-0011).
//!
//! Shape (D-0011 actor): the `!Send` engines and the `Pipeline` live on one worker thread
//! ([`worker`]); the async axum handlers ([`handlers`]) speak to it through a bounded command
//! channel and observe results via a one-shot reply (blocking `/v1/transcribe`), the session
//! store ([`store`]), and a tokio broadcast fanned out to `WS /v1/events`. Everything shared is
//! behind [`state::AppState`].

pub mod api;
pub mod config;
pub mod handlers;
#[cfg(feature = "platform")]
pub mod platform;
pub mod state;
pub mod store;
pub mod worker;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::{broadcast, mpsc, oneshot};

use whisper_core::ports::{CleanupEngine, Vad};
use whisper_core::vad::VadConfig;

use self::config::Config;
use self::handlers::ApiErr;
use self::state::LiveSourceProvider;
use self::state::{AppState, DictionaryState};
use self::store::SessionStore;
use self::worker::{null_sink_builder, BuildCtx, EngineBuilder, LangInjectingStt, ResidentEngines};
use crate::{LlamaCleanupEngine, SileroVad, WhisperCppEngine};

/// Broadcast backlog per WS subscriber before it starts lagging (best-effort, `ARCHITECTURE.md`
/// §4: a lagging client may drop events; the authoritative result is `GET /v1/sessions/{id}`).
const EVENT_CHANNEL_CAP: usize = 512;

/// Build the shared state and spawn the engine worker thread. Public so integration tests can
/// inject fake engines through `builder` and drive the router without real models.
pub fn build_state(
    config: Config,
    config_path: Option<PathBuf>,
    builder: EngineBuilder,
    live_source: Option<LiveSourceProvider>,
) -> Result<Arc<AppState>, String> {
    let (events, _rx) = broadcast::channel(EVENT_CHANNEL_CAP);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);

    let dictionary = load_dictionary(&config)?;
    let db_path = if config.history.enabled {
        Some(resolve_history_path(&config))
    } else {
        None
    };
    let store = SessionStore::new(db_path)?;
    let lang = Arc::new(RwLock::new(config.language.clone()));

    let state = Arc::new(AppState::new(
        env!("CARGO_PKG_VERSION").to_string(),
        events,
        cmd_tx,
        config,
        config_path,
        dictionary,
        lang,
        store,
        live_source,
    ));

    // The worker holds only a Weak ref, so dropping the daemon (all strong Arcs) closes the
    // command channel and lets the worker thread exit cleanly.
    worker::spawn_weak(Arc::downgrade(&state), cmd_rx, builder);
    Ok(state)
}

/// Assemble the `/v1` router, with token auth (when configured) layered over every route except
/// `/v1/health`.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/health", get(handlers::health))
        .route("/v1/status", get(handlers::status))
        .route("/v1/transcribe", post(handlers::transcribe))
        .route("/v1/sessions", post(handlers::create_session))
        .route("/v1/sessions/{id}", get(handlers::get_session))
        .route("/v1/sessions/{id}/stop", post(handlers::stop_session))
        .route("/v1/sessions/{id}/cancel", post(handlers::cancel_session))
        .route(
            "/v1/config",
            get(handlers::get_config).put(handlers::put_config),
        )
        .route(
            "/v1/dictionary",
            get(handlers::get_dictionary).put(handlers::put_dictionary),
        )
        .route("/v1/events", get(ws_events))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

/// Bearer-token gate (`ARCHITECTURE.md` §4, D-0009: optional token). No token configured ⇒
/// open. `/v1/health` is always reachable (liveness probes). WS clients may pass `?token=`.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let token = state
        .config
        .read()
        .expect("config lock")
        .token
        .clone()
        .filter(|t| !t.is_empty());
    if let Some(token) = token {
        if req.uri().path() != "/v1/health" && !authorized(&req, &token) {
            return ApiErr::unauthorized().into_response();
        }
    }
    next.run(req).await
}

fn authorized(req: &axum::extract::Request, token: &str) -> bool {
    if let Some(h) = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if h == format!("Bearer {token}") {
            return true;
        }
    }
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("token=") {
                if v == token {
                    return true;
                }
            }
        }
    }
    false
}

/// `WS /v1/events`: subscribe to the broadcast and stream every event as JSON text. Best-effort:
/// on lag we skip the missed events and keep going (`ARCHITECTURE.md` §4).
async fn ws_events(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(event) => {
                    let text = match serde_json::to_string(&event) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // Observe client-side close/ping so the task ends when the client disconnects.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) => break,
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// A running daemon: a bound socket serving the router with graceful shutdown.
pub struct Daemon {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    shutdown: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Daemon {
    /// Bind `config.bind:config.port` (use port 0 for an ephemeral test port) and start serving.
    pub async fn start(
        config: Config,
        config_path: Option<PathBuf>,
        builder: EngineBuilder,
        live_source: Option<LiveSourceProvider>,
    ) -> Result<Daemon, String> {
        let bind = format!("{}:{}", config.bind, config.port);
        let state = build_state(config, config_path, builder, live_source)?;
        let router = build_router(state.clone());

        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .map_err(|e| format!("bind {bind}: {e}"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        Ok(Daemon {
            addr,
            state,
            shutdown: Some(shutdown_tx),
            server: Some(server),
        })
    }

    /// Signal graceful shutdown and wait for the server task to finish.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(server) = self.server.take() {
            let _ = server.await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Best-effort shutdown if the caller dropped the handle without awaiting `shutdown`.
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

/// Load the dictionary JSON from `config.dictionary_path` if set, else start empty. Validates the
/// file so a broken dictionary fails startup rather than silently disabling substitutions.
fn load_dictionary(config: &Config) -> Result<DictionaryState, String> {
    match &config.dictionary_path {
        Some(path) if path.exists() => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("read dictionary {}: {e}", path.display()))?;
            let json: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| format!("parse dictionary {}: {e}", path.display()))?;
            whisper_core::dictionary::Dictionary::from_json_str(&json.to_string())
                .map_err(|e| format!("invalid dictionary {}: {e}", path.display()))?;
            Ok(DictionaryState {
                json,
                path: Some(path.clone()),
            })
        }
        Some(path) => Ok(DictionaryState {
            json: serde_json::json!({ "pairs": [] }),
            path: Some(path.clone()),
        }),
        None => Ok(DictionaryState::empty()),
    }
}

/// Resolve the SQLite history path: explicit config path, else the XDG data dir
/// (`~/.local/share/whisper-engine/history.db` on Linux, `~/Library/Application Support/...` on
/// macOS) (`ARCHITECTURE.md` §6, D-0010).
fn resolve_history_path(config: &Config) -> PathBuf {
    if let Some(path) = &config.history.path {
        return path.clone();
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("whisper-engine").join("history.db")
}

/// Factory for the production engine set (`ARCHITECTURE.md` §3.1: heavy native adapters live in
/// this crate). Loads whisper.cpp (D-0004) and, if configured, llama.cpp cleanup (D-0005); the
/// STT is wrapped so `/v1/config` `language` reaches decoding. A cleanup load failure degrades to
/// cleanup-off rather than failing the daemon (`ARCHITECTURE.md` §3.2).
pub fn production_engine_builder(
    stt_path: PathBuf,
    cleanup_path: Option<PathBuf>,
    vad_config: VadConfig,
) -> EngineBuilder {
    // Headless default: delivery is "available via the API".
    production_engine_builder_with_sink(stt_path, cleanup_path, vad_config, null_sink_builder())
}

/// Like [`production_engine_builder`] but with an explicit delivery [`SinkBuilder`](worker::SinkBuilder)
/// — the seam the `platform` feature uses to install the pasteboard sink (D-0012).
pub fn production_engine_builder_with_sink(
    stt_path: PathBuf,
    cleanup_path: Option<PathBuf>,
    vad_config: VadConfig,
    sink_builder: worker::SinkBuilder,
) -> EngineBuilder {
    let stt_label = file_label(&stt_path);
    Box::new(move |ctx: BuildCtx| {
        let stt = WhisperCppEngine::load(&stt_path).map_err(|e| e.to_string())?;
        let lang_stt = LangInjectingStt::new(Box::new(stt), ctx.lang.clone());

        let (cleanup, cleanup_model_label, cleanup_load_error) = match &cleanup_path {
            Some(path) => {
                let label = file_label(path);
                match LlamaCleanupEngine::load(path) {
                    Ok(engine) => (
                        Some(Rc::new(engine) as Rc<dyn CleanupEngine>),
                        Some(label),
                        None,
                    ),
                    Err(err) => (None, Some(label), Some(err.to_string())),
                }
            }
            None => (None, None, None),
        };

        let vad_builder = Box::new(move || {
            SileroVad::new(vad_config)
                .map(|v| Box::new(v) as Box<dyn Vad>)
                .map_err(|e| e.to_string())
        });

        Ok(ResidentEngines {
            stt: Rc::new(lang_stt),
            stt_model_label: stt_label.clone(),
            cleanup,
            cleanup_model_label,
            cleanup_load_error,
            vad_builder,
            sink_builder,
        })
    })
}

fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}
