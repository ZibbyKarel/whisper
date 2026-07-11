# Design: In-app model provisioning (D-0015)

**Date:** 2026-07-11 · **Status:** approved (pending user spec review)

## Problem

The app currently assumes the STT and cleanup-LLM model files already exist in the
**developer repo** (`models/`). They reach the runtime only via
`scripts/package.sh --link-models` (a symlink from the repo) or a hand-written
app-support config with absolute paths into the developer workspace. A fresh install
on a machine with **no models** has no in-app way to obtain them, and if a user
deletes the models there is no way to get them back. Settings → Models is just two
raw text-path fields with no presence detection and no download.

**Goal:** the app must assume no models are present. On every launch it checks whether
the required model is present; if not, the user can download it. Downloads go into the
**application data directory**, never the developer repo. Re-download must be possible
at any time (e.g. after accidental deletion).

## Constraints (from the project constitution)

- **Local-first:** models run fully offline. Downloading is a one-time *provisioning*
  step over HTTPS from HuggingFace — not a runtime cloud dependency. This exception is
  recorded in ADR D-0015.
- **Layering / D-0013:** the download capability lives in the **daemon**
  (`whisper-iface`) and is exposed over `/v1`. The tray (`whisper-ux`) and
  `whisper-cli` are pure `/v1` clients. Everything is reachable without a UI.
- **Permissive licenses only:** models are MIT (whisper.cpp) and Apache-2.0 (Qwen3);
  both permissive and locally executable. New crate `sha2` (RustCrypto, MIT/Apache-2.0)
  is the only added dependency — the HTTP download reuses `ureq` (already a workspace
  dep, rustls-based, no OpenSSL).
- **Czech-first:** the pinned SHA-256 of the STT model is byte-identical to the file
  that already passes the Czech STT parity / e2e tests, so provisioning introduces no
  pipeline change and no Czech regression.

## Model catalog (pinned)

Compiled-in static catalog, two entries. Values verified against HuggingFace on
2026-07-11 (`x-linked-etag` = SHA-256; STT checksum also confirmed identical to the
local repo file).

| id | kind | filename | url | size (bytes) | sha256 | required | license |
|----|------|----------|-----|--------------|--------|----------|---------|
| `stt-large-v3-turbo` | stt | `ggml-large-v3-turbo-q5_0.bin` | `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin` | 574041195 | `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` | true | MIT |
| `cleanup-qwen3-8b` | cleanup | `Qwen3-8B-Q4_K_M.gguf` | `https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf` | 5027783488 | `d98cdcbd03e17ce47681435b5150e34c1417f50b5c0019dd560e4882c5745785` | false | Apache-2.0 |

Download target: `<data_dir>/models/<filename>`, where
`data_dir = dirs::config_dir()/whisper-engine` (matches the existing
`resolve_model_paths()` rebase target).

## Architecture

### Core / layering

The download is network I/O and belongs in the **Interface layer** (`whisper-iface`),
not `whisper-core` (which stays pure). Exposed over the existing axum `/v1` HTTP + WS
surface. `whisper-core` is untouched.

### New module: `whisper-iface/src/daemon/models.rs`

- **Catalog:** `ManagedModel { id, kind, filename, url, size_bytes, sha256, required,
  license }` and `fn catalog() -> &'static [ManagedModel]` (the two entries above).
- **Status:** `fn model_status(m, data_dir) -> ModelStatus` where
  `present = target.exists() && metadata.len() == m.size_bytes` (cheap; a truncated
  `.part` or wrong-size file is *not* present). No full checksum on status (5 GB is too
  slow to hash on every launch).
- **Downloader:** runs on `tokio::task::spawn_blocking` using `ureq`:
  stream `GET url` → write to `<target>.part` (fresh each attempt — **no resume in
  this iteration**), emit throttled progress, verify SHA-256 against the catalog, then
  atomically rename `.part` → final. On any error or cancel: delete `.part`.
- **Registry:** an in-flight map `id -> DownloadHandle { cancel: Arc<AtomicBool>,
  progress: watch/shared }` guarding one active download per id, shared via axum state.

### `/v1` endpoints (daemon/api.rs; mirror DTOs in ux/api.rs)

- `GET /v1/models` → `{ models: [ ModelStatusDto ] }` where `ModelStatusDto = { id,
  kind, filename, required, size_bytes, present, path, downloading, progress: {
  downloaded, total } | null }`.
- `POST /v1/models/{id}/download` → `202 Accepted` (starts the task; `409` if already
  downloading, `404` for unknown id).
- `DELETE /v1/models/{id}/download` → cancels an in-flight download (aborts, deletes
  `.part`); `404`/no-op if none.
- **WS event** `model.download`: `{ id, downloaded, total, phase:
  "downloading"|"verifying"|"done"|"error", error?: string }`, pushed on the existing
  WS channel the tray already consumes (alongside waveform / platform.permission).

Contract-mirror discipline (HANDOFF item 8): `ux/api.rs` ↔ `daemon/api.rs` have no
compile-time link — update both, with a shared JSON fixture that fails loudly on drift.

### Config integration

- Config model-path defaults stay **relative** (`models/…`); `resolve_model_paths()`
  already rebases them onto `<data_dir>/models/…` when present, so a freshly downloaded
  model is picked up with no config edit.
- Fix the tray fallback default (`whisper-ux/src/api.rs:143-144`) so it carries no
  developer-repo-specific path (keep relative `models/…`).
- `scripts/package.sh --link-models` stays as a **dev** convenience but is no longer the
  primary provisioning path; the primary path is in-app download.

### Tray UX (`whisper-ux`, egui) — three surfaces, one shared component

A single reusable "model download" widget/state, backed by `GET /v1/models` +
download/cancel endpoints + `model.download` WS events, is rendered in three places:

1. **Startup gate:** on launch the tray calls `GET /v1/models`; if any `required` model
   is missing it shows a blocking "Download models" screen before normal operation.
   This also covers **accidental deletion** — the gate reappears whenever a required
   model goes missing. STT is offered immediately; the optional cleanup LLM has its own
   button with its size shown.
2. **Settings → Models** (rework the Models card in `ui_cleanup_section`): one row per
   catalog model showing name, size, and status (Present ✓ / Missing / Downloading NN%
   / Verifying / Error) with a Download / Re-download / Cancel button. The advanced raw
   text-path fields are **kept but collapsed** (power users pointing at custom paths).
   This is the primary re-download surface.
3. **Onboarding:** a new `models_ready` field on `OnboardingState`, satisfied when the
   required models are present; rendered as a checklist step reusing the same widget.

WS `model.download` events update per-model progress in the tray's UI model.

### Error handling

| Failure | Behaviour |
|---------|-----------|
| Network / non-2xx / TLS error | `phase=error` + message; UI shows error + Retry (fresh download) |
| SHA-256 mismatch | delete `.part`; `phase=error` "integrity check failed"; Retry |
| Disk full / io error | surface the io error string via `phase=error` |
| User cancel | abort task, delete `.part`; status returns to Missing |
| Daemon unreachable | tray's existing "daemon down" state; gate cannot proceed until daemon is up |
| Truncated file from a previous run | detected as **not present** (size mismatch) → download offered |

### Testing (no network in tests)

- **Unit (`whisper-iface`):** catalog integrity (unique ids, well-formed URLs);
  `present` detection (size match; short file = not present); SHA-256 verifier against
  a small good/corrupt fixture (not 5 GB); atomic `.part`→final rename.
- **Download flow:** spawn a tiny local axum/hyper server serving a small file; assert
  progress events, checksum-pass completion, checksum-fail cleanup, and cancel. No real
  network, no large files.
- **API contract:** JSON fixture for `GET /v1/models`; `model.download` WS event shape
  mirrored across `ux/api.rs` ↔ `daemon/api.rs`.
- **Czech:** unchanged — the STT bytes are pinned by SHA-256 to the exact file that
  already passes the Czech parity/e2e suites; no pipeline change.

## Scope explicitly deferred (follow-ups)

- **Resumable downloads** (HTTP Range): not in this iteration; a failed/cancelled
  download restarts from zero.
- Selectable STT accuracy variant (large-v3 vs turbo): out of scope; turbo stays the
  default STT, Qwen3-8B the optional cleanup — matching current defaults.
- Mirror/alternate download hosts: out of scope.

## ADR

D-0015 to be written in `DECISIONS.md` at implementation time (decision-time ADR per
constitution): records sources, pinned checksums/sizes, licenses, daemon-side rationale
(layering/D-0013/CLI reuse), the one-time-network exception to local-first, the `sha2`
dependency, and the relationship to `--link-models`.

## Delegation

Implementation → `sonnet-implementer` (normal feature + UI + tests, per delegation
rules). Review → performed by the orchestrator (reviewer ≠ implementer).
