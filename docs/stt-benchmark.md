# STT benchmark — whisper.cpp on Czech fixtures (round 1)

Date: 2026-07-10 · Hardware: Apple M5 (Metal), 24 GB · Runtime: whisper-cpp 1.9.1, q5 quantized models
Fixtures: 6 synthetic TTS clips (see `fixtures/audio/manifest.json`), raw data `docs/stt-benchmark.json`.

## Raw results (WER, diacritics-preserving normalization)

| Model | Size | avg WER | cs | cs+en mix | en | s/clip |
|---|---|---|---|---|---|---|
| large-v3 q5_0 | 1.0 GB | 0.18 | 0.18 | 0.28 | 0.00 | ~1.5 |
| large-v3-turbo q5_0 | 547 MB | 0.20 | 0.23 | 0.25 | 0.00 | ~0.8 |
| medium q5_0 | 514 MB | 0.26 | 0.35 | 0.27 | 0.00 | ~0.9 |
| small q5_1 | 181 MB | 0.31 | 0.38 | 0.37 | 0.00 | ~0.4 |

## Why raw WER overstates errors — hypothesis inspection

- **cs-02**: every model rendered "dvě stě třicet sedm" as **"237"** — that is correct inverse text normalization (desirable for dictation!), but scores as 3 word errors. Large models' only real cs-02 deviation was casing of "Ústí nad Labem". Effective large-v3/turbo error rate on plain Czech prose ≈ near zero (cs-03 was verbatim-perfect for both).
- **cs-01** is the adversarial pangram ("Příliš žluťoučký kůň úpěl ďábelské ódy") — rare words; errors were single-diacritic slips (kuň, upěl) and "ďábelské"→"dvábelské". Worst case, not typical prose.
- **mix (code-switching)**: large/turbo preserved English tech terms well ("branch", "pull request", "npm install"); failures concentrated on "package.json" — which the Czech TTS voice itself pronounces with a heavy Czech accent, so partly a fixture artifact. Czechified verbs ("mergnout") are near-homophones and will need custom-dictionary/post-processing support regardless of model.
- **medium/small degrade Czech specifically**: dropped/warped diacritics (ody, jábelské, výskum, růstvu) and mangled English terms ("půl rekvest"). Confirms the mission's warning that Czech needs the large tier.

## Conclusions (provisional until real-speech validation)

1. **large-v3-turbo q5_0 is the working default**: near-large Czech quality at ~2× speed and half the memory (547 MB). large-v3 q5_0 stays as a "max quality" profile.
2. medium/small are acceptable only as an explicit low-latency/low-memory profile with documented Czech quality loss; small is out.
3. Whisper already performs number ITN ("237"), casing and punctuation — the cleanup LLM's job is smaller than assumed: casing of proper names, custom vocabulary, disfluency removal, commands.
4. Benchmark harness must gain an ITN-aware scoring mode (count digit-forms of spoken numbers as correct).

## Known limitations of this round

- TTS speech (clean, accent-consistent) is an upper bound vs. real dictation with disfluencies and mic noise.
- n=6 clips; rare-word pangram skews cs averages.
- Real-microphone Czech recordings must validate the finalist before the model choice is locked into an ADR (mission requirement).
- Not yet compared: faster-whisper / MLX-whisper backends (same models, different runtime perf) — Phase 2 decision input.
