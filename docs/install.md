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

The first build compiles whisper.cpp from source and takes several
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

The tray's onboarding screen walks you through these on first run, and shows a
live status (Granted/Denied/Unknown) for Input Monitoring and Accessibility with
a **Re-check** button and a one-click **Open System Settings** link straight to
the right pane.

> **After updating the app**, macOS may silently drop the Input Monitoring and
> Accessibility grants. Whisper is **ad-hoc signed** (no paid Apple Developer
> ID, D-0014) — every reinstalled build has a slightly different code identity,
> and macOS's TCC privacy database keys grants to that identity. The result:
> the hotkey (and paste) can stop working after an update with **no error
> message**, because the underlying listener starts successfully — it just
> silently receives no key events. Whisper now detects this proactively (the
> tray shows a persistent banner naming the exact permission the moment it is
> missing, instead of leaving you to guess why the hotkey "does nothing"), but
> you still have to re-grant it by hand:
>
> 1. Open **System Settings → Privacy & Security → Input Monitoring** (and
>    **Accessibility**).
> 2. If `Whisper` is listed but its toggle is off, turn it back on. If it looks
>    "stuck" (toggle on but still not working), remove it with the **−**
>    button and re-add it with **+** (`/Applications/Whisper.app`).
> 3. Quit and relaunch Whisper (the listener only starts once, at daemon
>    startup — a grant made while it's running needs a restart to take
>    effect).
>
> This is a known limitation of ad-hoc signing (deferred pending a paid
> Developer ID / notarization, D-0014); see D-0016 for the detection mechanism.

## 5. Set up the models

Models are **not** bundled (they are 6+ GB). They live in the app-support data
dir: `~/Library/Application Support/whisper-engine/models/`.

### Option A — you have the repo models (fastest)

Symlink the repo's STT model into the data dir and write a starter config:

```bash
scripts/package.sh --link-models
```

This never overwrites an existing `config.toml`. It writes one only if none is
present, pointing at:

- STT: `models/ggml-large-v3-turbo-q5_0.bin` (Whisper large-v3-turbo, D-0004)
- Cleanup: served by Ollama, `[cleanup] model = "qwen3-vl:8b-instruct"` (see
  "Text cleanup (Ollama)" below)

with `delivery = "paste"` and the push-to-talk hotkey `Ctrl+Alt` enabled.

### Text cleanup (Ollama)

Optional LLM cleanup runs on a local [Ollama](https://ollama.com) server. Whisper
never installs Ollama and never deletes Ollama models:

```bash
brew install ollama && brew services start ollama
ollama pull qwen3-vl:8b-instruct   # default; or pick/pull it from the tray
```

The default model `qwen3-vl:8b-instruct` is shared with any other app using the
same Ollama. If Ollama is installed but not running, whisperd starts it (tries
`brew services start ollama`, then `open -a Ollama`, then `ollama serve`). If it
is missing, dictation still works and cleanup is skipped. Config:

```toml
[cleanup]
ollama_url = "http://127.0.0.1:11434"
model = "qwen3-vl:8b-instruct"   # "" turns cleanup off
keep_alive = "30m"
```

Validated for Czech: `qwen3-vl:8b-instruct`, `qwen3:8b`; other installed models
are listed in the tray as "not validated for Czech". The legacy
`[models] cleanup = ...` key is ignored, so `models/Qwen3-8B-Q4_K_M.gguf` is no
longer used and can be deleted.

### Option B — manual

```bash
mkdir -p ~/Library/Application\ Support/whisper-engine/models
# copy or symlink your GGML Whisper (STT) model file into that models/ directory
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
- **Nothing happens on the hotkey** — check the tray's permission banner (top of
  the main window) or Settings; it names the exact missing grant. Otherwise
  check Input Monitoring + Accessibility are granted (see "After updating the
  app" above — a reinstall can silently drop them) and that `delivery = "paste"`
  in the config.
- **Engine won't start / "unreachable"** — read `~/Library/Logs/whisper-engine/whisperd.log`;
  a missing model file is the usual cause (see step 5).
