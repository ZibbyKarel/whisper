//! `whisper-cli`: a thin HTTP/WS client of the `whisperd` `/v1/*` API (`ARCHITECTURE.md` §4,
//! D-0009). It never links `whisper-core` or any engine directly — "CLI parity is free" — so it
//! exercises the same public contract an external orchestrator would.

use std::path::PathBuf;
use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use ureq::Agent;

/// Thin client of the whisperd `/v1` API.
#[derive(Parser)]
#[command(name = "whisper-cli", version)]
struct Cli {
    /// Base URL of the whisperd daemon.
    #[arg(long, default_value = "http://127.0.0.1:8737", global = true)]
    server: String,
    /// Bearer token, if the daemon requires one (or set WHISPERD_TOKEN).
    #[arg(long, env = "WHISPERD_TOKEN", global = true)]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One-shot transcription of a local WAV file (POST /v1/transcribe).
    Transcribe {
        file: PathBuf,
        /// Override cleanup gating: always | never | auto.
        #[arg(long)]
        cleanup_mode: Option<String>,
    },
    /// Engine/model status (GET /v1/status).
    Status,
    /// Read or update configuration (GET/PUT /v1/config).
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Stream live events from WS /v1/events to stdout (one JSON object per line).
    Listen,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the current configuration.
    Get,
    /// Set top-level fields, e.g. `config set language=cs cleanup_mode=never port=8737`.
    Set { assignments: Vec<String> },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Transcribe { file, cleanup_mode } => {
            cmd_transcribe(&cli, file, cleanup_mode.as_deref())
        }
        Command::Status => cmd_status(&cli),
        Command::Config { action } => cmd_config(&cli, action),
        Command::Listen => cmd_listen(&cli),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("whisper-cli: {err}");
            ExitCode::FAILURE
        }
    }
}

/// An agent that surfaces 4xx/5xx as normal responses so the structured error envelope is
/// printed rather than swallowed.
fn agent() -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

fn cmd_transcribe(cli: &Cli, file: &PathBuf, cleanup_mode: Option<&str>) -> Result<(), String> {
    let bytes = std::fs::read(file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let mut body = serde_json::json!({ "audio_b64": STANDARD.encode(bytes) });
    if let Some(mode) = cleanup_mode {
        body["cleanup_mode"] = serde_json::Value::String(mode.to_string());
    }
    let value = post_json(cli, "/v1/transcribe", &body)?;
    println!("{}", pretty(&value));
    Ok(())
}

fn cmd_status(cli: &Cli) -> Result<(), String> {
    let value = get_json(cli, "/v1/status")?;
    println!("{}", pretty(&value));
    Ok(())
}

fn cmd_config(cli: &Cli, action: &ConfigAction) -> Result<(), String> {
    match action {
        ConfigAction::Get => {
            let value = get_json(cli, "/v1/config")?;
            println!("{}", pretty(&value));
            Ok(())
        }
        ConfigAction::Set { assignments } => {
            let mut config = get_json(cli, "/v1/config")?;
            for assignment in assignments {
                let (key, raw) = assignment
                    .split_once('=')
                    .ok_or_else(|| format!("expected key=value, got '{assignment}'"))?;
                config[key] = parse_value(key, raw);
            }
            let value = put_json(cli, "/v1/config", &config)?;
            println!("{}", pretty(&value));
            Ok(())
        }
    }
}

/// `port` is numeric; everything else is a string field on the config contract.
fn parse_value(key: &str, raw: &str) -> serde_json::Value {
    if key == "port" {
        if let Ok(n) = raw.parse::<u64>() {
            return serde_json::Value::Number(n.into());
        }
    }
    serde_json::Value::String(raw.to_string())
}

fn cmd_listen(cli: &Cli) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("build runtime: {e}"))?;
    runtime.block_on(listen(cli))
}

async fn listen(cli: &Cli) -> Result<(), String> {
    let base = cli
        .server
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut url = format!("{base}/v1/events");
    if let Some(token) = &cli.token {
        url.push_str(&format!("?token={token}"));
    }

    let (stream, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("connect ws: {e}"))?;
    let (_write, mut read) = stream.split();
    while let Some(message) = read.next().await {
        match message.map_err(|e| format!("ws error: {e}"))? {
            tokio_tungstenite::tungstenite::Message::Text(text) => println!("{text}"),
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn get_json(cli: &Cli, path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", cli.server, path);
    let mut req = agent().get(&url);
    if let Some(token) = &cli.token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let mut res = req.call().map_err(|e| format!("GET {path}: {e}"))?;
    read_checked(res.status().as_u16(), &mut res, path)
}

fn post_json(cli: &Cli, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", cli.server, path);
    let mut req = agent().post(&url);
    if let Some(token) = &cli.token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let mut res = req
        .send_json(body)
        .map_err(|e| format!("POST {path}: {e}"))?;
    read_checked(res.status().as_u16(), &mut res, path)
}

fn put_json(cli: &Cli, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", cli.server, path);
    let mut req = agent().put(&url);
    if let Some(token) = &cli.token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let mut res = req
        .send_json(body)
        .map_err(|e| format!("PUT {path}: {e}"))?;
    read_checked(res.status().as_u16(), &mut res, path)
}

/// Parse the JSON body and turn a non-2xx status into an error carrying the server's structured
/// message (`ARCHITECTURE.md` §3: structured error envelopes).
fn read_checked(
    status: u16,
    res: &mut ureq::http::Response<ureq::Body>,
    path: &str,
) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = res
        .body_mut()
        .read_json()
        .map_err(|e| format!("decode {path} response: {e}"))?;
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        let message = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("request failed");
        Err(format!("{path} -> HTTP {status}: {message}"))
    }
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
