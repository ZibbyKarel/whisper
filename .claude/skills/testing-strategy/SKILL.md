---
name: testing-strategy
description: Use when planning tests for a new module or reviewing test coverage — defines the project's test pyramid, fixtures for audio/STT, and the Czech-language test requirements.
---

# Testing Strategy

## Test pyramid for this project

1. **Unit** — pure logic: text cleanup, formatting, command detection, config. Fast, no I/O, run on every commit.
2. **Component** — one module with real I/O but fake peers: STT adapter against recorded WAV fixtures; API layer against a fake engine.
3. **Integration** — pipeline end-to-end with fake audio input and a real (small) model; run on demand or CI-nightly.
4. **Manual/perceptual** — real-microphone dictation sessions; latency measurements; Czech quality scoring.

## Hard rules

- Every text-processing test suite MUST include Czech cases: full diacritics ("Příliš žluťoučký kůň úpěl ďábelské ódy"), code-switching ("Pushni ten branch do repa a udělej pull request"), punctuation/number formatting in Czech conventions.
- STT quality is measured, not eyeballed: keep a fixture set of Czech + English + mixed WAV samples with reference transcripts; compute WER/CER per model and record results in DECISIONS.md.
- No test may require network access or cloud services.
- Hardware-dependent code (mic capture, hotkeys, injection) sits behind interfaces so the rest tests headlessly; the hardware shims get thin manual smoke tests.
- A feature is "done" only when: compiles, tests pass, lint passes, docs updated, reviewed by a different agent than the implementer (mission quality gate).

## Fixtures

Keep audio fixtures small (seconds, 16 kHz mono WAV) in `fixtures/audio/` with a `manifest.json` mapping file → language → reference transcript. Generated/recorded fixtures are committed; large model files are never committed.
