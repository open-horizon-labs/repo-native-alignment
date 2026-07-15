---
id: pr-754-ship
outcome: context-assembly
severity: mixed
---

# PR #754 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA MCP tools | skipped | RNA MCP tools were not exposed to the issue agent. | Repository context and source inspection required targeted `rg`/`sed` fallback. | Preserve this evidence for RNA tool-delivery debugging. |
| 2026-07-15 | `rg`/`sed` fallback | skipped | Targeted reads of `README.md`, `plugin/skills/setup/SKILL.md`, `src/setup.rs`, and workflow instructions were used after RNA MCP was unavailable. | The issue was still reviewable, but not through the mandated dogfood path. | Use RNA MCP when it is available to the agent session. |
| 2026-07-15 | `repo-native-alignment scan --repo . --full` | blocking for graph review | The mandatory ship scan remained sleeping with no output and 0% CPU for roughly nine minutes during LSP enrichment and was interrupted. | Graph impact analysis is unavailable for this documentation/setup-guidance change. Diff review and targeted setup tests remain available. | Investigate the scan/LSP no-progress path separately; do not represent this run as a successful full scan. |
