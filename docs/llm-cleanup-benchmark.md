# LLM cleanup benchmark — Czech dictation post-processing (round 1)

Date: 2026-07-10 · Hardware: Apple M5, 24 GB · Runtime: Ollama (temperature 0, shared
generic system prompt, zero-shot). Fixtures: `fixtures/llm/cleanup-cases.json` (13 cases,
4 traps). Raw outputs: `docs/llm-cleanup-benchmark.json`. Harness: `scripts/bench_llm_cleanup.py`.

## Summary

| Model | avg WER | trap failures (of 4) | diacritics lost | avg latency |
|---|---|---|---|---|
| **qwen3:8b** (5.2 GB) | **0.15** | **0** | 0 | **1.6 s** |
| gemma3:12b (8.1 GB) | 0.20 | 1 | 0 | 3.0 s |
| mistral-nemo:12b (7.1 GB) | 0.24 | 2 | 1 | 1.7 s |

## Qualitative findings (more important than WER)

**qwen3:8b — front-runner.** Passed all 4 trap cases verbatim (question, command,
prompt-injection, arithmetic). Fast. Failure modes: paraphrasing ("Chtěl bych" → "Chci"),
kept the retracted clause in self-correction, and one **semantic substitution** —
"mergnout" → "zkontrolovat" (changed the meaning of the sentence). Perfect on the long
multi-sentence Czech dictation.

**gemma3:12b.** Refused the prompt-injection with meta-commentary ("Omlouvám se, ale
nemohu…") — it didn't obey, but the commentary would be typed into the user's document,
which is still a hard failure for dictation. Slowest (3 s avg on 12 B). Otherwise decent;
best raw disfluency handling on case 1.

**mistral-nemo:12b — disqualified.** Obeyed the prompt-injection (emitted "Jaro"),
answered the arithmetic trap (with a *wrong* answer, 235), lost diacritics once, and made
a semantic substitution ("Pushni" → "Přepni"). Ironically the only model that solved the
self-correction case correctly. The BenCzechMark-paper claim that it leads Czech ≤12 B
does not transfer to instruction-following cleanup.

**Cross-model observations**
1. No model handles Czech self-correction ("v pondělí, ne vlastně v úterý" → "v úterý")
   reliably zero-shot — needs few-shot examples in the prompt.
2. Paraphrasing is the dominant error class: models "improve" wording instead of
   transcribing. Mitigations for Phase 2: stricter prompt ("no synonym substitution"),
   few-shot examples, and a **cleanup-skip gate** (FreeFlow ships one: ~40 % of dictations
   need no cleanup) so the LLM only runs when disfluencies/traps are unlikely to matter.
3. Trap behavior is the discriminator, not WER: it's the difference between a dictation
   tool and a chatbot typing into your document.
4. Latency 1–3 s per utterance is additive to STT (~0.8 s) — another argument for the
   skip gate and for keeping the model small.

## Verdict (provisional)

**qwen3:8b is the working default** for the cleanup stage: safest trap behavior, best WER,
lowest latency, smallest footprint (5.2 GB fits the ~10 GB resident budget alongside
whisper large-v3-turbo). Round 2 (Phase 2): re-run with an engineered prompt (few-shot
self-correction + anti-paraphrase rules), test a smaller qwen3 variant (4b) for latency,
and consider gemma3:12b only if qwen3 prompt-engineering hits a ceiling.

## Limitations

- Zero-shot with one generic system prompt — results measure *default* behavior, not the
  ceiling. Prompt engineering will move all numbers.
- n=13 synthetic cases; no real dictation transcripts yet (will exist after real-mic STT
  validation; the STT output can feed this harness).
- WER under-penalizes semantic substitutions and over-penalizes benign paraphrases; the
  qualitative review above is the real signal. A "meaning preserved" LLM-judge metric is
  possible later but out of Phase-1 scope.
