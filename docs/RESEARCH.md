# Phase 1 Research — Wispr Flow teardown & local, Czech-first replacement landscape

Date: 2026-07-10. Hardware target: Apple M5, 24 GB unified memory (D-0001).

Sources: multi-agent deep-research run (106 agents; 76 completed, 30 failed on a session
limit — the verification phase was partially cut short) + our own measured STT benchmark
(`docs/stt-benchmark.md`). Every claim below carries a confidence tag:

- **[MEASURED]** — measured locally by us, reproducible from this repo.
- **[VERIFIED]** — survived 3-vote adversarial verification against the cited source.
- **[PLAUSIBLE]** — extracted from a cited source, but verification never ran (session limit).
- **[SNIPPET]** — search-result snippet only; source never fetched. Lowest confidence.

---

## 1. Wispr Flow teardown

### Interaction model
- Push-to-talk: hold **Fn** to dictate into any app; release → formatted text inserted at
  cursor. Hands-free: **Fn+Space** or double-tap Fn, sessions up to 5 min. [PLAUSIBLE —
  HN launch thread 41696153; getvoibe.com review 2026]
- Pro-only "Command Mode": natural-language edits ("delete that sentence", "rewrite
  shorter"). [PLAUSIBLE — getvoibe]

### Pipeline (cloud-only — the gap we exploit)
- No offline mode exists; audio (and reportedly active-window screenshots) go to remote
  servers. Reported pipeline: audio → Baseten (STT) → OpenAI/Anthropic/Cerebras
  (formatting) → stored on AWS S3 us-east-1. [PLAUSIBLE — getvoibe 2026-05-27]
- Founders declined to disclose models or local-vs-cloud at launch. [PLAUSIBLE — HN 41696153]
- Post-processing: removes fillers ("um", "uh"), punctuates, adapts tone/structure per
  active app (Gmail vs Slack vs VS Code). Founders claim 50–70 % zero-edit messages vs
  <5 % for OS built-in dictation. [PLAUSIBLE — HN 41696153, getvoibe]
- Claimed <700 ms p99 latency; reviewers report a real-world feel closer to 1–2 s. [SNIPPET]
- Languages: "100+ incl. mid-sentence code-switching" (founder claim, no benchmark). [PLAUSIBLE]

### Pricing & complaints
- Free tier 2 000 words/week; Pro ~$12/mo annual ($144/yr) or $15/mo. [PLAUSIBLE — HN + getvoibe]
- Trustpilot 2.7/5; idle usage ~800 MB RAM / ~8 % CPU (2021 MBP); Electron Windows app
  reportedly freezes target apps incl. VS Code; privacy allegations (app/URL tracking,
  screenshots; 2025 Reddit "screenshot scandal" → Context Awareness became opt-in). [PLAUSIBLE/SNIPPET]

**Takeaway:** the product's value = STT + narrow LLM cleanup + frictionless insertion +
per-app adaptation. Its weaknesses (cloud dependency, privacy, resource use, subscription)
are exactly what a local-first engine removes. Feature checklist to match: push-to-talk +
hands-free toggle, filler removal, per-app formatting profiles, personal dictionary,
command mode (later phase).

---

## 2. Open-source alternative landscape (architecture donors)

| Tool | License | Stack | STT | Insertion | Notes |
|---|---|---|---|---|---|
| **Handy** | MIT (brand excluded) [VERIFIED] | Tauri: React/TS + **Rust** [VERIFIED] | whisper.cpp GGML via transcribe-cpp; Parakeet (CPU) via transcribe-rs [VERIFIED] | paste into frontmost app; Linux needs xdotool/wtype [VERIFIED] | Silero VAD via vad-rs, cpal audio, rdev hotkeys, rubato resampling [VERIFIED]. ~26k★. Raw transcript output, minimal cleanup → confirms need for our LLM stage. Known issues: 1–2 s start delay on Bluetooth mics; Linux modifier-race on paste. [PLAUSIBLE] |
| **VoiceInk** | GPL-3.0, no PRs accepted | native Swift, macOS 14.4+ | whisper.cpp + Parakeet via FluidAudio [VERIFIED core] | system-wide, per-app "Power mode" | Closest UX template to Wispr Flow: screen-context awareness, personal dictionary + smart replacements, per-app auto settings [VERIFIED]. ~5.5k★, $39 lifetime option. GPL → **study, don't copy code**. |
| **OpenWhispr** | MIT [VERIFIED] | Electron 41 + React 19 + TS + better-sqlite3 [VERIFIED] | whisper.cpp + Parakeet via **sherpa-onnx** [VERIFIED] | hotkey + automatic pasteboard paste | Optional cloud BYOK. Proves Parakeet runs locally on Mac via sherpa-onnx. ~4.4k★. |
| **VocaMac** | AGPL-3.0 | Swift + WhisperKit (CoreML/ANE) | WhisperKit | pasteboard + simulated Cmd-V | Hold-Right-Option PTT via CGEventTap; needs Mic + Accessibility + Input Monitoring permissions. [PLAUSIBLE — 0 verification votes ran] |
| **FluidVoice** | GPL-3.0 | Swift, macOS 15+ | Parakeet TDT v3 default, Whisper fallback, Nemotron | — | On-device "Fluid-1" post-processing model adapting tone per app. [PLAUSIBLE] |
| **Talon** | core proprietary | — | — | — | Voice-control framework, not a dictation engine; not a code donor. [PLAUSIBLE] |
| **FreeFlow** | OSS (build-trust) | macOS client + self-hosted server | server-side realtime STT + fast-LLM cleanup | — | NOT fully local (WebSocket to server). Interesting latency data: ~2/3 of dictations <0.6 s end-to-end; LLM cleanup adds ~0.4 s; ~40 % of dictations skipped cleanup entirely (heuristic gate before LLM = good idea). [PLAUSIBLE] |

**Takeaway:** the proven local stack is **Rust core (Handy pattern): cpal → Silero VAD →
whisper.cpp/Parakeet → clipboard-paste insertion**, with VoiceInk as the UX feature
template (dictionary, per-app profiles, context awareness). MIT-licensed Handy and
OpenWhispr are safe references; GPL/AGPL projects (VoiceInk, VocaMac) are design
references only.

---

## 3. STT for Czech

### Our measurements [MEASURED — docs/stt-benchmark.md]
- whisper.cpp large-v3-turbo q5_0 ≈ large-v3 q5_0 on Czech at ~2× speed, 547 MB →
  provisional default. medium/small corrupt Czech diacritics → disqualified as default.
- Whisper already does number ITN, casing, punctuation → cleanup LLM's job is narrow.
- Caveats: synthetic TTS fixtures (upper bound), n=6; real-mic validation required before ADR.

### Czech fine-tunes (quality ceiling)
- **mikr/whisper-large-v3-czech-cv13: WER 7.89 % on Common Voice 13 cs, Apache-2.0**
  [VERIFIED] — fine-tuned from large-v3; late-2023 training run; needs GGML conversion +
  our fixture benchmark to compare against stock large-v3-turbo.
- mikr/whisper-large2-czech-cv11: WER ≈9.03 on CV11 cs, Apache-2.0. [VERIFIED]

### Parakeet (speed alternative)
- Parakeet TDT 0.6B **v3 supports Czech** (25 languages). FLEURS Czech WER: Parakeet v3
  11.0 % vs Whisper large-v3 11.3 % — near parity at ~10× speed; 465 MB download,
  ~800 MB RAM; trained on 36 000 h of non-speech audio to avoid hallucination-on-silence
  (relevant for push-to-talk silences). [PLAUSIBLE — whispernotes.app blog]
- Runs locally on Mac via sherpa-onnx (OpenWhispr) [VERIFIED] and transcribe-rs/FluidAudio.

### Runtime speed on Apple Silicon (M4/24 GB ≈ our target)
- Short-utterance latency: FluidAudio CoreML 0.19 s < Parakeet MLX 0.50 s < mlx-whisper
  1.02 s < whisper.cpp 1.23 s ≪ faster-whisper 6.96 s (slowest — poor fit for Mac
  dictation). [VERIFIED — mac-whisper-speedtest]
- mlx-whisper ~2× faster than whisper.cpp on large-v3-turbo (long-form). [PLAUSIBLE]

**Round 2 [MEASURED — docs/stt-benchmark-round2.md]:** both candidates benchmarked on our
fixtures (ITN-aware scoring). mikr fine-tune: cs 0.14 — beats turbo (0.17) but **not**
stock large-v3 (0.11); best code-switching (mix 0.22) and fixes `ďábelské`, but **lost
whisper's number ITN** (grows the cleanup-LLM's job) and is large-v3-sized (1031 MB,
~1.4 s/clip). Parakeet v3 int8 via sherpa-onnx: **cs 0.11 ties stock large-v3 at ~10×
speed** (~0.15 s/clip decode, 640 MB), correct ITN, but **worst code-switching (mix
0.38)** — mangles Czech-embedded English tech terms.

**Phase-2 shortlist (post round 2):** default remains **large-v3-turbo q5_0** (best
speed/quality/memory balance with intact ITN); **Parakeet v3** as low-latency profile
(or a future code-switch-router pairing); mikr fine-tune only if real-speech validation
shows rare-word diacritics matter more than ITN; MLX/CoreML runtimes as perf optimization.
Vosk: zero data surfaced — deprioritized.

---

## 4. Local LLM for Czech text cleanup (≤12 B, ~6–8 GB quantized)

- **BenCzechMark** (BUT FIT + FI MUNI + CIIRC CTU + HF) is the authoritative Czech
  benchmark: 50 tasks, 9 categories, 90 % native Czech, duel-win-rate ranking. [VERIFIED]
- **Gemma-2 9B: excellent Czech reading comprehension, beats much larger models.**
  [VERIFIED 2-1] Also most Sentiment/Math duel wins among small models. [PLAUSIBLE]
- **Mistral-Nemo 12B: leads Czech NLU / reading comprehension / NER among ≤12 B.**
  [PLAUSIBLE — arXiv 2412.17933, 1 of 3 votes completed]
- CzechBench leaderboard numbers for Gemma-2-9B-it (ARC-cs 0.921/0.821, Belebele 0.904…)
  [PLAUSIBLE — 0 votes]; leaderboard itself is stale (no Qwen3/Gemma3/Mistral-Nemo/Aya-Expanse
  entries) → **published benchmarks cannot decide our pick; hands-on eval required.**
- Multilingual GEC (no Czech in study): Gemma-9B best ≤9 B across EN/DE/IT/SV. [PLAUSIBLE]

**Hands-on round 1 [MEASURED — docs/llm-cleanup-benchmark.md]:** qwen3:8b, gemma3:12b and
mistral-nemo:12b evaluated on 13 Czech/mixed cleanup cases incl. 4 trap cases (dictated
questions/commands/prompt-injections must be transcribed, never answered).
**qwen3:8b wins**: avg WER 0.15, 0/4 trap failures, 1.6 s avg, 5.2 GB. gemma3:12b leaked
meta-commentary into the output on the prompt-injection trap; **mistral-nemo:12b is
disqualified** (obeyed the injection, answered the arithmetic trap — the BenCzechMark NLU
lead does not transfer to instruction-following cleanup). Cross-model: Czech
self-correction needs few-shot prompting; paraphrasing is the dominant error class →
adopt FreeFlow-style cleanup-skip gate + anti-paraphrase prompt in Phase 2.

---

## 5. VAD, audio capture, hotkeys, text insertion (macOS)

- **VAD:** Silero VAD is the de-facto standard (Handy ships it via `vad-rs` [VERIFIED]);
  no dedicated primary source was researched — small gap, low risk.
- **Capture/resample (Rust):** cpal + rubato, proven in Handy. [VERIFIED]
- **Hotkeys:** rdev (Rust, Handy) [VERIFIED]; CGEventTap for Fn/modifier-hold patterns
  (VocaMac, Wispr Flow). [PLAUSIBLE]
- **Insertion:** no official macOS API for typing into other apps; CGEvent synthesis
  needs Accessibility permission and is unreliable at speed (~100 ms/keystroke advised);
  arbitrary characters (Czech diacritics!) require UCKeyTranslate against the active
  keyboard layout → **pasteboard + simulated Cmd-V is the robust bulk-text path** (used
  by Handy, OpenWhispr, VocaMac); save/restore clipboard around the paste. [PLAUSIBLE —
  kulman.sk; convergent evidence from three codebases]
- **Secure Input:** blocks injection in password contexts and can get *stuck* system-wide
  (commonly by password managers), with no programmatic override — correct behavior is to
  detect and *surface the blocking app*, never bypass. [PLAUSIBLE — espanso docs, 0 votes]

---

## 6. Known gaps / follow-ups

1. Real-microphone Czech recordings → validate STT finalist (blocks the model ADR).
2. Benchmark mikr Czech fine-tune + Parakeet v3 on our fixtures (extend `bench_stt.py`).
3. Run LLM cleanup benchmark (models downloading at time of writing).
4. ITN-aware WER scoring mode in `bench_stt.py`.
5. Unresearched: Superwhisper teardown, Vosk, distil-whisper Czech numbers, dedicated
   Silero VAD + global-hotkey API primary sources, Czech/English code-switching mechanics
   (only qualitative claims exist anywhere).
6. Deep-research verification debt: Secure Input, VocaMac, CzechBench numbers, Mistral-Nemo
   Czech claims got 0–1 of 3 verification votes (session limit). Treat as [PLAUSIBLE] until
   they matter for an ADR, then verify individually.
