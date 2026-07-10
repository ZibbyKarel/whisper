//! WebSocket fan-out contract test (`ARCHITECTURE.md` §4 `WS /v1/events`): a real socket, a real
//! tokio-tungstenite client, fake engines. Proves session lifecycle events reach a subscribed
//! client and carry Czech text byte-exact.

mod support;

use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use whisper_iface::daemon::config::Config;
use whisper_iface::daemon::state::EnginePhase;
use whisper_iface::{production_engine_builder, Daemon};

use support::{fake_builder, tiny_wav_b64, CZECH_TEXT};

#[tokio::test]
async fn ws_streams_session_lifecycle_events() {
    let config = Config {
        port: 0,
        cleanup_mode: whisper_iface::daemon::api::CleanupModeDto::Never,
        ..Config::default()
    };
    let daemon = Daemon::start(config, None, fake_builder(), None)
        .await
        .expect("start daemon");

    // Wait for the (instant) fake model load.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while daemon.state.status.read().unwrap().phase == EnginePhase::Loading {
        assert!(std::time::Instant::now() < deadline, "engines never ready");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let addr = daemon.addr;
    let (stream, _resp) = connect_async(format!("ws://{addr}/v1/events"))
        .await
        .expect("connect ws");
    let (_write, mut read) = stream.split();
    // Give the upgrade handler a moment to subscribe before we generate events.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Kick off a transcription over the real socket (blocking client on a blocking task).
    let post = tokio::task::spawn_blocking(move || {
        let body = serde_json::json!({ "audio_b64": tiny_wav_b64() });
        let _ = ureq::post(format!("http://{addr}/v1/transcribe")).send_json(&body);
    });

    let mut seen: Vec<String> = Vec::new();
    let mut delivered_text: Option<String> = None;
    let recv_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < recv_deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(2), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                let ty = value["type"].as_str().unwrap_or_default().to_string();
                if ty == "delivered" {
                    delivered_text = value["text"].as_str().map(str::to_string);
                }
                seen.push(ty.clone());
                if ty == "delivered" {
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break, // timeout
        }
    }

    let _ = post.await;
    daemon.shutdown().await;

    assert!(
        seen.iter().any(|t| t == "session_started"),
        "missing session_started; saw {seen:?}"
    );
    assert!(
        seen.iter().any(|t| t == "raw_transcript"),
        "missing raw_transcript; saw {seen:?}"
    );
    assert!(
        seen.iter().any(|t| t == "delivered"),
        "missing delivered; saw {seen:?}"
    );
    assert_eq!(delivered_text.as_deref(), Some(CZECH_TEXT));
}

/// Compile-time proof that the production engine factory is constructible from config paths
/// (it is exercised for real by the `#[ignore]` model test). Building it loads no models.
#[test]
fn production_builder_constructs() {
    let _builder = production_engine_builder(
        std::path::PathBuf::from("models/ggml-large-v3-turbo-q5_0.bin"),
        Some(std::path::PathBuf::from("models/Qwen3-8B-Q4_K_M.gguf")),
        whisper_core::vad::VadConfig::default(),
    );
}
