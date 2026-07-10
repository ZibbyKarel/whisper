# Decisions (ADR log)

Format defined in `.claude/skills/architecture-adr/SKILL.md`. Never renumber; supersede instead.

## D-0001: Target hardware budget is Apple M5 / 24 GB unified memory
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Model selection must fit the machine that runs the engine offline in real time. Measured: Apple M5, 10-core CPU, 10-core GPU (Metal 4), 24 GB unified memory, macOS 27.
- **Decision:** Budget ≤ ~10 GB resident model memory: Whisper large-v3 tier (~3 GB quantized) for Czech-quality STT plus a quantized 7–12 B LLM (~5–8 GB Q4) for optional cleanup, concurrently.
- **Alternatives:** Smaller STT (medium/small) — rejected as default because Czech accuracy degrades noticeably below large-v3; may still be offered as a low-latency profile after measurement.
- **Consequences:** Czech quality achievable without cloud. Latency vs. quality profiles must be measured in Phase 1, not assumed.

## D-0002: Author missing engineering skills locally instead of installing vendor plugin bundles
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Mission Phase 0.2 requires skills for architecture/ADR, system-design, testing-strategy, code-review, debug, documentation, deploy-checklist, tech-debt. Marketplace search (official + netresearch) found no focused, well-maintained plugins for the first and last four without pulling large vendor bundles; code-review and debugging are already covered by installed superpowers 6.1.1 and built-in /code-review.
- **Decision:** Author project-local skills under `.claude/skills/` (architecture-adr, system-design, testing-strategy, documentation, deploy-checklist, tech-debt); rely on superpowers + built-ins for review/debug/TDD/planning.
- **Alternatives:** Installing multi-skill vendor plugins (aws-*, forge-*, etc.) — rejected: wrong domain, maintenance risk, context bloat.
- **Consequences:** Skills are versioned with the repo (reproducible, project-tailored); we maintain them ourselves.

## D-0003: Runtime toolchain — Homebrew whisper-cpp + llama.cpp + Ollama; uv for Python; pnpm for Node
- **Date:** 2026-07-10 · **Status:** accepted
- **Context:** Phase 0.4 requires local STT/LLM runtimes installed before research so candidate models can be measured, not guessed. All are MIT-licensed, offline-capable, Metal-accelerated, actively maintained.
- **Decision:** Install whisper-cpp (STT, Metal), llama.cpp + Ollama (LLM serving/eval), ffmpeg (audio conversion), uv (Python env for faster-whisper comparison), alongside existing Node 25/pnpm 10.
- **Alternatives:** MLX-whisper (Apple MLX) — deferred to Phase 1 benchmarking, not excluded. Cloud STT — forbidden by mission. Docker-based runtimes — needless overhead on native macOS.
- **Consequences:** Phase 1 can benchmark real Czech samples immediately. Final engine embeds a subset of these; Ollama may remain dev-only (ADR to follow in Phase 2).
