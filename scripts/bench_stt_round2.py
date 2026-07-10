#!/usr/bin/env python3
"""STT benchmark round 2 — Czech fine-tune + Parakeet vs. round-1 whisper.cpp.

Candidate A: mikr/whisper-large-v3-czech-cv13 converted to GGML and run through
whisper-cli (same measurement as round 1, scripts/bench_stt.py).
Candidate B: Parakeet TDT 0.6B v3 (sherpa-onnx ONNX, int8) via the sherpa_onnx
Python API.

Normalization / WER / CER are copied VERBATIM from scripts/bench_stt.py so the
numbers are directly comparable: lowercase + strip punctuation but KEEP
diacritics (diacritic errors must count as errors — Czech quality requirement).

Reproduction (see docs/stt-benchmark-round2.md for the full recipe):

  # throwaway venv (do NOT pollute global python)
  uv venv --python 3.11 /tmp/stt_round2/.venv
  uv pip install --python /tmp/stt_round2/.venv/bin/python \
      torch transformers "numpy<2" sherpa-onnx huggingface_hub

  # candidate A: HF -> GGML F16 -> q5_0
  #   download mikr/whisper-large-v3-czech-cv13 (config.json, vocab.json,
  #   added_tokens.json, merges.txt, tokenizer*, model.safetensors) into <hf_dir>
  #   download openai/whisper mel_filters.npz into <wdir>/whisper/assets/
  /tmp/stt_round2/.venv/bin/python convert-h5-to-ggml.py <hf_dir> <wdir> models
  mv models/ggml-model.bin models/ggml-mikr-large-v3-czech-cv13-f16.bin
  whisper-quantize models/ggml-mikr-large-v3-czech-cv13-f16.bin \
      models/ggml-mikr-large-v3-czech-cv13-q5_0.bin q5_0

  # candidate B: sherpa-onnx parakeet-tdt-0.6b-v3 int8
  #   download csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8
  #   (encoder.int8.onnx decoder.int8.onnx joiner.int8.onnx tokens.txt) into <pk_dir>

  /tmp/stt_round2/.venv/bin/python scripts/bench_stt_round2.py \
      --whisper-model models/ggml-mikr-large-v3-czech-cv13-q5_0.bin \
      --parakeet-dir  models/parakeet-tdt-0.6b-v3-int8

Requires the uv venv for the Parakeet path (sherpa_onnx). The whisper path only
needs whisper-cli on PATH; it can run under any python.
"""
import argparse
import json
import re
import subprocess
import time
import unicodedata
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "fixtures" / "audio"
OUT = ROOT / "docs" / "stt-benchmark-round2.json"

PUNCT = re.compile(r"[^\w\s]", re.UNICODE)


# --- copied verbatim from scripts/bench_stt.py (diacritics-preserving) ---
def normalize(text: str) -> str:
    text = unicodedata.normalize("NFC", text)
    text = PUNCT.sub("", text.lower())
    return " ".join(text.split())


def edit_distance(a, b):
    m, n = len(a), len(b)
    prev = list(range(n + 1))
    for i in range(1, m + 1):
        cur = [i] + [0] * n
        for j in range(1, n + 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (a[i - 1] != b[j - 1]))
        prev = cur
    return prev[n]


def wer(ref: str, hyp: str) -> float:
    r, h = normalize(ref).split(), normalize(hyp).split()
    return edit_distance(r, h) / max(len(r), 1)


def cer(ref: str, hyp: str) -> float:
    r, h = normalize(ref), normalize(hyp)
    return edit_distance(r, h) / max(len(r), 1)
# --- end verbatim block ---


def transcribe_whisper(model: Path, wav: Path, lang: str) -> tuple[str, float]:
    lang_arg = "cs" if lang.startswith("cs") else "en"
    t0 = time.time()
    res = subprocess.run(
        ["whisper-cli", "-m", str(model), "-l", lang_arg, "-nt", "-np", "-f", str(wav)],
        capture_output=True, text=True, timeout=600,
    )
    dt = time.time() - t0
    if res.returncode != 0:
        raise RuntimeError(f"{model.name} on {wav.name}: {res.stderr[-500:]}")
    return res.stdout.strip(), dt


def read_wav_f32(wav: Path):
    with wave.open(str(wav), "rb") as w:
        assert w.getsampwidth() == 2, f"{wav.name}: expected 16-bit PCM"
        sr = w.getframerate()
        n = w.getnframes()
        raw = w.readframes(n)
    import numpy as np
    samples = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    return samples, sr


def make_parakeet(pk_dir: Path):
    import sherpa_onnx
    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(pk_dir / "encoder.int8.onnx"),
        decoder=str(pk_dir / "decoder.int8.onnx"),
        joiner=str(pk_dir / "joiner.int8.onnx"),
        tokens=str(pk_dir / "tokens.txt"),
        num_threads=4,
        model_type="nemo_transducer",
        debug=False,
    )


def transcribe_parakeet(recognizer, wav: Path) -> tuple[str, float]:
    samples, sr = read_wav_f32(wav)
    t0 = time.time()
    stream = recognizer.create_stream()
    stream.accept_waveform(sr, samples)
    recognizer.decode_stream(stream)
    dt = time.time() - t0
    return stream.result.text.strip(), dt


def row(model_name, fx, hyp, dt):
    return {
        "model": model_name,
        "fixture": fx["file"],
        "lang": fx["lang"],
        "reference": fx["reference"],
        "hypothesis": hyp,
        "wer": round(wer(fx["reference"], hyp), 3),
        "cer": round(cer(fx["reference"], hyp), 3),
        "seconds": round(dt, 2),
    }


def print_summary(results, model_names):
    print(f"\n{'model':44s} {'avg WER':>8s} {'cs WER':>8s} {'mix WER':>8s} {'en WER':>8s} {'s/clip':>7s}")
    for name in model_names:
        rows = [r for r in results if r["model"] == name]
        if not rows:
            continue

        def avg(key, sel):
            xs = [r[key] for r in rows if sel(r)]
            return sum(xs) / len(xs) if xs else float("nan")

        print(f"{name:44s} {avg('wer', lambda r: True):8.2f} "
              f"{avg('wer', lambda r: r['lang'] == 'cs'):8.2f} "
              f"{avg('wer', lambda r: r['lang'] == 'cs+en'):8.2f} "
              f"{avg('wer', lambda r: r['lang'] == 'en'):8.2f} "
              f"{avg('seconds', lambda r: True):7.2f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--whisper-model", type=Path, help="GGML model for whisper-cli (candidate A)")
    ap.add_argument("--parakeet-dir", type=Path, help="dir with sherpa-onnx parakeet onnx (candidate B)")
    args = ap.parse_args()

    manifest = json.loads((FIXTURES / "manifest.json").read_text())["fixtures"]
    results = []
    model_names = []

    if args.whisper_model:
        name = args.whisper_model.name
        model_names.append(name)
        for fx in manifest:
            hyp, dt = transcribe_whisper(args.whisper_model, FIXTURES / fx["file"], fx["lang"])
            r = row(name, fx, hyp, dt)
            results.append(r)
            print(f"{name:44s} {fx['file']:12s} WER={r['wer']:.2f} CER={r['cer']:.2f} {dt:5.1f}s")

    if args.parakeet_dir:
        name = "parakeet-tdt-0.6b-v3-int8"
        model_names.append(name)
        rec = make_parakeet(args.parakeet_dir)
        for fx in manifest:
            hyp, dt = transcribe_parakeet(rec, FIXTURES / fx["file"])
            r = row(name, fx, hyp, dt)
            results.append(r)
            print(f"{name:44s} {fx['file']:12s} WER={r['wer']:.2f} CER={r['cer']:.2f} {dt:5.1f}s")

    OUT.write_text(json.dumps(results, ensure_ascii=False, indent=2))
    print(f"\nwrote {OUT}")
    print_summary(results, model_names)


if __name__ == "__main__":
    main()
