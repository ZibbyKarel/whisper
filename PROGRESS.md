# Progress log

Append-only. Newest at bottom. Absolute dates.

## 2026-07-10 — Phase 0: environment bootstrap
- Capability audit written (`docs/CAPABILITY_AUDIT.md`); hardware budget measured: Apple M5, 24 GB unified memory, Metal 4 (D-0001).
- Local skills authored: architecture-adr, system-design, testing-strategy, documentation, deploy-checklist, tech-debt (D-0002). Review/debug/TDD covered by installed superpowers 6.1.1 + built-ins.
- Subagents defined in `.claude/agents/`: opus-implementer (opus), sonnet-implementer (sonnet), small-worker (haiku).
- Runtimes installed via Homebrew: ffmpeg, uv, whisper-cpp, llama.cpp, ollama (D-0003). Pre-existing: node v25.8.1, pnpm 10.33.0, python 3.12.13.
- Memory files created: ROADMAP, ARCHITECTURE (skeleton), DECISIONS, PROGRESS, HANDOFF, CLAUDE.md constitution section.
- Next: Phase 0.6 verification gate → commit → Phase 1 research.

## 2026-07-10 — Phase 1: Czech STT benchmark, round 1
- Built Czech/English/mixed fixture set in `fixtures/audio/`: 6 synthetic TTS clips (macOS `say`, Zuzana cs_CZ / Samantha en_US, 16 kHz mono) + `manifest.json` with references.
- Wrote WER/CER benchmark harness `scripts/bench_stt.py` (diacritic-preserving normalization) driving whisper.cpp (`whisper-cli`).
- Pulled 4 GGML models into `models/` (gitignored): large-v3 q5_0 (1.0 GB), large-v3-turbo q5_0 (547 MB), medium q5_0 (514 MB), small q5_1 (181 MB).
- Ran round-1 benchmark; raw results `docs/stt-benchmark.json`, write-up `docs/stt-benchmark.md`.
- Finding: **large-v3-turbo q5_0 is the provisional working default** — near-large-v3 Czech quality at ~2× speed, 547 MB. medium/small measurably degrade Czech diacritics and are unsuitable as default. Raw WER overstates real error rate because whisper's inverse-text-normalization (e.g. spoken numbers → digits) is desirable but scores as errors; harness still needs ITN-aware scoring.
- Known gap: fixtures are clean synthetic TTS (upper bound vs. real dictation); real-microphone Czech recordings are still required before the model choice can be locked into an ADR.
- Next: ITN-aware WER scoring, real-mic Czech validation, local LLM cleanup comparison (no Ollama models pulled yet), VAD/capture/macOS text-injection research, `docs/RESEARCH.md`.

## 2026-07-10 — Phase 1: LLM cleanup benchmark harness
- Built `fixtures/llm/cleanup-cases.json` (13 cases: cs disfluencies, false starts/self-corrections, proper-name casing, dev-vocab code-switching, 4 trap cases where dictated questions/commands/prompt-injections must be transcribed, never answered) and `scripts/bench_llm_cleanup.py` (stdlib-only Ollama chat client; WER/CER + `answered_instead` + `diacritics_lost` heuristics + latency; writes `docs/llm-cleanup-benchmark.json`). Implemented by sonnet-implementer, reviewed by Fable. py_compile + ruff clean; graceful-skip path exercised; not yet run against real models.
- Ollama daemon started; qwen3:8b, gemma3:12b, mistral-nemo:12b downloading in background.
- Next: run the benchmark once models finish pulling; record results + model pick input for Phase 2.

## 2026-07-10 — Phase 1: LLM cleanup benchmark, round 1 (results)
- Ran `scripts/bench_llm_cleanup.py` on qwen3:8b / gemma3:12b / mistral-nemo:12b (Ollama, temp 0, zero-shot). Write-up `docs/llm-cleanup-benchmark.md`, raw outputs `docs/llm-cleanup-benchmark.json`.
- **qwen3:8b = provisional cleanup default**: avg WER 0.15, 0/4 trap failures, 1.6 s avg, 5.2 GB. gemma3:12b failed 1 trap (leaked refusal commentary into output); **mistral-nemo:12b disqualified** (obeyed prompt-injection, answered arithmetic trap, 1× diacritics loss) despite BenCzechMark's Czech-NLU lead — NLU benchmarks don't predict cleanup instruction-following.
- Cross-model gaps: Czech self-correction unreliable zero-shot (needs few-shot prompt); paraphrasing is the dominant error class → Phase 2: anti-paraphrase prompt + cleanup-skip gate (FreeFlow pattern), possibly qwen3:4b for latency.

## 2026-07-10 — Phase 1: research synthesis (`docs/RESEARCH.md`)
- Recovered the deep-research workflow output (106 agents, 76 completed; 30 verification agents + synthesis lost to a session limit) via a subagent extraction of the workflow journal — no web research repeated.
- Wrote `docs/RESEARCH.md`: Wispr Flow teardown (cloud-only pipeline audio→Baseten→OpenAI/Anthropic, pricing, complaints), OSS landscape (Handy MIT/Rust/Tauri stack = architecture donor; VoiceInk = UX template, GPL so design-reference only; OpenWhispr MIT/Electron; Parakeet-on-Mac proof via sherpa-onnx), Czech STT (mikr/whisper-large-v3-czech-cv13 WER 7.89 % Apache-2.0; Parakeet v3 FLEURS cs 11.0 % ≈ whisper large-v3 11.3 % at ~10× speed; Apple-Silicon runtime latency ladder), LLM cleanup candidates (BenCzechMark: Gemma-2-9B strong Czech; Mistral-Nemo 12B plausible leader ≤12B; published data one generation stale → hands-on eval decisive), macOS integration (pasteboard+Cmd-V as robust insertion path; Secure Input must be detected, never bypassed).
- Every claim tagged MEASURED / VERIFIED / PLAUSIBLE / SNIPPET; verification debt and unresearched topics (Superwhisper, Vosk, code-switching mechanics) listed in §6.
- Phase 1 remaining: run LLM cleanup benchmark, benchmark mikr fine-tune + Parakeet on our fixtures, real-mic Czech validation, ITN-aware scoring.

## 2026-07-10 — Phase 1: ITN-aware STT scoring
- `scripts/bench_stt.py` now scores against the primary reference plus optional `accepted_references` (manifest) and reports the min-WER match (`scored_against: primary|itn`); cs-02 gained the digit-form alternative. Implemented by sonnet-implementer, reviewed by Fable.
- Re-ran all 4 whisper models: ITN-aware cs WER — large-v3 0.11 (was 0.18), large-v3-turbo 0.17 (was 0.23), medium 0.28, small 0.32. cs-02 is now 0.00 for large/turbo/medium; ranking unchanged, large-v3-turbo stays the provisional default. Docs updated (`docs/stt-benchmark.{md,json}`).

## 2026-07-10 — Phase 1: STT benchmark, round 2 (mikr Czech fine-tune + Parakeet v3)
- opus-implementer converted mikr/whisper-large-v3-czech-cv13 (Apache-2.0) HF→GGML→q5_0 (1031 MB) and benchmarked Parakeet TDT 0.6B v3 int8 via sherpa-onnx (~640 MB). Runner `scripts/bench_stt_round2.py`, results `docs/stt-benchmark-round2.{md,json}`.
- Fable review caught a scoring mismatch (round 2 ran concurrently with the ITN-aware change) — re-scored ITN-aware: **large-v3 cs 0.11 · mikr cs 0.14 · turbo cs 0.17 · Parakeet cs 0.11**.
- Verdict: mikr beats turbo modestly but not stock large-v3, loses whisper's number ITN, no speed win → only a niche "best code-switching" profile (mix 0.22). **Parakeet ties best Czech at ~10× speed** (~0.15 s/clip) with correct ITN, but worst code-switching (mix 0.38) blocks it as default. **Default stays large-v3-turbo q5_0**; Parakeet = low-latency profile candidate.
- RESEARCH.md §3 shortlist updated accordingly. Phase 1 automatable work done; real-mic Czech validation (user recordings) is the remaining gate before the model ADR.

## 2026-07-10 — Phase 1: real-microphone validation → gate PASSED, Phase 1 closed
- User recorded `dev-diktat.m4a` (31.7 s spontaneous Czech dev dictation, heavy code-switching); converted to `fixtures/audio/real/dev-diktat.wav`; all 4 STT candidates run. Write-up `docs/stt-real-validation.md`.
- **large-v3-turbo q5_0 confirmed default** (near-perfect, diacritics intact; residual errors = custom-vocab class). mikr fine-tune regressed on real speech ("refakturovat", "jason payload") → out of default consideration. Parakeet collapsed on real code-switching incl. a negation inversion → monolingual low-latency option only.
- **Measured hallucination finding:** all whisper models appended "Titulky vytvořil JohnyX." on 3 s of trailing silence; trimming silence removes it → VAD silence-trimming before STT is a hard Phase-2/3 requirement. Parakeet did not hallucinate.
- Phase 1 complete. Next: Phase 2 architecture (ARCHITECTURE.md + ADRs; model ADR now unblocked).

## 2026-07-10 — Phase 2: architecture design + ADRs D-0004..D-0010
- ARCHITECTURE.md rewritten from skeleton to full design: Rust workspace (whisper-core pure lib with ports/adapters, whisper-iface = `whisperd` HTTP+WS daemon hosting production STT/LLM adapters, whisper-platform macOS adapters, whisper-cli), session FSM with cancellation + model-failure + device-loss paths, per-VAD-segment decoding (whisper.cpp is non-streaming; segments decode during recording → bounded release-to-text even for hands-free), cleanup skip-gate, single-active-session concurrency (409), latency budget (≤1.5 s gate-skipped / ~2.5 s with LLM).
- ADRs: D-0004 STT default turbo q5_0 + profiles; D-0005 qwen3:8b via llama.cpp (Ollama dev-only) with evidence-transfer guard; D-0006 Rust workspace + dep license table; D-0007 pasteboard+Cmd-V insertion, Secure Input never bypassed; D-0008 Silero VAD mandatory (measured hallucination); D-0009 localhost HTTP+WS JSON; D-0010 TOML/JSON/SQLite storage.
- Cross-agent review (opus, adversarial): 16 findings, 0 blockers, 8 MAJOR — all addressed: streaming partials rescoped to per-segment, long-session latency solved by incremental decoding, Ollama→llama.cpp parity guard added, Fn-key infeasibility via rdev documented, concurrency/WS-disconnect/model-load-failure semantics specified, latency header reconciled, Parakeet weights license flagged for verification, plugin hook deferred to Phase 4.
- Review-surfaced risk register: rdev maintenance, whisper-rs↔whisper-cli parity, qwen3 GGUF re-measurement in-process, clipboard NFD round-trip.

## 2026-07-10 — Phase 3: Rust workspace scaffold (Core Engine skeleton)
- Rust 1.97 stable installed (brew rustup). Workspace scaffolded per D-0006: `crates/whisper-core` (ports AudioSource/SttEngine/CleanupEngine/TextSink/Vad, session FSM with Cancelled/Failed(best-text-so-far), pipeline orchestrator with event subscriber channel, JSON dictionary pass with whole-word UTF-8-safe substitutions) + stub crates whisper-iface/-platform/-cli. Implemented by sonnet-implementer, reviewed by Fable.
- Gates: build/test/clippy -D warnings/fmt all clean; **23/23 tests** incl. byte-exact Czech diacritics round-trip, dictionary "merknout"→"mergnout", mid-pipeline cancellation, device-loss → Failed with partial text.
- Noted for later: per-VAD-segment decoding lands with the real Silero/whisper adapters; NFC normalization is the STT-adapter boundary's job (§3.3); token checked between stages, in-loop capture cancellation arrives with real mic stream.
- Next: whisper-rs STT adapter + parity test vs whisper-cli (opus task).

## 2026-07-10 — Phase 3: whisper.cpp STT adapter + parity vs whisper-cli
- `WhisperCppEngine` in whisper-iface (whisper-rs 0.16 + Metal, model resident, NFC boundary per §3.3, `SttError::ModelLoadFailed` for D-0004/§3.2 semantics). Shared test adapters via whisper-core `test-support` feature. Implemented by opus-implementer, reviewed by Fable.
- **Parity vs whisper-cli: 6/6 fixtures PASS** (4 byte-identical; cs-01/mix-02 differ by 1 benign token each — cross-build FP drift between brew whisper-cpp 1.9.1/BLAS and whisper-rs's bundled whisper.cpp/Metal; engine output on cs-01 actually closer to reference). Accepted tolerance: sim ≥ 0.95 OR ≤ 1 token diff.
- Gates: build/test (23 fast + 3 real-model)/clippy -D warnings/fmt clean. Real-model tests are `#[ignore]`d (run via `cargo test -p whisper-iface -- --ignored`).
- Watched: whisper-cli's true default is beam-5 (our benchmark numbers) vs adapter greedy (byte-identical on fixtures, faster) — re-check on a bigger real corpus.

## 2026-07-10 — Phase 3: Silero VAD + per-segment decoding + anti-hallucination proof
- `Vad` port redesigned to a streaming segmenter (`push→VadEvent::{SpeechStart,SpeechFrame,SpeechEnd}` + `flush`); model-free trimming FSM in core (`vad.rs`, defaults: threshold 0.5, hangover 300 ms, max-silence 1 s), Silero V5 adapter in whisper-iface via `voice_activity_detector` 0.2.1 (MIT, model bundled, no runtime network). Pipeline decodes each segment at SpeechEnd during recording and emits `PartialTranscript` (§3.2 per-segment design now real). Implemented by opus-implementer, reviewed by Fable.
- **Anti-hallucination test passes**: cs-03 + 4 s appended silence ⇒ byte-identical transcript to trimmed version, no "Titulky vytvořil JohnyX."; pure silence ⇒ empty text + clean Idle. D-0008's measured requirement is now enforced by code + regression test.
- Gates: build / 27 fast + 5 real-model tests / clippy -D warnings / fmt — all clean.
- Noted: `ort` fetches its onnxruntime binary at build time (cached; no runtime network) — flagged in D-0008 for air-gapped builds; VadConfig defaults need tuning on a bigger real corpus; PlaceholderVad in whisper-platform is now redundant (cleanup later).

## 2026-07-10 — Phase 3: llama.cpp cleanup adapter + D-0005 guard re-measured in-process
- `LlamaCleanupEngine` in whisper-iface: llama-cpp-2 (Metal), official Qwen/Qwen3-8B-GGUF Q4_K_M (Apache-2.0, 5.03 GB, gitignored), model resident + fresh context per call, thinking disabled via prefilled empty `<think>` block, greedy, UTF-8-safe detokenization + NFC. Few-shot prompt (5 examples, disjoint from eval set): anti-paraphrase + self-correction rules, never-answer contract. Implemented by opus-implementer, reviewed by Fable (pipeline order dictionary→cleanup confirmed).
- **D-0005 evidence-transfer guard PASSED in-process**: 13/13 cases — 0/4 trap failures (hard), 0 diacritics lost (hard), **avg WER 0.069 (vs 0.15 Ollama zero-shot)**; the round-1 semantic substitution class is gone (adapter correctly leaves "merknout" to the dictionary pass).
- Gates: build / 31 fast + 6 real-model suites (cleanup guard, STT parity, VAD anti-hallucination ×2, e2e ×2) / clippy -D warnings / fmt — all green.
- Open (tracked): Czech self-correction "ne vlastně X" still keeps the retracted value (2 cases) — prompt NOT frozen, needs targeted few-shot or rule pass; latency 3.11 s in debug build — re-measure in `--release` + cache the static prompt-prefix KV state before updating §3.4; silence llama.cpp stderr logging when `whisperd` lands.

## 2026-07-10 — Phase 3: cleanup skip-gate heuristic (CleanupMode::Auto)
- New `whisper-core/src/skip_gate.rs`: deterministic, dependency-free `needs_cleanup(&str)` heuristic — Czech-first self-correction markers ("ne vlastně", "pardon", "škrtni to", …), filler words ("ehm", "prostě", …), immediate word repetition ("to to"), long run-on with zero punctuation (>30 words). Whole-word/whole-phrase UTF-8-safe matching mirrors dictionary.rs boundary logic; gate is read-only. Implemented by sonnet-implementer, reviewed by Fable.
- `PipelineConfig.cleanup_enabled` (bool stub) → `cleanup_mode: CleanupMode { Always, Never, Auto(default) }` + swappable `SkipGateConfig`; `PipelineEvent::CleanupSkipped` now carries `reason: CleanupSkipReason { ModeDisabled, GateClean, NoEngineConfigured }` for the future WS bridge.
- Gates: 46 whisper-core tests (22 new skip-gate incl. false-positive traps prostěradlo/umyji/trojúhelník/opravář + 2 new Auto-mode pipeline integration tests) / clippy -D warnings / fmt / workspace check — all clean.
- Noted: marker lists are a first pass — tune on the growing real-speech corpus (§3.2 deliverable); heuristic biases toward cleanup (false "needs cleanup" costs only latency, false "clean" costs quality).
- Incident: a mid-task `git reset` in the shared tree wiped uncommitted work (redone + re-verified); concurrent-agent git hygiene tightened — commit reviewed work immediately.

## 2026-07-10 — Phase 3: cleanup adapter round 2 — KV-prefix caching, latency under budget, self-correction fixed
- `LlamaCleanupEngine` reworked: one persistent `LlamaContext`, static few-shot prefix decoded once, per-call self-healing KV trim back to `prefix_len` + suffix-only decode (split exactly at the `<|im_start|>` special-token boundary — separate tokenization provably identical; byte-identical outputs vs fresh-context path verified on the whole guard suite). Overflow fallback to full-prompt fresh context. Model now owned (`Box`, documented `unsafe` self-reference, load-bearing field drop order) — fixes a Metal residency-set abort the earlier leak-to-`'static` approach caused at exit. Implemented by opus-implementer, reviewed by Fable.
- **Latency (release, D-0005 guard): 3.22 s → 1.12 s/call** (0.5–2.7 s by length) — under the 1.6 s budget; ARCHITECTURE §3.4 updated from measurements. llama.cpp stderr silenced via `send_logs_to_tracing(logs_enabled=false)` (0 ggml/metal lines).
- **Self-correction round 2 fixed via few-shot only**: root cause — system prompt listed "vlastně" as pure filler; two new disjoint retraction examples (clause restart + "ne vlastně" value swap) and trap examples reordered last. **Guard: avg WER 0.069 → 0.019, both retraction cases now WER 0.00, traps 0/4, diacritics 0** — independently re-run by Fable (`SUMMARY cases=13 avg WER=0.019 trap failures=0 diacritics lost=0 avg latency=1.17s`). Cleanup prompt now frozen.
- Noted: engine is `!Send`/`!Sync` (persistent context) — future daemon must pin it to one worker thread or front with an actor (flagged in struct doc); `dev-vocab-packajson` WER 0.00→0.09 (LLM no longer fixes "packa"→"package" — dictionary pass is the primary dev-vocab fixer per D-0005, low severity).
- Incident closed: agent's early `git stash` in the shared tree caused the reverted-files episode; both agents' final work is committed. A stale `stash@{0}` (superseded content) remains — safe to `git stash drop` manually.

## 2026-07-10 — Phase 4: whisperd daemon (HTTP /v1 + WS events) + thin CLI
- `whisperd` binary in whisper-iface per D-0009/D-0011: axum on 127.0.0.1 (optional bearer token), dedicated `whisperd-engine` OS thread owns the `!Send` engines (built *on* that thread via a `Send` builder closure; shared into per-job Pipelines via `Rc` — never cross a thread). Async side ↔ worker via bounded command channel + oneshot waiters + tokio broadcast for `WS /v1/events`. Implemented by opus-implementer, reviewed by Fable (worker.rs actor pattern, finalize ordering store→waiter→gate-release, Weak<AppState> shutdown, busy-gate rollback on send failure — all verified in review).
- Endpoints: health, status (loading/ready/degraded/unavailable), one-shot `POST /v1/transcribe` (audio_b64/path), async sessions + stop/cancel + authoritative `GET /v1/sessions/{id}` (survives WS disconnect), GET/PUT config (applied vs staged) & dictionary (validated via core parser). Session persistence: in-memory map + opt-in SQLite history (D-0010). `whisper-cli` = thin HTTP client (transcribe/status/config/listen over WS), no engine deps.
- Gates (independently re-run by Fable): **73 fast tests** (13 daemon contract + 2 WS + config/store units incl. Czech byte-exact round-trips through API, JSON and SQLite) / clippy --workspace -D warnings / fmt — clean; real-model daemon test (in-process whisperd + turbo → Czech "žluťoučký" via HTTP) green in release. New deps all MIT/Apache-2.0 (tokio, axum, tower, rusqlite bundled-SQLite public domain, base64, dirs, clap, ureq, tokio-tungstenite) — recorded in D-0011.
- Known limits (tracked in HANDOFF): second request = immediate 409 (no queued transcribe); cleanup-off profile not yet expressible in config.toml (needs sentinel); no input resampling (16 kHz WAV assumed — add check/rubato); headless cancel usually lands post-completion (real-time cancel arrives with mic port, Phase 5); in-memory session map unbounded; whisper.cpp load banner still on stderr.

## Debt register

(empty)
