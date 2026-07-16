---
date: "2026-07-15"
pipeline_issue: "#713"
pr: 756
phase: execute
---

# PR #756 execution friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA CLI source navigation | skipped | RNA located the existing Markdown extractor, contract, symbols, and graph neighbors but did not return function bodies. | Narrow `sed`/`rg` fallbacks were required to inspect the implementation seam, tests, and README placement. | Add bounded source-body retrieval to the CLI path or expose the configured RNA MCP tools consistently in issue-agent sessions. |
