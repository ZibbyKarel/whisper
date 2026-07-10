//! Fast contract tests for the `whisperd` `/v1` API (`ARCHITECTURE.md` §4, D-0009/D-0011): no
//! models, no mic, no sockets — fake engines drive the real actor/pipeline and the router is
//! exercised through `tower::oneshot`. Covers routing, the `409 busy` gate, session persistence,
//! config staging, dictionary validation, token auth, and byte-exact Czech UTF-8 round-trips.

mod support;

use axum::http::StatusCode;
use serde_json::json;

use whisper_iface::daemon::api::CleanupModeDto;
use whisper_iface::daemon::config::{Config, HistoryConfig};

use support::{
    call, call_with_headers, fake_builder, fake_builder_with_cleanup, gate_builder, harness,
    tiny_wav_b64, tiny_wav_bytes, CZECH_TEXT,
};

fn cfg(mode: CleanupModeDto) -> Config {
    Config {
        port: 0,
        cleanup_mode: mode,
        ..Config::default()
    }
}

#[tokio::test]
async fn health_reports_ok_and_version() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(&router, "GET", "/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
}

#[tokio::test]
async fn status_reports_ready_engines() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(&router, "GET", "/v1/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["phase"], "ready");
    assert_eq!(body["stt"]["ready"], true);
    assert_eq!(body["language"], "cs");
}

#[tokio::test]
async fn transcribe_returns_czech_byte_exact_with_skip_reason() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "completed");
    // Diacritics survive the full audio -> STT -> API -> JSON path byte-exact (§3.3).
    assert_eq!(body["text"], CZECH_TEXT);
    assert_eq!(body["lang"], "cs");
    assert_eq!(body["cleanup"]["applied"], false);
    assert_eq!(body["cleanup"]["skip_reason"], "mode_disabled");
    assert!(body["latency"]["total_ms"].is_u64());
}

#[tokio::test]
async fn transcribe_accepts_a_wav_path() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let dir = std::env::temp_dir().join(format!("whisperd-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("in.wav");
    std::fs::write(&path, tiny_wav_bytes()).unwrap();

    let (status, body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "path": path.to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], CZECH_TEXT);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn transcribe_with_both_inputs_is_rejected() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64(), "path": "x.wav" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn cleanup_applied_when_engine_present_and_mode_always() {
    let (_state, router) = harness(cfg(CleanupModeDto::Always), fake_builder_with_cleanup());
    let (status, body) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cleanup"]["applied"], true);
    assert!(body["cleanup"]["skip_reason"].is_null());
    assert_eq!(body["text"], CZECH_TEXT);
}

#[tokio::test]
async fn second_request_while_busy_is_structured_409() {
    let (builder, _started, release) = gate_builder();
    let (_state, router) = harness(cfg(CleanupModeDto::Never), builder);

    // First session is admitted and holds the engine (blocking STT).
    let (s1, _) = call(
        &router,
        "POST",
        "/v1/sessions",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(s1, StatusCode::ACCEPTED);

    // Second request must be rejected with the structured busy error.
    let (s2, body2) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body2["error"]["code"], "busy");

    // Release the held session so the worker thread can exit cleanly.
    let _ = release.send(());
}

#[tokio::test]
async fn async_session_result_persists_and_is_retrievable() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(
        &router,
        "POST",
        "/v1/sessions",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["session_id"].as_str().unwrap().to_string();

    // Poll the authoritative result until the session completes (fake engines are instant).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (gs, gbody) = call(&router, "GET", &format!("/v1/sessions/{id}"), None).await;
        assert_eq!(gs, StatusCode::OK);
        if gbody["status"] == "completed" {
            assert_eq!(gbody["text"], CZECH_TEXT);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session never completed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn unknown_session_is_404() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());
    let (status, body) = call(&router, "GET", "/v1/sessions/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn dictionary_round_trips_czech_and_rejects_invalid() {
    let (_state, router) = harness(cfg(CleanupModeDto::Never), fake_builder());

    let dict = json!({ "pairs": [{ "from": "žluťoučký", "to": "přežluťoučký" }] });
    let (ps, pbody) = call(&router, "PUT", "/v1/dictionary", Some(dict.clone())).await;
    assert_eq!(ps, StatusCode::OK);
    assert_eq!(pbody, dict);

    let (gs, gbody) = call(&router, "GET", "/v1/dictionary", None).await;
    assert_eq!(gs, StatusCode::OK);
    assert_eq!(gbody, dict);

    // The dictionary pass actually runs in the pipeline, with diacritics intact.
    let (ts, tbody) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(ts, StatusCode::OK);
    assert_eq!(tbody["text"], "Příliš přežluťoučký kůň úpěl ďábelské ódy.");

    let (bs, bbody) = call(
        &router,
        "PUT",
        "/v1/dictionary",
        Some(json!({ "pairs": "not-an-array" })),
    )
    .await;
    assert_eq!(bs, StatusCode::BAD_REQUEST);
    assert_eq!(bbody["error"]["code"], "bad_request");
}

#[tokio::test]
async fn config_put_applies_runtime_fields_and_stages_infra() {
    let (_state, router) = harness(cfg(CleanupModeDto::Auto), fake_builder());

    let new = Config {
        port: 9999,
        language: "en".to_string(),
        cleanup_mode: CleanupModeDto::Never,
        ..Config::default()
    };
    let (status, body) = call(
        &router,
        "PUT",
        "/v1/config",
        Some(serde_json::to_value(&new).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let applied: Vec<String> = serde_json::from_value(body["applied"].clone()).unwrap();
    let staged: Vec<String> = serde_json::from_value(body["staged"].clone()).unwrap();
    assert!(applied.contains(&"language".to_string()));
    assert!(applied.contains(&"cleanup_mode".to_string()));
    assert!(staged.contains(&"port".to_string()));

    // Runtime-applied language is visible immediately in /v1/status.
    let (_s, sbody) = call(&router, "GET", "/v1/status", None).await;
    assert_eq!(sbody["language"], "en");
    assert_eq!(sbody["cleanup_mode"], "never");
}

#[tokio::test]
async fn bearer_token_gates_all_but_health() {
    let mut config = cfg(CleanupModeDto::Never);
    config.token = Some("s3cr3t".to_string());
    let (_state, router) = harness(config, fake_builder());

    // Health is always open.
    let (hs, _) = call(&router, "GET", "/v1/health", None).await;
    assert_eq!(hs, StatusCode::OK);

    // Status without a token is rejected.
    let (us, ubody) = call(&router, "GET", "/v1/status", None).await;
    assert_eq!(us, StatusCode::UNAUTHORIZED);
    assert_eq!(ubody["error"]["code"], "unauthorized");

    // With the correct bearer token it succeeds.
    let (as_, _) = call_with_headers(
        &router,
        "GET",
        "/v1/status",
        None,
        &[("authorization", "Bearer s3cr3t")],
    )
    .await;
    assert_eq!(as_, StatusCode::OK);
}

#[tokio::test]
async fn opt_in_history_writes_sqlite_and_reports_enabled() {
    let dir = std::env::temp_dir().join(format!("whisperd-hist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("history.db");
    let _ = std::fs::remove_file(&db);

    let config = Config {
        port: 0,
        cleanup_mode: CleanupModeDto::Never,
        history: HistoryConfig {
            enabled: true,
            path: Some(db.clone()),
        },
        ..Config::default()
    };
    let (_state, router) = harness(config, fake_builder());

    let (_s, sbody) = call(&router, "GET", "/v1/status", None).await;
    assert_eq!(sbody["history_enabled"], true);

    let (ts, tbody) = call(
        &router,
        "POST",
        "/v1/transcribe",
        Some(json!({ "audio_b64": tiny_wav_b64() })),
    )
    .await;
    assert_eq!(ts, StatusCode::OK);
    let id = tbody["session_id"].as_str().unwrap();

    // The record is durably in SQLite: read it back directly, byte-exact.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT json FROM sessions WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(value["text"], CZECH_TEXT);

    let _ = std::fs::remove_file(&db);
}
