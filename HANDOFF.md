# HANDOFF

> Placeholder — filled before any run ends prematurely (context/token/rate limits).

## Current state
Phase 0 (environment bootstrap) complete; Phase 1 (research) not started.

## Architecture overview
See ARCHITECTURE.md (skeleton; binding layer contract only).

## Completed
See PROGRESS.md 2026-07-10 entry.

## Unfinished / next steps
1. Phase 1 research per ROADMAP.md: Wispr Flow analysis + Czech-first STT/LLM comparison.
2. Build Czech/English/mixed audio fixture set; measure WER of candidate models.
3. Then Phase 2 architecture with ADRs.

## Known issues / assumptions
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic; filters not applied).
- Ollama installed via brew but no models pulled yet (models are Phase 1 artifacts).
