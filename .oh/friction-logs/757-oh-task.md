---
id: pr-757-oh-task
outcome: context-assembly
severity: blocked
---

# PR #757 oh-task friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA MCP tools | skipped | RNA MCP tools were not exposed to the #714 issue agent. | Dependency validation used GitHub issue/PR metadata and targeted git inspection instead of the mandated RNA graph path. | Preserve this evidence for RNA tool-delivery debugging; repeat graph exploration when #712 and #713 land. |
| 2026-07-15 | Draft-PR-first workflow | minor | GitHub rejects a pull request when the pushed issue branch has no commits ahead of `main`. | A no-content workflow commit was required before the blocker-only draft PR could be opened. | Teach `oh-task` to create a deliberate empty commit for dependency-blocked issues. |
