# HANDOFF

## Current state
Phase 1 (research) mostly done. `docs/RESEARCH.md` written — synthesizes the deep-research
run (Wispr Flow teardown, OSS landscape, Czech STT/LLM evidence, macOS integration) with
per-claim confidence tags, plus our measured benchmarks. LLM cleanup benchmark round 1
done (`docs/llm-cleanup-benchmark.md`): **qwen3:8b = provisional cleanup default** (0/4
trap failures, avg WER 0.15, 1.6 s); mistral-nemo:12b disqualified (obeys prompt
injections). STT round 2 done (`docs/stt-benchmark-round2.md`): mikr Czech fine-tune and
Parakeet v3 benchmarked, ITN-aware — **default stays large-v3-turbo q5_0**; Parakeet
(cs 0.11, ~10× faster) is the low-latency profile candidate but worst at code-switching.
Provisional model pair: whisper large-v3-turbo q5_0 (STT) + qwen3:8b (cleanup) ≈ 5.8 GB
resident, inside the ~10 GB budget (D-0001). All automatable Phase-1 work is done —
only real-mic validation (needs the user) remains before Phase 2.

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
1. ~~Real-mic validation~~ DONE (`docs/stt-real-validation.md`): gate passed,
   large-v3-turbo q5_0 confirmed default; VAD silence-trimming = measured hard
   requirement (whisper hallucinates "Titulky vytvořil JohnyX." on trailing silence).
2. Round 2 of the LLM cleanup eval belongs to Phase 2 (engineered prompt: few-shot
   self-correction + anti-paraphrase; try qwen3:4b; cleanup-skip gate).
3. Phase 2 architecture (STARTING): ARCHITECTURE.md full design + ADRs (STT model, LLM model,
   stack language, insertion method — RESEARCH.md has the evidence base).

## Known issues / assumptions
- Ollama daemon started via `brew services start ollama`; model pulls ran in a background
  task — verify with `ollama list` (~20 GB total for the three models).
- Deep-research verification debt (RESEARCH.md §6): Secure Input, VocaMac, CzechBench
  numbers, Mistral-Nemo Czech claims are PLAUSIBLE only — verify individually if an ADR
  depends on them.
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic).
