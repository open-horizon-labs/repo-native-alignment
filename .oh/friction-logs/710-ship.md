---
date: "2026-07-14"
pipeline_issue: "/ship PR #710"
pr: 710
phase: ship
---

# PR #710 ship friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-14 | RNA `search("parse_edge_kind")` | minor | Exact search returned no result for an existing function in the refreshed worktree; broader searches found only related tests and constants. | The review could not use RNA traversal for the parser and relied on the PR diff instead. | Improve indexing of private functions in refreshed worktrees. |
| 2026-07-14 | RNA `search("test_edge_weight_values")` | minor | Exact and broader PageRank searches did not find the existing private unit test. | Used one narrow `sed`/`rg` fallback to place the independent review's required policy assertion. | Make private test functions reliably discoverable by exact name. |
