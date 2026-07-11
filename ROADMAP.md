# Roadmap

Local-first, modular voice intelligence engine (Wispr Flow–inspired), Czech-first, built as an embeddable platform component.

**Current position: Phase 6 hardened + Phase 7 packaging started (2026-07-11, feedback round 3 shipped). The app now has: Wispr Flow-style floating waveform overlay while dictating (top/bottom configurable), waveform menu-bar glyphs, generated app icon, Inter typography + sidebar UI, dictation-language picker, full-config Settings, Ctrl+Alt PTT default, Whisper.app/DMG packaging with tray-managed whisperd (D-0014). ⚠️ Manual verification pending (docs/install.md + phase5/6 checklists): Czech at normal volume, PTT into other apps, TCC grants, overlay behavior on the real desktop. Remaining: Fn/globe key, notarization decision, rest of Phase 7 hardening.**

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

## Phase 3 — Core Engine MVP ✅ (2026-07-10)
- Audio capture → VAD → STT (streaming) → raw transcript events
- Text cleanup + formatting pipeline (rule-based first, LLM optional)
- Headless, fully testable with fixture audio

## Phase 4 — Interface Layer ✅ (2026-07-10)
- Local HTTP + WebSocket API, CLI, event stream
- External orchestrator can start/stop, subscribe, inject processing steps (transform hook deferred past MVP per §4)

## Phase 5 — macOS Platform Layer 🔶 implemented, pending manual verification (2026-07-10)
- Global hotkey (push-to-talk / toggle), mic permissions, text insertion into focused app — done behind `platform` feature; menu-bar presence → Phase 6; Fn/globe key → follow-up work item
- Sign-off gate: `docs/phase5-manual-verification.md` (requires user-granted TCC permissions)

## Phase 6 — UX Layer + polish 🔶 MVP implemented, pending manual verification (2026-07-10)
- Settings, onboarding, visualization; replaceable UI shell — `whisper-tray` (D-0013), API-only, needs no TCC
- Sign-off gate: `docs/phase6-manual-verification.md`; polish items tracked in HANDOFF

## Phase 7 — Hardening & release 🔶 packaging + model provisioning landed (2026-07-11)
- ✅ Packaging: Whisper.app bundle + DMG, tray-managed daemon lifecycle, install docs (D-0014)
- ✅ In-app model provisioning (D-0015): daemon-side catalog/download/verify over `/v1`
  (`GET /v1/models`, `POST`/`DELETE /v1/models/{id}/download`, `model.download` WS event);
  tray startup gate + reworked Settings → Models + onboarding checklist step, all backed by
  one shared `ModelsPanel`/`ui_model_row` component
- Remaining: deploy checklist, app icon + UI polish round 2, performance/latency tuning, plugin system maturity, signing/notarization decision, resumable downloads (deferred by D-0015)

Milestone rule: after every milestone — update this file + CHANGELOG, commit.
