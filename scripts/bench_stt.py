#!/usr/bin/env python3
"""Benchmark whisper.cpp models on the Czech/English/mixed fixture set.

Computes WER and CER per fixture per model. Normalization lowercases and
strips punctuation but KEEPS diacritics — diacritic errors must count as
errors (Czech quality requirement).

Usage: python3 scripts/bench_stt.py [--models models/*.bin]
Writes results to docs/stt-benchmark.json and prints a summary table.
"""
import json
import re
import subprocess
import sys
import time
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "fixtures" / "audio"
MODELS = sorted((ROOT / "models").glob("*.bin"))
OUT = ROOT / "docs" / "stt-benchmark.json"

PUNCT = re.compile(r"[^\w\s]", re.UNICODE)


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


def transcribe(model: Path, wav: Path, lang: str) -> tuple[str, float]:
    # Mixed-language fixtures use Czech as primary language (code-switching case).
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


def main():
    manifest = json.loads((FIXTURES / "manifest.json").read_text())["fixtures"]
    models = [Path(m) for m in sys.argv[1:]] or MODELS
    results = []
    for model in models:
        for fx in manifest:
            wav = FIXTURES / fx["file"]
            hyp, dt = transcribe(model, wav, fx["lang"])
            row = {
                "model": model.name,
                "fixture": fx["file"],
                "lang": fx["lang"],
                "reference": fx["reference"],
                "hypothesis": hyp,
                "wer": round(wer(fx["reference"], hyp), 3),
                "cer": round(cer(fx["reference"], hyp), 3),
                "seconds": round(dt, 2),
            }
            results.append(row)
            print(f"{model.name:34s} {fx['file']:12s} WER={row['wer']:.2f} CER={row['cer']:.2f} {dt:5.1f}s")
    OUT.write_text(json.dumps(results, ensure_ascii=False, indent=2))
    print(f"\nwrote {OUT}")

    print(f"\n{'model':34s} {'avg WER':>8s} {'cs WER':>8s} {'mix WER':>8s} {'en WER':>8s}")
    for model in models:
        rows = [r for r in results if r["model"] == model.name]
        def avg(sel):
            xs = [r["wer"] for r in rows if sel(r)]
            return sum(xs) / len(xs) if xs else float("nan")
        print(f"{model.name:34s} {avg(lambda r: True):8.2f} {avg(lambda r: r['lang']=='cs'):8.2f} "
              f"{avg(lambda r: r['lang']=='cs+en'):8.2f} {avg(lambda r: r['lang']=='en'):8.2f}")


if __name__ == "__main__":
    main()
