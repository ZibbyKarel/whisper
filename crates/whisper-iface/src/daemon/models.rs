//! In-app model provisioning (D-0015): a compiled-in catalog of downloadable STT/cleanup
//! models, cheap presence detection, and a streaming HTTPS downloader with SHA-256
//! verification. Lives in the Interface layer, not `whisper-core` — network I/O is not the
//! core engine's job (`ARCHITECTURE.md` §3.1, D-0013 layering).
//!
//! Download target convention: `<data_dir>/models/<filename>`, matching the existing
//! `resolve_model_paths()` rebase target (`config.rs`) so a freshly downloaded model is
//! picked up by the daemon with no config edit. `data_dir` is the app-support directory
//! (`dirs::config_dir()/whisper-engine`, D-0014).
//!
//! This is the **only** place the daemon reaches the network unprompted by an existing
//! session: a one-time provisioning step over HTTPS, not a runtime cloud dependency
//! (local-first exception recorded in D-0015).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Which `ModelsConfig` slot a managed model fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Stt,
    Cleanup,
}

impl ModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelKind::Stt => "stt",
            ModelKind::Cleanup => "cleanup",
        }
    }
}

/// Where a managed model comes from.
#[derive(Debug, Clone, Copy)]
pub enum ModelSource {
    /// A pinned HTTPS download, SHA-256 verified, stored under `<data_dir>/models/` (D-0015).
    File(FileSource),
    /// A model served by the local Ollama server (D-0017): presence comes from `/api/tags`,
    /// the download is `/api/pull`. `size_hint` is an approximate on-disk size for the UI only.
    Ollama { tag: &'static str, size_hint: u64 },
}

/// The pinned-download half of [`ModelSource::File`].
#[derive(Debug, Clone, Copy)]
pub struct FileSource {
    pub filename: &'static str,
    pub url: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
}

/// One entry in the compiled-in catalog (D-0015 design doc table). Values verified against
/// HuggingFace on 2026-07-11; the STT entry's size and SHA-256 are additionally confirmed
/// byte-identical (`shasum -a 256`) to the file the Czech STT parity/e2e suites already use,
/// so provisioning introduces no pipeline change and no Czech regression. Cleanup entries are
/// Ollama tags (D-0017).
#[derive(Debug, Clone, Copy)]
pub struct ManagedModel {
    pub id: &'static str,
    pub kind: ModelKind,
    pub source: ModelSource,
    pub required: bool,
    /// Validated on real Czech samples before adoption (CLAUDE.md hard constraint).
    pub validated: bool,
    pub license: &'static str,
}

impl ManagedModel {
    /// The file source, `None` for an Ollama-sourced model.
    pub fn file(&self) -> Option<FileSource> {
        match self.source {
            ModelSource::File(f) => Some(f),
            ModelSource::Ollama { .. } => None,
        }
    }

    /// The Ollama tag, `None` for a file-sourced model.
    pub fn ollama_tag(&self) -> Option<&'static str> {
        match self.source {
            ModelSource::Ollama { tag, .. } => Some(tag),
            ModelSource::File(_) => None,
        }
    }

    /// Expected size in bytes (exact for files, an approximate hint for Ollama models).
    pub fn size_bytes(&self) -> u64 {
        match self.source {
            ModelSource::File(f) => f.size_bytes,
            ModelSource::Ollama { size_hint, .. } => size_hint,
        }
    }
}

/// The pinned catalog entries. STT is required (dictation cannot run without it); the cleanup
/// LLMs are optional (D-0005: cleanup-off is a supported profile). The id `cleanup-qwen3-8b`
/// is kept stable across the move from GGUF to Ollama (D-0017).
static CATALOG: [ManagedModel; 3] = [
    ManagedModel {
        id: "stt-large-v3-turbo",
        kind: ModelKind::Stt,
        source: ModelSource::File(FileSource {
            filename: "ggml-large-v3-turbo-q5_0.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
            size_bytes: 574_041_195,
            sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        }),
        required: true,
        validated: true,
        license: "MIT",
    },
    ManagedModel {
        id: "cleanup-qwen3-vl-8b-instruct",
        kind: ModelKind::Cleanup,
        source: ModelSource::Ollama {
            tag: "qwen3-vl:8b-instruct",
            size_hint: 6_200_000_000,
        },
        required: false,
        validated: true,
        license: "Apache-2.0",
    },
    ManagedModel {
        id: "cleanup-qwen3-8b",
        kind: ModelKind::Cleanup,
        source: ModelSource::Ollama {
            tag: "qwen3:8b",
            size_hint: 5_200_000_000,
        },
        required: false,
        validated: true,
        license: "Apache-2.0",
    },
];

/// The compiled-in model catalog.
pub fn catalog() -> &'static [ManagedModel] {
    &CATALOG
}

/// Look up a catalog entry by id.
pub fn find(id: &str) -> Option<&'static ManagedModel> {
    catalog().iter().find(|m| m.id == id)
}

/// The download target: `<data_dir>/models/<filename>`.
/// `None` for an Ollama-sourced model (it has no file under `data_dir`).
pub fn target_path(model: &ManagedModel, data_dir: &Path) -> Option<PathBuf> {
    model
        .file()
        .map(|f| data_dir.join("models").join(f.filename))
}

/// The in-progress download file: `<target>.part`. Written fresh on every attempt (no resume
/// in this iteration, per the D-0015 design doc); deleted on any error or cancel.
pub fn part_path(target: &Path) -> PathBuf {
    let mut os = target.as_os_str().to_owned();
    os.push(".part");
    PathBuf::from(os)
}

/// Cheap presence check: `present = target.exists() && metadata.len() == size_bytes`. No full
/// checksum here — hashing a 5 GB file on every launch would be far too slow. A short/
/// truncated `.part` or wrong-size file is NOT present (D-0015 design doc).
/// Always `false` for an Ollama-sourced model (its presence is `/api/tags`, not a file).
pub fn is_present(model: &ManagedModel, data_dir: &Path) -> bool {
    let (Some(f), Some(target)) = (model.file(), target_path(model, data_dir)) else {
        return false;
    };
    match std::fs::metadata(target) {
        Ok(meta) => meta.len() == f.size_bytes,
        Err(_) => false,
    }
}

/// Phase of an in-flight or just-finished download; the wire shape of the `model.download` WS
/// event's `phase` field and part of `GET /v1/models`' internal bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    Downloading,
    Verifying,
    Done,
    Error,
}

/// A progress snapshot, cloned out to callers without holding any lock.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
    pub phase: DownloadPhase,
    pub error: Option<String>,
}

/// Shared handle to one in-flight download: a cancel flag plus the latest progress snapshot
/// (D-0015 design doc: `DownloadHandle { cancel: Arc<AtomicBool>, progress: shared }`).
#[derive(Clone)]
pub struct DownloadHandle {
    pub cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<DownloadProgress>>,
}

impl DownloadHandle {
    fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(DownloadProgress {
                downloaded: 0,
                total: 0,
                phase: DownloadPhase::Downloading,
                error: None,
            })),
        }
    }

    /// The latest progress snapshot (never blocks on the download itself — just a mutex read).
    pub fn snapshot(&self) -> DownloadProgress {
        self.progress
            .lock()
            .expect("download progress lock")
            .clone()
    }

    fn set(&self, p: DownloadProgress) {
        *self.progress.lock().expect("download progress lock") = p;
    }
}

/// In-flight download registry: guards one active download per model id, shared via axum
/// state (D-0015 design doc).
#[derive(Default)]
pub struct DownloadRegistry {
    inner: Mutex<HashMap<String, DownloadHandle>>,
}

impl DownloadRegistry {
    /// Register a new in-flight download for `id`. `None` means one is already running — the
    /// caller answers `409`.
    pub fn begin(&self, id: &str) -> Option<DownloadHandle> {
        let mut map = self.inner.lock().expect("download registry lock");
        if map.contains_key(id) {
            return None;
        }
        let handle = DownloadHandle::new();
        map.insert(id.to_string(), handle.clone());
        Some(handle)
    }

    /// The handle for `id`'s in-flight download, if any.
    pub fn get(&self, id: &str) -> Option<DownloadHandle> {
        self.inner
            .lock()
            .expect("download registry lock")
            .get(id)
            .cloned()
    }

    /// Signal cancellation. `true` if a download was in flight; `false` (no-op) otherwise.
    pub fn cancel(&self, id: &str) -> bool {
        match self.get(id) {
            Some(h) => {
                h.cancel.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Remove a finished (done/errored/cancelled) download so a retry can start fresh.
    pub fn end(&self, id: &str) {
        self.inner
            .lock()
            .expect("download registry lock")
            .remove(id);
    }
}

/// Run one download attempt to completion (success, error, or cancel) on the calling thread —
/// callers run this via `tokio::task::spawn_blocking` (D-0015 design doc). `on_progress` is
/// invoked after every state change; it never runs while `handle`'s internal lock is held, so
/// it is free to do its own I/O (e.g. broadcasting a WS event).
pub fn run_download(
    model: &ManagedModel,
    data_dir: &Path,
    handle: &DownloadHandle,
    mut on_progress: impl FnMut(DownloadProgress),
) {
    let (Some(file), Some(target)) = (model.file(), target_path(model, data_dir)) else {
        let p = DownloadProgress {
            downloaded: 0,
            total: model.size_bytes(),
            phase: DownloadPhase::Error,
            error: Some(format!("{} is not a file-sourced model", model.id)),
        };
        handle.set(p.clone());
        on_progress(p);
        return;
    };
    let part = part_path(&target);
    let outcome = attempt_download(&file, &target, &part, handle, &mut on_progress);
    let final_progress = match outcome {
        Ok(()) => DownloadProgress {
            downloaded: file.size_bytes,
            total: file.size_bytes,
            phase: DownloadPhase::Done,
            error: None,
        },
        Err(e) => {
            // Never leave a partial/corrupt file behind, on ANY failure path (network error,
            // checksum mismatch, cancel) — D-0015: "on any error/cancel: delete .part".
            let _ = std::fs::remove_file(&part);
            DownloadProgress {
                downloaded: 0,
                total: file.size_bytes,
                phase: DownloadPhase::Error,
                error: Some(e),
            }
        }
    };
    handle.set(final_progress.clone());
    on_progress(final_progress);
}

/// Pull an Ollama-sourced model (`POST /api/pull`) on the calling thread — callers run this via
/// `spawn_blocking`, like [`run_download`], and the same terminal contract holds (exactly one
/// `Done` or `Error` final progress). Cancel = return from the pull, which drops the stream:
/// Ollama aborts the pull and keeps its `-partial` blobs, so a retry resumes (D-0017).
pub fn run_pull(
    model: &ManagedModel,
    client: &crate::ollama::OllamaClient,
    handle: &DownloadHandle,
    mut on_progress: impl FnMut(DownloadProgress),
) {
    let Some(tag) = model.ollama_tag() else {
        let p = DownloadProgress {
            downloaded: 0,
            total: model.size_bytes(),
            phase: DownloadPhase::Error,
            error: Some(format!("{} is not an Ollama-sourced model", model.id)),
        };
        handle.set(p.clone());
        on_progress(p);
        return;
    };
    let mut last_total = model.size_bytes();
    let result = client.pull(tag, &handle.cancel, |line| {
        if line.total > 0 {
            last_total = line.total;
        }
        let phase = if line.status.contains("verifying") {
            DownloadPhase::Verifying
        } else {
            DownloadPhase::Downloading
        };
        emit(handle, line.completed, line.total, phase, &mut on_progress);
    });
    let final_progress = match result {
        Ok(()) => DownloadProgress {
            downloaded: last_total,
            total: last_total,
            phase: DownloadPhase::Done,
            error: None,
        },
        Err(e) => DownloadProgress {
            downloaded: 0,
            total: last_total,
            phase: DownloadPhase::Error,
            error: Some(match e {
                crate::ollama::OllamaError::Cancelled => "cancelled".to_string(),
                // The server's own error text, verbatim (e.g. "pull model manifest: file does
                // not exist").
                crate::ollama::OllamaError::Protocol(text) => text,
                other => other.to_string(),
            }),
        },
    };
    handle.set(final_progress.clone());
    on_progress(final_progress);
}

fn attempt_download(
    model: &FileSource,
    target: &Path,
    part: &Path,
    handle: &DownloadHandle,
    on_progress: &mut impl FnMut(DownloadProgress),
) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create model dir {}: {e}", parent.display()))?;
    }

    // Bound only the connect + response-headers phases so a dead/unreachable host fails fast;
    // deliberately leave `timeout_recv_body`/`timeout_global` unset so a multi-GB stream is
    // never aborted mid-download (both cap the WHOLE body read, per ureq's own docs — a hard
    // 30s global timeout would require >153 Mbps for the 574 MB STT model and >1.3 Gbps for
    // the 5 GB cleanup model, failing on most real links). A truly stalled mid-stream transfer
    // relies on the user's Cancel (`DELETE /v1/models/{id}/download`) — acceptable for this
    // iteration (D-0015: resumable/bounded-stall handling is deferred).
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut resp = agent
        .get(model.url)
        .call()
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("unexpected HTTP status {}", resp.status()));
    }

    let total = model.size_bytes;
    let mut file =
        std::fs::File::create(part).map_err(|e| format!("create {}: {e}", part.display()))?;
    let mut reader = resp.body_mut().as_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_emit_bytes: u64 = 0;
    let mut last_emit_at = Instant::now();
    // Emit roughly every 2% of the file (floor 64 KiB) or every 200 ms, whichever comes
    // first — frequent enough to feel live, far too infrequent to flood WS on a 500 MB+/5 GB
    // download.
    let emit_threshold = (total / 50).max(64 * 1024);

    emit(
        handle,
        downloaded,
        total,
        DownloadPhase::Downloading,
        on_progress,
    );

    loop {
        if handle.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("read response body: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("write {}: {e}", part.display()))?;
        downloaded += n as u64;

        if downloaded - last_emit_bytes >= emit_threshold
            || last_emit_at.elapsed() >= Duration::from_millis(200)
        {
            last_emit_bytes = downloaded;
            last_emit_at = Instant::now();
            emit(
                handle,
                downloaded,
                total,
                DownloadPhase::Downloading,
                on_progress,
            );
        }
    }
    drop(file);

    emit(
        handle,
        downloaded,
        total,
        DownloadPhase::Verifying,
        on_progress,
    );

    verify_and_install(part, target, model.sha256)
}

fn emit(
    handle: &DownloadHandle,
    downloaded: u64,
    total: u64,
    phase: DownloadPhase,
    on_progress: &mut impl FnMut(DownloadProgress),
) {
    let p = DownloadProgress {
        downloaded,
        total,
        phase,
        error: None,
    };
    handle.set(p.clone());
    on_progress(p);
}

/// Hash `part` and, if it matches `expected_sha256`, atomically rename it into `target`. Pure
/// filesystem operation — no network — so it is unit-tested directly against small fixtures
/// (never the multi-GB production models).
fn verify_and_install(part: &Path, target: &Path, expected_sha256: &str) -> Result<(), String> {
    let digest_hex = hash_file_sha256(part)?;
    if digest_hex != expected_sha256 {
        return Err(format!(
            "integrity check failed: expected sha256 {expected_sha256}, got {digest_hex}"
        ));
    }
    std::fs::rename(part, target).map_err(|e| format!("rename to {}: {e}", target.display()))?;
    Ok(())
}

fn hash_file_sha256(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Minimal RAII temp-dir helper (avoids adding a `tempfile` dependency for tests).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "whisper-iface-models-test-{}-{name}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A tiny model entry for tests that never touch the (large) real catalog.
    fn small_test_model() -> ManagedModel {
        ManagedModel {
            id: "test-small",
            kind: ModelKind::Cleanup,
            source: ModelSource::File(FileSource {
                filename: "small.bin",
                url: "https://example.invalid/small.bin",
                size_bytes: 128,
                sha256: "0",
            }),
            required: false,
            validated: false,
            license: "MIT",
        }
    }

    /// Bind an ephemeral local server that serves exactly one request with a fixed byte body
    /// (D-0015 testing: no network, no large files). Returns the `http://…` base URL; the
    /// server thread exits after the one connection.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                let mut stream = stream;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    /// Build a test [`ManagedModel`] whose pinned SHA-256 matches `body` exactly.
    fn matching_test_model(url: String, body: &[u8]) -> ManagedModel {
        let digest = format!("{:x}", Sha256::digest(body));
        ManagedModel {
            id: "test-model",
            kind: ModelKind::Stt,
            source: ModelSource::File(FileSource {
                filename: "test-model.bin",
                url: Box::leak(url.into_boxed_str()),
                size_bytes: body.len() as u64,
                sha256: Box::leak(digest.into_boxed_str()),
            }),
            required: true,
            validated: false,
            license: "MIT",
        }
    }

    // -- catalog ----------------------------------------------------------

    #[test]
    fn catalog_ids_are_unique_and_sources_well_formed() {
        let entries = catalog();
        let mut ids: Vec<&str> = entries.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), entries.len(), "catalog ids must be unique");
        for m in entries {
            match m.source {
                ModelSource::File(f) => {
                    assert!(f.url.starts_with("https://"), "{} url not https", m.id);
                    assert_eq!(f.sha256.len(), 64, "{} sha256 not 64 hex chars", m.id);
                    assert!(
                        f.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                        "{} sha256 not hex",
                        m.id
                    );
                    assert!(!f.filename.is_empty(), "{} has empty filename", m.id);
                }
                ModelSource::Ollama { tag, .. } => {
                    assert!(tag.contains(':'), "{} tag must be explicit", m.id);
                }
            }
            assert!(m.size_bytes() > 0, "{} has zero size", m.id);
            assert!(
                m.validated,
                "{} is in the catalog so must be validated",
                m.id
            );
        }
        // Exactly the STT default is required (D-0015 design doc: STT required, cleanup
        // optional).
        assert_eq!(entries.iter().filter(|m| m.required).count(), 1);
        assert!(find("stt-large-v3-turbo").is_some_and(|m| m.required));
        // Stable id kept across the GGUF -> Ollama move (D-0017).
        assert_eq!(
            find("cleanup-qwen3-8b").and_then(|m| m.ollama_tag()),
            Some("qwen3:8b")
        );
        assert_eq!(
            find("cleanup-qwen3-vl-8b-instruct").and_then(|m| m.ollama_tag()),
            Some("qwen3-vl:8b-instruct")
        );
    }

    #[test]
    fn find_looks_up_by_id_or_none() {
        assert!(find("stt-large-v3-turbo").is_some());
        assert!(find("nope").is_none());
    }

    // -- presence -----------------------------------------------------------

    #[test]
    fn presence_requires_exact_size_match() {
        let dir = TempDir::new("presence");
        let model = small_test_model();
        let target = target_path(&model, dir.path()).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();

        assert!(
            !is_present(&model, dir.path()),
            "missing file is not present"
        );

        std::fs::write(&target, vec![0u8; (model.size_bytes() - 1) as usize]).unwrap();
        assert!(
            !is_present(&model, dir.path()),
            "short/truncated file is not present"
        );

        std::fs::write(&target, vec![0u8; model.size_bytes() as usize]).unwrap();
        assert!(is_present(&model, dir.path()), "exact-size file is present");
    }

    #[test]
    fn target_and_part_paths_follow_convention() {
        let model = small_test_model();
        let data_dir = Path::new("/support/whisper-engine");
        let target = target_path(&model, data_dir).unwrap();
        assert_eq!(target, data_dir.join("models").join("small.bin"));
        assert_eq!(part_path(&target), data_dir.join("models/small.bin.part"));
    }

    // -- SHA-256 verifier + atomic rename (pure, no network) ----------------

    #[test]
    fn verify_and_install_renames_on_matching_checksum() {
        let dir = TempDir::new("verify-good");
        // Includes a Czech sample so the fixture isn't purely ASCII (models are binary blobs,
        // but nothing here should assume ASCII either).
        let content = "the quick czech fox: Příliš žluťoučký kůň úpěl ďábelské ódy"
            .as_bytes()
            .to_vec();
        let part = dir.path().join("model.bin.part");
        std::fs::write(&part, &content).unwrap();
        let expected = format!("{:x}", Sha256::digest(&content));
        let target = dir.path().join("model.bin");

        verify_and_install(&part, &target, &expected).unwrap();

        assert!(target.exists(), "target must exist after a matching hash");
        assert!(!part.exists(), ".part must be gone after an atomic rename");
        assert_eq!(std::fs::read(&target).unwrap(), content);
    }

    #[test]
    fn verify_and_install_rejects_a_corrupt_fixture() {
        let dir = TempDir::new("verify-corrupt");
        let part = dir.path().join("model.bin.part");
        std::fs::write(&part, b"corrupt bytes").unwrap();
        let target = dir.path().join("model.bin");
        let wrong_sha256 = "0".repeat(64);

        let err = verify_and_install(&part, &target, &wrong_sha256).unwrap_err();

        assert!(err.contains("integrity check failed"), "err was: {err}");
        assert!(!target.exists(), "corrupt content must not be installed");
        // `verify_and_install` itself doesn't delete `part` on mismatch — cleanup is
        // centralized in `run_download` so every failure path gets it uniformly.
        assert!(part.exists());
    }

    // -- download flow (local mock server, no real network, no large files) -

    #[test]
    fn download_verifies_checksum_and_installs_atomically() {
        let body = vec![0xAB_u8; 300_000];
        let base = serve_once(body.clone());
        let model = matching_test_model(format!("{base}/file.bin"), &body);
        let dir = TempDir::new("download-good");
        let handle = DownloadHandle::new();
        let mut phases = Vec::new();

        run_download(&model, dir.path(), &handle, |p| phases.push(p.phase));

        let target = target_path(&model, dir.path()).unwrap();
        let part = part_path(&target);
        assert!(target.exists());
        assert!(!part.exists());
        assert!(is_present(&model, dir.path()));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        assert_eq!(phases.last(), Some(&DownloadPhase::Done));
        assert!(phases.contains(&DownloadPhase::Downloading));
        assert!(phases.contains(&DownloadPhase::Verifying));
        assert_eq!(handle.snapshot().phase, DownloadPhase::Done);
    }

    #[test]
    fn download_reports_progress_events() {
        let body = vec![0x11_u8; 500_000];
        let base = serve_once(body.clone());
        let model = matching_test_model(format!("{base}/file.bin"), &body);
        let dir = TempDir::new("download-progress");
        let handle = DownloadHandle::new();
        let mut downloaded_values = Vec::new();

        run_download(&model, dir.path(), &handle, |p| {
            downloaded_values.push(p.downloaded)
        });

        assert_eq!(downloaded_values.first(), Some(&0));
        assert!(
            downloaded_values
                .iter()
                .any(|&d| d > 0 && d < body.len() as u64),
            "expected at least one mid-flight progress tick: {downloaded_values:?}"
        );
        assert_eq!(downloaded_values.last(), Some(&(body.len() as u64)));
    }

    #[test]
    fn download_fails_and_cleans_up_on_checksum_mismatch() {
        let served = vec![0xCD_u8; 200_000];
        let base = serve_once(served.clone());
        let mut model = matching_test_model(format!("{base}/file.bin"), &served);
        // Pin a checksum that does NOT match what the server actually serves.
        if let ModelSource::File(f) = &mut model.source {
            f.sha256 = Box::leak("0".repeat(64).into_boxed_str());
        }
        let dir = TempDir::new("download-corrupt");
        let handle = DownloadHandle::new();
        let mut last_error = None;

        run_download(&model, dir.path(), &handle, |p| {
            if p.phase == DownloadPhase::Error {
                last_error = p.error.clone();
            }
        });

        let target = target_path(&model, dir.path()).unwrap();
        let part = part_path(&target);
        assert!(!target.exists(), "corrupt download must not be installed");
        assert!(
            !part.exists(),
            ".part must be cleaned up on checksum failure"
        );
        assert!(last_error.unwrap_or_default().contains("integrity"));
        assert_eq!(handle.snapshot().phase, DownloadPhase::Error);
    }

    #[test]
    fn cancel_stops_the_download_and_cleans_up() {
        // Large enough (relative to the 64 KiB read buffer) that the first progress tick
        // fires before the whole body is read, so the cancel-in-callback trick below is
        // deterministic (no sleeps/timing races).
        let body = vec![0x42_u8; 1_000_000];
        let base = serve_once(body.clone());
        let model = matching_test_model(format!("{base}/file.bin"), &body);
        let dir = TempDir::new("download-cancel");
        let handle = DownloadHandle::new();
        let cancel_flag = handle.cancel.clone();
        let mut saw_downloading = false;

        run_download(&model, dir.path(), &handle, |p| {
            if p.phase == DownloadPhase::Downloading && p.downloaded > 0 && !saw_downloading {
                saw_downloading = true;
                // Cancel as soon as the first real progress tick is observed — the very next
                // loop iteration in `attempt_download` checks the flag before its next read.
                cancel_flag.store(true, Ordering::SeqCst);
            }
        });

        assert!(saw_downloading, "expected at least one progress tick");
        let target = target_path(&model, dir.path()).unwrap();
        let part = part_path(&target);
        assert!(!target.exists());
        assert!(!part.exists(), ".part must be removed on cancel");
        assert_eq!(handle.snapshot().phase, DownloadPhase::Error);
        assert_eq!(handle.snapshot().error.as_deref(), Some("cancelled"));
    }

    // -- registry -------------------------------------------------------------

    #[test]
    fn registry_allows_only_one_active_download_per_id() {
        let reg = DownloadRegistry::default();
        let first = reg.begin("m1");
        assert!(first.is_some());
        assert!(reg.begin("m1").is_none(), "second begin must be refused");
        assert!(reg.begin("m2").is_some(), "different id is independent");

        reg.end("m1");
        assert!(reg.begin("m1").is_some(), "begin succeeds again after end");
    }

    #[test]
    fn registry_cancel_is_a_no_op_when_nothing_in_flight() {
        let reg = DownloadRegistry::default();
        assert!(!reg.cancel("missing"));
        reg.begin("m1").unwrap();
        assert!(reg.cancel("m1"));
    }
}
