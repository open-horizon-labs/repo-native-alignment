---
issue: 784
outcome: context-assembly
date: 2026-07-17
---

# Issue #784 builder friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-17 | RNA MCP → installed RNA CLI | minor | RNA MCP tools were not exposed; the installed CLI found the exact report, job, persistence, language, scanner, service, and CLI symbols but cannot return bounded function bodies. | Implementation uses targeted bounded source reads only at RNA-discovered locations. | Expose RNA MCP consistently or add bounded source retrieval to the CLI. |
