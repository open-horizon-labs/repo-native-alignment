# Salvage: code-graph-rag (CGR)

## Source
`~/src/open-horizon-labs/code-graph-rag` — Python RAG system using Memgraph + Qdrant + tree-sitter + UniXcoder embeddings. 237 test files, 11 languages, MCP server with 9 tools.

## Aim Filter
RNA is aim-conditioned decision infrastructure. CGR is general-purpose code RAG. Only salvage what strengthens outcome-to-code alignment.

---

## LOW-BAR — Obvious, ought to do

### 1. Content hashing for incremental skip
CGR hashes file contents (SHA256) to skip unchanged files. RNA uses mtime which is less reliable (touch without change, editor save-without-modify).
- **Effort:** Small — hash on scan, compare before extract
- **Payoff:** Fewer wasted re-extractions

### 2. Bounded memory cache (LRU + memory limits)
CGR's `BoundedASTCache` uses LRU eviction with configurable memory threshold. RNA's petgraph index grows unbounded.
- **Effort:** Small — Rust has excellent bounded-map crates
- **Payoff:** Prevents OOM on large repos
- **Ref:** `codebase_rag/graph_updater.py:BoundedASTCache`

### 3. Binary file detection
CGR detects binary files (PNG, PDF) and skips. RNA should verify scanner never feeds binaries to tree-sitter.
- **Effort:** Trivial — likely already handled, verify

---

## HIGH-BAR — Innovative, fits our aims

### 1. Semantic graph entry points (NL→graph)
CGR translates natural language → Cypher queries via LLM. RNA's `graph_query` requires knowing a node ID — agents skip it because the API is too structural.
- **RNA approach:** Accept natural language query, embed it, match against node signature embeddings, return structurally-connected subgraph. Semantic entry → structural traversal. No external service — uses existing local Metal GPU embedding index.
- **Effort:** Medium — extend `graph_query` to accept optional `query` param alongside `node`, use existing embedding layer
- **Payoff:** Directly addresses #81 (agents prefer Grep over RNA tools). One tool call instead of two.

### 2. Function-level semantic embeddings
CGR embeds individual function bodies using UniXcoder (code-specific model). RNA embeds signatures but not full bodies.
- **Why it fits:** `oh_search_context` with intent-based queries ("error handling", "rate limiting") is exactly what aim-conditioned agents need.
- **Effort:** Medium — change what gets embedded, potentially swap embedding model
- **Payoff:** Better recall for outcome-scoped queries
- **Ref:** `codebase_rag/embedder.py`

### 3. AST-boundary chunking
CGR uses tree-sitter AST boundaries as chunk boundaries — never splits a function. RNA already extracts at AST boundaries but may not embed at those same boundaries.
- **Effort:** Small (we already have the boundaries)
- **Payoff:** Higher quality embeddings, no cross-function noise

---

## MID-BAR — Inventory, probably won't do

| Feature | What CGR does | Why skip |
|---------|--------------|----------|
| Surgical code editing | AST-targeted function replacement | RNA is read/align, not write/edit |
| Interactive optimization | LLM suggests refactoring, user approves | Agent's job, not context server's |
| Multi-provider LLM | Pluggable OpenAI/Gemini/Ollama | RNA doesn't call LLMs — no LLM dependency is a feature |
| Reference-doc optimization | Coding standards guide LLM suggestions | `.oh/guardrails/` already surfaces these |
| Real-time filesystem watcher | `watchdog` for live sync | Event-driven reindex + background scanner sufficient |
| Memgraph/Cypher storage | Property graph DB | LanceDB + petgraph chosen deliberately, no Docker dep |
| VS Code extension | Editor integration | MCP works with any client |
| Type inference engine | Regex-based for Python/TS/Java | RNA uses LSP — real type info, not regex heuristics |
| Call resolver with inheritance | 700 LOC Python module | LSP enrichment handles this more accurately |
| Document analysis (PDF/images) | Binary content extraction | Out of scope |

---

## Key Architectural Insight
CGR is a general-purpose code RAG system. RNA's differentiation is structural outcome-to-code joins. The high-bar items (semantic graph entry, function-level embeddings, AST-boundary chunking) are powerful because they make the structural join more *discoverable* — which is RNA's core value proposition.
