//! `/v1/models` contract tests (D-0015): `GET /v1/models` shape, `POST`/`DELETE
//! /v1/models/{id}/download` status codes. Fake engines, a throwaway temp `data_dir` — no
//! network, no large files (the real catalog models are never downloaded here; presence is
//! always `false` against an empty temp dir).

mod support;

use axum::http::StatusCode;

use whisper_iface::daemon::config::Config;
use whisper_iface::daemon::models;

use support::{call, fake_builder, harness_with_data_dir, TempDataDir};

#[tokio::test]
async fn get_models_lists_the_full_catalog_as_not_present() {
    let data_dir = TempDataDir::new("list");
    let (_state, router) =
        harness_with_data_dir(Config::default(), fake_builder(), data_dir.0.clone());

    let (status, body) = call(&router, "GET", "/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);

    let models = body["models"].as_array().expect("models array");
    assert_eq!(models.len(), models::catalog().len());

    let stt = models
        .iter()
        .find(|m| m["id"] == "stt-large-v3-turbo")
        .expect("stt entry present");
    assert_eq!(stt["kind"], "stt");
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
    assert_eq!(cleanup["required"], false);
    assert_eq!(cleanup["size_bytes"], 5_027_783_488_u64);

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
    let cleanup_model = &models::catalog()[1]; // cleanup-qwen3-8b, small enough to fake tiny.
    let target = models::target_path(cleanup_model, &data_dir.0);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    // Presence is a pure byte-length check — write a file of the *declared* size without
    // actually downloading 5 GB (a sparse file is enough to make `len()` match).
    let file = std::fs::File::create(&target).unwrap();
    file.set_len(cleanup_model.size_bytes).unwrap();

    let (_state, router) =
        harness_with_data_dir(Config::default(), fake_builder(), data_dir.0.clone());
    let (status, body) = call(&router, "GET", "/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);

    let entry = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "cleanup-qwen3-8b")
        .unwrap();
    assert_eq!(entry["present"], true);
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
