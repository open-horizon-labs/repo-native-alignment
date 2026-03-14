---
id: mcp-tool-exercise-findings
outcome: agent-alignment
title: 'MCP tool exercise: graph edges sparse for structs and constants'
---

Exercising all 8 MCP tools revealed that graph_query returns rich results for git merge nodes but sparse edges for code-level struct/const nodes. Scanner struct shows 0 reachable nodes despite being central to the pipeline. MAX_BATCH_SIZE const only traces to a PR merge commit, not to the functions that reference it. This suggests the graph edge population from tree-sitter extraction may not yet capture usage-site references (where a struct is instantiated or a const is read) — only declaration-site edges exist. LSP enrichment may be the intended path to fill these gaps.
