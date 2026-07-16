---
title: PR #776 execution friction
date: 2026-07-16
pr: 776
outcome: context-assembly
---

# PR #776 execution friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-16 | GitButler status/mutations | skipped | `but status -fv` rejected the ordinary checkout because it is not on a `gitbutler/*` branch. | Used the repo-local `/oh-task` Git workflow on the existing `issue/770` branch; unrelated untracked `.oh` files remained untouched. | Support ordinary checked-out branches or surface setup requirements before mutation. |
| 2026-07-16 | RNA full/incremental scan | moderate | Both scans entered bounded TypeScript LSP enrichment and took minutes; caller coverage finalized degraded. | Exact source/artifact search remained usable, while graph-impact confidence stayed explicitly partial. | Make requested versus observed enrichment and quick structural refresh behavior clearer. |
| 2026-07-16 | RNA current-source symbol location | minor | RNA returned the stale indexed range for the diagnostic regression after current source had shifted. | A single `rg -n` located the already-known function name; all surrounding source retrieval stayed on RNA's bounded source-span path. | Refresh working-tree symbol ranges deterministically or distinguish indexed from current line ranges in symbol results. |
| 2026-07-16 | Installed RNA artifact | blocking | The installed 0.2.10 binary panicked on Django UTF-8 diagnostic truncation, and its reported line did not match current source. | Preserved the failed bundle and switched to the successful exact-head CI artifact as required. | Always use the exact-head CI artifact for user-facing verification. |
| 2026-07-16 | Exact-head RNA artifact | blocking | Full Django LSP enrichment reproduced the UTF-8 byte-slice panic at current source line 2185 before persistence. | Added character-safe truncation and a non-breaking-space regression; no model tokens were spent. | Treat all externally sourced string truncation as UTF-8-sensitive. |
