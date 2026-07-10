# HANDOFF

## Current state
Phases 1–2 complete; Phase 3 (Core Engine MVP) mostly done, 2026-07-10. The headless
pipeline runs end to end with real models: Rust workspace (D-0006) with whisper.cpp STT
adapter (parity vs whisper-cli proven), Silero VAD streaming segmenter with per-segment
decoding (anti-hallucination regression test green), and llama.cpp qwen3:8b cleanup
adapter (D-0005 in-process guard: 0 traps, 0 diacritics loss, avg WER 0.069). 31 fast +
6 real-model test suites green; clippy -D warnings; fmt clean.

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
1. Phase-3 remainder: cleanup skip-gate marker heuristic (currently always-run flag);
   `--release` latency re-measurement + static-prompt KV-prefix caching (debug build
   measured 3.11 s/call vs ~1.6 s budget — §3.4 not yet updated on purpose); Czech
   self-correction ("ne vlastně X") still fails 2 cases — prompt NOT frozen (targeted
   few-shot or rule pass); silence llama.cpp stderr logging before `whisperd`.
2. Phase 4: `whisperd` — HTTP `/v1/*` + `WS /v1/events` per ARCHITECTURE §4 / D-0009,
   bridging the existing PipelineEvent subscriber channel; CLI as thin client.
3. Verify Parakeet v3 weights license before adopting the low-latency profile (D-0004).
4. Grow the real-speech Czech corpus (noise, speakers) for VAD tuning + WER regression.

## Known issues / assumptions
- Review-surfaced watched risks: rdev maintenance (fallback CGEventTap), Fn/globe key
  needs flagsChanged/IOKit (Phase 5), clipboard NFD round-trip (NFC-normalize), Secure
  Input behavior still [PLAUSIBLE] (verify in Phase 5).
- Ollama + pulled models (qwen3/gemma3/mistral-nemo, 19 GB) remain as dev/benchmark
  tooling only; `/tmp/stt_round2/` holds the throwaway venv + Parakeet model dir (not in
  models/ — re-download if /tmp got cleaned).
- Real-speech corpus is n=1 recording; Phase 3 testing should grow it (noise, speakers).
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic).
