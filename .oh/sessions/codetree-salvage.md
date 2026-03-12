# Salvage: codeTree

## Source
`~/src/open-horizon-labs/codeTree` — Python MCP server using FastMCP + tree-sitter + SQLite. 23 tools, 10 languages, ~1,058 tests. Zero external dependencies (no embeddings, no vector DB). Published on PyPI as `mcp-server-codetree`.

## Aim Filter
RNA is aim-conditioned decision infrastructure. codeTree is general-purpose structural code analysis. Only salvage what strengthens outcome-to-code alignment or meaningfully improves agent ergonomics for RNA's existing tools.

---

## LOW-BAR — Obvious, ought to do

### 1. Compact output mode (skeleton view)
codeTree's core insight: agents don't need full function bodies for navigation. `get_file_skeleton()` returns name + line + params + one-line docstring — 25x token reduction. RNA's `search_symbols` returns signatures but `graph_query` returns full bodies. A `compact: true` flag on graph_query/search_symbols would let agents explore broadly then drill into specifics.
- **Effort:** Small — truncate body field, return signature + line range only
- **Payoff:** Agents can scan more symbols per context window, then request full bodies only when needed

### 2. Batch symbol retrieval
codeTree's `get_symbols()` and `get_skeletons()` accept lists of qualified names. RNA tools are one-query-at-a-time — agents make N sequential MCP calls to gather context for a single decision. A `nodes: [id1, id2, ...]` param on `graph_query` would cut roundtrips.
- **Effort:** Small — loop over existing single-node logic, return combined result
- **Payoff:** Fewer MCP roundtrips = faster agent workflows, less context spent on tool-call overhead

### 3. Test coverage mapping
codeTree's `find_tests()` locates test functions that reference a given symbol. RNA already extracts test files (and demotes them in ranking) but doesn't surface "which tests cover this function?" as a query.
- **Effort:** Small — reverse lookup from test-file symbols' Calls edges to production symbols
- **Payoff:** Directly useful for outcome_progress: "these tests verify the symbols changed for outcome X"

---

## HIGH-BAR — Innovative, fits our aims

### 1. Change impact with risk classification
codeTree's `get_change_impact()` takes a git diff and traces transitive callers, classifying each as CRITICAL (entry points), HIGH (high-degree hubs), MEDIUM, LOW. RNA's `outcome_progress` answers "what changed for this outcome?" but not "what's at risk from those changes?"
- **RNA approach:** Extend outcome_progress with an optional `include_impact: true` flag. For each modified symbol, walk reverse Calls/DependsOn edges (RNA already has `impact` mode in graph_query). Classify using RNA's existing 5-tier ranking (entry points, edge count). Return risk-annotated dependency tree alongside the commit list.
- **Effort:** Medium — compose existing graph_query impact mode with outcome_progress results
- **Payoff:** Agents answering "is this outcome safe to ship?" get blast radius for free. Directly strengthens the outcome-to-code join.

### 2. PageRank-style symbol importance
codeTree uses PageRank on the call graph for its repository map hotspots. RNA's ranking tier 5 uses raw edge count — but a hub with 3 high-connectivity callers matters more than a leaf called from 5 test files. PageRank captures this transitivity.
- **RNA approach:** Compute PageRank over petgraph at index time (petgraph has `page_rank()` in petgraph-algorithms). Store as `importance` metadata on each node. Use as tie-breaker in ranking.
- **Effort:** Small — petgraph already supports this, one pass at index time
- **Payoff:** Better ranking in search_symbols and oh_search_context. Hub symbols surface first.

### 3. Repository map / entry point detection
codeTree's `get_repository_map()` returns a compact overview: top entry points, hotspot files, and a suggested "start here" path. Agents exploring a new codebase need orientation. RNA's tools assume you already know what you're looking for.
- **RNA approach:** A `list_roots` enhancement or new lightweight query that returns: top-N symbols by PageRank, files with most definitions, and any `.oh/outcomes/` that touch those files. Outcome-conditioned orientation — "here's what matters and which outcomes it serves."
- **Effort:** Medium — aggregation query over existing index data
- **Payoff:** Agents can orient themselves in one tool call instead of exploratory search_symbols loops

---

## MID-BAR — Inventory, probably won't do

| Feature | What codeTree does | Why skip |
|---------|-------------------|----------|
| Clone detection | AST normalization, structural duplicate finding | Code quality metric, not outcome alignment |
| Taint/dataflow analysis | Source→sink tracing with sanitizer detection | Security-focused; RNA is alignment, not security scanning |
| Dead code detection | Symbols defined but never referenced | Derivable from graph (zero incoming edges) but not aim-relevant |
| Doc suggestions | Find undocumented functions | Agent's job, not context server's |
| Git blame/churn/coupling | Per-line authorship, file co-change frequency | RNA's commit tracking covers the outcome-relevant slice; general git analysis is out of scope |
| SQLite graph storage | Persistent graph in SQLite with 5-table schema | RNA chose LanceDB (columnar + vectors + FTS) deliberately; SQLite would be a downgrade |
| FastMCP framework | Python MCP server framework, simpler registration | RNA uses rust-mcp-sdk; different language, different ergonomics |
| Language plugin ABC | 5-method abstract base class per language | RNA's tree-sitter extractors already have per-language query patterns; a trait abstraction would add ceremony without benefit since RNA also has LSP as a second extraction channel |
| Skeleton caching (mtime) | `.codetree/index.json` with mtime-based invalidation | RNA doesn't do incremental skip yet (re-extracts on every scan). Content hashing is already identified as a LOW-BAR item in the CGR salvage — codeTree's mtime approach is less reliable than the BLAKE3 hashing planned there |
| Cyclomatic complexity | Per-function control-flow counting | RNA already extracts this in tree-sitter phase and stores in node metadata |
| Mermaid dependency graphs | File-level dependency visualization | Presentation concern; agents can format graph_query output however they want |

---

## Key Architectural Insight
codeTree is a *breadth-first* code analysis tool — 23 tools covering many use cases (security, docs, clones, dead code, visualization). RNA is *depth-first* on one use case — outcome-to-code alignment with semantic search. The valuable salvage isn't features (RNA has deeper extraction via LSP, richer storage via LanceDB) but **agent ergonomics**: compact output, batch operations, and orientation tools that reduce the number of MCP roundtrips an agent needs to make a decision. The HIGH-BAR items (impact classification, PageRank, repo map) are worth doing because they make RNA's existing structural joins more *actionable* — turning "here are the related symbols" into "here's the risk profile for this outcome."
