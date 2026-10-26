//! `/v1/models` contract tests (D-0015): `GET /v1/models` shape, `POST`/`DELETE
//! /v1/models/{id}/download` status codes. Fake engines, a throwaway temp `data_dir` — no
//! network, no large files (the real catalog models are never downloaded here; presence is
//! always `false` against an empty temp dir).

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use axum::http::StatusCode;

use whisper_iface::daemon::config::Config;
use whisper_iface::daemon::models;
use whisper_iface::daemon::state::AppState;

use support::{call, fake_builder, harness_with_data_dir, TempDataDir};

/// A config whose Ollama URL points at a port nothing listens on, so tests never see a real
/// Ollama running on the dev machine.
fn config_with_dead_ollama() -> Config {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let mut cfg = Config::default();
    cfg.cleanup.ollama_url = format!("http://127.0.0.1:{port}");
    cfg
}

/// Tiny mock Ollama: serves `GET /api/tags` with `tags_json` and `POST /api/pull` with
/// `pull_ndjson`, any number of times. Returns the `http://…` base URL.
fn mock_ollama(tags_json: String, pull_ndjson: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let (tags_json, pull_ndjson) = (tags_json.clone(), pull_ndjson.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap_or(0);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" => break,
                        Ok(_) => {
                            if let Some(v) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
                let payload = if request_line.starts_with("GET /api/tags") {
                    tags_json
                } else if request_line.starts_with("POST /api/pull") {
                    pull_ndjson
                } else {
                    String::new()
                };
                let mut stream = stream;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(payload.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    format!("http://{addr}")
}

fn model<'a>(body: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("no model entry {id}"))
}

/// Collect `model.download` events for `id` from the broadcast until a terminal phase.
async fn collect_download_events(
    rx: &mut tokio::sync::broadcast::Receiver<whisper_iface::daemon::api::ApiEvent>,
    id: &str,
) -> Vec<serde_json::Value> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await else {
            break;
        };
        let v = serde_json::to_value(&ev).unwrap();
        if v["type"] != "model.download" || v["id"] != id {
            continue;
        }
        let terminal = v["phase"] == "done" || v["phase"] == "error";
        seen.push(v);
        if terminal {
            break;
        }
    }
    seen
}

/// Wait until the registry slot for `id` is freed (the spawned task ran `end()`).
async fn wait_registry_free(state: &AppState, id: &str) {
    for _ in 0..200 {
        if state.model_downloads.get(id).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("registry slot for {id} never freed");
}

#[tokio::test]
async fn get_models_lists_the_full_catalog_as_not_present() {
    let data_dir = TempDataDir::new("list");
    let (_state, router) = harness_with_data_dir(
        config_with_dead_ollama(),
        fake_builder(),
        data_dir.0.clone(),
    );

    let (status, body) = call(&router, "GET", "/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);

    let models = body["models"].as_array().expect("models array");
    assert_eq!(models.len(), models::catalog().len());

    let stt = models
        .iter()
        .find(|m| m["id"] == "stt-large-v3-turbo")
        .expect("stt entry present");
    assert_eq!(stt["kind"], "stt");
    assert_eq!(stt["source"], "file");
    assert!(stt["ollama_tag"].is_null());
    assert_eq!(stt["validated"], true);
    assert_eq!(stt["selected"], false);
    assert_eq!(stt["filename"], "ggml-large-v3-turbo-q5_0.bin");
    assert_eq!(stt["required"], true);
    assert_eq!(stt["size_bytes"], 574_041_195_u64);
    assert_eq!(stt["present"], false);
    assert_eq!(stt["downloading"], false);
    assert!(stt["progress"].is_null());
    assert!(
        stt["path"]
            .as_str()
            .unwrap()
            .ends_with("models/ggml-large-v3-turbo-q5_0.bin"),
        "path was: {}",
        stt["path"]
    );

    let cleanup = models
        .iter()
        .find(|m| m["id"] == "cleanup-qwen3-8b")
        .expect("cleanup entry present");
    assert_eq!(cleanup["kind"], "cleanup");
    assert_eq!(cleanup["source"], "ollama");
    assert_eq!(cleanup["ollama_tag"], "qwen3:8b");
    assert_eq!(cleanup["required"], false);
    assert_eq!(cleanup["size_bytes"], 5_200_000_000_u64);
    assert!(cleanup["filename"].is_null());
    assert!(cleanup["path"].is_null());
    assert_eq!(cleanup["present"], false);

    // Exactly the documented keys, no more (contract fixture — fails loudly on drift).
    for entry in models {
        let keys: std::collections::BTreeSet<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "id",
            "kind",
            "source",
            "ollama_tag",
            "validated",
            "selected",
            "filename",
            "required",
            "size_bytes",
            "present",
            "path",
            "downloading",
            "progress",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected);
    }
}

#[tokio::test]
async fn get_models_reflects_a_present_file() {
    let data_dir = TempDataDir::new("present");
    let stt_model = models::find("stt-large-v3-turbo").unwrap();
    let target = models::target_path(stt_model, &data_dir.0).unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    // Presence is a pure byte-length check — write a file of the *declared* size without
    // actually downloading 574 MB (a sparse file is enough to make `len()` match).
    let file = std::fs::File::create(&target).unwrap();
    file.set_len(stt_model.size_bytes()).unwrap();

    let (_state, router) = harness_with_data_dir(
        config_with_dead_ollama(),
        fake_builder(),
        data_dir.0.clone(),
    );
    let (status, body) = call(&router, "GET", "/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(model(&body, "stt-large-v3-turbo")["present"], true);
}

#[tokio::test]
async fn post_download_unknown_id_is_404() {
    let data_dir = TempDataDir::new("post-404");
    let (_state, router) =
        harness_with_data_dir(Config::default(), fake_builder(), data_dir.0.clone());

    let (status, body) = call(&router, "POST", "/v1/models/nonexistent/download", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn delete_download_unknown_id_is_404() {
    let data_dir = TempDataDir::new("delete-404");
    let (_state, router) =
        harness_with_data_dir(Config::default(), fake_builder(), data_dir.0.clone());

    let (status, _body) = call(&router, "DELETE", "/v1/models/nonexistent/download", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_download_with_nothing_in_flight_is_a_no_op() {
    let data_dir = TempDataDir::new("delete-noop");
    let (_state, router) =
        harness_with_data_dir(Config::default(), fake_builder(), data_dir.0.clone());

    let (status, _body) = call(
        &router,
        "DELETE",
        "/v1/models/stt-large-v3-turbo/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn post_download_already_in_flight_is_409() {
    let data_dir = TempDataDir::new("post-409");
    let (state, router) =
        harness_with_data_dir(Config::default(), fake_builder(), data_dir.0.clone());

    // Simulate an in-flight download directly on the registry — no real network needed to
    // exercise the "already downloading" contract path.
    let _handle = state
        .model_downloads
        .begin("stt-large-v3-turbo")
        .expect("begin");

    let (status, body) = call(
        &router,
        "POST",
        "/v1/models/stt-large-v3-turbo/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "download_busy");

    // GET /v1/models must reflect it as downloading (even though no bytes have moved yet).
    let (_status, list) = call(&router, "GET", "/v1/models", None).await;
    let entry = list["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "stt-large-v3-turbo")
        .unwrap();
    assert_eq!(entry["downloading"], true);
    assert_eq!(entry["progress"]["downloaded"], 0);
}

#[tokio::test]
async fn list_models_marks_ollama_presence_from_tags() {
    let data_dir = TempDataDir::new("tags");
    // Czech text in a model name is not realistic, but the response must stay UTF-8 safe.
    let tags = serde_json::json!({ "models": [
        { "name": "qwen3-vl:8b-instruct", "size": 6_100_000_000_u64,
          "details": { "families": ["qwen3vl"] } },
        { "name": "gemma3:12b", "size": 8_100_000_000_u64,
          "details": { "families": ["gemma3"] } },
        { "name": "embeddinggemma:latest", "size": 600_000_000_u64,
          "details": { "families": ["gemma3"] } },
    ]})
    .to_string();
    let mut cfg = Config::default();
    cfg.cleanup.ollama_url = mock_ollama(tags, String::new());
    let (_state, router) = harness_with_data_dir(cfg, fake_builder(), data_dir.0.clone());

    let (status, body) = call(&router, "GET", "/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);

    let vl = model(&body, "cleanup-qwen3-vl-8b-instruct");
    assert_eq!(vl["present"], true);
    assert_eq!(vl["validated"], true);
    assert_eq!(vl["selected"], true);
    assert_eq!(vl["source"], "ollama");
    assert_eq!(vl["ollama_tag"], "qwen3-vl:8b-instruct");

    let q8 = model(&body, "cleanup-qwen3-8b");
    assert_eq!(q8["present"], false);
    assert_eq!(q8["selected"], false);

    let extra = model(&body, "ollama:gemma3:12b");
    assert_eq!(extra["validated"], false);
    assert_eq!(extra["required"], false);
    assert_eq!(extra["present"], true);
    assert_eq!(extra["kind"], "cleanup");
    assert_eq!(extra["size_bytes"], 8_100_000_000_u64);
    assert!(extra["filename"].is_null());
    assert!(extra["path"].is_null());
    assert_eq!(extra["selected"], false);

    let ids: Vec<&str> = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "stt-large-v3-turbo",
            "cleanup-qwen3-vl-8b-instruct",
            "cleanup-qwen3-8b",
            "ollama:gemma3:12b"
        ],
        "embedding model must not be listed"
    );
    assert_eq!(model(&body, "stt-large-v3-turbo")["source"], "file");
}

#[tokio::test]
async fn list_models_when_ollama_down() {
    let data_dir = TempDataDir::new("ollama-down");
    let (_state, router) = harness_with_data_dir(
        config_with_dead_ollama(),
        fake_builder(),
        data_dir.0.clone(),
    );

    let (status, body) = call(&router, "GET", "/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        model(&body, "cleanup-qwen3-vl-8b-instruct")["present"],
        false
    );
    assert_eq!(model(&body, "cleanup-qwen3-8b")["present"], false);
    assert_eq!(body["models"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn ollama_download_maps_pull_progress_and_done() {
    let data_dir = TempDataDir::new("pull-ok");
    let pull = [
        r#"{"status":"pulling manifest"}"#,
        r#"{"status":"pulling abc","completed":50,"total":100}"#,
        r#"{"status":"success"}"#,
    ]
    .join("\n")
        + "\n";
    let mut cfg = Config::default();
    cfg.cleanup.ollama_url = mock_ollama(r#"{"models":[]}"#.to_string(), pull);
    let (state, router) = harness_with_data_dir(cfg, fake_builder(), data_dir.0.clone());
    let mut rx = state.events.subscribe();

    let (status, _) = call(
        &router,
        "POST",
        "/v1/models/cleanup-qwen3-8b/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let events = collect_download_events(&mut rx, "cleanup-qwen3-8b").await;
    let phases: Vec<(&str, u64, u64)> = events
        .iter()
        .map(|e| {
            (
                e["phase"].as_str().unwrap(),
                e["downloaded"].as_u64().unwrap(),
                e["total"].as_u64().unwrap(),
            )
        })
        .collect();
    let half = phases
        .iter()
        .position(|p| *p == ("downloading", 50, 100))
        .unwrap_or_else(|| panic!("no 50/100 event in {phases:?}"));
    let last = phases.last().unwrap();
    assert_eq!(*last, ("done", 100, 100), "events: {phases:?}");
    assert!(half < phases.len() - 1);
    wait_registry_free(&state, "cleanup-qwen3-8b").await;
}

#[tokio::test]
async fn pull_error_line_maps_to_error_phase() {
    let data_dir = TempDataDir::new("pull-err");
    let pull = r#"{"error":"pull model manifest: file does not exist"}"#.to_string() + "\n";
    let mut cfg = Config::default();
    cfg.cleanup.ollama_url = mock_ollama(r#"{"models":[]}"#.to_string(), pull);
    let (state, router) = harness_with_data_dir(cfg, fake_builder(), data_dir.0.clone());
    let mut rx = state.events.subscribe();

    let (status, _) = call(
        &router,
        "POST",
        "/v1/models/cleanup-qwen3-8b/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let events = collect_download_events(&mut rx, "cleanup-qwen3-8b").await;
    let last = events.last().expect("a terminal event");
    assert_eq!(last["phase"], "error");
    assert_eq!(last["error"], "pull model manifest: file does not exist");

    // The slot is freed, so a retry is accepted rather than 409.
    wait_registry_free(&state, "cleanup-qwen3-8b").await;
    let (status, _) = call(
        &router,
        "POST",
        "/v1/models/cleanup-qwen3-8b/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn download_of_installed_extra_is_409() {
    let data_dir = TempDataDir::new("extra-409");
    let (_state, router) = harness_with_data_dir(
        config_with_dead_ollama(),
        fake_builder(),
        data_dir.0.clone(),
    );

    let (status, body) = call(
        &router,
        "POST",
        "/v1/models/ollama:gemma3:12b/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "already_present");
}

#[tokio::test]
async fn cancel_in_flight_ollama_pull_sets_cancel_flag() {
    let data_dir = TempDataDir::new("pull-cancel");
    let (state, router) = harness_with_data_dir(
        config_with_dead_ollama(),
        fake_builder(),
        data_dir.0.clone(),
    );
    let handle = state.model_downloads.begin("cleanup-qwen3-8b").unwrap();

    let (status, _) = call(
        &router,
        "DELETE",
        "/v1/models/cleanup-qwen3-8b/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(handle.cancel.load(std::sync::atomic::Ordering::SeqCst));
}
