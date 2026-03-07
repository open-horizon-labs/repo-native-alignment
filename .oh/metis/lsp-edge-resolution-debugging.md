---
id: lsp-edge-resolution-debugging
outcome: agent-alignment
title: LSP edge resolution needs debug observability + better caller matching
---

## What Happened

LSP enrichment infrastructure works (rust-analyzer initializes, finds references for 439 functions) but produces 0 usable Calls edges because:

1. textDocument/references returns ALL references to a function name, including within the function's own body
2. After filtering self-body references and external crates, no cross-function calls remain
3. The enclosing-symbol approach (find what function contains the reference line) resolves to the same function for in-body references

## Root Cause

The approach of "find references → find enclosing symbol → create Calls edge" fails for same-file calls because:
- Function A calls function B at line X
- textDocument/references for B returns line X
- find_enclosing_symbol at line X returns function A (correct!)
- But ALSO returns many lines within B's own body that happen to reference B's name

The self-body filter catches all the in-body references but also catches some legitimate cross-function calls when the function ranges overlap or when the reference appears ambiguous.

## What Would Fix It

1. Use the reference COLUMN position to verify the symbol name at that position matches the function being looked up (avoids false matches)
2. Or: for each reference, call textDocument/definition to verify it resolves back to the original function
3. Or: use textDocument/callHierarchy (LSP 3.16+) which is specifically designed for this — returns callers/callees directly

## Debug Observability Gap

Debugging this took 10+ iterations because we had no way to inspect the graph contents from MCP tools. Need:
- A debug mode on graph_query that shows raw edge data
- A "graph stats" tool that shows edge type counts, node type counts
- Ability to query "show me 5 Calls edges" to verify they exist
