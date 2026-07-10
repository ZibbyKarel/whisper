# HANDOFF

## Current state
Phase 1 (research) mostly done. `docs/RESEARCH.md` written — synthesizes the deep-research
run (Wispr Flow teardown, OSS landscape, Czech STT/LLM evidence, macOS integration) with
per-claim confidence tags, plus our measured STT benchmark. LLM cleanup benchmark harness
built (`scripts/bench_llm_cleanup.py`, `fixtures/llm/cleanup-cases.json`) but not yet run —
qwen3:8b / gemma3:12b / mistral-nemo:12b were still downloading via Ollama.

## Architecture overview
See ARCHITECTURE.md (skeleton; binding layer contract only) — unchanged since Phase 0.
RESEARCH.md §2 identifies the proven stack pattern (Rust core: cpal → Silero VAD →
whisper.cpp/Parakeet → pasteboard+Cmd-V insertion; Handy MIT as reference) as Phase-2 input.

## Completed
See PROGRESS.md 2026-07-10 entries. Key artifacts:
- `docs/stt-benchmark.{md,json}` — large-v3-turbo q5_0 provisional STT default.
- `docs/RESEARCH.md` — Phase 1 research deliverable (claims tagged MEASURED/VERIFIED/PLAUSIBLE/SNIPPET).
- `scripts/bench_llm_cleanup.py` + `fixtures/llm/cleanup-cases.json` — 13 cases incl. 4
  trap cases (dictated questions/commands must be transcribed, never answered).

## Unfinished / next steps
1. Run `python3 scripts/bench_llm_cleanup.py` once `ollama list` shows qwen3:8b,
   gemma3:12b, mistral-nemo:12b; write results into `docs/llm-cleanup-benchmark.json` +
   a short `docs/llm-cleanup-benchmark.md` analysis; update RESEARCH.md §4.
2. Benchmark mikr/whisper-large-v3-czech-cv13 (needs GGML conversion) and Parakeet v3
   (sherpa-onnx) on our fixtures — extend `scripts/bench_stt.py`.
3. Real-microphone Czech validation of the STT finalist (blocks the model ADR).
4. ITN-aware WER scoring mode in `bench_stt.py`.
5. Then Phase 2 architecture: ARCHITECTURE.md full design + ADRs (STT model, LLM model,
   stack language, insertion method — RESEARCH.md has the evidence base).

## Known issues / assumptions
- Ollama daemon started via `brew services start ollama`; model pulls ran in a background
  task — verify with `ollama list` (~20 GB total for the three models).
- Deep-research verification debt (RESEARCH.md §6): Secure Input, VocaMac, CzechBench
  numbers, Mistral-Nemo Czech claims are PLAUSIBLE only — verify individually if an ADR
  depends on them.
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic).
