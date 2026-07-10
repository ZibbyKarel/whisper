# HANDOFF

## Current state
Phases 1–4 complete, 2026-07-10. The engine is now reachable headless over the Interface
Layer: `whisperd` (axum, 127.0.0.1, D-0009/D-0011) exposes `/v1/health|status|transcribe|
sessions|config|dictionary` + `WS /v1/events`, with the `!Send` engines pinned to a
dedicated worker thread (actor: bounded cmd channel + oneshot waiters + broadcast events),
single-active-session 409 semantics, opt-in SQLite history, and `whisper-cli` as a thin
HTTP client. Core pipeline underneath: whisper.cpp STT (parity-proven), Silero VAD
per-segment decoding (anti-hallucination test), llama.cpp qwen3:8b cleanup (KV-prefix
cached, 1.1 s/call release, guard WER 0.019, 0 traps — prompt frozen), skip-gate
`CleanupMode::Auto`. 73 fast tests + 7 real-model suites green; clippy -D warnings; fmt
clean. Next: Phase 5 macOS platform layer.

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
1. Phase 5: macOS platform layer in whisper-platform — cpal mic AudioSource, global
   hotkey (push-to-talk/toggle; rdev with CGEventTap fallback, Fn/globe needs
   flagsChanged/IOKit), pasteboard+Cmd-V TextSink (D-0007: restore-after-paste, NFC
   read-back), Secure Input detect-never-bypass, menu-bar presence. Wire into whisperd
   as the mic-port audio source for live sessions.
2. Phase-4 follow-ups (tracked, not blocking): queued one-shot transcribe behind active
   session (now immediate 409); config sentinel to disable cleanup via config.toml;
   input sample-rate check/resample (16 kHz assumed); cap the in-memory session map;
   silence whisper.cpp load banner.
3. Verify Parakeet v3 weights license before adopting the low-latency profile (D-0004).
4. Grow the real-speech Czech corpus (noise, speakers) for VAD tuning + WER regression;
   tune skip-gate marker lists on it (first-pass lists, §3.2 deliverable).
5. Housekeeping: remove redundant PlaceholderVad in whisper-platform; a stale superseded
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
