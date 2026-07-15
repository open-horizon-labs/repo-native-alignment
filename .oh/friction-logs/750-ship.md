---
pr: 750
date: 2026-07-15
outcome: context-assembly
---

# PR #750 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA MCP `repo_map` / `search` / `outcome_progress` | skipped | No RNA MCP tools were exposed in this agent session. | Used the installed RNA CLI with `--repo .` for index readiness, artifact search, and caller traversal. | Ensure issue sub-agents inherit the repository MCP server. |
| 2026-07-15 | Ship scan-gate empty query | minor | The documented `search ""` readiness probe now rejects empty queries. | Used an exact `persist_graph_incremental` query, which loaded 22,360 cached symbols and confirmed a current schema-v23 index. | Update the ship gate to use a supported readiness command or non-empty probe. |
| 2026-07-15 | Pre-scan local `sed` reads | skipped | The ship procedure and computed-but-not-delivered metis were opened before the CLI scan gate was confirmed. | No code decision depended on those reads; subsequent code/artifact review used RNA plus the GitHub diff. | Move the scan gate ahead of all procedure-linked code-context reads in future runs. |
| 2026-07-15 | Long Cargo command bridge | minor | The command bridge detached after 30 seconds while optimized Lance dependencies continued compiling. | Cargo work stayed serialized; process checks confirmed progress and the completed checks were rerun for captured pass output. | Prefer a persistent PTY/session for long Cargo quality gates. |
