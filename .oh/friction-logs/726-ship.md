---
date: "2026-07-14"
pipeline_issue: "/ship PR #726"
pr: 726
phase: ship
---

# PR #726 ship friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-14 | RNA `search("mozilla-actions/sccache-action")` | minor | Exact YAML action references returned no results after a successful extract-only worktree scan. | Used a narrow `rg` fallback to enumerate all seven workflow pins and extended the repository oracle to prevent future drift. | Improve exact-value indexing for workflow YAML action references. |
