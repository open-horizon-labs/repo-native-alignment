---
date: "2026-07-14"
pipeline_issue: "/ship PR #729"
pr: 729
phase: ship
---

# PR #729 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-14 | RNA artifact search | minor | The ship instructions reference `.oh/metis/computed-but-not-delivered.md`, but RNA returned no result for that path and git confirmed only the promoted guardrail exists. | Reviewed `.oh/guardrails/computed-but-not-delivered.md`, which contains the current rule and original PR #137 evidence. | Update `.claude/agents/ship.md` to reference the promoted guardrail instead of the retired metis path. |
| 2026-07-14 | `cargo fmt --check` | minor | Repository-wide formatting reports extensive pre-existing drift across unrelated files. | Applied the one rustfmt change attributable to PR #729 manually and kept unrelated formatting out of scope. | Establish a clean formatting baseline or scope CI formatting to changed files. |
