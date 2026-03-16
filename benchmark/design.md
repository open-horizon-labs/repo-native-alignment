# RNA vs Vanilla Benchmark Design

## Hypothesis
RNA MCP tools reduce token usage and improve solution quality for code
exploration tasks that require multi-hop traversal, semantic search,
blast radius analysis, or codebase orientation.

## Method

### Agents
- **Vanilla**: Grep, Read, Glob, Bash only. Explicitly instructed NOT to use RNA MCP tools.
- **RNA**: Same tools + RNA MCP. Instructed to prefer RNA tools for exploration.

### Task
Design a Spotify Connect adapter for unified-hifi-control (25K lines Rust).
Same prompt for both agents (see prompt.md).

### Metrics (per run)
- `total_tokens`: input + output tokens
- `duration_ms`: wall time
- `tool_uses`: count by tool type
- `solution`: the full design document produced

### Eval (per pair)
Blind head-to-head by third agent. Criteria:
| Criterion | Weight |
|---|---|
| Correct trait identification (Startable, impl_startable!) | 20% |
| Bus integration accuracy (PrefixedZoneId, BusEvent) | 20% |
| Discovery approach (mdns.rs, LMS UDP discovery) | 15% |
| Reference adapter choice and rationale | 15% |
| Volume flow trace completeness | 15% |
| Config completeness | 10% |
| Specificity (exact files/structs named) | 5% |

### Sample size
- Pilot: 1 run each (validate data capture)
- Full: 10 runs each (20 total)

### Controls
- Same model (inherited from parent)
- Same repo state (no changes between runs)
- Same prompt text
- Randomized eval order (evaluator doesn't know which is which)
