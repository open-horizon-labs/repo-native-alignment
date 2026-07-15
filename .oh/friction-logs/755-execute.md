---
date: "2026-07-15"
pipeline_issue: "#712"
pr: 755
phase: execute
---

# PR #755 execution friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA MCP discovery | major | The agent session exposed no RNA MCP tools or server resources, and no `rna` executable was installed. | Repository context could not be queried through the mandated dogfood path. Narrow `rg`, `sed`, `find`, and GitHub issue/PR fallbacks were used for contract precedents and logged here. | Ensure dedicated issue-agent sessions inherit the repository's RNA MCP server and prewarmed root. |
| 2026-07-15 | `cargo fmt --check` | minor | The clean `origin/main` baseline contains repository-wide rustfmt drift in unrelated Rust files. | A global formatting check cannot pass without out-of-scope rewrites. The new test was formatted directly with `rustfmt` and the focused contract test was used for verification. | Restore repository-wide formatting in its owning change; do not mix it into #712. |
