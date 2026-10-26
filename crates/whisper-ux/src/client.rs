//! HTTP client of the whisperd `/v1` control API (D-0013: the shell talks ONLY
//! to the Interface Layer). Behind an [`Api`] trait so the GUI can be driven by a
//! fake in tests and the real [`HttpClient`] (ureq) is exercised against a
//! std-`TcpListener` mock — no daemon, no engines.

use std::time::Duration;

use ureq::Agent;

use crate::api::{
    Config, ConfigUpdateResponse, ErrorEnvelope, HealthResponse, ModelsListResponse, OllamaStatus,
    PlatformPermissions, SessionAccepted, SessionRecord, StatusResponse,
};

/// A failed `/v1` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// Could not reach the daemon (connection refused, timeout, ...).
    Transport(String),
    /// The daemon answered non-2xx; `code`/`message` come from the structured
    /// error envelope when present.
    Http {
        status: u16,
        code: String,
        message: String,
    },
    /// The response body was not the JSON we expected.
    Decode(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Transport(m) => write!(f, "cannot reach daemon: {m}"),
            ApiError::Http {
                status,
                code,
                message,
            } => write!(f, "HTTP {status} [{code}]: {message}"),
            ApiError::Decode(m) => write!(f, "bad response: {m}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// The control operations the UX shell needs. Trait seam for testing/faking.
pub trait Api {
    fn health(&self) -> Result<HealthResponse, ApiError>;
    fn status(&self) -> Result<StatusResponse, ApiError>;
    fn get_config(&self) -> Result<Config, ApiError>;
    fn put_config(&self, cfg: &Config) -> Result<ConfigUpdateResponse, ApiError>;
    fn get_dictionary(&self) -> Result<serde_json::Value, ApiError>;
    fn put_dictionary(&self, body: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    /// `POST /v1/sessions {"source":"mic"}` — start a live dictation session.
    fn start_mic_session(&self) -> Result<SessionAccepted, ApiError>;
    /// `POST /v1/sessions/{id}/stop` — graceful stop (D-0012).
    fn stop_session(&self, id: &str) -> Result<SessionRecord, ApiError>;
    /// `GET /v1/models` — the model catalog plus each entry's presence/download state (D-0015).
    fn list_models(&self) -> Result<ModelsListResponse, ApiError>;
    /// `POST /v1/models/{id}/download` — start (or resume-from-scratch) a model download.
    fn start_model_download(&self, id: &str) -> Result<(), ApiError>;
    /// `DELETE /v1/models/{id}/download` — cancel an in-flight download.
    fn cancel_model_download(&self, id: &str) -> Result<(), ApiError>;
    /// `GET /v1/platform/permissions` — current macOS TCC grant status for the hotkey/paste
    /// path (D-0016).
    fn platform_permissions(&self) -> Result<PlatformPermissions, ApiError>;
    /// `GET /v1/ollama` — is a local Ollama installed/running, and may the daemon start it
    /// (D-0017).
    fn ollama_status(&self) -> Result<OllamaStatus, ApiError>;
    /// `POST /v1/ollama/start` — ask the daemon to start the local Ollama (`202`; `409
    /// ollama_not_startable` when it cannot).
    fn start_ollama(&self) -> Result<(), ApiError>;
}

/// ureq-backed [`Api`] implementation.
pub struct HttpClient {
    base: String,
    token: Option<String>,
    agent: Agent,
}

impl HttpClient {
    /// `base` is e.g. `http://127.0.0.1:8737` (no trailing slash).
    #[must_use]
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        // Surface 4xx/5xx as normal responses so the error envelope is read, not
        // swallowed (mirrors whisper-cli).
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            agent,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// GET `path`, returning the decoded JSON value or a structured error.
    fn get_value(&self, path: &str) -> Result<serde_json::Value, ApiError> {
        let mut req = self.agent.get(self.url(path));
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let res = req.call().map_err(|e| ApiError::Transport(e.to_string()))?;
        read_checked(res)
    }

    /// POST/PUT `body` to `path`.
    fn send_value(
        &self,
        method: Method,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let mut req = match method {
            Method::Post => self.agent.post(self.url(path)),
            Method::Put => self.agent.put(self.url(path)),
        };
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let res = req
            .send_json(body)
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        read_checked(res)
    }
}

enum Method {
    Post,
    Put,
}

impl HttpClient {
    /// POST/DELETE `path` with no body and only care about the status (`202`/`204` responses
    /// carry no JSON body — model-download start/cancel, D-0015). A non-2xx status is decoded
    /// as the structured error envelope when present, mirroring [`read_checked`].
    fn call_status_only(&self, delete: bool, path: &str) -> Result<(), ApiError> {
        let mut res = if delete {
            let mut req = self.agent.delete(self.url(path));
            if let Some(t) = &self.token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            req.call()
        } else {
            let mut req = self.agent.post(self.url(path));
            if let Some(t) = &self.token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            req.send_empty()
        }
        .map_err(|e| ApiError::Transport(e.to_string()))?;

        let status = res.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let value: serde_json::Value = res
            .body_mut()
            .read_json()
            .unwrap_or(serde_json::Value::Null);
        let (code, message) = match serde_json::from_value::<ErrorEnvelope>(value) {
            Ok(env) => (env.error.code, env.error.message),
            Err(_) => ("unknown".to_string(), "request failed".to_string()),
        };
        Err(ApiError::Http {
            status,
            code,
            message,
        })
    }
}

/// Read the body as JSON and convert a non-2xx status into [`ApiError::Http`]
/// carrying the daemon's structured `error.code`/`error.message` when present.
fn read_checked(mut res: ureq::http::Response<ureq::Body>) -> Result<serde_json::Value, ApiError> {
    let status = res.status().as_u16();
    let value: serde_json::Value = res
        .body_mut()
        .read_json()
        .map_err(|e| ApiError::Decode(e.to_string()))?;
    if (200..300).contains(&status) {
        return Ok(value);
    }
    // Best-effort structured envelope; fall back to a generic message.
    let (code, message) = match serde_json::from_value::<ErrorEnvelope>(value.clone()) {
        Ok(env) => (env.error.code, env.error.message),
        Err(_) => ("unknown".to_string(), "request failed".to_string()),
    };
    Err(ApiError::Http {
        status,
        code,
        message,
    })
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|e| ApiError::Decode(e.to_string()))
}

impl Api for HttpClient {
    fn health(&self) -> Result<HealthResponse, ApiError> {
        decode(self.get_value("/v1/health")?)
    }

    fn status(&self) -> Result<StatusResponse, ApiError> {
        decode(self.get_value("/v1/status")?)
    }

    fn get_config(&self) -> Result<Config, ApiError> {
        decode(self.get_value("/v1/config")?)
    }

    fn put_config(&self, cfg: &Config) -> Result<ConfigUpdateResponse, ApiError> {
        let body = serde_json::to_value(cfg).map_err(|e| ApiError::Decode(e.to_string()))?;
        decode(self.send_value(Method::Put, "/v1/config", &body)?)
    }

    fn get_dictionary(&self) -> Result<serde_json::Value, ApiError> {
        self.get_value("/v1/dictionary")
    }

    fn put_dictionary(&self, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.send_value(Method::Put, "/v1/dictionary", body)
    }

    fn start_mic_session(&self) -> Result<SessionAccepted, ApiError> {
        let body = serde_json::json!({ "source": "mic" });
        decode(self.send_value(Method::Post, "/v1/sessions", &body)?)
    }

    fn stop_session(&self, id: &str) -> Result<SessionRecord, ApiError> {
        let path = format!("/v1/sessions/{id}/stop");
        decode(self.send_value(Method::Post, &path, &serde_json::Value::Null)?)
    }

    fn list_models(&self) -> Result<ModelsListResponse, ApiError> {
        decode(self.get_value("/v1/models")?)
    }

    fn start_model_download(&self, id: &str) -> Result<(), ApiError> {
        self.call_status_only(false, &format!("/v1/models/{id}/download"))
    }

    fn cancel_model_download(&self, id: &str) -> Result<(), ApiError> {
        self.call_status_only(true, &format!("/v1/models/{id}/download"))
    }

    fn platform_permissions(&self) -> Result<PlatformPermissions, ApiError> {
        decode(self.get_value("/v1/platform/permissions")?)
    }

    fn ollama_status(&self) -> Result<OllamaStatus, ApiError> {
        decode(self.get_value("/v1/ollama")?)
    }

    fn start_ollama(&self) -> Result<(), ApiError> {
        self.call_status_only(false, "/v1/ollama/start")
    }
}

/// Build the `ws(s)://.../v1/events` URL from an HTTP base URL and optional token.
#[must_use]
pub fn events_ws_url(base: &str, token: Option<&str>) -> String {
    let base = base
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut url = format!("{base}/v1/events");
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        url.push_str(&format!("?token={t}"));
    }
    url
}

/// Reconnect backoff for the WS listener (D-0013 §2: reconnect with backoff).
/// Exponential from 500 ms, capped at 10 s.
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    let base_ms: u64 = 500;
    let cap_ms: u64 = 10_000;
    let ms = base_ms.saturating_mul(1u64 << attempt.min(6)).min(cap_ms);
    Duration::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// A one-shot HTTP mock: binds an ephemeral port, serves exactly one request
    /// with the given status/body, and returns the captured request head.
    fn mock_once(status_line: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let body = body.to_string();
        let status_line = status_line.to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = tx.send(head);
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (base, rx)
    }

    #[test]
    fn get_status_parses_body() {
        let (base, _rx) = mock_once(
            "200 OK",
            r#"{"phase":"ready","version":"0.1.0","stt":{"ready":true},"cleanup":{"ready":true},"cleanup_mode":"auto","language":"cs"}"#,
        );
        let client = HttpClient::new(base, None);
        let st = client.status().unwrap();
        assert_eq!(st.phase, "ready");
        assert_eq!(st.language, "cs");
        assert!(st.stt.ready);
    }

    #[test]
    fn get_config_parses_and_keeps_czech() {
        let (base, _rx) = mock_once(
            "200 OK",
            r#"{"bind":"127.0.0.1","port":8737,"cleanup_mode":"auto","language":"cs","delivery":"none","models":{"stt":"m.bin","cleanup":null},"history":{"enabled":false},"hotkey":{"enabled":true,"combo":"Ctrl+Alt","mode":"push_to_talk"},"vad":{"threshold":0.35,"hangover_ms":300,"max_silence_ms":1000},"audio":{"agc_enabled":true,"agc_target_peak":0.7,"agc_max_gain":12.0}}"#,
        );
        let client = HttpClient::new(base, None);
        let cfg = client.get_config().unwrap();
        assert_eq!(cfg.language, "cs");
        assert_eq!(cfg.port, 8737);
        assert_eq!(cfg.hotkey.combo, "Ctrl+Alt");
        assert_eq!(cfg.vad.threshold, 0.35);
    }

    #[test]
    fn structured_error_is_surfaced() {
        let (base, _rx) = mock_once(
            "409 Conflict",
            r#"{"error":{"code":"busy","message":"a session is already active"}}"#,
        );
        let client = HttpClient::new(base, None);
        let err = client.start_mic_session().unwrap_err();
        match err {
            ApiError::Http {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 409);
                assert_eq!(code, "busy");
                assert!(message.contains("already active"));
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[test]
    fn put_config_sends_bearer_token_and_body() {
        let (base, rx) = mock_once(
            "200 OK",
            r#"{"applied":["language"],"staged":[],"config":{"bind":"127.0.0.1","port":8737,"cleanup_mode":"auto","language":"en","delivery":"none","models":{"stt":"m.bin","cleanup":null},"history":{"enabled":false},"hotkey":{"enabled":true,"combo":"Ctrl+Alt","mode":"push_to_talk"},"vad":{"threshold":0.35,"hangover_ms":300,"max_silence_ms":1000},"audio":{"agc_enabled":true,"agc_target_peak":0.7,"agc_max_gain":12.0}}}"#,
        );
        let client = HttpClient::new(base, Some("secret".to_string()));
        let cfg = Config {
            language: "en".to_string(),
            ..Config::default()
        };
        let resp = client.put_config(&cfg).unwrap();
        assert_eq!(resp.applied, vec!["language".to_string()]);

        let head = rx.recv().unwrap();
        assert!(head.starts_with("PUT /v1/config"), "head was: {head}");
        // HTTP header names are case-insensitive; ureq lowercases them.
        assert!(
            head.to_lowercase().contains("authorization: bearer secret"),
            "head was: {head}"
        );
    }

    #[test]
    fn list_models_parses_body() {
        let (base, _rx) = mock_once(
            "200 OK",
            r#"{"models":[{"id":"stt-large-v3-turbo","kind":"stt","filename":"ggml-large-v3-turbo-q5_0.bin","required":true,"size_bytes":574041195,"present":false,"path":"/x/ggml-large-v3-turbo-q5_0.bin","downloading":false,"progress":null}]}"#,
        );
        let client = HttpClient::new(base, None);
        let resp = client.list_models().unwrap();
        assert_eq!(resp.models.len(), 1);
        assert_eq!(resp.models[0].id, "stt-large-v3-turbo");
        assert!(resp.models[0].required);
        assert!(!resp.models[0].present);
    }

    #[test]
    fn start_model_download_posts_with_no_body_and_no_decode_error_on_202() {
        let (base, rx) = mock_once("202 Accepted", "");
        let client = HttpClient::new(base, None);
        client.start_model_download("stt-large-v3-turbo").unwrap();
        let head = rx.recv().unwrap();
        assert!(
            head.starts_with("POST /v1/models/stt-large-v3-turbo/download"),
            "head was: {head}"
        );
    }

    #[test]
    fn cancel_model_download_sends_delete_and_tolerates_204_empty_body() {
        let (base, rx) = mock_once("204 No Content", "");
        let client = HttpClient::new(base, None);
        client.cancel_model_download("stt-large-v3-turbo").unwrap();
        let head = rx.recv().unwrap();
        assert!(
            head.starts_with("DELETE /v1/models/stt-large-v3-turbo/download"),
            "head was: {head}"
        );
    }

    #[test]
    fn start_model_download_surfaces_structured_409() {
        let (base, _rx) = mock_once(
            "409 Conflict",
            r#"{"error":{"code":"download_busy","message":"model stt-large-v3-turbo is already downloading"}}"#,
        );
        let client = HttpClient::new(base, None);
        let err = client
            .start_model_download("stt-large-v3-turbo")
            .unwrap_err();
        match err {
            ApiError::Http { status, code, .. } => {
                assert_eq!(status, 409);
                assert_eq!(code, "download_busy");
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[test]
    fn platform_permissions_parses_body() {
        let (base, _rx) = mock_once(
            "200 OK",
            r#"{"input_monitoring":"denied","accessibility":"granted"}"#,
        );
        let client = HttpClient::new(base, None);
        let p = client.platform_permissions().unwrap();
        assert_eq!(p.input_monitoring, "denied");
        assert_eq!(p.accessibility, "granted");
    }

    #[test]
    fn ollama_status_parses_body_and_hits_the_route() {
        let (base, rx) = mock_once(
            "200 OK",
            r#"{"installed":true,"running":false,"version":null,"url":"http://127.0.0.1:11434","can_start":true}"#,
        );
        let client = HttpClient::new(base, None);
        let st = client.ollama_status().unwrap();
        assert!(st.installed && !st.running && st.can_start);
        assert!(rx.recv().unwrap().starts_with("GET /v1/ollama "));
    }

    #[test]
    fn start_ollama_accepts_202_and_surfaces_409_code() {
        let (base, rx) = mock_once("202 Accepted", "");
        let client = HttpClient::new(base, None);
        client.start_ollama().unwrap();
        assert!(rx.recv().unwrap().starts_with("POST /v1/ollama/start "));

        let (base, _rx) = mock_once(
            "409 Conflict",
            r#"{"error":{"code":"ollama_not_startable","message":"not startable"}}"#,
        );
        let client = HttpClient::new(base, None);
        match client.start_ollama() {
            Err(ApiError::Http { status, code, .. }) => {
                assert_eq!(status, 409);
                assert_eq!(code, "ollama_not_startable");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn transport_error_when_nothing_listens() {
        // Reserved TEST-NET address / closed port: connection fails fast.
        let client = HttpClient::new("http://127.0.0.1:1", None);
        assert!(matches!(client.status(), Err(ApiError::Transport(_))));
    }

    #[test]
    fn ws_url_derivation() {
        assert_eq!(
            events_ws_url("http://127.0.0.1:8737", None),
            "ws://127.0.0.1:8737/v1/events"
        );
        assert_eq!(
            events_ws_url("http://127.0.0.1:8737/", Some("tok")),
            "ws://127.0.0.1:8737/v1/events?token=tok"
        );
        assert_eq!(
            events_ws_url("https://host:9/", Some("")),
            "wss://host:9/v1/events"
        );
    }

    #[test]
    fn backoff_is_monotonic_and_capped() {
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1000));
        assert!(backoff_delay(20) <= Duration::from_millis(10_000));
        assert!(backoff_delay(3) > backoff_delay(1));
    }
}
