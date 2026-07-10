//! Contract tests for the Phase-5 platform integration (`ARCHITECTURE.md` §5, D-0012), driven
//! entirely by injected fakes — no CoreAudio, no synthesized keys, no TCC. They prove the live
//! mic-session path through the actor, graceful stop, the `mic`-source guards, delivery-sink
//! selection, and the `secure_input.blocked` / `platform.permission` WS mappings. Because the
//! fakes are injected through the daemon's live-source / sink seams, these run in the default
//! (headless) test build — the real hardware paths are `#[ignore]` + the manual checklist.

mod support;

use axum::http::StatusCode;
use serde_json::json;

use whisper_iface::daemon::api::CleanupModeDto;
use whisper_iface::daemon::config::{Config, DeliveryMode};

use support::{
    call, capture_sink_builder, fake_builder, fake_builder_with_sink, fake_live_source,
    fake_live_source_denied, harness, harness_with_live, secure_input_sink_builder, tiny_wav_b64,
    CZECH_TEXT,
};

fn cfg(mode: CleanupModeDto) -> Config {
    Config {
        port: 0,
        cleanup_mode: mode,
        ..Config::default()
    }
}

async fn poll_status(router: &axum::Router, id: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (_s, body) = call(router, "GET", &format!("/v1/sessions/{id}"), None).await;
        let status = body["status"].as_str().unwrap_or("").to_string();
        if status == "completed" || status == "failed" || status == "cancelled" {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session {id} never settled (last: {body})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn mic_session_runs_and_stops_gracefully() {
    let (_state, router) = harness_with_live(
        cfg(CleanupModeDto::Never),
        fake_builder(),
        Some(fake_live_source()),
    );

    // Start a live mic session (no audio_b64/path).
    let (status, body) = call(
        &router,
        "POST",
        "/v1/sessions",
        Some(json!({ "source": "mic" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["session_id"].as_str().unwrap().to_string();

    // The fake mic keeps yielding frames until we stop; graceful stop must complete the session.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let (stop_status, _b) = call(&router, "POST", &format!("/v1/sessions/{id}/stop"), None).await;
    assert_eq!(stop_status, StatusCode::OK);

    let final_body = poll_status(&router, &id).await;
    assert_eq!(final_body["status"], "completed");
    assert_eq!(final_body["text"], CZECH_TEXT);
}

#[tokio::test]
async fn mic_source_without_backend_is_rejected() {
    // No live source provider (headless build) → mic must be a structured 400, gate untouched.
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(
        &router,
        "POST",
        "/v1/sessions",
        Some(json!({ "source": "mic" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
    // A subsequent normal transcribe still works (the gate was not consumed).
    let (ts, tbody) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(ts, StatusCode::OK);
    assert_eq!(tbody["text"], CZECH_TEXT);
}

#[tokio::test]
async fn mic_source_on_transcribe_is_rejected() {
    let (_state, router) = harness_with_live(
        cfg(CleanupModeDto::Never),
        fake_builder(),
        Some(fake_live_source()),
    );
    let (status, body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "source": "mic" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn mic_permission_denied_fails_and_emits_platform_permission() {
    let (state, router) = harness_with_live(
        cfg(CleanupModeDto::Never),
        fake_builder(),
        Some(fake_live_source_denied()),
    );
    let mut events = state.events.subscribe();

    let (status, body) = call(
        &router,
        "POST",
        "/v1/sessions",
        Some(json!({ "source": "mic" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["session_id"].as_str().unwrap().to_string();

    let final_body = poll_status(&router, &id).await;
    assert_eq!(final_body["status"], "failed");

    // A `platform.permission` event (mic denied) must have been broadcast.
    let mut saw_permission = false;
    while let Ok(ev) = events.try_recv() {
        let v = serde_json::to_value(&ev).unwrap();
        if v["type"] == "platform.permission" {
            assert_eq!(v["permission"], "microphone");
            assert_eq!(v["granted"], false);
            saw_permission = true;
        }
    }
    assert!(saw_permission, "expected a platform.permission event");
}

#[tokio::test]
async fn delivery_paste_routes_to_the_paste_sink() {
    let (sink_builder, delivered) = capture_sink_builder();
    let config = Config {
        port: 0,
        cleanup_mode: CleanupModeDto::Never,
        delivery: DeliveryMode::Paste,
        ..Config::default()
    };
    let (_state, router) = harness(config, fake_builder_with_sink(sink_builder));

    let (status, _body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The paste sink received the final text, byte-exact.
    assert_eq!(delivered.lock().unwrap().as_deref(), Some(CZECH_TEXT));
}

#[tokio::test]
async fn secure_input_completes_and_emits_secure_input_blocked() {
    let config = Config {
        port: 0,
        cleanup_mode: CleanupModeDto::Never,
        delivery: DeliveryMode::Paste,
        ..Config::default()
    };
    let (state, router) = harness(
        config,
        fake_builder_with_sink(secure_input_sink_builder("Terminal")),
    );
    let mut events = state.events.subscribe();

    let (status, body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    // Secure Input is not a failure: the session completes (text is on the clipboard).
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "completed");

    let mut saw_blocked = false;
    while let Ok(ev) = events.try_recv() {
        let v = serde_json::to_value(&ev).unwrap();
        if v["type"] == "secure_input.blocked" {
            assert_eq!(v["owner"], "Terminal");
            saw_blocked = true;
        }
    }
    assert!(saw_blocked, "expected a secure_input.blocked event");
}
