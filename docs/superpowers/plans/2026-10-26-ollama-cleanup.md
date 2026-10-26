# Cleanup LLM via Ollama — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the in-process llama.cpp cleanup with an Ollama-backed cleanup engine. Provision cleanup models through Ollama (validated list + already-installed models), and surface Ollama presence in the onboarding, Settings and a banner.

**Architecture:** A reusable blocking `OllamaClient` (ureq) in `whisper-iface` backs three consumers:
- `OllamaCleanupEngine` (implements the core `CleanupEngine` port);
- a daemon `ollama` service module (probe/start, `/v1/ollama*`);
- Ollama-sourced entries in the D-0015 model catalog (presence via `/api/tags`, download via `/api/pull`).

The core pipeline degrades to dictionary-passed text on any cleanup failure. The tray stays a `/v1` client.

**Tech Stack:** Rust workspace (`whisper-core`, `whisper-iface`, `whisper-ux`), ureq 3, axum 0.8, serde, egui. Ollama HTTP API (`/api/version`, `/api/tags`, `/api/chat`, `/api/pull`).

**Spec:** `docs/superpowers/specs/2026-10-26-ollama-cleanup-design.md`. Read it first; this plan argues from it.

## Global Constraints

- Default cleanup model `qwen3-vl:8b-instruct`; validated catalog = `qwen3-vl:8b-instruct` (default), `qwen3:8b`.
- Default `ollama_url = "http://127.0.0.1:11434"`, `keep_alive = "30m"`.
- Chat request: `stream:false`, `think:false`, `options.temperature:0`, `options.num_predict:512`, the `keep_alive` value.
- Chat timeouts: 2 s connect, 20 s total.
- Runtime network only to `ollama_url`. No new HTTP dependency (use `ureq` 3). No new third-party crate without an ADR line.
- Remove `llama-cpp-2` entirely (both `[dependencies]` and the macOS target block).
- Whisper never deletes Ollama models (no DELETE route for `source:"ollama"` entries).
- Auto-start / `POST /v1/ollama/start` only when `ollama_url` host is loopback (`127.0.0.1`, `localhost`, `::1`).
- Start order: `brew services start ollama` (if `ollama` resolves under a Homebrew prefix `/opt/homebrew` or `/usr/local`), else `open -a Ollama` (if `/Applications/Ollama.app` exists), else spawn `ollama serve` detached. Then poll `/api/version` ≤10 s.
- Tag equality is normalized: a tag with no `:` ≡ `<tag>:latest`.
- Contract-mirror discipline: every `/v1` DTO change gets mirrored in `crates/whisper-ux/src/api.rs` with a JSON fixture test on both sides.
- Unit tests use tiny local mock HTTP servers (existing pattern in `crates/whisper-iface/tests/model_provisioning.rs`). No test touches the real network or a real model unless it's `#[ignore]`.
- Use `rtk` prefixes for cargo/git commands (repo CLAUDE.md). Quality gate per task: `rtk cargo build --workspace`, `rtk cargo test --workspace`, `rtk cargo clippy --workspace --all-targets -- -D warnings`.
- Czech test cases are mandatory wherever text flows (use Czech diacritic strings in fixtures, e.g. `"Příliš žluťoučký kůň"`).

## Review Focus

- **Ollama dies mid-dictation** (connection refused / reset after the session started). Expected: the session completes, dictionary text is delivered, `cleanup.skipped_reason="engine_failed"`. Test: Task 1 `cleanup_error_degrades_to_dict_text` + Task 3 `chat_connection_refused_is_error`.
- **Ollama returns HTTP 404 `model not found`** (the user deleted the model in Ollama). Expected: same degrade; status `cleanup.ready=false` with the error text. Test: Task 3 `chat_404_is_error_with_body`, Task 5 `status_reflects_last_cleanup_failure`.
- **The model emits an empty string or only a `<think>` block.** Expected: treated as failure (degrade), never an empty paste. Test: Task 4 `empty_output_is_error`.
- **Pull of an unknown tag** (Ollama streams `{"error":"pull model manifest: file does not exist"}`). Expected: `model.download` phase `error` with that message, and the registry is freed so Retry works. Test: Task 6 `pull_error_line_maps_to_error_phase`.
- **The user picks "Off" while a session is in flight, or switches the model between sessions.** Expected: the next session uses the new value with no restart; "Off" → `no_engine_configured`. Test: Task 5 `put_config_cleanup_model_applies_at_runtime`.

---

### Task 1: Core — degrade on cleanup failure, `prepare`/`is_enabled` hooks

**Files:**
- Modify: `crates/whisper-core/src/ports.rs` (trait `CleanupEngine`, ~line 30)
- Modify: `crates/whisper-core/src/pipeline.rs` (`CleanupSkipReason` ~93, session run ~157-285)
- Modify: `crates/whisper-iface/src/daemon/api.rs` (`skip_reason_str` ~44)
- Modify: `crates/whisper-ux/src/api.rs` if it enumerates skip reasons (grep `no_engine_configured`)
- Test: `crates/whisper-core/src/pipeline.rs` tests module / `crates/whisper-core/tests/pipeline_integration.rs`

**Interfaces:**
- Produces:
  - `trait CleanupEngine { fn clean(&self, text:&str, ctx:&CleanupCtx)->Result<String,CleanupError>; fn prepare(&self) {} fn is_enabled(&self)->bool { true } }`
  - `CleanupSkipReason::EngineFailed` (wire `"engine_failed"`).
  - The pipeline calls `prepare()` once when a session enters `Recording` (before capture frames are consumed), when an engine is present.
  - `is_enabled()==false` is treated exactly like `None` → `NoEngineConfigured`.
  - On `Err` from `clean()`: emit `CleanupSkipped{reason:EngineFailed}`, log the error via the existing pipeline event/log path, and deliver `dict_text`.

- [ ] **Step 1: Write failing tests** in the pipeline tests (use the existing `test_support` fakes; add a `FailingCleanup` fake returning `Err` and a `CountingCleanup` fake that counts `prepare` calls and has a togglable `is_enabled`):
  - `cleanup_error_degrades_to_dict_text`: mode `Always`, Czech input `"ehm, Příliš žluťoučký kůň"` (or whatever the fake STT returns). Assert the session `Ok`, the delivered text == dict-passed text, and the events contain `CleanupSkipped{reason: EngineFailed}`.
  - `prepare_called_once_per_session`: assert the count == 1 after one session.
  - `disabled_engine_skips_as_no_engine_configured`: `is_enabled=false`, mode `Always`. Assert `CleanupSkipped{NoEngineConfigured}` and `clean` is never called.
  - Update the existing test that asserted failure-on-cleanup-error to the new behavior.
- [ ] **Step 2: Run** `rtk cargo test -p whisper-core` → the new tests FAIL.
- [ ] **Step 3: Implement** the trait defaults, the new variant, and the pipeline changes. Add `"engine_failed"` to `skip_reason_str`, and mirror it in the ux enum if one exists.
- [ ] **Step 4: Run** `rtk cargo test --workspace` → PASS. Clippy clean.
- [ ] **Step 5: Commit** `feat(core): degrade to dictionary text on cleanup failure; prepare/is_enabled hooks`

### Task 2: Config — `[cleanup]` section, legacy `models.cleanup` ignored

**Files:**
- Modify: `crates/whisper-iface/src/daemon/config.rs` (`ModelsConfig` ~40-60, `Config` ~229, `resolve_model_paths` ~300, tests)
- Modify: `crates/whisper-ux/src/api.rs` (`ModelsConfig` ~48, `Config` ~115) + its fixture tests
- Modify: repo-root `config.toml` (example) to show `[cleanup]`

**Interfaces:**
- Produces (daemon and an identical ux mirror):
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(default)]
  pub struct CleanupConfig { pub ollama_url: String, pub model: String, pub keep_alive: String }
  // Default: "http://127.0.0.1:11434", "qwen3-vl:8b-instruct", "30m"
  // Config gains: pub cleanup: CleanupConfig
  impl CleanupConfig { pub fn enabled(&self) -> bool { !self.model.trim().is_empty() } }
  ```
- `ModelsConfig.cleanup` stays as `#[serde(default, skip_serializing)] pub cleanup: Option<PathBuf>`, with a doc comment "legacy, ignored (D-0017)" and default `None`. `resolve_model_paths` stops touching it. The daemon startup logs one line if it is `Some`.

- [ ] **Step 1: Failing tests** (daemon `config.rs` tests):
  - `default_cleanup_is_qwen3_vl_on_ollama`: assert the three defaults above.
  - `legacy_models_cleanup_parses_and_is_ignored`: parse TOML containing `[models]\nstt="x.bin"\ncleanup="/abs/Qwen3-8B-Q4_K_M.gguf"`. Assert `cfg.cleanup.model=="qwen3-vl:8b-instruct"`, and that the re-serialized TOML has no `Qwen3-8B` substring.
  - `empty_model_disables`: `[cleanup]\nmodel=""` → `!cfg.cleanup.enabled()`.
  - ux mirror: a JSON fixture round-trip including `"cleanup":{"ollama_url":..,"model":..,"keep_alive":..}`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** Remove `default_cleanup_model()`.
- [ ] **Step 4: Run** the workspace tests → PASS (fix the config tests that referenced `models/llm.gguf` rebasing).
- [ ] **Step 5: Commit** `feat(config): [cleanup] Ollama section; legacy models.cleanup ignored`

### Task 3: `OllamaClient` — blocking HTTP client for Ollama

**Files:**
- Create: `crates/whisper-iface/src/ollama.rs`
- Modify: `crates/whisper-iface/src/lib.rs` (`pub mod ollama;`)
- Test: unit tests inside `ollama.rs` with a mock `std::net::TcpListener` server (follow the mock pattern from `tests/model_provisioning.rs`)

**Interfaces:**
- Produces:
  ```rust
  pub struct OllamaClient { base: String }               // base without trailing '/'
  #[derive(Debug, Clone, PartialEq, Eq, Serialize)] pub struct ChatMessage { pub role: String, pub content: String }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct PullProgress { pub status: String, pub completed: u64, pub total: u64 }
  #[derive(Debug, thiserror::Error)] pub enum OllamaError { #[error("ollama unreachable at {url}: {msg}")] Unreachable{url:String,msg:String}, #[error("ollama HTTP {status}: {body}")] Http{status:u16,body:String}, #[error("ollama protocol: {0}")] Protocol(String), #[error("cancelled")] Cancelled, #[error("timeout")] Timeout }
  impl OllamaClient {
    pub fn new(base: impl Into<String>) -> Self;
    pub fn base(&self) -> &str;
    pub fn version(&self) -> Result<String, OllamaError>;            // GET /api/version, 2 s timeout
    pub fn tags(&self) -> Result<Vec<InstalledModel>, OllamaError>;  // GET /api/tags
    pub fn chat(&self, model:&str, messages:&[ChatMessage], keep_alive:&str, timeout:Duration) -> Result<String, OllamaError>;
    pub fn load(&self, model:&str, keep_alive:&str) -> Result<(), OllamaError>; // /api/chat with messages:[] (loads model), 120 s timeout
    pub fn pull(&self, model:&str, cancel:&AtomicBool, on_progress: impl FnMut(PullProgress)) -> Result<(), OllamaError>; // POST /api/pull stream:true, NDJSON
  }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct InstalledModel { pub name: String, pub size: u64, pub families: Vec<String> }
  pub fn normalize_tag(tag: &str) -> String;  // "qwen3" -> "qwen3:latest"; trims
  pub fn is_loopback_url(url: &str) -> bool;
  ```
- The chat body has the exact fields from Global Constraints. The response is read from `message.content`.
- A pull NDJSON line with an `"error"` key → `Err(Http{status:200, body:<error>})`, or a `Protocol` with the error text (pick one; the message text must be preserved).
- A final `{"status":"success"}` → `Ok`. Cancel is checked between lines; drop the reader on cancel.

- [ ] **Step 1: Failing tests:**
  - `chat_sends_contract_fields_and_returns_content`: the mock asserts the request JSON has `stream==false`, `think==false`, `options.temperature==0`, `keep_alive=="30m"`, `model=="qwen3-vl:8b-instruct"`. It replies `{"message":{"role":"assistant","content":"Příliš žluťoučký kůň."}}` → result equals that string.
  - `chat_404_is_error_with_body`: the mock replies 404 `{"error":"model 'x' not found"}` → `Http{status:404, body}` with the body containing `not found`.
  - `chat_connection_refused_is_error`: bind a port, drop the listener → `Unreachable`.
  - `chat_times_out`: the mock accepts and sleeps past a 300 ms timeout → `Timeout` (or `Unreachable`; assert `is_err` and that the elapsed time < 2 s).
  - `tags_parses_names_and_families`: from the fixture `{"models":[{"name":"qwen3-vl:8b-instruct","size":6200000000,"details":{"families":["qwen3vl"]}},{"name":"embeddinggemma:latest","size":1,"details":{"families":["gemma3"]}}]}`.
  - `pull_streams_progress_then_success`: the mock writes 3 NDJSON lines (`pulling manifest`, `{"status":"pulling abc","completed":50,"total":100}`, `{"status":"success"}`) → callback sees `completed=50,total=100`, result `Ok`.
  - `pull_error_line_is_error`: the line `{"error":"pull model manifest: file does not exist"}` → `Err` with the message preserved.
  - `pull_cancel_stops`: cancel set before the second line → `Err(Cancelled)`.
  - `normalize_tag_adds_latest`, `is_loopback_url_cases` (`127.0.0.1`, `localhost`, `[::1]` true; `192.168.1.4` false).
- [ ] **Step 2: Run** `rtk cargo test -p whisper-iface ollama` → FAIL.
- [ ] **Step 3: Implement** with a `ureq::Agent` per call kind (configure the connect/global timeouts like `models.rs` does). Read the pull with `BufRead::lines()` over `body_mut().as_reader()`.
- [ ] **Step 4: Run** → PASS; clippy clean.
- [ ] **Step 5: Commit** `feat(iface): OllamaClient (version/tags/chat/load/pull)`

### Task 4: `OllamaCleanupEngine` replaces llama.cpp; Czech trap gate test

**Files:**
- Rewrite: `crates/whisper-iface/src/cleanup.rs`. Keep `SYSTEM_PROMPT`, `FEW_SHOT` (same order, load-bearing), `strip_think` and their tests. Delete all llama/ChatML/KV-cache code.
- Modify: `crates/whisper-iface/src/lib.rs` (`pub use cleanup::{OllamaCleanupEngine, CleanupHealth};`, drop `LlamaCleanupEngine`)
- Modify: `crates/whisper-iface/Cargo.toml` (remove both `llama-cpp-2` lines and their comments)
- Rewrite: `crates/whisper-iface/tests/cleanup_trap_safety.rs` to drive `OllamaCleanupEngine`

**Interfaces:**
- Consumes: Task 1 trait hooks, Task 3 `OllamaClient`, `ChatMessage`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Default, PartialEq, Eq)] pub struct CleanupHealth { pub ready: bool, pub error: Option<String> }
  pub struct OllamaCleanupEngine { /* client, model: Arc<RwLock<String>>, keep_alive: String, health: Arc<RwLock<CleanupHealth>> */ }
  impl OllamaCleanupEngine {
    pub fn new(client: OllamaClient, model: Arc<RwLock<String>>, keep_alive: String, health: Arc<RwLock<CleanupHealth>>) -> Self;
    pub fn messages_for(text: &str) -> Vec<ChatMessage>; // system SYSTEM_PROMPT, then FEW_SHOT user/assistant pairs, then user text
    pub fn warm_up_async(&self);                          // spawns a std::thread calling client.load(); updates health
  }
  impl CleanupEngine for OllamaCleanupEngine { clean, prepare (=warm_up_async), is_enabled (= model non-empty) }
  ```
- `clean`: chat with a 20 s timeout → `strip_think` → trim. An empty result → `Err(CleanupError::..)` ("empty cleanup output"). On success, `health={ready:true,error:None}`. On error, `health={ready:false,error:Some(msg)}` and fire `warm_up_async()`.
- Error mapping uses an existing `CleanupError` variant (check `whisper-core/src/error.rs`). Add one only if none fits.

- [ ] **Step 1: Failing tests** (unit, mock server via a test helper or by pointing the client at a mock):
  - `messages_for_has_system_fewshot_then_user`: len == 1 + 2*8 + 1; `[0].role=="system"`; the last is the user with the Czech input; the trap few-shots are positions 13-16 (the last two pairs).
  - `clean_returns_stripped_content`: the mock content `"<think>\n\n</think>\n\nPřijdu ve čtyři hodiny."` → `"Přijdu ve čtyři hodiny."`, health ready.
  - `empty_output_is_error`: the mock content `"<think>x</think>"` → `Err`, health not ready with an error.
  - `disabled_when_model_empty`: model handle `""` → `is_enabled()==false`.
  - `model_handle_swap_is_used_next_call`: write a new tag into the `Arc<RwLock<String>>`; the mock asserts the second request's `model` field.
  - Rewrite `cleanup_trap_safety.rs` (`#[ignore]`): model from env `WHISPER_CLEANUP_MODEL` (default `qwen3-vl:8b-instruct`), URL from `WHISPER_OLLAMA_URL` (default the localhost URL). Keep the existing heuristics and asserts (0 trap failures, 0 diacritics lost), and update the doc comment to D-0017.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement**; remove llama-cpp-2.
- [ ] **Step 4: Run** `rtk cargo build --workspace` (confirm no cmake/llama build in the output) and `rtk cargo test --workspace` → PASS. Then the gate:
  - `WHISPER_CLEANUP_MODEL=qwen3-vl:8b-instruct rtk cargo test -p whisper-iface --test cleanup_trap_safety -- --ignored --nocapture`
  - the same with `qwen3:8b`.
  Paste both SUMMARY lines into the task report.
- [ ] **Step 5: Commit** `feat(iface): OllamaCleanupEngine replaces in-process llama.cpp (D-0017)`

### Task 5: Daemon wiring — engine builder, live status, runtime model swap, `/v1/ollama`

**Files:**
- Create: `crates/whisper-iface/src/daemon/ollama.rs` (probe + start)
- Modify: `crates/whisper-iface/src/daemon/mod.rs` (`production_engine_builder*` ~332-395, router ~122-146)
- Modify: `crates/whisper-iface/src/daemon/worker.rs` (`BuildCtx` ~38, `ResidentEngines` ~50, `update_status_ready` ~131)
- Modify: `crates/whisper-iface/src/daemon/state.rs` (`AppState` ~175: add `cleanup_model: Arc<RwLock<String>>`, `cleanup_health: Arc<RwLock<CleanupHealth>>`)
- Modify: `crates/whisper-iface/src/daemon/handlers.rs` (`status` ~105, `put_config` ~350, new handlers)
- Modify: `crates/whisper-iface/src/daemon/api.rs` (`OllamaStatusDto`)
- Modify: `crates/whisper-iface/src/bin/whisperd.rs` (~44-64: pass `config.cleanup`, call auto-start)
- Modify: `crates/whisper-iface/src/daemon/platform.rs` (`platform_engine_builder` signature)
- Test: `crates/whisper-iface/tests/daemon_contract.rs`, unit tests in `daemon/ollama.rs`

**Interfaces:**
- Consumes: Tasks 2-4.
- Produces:
  - `production_engine_builder(stt_path: PathBuf, cleanup: CleanupConfig) -> EngineBuilder`. The same change applies to `_with_sink` and `platform_engine_builder`.
  - `BuildCtx` gains `cleanup_model: Arc<RwLock<String>>, cleanup_health: Arc<RwLock<CleanupHealth>>`.
  - The engine is **always constructed** (even when Ollama is down); `warm_up_async()` is called once at build. `is_enabled()` covers the "Off" case.
  - `status` handler: `cleanup.ready/error` from `cleanup_health`; `cleanup.model` = the current tag, or `None` when empty. The phase is `Degraded` when the model is enabled and `!ready` with an error.
  - `put_config`: if `new.cleanup.model != cur.cleanup.model`, write the shared handle and push `"cleanup.model"` to `applied`. `ollama_url`/`keep_alive` changes → `staged` `"cleanup"`.
  - `daemon/ollama.rs`:
    ```rust
    #[derive(Debug, Clone, PartialEq, Eq)] pub struct Probe { pub installed: bool, pub running: bool, pub version: Option<String> }
    pub fn find_installation(path_env: Option<&str>, exists: impl Fn(&Path)->bool) -> Option<Installation>; // enum Installation { Brew(PathBuf), App, Binary(PathBuf) }
    pub fn probe(client: &OllamaClient) -> Probe;
    pub fn start(inst: &Installation, client: &OllamaClient) -> Result<(), String>; // order per Global Constraints, then ≤10 s poll
    ```
  - Routes:
    - `GET /v1/ollama` → `OllamaStatusDto { installed, running, version: Option<String>, url, can_start: bool }`, where `can_start` = installed && !running && loopback.
    - `POST /v1/ollama/start` → 202 and spawn_blocking start, or 409 when `!can_start`. Emit WS `ollama.status` with the same fields when done.
  - `whisperd` startup: if `cleanup.enabled()` && loopback && installed && !running → start in the background (non-blocking).

- [ ] **Step 1: Failing tests:**
  - `find_installation_prefers_brew` / `_app` / `_binary_on_path` / `_none`, with injected `exists` + PATH strings (`/opt/homebrew/bin/ollama` → Brew).
  - `daemon_contract.rs` (using a fake engine builder):
    - `get_ollama_reports_not_running_for_dead_url` (cleanup URL → closed port) → `running:false`.
    - `put_config_cleanup_model_applies_at_runtime`: PUT with `cleanup.model="qwen3:8b"` → `applied` contains `"cleanup.model"`, and the shared handle reads `qwen3:8b`. PUT `""` → the status shows `cleanup.model: null`.
    - `status_reflects_last_cleanup_failure`: set `cleanup_health` to `{ready:false,error:Some("model 'x' not found")}` → `/v1/status` cleanup.error equals it.
  - api.rs JSON fixture for `OllamaStatusDto` and the `ollama.status` event.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the workspace tests + clippy → PASS.
- [ ] **Step 5: Commit** `feat(daemon): Ollama-backed cleanup wiring, live status, /v1/ollama probe+start`

### Task 6: Model catalog — Ollama entries, tags-based presence, pull download

**Files:**
- Modify: `crates/whisper-iface/src/daemon/models.rs` (catalog ~60-95, `ManagedModel`, `run_download`)
- Modify: `crates/whisper-iface/src/daemon/api.rs` (`ModelEntryDto` ~223)
- Modify: `crates/whisper-iface/src/daemon/handlers.rs` (`list_models` ~470, `start_model_download` ~505, cancel ~539)
- Test: `crates/whisper-iface/tests/model_provisioning.rs`, unit tests in `models.rs`

**Interfaces:**
- Consumes: Task 3 `OllamaClient::{tags,pull}`, `normalize_tag`; `AppState.config.cleanup.ollama_url`.
- Produces:
  - `ManagedModel` gains `source: ModelSource` (`enum ModelSource { File { filename, url, size_bytes, sha256 }, Ollama { tag, size_hint } }`, or equivalent flat fields; builder's call), plus `validated: bool`.
  - Catalog:
    - `stt-large-v3-turbo` (unchanged, File);
    - `cleanup-qwen3-vl-8b-instruct` (Ollama, tag `qwen3-vl:8b-instruct`, size_hint 6_200_000_000, validated);
    - `cleanup-qwen3-8b` (Ollama, tag `qwen3:8b`, size_hint 5_200_000_000, validated).
    The GGUF entry is removed.
  - `ModelEntryDto` (additive):
    - `source: "file"|"ollama"`, `ollama_tag: Option<String>`, `validated: bool`;
    - `filename: Option<String>`, `path: Option<String>`;
    - `selected: bool` (true when `normalize_tag(ollama_tag)==normalize_tag(config.cleanup.model)`).
  - `list_models`:
    - Ollama presence = the tag in `tags()`. If Ollama is unreachable, `present:false` for Ollama entries (no error status).
    - Then append every installed tag not in the catalog as `id="ollama:<tag>"`, `kind="cleanup"`, `validated:false`, `required:false`, `present:true`, `size_bytes`=the reported size.
    - Skip models whose `families` contain only embedding families (name contains `embed`) — the builder may refine this.
  - Download for an Ollama entry: `spawn_blocking` → `client.pull(tag, &handle.cancel, ...)`, mapping progress to `DownloadProgress{downloaded:completed,total,phase:Downloading}`, and `verifying` for the status lines containing `verifying`. Use the same `model.download` WS event, `done`/`error` terminals and the registry `end()` in every path.
  - `DELETE /v1/models/{id}/download` cancel works for both kinds. Download for an `ollama:<tag>` id → 409 (already present).

- [ ] **Step 1: Failing tests** (mock Ollama server bound to a port, injected via config `cleanup.ollama_url`):
  - `list_models_marks_ollama_presence_from_tags`: the mock tags = [`qwen3-vl:8b-instruct`, `gemma3:12b`, `embeddinggemma:latest`]. Assert:
    - `cleanup-qwen3-vl-8b-instruct` present, validated, selected;
    - `cleanup-qwen3-8b` not present;
    - one extra entry `ollama:gemma3:12b` with `validated:false`;
    - no embedding entry;
    - the STT entry unchanged, with `source:"file"`.
  - `list_models_when_ollama_down`: Ollama entries `present:false`, still 200.
  - `ollama_download_maps_pull_progress_and_done`: WS events in order: downloading(50/100) → done.
  - `pull_error_line_maps_to_error_phase`: the error message is carried, the registry is freed (a second POST → 202 not 409).
  - ux mirror fixture test in Task 7 (not here).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the workspace tests + clippy → PASS.
- [ ] **Step 5: Commit** `feat(models): Ollama-sourced cleanup models (tags presence, pull download)`

### Task 7: Tray — API mirror, Ollama card, onboarding step, banner

**Files:**
- Modify: `crates/whisper-ux/src/api.rs` (`ModelEntry`, `Config` from Task 2, new `OllamaStatus`, `ApiEvent::OllamaStatus`)
- Modify: `crates/whisper-ux/src/client.rs` (trait `Api` + `HttpClient`: `ollama_status()`, `start_ollama()`)
- Create: `crates/whisper-ux/src/ollama_ui.rs` (GUI-free state like `permissions_ui.rs`)
- Modify: `crates/whisper-ux/src/models_ui.rs` (helpers for the cleanup lists), `onboarding.rs` (new step), `gui.rs` (Settings card, onboarding step, banner), `lib.rs`
- Test: unit tests in `api.rs`, `ollama_ui.rs`, `models_ui.rs`, `onboarding.rs`

**Interfaces:**
- Consumes: the `/v1` shapes from Tasks 2, 5 and 6 (mirror them exactly; unknown fields `#[serde(default)]`).
- Produces:
  - `OllamaPanel { status: Option<OllamaStatus>, last_error: Option<String> }`, with `refresh(&mut self, api:&dyn Api)`, `needs_attention(&self, cleanup_model:&str)->bool` and `apply_event(&mut self, ev:&ApiEvent)`.
  - `ModelsPanel::cleanup_validated(&self)->Vec<&ModelEntry>`, `cleanup_unvalidated(&self)->Vec<&ModelEntry>`, `selected_cleanup(&self)->Option<&ModelEntry>`.
  - `OnboardingState.ollama_ready: bool` (`#[serde(default)]`; included in `all_steps_done`). It latches when Ollama is running and the selected model is present, **or** when the user chose Off.
  - UI copy (exact):
    - card title **"Text cleanup (Ollama)"**;
    - status strings "Ollama not installed", "Ollama installed — not running", "Ollama running (v{version})";
    - buttons "Install Ollama" (opens `https://ollama.com/download` via the existing `open` helper in `permissions_ui.rs`), "Start", "Download", "Cancel", "Use";
    - hint label `brew install ollama` (selectable text);
    - section header "Installed in Ollama — not validated for Czech";
    - option "Off (no cleanup)".
  - "Use" = GET config → set `cleanup.model` → PUT config. "Off" sets `""`.
  - Banner (main window, same placement/pattern as the D-0016 permission banner, hidden during onboarding) when `needs_attention`. Its text is one of: "Text cleanup needs Ollama — not installed.", "Text cleanup: Ollama is not running.", or "Text cleanup: model {tag} is not downloaded.", each with the matching button.
  - The poll cadence is the existing 5 s poll (add `ollama_status` to it).

- [ ] **Step 1: Failing tests:**
  - api.rs: deserialize the exact JSON produced by Task 6's `list_models` fixture (an Ollama entry with `filename:null`) and Task 5's `OllamaStatusDto`. Assert an old-daemon payload without the new fields still deserializes.
  - `ollama_ui`:
    - `needs_attention_when_not_installed`;
    - `needs_attention_false_when_model_off` (`cleanup_model==""`);
    - `needs_attention_when_running_but_model_missing` (uses ModelsPanel state).
  - `models_ui`: validated vs unvalidated split and `selected_cleanup` from a fixture.
  - onboarding:
    - `all_steps_done_requires_ollama_ready`;
    - an old state file without `ollama_ready` loads with `false`.
- [ ] **Step 2: Run** `rtk cargo test -p whisper-ux` → FAIL.
- [ ] **Step 3: Implement** the state + GUI. Keep egui code thin and logic in the GUI-free modules.
- [ ] **Step 4: Run** the workspace tests + clippy → PASS. `rtk cargo build -p whisper-ux` succeeds.
- [ ] **Step 5: Commit** `feat(ux): Ollama cleanup card, onboarding step, attention banner`

### Task 8: Scripts + user docs

**Files:**
- Modify: `scripts/run.sh`, `scripts/package.sh` (warn if `command -v ollama` fails; `--link-models` links only the STT model), `docs/install.md` (Ollama prerequisite: `brew install ollama && brew services start ollama`; the default model `qwen3-vl:8b-instruct` shared with other apps; the old `models/Qwen3-8B-Q4_K_M.gguf` can be deleted), `README.md` if it mentions the GGUF
- Modify: `scripts/bench_llm_cleanup.py`. Add a module-docstring note that its zero-shot prompt is **not** the production prompt; the gate is `cleanup_trap_safety.rs`.

- [ ] **Step 1:** Make the edits.
- [ ] **Step 2:** Run `bash -n scripts/run.sh scripts/package.sh` → no output. Check `PATH=/usr/bin:/bin bash scripts/package.sh --help 2>&1 | grep -i ollama` (or the equivalent dry path) to show the warning.
- [ ] **Step 3: Commit** `docs/scripts: Ollama prerequisite for text cleanup`

(The orchestrator writes ADR D-0017, the ARCHITECTURE/ROADMAP/PROGRESS/HANDOFF/CHANGELOG updates and runs `graphify update .` after the final review. That's not a subagent task.)
