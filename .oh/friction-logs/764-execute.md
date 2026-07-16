---
date: 2026-07-15
issue: 642
pr: 764
---

# RNA friction log

| Date | Activity | Severity | RNA limitation | Fallback | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | Explore artifact semantics and ADR validation precedent | moderate | RNA MCP tools were not exposed in the agent session, and neither `rna` nor `repo-native-alignment` was installed. | Used narrow `rg` results and bounded reads of the directly relevant guardrail, metis, and skill files; used GitHub PR metadata for ADR precedent. | Re-run artifact discovery through RNA during ship/delivery verification if a CI-built executable or MCP connection becomes available. |
