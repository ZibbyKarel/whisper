# Real-microphone Czech validation — dev dictation

Date: 2026-07-10 · Input: `dev-diktat.m4a` (user recording, 31.7 s, AAC 48 kHz mono →
`fixtures/audio/real/dev-diktat.wav`, 16 kHz PCM). Content: spontaneous Czech dev
dictation, heavy code-switching (endpoint, memory leak, garbage collector, feature
branch, unit testy, pull request, middleware, JSON payload, Kubernetes pod, timeout).
No written reference existed; outputs were cross-compared and inspected manually.

## Transcripts (full audio, `-l cs`)

**large-v3-turbo q5_0** (~0.9 s): near-perfect. Errors: "Pušnit to" (pushni to),
"spust" (spusť) — both custom-vocab class. Diacritics correct throughout. Plus the
hallucination (below).

**large-v3 q5_0** (~1.5 s): cleanest raw output — "Pušni to" correct; otherwise identical
to turbo. Plus the hallucination.

**mikr-large-v3-czech-cv13 q5_0**: *worse than stock on real dev speech* —
"refakturovat" (semantic error for refaktorovat), "jason payload" (JSON lost),
over-capitalizes English terms ("Feature branch", "Unit testy", "Pull request").
Its synthetic-fixture edge did not transfer. Plus the hallucination.

**parakeet-tdt-0.6b-v3 int8** (1.6 s ≈ 19× realtime): collapses on real code-switched
speech — "půl request", "Gybernés pot" (Kubernetes pod), "MILB" (middleware), "JSO plou"
(JSON payload), and a **meaning inversion**: "Zapoměň zkontrolovat" (dropped the
negation of "Nezapomeň"). Confirms the round-2 code-switching blocker on real audio.
Notably it did NOT hallucinate on the trailing silence.

## Hallucination on trailing silence — confirmed experimentally

All three whisper models appended **"Titulky vytvořil JohnyX."** — the canonical Czech
Whisper hallucination (YouTube subtitle credits from training data). `silencedetect`
shows speech ends at 28.6 s followed by 3.07 s of silence; re-running turbo on audio
trimmed to 28.9 s removes the hallucination completely.

**Pipeline consequence (hard requirement for Phase 2/3):** VAD must trim leading/trailing
silence before STT — whisper must never see multi-second silence windows. This turns the
Silero-VAD plan from "best practice" into a measured requirement. Parakeet's
silence-robustness (trained on 36 000 h of non-speech) is confirmed in practice.

## Verdict — validation gate PASSED

1. **large-v3-turbo q5_0 is confirmed as the default STT model**: real spontaneous Czech
   dev dictation with heavy code-switching transcribed near-perfectly (diacritics intact;
   residual errors are the custom-vocabulary class the dictionary/cleanup stage owns).
2. large-v3 q5_0 confirmed as the max-quality profile (marginally cleaner).
3. mikr fine-tune drops out of default consideration (real-speech regressions + ITN loss).
4. Parakeet stays a monolingual low-latency option only; unusable for the code-switching
   requirement.
5. New measured requirement: VAD silence-trimming before STT (anti-hallucination).

## Limitations

- n=1 recording, one speaker, one mic, quiet environment; no reference transcript
  (manual inspection). Enough to unblock the model ADR (synthetic + real evidence now
  agree), not a formal WER measurement. More recordings (noise, other speakers) belong
  to Phase 3 testing.
