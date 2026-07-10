# Installing Whisper (macOS)

Whisper is a local-first, Czech-first dictation app: audio → speech-to-text →
optional cleanup → paste into the focused app. Everything runs on your Mac; no
cloud, no telemetry (see the project constitution). This guide walks through
building the app, installing it, granting the three macOS permissions it needs,
and setting up the models.

The app bundle contains **two** programs: the menu-bar UI (`whisper-tray`) and the
engine daemon (`whisperd`). You only launch the app — the tray starts the engine
for you and stops it when you quit (D-0014).

## 1. Build the DMG

From the repository root:

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
scripts/package.sh
```

This produces:

- `dist/Whisper.app` — the app bundle (ad-hoc signed).
- `dist/Whisper.dmg` — a disk image with a drag-to-Applications window.

The first build compiles whisper.cpp and llama.cpp from source and takes several
minutes; later builds are fast.

## 2. Install

1. Open `dist/Whisper.dmg` (double-click).
2. In the window that appears, **drag `Whisper.app` onto the `Applications`
   folder** shortcut.
3. Eject the disk image.

## 3. First launch (Gatekeeper)

The app is **ad-hoc signed** (no paid Apple Developer ID), so Gatekeeper blocks a
normal double-click the first time. Once:

1. In `Applications`, **right-click `Whisper` → Open**.
2. Confirm **Open** in the dialog.

After this first approval you can launch it normally (Spotlight, Launchpad, etc.).
Whisper has no Dock icon — look for its icon in the **menu bar** (top-right).

## 4. Grant the three permissions

Dictation needs three macOS privacy grants. They attach to `Whisper.app` (its
embedded `whisperd`), not to a terminal. Open **System Settings → Privacy &
Security** and enable Whisper under each:

| Permission | Why | System Settings path |
| --- | --- | --- |
| **Microphone** | Capture your voice | Privacy & Security → **Microphone** |
| **Accessibility** | Paste text into the focused app (Cmd-V) | Privacy & Security → **Accessibility** |
| **Input Monitoring** | Global push-to-talk hotkey | Privacy & Security → **Input Monitoring** |

macOS usually prompts for Microphone on your first dictation; Accessibility and
Input Monitoring you may need to add manually with the **+** button (choose
`/Applications/Whisper.app`). Toggle Whisper off and on again if a grant does not
take effect immediately.

The tray's onboarding screen walks you through these on first run.

## 5. Set up the models

Models are **not** bundled (they are 6+ GB). They live in the app-support data
dir: `~/Library/Application Support/whisper-engine/models/`.

### Option A — you have the repo models (fastest)

Symlink the repo `models/` into the data dir and write a starter config:

```bash
scripts/package.sh --link-models
```

This never overwrites an existing `config.toml`. It writes one only if none is
present, pointing at:

- STT: `models/ggml-large-v3-turbo-q5_0.bin` (Whisper large-v3-turbo, D-0004)
- Cleanup: `models/Qwen3-8B-Q4_K_M.gguf` (qwen3:8b, D-0005)

with `delivery = "paste"` and the push-to-talk hotkey `Ctrl+Alt` enabled.

### Option B — manual

```bash
mkdir -p ~/Library/Application\ Support/whisper-engine/models
# copy or symlink your GGML/GGUF model files into that models/ directory
```

Then create `~/Library/Application Support/whisper-engine/config.toml` (see the
starter config that `--link-models` writes for the exact shape). Relative model
paths in the config are resolved against the app-support dir at startup, so
`models/…bin` works even though the app's working directory is not the repo.

## 6. Where things live

| What | Path |
| --- | --- |
| Config | `~/Library/Application Support/whisper-engine/config.toml` |
| Models | `~/Library/Application Support/whisper-engine/models/` |
| Engine log | `~/Library/Logs/whisper-engine/whisperd.log` |
| Tray onboarding state | `~/Library/Application Support/whisper-tray/state.json` |

The engine log path is also shown in the tray's **Status** section. When the tray
started the engine, a **Restart whisperd** button there (and next to any
"restart to apply" notice) restarts it; if the engine was already running when the
app launched, that button is disabled ("daemon not managed by this app").

## 7. Dev launcher (no bundle)

To run the same thing from the repo without building a DMG:

```bash
scripts/run.sh          # builds release binaries if missing
scripts/run.sh --build  # force a rebuild first
```

It starts `whisperd` in the background (logging to the same file), waits for the
engine to come up, then runs the tray in the foreground. Ctrl-C stops the tray and
the engine it started — an already-running engine is left alone.

## Troubleshooting

- **"Whisper is damaged / cannot be opened"** — Gatekeeper on the ad-hoc build.
  Use right-click → Open (step 3). If it persists, clear the quarantine flag:
  `xattr -dr com.apple.quarantine /Applications/Whisper.app`.
- **Nothing happens on the hotkey** — check Input Monitoring + Accessibility are
  granted, and that `delivery = "paste"` in the config.
- **Engine won't start / "unreachable"** — read `~/Library/Logs/whisper-engine/whisperd.log`;
  a missing model file is the usual cause (see step 5).
