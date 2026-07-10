# HANDOFF

## Current state
Phases 1–4 complete; Phases 5–6 implemented + hardened from two rounds of live user
feedback, **pending final manual verification** (2026-07-10). Round 2 shipped: PUT
/v1/config 500 fixed (mkdir parent), default hotkey **Ctrl+Alt** (modifier-only,
enabled by default), rdev permission failure surfaced (stderr + `platform.permission`
WS event), full-config Settings UI with modern dark theme, and **Whisper.app / DMG
packaging with tray-managed whisperd lifecycle (D-0014)** — `scripts/package.sh`
builds `dist/Whisper.dmg`; `scripts/run.sh` is the one-command dev launcher.
Root cause of "hotkey did nothing in other apps": whisperd reads ONLY
`~/Library/Application Support/whisper-engine/config.toml` (not the repo-root file);
that dir didn't exist, so hotkey/delivery ran on old defaults (disabled/none). The
user's canonical config now exists there (paste, Ctrl+Alt PTT, absolute model paths).
**USER ACTION REQUIRED:** re-test via `scripts/run.sh` or the packaged app
(docs/install.md): Czech at normal volume, hotkey dictation into another app, the
three TCC grants (Microphone, Input Monitoring, Accessibility), then the rest of
docs/phase5-manual-verification.md + docs/phase6-manual-verification.md.

## Architecture overview
See ARCHITECTURE.md (authoritative). Rust workspace — `whisper-core` (pure lib,
ports), `whisper-iface` (`whisperd` localhost HTTP+WS daemon + production adapters),
`whisper-platform` (macOS: cpal+AGC, rdev hotkey, pasteboard sink), `whisper-cli`,
`whisper-ux` (`whisper-tray` shell: /v1-only client, D-0013; owns whisperd process
in managed mode, D-0014). Pipeline: capture → Silero VAD → per-segment whisper decode
→ dictionary → skip-gate → optional LLM cleanup (KV-prefix cached) → TextSink.

## Completed
See PROGRESS.md 2026-07-10 entries (research → architecture → core → daemon →
platform → tray → feedback round 2). ADRs D-0004..D-0014 in DECISIONS.md.

## Unfinished / next steps
1. **Manual verification (user):** packaged-app or run.sh walkthrough — Czech at
   normal volume (AGC + VAD 0.35), Ctrl+Alt PTT into another app, TCC grants,
   Secure Input, device unplug, tray sections/settings round-trip (staged notice +
   Restart button), onboarding.
2. **Before pushing to origin:** commit 3a87853 accidentally tracked 84k `target/`
   files (fixed forward in 7e88ad0, history still bloated ~GBs; amend/rewrite was
   policy-denied mid-session). Decide: interactive rebase before first push, or
   accept the bloat. Also: stale `stash@{0}` — safe to `git stash drop` manually.
3. UI polish round 2 (user has "many reservations", wants it less form-like): visual
   refinement beyond the new theme, app icon (`assets/AppIcon.icns` — package.sh
   already embeds it if present), async HTTP off the UI thread, language presets.
4. Phase-5 remainder: Fn/globe-key capture (NSEvent flagsChanged/IOKit behind the
   hotkey port).
5. Phase-4 follow-ups (tracked, not blocking): queued transcribe behind active
   session; cleanup-off config sentinel; HTTP-input resample check; session-map cap;
   whisper.cpp load banner.
6. Verify Parakeet v3 weights license before the low-latency profile (D-0004).
7. Grow the real-speech Czech corpus (noise, speakers) for VAD/WER regression and
   skip-gate marker tuning.
8. Contract mirror discipline: ux/api.rs ↔ daemon/api.rs+config.rs have no
   compile-time link — update both on any /v1 change (JSON fixtures fail loudly).

## Known issues / assumptions
- Managed-daemon orphaning: if the tray dies abnormally (not Quit), the child
  whisperd survives until the next launch adopts it (accepted MVP risk, D-0014).
- Ad-hoc signing only: first launch of Whisper.app needs right-click → Open
  (Gatekeeper); no Developer ID / notarization (deferred).
- Duplicate ggml symbol linker warnings (whisper-rs-sys + llama-cpp-2 both vendor
  ggml) — pre-existing, links fine; worth a dedup look later.
- rdev maintenance is a watched risk (fallback: CGEventTap); Secure Input behavior
  still needs the manual pass; clipboard NFD round-trip guarded by NFC-normalize.
- Ollama + pulled models remain dev/benchmark tooling only; real-speech corpus n=1.
