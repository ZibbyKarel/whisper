# Phase 6 — UX shell (`whisper-tray`): manual verification checklist

> Why manual: the tray icon (NSStatusItem), the egui window, macOS activation
> policy, and any live dictation all need a real GUI session and a running,
> permission-granted `whisperd`. CI cannot launch a display server or grant TCC.
> The automated gates cover every GUI-free decision (`cargo test -p whisper-ux`:
> event parsing, tray state machine, icon rasterization, HTTP client against a
> mock, onboarding persistence, dictionary validation); this checklist covers the
> window/tray/daemon seams they cannot.
>
> The shell is a **client** of the `/v1` Interface Layer only (D-0013): it never
> links an engine or the platform crate. It needs **no** TCC permissions of its
> own — the daemon owns mic/paste/input-monitoring.

## Build

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
# Platform-enabled daemon (owns mic/paste permissions):
cargo build -p whisper-iface --features platform --bin whisperd
# The UX shell (no features, no engine deps):
cargo build -p whisper-ux --bin whisper-tray
```

## Run

1. Start the daemon with real models present in `models/` and, for dictation,
   `delivery = "paste"` in `~/.config/whisper-engine/config.toml` (Phase 5 doc).
2. Start the shell (defaults to `http://127.0.0.1:8737`; override with
   `WHISPERD_URL` / `WHISPERD_TOKEN`):
   ```bash
   ./target/debug/whisper-tray
   ```
   The shell needs no permissions; the **daemon** prompts for mic/accessibility on
   first dictation.

## 1. Menu-bar presence and dockless launch (D-0013 §6)

1. **Expect:** a status-bar icon appears (gray disc = idle). On macOS the app runs
   as an **accessory** — **no Dock icon** and no app-menu.
2. Click the icon → the menu shows: **Start dictation**, ─, **Settings…**,
   **Daemon: <phase>** (disabled), ─, **Quit**.
3. Known limit to check: the accessory activation policy is set via eframe's
   `event_loop_builder` hook (`winit::…::ActivationPolicy::Accessory`). If a Dock
   icon still appears, the hook did not take effect on this eframe/winit version —
   record the versions and fall back to an `Info.plist` `LSUIElement` bundle.

## 2. Icon reflects session/daemon state (event → state machine)

Drive the daemon over HTTP or the hotkey and watch the icon color:

| Trigger (WS event)                       | Icon      |
| ---------------------------------------- | --------- |
| model loading at startup (`model_loading`) | orange (processing) |
| models ready (`model_ready`)             | gray (idle) |
| session start (`session_started`)        | red (recording) |
| post-capture (`raw_transcript`)          | orange (processing) |
| finish (`cleaned`/`delivered`)           | gray (idle) |
| cancel/fail                              | gray (idle) |
| **kill the daemon** (connection lost)     | dark red (error) |
| **restart the daemon** (reconnect)        | gray (idle) after backoff |

1. `curl -s -XPOST localhost:8737/v1/sessions -d '{"source":"mic"}'` → icon turns
   red; the menu's first item flips to **Stop dictation**.
2. `curl -s -XPOST localhost:8737/v1/sessions/<id>/stop` → orange while the
   pipeline finishes, then gray.
3. Kill `whisperd` (Ctrl-C). **Expect:** the icon goes dark red within ~1 s and
   the window's top-right badge shows 🔴 Error. Restart `whisperd`; the WS thread
   reconnects with backoff (0.5 s → capped 10 s) and the icon returns to gray.

## 3. Start/Stop from the tray

1. With no active session, click **Start dictation** → a mic session starts
   (daemon may prompt for Microphone the first time — grant it).
2. Click **Stop dictation** → the session stops gracefully and text is delivered
   per the daemon's `delivery` config.

## 4. Settings window (GET on open, PUT on Save)

1. Click **Settings…** → the window shows the live config pulled from
   `GET /v1/config`.
2. Change **Language** (cs/en/auto), **Cleanup mode** (always/never/auto),
   **Delivery** (none/paste), **Hotkey** combo + mode, **Port**.
3. Click **Save**. **Expect:** the status line reports `applied: …` for runtime
   fields (language, cleanup_mode, delivery) and
   `staged (needs daemon restart): …` for hotkey/port (matches `PUT /v1/config`
   `applied`/`staged`). Confirm staged fields only take effect after a daemon
   restart.
4. **Czech check:** set the language label / type Czech text (e.g. `příliš
   žluťoučký kůň`) into a dictionary field — diacritics must render correctly
   (egui default fonts cover Latin-Extended).

## 5. Dictionary editor (`PUT /v1/dictionary`, validation)

1. In Settings → **Dictionary**, add a row `packa json → package.json` and a
   Czech row `žluťoučký → žluťoučký kůň`.
2. Add a row with an **empty `from`** → an inline red validation error appears and
   **Save** refuses to send (client-side mirror of the daemon's rule).
3. Fix/remove the bad row, Save → `dictionary saved.`; re-open Settings and
   confirm the pairs round-tripped byte-exact (diacritics intact).
4. To exercise the **server-side** error path, PUT a structurally invalid body via
   the daemon directly and confirm the shell surfaces `dictionary rejected: …`
   (the shell drops blank rows and validates first, so this normally can't happen
   from the UI).

## 6. Live indication (level meter + partial transcript, D-0013 §4)

1. Start a dictation session and speak.
2. **Expect:** the "Recording" banner shows a level meter driven by the
   `audio.level` WS event (`rms`/`peak`) and the latest `partial_transcript`
   text below it, updating live with correct Czech diacritics.
3. If the daemon build does **not** emit `audio.level` yet (parallel work item),
   the meter stays at zero — the rest of the banner (partial text, red dot) still
   works. This is expected, not a bug.

## 7. Onboarding (first run)

1. Delete `~/Library/Application Support/whisper-tray/state.json` and relaunch.
2. **Expect:** the window opens on the **Onboarding** page with a 4-step
   checklist: daemon running (**Check daemon** → `GET /v1/health`/status), mic
   permission (text + "I granted it"), accessibility permission (text +
   "I granted it"), test dictation (**Start test** → speak → "Heard: …").
3. Click **Finish** (or **Skip for now**) → the window switches to Settings and
   `state.json` records `completed: true`; relaunching goes straight to Settings.

## 8. Close-to-tray

1. Close the window (red traffic-light / Cmd-W). **Expect:** the window **hides**
   (does not quit); the tray icon remains. Re-open via **Settings…**.
2. **Quit** from the tray menu terminates the process (the tray icon disappears).

## Automated coverage (run in CI, no GUI)

```bash
cargo test -p whisper-ux            # 39 tests: parsing, state machine, client, icons, onboarding, dict
cargo clippy -p whisper-ux --all-targets -- -D warnings
cargo build -p whisper-ux --bin whisper-tray   # the tray binary links
```

## Known limits (see the implementation report)

- **Accessory activation policy** is compile-verified only; the dockless behavior
  needs a real desktop to confirm (see step 1).
- **Blocking HTTP on the UI thread:** config/session calls run inline with ureq.
  On localhost this is sub-millisecond; against a slow/remote daemon the frame
  would stutter. A future async/worker-thread refactor is deferred (MVP).
- **`audio.level` event** is consumed per the agreed shape but is emitted by a
  parallel daemon work item; until it lands the meter reads zero (step 6).
- **Tray creation is best-effort:** if the status bar cannot be created (headless
  session), the shell logs nothing and runs windowed without a tray rather than
  crashing.
- The shell holds **no config of its own**; URL/token come from
  `WHISPERD_URL`/`WHISPERD_TOKEN`. The "Daemon URL" field in Settings is
  informational (changing the endpoint means relaunching with a new env var).
