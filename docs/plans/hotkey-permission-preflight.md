# Plan — Fix silent hotkey failure after app update (Input Monitoring / Accessibility preflight)

Status: ready for implementation (sonnet-implementer). Author: Fable. Date: 2026-07-11.

## Problem (root cause, evidence-backed)

User installed a new build of `Whisper.app`; `Ctrl+Alt` push-to-talk no longer triggers
dictation into other apps.

Investigation (this session) established, with evidence from the user's machine:

- Loaded config `~/Library/Application Support/whisper-engine/config.toml` is **correct**:
  `[hotkey] enabled = true, combo = "Ctrl+Alt", mode = "push_to_talk"`.
- STT model loads fine (large-v3, 573 MB in `whisperd.log`) → **not** a model-gate problem.
- Installed `whisperd` (running, PID observed) **is** built with `--features platform`
  (contains `whisper-hotkey`, rdev `EventTapError`, the "hotkey listener failed to start"
  string) → the listener code is compiled and reached.
- The bundle is **ad-hoc signed**: `codesign` shows `flags=0x2(adhoc)`, `Signature=adhoc`,
  `TeamIdentifier=not set`, `Identifier=cz.zibby.whisper`.
- `whisperd.log` contains startup + model load but **no** "hotkey listener failed" line and
  **no** rdev error — even though stderr is captured (ggml output is in the same log). So
  `rdev::listen` returned `Ok`, not `Err`.

**Mechanism:** ad-hoc signatures have no stable code identity, so after each reinstall macOS
treats the bundle as a *new* program and the prior **Input Monitoring** (and **Accessibility**)
TCC grants do not carry over. rdev's listen-only `CGEventTapCreate` **succeeds without the
grant** but then receives **zero** key events. Because `rdev::listen` returns `Ok` in this
state, the existing `on_error` path in `platform.rs` never fires — the failure is **completely
silent**: nothing on stderr, no `platform.permission` WS event, no tray indication. The user
just sees "Ctrl+Alt does nothing." Paste into other apps additionally needs **Accessibility**,
subject to the identical churn.

We cannot make ad-hoc signing keep the grant (that needs a paid Developer ID / notarization,
deferred per D-0014). The durable fix is to **detect the missing grant proactively and surface
it with a one-click remediation**, closing the silent-failure gap so this class of problem is
always visible and self-explanatory to the user after any update.

## Goal

After an update where Input Monitoring / Accessibility is not (yet) granted to the new bundle:
1. whisperd **detects** the missing grant proactively (not relying on an rdev `Err`).
2. The tray shows a **persistent, actionable banner** naming the exact permission and a button
   that deep-links to the right System Settings pane.
3. Onboarding **verifies** the real grant instead of only asking the user to tick a checkbox.
4. Docs tell the user the update-caveat + exact re-grant steps.

Keep layering intact: platform FFI in `whisper-platform`; whisperd exposes status over `/v1`;
tray is `/v1`-only (no direct OS calls).

## Implementation

### 1. `whisper-platform` — real TCC preflight (macOS)

New module `crates/whisper-platform/src/permission.rs` (follow the existing raw-FFI idiom in
`paste.rs`: `#[link(name = "...", kind = "framework")] extern "C"`).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access { Granted, Denied, Unknown }
```

- **Input Monitoring** via IOKit:
  `IOHIDCheckAccess(IOHIDRequestType) -> IOHIDAccessType`, request type
  `kIOHIDRequestTypeListenEvent = 1`; result `kIOHIDAccessTypeGranted = 0`,
  `kIOHIDAccessTypeDenied = 1`, `kIOHIDAccessTypeUnknown = 2`. Framework: `IOKit`.
  Also expose `pub fn request_input_monitoring() -> bool` wrapping
  `IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)` (triggers the OS prompt; call only on an
  explicit user action, never in a loop).
- **Accessibility** via ApplicationServices:
  `AXIsProcessTrusted() -> Boolean` (query, no prompt) and, for an explicit user action,
  `AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean` with
  `kAXTrustedCheckOptionPrompt` set to true. Framework: `ApplicationServices`.
  `AX*` distinguishes only trusted/untrusted → map to `Granted`/`Denied` (no `Unknown`).
- **Microphone** is optional here (cpal already surfaces its own failure) — skip unless trivial.

Public API:
```rust
pub fn input_monitoring_access() -> Access;   // IOHIDCheckAccess(listen)
pub fn accessibility_access() -> Access;      // AXIsProcessTrusted
pub fn request_input_monitoring() -> bool;    // IOHIDRequestAccess(listen)
pub fn request_accessibility() -> bool;       // AXIsProcessTrustedWithOptions(prompt)
```
Provide a non-macOS stub returning `Access::Unknown` / `false` so the crate builds everywhere
and the pure logic is unit-testable. SAFETY comments on every FFI call (parameterless queries,
no side effects except the explicit `request_*` prompts). Re-export from `lib.rs`.

### 2. `whisper-iface` — close the silent gap + expose status over /v1

- In `daemon/platform.rs::spawn_hotkey`, **before** spawning the rdev listener, call
  `input_monitoring_access()`. If it is not `Granted`, log the same actionable stderr message
  and emit `ApiEvent::PlatformPermission { permission: "input_monitoring", granted: false, .. }`
  **immediately** (this is what fixes the silent case — the event now fires without waiting for
  an rdev `Err` that never comes). Still spawn the listener (a later grant + restart recovers).
- Add `GET /v1/platform/permissions` handler returning current status, e.g.
  `{ "input_monitoring": "granted|denied|unknown", "accessibility": "granted|denied|unknown" }`.
  Under a non-`platform` build, return `unknown` for all. Mirror the DTO in the three contract
  places called out in HANDOFF item 8 (ux/api.rs ↔ daemon/api.rs) and add a JSON fixture test.
- Keep the existing `on_error` path (it still catches genuine rdev `Err`s).

### 3. `whisper-ux` (tray) — make it visible + actionable

- Poll `GET /v1/platform/permissions` on the same cadence as the models poll (see
  `gui.rs` model poll around lines 395–406) — store latest status.
- Add a **persistent banner** in the main window (reuse the `ui_recording_banner` styling as a
  reference) shown whenever `input_monitoring != granted` (headline) or
  `accessibility != granted` (paste won't work): explain the effect and render a button
  **"Open System Settings"** that opens the correct pane via `open`:
  - Input Monitoring: `x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent`
  - Accessibility: `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`
  Plus a short "After granting, restart Whisper" note (listener starts once at daemon startup).
- Upgrade onboarding steps 3/4 (`gui.rs` ~1347–1370): replace the ack-only checkboxes for
  Input Monitoring and Accessibility with **live status** from the endpoint + a "Re-check"
  button + the deep-link button. Keep a manual override if status is `unknown`.

### 4. Docs + cleanup + ADR

- `docs/install.md`: add a prominent **"After updating the app"** note — because the app is
  ad-hoc signed, macOS may drop Input Monitoring & Accessibility grants on update; steps:
  System Settings › Privacy & Security › Input Monitoring (and Accessibility) → remove the old
  `Whisper`/`whisperd` entry if present, re-add / re-enable, then quit and relaunch Whisper.
- Fix stale `Ctrl+Alt+Space` → `Ctrl+Alt` in repo-root `config.toml` and in
  `crates/whisper-platform/tests/hardware.rs` (comments at ~lines 77, 86).
- Add an ADR to `DECISIONS.md` (use `.claude/skills/architecture-adr`): "Proactive macOS
  permission preflight + /v1 status surface for the hotkey/paste path" — context (ad-hoc
  signing churn + rdev Ok-but-silent), decision, alternatives (rely on rdev Err only — rejected
  as it misses the dominant failure mode; a helper process — already rejected in D-0012).

## Quality gates (constitution)

- `cargo build --features platform` and default build both compile.
- `cargo test` passes, including a new unit test for the `Access` mapping / non-macOS stub and
  the new `/v1/platform/permissions` JSON fixture. Czech cases unaffected but keep existing
  Czech tests green.
- `cargo clippy` clean; docs updated (install.md, DECISIONS.md, PROGRESS.md checkpoint).
- The FFI cannot be unit-tested for the real grant (needs TCC) — add a `#[ignore]` hardware
  test in `whisper-platform/tests/hardware.rs` that prints `input_monitoring_access()` /
  `accessibility_access()` for the manual pass, matching the existing hardware-test pattern.

## Out of scope (do not do)

- Developer ID signing / notarization (deferred, D-0014) — the actual root of the churn.
- Fn/globe key capture (separate work item).
- Changing the rdev backend to CGEventTap directly (watched risk, not this fix).
