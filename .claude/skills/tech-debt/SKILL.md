---
name: tech-debt
description: Use when taking a deliberate shortcut, discovering existing debt, or planning a cleanup pass — keeps debt visible and budgeted instead of silent.
---

# Tech Debt

Debt is acceptable only when it is **visible, recorded, and deliberate**.

## Taking on debt

When shortcutting (hardcoded value, missing edge case, skipped abstraction, TODO):
1. Mark it in code: `// DEBT(<area>): <what's wrong, what right looks like>`
2. Add a line to the `## Debt register` section of PROGRESS.md: location, cost of leaving it, trigger for paying it down.
3. Never take silent debt in: the Interface Layer contract, data persistence formats, or Czech text handling — these compound across every consumer.

## Paying down

- Before each new milestone, scan the register (`grep -rn "DEBT("`) and pay items that the milestone would build on top of.
- Prefer paying debt in the module you're about to modify anyway.
- Deleting debt = delete the marker + register line in the same commit.

## Review heuristics

- Debt that changes a public interface later is 10× the cost — fix interface debt first.
- Duplicate logic in two layers is debt even if both copies work.
- An untested module is debt regardless of how simple it looks.
