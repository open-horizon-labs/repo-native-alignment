---
pr: 753
issue: 742
date: 2026-07-15
---

# PR #753 friction log

| Time | Attempt | Severity | Friction | Fallback | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA MCP/CLI exploration before implementation | skipped | RNA MCP tools were not exposed in the session and no `rna`/`repo-native-alignment` executable was installed. | Used narrow `rg` and `sed` reads to locate the unified search request, shared service, CLI, MCP handler, and root configuration. | Issue #742 directly closes this source-span fallback for compiler-supplied locations; tool availability remains an environment concern. |
| 2026-07-15 | RNA-grounded pre-commit review | skipped | RNA MCP graph and artifact queries remained unavailable. | Reviewed the bounded Git diff, issue acceptance criteria, AGENTS.md context, and required guardrails already injected into the session. | Repeat graph/artifact review in `/ship` if a CI-built RNA executable becomes available. |
