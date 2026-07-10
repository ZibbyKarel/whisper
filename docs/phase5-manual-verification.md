# Phase 5 — macOS platform layer: manual verification checklist

> Why manual: every path here needs a macOS TCC grant (Microphone, Accessibility, Input
> Monitoring) that CI cannot provide (D-0012). The headless gates cover all decision logic with
> fakes; this checklist covers the hardware seams they cannot.

Build the platform-enabled daemon:

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build -p whisper-iface --features platform --bin whisperd
```

Enable delivery + hotkey in `~/.config/whisper-engine/config.toml`:

```toml
delivery = "paste"        # inject via pasteboard + Cmd-V (D-0007)

[hotkey]
enabled = true
combo = "Ctrl+Alt+Space"  # default; push-to-talk
mode = "push_to_talk"     # or "toggle"
```

Grant permissions in System Settings → Privacy & Security:
- **Microphone** → the terminal running `whisperd` (or the `whisperd` binary).
- **Accessibility** → same (needed to synthesize Cmd-V).
- **Input Monitoring** → same (needed for the global hotkey listener).

The first mic/hotkey use will prompt if not pre-granted; grant, then restart `whisperd`.

## 1. Live dictation in Czech (mic → STT → paste)
1. Start the daemon with the real models present in `models/` and the config above.
2. Open a plain text field (e.g. TextEdit) and focus it.
3. Start a session over HTTP (or use the hotkey, step 4):
   ```bash
   curl -s -XPOST localhost:8737/v1/sessions -d '{"source":"mic"}'
   ```
4. Dictate a Czech sentence with full diacritics, e.g.
   *"Příliš žluťoučký kůň úpěl ďábelské ódy."* plus an English tech term
   (*"pushni to do mainu"*) to exercise code-switching.
5. Stop the session:
   ```bash
   curl -s -XPOST localhost:8737/v1/sessions/<id>/stop
   ```
6. **Expect:** the text is pasted into the focused field, **byte-exact with diacritics**
   (no `z`+combining-caron NFD corruption), and the previous clipboard is restored after ~300 ms.

## 2. Hotkey (push-to-talk and toggle)
1. Push-to-talk: hold `Ctrl+Alt+Space`, speak, release → a session runs while held and the text is
   pasted on release.
2. Set `mode = "toggle"`, restart: press once to start, press again to stop.
3. **Expect:** WS `session.started` / `delivered` events on `ws://localhost:8737/v1/events`.

## 3. Secure Input (never bypass)
1. Focus a macOS **password field** (login window, `sudo` in Terminal with secure keyboard entry,
   or a Keychain prompt) — this enables Secure Input system-wide.
2. Run a dictation session as in step 1 while the secure field holds focus.
3. **Expect:** the text is **not** typed; instead it is left on the clipboard, and a WS
   `secure_input.blocked` event is emitted. Session status is `completed` (clipboard-only is a
   fallback, not a failure). Verify by pasting manually — the diacritics are intact (NFC).

## 4. Device unplug (device loss mid-session)
1. Start a mic session on a **Bluetooth or USB** microphone.
2. Mid-utterance, disconnect it (power off the Bluetooth mic / unplug the USB device).
3. **Expect:** a WS `audio.device_lost` event, and the session ends `failed` carrying the partial
   transcript decoded before the drop (raw transcript beats nothing, `ARCHITECTURE.md` §3.2).

## 5. Microphone permission denied
1. Revoke Microphone permission for `whisperd` (System Settings → Privacy → Microphone → off) and
   restart.
2. Start a mic session.
3. **Expect:** a WS `platform.permission` event (`permission: "microphone", granted: false`) and a
   `failed` session — no blocking dialog inside the engine (`ARCHITECTURE.md` §5).

## Hardware smoke tests (one-off, per permission)
Focused `#[ignore]` tests exercise each seam in isolation; run one at a time after granting the
matching permission:

```bash
cargo test -p whisper-platform --test hardware -- --ignored mic_capture_opens_and_streams
cargo test -p whisper-platform --test hardware -- --ignored pasteboard_sink_injects_czech
cargo test -p whisper-platform --test hardware -- --ignored secure_input_query_runs
cargo test -p whisper-platform --test hardware -- --ignored hotkey_listener_reports_events
```

## Known limits (see the implementation report / ADRs)
- Secure Input **owner app name** is not reported (needs a private API,
  `kCGSSessionSecureInputPID`); `secure_input.blocked.owner` is `null` on the real sink.
- Clipboard restore uses a fixed 300 ms delay — macOS exposes no paste-completion signal.
- Fn/globe-key capture is out of scope (needs `flagsChanged`/IOKit, `ARCHITECTURE.md` §8).
- The permission classifier is heuristic (matches cpal error text); step 5 confirms the real path.
