# HANDOFF

## Current state
Phases 1–3 complete, 2026-07-10. The headless pipeline runs end to end with real models:
Rust workspace (D-0006) with whisper.cpp STT adapter (parity vs whisper-cli proven),
Silero VAD streaming segmenter with per-segment decoding (anti-hallucination regression
test green), llama.cpp qwen3:8b cleanup adapter (KV-prefix cached: **1.1 s/call release**,
guard avg WER 0.019, 0 traps, 0 diacritics loss — prompt frozen; llama.cpp logs silenced),
and skip-gate `CleanupMode::Auto` (Czech marker heuristic, 22 unit tests). 53 fast +
6 real-model test suites green; clippy -D warnings; fmt clean. Next: Phase 4 `whisperd`.

## Architecture overview
See ARCHITECTURE.md (authoritative). Shape: Rust workspace — `whisper-core` (pure lib,
ports: AudioSource/SttEngine/CleanupEngine/TextSink), `whisper-iface` (`whisperd`
localhost HTTP+WS daemon; hosts production whisper.cpp/llama.cpp adapters),
`whisper-platform` (macOS: cpal, hotkeys, pasteboard injection), `whisper-cli`.
Pipeline: capture → Silero VAD (mandatory silence-trim, D-0008) → per-VAD-segment
whisper decode → dictionary pass → skip-gate → optional LLM cleanup → TextSink.

## Completed
See PROGRESS.md 2026-07-10 entries. Key artifacts: docs/RESEARCH.md,
docs/stt-benchmark{,.-round2}.md, docs/stt-real-validation.md (turbo confirmed on real
speech; silence-hallucination finding), docs/llm-cleanup-benchmark.md (qwen3:8b wins,
mistral-nemo disqualified), ARCHITECTURE.md + ADRs D-0004..D-0010 (cross-reviewed, 16
findings addressed).

## Unfinished / next steps
1. Phase 4: `whisperd` — HTTP `/v1/*` + `WS /v1/events` per ARCHITECTURE §4 / D-0009,
   bridging the existing PipelineEvent subscriber channel (CleanupSkipped now carries a
   `reason`); CLI as thin client. NOTE: LlamaCleanupEngine is `!Send`/`!Sync` — pin to
   one worker thread or front with an actor (see cleanup.rs struct doc).
2. Verify Parakeet v3 weights license before adopting the low-latency profile (D-0004).
3. Grow the real-speech Czech corpus (noise, speakers) for VAD tuning + WER regression;
   tune skip-gate marker lists on it (first-pass lists, §3.2 deliverable).
4. Housekeeping: remove redundant PlaceholderVad in whisper-platform; a stale superseded
   `stash@{0}` sits in the repo — safe to `git stash drop` manually.

## Known issues / assumptions
- Review-surfaced watched risks: rdev maintenance (fallback CGEventTap), Fn/globe key
  needs flagsChanged/IOKit (Phase 5), clipboard NFD round-trip (NFC-normalize), Secure
  Input behavior still [PLAUSIBLE] (verify in Phase 5).
- Ollama + pulled models (qwen3/gemma3/mistral-nemo, 19 GB) remain as dev/benchmark
  tooling only; `/tmp/stt_round2/` holds the throwaway venv + Parakeet model dir (not in
  models/ — re-download if /tmp got cleaned).
- Real-speech corpus is n=1 recording; Phase 3 testing should grow it (noise, speakers).
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic).
