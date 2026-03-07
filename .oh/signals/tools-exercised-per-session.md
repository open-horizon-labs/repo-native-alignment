---
id: tools-exercised-per-session
outcome: agent-alignment
threshold: Agent calls at least 3 RNA MCP tools during a normal work session without being prompted
type: metric
---

# Tools Exercised Per Session

Measures whether agents naturally use the RNA MCP tools during work — not just when explicitly asked.

## Measurement
- Count distinct RNA tool calls per session (from MCP server logs or observation)
- Distinguish prompted vs unprompted usage
- Target: agent reads outcomes/guardrails at session start and records metis at session end without being told

## Why This Matters
If tools exist but agents don't use them, the system has no compound effect. This signal catches adoption failure before it compounds into stale `.oh/` data.
