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

## Debt register

(empty)
