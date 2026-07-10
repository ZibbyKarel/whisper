---
name: system-design
description: Use when designing or changing system structure — new modules, layer boundaries, APIs, protocols, data flow — to keep the design modular, local-first, and integration-ready.
---

# System Design

Checklist for any structural design work on this project.

## Layer discipline (from the mission)

1. **Core Engine** — audio capture, VAD, STT, post-processing, formatting, command detection, context. Zero UI dependencies. Zero platform-specific code.
2. **Interface Layer** — HTTP/WebSocket/CLI/events. The only integration point for external systems.
3. **Platform Layer** — OS specifics: permissions, mic access, global shortcuts, text injection, tray.
4. **UX Layer** — settings, onboarding, visualization. Must be replaceable without touching lower layers.

Dependencies point downward only (UX → Interface → Core; Platform plugs into Core via defined ports). Anything crossing layers goes through an explicit interface.

## For every design, answer in writing (ARCHITECTURE.md)

- What is the module's single responsibility? What is explicitly out of scope?
- What interface does it expose (types, protocol, events)? Could an external orchestrator drive it with no UI present?
- How does data flow through it, including error and cancellation paths?
- What happens offline? (Must be: everything works.)
- How is Czech/UTF-8 text handled end to end?
- How is it tested without hardware (mic) present — what seam allows fake audio/fake STT?
- Latency budget: dictation feels broken above ~1–2 s end-to-end; where does this design spend it?

## Red flags

- Core Engine importing UI or OS APIs directly
- A feature reachable only through the UI (breaks the "embeddable engine" requirement)
- A dependency that cannot run offline
- Synchronous coupling where an event would do
