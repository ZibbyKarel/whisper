# STT benchmark — round 2: Czech fine-tune + Parakeet vs. round-1 whisper.cpp

Date: 2026-07-10 · Hardware: Apple M5 (Metal), 24 GB · whisper-cpp 1.9.1, sherpa-onnx 1.13.4
Fixtures: same 6 synthetic TTS clips as round 1 (`fixtures/audio/manifest.json`).
Raw data: `docs/stt-benchmark-round2.json`. Runner: `scripts/bench_stt_round2.py`
(normalize/WER/CER copied verbatim from `scripts/bench_stt.py` — diacritics-preserving).

## Candidates

- **A — mikr/whisper-large-v3-czech-cv13** (HF, Apache-2.0). Czech fine-tune of
  whisper-large-v3. Converted HF→GGML F16 with whisper.cpp `convert-h5-to-ggml.py`,
  quantized to **q5_0** with `whisper-quantize`. Size: F16 2952 MB, q5_0 **1031 MB**
  (full large-v3, 32 decoder layers — same tier as round-1 large-v3 q5_0, *not* turbo).
- **B — Parakeet TDT 0.6B v3** (sherpa-onnx, NeMo, int8 ONNX). Run through the
  `sherpa_onnx.OfflineRecognizer` transducer API. Size: encoder+decoder+joiner int8 **≈ 640 MB**.

## Results (WER, diacritics-preserving; round-1 rows included for comparison)

| Model | Size | avg WER | cs | cs+en mix | en | s/clip |
|---|---|---|---|---|---|---|
| **mikr-large-v3-czech-cv13 q5_0** (A) | 1031 MB | **0.17** | **0.14** | **0.22** | 0.17 | 1.44 |
| large-v3 q5_0 (round 1) | 1031 MB | 0.18 | 0.18 | 0.28 | 0.00 | ~1.5 |
| large-v3-turbo q5_0 (round 1) | 547 MB | 0.20 | 0.23 | 0.25 | 0.00 | ~0.8 |
| **parakeet-tdt-0.6b-v3 int8** (B) | ~640 MB | 0.22 | 0.18 | 0.38 | 0.00 | **0.15** |

Latency caveat: whisper `s/clip` includes per-call whisper-cli process + Metal init
(same method as round 1, so A is comparable to round-1 rows). Parakeet `s/clip` is
in-process decode only (model loaded once), so the two backends' latencies are not
strictly apples-to-apples — but Parakeet is genuinely ~5–10× faster per decode.

## Per-fixture inspection (interesting errors)

**Candidate A (mikr fine-tune):**
- **cs-01** (pangram): `kuň`→kůň, `upěl`→úpěl (two single-diacritic slips), but it
  rendered **`ďábelské ódy` correctly** — the exact word every round-1 model botched
  (`dvábelské`/`jábelské`). CER 0.05, best on this clip. WER 0.33 is 2 slips in a
  6-word rare-word pangram, not representative prose.
- **cs-02**: it did **not** perform number ITN — wrote `dvěstě třicet sedm` (spelled
  out, matching the reference) instead of `237`. So it scores *better* here (WER 0.10)
  than stock (WER 0.20) — but note the fine-tune appears to have **lost whisper's ITN
  behavior**, and it merged `dvě stě`→`dvěstě`. Only other deviation: casing of proper
  names (`ústí nad labem`), normalized away.
- **cs-03**: verbatim-perfect (only `Ministerstva` casing). WER 0.00.
- **mix-01**: WER **0.13 — best of any model on this fixture**. `branch`, `pull request`,
  `mainu` all correct; only `Puzhni`→Pushni and near-homophone `mrknout`→mergnout.
- **mix-02**: `spust`→spusť, `packa json`→package.json (same package.json failure as
  round 1 — partly a TTS-accent artifact).
- **en-01**: `river bank`→riverbank — compound split, 1 word error (WER 0.17). The
  fine-tune's only English regression; minor and semantically harmless.

**Candidate B (Parakeet):**
- **cs-03**: verbatim-perfect. **cs-01**: `kuň`→kůň, `dábelské`→ďábelské (2 diacritic
  slips, CER 0.05) — Czech prose quality is on par with the whisper large tier.
- **cs-02**: did perform ITN (`237`), plus proper-name casing → CER 0.18 (same class of
  "error" as stock whisper; desirable for dictation).
- **mix-01 / mix-02**: code-switching is Parakeet's clear weak spot — `Půzvni`,
  `půrequest`, `mrgnout`, `domainu` (mix-01 WER 0.47). It mangles Czech-embedded
  English tech terms and glues words together far more than whisper.
- **en-01**: perfect.

## ITN-aware re-score (added after review — the numbers that matter)

Round 1 switched to ITN-aware scoring (`accepted_references`, see `docs/stt-benchmark.md`)
while this round ran; the table above uses raw primary-reference scoring, which favors
the mikr fine-tune on cs-02 (it spells numbers out, matching the primary reference) and
penalizes Parakeet (correct ITN `237` scored as errors). Re-scored with the same
ITN-aware method, all four models apples-to-apples:

| Model | avg WER | cs | cs+en mix | en | s/clip |
|---|---|---|---|---|---|
| large-v3 q5_0 | **0.15** | **0.11** | 0.28 | 0.00 | ~1.7 |
| mikr-large-v3-czech-cv13 q5_0 | 0.17 | 0.14 | **0.22** | 0.17 | 1.44 |
| large-v3-turbo q5_0 | 0.17 | 0.17 | 0.25 | 0.00 | ~0.8 |
| parakeet-tdt-0.6b-v3 int8 | 0.18 | **0.11** | 0.38 | 0.00 | **0.15** |

This changes the headline: under fair scoring the mikr fine-tune does **not** beat stock
large-v3 on Czech (0.14 vs 0.11) — its wins are the hardest rare words (`ďábelské`, best
Czech CER) and the best code-switching score (mix 0.22). Parakeet's Czech prose ties
stock large-v3 for best (cs 0.11) at ~10× speed, but code-switching stays its blocker.

## Provisional verdict

1. **Does the mikr fine-tune beat stock large-v3-turbo on Czech? Modestly, yes**
   (ITN-aware cs 0.14 vs turbo 0.17; best mix 0.22; fixes `ďábelské`; lowest Czech CER) —
   **but it does not beat stock large-v3** (cs 0.11), and it costs: (a) **lost number
   ITN** (spells numbers out) — relevant because round 1 leaned on whisper's ITN to
   shrink the cleanup-LLM's job; (b) a minor English compound-split regression
   (`river bank`). It is the **same size/speed tier as large-v3 q5_0 (1031 MB,
   ~1.4 s/clip), not turbo** — no 2× speed/half-memory win. Verdict: a possible
   "best code-switching" profile, but not clearly worth the ITN loss over stock
   large-v3; validate on real speech before any ADR.

2. **Is Parakeet's Czech usable, and how fast is it really?** Better than usable:
   ITN-aware cs WER 0.11 **ties stock large-v3 for best Czech prose** (cs-03 perfect,
   correct number ITN), and it is **genuinely fast** (~0.15 s/clip decode, ~5–10×
   faster than the whisper large tier — consistent with its ~10× claim). The blocker:
   **code-switching is the worst of all candidates** (mix 0.38 — mangles Czech-embedded
   English tech terms) — a hard problem given the Czech+English constraint. Attractive
   as a low-latency profile or a code-switch-router pairing, not as the default.

## Known limitations / deviations

- TTS fixtures are an upper bound vs. real dictation (same caveat as round 1); n=6,
  pangram skews cs averages. Real-mic Czech validation still required before any ADR.
- Latency between whisper-cli (per-call process+Metal init) and in-process sherpa-onnx
  is not strictly comparable (see caveat above).
- The mikr q5_0 is the **large-v3 tier**, so it is compared against round-1 large-v3
  q5_0 as its true peer; turbo remains the speed/memory reference.
- Followed the mission's sherpa-onnx path for Parakeet. Note: whisper-cpp 1.9.1 also
  ships a native `parakeet-cli`/`parakeet-quantize` (ggml) — a possible alternative
  backend worth a future check, not benchmarked here.

## Reproduction

Throwaway `uv` venv (no global-python pollution); models under `models/` (gitignored):

```bash
uv venv --python 3.11 /tmp/stt_round2/.venv
uv pip install --python /tmp/stt_round2/.venv/bin/python \
    torch transformers "numpy<2" sherpa-onnx huggingface_hub

# Candidate A: HF -> GGML F16 -> q5_0
#   download mikr/whisper-large-v3-czech-cv13 (config.json, vocab.json,
#   added_tokens.json, merges.txt, tokenizer*, model.safetensors) -> <hf_dir>
#   download openai/whisper mel_filters.npz -> <wdir>/whisper/assets/
#   (convert-h5-to-ggml.py from github.com/ggerganov/whisper.cpp/master/models/)
/tmp/stt_round2/.venv/bin/python convert-h5-to-ggml.py <hf_dir> <wdir> models
mv models/ggml-model.bin models/ggml-mikr-large-v3-czech-cv13-f16.bin
whisper-quantize models/ggml-mikr-large-v3-czech-cv13-f16.bin \
    models/ggml-mikr-large-v3-czech-cv13-q5_0.bin q5_0

# Candidate B: csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8
#   (encoder.int8.onnx decoder.int8.onnx joiner.int8.onnx tokens.txt) -> <pk_dir>

/tmp/stt_round2/.venv/bin/python scripts/bench_stt_round2.py \
    --whisper-model models/ggml-mikr-large-v3-czech-cv13-q5_0.bin \
    --parakeet-dir  <pk_dir>
```
