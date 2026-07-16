---
title: PR #773 ship friction
date: 2026-07-16
pr: 773
outcome: context-assembly
---

# PR #773 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-16 | Ship pre-flight metis read | low | `.claude/agents/ship.md` requires `.oh/metis/computed-but-not-delivered.md`, but that file does not exist; the canonical artifact is `.oh/guardrails/computed-but-not-delivered.md`. | Pre-flight used RNA artifact search and the canonical guardrail instead. | Update the ship instruction to reference the guardrail path. |
| 2026-07-16 | RNA scan gate | moderate | Search reported “last scan just now” and 22,545 symbols while returning no result for the newly committed `LspQueryProfile`. | Ship review could not trust graph impact until forcing a full branch scan. | Surface working-tree/index mismatch explicitly and make targeted refresh deterministic. |
| 2026-07-16 | Ship source review | skipped | RNA CLI cannot return bounded source ranges or disambiguate same-name impl methods reliably. | Bounded `sed` reads were used after graph-first discovery to inspect changed implementations. | Add line-range retrieval and parent-qualified method identity to CLI parity. |
