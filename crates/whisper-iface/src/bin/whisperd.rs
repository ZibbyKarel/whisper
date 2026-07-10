//! `whisperd` — the local-first voice intelligence daemon (`ARCHITECTURE.md` §4, D-0009/D-0011).
//!
//! Binds `127.0.0.1` (configurable), loads the resident STT/cleanup models on a dedicated worker
//! thread, and serves the `/v1` HTTP control API plus `WS /v1/events`. Fully offline; no cloud,
//! no telemetry (mission hard constraint).
//!
//! Usage: `whisperd [--config <path>] [--port <n>]`.

use std::path::PathBuf;

use whisper_iface::daemon::config::Config;
#[cfg(not(feature = "platform"))]
use whisper_iface::production_engine_builder;
use whisper_iface::Daemon;

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

    // App-bundle launches run with a cwd that is not the repo, so rebase relative
    // model paths onto the app-support data dir when the model lives there (Task 2,
    // D-0014). No-op for absolute paths or models already found relative to cwd.
    if let Some(base) = dirs::config_dir() {
        config.resolve_model_paths(&base.join("whisper-engine"));
    }

    let stt = config.models.stt.clone();
    let cleanup = config.models.cleanup.clone();

    // Compose the platform adapters only when built with `--features platform` (D-0012); the
    // default headless build keeps the null delivery sink and no mic/hotkey capture.
    #[cfg(feature = "platform")]
    let hotkey_cfg = config.hotkey.clone();
    // AGC-lite tuning from `[audio]`; the capture stream is built at startup, so it is fixed for
    // the process lifetime (edits are staged for the next restart).
    #[cfg(feature = "platform")]
    let agc = whisper_platform::AutoGainConfig {
        enabled: config.audio.agc_enabled,
        target_peak: config.audio.agc_target_peak,
        max_gain: config.audio.agc_max_gain,
    };
    #[cfg(feature = "platform")]
    let (builder, live_source) = (
        whisper_iface::daemon::platform::platform_engine_builder(stt, cleanup),
        Some(whisper_iface::daemon::platform::cpal_live_source(agc)),
    );
    #[cfg(not(feature = "platform"))]
    let (builder, live_source) = (production_engine_builder(stt, cleanup), None);

    let daemon = Daemon::start(config, Some(config_path), builder, live_source).await?;
    println!("whisperd listening on http://{}", daemon.addr);
    println!("  events: ws://{}/v1/events", daemon.addr);

    #[cfg(feature = "platform")]
    let _hotkey = whisper_iface::daemon::platform::spawn_hotkey(daemon.state.clone(), &hotkey_cfg);

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
