---
name: deploy-checklist
description: Use before tagging a release, packaging the app, or declaring a milestone shippable — the release gate checklist for this local-first voice engine.
---

# Deploy / Release Checklist

Run top to bottom before any release, package, or "milestone complete" claim.

## Quality gate

- [ ] Clean checkout builds from scratch (documented commands only)
- [ ] Full test suite passes; lint passes
- [ ] Czech fixture WER/CER within thresholds recorded in DECISIONS.md; English and code-switching fixtures pass
- [ ] End-to-end latency measured and within budget on target hardware

## Local-first audit

- [ ] Runs fully offline: test with network disabled
- [ ] No calls to paid/cloud APIs anywhere in the dependency tree
- [ ] All licenses of shipped dependencies are permissive and recorded
- [ ] Model files downloaded from documented sources with checksums; download step is explicit, never silent

## Packaging & platform

- [ ] First-run experience: permissions (microphone, accessibility) requested with explanation, graceful denial handling
- [ ] Version number bumped; CHANGELOG.md updated
- [ ] Install + uninstall leave no orphaned state (document what is stored where)
- [ ] Upgrade path from previous version tested (config/data migration)

## Docs & repo

- [ ] README quickstart re-verified by following it literally
- [ ] ROADMAP.md and PROGRESS.md updated; release commit tagged
- [ ] HANDOFF.md reflects post-release state
