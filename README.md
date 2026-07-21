# whisper

Local-first, Czech-first voice dictation engine (Wispr Flow–inspired): audio → VAD →
speech-to-text → optional LLM cleanup → text injection into whatever app has focus.
Runs fully offline, no cloud services or telemetry.

Built as an embeddable platform component rather than just an app — a headless Rust
core (`whisper-core`) is driven through a local HTTP/WebSocket daemon (`whisperd`),
with a macOS menu-bar tray (`whisper-tray`) as the default UI shell. Everything the
tray can do is also reachable over the API, so another program can orchestrate
dictation sessions directly.

## Architecture at a glance

```
whisper-ux (tray UI)  ──/v1 HTTP+WS──▶  whisper-iface (whisperd daemon)
                                              │
                                        whisper-core (pure lib: VAD, STT,
                                        cleanup, dictionary, session FSM)
                                              │
                                        whisper-platform (macOS: mic capture,
                                        global hotkeys, pasteboard injection)
```

See `ARCHITECTURE.md` for the full design, `DECISIONS.md` for the ADRs behind each
choice, and `ROADMAP.md` / `PROGRESS.md` / `HANDOFF.md` for current status.

## Requirements

- macOS (only supported platform for now)
- Rust (version pinned in `rust-toolchain.toml`)
- STT/LLM models — downloaded in-app on first run (Settings → Models / onboarding),
  no manual setup needed

## Quick start

```bash
git clone <this repo>
cd whisper
scripts/run.sh
```

This builds `whisperd` (daemon) and `whisper-tray` (menu-bar UI) in release mode if
needed, starts the daemon, waits for it to be healthy, then launches the tray in the
foreground. First launch walks you through onboarding, model downloads, and the
macOS permission grants (Microphone, Input Monitoring, Accessibility).

Default hotkey is **Ctrl+Alt** (push-to-talk, hold to record). Configure hotkeys,
language, and cleanup behavior in the tray's Settings.

## Building the .app / DMG

```bash
scripts/package.sh                # build Whisper.app + dist/Whisper.dmg
scripts/package.sh --link-models   # also symlink repo models/ into the app-support
                                    # dir + write a starter config (dev convenience)
```

This builds `whisperd` and `whisper-tray` in release mode, assembles `dist/Whisper.app`
(menu-bar only, no Dock icon), ad-hoc signs it, and wraps it in `dist/Whisper.dmg`.
Models are **not** bundled (multi-GB) — they're downloaded in-app on first run.

There's no Developer ID / notarization yet, so the first launch is Gatekeeper-blocked:
right-click → Open. See `docs/install.md` for the full install/update walkthrough.

## Using it headlessly

The daemon exposes a local HTTP + WebSocket API (`http://127.0.0.1:8737` by default),
so you don't need the tray at all:

```bash
# one-shot: audio file in, final text out
whisper-cli transcribe file.wav

# start/stop a live dictation session, stream events
whisper-cli listen
```

See `ARCHITECTURE.md` §4 for the full endpoint/event list.

## Development

```bash
cargo build          # build all crates
cargo test            # unit + fixture-based integration tests (incl. Czech set)
cargo clippy          # lint
```

Test fixtures (including Czech audio) live under `fixtures/`; no hardware or network
access is required to run the test suite.
