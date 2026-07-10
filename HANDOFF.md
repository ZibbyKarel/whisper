# HANDOFF

## Current state
Phases 1–4 complete; Phases 5–6 implemented, **pending manual verification**
(2026-07-10). User's first live test: English great, Czech needed loud speech → fixed
same day (AGC-lite gain, VAD threshold 0.35, `audio.level` meter event). Phase 6 MVP:
`whisper-tray` (D-0013) — tray states, settings + dictionary editor, level meter,
onboarding; API-only, no TCC needed by the shell. Full stack: core pipeline (whisper.cpp STT parity-proven, Silero VAD
per-segment + anti-hallucination test, qwen3:8b cleanup KV-prefix cached 1.1 s/call
release, guard WER 0.019/0 traps — prompt frozen, skip-gate Auto) → `whisperd` daemon
(axum 127.0.0.1, actor over `!Send` engines, 409 semantics, SQLite history opt-in,
`whisper-cli` thin client) → macOS platform layer behind cargo feature `platform`
(D-0012): cpal mic → live sessions (`{"source":"mic"}`), rdev hotkey (PTT/toggle),
pasteboard+Cmd-V sink with Secure Input detect-never-bypass (D-0007), WS events
`secure_input.blocked`/`audio.device_lost`/`platform.permission`. Gates: 96 (default) +
33 (platform) tests, clippy -D warnings, fmt — clean in both configs.
**USER ACTION REQUIRED:** run `docs/phase5-manual-verification.md` (TCC permissions
cannot be granted by agents) — live Czech dictation, diacritics paste, Secure Input,
device-unplug.

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
1. **Manual Phase-5 verification (user) — first pass done 2026-07-10:** live dictation
   works; English great; Czech acceptable but ONLY when speaking loudly — quiet speech
   drops out (VAD threshold 0.5 too strict for quiet mics + no input gain). Fix in
   flight: AGC-lite in the capture path, configurable `[vad]` sensitivity (default
   threshold lowered), `audio.level` WS event for a UI meter. Re-verify Czech quality
   at normal speaking volume after the fix; rest of the checklist
   (`docs/phase5-manual-verification.md`: Secure Input, device unplug) still pending.
2. Phase-5 remainder: Fn/globe-key capture (NSEvent flagsChanged/IOKit path behind the
   same hotkey port).
3. Phase 6 follow-ups: manual walkthrough of `whisper-tray`
   (`docs/phase6-manual-verification.md` — onboarding, icon states, settings round-trip,
   dockless Accessory policy on real desktop); async HTTP off the UI thread; contract
   mirror (ux/api.rs ↔ daemon/api.rs) has no compile-time link — update both on any /v1
   change (JSON fixtures fail loudly).
4. Phase-4 follow-ups (tracked, not blocking): queued one-shot transcribe behind active
   session (now immediate 409); config sentinel to disable cleanup via config.toml;
   HTTP-input sample-rate check/resample (16 kHz WAV assumed on audio_b64/path — mic
   path resamples properly); cap the in-memory session map; silence whisper.cpp load
   banner.
5. Verify Parakeet v3 weights license before adopting the low-latency profile (D-0004).
6. Grow the real-speech Czech corpus (noise, speakers) for VAD tuning + WER regression;
   tune skip-gate marker lists on it (first-pass lists, §3.2 deliverable).
7. Housekeeping: a stale superseded `stash@{0}` sits in the repo — safe to
   `git stash drop` manually (PlaceholderVad already removed in Phase 5).

## Known issues / assumptions
- Review-surfaced watched risks: rdev maintenance (fallback CGEventTap), Fn/globe key
  needs flagsChanged/IOKit (Phase 5), clipboard NFD round-trip (NFC-normalize), Secure
  Input behavior still [PLAUSIBLE] (verify in Phase 5).
- Ollama + pulled models (qwen3/gemma3/mistral-nemo, 19 GB) remain as dev/benchmark
  tooling only; `/tmp/stt_round2/` holds the throwaway venv + Parakeet model dir (not in
  models/ — re-download if /tmp got cleaned).
- Real-speech corpus is n=1 recording; Phase 3 testing should grow it (noise, speakers).
- rtk warns about untrusted `.rtk/filters.toml` (cosmetic).
