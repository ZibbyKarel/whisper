//! `whisperd` — the local-first voice intelligence daemon (`ARCHITECTURE.md` §4, D-0009/D-0011).
//!
//! Binds `127.0.0.1` (configurable), loads the resident STT/cleanup models on a dedicated worker
//! thread, and serves the `/v1` HTTP control API plus `WS /v1/events`. Fully offline; no cloud,
//! no telemetry (mission hard constraint).
//!
//! Usage: `whisperd [--config <path>] [--port <n>]`.

use std::path::PathBuf;

use whisper_core::vad::VadConfig;
use whisper_iface::daemon::config::Config;
use whisper_iface::{production_engine_builder, Daemon};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("whisperd: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args = Args::parse(std::env::args().skip(1))?;
    let config_path = args.config.unwrap_or_else(default_config_path);

    let mut config = Config::load_or_default(&config_path)?;
    if let Some(port) = args.port {
        config.port = port;
    }

    let builder = production_engine_builder(
        config.models.stt.clone(),
        config.models.cleanup.clone(),
        VadConfig::default(),
    );

    let daemon = Daemon::start(config, Some(config_path), builder).await?;
    println!("whisperd listening on http://{}", daemon.addr);
    println!("  events: ws://{}/v1/events", daemon.addr);

    // Models load on the worker thread; the server is already accepting connections and reports
    // readiness via GET /v1/status and the model.* WS events.
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("install ctrl-c handler: {e}"))?;
    println!("whisperd: shutting down");
    daemon.shutdown().await;
    Ok(())
}

struct Args {
    config: Option<PathBuf>,
    port: Option<u16>,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut config = None;
        let mut port = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" | "-c" => {
                    config = Some(PathBuf::from(
                        args.next().ok_or("--config requires a path")?,
                    ));
                }
                "--port" | "-p" => {
                    port = Some(
                        args.next()
                            .ok_or("--port requires a number")?
                            .parse()
                            .map_err(|e| format!("invalid --port: {e}"))?,
                    );
                }
                "--help" | "-h" => {
                    println!("Usage: whisperd [--config <path>] [--port <n>]");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(Self { config, port })
    }
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("whisper-engine")
        .join("config.toml")
}
