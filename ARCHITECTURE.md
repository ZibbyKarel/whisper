# Architecture

> Status: Phase 2 design, 2026-07-10. Grounded in `docs/RESEARCH.md` (evidence tags) and
> Phase-1 measurements. Decisions referenced as D-NNNN live in `DECISIONS.md`.

## 1. System overview

A local-first, Czech-first voice dictation **engine** packaged as an embeddable component:
a headless Rust core exposed through a local HTTP/WebSocket interface, with OS specifics
(mic, hotkeys, text insertion) implemented as adapters behind ports. A UI is optional;
every capability is reachable programmatically.

```
┌───────────────────────────────────────────────────────────────┐
│ UX Layer            settings UI · onboarding · viz            │ replaceable, Phase 6
├───────────────────────────────────────────────────────────────┤
│ Interface Layer     HTTP (control) · WebSocket (events) ·     │ crate: whisper-iface
│                     CLI · config                              │ binary: `whisperd`
├───────────────────────────────────────────────────────────────┤
│ Core Engine         session FSM · VAD · STT · cleanup ·       │ crate: whisper-core
│                     dictionary · formatting · history         │ pure lib, no UI/OS deps
├───────────────────────────────────────────────────────────────┤
│ Platform Layer      audio capture · global hotkeys ·          │ crate: whisper-platform
│  (adapters)         text injection · permissions · tray       │ (macOS first)
└───────────────────────────────────────────────────────────────┘
```

Binding rules (from the mission, unchanged): dependencies point downward only; Core runs
headless and fully offline; everything reachable through the Interface Layer without UI;
Czech/UTF-8 correctness end to end.

## 2. Language & process model (D-0006)

**Rust workspace, single daemon process.** Evidence: Handy (MIT) proves the entire
pipeline in Rust — cpal (capture), vad-rs (Silero), whisper.cpp bindings, rdev (hotkeys),
rubato (resampling) — with acceptable latency on Apple Silicon.

- `whisper-core` — pure library. No tokio-main dependency leakage; async internally via
  channels. Compiles on any OS; zero `#[cfg(target_os)]`.
- `whisper-iface` — `whisperd` binary: loads core, binds `127.0.0.1` HTTP + WS, reads
  config, hosts plugins. This is the embeddable artifact.
- `whisper-platform` — macOS adapter crate implementing core's ports (mic via cpal,
  hotkeys via rdev/CGEventTap, insertion via pasteboard). Other OSes later.
- `whisper-cli` — thin client of the HTTP API (also used in tests/CI).

An external orchestrator can run `whisperd` and drive everything over HTTP/WS — the
"personal AI orchestration system" integration path — or link `whisper-core` directly.

## 3. Core Engine

### 3.1 Ports (traits) — the only seams to the outside

```rust
trait AudioSource   { fn start(&self, cfg: AudioCfg) -> AudioStream; }  // mic OR fixture WAV
trait SttEngine     { fn transcribe(&self, pcm: &[f32], opts: SttOpts) -> SttResult; }
trait CleanupEngine { fn clean(&self, text: &str, ctx: CleanupCtx) -> CleanResult; }
trait TextSink      { fn deliver(&self, text: &str) -> DeliverResult; }  // injection OR API return
trait Clock/EventBus…                                                    // testability
```

Production adapters: cpal mic, whisper.cpp (whisper-rs), `OllamaCleanupEngine` (HTTP to the
shared local Ollama server, default `qwen3-vl:8b-instruct`, D-0017), pasteboard-paste. **Adapter placement:** production STT/LLM adapters live in
`whisper-iface` (heavy native deps); `whisper-core` depends only on the traits, keeping
it a genuinely pure lib and CI hardware-free. Test adapters: fixture WAVs, canned STT,
identity cleanup, capture sink. `AudioSource` reports device loss (mic disconnect —
e.g. Bluetooth drop) as a typed error; the session FSM transitions to `Failed` carrying
any partial text, and a `audio.device_lost` event is emitted.
**Everything below is testable without hardware** — CI runs the full pipeline on
`fixtures/audio/` including the Czech set.

### 3.2 Dictation session — data flow

```
hotkey/API start
   │
   ▼
capture (cpal 48k → rubato → 16k mono f32 ring buffer)
   │ frames (30 ms)
   ▼
VAD (Silero, streaming): speech gating + **silence trimming**       [D-0008]
   │ speech segments only — whisper must never see >~1 s of silence
   │ (measured: 3 s trailing silence ⇒ "Titulky vytvořil JohnyX." hallucination)
   ▼
STT (whisper.cpp large-v3-turbo q5_0, -l cs; profiles per D-0004)
   │ **per-VAD-segment decoding**: whisper.cpp is non-streaming, so each VAD-closed
   │ segment is decoded as it completes. This (a) yields segment-granular partial
   │ transcripts (no word-level live partials in v1), (b) bounds end-of-session
   │ latency for long/hands-free sessions to the final segment (~0.8 s), and
   │ (c) bounds PCM buffering to one segment. Cost: whisper loses cross-segment
   │ context — punctuation/casing joins are handled in the formatting pass.
   │ TranscriptRaw {text, lang, segments, timings}
   ▼
dictionary pass (deterministic, rule-based): user vocabulary substitutions
   │ ("merknout"→"mergnout", "packa json"→"package.json") — cheap, always on
   ▼
cleanup gate: skip LLM if no disfluency/self-correction markers detected
   │ (FreeFlow evidence: ~40 % of dictations need no cleanup; saves 1–2 s)
   │ marker set is a Phase-3 deliverable, tuned on real recordings; note that
   │ self-correction handling is UNVALIDATED until LLM round 2 (few-shot prompt)
   ▼
LLM cleanup (local Ollama `/api/chat`, qwen3-vl:8b-instruct, few-shot anti-paraphrase prompt) — OPTIONAL per profile
   │ any engine error/timeout (20 s) → dictionary text delivered, skip reason `engine_failed`
   │ trap-safety contract: dictated questions/commands are transcribed, never answered
   ▼
TextFinal event → TextSink (platform injection) + WS event + history
```

Cancellation: every stage checks a session-scoped `CancellationToken`; hotkey release
modes (push-to-talk vs toggle) and API `POST /sessions/{id}/stop|cancel` feed the same
FSM: `Idle → Recording → Transcribing → Cleaning → Delivering → Idle`, with `Cancelled`
and `Failed(err)` exits. Errors emit structured WS events; the engine never dies with a
session — a failed stage returns the best text so far (raw transcript beats nothing).

Model lifecycle failures are first-class: if the STT model fails to load (missing GGUF,
OOM), the engine emits `model.load_failed`, stays up, and rejects sessions with a
structured error; if only the cleanup LLM fails (Ollama missing/stopped, model not pulled), the engine
degrades to cleanup-off and says so in `/v1/status` (phase `degraded`); STT keeps working. Concurrency: **one active dictation session per engine**
(`409 Conflict` on start-while-busy); `/v1/transcribe` one-shots queue behind the active
session for the shared STT/LLM resources (bounded queue, structured busy error beyond it).

### 3.3 Czech/UTF-8 contract

- All text is Rust `String` (UTF-8) end to end; NFC-normalized at STT output boundary.
- Diacritics are load-bearing: tests assert byte-exact Czech output on the fixture set.
- Code-switching: single STT pass with `-l cs` (measured best); dictionary + cleanup
  handle Czech-inflected dev vocabulary. No per-language routing in v1 (Parakeet's
  code-switching collapse rules out engine-per-language until a router exists).

### 3.4 Latency budget (release-to-text: ≤ 1.5 s when cleanup is skipped; ~2 s when the LLM runs)

Default profile: cleanup **on with skip-gate** — the gate exists precisely to keep the
median case on the fast path; when the LLM does run, the extra ~1.1 s (measured in-process,
release) buys the measured cleanup quality. Per-segment decoding overlaps STT with recording,
so for multi-segment (long/hands-free) sessions only the *final* segment's decode lands after
release.

| Stage | Budget | Evidence |
|---|---|---|
| VAD trim + handoff | ~0 ms (streaming, overlaps recording) | Silero real-time |
| STT turbo q5_0, final segment ≤10 s | ~0.8 s | measured round 1 |
| earlier segments | 0 ms at release (decoded during recording) | design (§3.2) |
| dictionary pass | <1 ms | string ops |
| cleanup gate: skipped | 0 ms (est. ~40 % of cases per FreeFlow) | |
| cleanup LLM (when it runs) | ~1.3 s (Ollama on localhost, `qwen3-vl:8b-instruct`, model kept warm via `keep_alive`) | D-0017 gate |
| injection (paste) | ~50 ms | |

Mitigations: segment-granular partials over WS (UX shows text as segments close), models
kept resident between sessions (turbo 547 MB + qwen3 5.2 GB ≈ 5.8 GB, inside D-0001's
10 GB budget). Per-segment decode does add total compute during recording (each segment
decoded once) — acceptable at turbo speeds (~0.1× realtime).

## 4. Interface Layer

Local-only by default (`127.0.0.1`, token auth optional). JSON everywhere.

- `GET  /v1/health`, `GET /v1/status` — engine, models, profile.
- `POST /v1/sessions` / `POST /v1/sessions/{id}/stop` / `…/cancel` — dictation lifecycle
  (audio from mic port, or `audio_b64`/file for headless use).
- `POST /v1/transcribe` — one-shot: audio in → final text out (the embeddable primitive).
- `GET/PUT /v1/config`, `GET/PUT /v1/dictionary` — runtime configuration.
- `GET /v1/ollama` (installed/running/version/can_start), `POST /v1/ollama/start` — the
  daemon probes and (loopback only) starts the shared Ollama server; the tray never talks
  to Ollama directly (D-0017).
- `WS /v1/events` — `session.started|partial_transcript|raw_transcript|cleaned|delivered|
  cancelled|failed`, `secure_input.blocked`, `audio.device_lost`,
  `model.loading|ready|load_failed`, `model.download`, `ollama.status`.
  Events are best-effort; the authoritative final
  result of any session is persisted and retrievable via `GET /v1/sessions/{id}` — a WS
  client disconnecting mid-session does not cancel the session or lose the result.
- Plugins/processing injection (Phase 4, not MVP): an orchestrator may register a
  WS-driven hook stage (`transform` request/response, hard timeout + fallthrough to
  unmodified text) between cleanup and delivery; it gets its own FSM state and latency
  line when implemented.
- CLI: `whisper-cli transcribe file.wav`, `whisper-cli listen`, `whisper-cli config …`.

## 5. Platform Layer (macOS first)

- **Capture:** cpal (CoreAudio). Mic permission surfaced via a `platform.permission`
  event, never a blocking dialog inside core.
- **Hotkeys:** configurable global hotkeys (rdev path, Handy pattern), push-to-talk
  (hold) and toggle modes. **Fn/globe-key capture (Wispr Flow's default) is NOT
  achievable via rdev's keyboard events** — it needs an NSEvent `flagsChanged`/IOKit HID
  path; that is a separate Phase-5 work item behind the same hotkey port. rdev's
  maintenance status is a known risk (D-0007).
- **Insertion (D-0007):** pasteboard + synthesized Cmd-V with clipboard save/restore
  (convergent choice of Handy/OpenWhispr/VocaMac; per-keystroke CGEvent rejected —
  unreliable at speed and diacritics require UCKeyTranslate against the live layout).
  Restore happens only after a paste-completion signal or a bounded delay (never before
  the target app reads the pasteboard); clipboard read-back is NFC-normalized to protect
  Czech diacritics. Secure Input: detect, emit `secure_input.blocked` naming the owning
  app, fall back to clipboard-only. Never bypass.
- **Tray/menu-bar & UX:** Phase 5/6; talks only to the Interface Layer.

## 6. Storage

- Config: TOML file (`~/.config/whisper-engine/config.toml`), hot-reloadable.
- Dictionary: user vocabulary as versioned JSON (exportable; the round-2 measured
  substitution pairs seed it).
- History: SQLite (rusqlite, single local file), opt-in, prunable — Phase 4+.

## 7. Testing strategy hooks

Per `.claude/skills/testing-strategy`: unit tests per module; pipeline integration tests
run headless on `fixtures/audio/` (synthetic + `real/`); Czech assertions are byte-exact;
benchmark harnesses (`scripts/bench_*.py`) stay as regression tools for model upgrades;
trap-case suite guards the cleanup prompt against "answered instead of transcribed"
regressions.

## 8. Out of scope (v1)

Cloud anything; per-app context awareness via screenshots (privacy-hostile — revisit as
opt-in local OCR later); command mode ("delete that sentence"); Windows/Linux platform
adapters (ports designed for them, not implemented); model auto-download UI; word-level
live partial transcripts (v1 partials are per-VAD-segment); the plugin transform hook
(Phase 4); Fn/globe-key hotkey (Phase 5, dedicated `flagsChanged`/IOKit path).
