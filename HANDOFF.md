# HANDOFF

## Current state
Phase 1 (research) and Phase 2 (architecture) complete, 2026-07-10. ARCHITECTURE.md holds
the full reviewed design; DECISIONS.md has ADRs D-0001..D-0010. Confirmed model pair:
whisper large-v3-turbo q5_0 (STT, D-0004) + qwen3:8b via llama.cpp (cleanup, D-0005),
≈5.8 GB resident. Next: Phase 3 — Core Engine MVP.

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

## Unfinished / next steps (Phase 3 — Core Engine MVP)
1. Scaffold the Rust workspace (4 crates per D-0006), CI running fixture-audio pipeline
   tests (no hardware). Delegate scaffold to sonnet-implementer; whisper-rs/VAD
   integration to opus-implementer; Fable reviews.
2. Implement capture→VAD→STT path against fixtures first (headless), incl. the
   anti-hallucination test (silence-padded clip ≡ trimmed clip output).
3. whisper-rs output parity check vs whisper-cli on the fixture set (D-0006 consequence).
4. LLM cleanup stage: re-measure qwen3:8b traps+latency in-process via llama.cpp with the
   final few-shot prompt BEFORE freezing (D-0005 evidence-transfer guard); qwen3:4b trial.
5. Verify Parakeet v3 weights license before adopting the low-latency profile (D-0004).

## Known issues / assumptions
- Review-surfaced watched risks: rdev maintenance (fallback CGEventTap), Fn/globe key
  needs flagsChanged/IOKit (Phase 5), clipboard NFD round-trip (NFC-normalize), Secure
  Input behavior still [PLAUSIBLE] (verify in Phase 5).
- Ollama + pulled models (qwen3/gemma3/mistral-nemo, 19 GB) remain as dev/benchmark
  tooling only; `/tmp/stt_round2/` holds the throwaway venv + Parakeet model dir (not in
  models/ — re-download if /tmp got cleaned).
- Real-speech corpus is n=1 recording; Phase 3 testing should grow it (noise, speakers).
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic).
