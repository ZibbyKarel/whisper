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

## Debt register

(empty)
