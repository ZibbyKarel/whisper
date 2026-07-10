# Phase 0.1 — Capability Audit

Date: 2026-07-10 · Machine: Apple M5, 10-core CPU, 10-core GPU (Metal 4), 24 GB unified memory, macOS 27.0

Maps every project need to a concrete tool, with status. This is the checklist for the rest of Phase 0.

## Hardware budget (drives model selection in Phase 1)

| Resource | Available | Working budget for models |
|---|---|---|
| Unified memory | 24 GB | ≤ ~10 GB resident for models (leave room for OS + apps + engine) |
| GPU | 10-core Apple GPU, Metal 4 | whisper.cpp/llama.cpp Metal backends; MLX viable |
| CPU | 10 cores | faster-whisper CPU fallback viable |

Implication: Whisper **large-v3** (~3.1 GB ggml Q5, best Czech tier) + a quantized 7–12 B instruct LLM (~5–8 GB Q4) fit simultaneously. Czech quality does not force a hardware compromise on this machine.

## Skills

| Need | Tool | Status |
|---|---|---|
| Architecture / ADR | local skill `.claude/skills/architecture-adr` | authored in Phase 0.2 |
| System design | local skill `.claude/skills/system-design` | authored in Phase 0.2 |
| Testing strategy | local skill `.claude/skills/testing-strategy` (+ `superpowers:test-driven-development`) | authored in Phase 0.2 |
| Code review | built-in `/code-review`, `superpowers:requesting-code-review`, `superpowers:receiving-code-review` | installed (superpowers 6.1.1) |
| Debugging | `superpowers:systematic-debugging` | installed |
| Documentation | local skill `.claude/skills/documentation` | authored in Phase 0.2 |
| Deploy checklist | local skill `.claude/skills/deploy-checklist` | authored in Phase 0.2 |
| Tech debt | local skill `.claude/skills/tech-debt` | authored in Phase 0.2 |
| Planning/execution | `superpowers:writing-plans`, `superpowers:executing-plans`, `superpowers:subagent-driven-development` | installed |
| Verification | built-in `verify`, `superpowers:verification-before-completion` | installed |
| Research | `deep-research` skill + WebSearch/WebFetch | installed |

Marketplace note: official + netresearch marketplaces were searched; no focused, well-maintained plugin covers architecture-adr / system-design / deploy-checklist / tech-debt without pulling large vendor bundles. Decision: author them as local project skills (see DECISIONS.md D-0002).

## Plugins (installed, user scope)

- `superpowers@claude-plugins-official` 6.1.1 — process skills (brainstorming, TDD, debugging, plans, review)
- `context7@claude-plugins-official` — up-to-date library docs
- `code-simplifier@claude-plugins-official` 1.0.0 — refactoring agent

## Subagents (`.claude/agents/`)

| Role | File | Model | Purpose |
|---|---|---|---|
| Opus implementer | `opus-implementer.md` | opus | complex implementation, algorithms, integration, perf |
| Sonnet implementer | `sonnet-implementer.md` | sonnet | normal features, UI, tests, docs, refactors |
| Small-model worker | `small-worker.md` | haiku | formatting, lint fixes, migrations, repetitive edits |

## MCP servers / built-in tools

- Filesystem/git: built-in tools + `gh` CLI — present
- Docs lookup: context7 MCP — present
- Browser research: claude-in-chrome MCP — present
- No additional MCP servers required for Phase 1–2.

## CLI tools & runtimes

| Need | Tool | Version / status |
|---|---|---|
| JS runtime | Node | v25.8.1 ✓ |
| Package manager | pnpm | 10.33.0 ✓ (npm 11.11.0 also present) |
| Python | python3 | 3.12.13 ✓ |
| Python env/pkg mgr | uv | 0.11.28 ✓ |
| Audio/media | ffmpeg | 8.1.2 ✓ |
| STT runtime | whisper-cpp (Metal) | 1.9.1 (ggml 0.15.3) ✓; faster-whisper via uv as alternative |
| LLM runtime | llama.cpp + Ollama | llama.cpp b9910 ✓ · ollama client 0.31.1 ✓ (serve on demand) |
| Build tooling | Homebrew 6.0.2, git 2.54.0, Xcode CLT | ✓ |

Audio capture libraries are runtime-language-dependent and chosen in Phase 2 (architecture), not preinstalled blindly.

## Phase 0.6 verification gate — results (2026-07-10)

- Skills: superpowers 6.1.1 / context7 / code-simplifier load (listed in session skill registry). Six local skills authored under `.claude/skills/` with valid frontmatter.
- Subagents: three definitions in `.claude/agents/`. Note: agent definitions register at session start, so named invocation activates next session; the delegation path itself was verified live via the Agent tool with an explicit model binding — a haiku-bound worker was invoked and returned Czech diacritics intact ("Příliš žluťoučký kůň").
- Runtimes verified (versions in table above); ffmpeg/whisper-cli/llama-cli/ollama/uv all on PATH.
- Repo + memory files exist: ROADMAP, ARCHITECTURE (skeleton), DECISIONS (D-0001…D-0003), PROGRESS, HANDOFF, CLAUDE.md constitution.
- Gate: PASS → bootstrap committed as `chore: environment bootstrap`.
