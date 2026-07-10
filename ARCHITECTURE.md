# Architecture

> Status: skeleton. To be filled by Phase 2 after research. Layer contract below is binding from the mission and already governs design.

## Target layering

```
┌─────────────────────────────────────────────┐
│ UX Layer          settings · onboarding · viz │  replaceable
├─────────────────────────────────────────────┤
│ Interface Layer   HTTP · WebSocket · CLI ·   │  primary integration point
│                   events · plugins           │
├─────────────────────────────────────────────┤
│ Core Engine       capture · VAD · STT ·      │  no UI, no OS specifics
│                   cleanup · formatting ·     │
│                   commands · context         │
├─────────────────────────────────────────────┤
│ Platform Layer    mac/win/linux · perms ·    │  plugs into Core via ports
│                   mic · hotkeys · tray ·     │
│                   text injection             │
└─────────────────────────────────────────────┘
```

Rules (binding):
- Dependencies point downward only; cross-layer access goes through explicit interfaces.
- Core Engine must run headless and fully offline.
- Every capability must be reachable through the Interface Layer without any UI.
- Czech/UTF-8 correctness end to end.

## To be decided in Phase 2 (each gets an ADR)

- Implementation language(s) and process model
- STT engine + model tier (Czech-validated)
- Optional LLM cleanup model + runtime (Czech-validated)
- API schema & event protocol
- Storage (config, history, dictionary)
- Text injection mechanism per OS
- Packaging & update strategy
