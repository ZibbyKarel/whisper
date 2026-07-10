---
name: architecture-adr
description: Use when making or revisiting any significant technical decision (model choice, protocol, storage, dependency, layer boundary) — records it as an Architecture Decision Record in DECISIONS.md so future sessions inherit the reasoning.
---

# Architecture Decision Records

Every significant decision gets an ADR in `DECISIONS.md` **at the time it is made**, not retroactively.

"Significant" = expensive to reverse, constrains other components, adds a dependency, or picks between real alternatives (models, protocols, languages, storage, licenses).

## Format

```markdown
## D-NNNN: <short imperative title>
- **Date:** YYYY-MM-DD · **Status:** accepted | superseded by D-MMMM | rejected
- **Context:** the problem and constraints (hardware budget, Czech requirement, local-first, licensing)
- **Decision:** what was chosen, in one or two sentences
- **Alternatives:** each rejected option and the one-line reason it lost
- **Consequences:** what this makes easier/harder; follow-ups it creates
```

## Rules

- Number sequentially (D-0001, D-0002, …). Never renumber or delete — supersede.
- Dependencies additionally record: license, local-execution capability, maintenance health, hardware cost.
- Model choices additionally record: Czech-quality evidence (actual sample results, not English benchmarks).
- If a decision is reversed, write a new ADR that supersedes the old one and says what was learned.
