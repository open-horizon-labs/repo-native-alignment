---
id: tools-exercised-per-session
outcome: agent-alignment
threshold: Agent calls at least 3 RNA MCP tools during a normal work session without being prompted
type: metric
---

# Tools Exercised Per Session

Measures whether agents naturally use the RNA MCP tools during work — not just when explicitly asked.

## Observation: 2026-03-07 (workspace-context-engine session)

**Result: PASS — 10+ distinct tools called**

Tools exercised during this session:
- `oh_get_outcomes` — read business context at session start
- `oh_get_guardrails` — loaded constraints
- `oh_get_signals` — checked signal status
- `oh_get_metis` — reviewed learnings
- `oh_search_context` — semantic search for related context
- `oh_record_metis` — recorded fspulse salvage learnings
- `oh_record_guardrail_candidate` — recorded 2 new guardrails
- `oh_update_outcome` — updated outcome mechanism + files
- `search_symbols` — queried multi-language code graph
- `graph_neighbors` — traversed import edges
- `graph_impact` — tested reverse BFS
- `outcome_progress` — checked structural join

Most calls were unprompted — the agent used RNA tools as part of its normal workflow (aim → salvage → execute → review cycle).

## Measurement
- Count distinct RNA tool calls per session (from MCP server logs or observation)
- Distinguish prompted vs unprompted usage
- Target: agent reads outcomes/guardrails at session start and records metis at session end without being told

## Why This Matters
If tools exist but agents don't use them, the system has no compound effect. This signal catches adoption failure before it compounds into stale `.oh/` data.
