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
| 2026-07-15 | RNA CLI source navigation | skipped | RNA found the relevant symbols, contracts, and persistence seams but does not return bounded function bodies from CLI search. | Targeted `sed`/`rg` fallbacks were required after RNA discovery to inspect exact construction and rendering sites. | Expose the configured RNA MCP tools consistently or add bounded source retrieval to the CLI. |
| 2026-07-15 | Workspace dependency loader | minor | Bundled dependency discovery produced no result and had to be terminated after more than a minute. | MCP smoke could not use the advertised dependency path. | Diagnose the desktop workspace dependency loader hang. |
| 2026-07-15 | Shell Node selection | minor | `node` had no selected version for this repository. | The standard MCP smoke command failed before execution. | `mise exec node@22.21.1 -- node` provided the configured runtime and the smoke suite passed. |
| 2026-07-16 | GitButler workspace | skipped | The shared checkout is on a conventional `issue/*` branch, so `but status -fv` refused to operate without a `gitbutler/*` setup branch. | The required no-worktree continuation used conventional git branch/merge commands to preserve the existing PR topology. | Align the long-running sequential issue workflow with GitButler before future branch handoffs. |
| 2026-07-16 | RNA CLI conflict inspection | skipped | RNA confirmed a live 33,652-symbol cache but cannot expose unmerged index stages or conflict markers. | Targeted `git diff --cc` and one bounded line read were required to resolve the `origin/main` integration conflict. | Add conflict-aware source retrieval or explicitly document merge-conflict inspection as an RNA exception. |
