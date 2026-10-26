//! Blocking HTTP client for a local Ollama server (cleanup chat, model load, version probe,
//! tag listing, model pull). Uses `ureq` like model provisioning; callers run it on blocking
//! threads. The only network peer is the configured `ollama_url`.

use std::io::{BufRead, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const TAGS_TIMEOUT: Duration = Duration::from_secs(10);
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Pull streams for minutes/GBs: only bound connect + response headers, never the body.
const PULL_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// One chat message (`role` is `system`/`user`/`assistant`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// One progress line of a model pull (`completed`/`total` are 0 when the line has none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullProgress {
    pub status: String,
    pub completed: u64,
    pub total: u64,
}

/// A model installed in Ollama (`GET /api/tags`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledModel {
    pub name: String,
    pub size: u64,
    pub families: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("ollama unreachable at {url}: {msg}")]
    Unreachable { url: String, msg: String },
    #[error("ollama HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("ollama protocol: {0}")]
    Protocol(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timeout")]
    Timeout,
}

/// Blocking Ollama client; cheap to construct, one `ureq` agent per call.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    base: String,
}

impl OllamaClient {
    pub fn new(base: impl Into<String>) -> Self {
        let mut base: String = base.into();
        base = base.trim().to_string();
        while base.ends_with('/') {
            base.pop();
        }
        Self { base }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// `GET /api/version` (2 s timeout).
    pub fn version(&self) -> Result<String, OllamaError> {
        let v = self.get_json("/api/version", VERSION_TIMEOUT)?;
        v["version"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| OllamaError::Protocol("version response lacks `version`".into()))
    }

    /// `GET /api/tags`: installed models.
    pub fn tags(&self) -> Result<Vec<InstalledModel>, OllamaError> {
        let v = self.get_json("/api/tags", TAGS_TIMEOUT)?;
        let models = v["models"]
            .as_array()
            .ok_or_else(|| OllamaError::Protocol("tags response lacks `models`".into()))?;
        Ok(models
            .iter()
            .filter_map(|m| {
                Some(InstalledModel {
                    name: m["name"].as_str()?.to_string(),
                    size: m["size"].as_u64().unwrap_or(0),
                    families: m["details"]["families"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|f| f.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect())
    }

    /// `POST /api/chat` (non-streaming, thinking off, temperature 0); returns `message.content`.
    pub fn chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        keep_alive: &str,
        timeout: Duration,
    ) -> Result<String, OllamaError> {
        let body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "think": false,
            "options": { "temperature": 0, "num_predict": 512 },
            "keep_alive": keep_alive,
        });
        let v = self.post_json("/api/chat", &body, timeout)?;
        v["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| OllamaError::Protocol("chat response lacks `message.content`".into()))
    }

    /// Load `model` into memory (empty `messages` + `keep_alive`), 120 s timeout.
    pub fn load(&self, model: &str, keep_alive: &str) -> Result<(), OllamaError> {
        let body = json!({
            "model": model,
            "messages": [],
            "stream": false,
            "think": false,
            "keep_alive": keep_alive,
        });
        self.post_json("/api/chat", &body, LOAD_TIMEOUT).map(|_| ())
    }

    /// `POST /api/pull` (NDJSON stream). Checks `cancel` between lines; an `error` line
    /// becomes `Protocol(<text>)`; a stream that ends without `success` is a protocol error.
    pub fn pull(
        &self,
        model: &str,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(PullProgress),
    ) -> Result<(), OllamaError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(PULL_HEADER_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let mut resp = agent
            .post(&self.url("/api/pull"))
            .send_json(json!({ "model": model, "stream": true }))
            .map_err(|e| self.map_err(e))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(OllamaError::Http { status, body });
        }
        let reader = resp.body_mut().as_reader();
        for line in std::io::BufReader::new(reader).lines() {
            if cancel.load(Ordering::Relaxed) {
                return Err(OllamaError::Cancelled);
            }
            let line = line.map_err(|e| self.map_io(&e))?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line)
                .map_err(|e| OllamaError::Protocol(format!("bad pull line: {e}")))?;
            if let Some(err) = v["error"].as_str() {
                return Err(OllamaError::Protocol(err.to_string()));
            }
            let status = v["status"].as_str().unwrap_or("").to_string();
            if status == "success" {
                return Ok(());
            }
            on_progress(PullProgress {
                status,
                completed: v["completed"].as_u64().unwrap_or(0),
                total: v["total"].as_u64().unwrap_or(0),
            });
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(OllamaError::Cancelled);
        }
        Err(OllamaError::Protocol(
            "pull stream ended without success".into(),
        ))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn agent(&self, total: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT.min(total)))
            .timeout_global(Some(total))
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn get_json(&self, path: &str, timeout: Duration) -> Result<Value, OllamaError> {
        let resp = self
            .agent(timeout)
            .get(&self.url(path))
            .call()
            .map_err(|e| self.map_err(e))?;
        self.read_json(resp)
    }

    fn post_json(&self, path: &str, body: &Value, timeout: Duration) -> Result<Value, OllamaError> {
        let resp = self
            .agent(timeout)
            .post(&self.url(path))
            .send_json(body)
            .map_err(|e| self.map_err(e))?;
        self.read_json(resp)
    }

    fn read_json(&self, mut resp: ureq::http::Response<ureq::Body>) -> Result<Value, OllamaError> {
        let status = resp.status().as_u16();
        let mut text = String::new();
        resp.body_mut()
            .as_reader()
            .read_to_string(&mut text)
            .map_err(|e| self.map_io(&e))?;
        if !(200..300).contains(&status) {
            return Err(OllamaError::Http { status, body: text });
        }
        serde_json::from_str(&text).map_err(|e| OllamaError::Protocol(format!("bad JSON: {e}")))
    }

    fn map_err(&self, e: ureq::Error) -> OllamaError {
        match e {
            ureq::Error::Timeout(_) => OllamaError::Timeout,
            ureq::Error::Io(io) => self.map_io(&io),
            other => OllamaError::Unreachable {
                url: self.base.clone(),
                msg: other.to_string(),
            },
        }
    }

    fn map_io(&self, e: &std::io::Error) -> OllamaError {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            return OllamaError::Timeout;
        }
        // ureq wraps its own timeout inside an io::Error on body reads.
        if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<ureq::Error>()) {
            if matches!(inner, ureq::Error::Timeout(_)) {
                return OllamaError::Timeout;
            }
        }
        OllamaError::Unreachable {
            url: self.base.clone(),
            msg: e.to_string(),
        }
    }
}

/// Canonical tag form: trimmed, and a tag without `:` is `<tag>:latest`.
pub fn normalize_tag(tag: &str) -> String {
    let t = tag.trim();
    if t.contains(':') {
        t.to_string()
    } else {
        format!("{t}:latest")
    }
}

/// True when the URL's host is a loopback address (`127.0.0.1`, `localhost`, `::1`).
pub fn is_loopback_url(url: &str) -> bool {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Serve one request: parse headers + body, send the body to the returned channel, then
    /// let `respond` write the raw HTTP response.
    fn serve_once(
        respond: impl FnOnce(&mut std::net::TcpStream) + Send + 'static,
    ) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0usize;
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                let _ = tx.send(String::from_utf8(body).unwrap());
                let mut stream = stream;
                respond(&mut stream);
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), rx)
    }

    fn json_reply(status: &str, body: &str) -> impl FnOnce(&mut std::net::TcpStream) + Send {
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        move |s| {
            let _ = s.write_all(resp.as_bytes());
        }
    }

    fn user(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
        }
    }

    #[test]
    fn chat_sends_contract_fields_and_returns_content() {
        let (url, rx) = serve_once(json_reply(
            "200 OK",
            r#"{"message":{"role":"assistant","content":"Příliš žluťoučký kůň."}}"#,
        ));
        let out = OllamaClient::new(url)
            .chat(
                "qwen3-vl:8b-instruct",
                &[user("příliš žluťoučký kůň")],
                "30m",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(out, "Příliš žluťoučký kůň.");
        let req: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(req["stream"], false);
        assert_eq!(req["think"], false);
        assert_eq!(req["options"]["temperature"], 0);
        assert_eq!(req["options"]["num_predict"], 512);
        assert_eq!(req["keep_alive"], "30m");
        assert_eq!(req["model"], "qwen3-vl:8b-instruct");
        assert_eq!(req["messages"][0]["content"], "příliš žluťoučký kůň");
    }

    #[test]
    fn chat_404_is_error_with_body() {
        let (url, _rx) = serve_once(json_reply(
            "404 Not Found",
            r#"{"error":"model 'x' not found"}"#,
        ));
        let err = OllamaClient::new(url)
            .chat("x", &[user("a")], "30m", Duration::from_secs(5))
            .unwrap_err();
        match err {
            OllamaError::Http { status, body } => {
                assert_eq!(status, 404);
                assert!(body.contains("not found"), "{body}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn chat_connection_refused_is_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = OllamaClient::new(url)
            .chat("x", &[user("a")], "30m", Duration::from_secs(5))
            .unwrap_err();
        assert!(matches!(err, OllamaError::Unreachable { .. }), "{err:?}");
    }

    #[test]
    fn chat_times_out() {
        let (url, _rx) = serve_once(|_s| std::thread::sleep(Duration::from_millis(1500)));
        let start = std::time::Instant::now();
        let err = OllamaClient::new(url)
            .chat("x", &[user("a")], "30m", Duration::from_millis(300))
            .unwrap_err();
        assert!(matches!(err, OllamaError::Timeout), "{err:?}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn version_parses() {
        let (url, _rx) = serve_once(json_reply("200 OK", r#"{"version":"0.31.1"}"#));
        assert_eq!(OllamaClient::new(url).version().unwrap(), "0.31.1");
    }

    #[test]
    fn load_sends_empty_messages() {
        let (url, rx) = serve_once(json_reply("200 OK", r#"{"done":true}"#));
        OllamaClient::new(url).load("qwen3:8b", "30m").unwrap();
        let req: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(req["messages"], json!([]));
        assert_eq!(req["keep_alive"], "30m");
    }

    #[test]
    fn tags_parses_names_and_families() {
        let (url, _rx) = serve_once(json_reply(
            "200 OK",
            r#"{"models":[{"name":"qwen3-vl:8b-instruct","size":6200000000,"details":{"families":["qwen3vl"]}},{"name":"embeddinggemma:latest","size":1,"details":{"families":["gemma3"]}}]}"#,
        ));
        let tags = OllamaClient::new(url).tags().unwrap();
        assert_eq!(
            tags,
            vec![
                InstalledModel {
                    name: "qwen3-vl:8b-instruct".into(),
                    size: 6_200_000_000,
                    families: vec!["qwen3vl".into()],
                },
                InstalledModel {
                    name: "embeddinggemma:latest".into(),
                    size: 1,
                    families: vec!["gemma3".into()],
                },
            ]
        );
    }

    fn ndjson_reply(
        lines: Vec<&'static str>,
        gap: Duration,
    ) -> impl FnOnce(&mut std::net::TcpStream) + Send {
        move |s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n");
            for l in lines {
                let _ = s.write_all(format!("{l}\n").as_bytes());
                let _ = s.flush();
                std::thread::sleep(gap);
            }
        }
    }

    #[test]
    fn pull_streams_progress_then_success() {
        let (url, rx) = serve_once(ndjson_reply(
            vec![
                r#"{"status":"pulling manifest"}"#,
                r#"{"status":"pulling abc","completed":50,"total":100}"#,
                r#"{"status":"success"}"#,
            ],
            Duration::from_millis(10),
        ));
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        OllamaClient::new(url)
            .pull("qwen3:8b", &cancel, |p| seen.push(p))
            .unwrap();
        assert!(seen.iter().any(|p| p.completed == 50 && p.total == 100));
        let req: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(req["stream"], true);
        assert_eq!(req["model"], "qwen3:8b");
    }

    #[test]
    fn pull_error_line_is_error() {
        let (url, _rx) = serve_once(ndjson_reply(
            vec![r#"{"error":"pull model manifest: file does not exist"}"#],
            Duration::ZERO,
        ));
        let cancel = AtomicBool::new(false);
        let err = OllamaClient::new(url)
            .pull("nope", &cancel, |_| {})
            .unwrap_err();
        match err {
            OllamaError::Protocol(m) => assert_eq!(m, "pull model manifest: file does not exist"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pull_cancel_stops() {
        let (url, _rx) = serve_once(ndjson_reply(
            vec![
                r#"{"status":"pulling abc","completed":1,"total":100}"#,
                r#"{"status":"pulling abc","completed":2,"total":100}"#,
                r#"{"status":"success"}"#,
            ],
            Duration::from_millis(100),
        ));
        let cancel = AtomicBool::new(false);
        let err = OllamaClient::new(url)
            .pull("qwen3:8b", &cancel, |_| {
                cancel.store(true, Ordering::Relaxed)
            })
            .unwrap_err();
        assert!(matches!(err, OllamaError::Cancelled), "{err:?}");
    }

    #[test]
    fn normalize_tag_adds_latest() {
        assert_eq!(normalize_tag("qwen3"), "qwen3:latest");
        assert_eq!(normalize_tag("  qwen3:8b "), "qwen3:8b");
        assert_eq!(
            normalize_tag("qwen3-vl:8b-instruct"),
            "qwen3-vl:8b-instruct"
        );
    }

    #[test]
    fn is_loopback_url_cases() {
        assert!(is_loopback_url("http://127.0.0.1:11434"));
        assert!(is_loopback_url("http://localhost:11434/"));
        assert!(is_loopback_url("http://[::1]:11434"));
        assert!(!is_loopback_url("http://192.168.1.4:11434"));
        assert!(!is_loopback_url("http://localhost.evil.com:11434"));
    }
}
