# P4: Solution Space — Next Highest-Leverage for Agent Alignment

**Updated:** 2026-03-07

## Problem

RNA has 16 working MCP tools, a proven read-write loop, and skills integration underway. What compounds the most value next?

**Key Constraint:** Alignment is the constraint, not a hypothesis. Act on strong beliefs.

## Candidates Considered

| # | Option | Level | Trade-off |
|---|--------|-------|-----------|
| 1 | Grounded oh_init (OH graph) | Redesign | Couples to OH availability |
| 2 | Test on second repo | Reframe | No new features; just learning |
| 3 | Embeddings / LanceDB | Local Optimum | Heavy infra; grep works fine |
| 4 | Cross-references: code spans → symbols | Reframe | May be noisy on common names |
| 5 | Multi-language tree-sitter | Local Optimum | Broadens reach, doesn't deepen |
| 6 | Ship as installable package | Band-Aid | Reduces friction, not quality |

## Recommendation

**Selected:** Option 1 (Grounded oh_init) then Option 4 (Cross-references)

### Why oh_init first

Session 1 proved the cold-start problem is real. This entire session was a manual oh_init. If oh_init queries `oh_get_endeavors` and seeds `.oh/` from organizational context, every new repo starts grounded. This is the adoption multiplier.

**Implementation:**
1. When OH MCP available: find matching endeavor by repo name, pull aim as outcome body, seed signals from aim's declared feedback, seed guardrails from OH guardrails
2. When OH MCP absent: fall back to current behavior (Cargo.toml/package.json inference)
3. Always: scaffold directory structure, generate AGENTS.md integration section

### Why cross-references second

Once `.oh/` exists with real outcomes, the next friction is navigating from those documents to code. Code spans in markdown (`outcome_progress`, `write_artifact`, etc.) should resolve to actual symbols.

**Implementation:**
1. Extract code spans from all `.oh/` markdown via pulldown-cmark (already done)
2. Match each span against tree-sitter symbol table (exact name match)
3. Return: `(code_span, file_path, line, symbol_kind)` tuples
4. Integrate into `outcome_progress` — show symbol-level connections, not just file-level

### Why not the others

- **Test on second repo** — better after oh_init is grounded; otherwise we rediscover the same cold-start problem
- **Embeddings** — fails "validate before building" guardrail; grep works for agents who read `.oh/`
- **Multi-language** — do when a specific second repo demands it
- **Installable package** — not adoption-constrained yet

### Sequence

1. Grounded oh_init (next session)
2. Cross-references: code spans → symbol table
3. Test on second repo with both features
