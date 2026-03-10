# Salvage: OpenDev + arXiv paper (2603.05344v1)

## Sources
- `~/src/open-horizon-labs/opendev` — Python terminal-native AI coding agent (ReAct loop, 37 LSP servers, compound AI architecture)
- arXiv:2603.05344v1 — "Building AI Coding Agents for the Terminal" by Nghi D. Q. Bui (March 2026)

## Aim Filter
RNA is persistent code intelligence infrastructure (graph + MCP tools). OpenDev is an agent harness (ReAct loop + tools). Different layers. Only salvage what strengthens RNA's graph quality or LSP integration.

---

## LOW-BAR — Obvious, ought to do

### 1. Per-capability LSP fallback
OpenDev queries each LSP server's `ServerCapabilities` during init. If a server supports `textDocument/definition` but not `callHierarchy/incomingCalls`, it still gets definition edges and falls back to tree-sitter for call edges.
- **RNA today:** Falls back per-server (skip entirely if missing), not per-capability
- **Effort:** Small — query `ServerCapabilities` during init, gate enrichment per capability
- **Payoff:** More LSP data from partially-capable servers
- **Ref:** `opendev/core/context_engineering/tools/lsp/wrapper.py`

### 2. Stale server auto-restart
OpenDev detects crashed LSP processes and auto-recreates on next request. Dead server → delete from cache → fresh spawn.
- **RNA today:** If LSP process dies mid-scan, enrichment likely fails silently
- **Effort:** Small — check process liveness before requests, respawn if dead
- **Ref:** `opendev/core/context_engineering/tools/lsp/ls_handler.py:318-333`

### 3. Persistent symbol cache with content-hash invalidation
OpenDev caches parsed symbols to disk (pickle format), keyed by file content hash. Only re-parses files whose content actually changed. Cache versioning + migration logic included.
- **RNA today:** Uses mtime-based scan state
- **Effort:** Small-medium — add content hash to scan state
- **Ref:** `opendev/core/context_engineering/tools/lsp/ls/cache.py`

---

## HIGH-BAR — Innovative, worth considering

### 1. Call hierarchy for impact analysis (LSP-native)
OpenDev uses `callHierarchy/incomingCalls` and `outgoingCalls` to trace callers/callees across the codebase. This is the LSP-native equivalent of RNA's `graph_query(mode: "impact")`.
- **RNA opportunity:** Feed call hierarchy results directly into petgraph edges during enrichment. This gives type-accurate call chains that tree-sitter alone can't determine (especially for dynamically dispatched calls, trait methods, generic functions).
- **Effort:** Medium — add call hierarchy queries to LSP enrichment pass
- **Payoff:** Dramatically more accurate impact analysis edges
- **Ref:** `opendev/core/context_engineering/tools/lsp/ls_request.py:75-95`

### 2. Type hierarchy for architectural edges
OpenDev uses `typeHierarchy/supertypes` and `subtypes` to discover inheritance chains, interface implementations, and trait bounds.
- **RNA opportunity:** These map directly to `implements` and `inherits` edge types in petgraph. Currently RNA infers these from tree-sitter (syntactic), but LSP gives semantic accuracy (resolves trait bounds, generic constraints, cross-file inheritance).
- **Effort:** Medium — add type hierarchy queries, merge into graph
- **Payoff:** Better architectural understanding, more accurate `implements` edges
- **Ref:** `opendev/core/context_engineering/tools/lsp/ls_request.py:149-169`

### 3. Diagnostics as quality signals
OpenDev exposes `textDocument/diagnostic` for error/warning extraction with severity filtering.
- **RNA opportunity:** Feed LSP diagnostics into `.oh/signals/` as code quality measurements. "How many type errors in outcome-linked files?" becomes a computable signal.
- **Effort:** Medium — new signal type, connect to outcome file patterns
- **Payoff:** Automated quality signals for outcome progress tracking
- **Ref:** `opendev/core/context_engineering/tools/lsp/wrapper.py:340-394`

---

## MID-BAR — Inventory, probably won't do

| Feature | What OpenDev does | Why skip |
|---------|------------------|----------|
| LSP write tools (rename, replace, insert) | 8 semantic edit tools for agents | RNA is read-only; agents have their own editors |
| 5-stage context compaction | Progressive conversation summarization | Agent harness concern, not MCP server |
| Event-driven system reminders | Inject user-role reminders on loops/errors | Agent harness concern |
| Lazy tool discovery (search_tools) | Meta-tool to discover MCP tools | RNA has 7 tools — not needed |
| Compound AI model binding | Per-workflow model assignment | RNA serves LLMs, doesn't orchestrate them |
| ACE Playbook strategy memory | Embedding-based strategy retrieval | Interesting but belongs in OH Skills |
| Docker sandboxing | Container execution for untrusted code | RNA doesn't execute code |
| Web UI | React + FastAPI + WebSocket | MCP interface sufficient |
| Multi-provider secrets rotation | Rotating API keys across providers | RNA delegates to MCP client config |

---

## Key Architectural Insights

### OpenDev validates RNA's design
OpenDev's CodeExplorer subagent does multi-step LSP queries to build understanding (find symbol → find references → follow → reason). RNA pre-computes this into a persistent graph — one `graph_query(mode: "impact")` call replaces an agent's multi-step exploration loop. **External validation that the graph-first approach is differentiated.**

### Ephemeral vs persistent
OpenDev's LSP results are ephemeral (go into conversation, discarded). RNA's are persistent (stored in LanceDB + petgraph, survive across sessions). For aim-conditioned agents that need cross-session continuity, persistent is correct.

### Paper's honest admission on cross-language
Paper explicitly states: "Building a polyglot dependency graph is intractable; instead, provide powerful tools per language and trust agent to synthesize." RNA already does more here with cross-language constant search and topology edges. This is a differentiator.

### The 37 LSP servers are the same
Both RNA and OpenDev support ~37 LSP servers. The difference is depth of integration: OpenDev uses them transactionally (per agent query), RNA should use them for graph enrichment (batch, persistent). OpenDev's mixin architecture (`FileOpsMixin`, `RequestsMixin`, `SymbolsMixin`, `CacheMixin`) is a clean reference for how to structure the LSP client.

---

## LSP Integration Reference (from OpenDev)
For RNA's LSP enrichment work, key files to reference:
- `opendev/core/context_engineering/tools/lsp/ls_handler.py` — JSON-RPC 2.0 client, process lifecycle
- `opendev/core/context_engineering/tools/lsp/ls_request.py` — All LSP request types (34 methods)
- `opendev/core/context_engineering/tools/lsp/wrapper.py` — High-level symbol API
- `opendev/core/context_engineering/tools/lsp/ls/cache.py` — Persistent caching with versioning
- `opendev/core/context_engineering/tools/lsp/language_servers/` — 40+ per-language implementations
