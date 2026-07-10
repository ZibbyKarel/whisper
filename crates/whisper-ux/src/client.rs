//! HTTP client of the whisperd `/v1` control API (D-0013: the shell talks ONLY
//! to the Interface Layer). Behind an [`Api`] trait so the GUI can be driven by a
//! fake in tests and the real [`HttpClient`] (ureq) is exercised against a
//! std-`TcpListener` mock — no daemon, no engines.

use std::time::Duration;

use ureq::Agent;

use crate::api::{
    Config, ConfigUpdateResponse, ErrorEnvelope, HealthResponse, SessionAccepted, SessionRecord,
    StatusResponse,
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
            r#"{"bind":"127.0.0.1","port":8737,"cleanup_mode":"auto","language":"cs","delivery":"none","models":{"stt":"m.bin","cleanup":null},"history":{"enabled":false},"hotkey":{"enabled":false,"combo":"Ctrl+Alt+Space","mode":"push_to_talk"}}"#,
        );
        let client = HttpClient::new(base, None);
        let cfg = client.get_config().unwrap();
        assert_eq!(cfg.language, "cs");
        assert_eq!(cfg.port, 8737);
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
            r#"{"applied":["language"],"staged":[],"config":{"bind":"127.0.0.1","port":8737,"cleanup_mode":"auto","language":"en","delivery":"none","models":{"stt":"m.bin","cleanup":null},"history":{"enabled":false},"hotkey":{"enabled":false,"combo":"Ctrl+Alt+Space","mode":"push_to_talk"}}}"#,
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
