//! Real-model integration test (`ARCHITECTURE.md` §7): start `whisperd` in-process with the real
//! whisper.cpp turbo model (D-0004), POST a Czech fixture to `/v1/transcribe` over a real socket,
//! and assert the Czech output with diacritics intact. `#[ignore]` — needs the model in `models/`.
//!
//! Run with: `cargo test -p whisper-iface --test daemon_real_model -- --ignored`.

use std::path::{Path, PathBuf};

use whisper_iface::daemon::config::Config;
use whisper_iface::daemon::state::EnginePhase;
use whisper_iface::{production_engine_builder, Daemon};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test]
#[ignore = "requires the turbo model in models/; run with --ignored"]
async fn real_daemon_transcribes_czech_fixture() {
    let stt_model = workspace_root().join("models/ggml-large-v3-turbo-q5_0.bin");
    assert!(stt_model.exists(), "missing model {}", stt_model.display());
    let cs01 = workspace_root().join("fixtures/audio/cs-01.wav");
    assert!(cs01.exists(), "missing fixture {}", cs01.display());

    let config = Config {
        port: 0,
        cleanup_mode: whisper_iface::daemon::api::CleanupModeDto::Never,
        ..Config::default()
    };
    // STT only (empty cleanup model = cleanup off) keeps the test light — the transcribe path is
    // what we assert.
    let cleanup = whisper_iface::daemon::config::CleanupConfig {
        model: String::new(),
        ..Default::default()
    };
    let builder = production_engine_builder(stt_model, cleanup);
    let daemon = Daemon::start(config, None, builder, None)
        .await
        .expect("start daemon");

    // The whisper.cpp model loads on the worker thread; wait for readiness.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let phase = daemon.state.status.read().unwrap().phase;
        match phase {
            EnginePhase::Ready | EnginePhase::Degraded => break,
            EnginePhase::Unavailable => panic!("stt model failed to load"),
            EnginePhase::Loading => {}
        }
        assert!(std::time::Instant::now() < deadline, "model load timed out");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let addr = daemon.addr;
    let cs01_str = cs01.to_string_lossy().to_string();
    let body: serde_json::Value = tokio::task::spawn_blocking(move || {
        let req = serde_json::json!({ "path": cs01_str });
        let mut res = ureq::post(format!("http://{addr}/v1/transcribe"))
            .send_json(&req)
            .expect("transcribe request");
        res.body_mut()
            .read_json::<serde_json::Value>()
            .expect("json")
    })
    .await
    .unwrap();

    daemon.shutdown().await;

    assert_eq!(body["status"], "completed");
    let text = body["text"].as_str().unwrap();
    assert!(
        text.contains("žluťoučký"),
        "daemon lost Czech diacritics: {text:?}"
    );
}
