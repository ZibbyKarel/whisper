# Roadmap

Local-first, modular voice intelligence engine (Wispr Flow–inspired), Czech-first, built as an embeddable platform component.

**Current position: Phase 2 (architecture) ✅ designed & reviewed (2026-07-10) — ARCHITECTURE.md full design + ADRs D-0004..D-0010, adversarial cross-review addressed. Next: Phase 3 Core Engine MVP (Rust workspace scaffold → capture→VAD→STT pipeline on fixtures).**

## Phase 0 — Environment & tooling bootstrap ✅ (2026-07-10)
- Capability audit (`docs/CAPABILITY_AUDIT.md`), local skills, subagents, runtimes, memory files, verification gate.

## Phase 1 — Research ✅ (2026-07-10)
- Wispr Flow product/UX/pipeline analysis
- Local STT comparison with Czech focus (whisper.cpp large-v3, faster-whisper, alternatives) — measured on Czech fixtures
- Local LLM comparison for Czech text cleanup (Qwen, Gemma, Mistral tiers on 24 GB budget)
- VAD, audio capture, macOS text-injection methods
- Deliverable: `docs/RESEARCH.md` + Czech fixture set + model shortlist with measured WER

## Phase 2 — Architecture ✅ (2026-07-10)
- Full design: layers, APIs, data flow, failure paths, testing seams (`ARCHITECTURE.md`)
- Czech validation locked into ADRs D-0004 (STT), D-0005 (LLM); D-0006..D-0010 (stack, insertion, VAD, interface, storage)
- Adversarial cross-review (16 findings) addressed

## Phase 3 — Core Engine MVP
- Audio capture → VAD → STT (streaming) → raw transcript events
- Text cleanup + formatting pipeline (rule-based first, LLM optional)
- Headless, fully testable with fixture audio

## Phase 4 — Interface Layer
- Local HTTP + WebSocket API, CLI, event stream
- External orchestrator can start/stop, subscribe, inject processing steps

## Phase 5 — macOS Platform Layer
- Global hotkey (push-to-talk / toggle), mic permissions, text insertion into focused app, menu-bar presence

## Phase 6 — UX Layer + polish
- Settings, onboarding, visualization; replaceable UI shell

## Phase 7 — Hardening & release
- Deploy checklist, packaging, docs, performance/latency tuning, plugin system maturity

Milestone rule: after every milestone — update this file + CHANGELOG, commit.
