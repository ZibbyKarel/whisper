# HANDOFF

## Current state
Phases 1–4 complete; Phases 5–6 implemented + hardened through three rounds of live
user feedback, **pending final manual verification** (2026-07-11). Round 2: PUT
/v1/config 500 fixed (mkdir parent), default hotkey **Ctrl+Alt** (modifier-only,
enabled by default), rdev permission failure surfaced (stderr + `platform.permission`
WS event), full-config Settings UI, **Whisper.app / DMG packaging with tray-managed
whisperd lifecycle (D-0014)** — `scripts/package.sh` builds `dist/Whisper.dmg`;
`scripts/run.sh` is the one-command dev launcher. Round 3: **floating waveform
overlay** while dictating (second egui viewport, top/bottom shell-local setting),
waveform menu-bar glyphs + generated `assets/AppIcon.icns`, Inter typography +
sidebar UI, dictation-language picker (Čeština/English/Auto/Other).
Root cause of "hotkey did nothing in other apps": whisperd reads ONLY
`~/Library/Application Support/whisper-engine/config.toml` (not the repo-root file);
that dir didn't exist, so hotkey/delivery ran on old defaults (disabled/none). The
user's canonical config now exists there (paste, Ctrl+Alt PTT, absolute model paths).
**USER ACTION REQUIRED:** re-test via `scripts/run.sh` or the packaged app
(docs/install.md): Czech at normal volume, hotkey dictation into another app, the
three TCC grants (Microphone, Input Monitoring, Accessibility), then the rest of
docs/phase5-manual-verification.md + docs/phase6-manual-verification.md.

**2026-07-11 — in-app model provisioning shipped (D-0015):** the app no longer
assumes `models/` pre-exists. `whisperd` now serves `GET/POST/DELETE /v1/models*`
(compiled-in catalog, streaming download + SHA-256 verify + atomic install into
`<data_dir>/models/`); the tray gates on missing required models at startup, has a
reworked Settings → Models card (Download/Retry/Cancel per model, raw paths
collapsed under "Advanced"), and a new onboarding checklist step. **Not yet
manually verified against a real download** (all automated tests use tiny local
mock servers, per the no-large-files-in-tests constraint) — add "download the STT
model via the tray from a clean app-support dir" to the manual verification pass.

## Architecture overview
See ARCHITECTURE.md (authoritative). Rust workspace — `whisper-core` (pure lib,
ports), `whisper-iface` (`whisperd` localhost HTTP+WS daemon + production adapters),
`whisper-platform` (macOS: cpal+AGC, rdev hotkey, pasteboard sink), `whisper-cli`,
`whisper-ux` (`whisper-tray` shell: /v1-only client, D-0013; owns whisperd process
in managed mode, D-0014). Pipeline: capture → Silero VAD → per-segment whisper decode
→ dictionary → skip-gate → optional LLM cleanup (KV-prefix cached) → TextSink.

## Completed
See PROGRESS.md 2026-07-10/2026-07-11 entries (research → architecture → core →
daemon → platform → tray → feedback round 2/3 → model provisioning). ADRs
D-0004..D-0015 in DECISIONS.md.

## Unfinished / next steps
1. **Manual verification (user):** packaged-app or run.sh walkthrough — Czech at
   normal volume (AGC + VAD 0.35), Ctrl+Alt PTT into another app, TCC grants,
   Secure Input, device unplug, tray sections/settings round-trip (staged notice +
   Restart button), onboarding; round-3 additions: overlay transparency /
   no-focus-steal / click-through on the real desktop (platform-dependent, plan B
   is an NSPanel via raw-window-handle if winit's hints are ignored), sidebar +
   Inter rendering, language presets round-trip, new tray glyph states, Dock/DMG
   icon; **D-0015 additions:** a real end-to-end model download from a clean
   app-support dir (startup gate → Download → progress → gate clears; Settings →
   Models Retry after a forced checksum failure; Cancel mid-download).
2. **Before pushing to origin:** commit 3a87853 accidentally tracked 84k `target/`
   files (fixed forward in 7e88ad0, history still bloated ~GBs; amend/rewrite was
   policy-denied mid-session). Decide: interactive rebase before first push, or
   accept the bloat. Also: stale `stash@{0}` — safe to `git stash drop` manually.
3. UI follow-ups: async HTTP off the UI thread; truncate long session UUIDs in the
   Status grid; onboarding kept its old top-bar layout (restyled fonts only) —
   consider sidebar-izing it if the user wants.
4. Phase-5 remainder: Fn/globe-key capture (NSEvent flagsChanged/IOKit behind the
   hotkey port).
5. Phase-4 follow-ups (tracked, not blocking): queued transcribe behind active
   session; cleanup-off config sentinel; HTTP-input resample check; session-map cap;
   whisper.cpp load banner.
6. Verify Parakeet v3 weights license before the low-latency profile (D-0004).
7. Grow the real-speech Czech corpus (noise, speakers) for VAD/WER regression and
   skip-gate marker tuning.
8. Contract mirror discipline: ux/api.rs ↔ daemon/api.rs+config.rs+models.rs have
   no compile-time link — update all three on any /v1 change (JSON fixtures fail
   loudly).
9. **D-0015 not independently reviewed:** implemented in a single agent pass; the
   constitution's "reviewer must differ from implementer" rule was not exercised
   here — flag for a follow-up review pass before treating it as fully hardened.
10. D-0015 follow-ups: resumable (HTTP Range) downloads, a mirror/alternate
    download host, and re-verifying the Qwen3-8B SHA-256 against a live
    HuggingFace fetch (only checked against the on-disk repo copy this session)
    were all explicitly deferred — see D-0015.
11. **D-0016 not manually verified end-to-end:** the FFI (`whisper-platform::permission`)
    was exercised on a real macOS host in this session (both queries returned
    `Granted`) and every gate (build/test/clippy, default + `--features
    platform`) passed, but the actual triggering scenario — reinstall an
    ad-hoc-signed build, watch macOS drop the grant, confirm whisperd's
    preflight + the tray banner catch it — was not (and cannot be, without a
    real reinstall cycle) reproduced this session. Also not independently
    reviewed (single-agent pass, same caveat as item 9).

## Known issues / assumptions
- Managed-daemon orphaning: if the tray dies abnormally (not Quit), the child
  whisperd survives until the next launch adopts it (accepted MVP risk, D-0014).
- Ad-hoc signing only: first launch of Whisper.app needs right-click → Open
  (Gatekeeper); no Developer ID / notarization (deferred). It can also silently
  drop the Input Monitoring/Accessibility TCC grants on reinstall — **D-0016**
  (2026-07-11) closes the silent-failure gap this caused (whisperd now
  preflights the grant and the tray shows a persistent banner + deep-link), but
  the underlying ad-hoc-signing churn itself is unchanged, so re-granting after
  an update is still a manual step (see `docs/install.md` "After updating the
  app"). Not yet manually verified against a real ad-hoc-reinstall grant drop.
- Duplicate ggml symbol linker warnings (whisper-rs-sys + llama-cpp-2 both vendor
  ggml) — pre-existing, links fine; worth a dedup look later.
- rdev maintenance is a watched risk (fallback: CGEventTap); Secure Input behavior
  still needs the manual pass; clipboard NFD round-trip guarded by NFC-normalize.
- Ollama + pulled models remain dev/benchmark tooling only; real-speech corpus n=1.
- Model downloads (D-0015) are not resumable: any interruption (network drop, app
  quit, cancel) restarts the ~547 MB/~4.8 GB transfer from zero on retry.
