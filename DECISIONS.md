# Decisions (ADR log)

Format defined in `.claude/skills/architecture-adr/SKILL.md`. Never renumber; supersede instead.

## D-0001: Target hardware budget is Apple M5 / 24 GB unified memory
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Model selection must fit the machine that runs the engine offline in real time. Measured: Apple M5, 10-core CPU, 10-core GPU (Metal 4), 24 GB unified memory, macOS 27.
- **Decision:** Budget ≤ ~10 GB resident model memory: Whisper large-v3 tier (~3 GB quantized) for Czech-quality STT plus a quantized 7–12 B LLM (~5–8 GB Q4) for optional cleanup, concurrently.
- **Alternatives:** Smaller STT (medium/small) — rejected as default because Czech accuracy degrades noticeably below large-v3; may still be offered as a low-latency profile after measurement.
- **Consequences:** Czech quality achievable without cloud. Latency vs. quality profiles must be measured in Phase 1, not assumed.

## D-0002: Author missing engineering skills locally instead of installing vendor plugin bundles
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Mission Phase 0.2 requires skills for architecture/ADR, system-design, testing-strategy, code-review, debug, documentation, deploy-checklist, tech-debt. Marketplace search (official + netresearch) found no focused, well-maintained plugins for the first and last four without pulling large vendor bundles; code-review and debugging are already covered by installed superpowers 6.1.1 and built-in /code-review.
- **Decision:** Author project-local skills under `.claude/skills/` (architecture-adr, system-design, testing-strategy, documentation, deploy-checklist, tech-debt); rely on superpowers + built-ins for review/debug/TDD/planning.
- **Alternatives:** Installing multi-skill vendor plugins (aws-*, forge-*, etc.) — rejected: wrong domain, maintenance risk, context bloat.
- **Consequences:** Skills are versioned with the repo (reproducible, project-tailored); we maintain them ourselves.

## D-0003: Runtime toolchain — Homebrew whisper-cpp + llama.cpp + Ollama; uv for Python; pnpm for Node
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Phase 0.4 requires local STT/LLM runtimes installed before research so candidate models can be measured, not guessed. All are MIT-licensed, offline-capable, Metal-accelerated, actively maintained.
- **Decision:** Install whisper-cpp (STT, Metal), llama.cpp + Ollama (LLM serving/eval), ffmpeg (audio conversion), uv (Python env for faster-whisper comparison), alongside existing Node 25/pnpm 10.
- **Alternatives:** MLX-whisper (Apple MLX) — deferred to Phase 1 benchmarking, not excluded. Cloud STT — forbidden by mission. Docker-based runtimes — needless overhead on native macOS.
- **Consequences:** Phase 1 can benchmark real Czech samples immediately. Final engine embeds a subset of these; Ollama may remain dev-only (ADR to follow in Phase 2).

## D-0004: STT default is whisper.cpp large-v3-turbo q5_0 (`-l cs`), with quality/latency profiles
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Czech + code-switching quality is the hard requirement; ≤10 GB model budget (D-0001). Czech evidence, all measured on our fixtures + one real dictation (docs/stt-benchmark.md, -round2.md, stt-real-validation.md): ITN-aware cs WER — large-v3 0.11, turbo 0.17, mikr fine-tune 0.14, Parakeet 0.11; real-mic dev dictation: turbo near-perfect incl. diacritics and EN tech terms; mikr regressed ("refakturovat", "jason payload"); Parakeet collapsed on code-switching incl. a negation inversion.
- **Decision:** Default model: **ggml large-v3-turbo q5_0 (547 MB, ~0.8 s/clip)** via whisper.cpp with `-l cs`. Profiles: `max-quality` = large-v3 q5_0 (1 031 MB); `low-latency` (monolingual only, later) = Parakeet TDT 0.6B v3 int8 via sherpa-onnx.
- **Alternatives:** mikr/whisper-large-v3-czech-cv13 (Apache-2.0) — rejected as default: no speed/memory win over large-v3, lost number-ITN, real-speech regressions. Parakeet as default — rejected: worst code-switching (mix WER 0.38), meaning-inverting errors on real speech. faster-whisper — rejected: slowest runtime on Apple Silicon (verified). medium/small — rejected round 1: measurable Czech diacritic corruption.
- **License/local:** whisper.cpp MIT, Whisper weights MIT; fully offline; Metal-accelerated. Low-latency profile deps: sherpa-onnx Apache-2.0; **Parakeet v3 weights license must be verified before that profile is adopted** (NVIDIA model licenses are not uniformly permissive).
- **Consequences:** 547 MB resident STT leaves room for the cleanup LLM. Model upgrades must re-run `scripts/bench_stt.py` + real-mic check before swap. Evidence caveat: turbo-vs-large-v3 gap (cs 0.17 vs 0.11) rests on n≈6 synthetic clips + n=1 real recording — re-check when more real recordings exist (Phase 3 test corpus).

## D-0005: Cleanup LLM is qwen3:8b via llama.cpp, behind a skip-gate; cleanup is optional
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Cleanup stage = disfluency removal, proper-name casing, custom vocabulary — and must never answer/act on dictated content. Czech evidence, measured (docs/llm-cleanup-benchmark.md): qwen3:8b avg WER 0.15, 0/4 trap failures, 1.6 s, 5.2 GB; gemma3:12b leaked refusal commentary into output; mistral-nemo:12b obeyed a prompt injection and answered an arithmetic trap despite BenCzechMark's Czech-NLU lead.
- **Decision:** **qwen3:8b (Apache-2.0) served in-process via llama.cpp** (GGUF, `think:false`); deterministic dictionary pass runs first; a heuristic skip-gate bypasses the LLM when no disfluency/self-correction markers are present (FreeFlow evidence ~40 % skippable). Ollama remains a dev/benchmark tool only, not a runtime dependency.
- **Alternatives:** gemma3:12b — rejected: trap leak, 2× latency, +3 GB. mistral-nemo:12b — rejected: trap failures (disqualifying for dictation). No LLM at all — kept as a supported profile (cleanup is optional); rule-based dictionary alone already fixes the measured vocab errors.
- **License/local:** Qwen3 Apache-2.0; llama.cpp MIT; fully offline; ~5.2 GB Q4 resident → total with STT ≈ 5.8 GB, inside D-0001.
- **Consequences:** Phase-3 must implement the trap-safety contract as tests (existing `fixtures/llm/cleanup-cases.json`). Round 2 (few-shot anti-paraphrase prompt, qwen3:4b latency trial) scheduled before the prompt is frozen. **Evidence-transfer guard:** all round-1 numbers (0/4 traps, 0.15 WER, 1.6 s) were measured on Ollama with a generic zero-shot prompt — trap-safety and latency MUST be re-measured in-process on llama.cpp with the final prompt (chat template, sampling and `think:false` handling differ) before the cleanup stage ships; the 1.6 s figure is provisional (symmetric to D-0006's whisper-rs↔whisper-cli parity requirement).

## D-0006: Core engine in Rust; single `whisperd` daemon; workspace of 4 crates
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Core must be embeddable, headless, offline, cross-platform, with real-time audio (no GC pauses) and native bindings to whisper.cpp/llama.cpp. Handy (MIT) proves the exact stack in Rust: cpal, vad-rs (Silero), whisper bindings, rdev, rubato.
- **Decision:** Rust workspace: `whisper-core` (pure lib, ports/adapters), `whisper-iface` (`whisperd` HTTP+WS+CLI host), `whisper-platform` (macOS adapters), `whisper-cli`. Single daemon process; orchestrators integrate via localhost API or link the core crate.
- **Alternatives:** Swift — rejected: macOS-locked core violates layering. Electron/TS (OpenWhispr pattern) — rejected: RAM/latency cost, poor embedding story. Python — rejected: packaging + real-time audio. Tauri app-first (Handy) — rejected for v1: we build an engine, not an app; UI comes later on top of the API.
- **License/local:** load-bearing deps (verify each at adoption time in Cargo.toml review): whisper.cpp MIT · llama.cpp MIT · cpal Apache-2.0 · rubato MIT · rdev MIT · vad-rs MIT · Silero VAD model MIT · rusqlite MIT. All offline.
- **Consequences:** UI-less MVP is testable in CI with fixture audio. Bindings choice (whisper-rs vs cpp FFI) is an implementation detail owned by Phase 3, benchmarked against `whisper-cli` output for parity. rdev's maintenance status is a watched risk (fallback: direct CGEventTap in whisper-platform); Fn/globe-key capture is out of rdev's scope and needs a dedicated `flagsChanged`/IOKit path (Phase 5).

## D-0007: Text insertion via pasteboard + synthesized Cmd-V; Secure Input detected, never bypassed
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** macOS has no official cross-app text-insertion API. Per-keystroke CGEvent is unreliable at speed (~100 ms/keystroke guidance) and Czech diacritics need UCKeyTranslate against the live keyboard layout. Handy, OpenWhispr and VocaMac all converge on paste-based insertion. Secure Input blocks injection and can wedge system-wide (espanso docs; PLAUSIBLE tag — verify during Phase-5 implementation).
- **Decision:** Save clipboard → set text → synthesize Cmd-V → restore clipboard. On Secure Input: emit `secure_input.blocked` event naming the owning app and leave text on clipboard; never attempt bypass.
- **Alternatives:** CGEvent per-keystroke typing — rejected as primary (speed, diacritics); may return as opt-in fallback for paste-hostile apps. Accessibility-API direct insertion — deferred: per-app reliability unknown, revisit in Phase 5 with per-app strategy table.
- **Consequences:** Requires Accessibility permission; clipboard race conditions must be tested (focus changes between stop-record and paste). Restore only after a paste-completion signal or bounded delay; NFC-normalize clipboard read-back (macOS pasteboard can hand back NFD, corrupting Czech diacritics round-trip). Phase-5 must add the Secure-Input detection path — note the espanso-sourced Secure-Input behavior is still [PLAUSIBLE], verify during implementation.

## D-0008: Silero VAD with mandatory silence-trimming before STT
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Measured on the user's real recording: 3 s of trailing silence made every whisper model hallucinate "Titulky vytvořil JohnyX."; trimming the silence removed it. Silero VAD is the de-facto local standard (Handy ships it via vad-rs; MIT).
- **Decision:** Streaming Silero VAD gates capture: whisper receives speech segments only, never >~1 s of contiguous silence; VAD also provides utterance segmentation for streaming partials.
- **Alternatives:** No VAD / energy threshold — rejected: hallucination measured, and Whisper wastes compute on silence. Parakeet-style silence-robust model — not applicable to the whisper default.
- **Consequences:** Anti-hallucination test joins the fixture suite (silence-padded clips must transcribe identically to trimmed ones). VAD params (thresholds, hangover) need tuning on real recordings in Phase 3. Implementation note (2026-07-10): shipped via `voice_activity_detector` 0.2.1 (MIT, Silero V5 bundled — no runtime network); its `ort` dependency downloads the onnxruntime binary at **build** time (cached) — vendor it if fully air-gapped builds become a requirement.

## D-0009: Interface = localhost HTTP (control) + WebSocket (events) with JSON; CLI as client
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** External orchestrators must start/stop sessions, receive live events, inject processing steps, and run the engine headless — without linking Rust.
- **Decision:** `127.0.0.1`-bound HTTP `/v1/*` (sessions, transcribe, config, dictionary) + `WS /v1/events` (session lifecycle, partial transcripts, secure-input, model state); optional bearer token; plugin hook = WS-registered transform stage with timeout + fallthrough. CLI is a thin HTTP client, so CLI parity is free.
- **Alternatives:** gRPC — rejected: heavier toolchain for consumers, no browser/UX-layer story. Unix socket only — rejected: WS needed for the future UX layer anyway; HTTP adds negligible risk on loopback. stdin/stdout JSON-RPC — kept possible later via a `--stdio` flag (cheap on top of the same core API), not v1.
- **Consequences:** API schema versioned under `/v1`; contract tests drive it with fixture audio in CI (no mic needed).

## D-0010: Storage — TOML config, JSON dictionary, opt-in SQLite history
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Local-first; user data must be inspectable, portable and diff-able; history is privacy-sensitive.
- **Decision:** Config `config.toml` (hot-reload); user dictionary as versioned JSON (seeded with measured substitution pairs); transcript history in local SQLite (rusqlite, MIT), strictly opt-in and prunable.
- **Alternatives:** SQLite for everything — rejected: config/dictionary lose diff-ability and hand-editability. better-sqlite3/Node patterns — N/A in Rust core.
- **Consequences:** Dictionary format becomes a public contract of the Interface Layer (GET/PUT /v1/dictionary); migration policy needed once schema changes.

## D-0011: whisperd async runtime & web stack — tokio + axum
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** D-0009 fixes the contract (localhost HTTP `/v1/*` + `WS /v1/events`, JSON). Phase 4 needs a concrete server stack. Constraints: local-only, permissive licenses, native WS support, easy bridging from the synchronous `PipelineEvent` mpsc channel, and a home for the `!Send` `LlamaCleanupEngine` (persistent LlamaContext) — engines must be pinned to a dedicated worker thread, not shared across the async pool.
- **Decision:** tokio (MIT) + axum (MIT, WS built in) + tower (MIT); serde/serde_json (MIT/Apache-2.0) for the JSON schema. Engines live on a dedicated blocking worker thread owning the Pipeline; the axum side talks to it via command channel + broadcast events (actor pattern), satisfying the one-active-session + bounded-queue + structured-busy-error semantics of §3.
- **Alternatives:** actix-web — rejected: heavier actor framework on top of its own patterns; axum is thinner over hyper and maintained in the tokio org. warp — rejected: filter-combinator API harder to keep readable; axum superseded it in ecosystem momentum. tiny_http/std threads + tungstenite — rejected: hand-rolled WS fan-out and graceful shutdown would re-implement half of tokio for no dependency savings that matter locally.
- **Consequences:** whisper-iface gains a tokio dependency (build-time cost only for consumers of the daemon; whisper-core stays runtime-free). All licenses MIT/Apache-2.0, fully offline. The actor seam doubles as the place a future plugin/transform hook (D-0009) plugs in.

## D-0012: Phase-5 platform adapters compose into whisperd behind a `platform` feature
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Phase 5 needs live mic capture, a global hotkey, and paste injection reaching the pipeline. Layering (constitution, §2/§5): whisper-platform implements Core ports; Core and the daemon's contract stay OS-free. macOS TCC permissions (mic, accessibility, input monitoring) attach to the *process* doing the work.
- **Decision:** whisper-platform implements the ports — `CpalAudioSource` (CoreAudio, resample→16 kHz mono), `PasteboardSink` (D-0007 pasteboard+Cmd-V, save/restore, NFC read-back, Secure Input detect-never-bypass), rdev hotkey listener (push-to-talk + toggle) — and `whisperd` composes them behind an off-by-default cargo feature `platform` (the binary is the composition root). Live sessions via `POST /v1/sessions {"source":"mic"}`; the hotkey listener runs inside whisperd and drives the same session machinery; delivery sink per config (`paste`|`none`). Fn/globe capture stays a separate work item (NSEvent flagsChanged/IOKit) behind the same hotkey port.
- **Alternatives:** separate hotkey/paste helper binary over HTTP — rejected: injection needs accessibility permission anyway, a second TCC-holding process doubles the permission surface for nothing. Platform pieces owned by a UI app — rejected: engine must stay embeddable and fully driveable headless (constitution).
- **Consequences:** whisper-iface gains an optional, feature-gated whisper-platform dependency (default build stays headless-pure); all TCC prompts attach to whisperd; CI cannot grant TCC, so hardware paths ship as `#[ignore]` manual tests + a documented verification checklist. Dependency licenses recorded on adoption (cpal Apache-2.0, rdev MIT, clipboard/CGEvent crates MIT/Apache-2.0 — confirm exact set in implementation report).

## D-0013: UX shell — pure-Rust tray app (tray-icon + eframe/egui) over the Interface Layer
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Phase 6 needs a Wispr Flow–like UI: menu-bar presence with recording state, a settings window (hotkey, language, cleanup, delivery, dictionary), first-run permission onboarding. Constitution: UX is a replaceable shell that talks ONLY to the Interface Layer (HTTP/WS `/v1`); local-first; permissive licenses.
- **Decision:** New crate `whisper-ux` with a `whisper-tray` binary: `tray-icon` (MIT/Apache-2.0, tauri org) for the NSStatusItem with state icons (idle/recording/processing), `eframe`/`egui` (MIT/Apache-2.0) for the settings + onboarding window, `tao`/`winit` event loop. All state via `GET/PUT /v1/config`, `/v1/dictionary`, `WS /v1/events` — zero direct engine or platform dependencies, so the shell is deletable/replaceable per the constitution.
- **Alternatives:** Tauri 2 — rejected for MVP: webview + asset pipeline is heavy for a tray-first app; revisit if rich onboarding demands HTML. Native Swift/SwiftUI shell — rejected: second toolchain, breaks single-workspace quality gates; the API-only boundary keeps it possible later. egui-only window without tray — rejected: menu-bar presence is the product's primary surface (Wispr Flow pattern).
- **Consequences:** UI runs as a separate process from whisperd (daemon owns TCC permissions for mic/paste; the tray app needs none beyond notifications), reinforcing the layering. Adds ~1 new dependency tree (winit stack) compiled only for `whisper-ux`. Live input-level metering needs a new `audio.level` WS event from the daemon (added alongside).
