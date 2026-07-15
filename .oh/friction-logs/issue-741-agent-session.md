---
date: 2026-07-15
issue: 741
outcome: context-assembly
artifact_type: friction-log
---

# Issue #741 agent-session friction

| Event | Intended path | Fallback | Impact |
|---|---|---|---|
| RNA MCP tools unavailable in the agent session | Use `repo_map`, `search`, and `outcome_progress` for all repository exploration | Diagnose tool exposure, then use narrow source reads only when required | RNA could not dogfood its own graph in this session |
| GitButler rejected the shared checkout because it is not on a `gitbutler/*` branch | Use GitButler for branch and commit mutations | Use the GitHub connector for remote branch/bootstrap commit/PR creation | No implementation impact; GitButler workflow unavailable |
| Shared checkout occupied by `issue/745` | Switch the checkout to `issue/741` from `origin/main` | Bootstrap remote branch and draft PR through GitHub while preserving #745 work | Implementation waits until the shared checkout is available |
