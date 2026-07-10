# HANDOFF

> Placeholder — filled before any run ends prematurely (context/token/rate limits).

## Current state
Phase 0 (environment bootstrap) complete. Phase 1 (research) in progress: STT benchmark round 1 done (whisper.cpp on synthetic Czech/English/mixed fixtures). Wispr Flow product/UX analysis, LLM cleanup comparison, VAD/capture/text-injection research, and `docs/RESEARCH.md` not started.

## Architecture overview
See ARCHITECTURE.md (skeleton; binding layer contract only) — unchanged since Phase 0.

## Completed
See PROGRESS.md 2026-07-10 entries (environment bootstrap; STT benchmark round 1).
- `scripts/bench_stt.py` benchmark harness + `fixtures/audio/` (6 synthetic TTS clips, manifest with references).
- `docs/stt-benchmark.md` / `docs/stt-benchmark.json`: large-v3-turbo q5_0 is the provisional default model (near-large-v3 Czech quality, ~2× speed, 547 MB); medium/small degrade Czech diacritics.

## Unfinished / next steps
1. Make `scripts/bench_stt.py` scoring ITN-aware — current raw WER penalizes whisper's desirable number/format normalization, inflating error rates.
2. Validate the large-v3-turbo finalist on real-microphone Czech recordings (synthetic TTS fixtures are a clean upper bound) before locking a model choice into an ADR.
3. Local LLM cleanup comparison (Qwen/Gemma/Mistral tiers, 24 GB budget) — Ollama installed but no models pulled yet.
4. Wispr Flow product/UX/pipeline analysis; VAD, audio capture, macOS text-injection method research.
5. Write `docs/RESEARCH.md` deliverable, then move to Phase 2 architecture with ADRs.

## Known issues / assumptions
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic; filters not applied).
- Ollama installed via brew but its server isn't running and no LLM models are pulled yet (Phase 1 artifact, still pending).
- Uncommitted local change: `.gitignore` gained a `graphify-out/` entry (graphify knowledge-graph output dir) — not yet committed.
- `ROADMAP.md`'s "Current position" line still reads "starting Phase 1"; Phase 1 is now underway (STT benchmark round 1 done) but not yet marked as such there.
