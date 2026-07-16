---
title: PR #775 ship friction
date: 2026-07-16
pr: 775
outcome: context-assembly
---

# PR #775 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-16 | GitButler status/mutations | skipped | `but status -fv` rejected the ordinary checkout because it is not on a `gitbutler/*` branch. | Used the documented direct Git fallback for the existing `issue/769` branch; the two unrelated untracked `.oh` files remained untouched. | Support ordinary checked-out branches or make the setup requirement discoverable before mutation. |
| 2026-07-16 | RNA MCP exploration | skipped | RNA MCP tools were not exposed to this dedicated issue agent. | Used the installed `repo-native-alignment search/graph --repo .` CLI against the live index. | Expose the repository's own MCP tools consistently to delegated agents. |
| 2026-07-16 | RNA source retrieval | skipped | RNA identified symbols, callers, artifacts, and exact source ranges but does not provide reliable bounded current-source bodies through this CLI session. | Used bounded `sed` reads only after RNA located the relevant symbols and ranges. | Add current-filesystem source-span retrieval to the RNA CLI. |
| 2026-07-16 | Ship pre-flight metis path | low | `.claude/agents/ship.md` names `.oh/metis/computed-but-not-delivered.md`, while the canonical artifact is `.oh/guardrails/computed-but-not-delivered.md`. | Used the canonical guardrail. | Correct the ship procedure path. |
