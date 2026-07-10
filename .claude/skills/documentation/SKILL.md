---
name: documentation
description: Use when writing or updating project documentation — defines which document owns which content and the update triggers that keep docs current across autonomous sessions.
---

# Documentation

Docs are this project's persistent memory across sessions. Stale docs are worse than none.

## Document ownership

| File | Owns | Update trigger |
|---|---|---|
| `README.md` | what this is, quickstart, install | any user-facing change |
| `ROADMAP.md` | phases, milestones, current position | milestone start/finish |
| `ARCHITECTURE.md` | layers, modules, diagrams, data flow, interfaces | any structural change |
| `DECISIONS.md` | ADRs (see architecture-adr skill) | every significant decision |
| `PROGRESS.md` | running log: date, what was done, what's next | end of every work session |
| `HANDOFF.md` | state snapshot for the next run | before ending any run |
| `docs/` | deep dives: research, API reference, model evaluation | with the feature |
| code doc-comments | public interfaces of each module | with the code |

## Rules

- Write for a reader with zero conversation context — spell out codenames and shorthand.
- Every public API endpoint/event gets: purpose, request/response schema, one example.
- Keep one source of truth: link between docs instead of duplicating content.
- PROGRESS.md entries are append-only and dated (absolute dates, never "today").
- When behavior and docs disagree, fixing the doc is part of the change, not a follow-up.
