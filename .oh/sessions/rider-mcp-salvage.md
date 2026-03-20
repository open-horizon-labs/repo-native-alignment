# Salvage: JetBrains MCP (Rider/IntelliJ)

## Source
`~/src/open-horizon-labs/mcp-jetbrains` — 285-line Node.js proxy that bridges MCP clients to JetBrains IDEs. DEPRECATED: built into all IntelliJ IDEs since 2025.2. The proxy itself is infrastructure — all tool intelligence lives in the IDE.

## Context
From Slack benchmark: Rider MCP is 5-6x lower context (381 tokens vs 1,552-1,832) but 2-3x slower than built-in Read/Grep. Key finding: **its value is not file reading** — it's `get_file_problems`, `reformat_file`, `rename_refactoring`.

## Aim Filter
RNA is read/align infrastructure. JetBrains MCP is IDE-bridge infrastructure. Almost nothing to borrow from the proxy itself — but the benchmark and Slack context reveal important positioning.

---

## LOW-BAR — Obvious, ought to do

### 1. Nothing to borrow from the proxy code
The repo is 285 lines of HTTP forwarding. Zero analysis logic. Skip.

---

## HIGH-BAR — Relevant positioning

### 1. RNA + Rider MCP = complementary, not competing

The Slack benchmark reveals the natural pairing:

| Capability | RNA | Rider MCP |
|-----------|-----|-----------|
| Find all callers of X | Fast (pre-indexed graph) | Slow (IDE query, IDE must be running) |
| Semantic search "auth-related functions" | Yes (embeddings) | No |
| What breaks if I change X | Yes (impact traversal) | No |
| Real type errors / linter output | No | Yes (IDE diagnostics) |
| Rename across all files with type safety | No (read-only) | Yes (IDE refactoring) |
| Offline / no IDE needed | Yes | No |

**The framing for users:** Use RNA for fast navigation, graph queries, and large-scale analysis. Use Rider MCP to validate, refactor, and get real IDE diagnostics once you know what to change.

RNA's "Compared To" doc should mention this explicitly — Rider MCP is not a competitor, it's a companion.

### 2. IDE diagnostics as a future RNA source

Rider MCP's `get_file_problems` returns IDE-validated errors and warnings per file. This is richer than LSP diagnostics (which RNA already uses) — IDE inspections include static analysis, dead code, unused imports, performance hints that LSP doesn't surface.

RNA doesn't currently index IDE inspection results. If agents could query "what problems does the IDE see in the files changed by this outcome?" that would be a valuable signal.

- **RNA approach:** RNA already indexes `.oh/signals/`. An agent using Rider MCP could write inspection results to `.oh/signals/` and RNA would index them, making them queryable alongside code.
- **Effort:** Zero code changes to RNA — this is an agent workflow, not an RNA feature.
- **Payoff:** "What IDE-detected issues exist in files tagged to outcome X?" becomes answerable via RNA.

---

## MID-BAR — Inventory, probably won't do

| Feature | What Rider MCP does | Why skip |
|---------|-------------------|----------|
| `reformat_file` | IDE-based code formatting | RNA is read-only |
| `rename_refactoring` | Symbol rename across project | RNA is read-only |
| Port-range IDE discovery | Scans 63342-63352 to find IDE | RNA doesn't need IDE connection |
| STDIO↔HTTP bridging | Transparent proxy pattern | RNA connects to LSP servers directly |
| Real-time editor state | Current cursor, selection | RNA indexes persisted content |

---

## Key Architectural Insight

JetBrains MCP confirms the three-tier context assembly model:

```
Tier 1: RNA        — pre-indexed, fast, semantic, graph-based
Tier 2: Rider MCP  — live IDE diagnostics and refactoring
Tier 3: LSP        — raw single-symbol, single-hop queries
```

RNA replaces Tier 3 for most agent use cases. Rider MCP adds a complementary Tier 2 for IDE-specific capabilities (diagnostics, refactoring) that don't exist in Tier 1 or 3.

For JetBrains users specifically: RNA is faster than Rider MCP for navigation (pre-indexed vs live query), but Rider MCP provides IDE intelligence that RNA can't replicate. The "use Rider MCP for its unique capabilities" finding from Slack directly validates RNA's positioning — RNA is the fast navigation layer, Rider MCP is the IDE-action layer.

## Positioning update for docs

Add to `docs/compared-to.md`: RNA is read/navigation; Rider MCP is write/action. Complementary. If you use IntelliJ-family IDEs, use both.
