# Salvage: CodeGraphContext (CGC)

## Source
`~/src/open-horizon-labs/CodeGraphContext` — Python code graph toolkit with Neo4j/KuzuDB/FalkorDB backends, 18 language parsers, VS Code extension, web UI, MCP server with 17 tools. v0.2.14, single maintainer.

## Aim Filter
RNA is aim-conditioned decision infrastructure (outcome→code joins). CGC is general-purpose code exploration + visualization. Only salvage what strengthens outcome alignment or graph quality.

---

## LOW-BAR — Obvious, ought to do

### 1. Dead code detection heuristic
CGC identifies functions with zero in-project callers, excluding entry points (`main`, `setup`, `__dunder__`), test functions (`test_*`), and decorated functions. Simple graph query.
- **RNA approach:** `graph_query(mode: "impact", direction: "incoming")` with count=0 filter. Already have the graph for this.
- **Effort:** Small — query logic only, no new extraction
- **Payoff:** Useful signal for outcome progress ("outcome-linked code has N dead functions")
- **Ref:** `src/codegraphcontext/tools/code_finder.py:514-552`

### 2. Relevance scoring for search results
CGC fuses results from 4 sources (function name 0.9, class name 0.8, variable 0.7, content 0.6) with dependency penalty (-0.2). RNA's `search_all` returns unranked results.
- **Effort:** Small — attach relevance metadata to existing search
- **Payoff:** Agents get better context prioritization from `oh_search_context`
- **Ref:** `src/codegraphcontext/tools/code_finder.py:176-224`

### 3. Call chain path-finding API
CGC exposes "find path from function A to function B" via variable-length Cypher paths. RNA has `graph_query(mode: "reachable")` but doesn't expose start→end path finding.
- **Effort:** Small — filter reachable query with start/end
- **Payoff:** "How does this outcome's entry point reach this dependency?"

---

## HIGH-BAR — Innovative, worth considering

### 1. SCIP indexing for compiler-grade accuracy
CGC runs language-specific SCIP indexers (Pyright, tsc, scip-go, scip-rust) that emit protobuf with correct symbol definitions + references. No heuristics — compiler-verified.
- **Why it fits:** Impact analysis accuracy depends on edge correctness. "Is this call actually reachable?" requires correct CALLS edges, not import-map guessing.
- **RNA comparison:** RNA uses tree-sitter + LSP enrichment. SCIP would be a third source with highest confidence, complementing the existing two-tier approach.
- **Effort:** Large — protobuf parsing, fallback logic, per-language tool detection
- **Payoff:** Correct edges for the languages where it matters most (Python, TypeScript, Rust, Go)
- **Ref:** `src/codegraphcontext/tools/scip_indexer.py`

### 2. Pre-indexed bundle distribution (.cgc format)
CGC exports graphs as portable ZIP bundles (JSON-L nodes/edges + metadata). Pre-indexes famous repos weekly. Instant load without re-scanning.
- **Why it fits:** Not for distributing OSS graphs, but the format is interesting for **outcome bundles** — package up a project's outcome→code graph state and share it across team members or CI.
- **RNA twist:** `.oh/` is already git-versioned. But the graph cache in `.oh/.cache/` isn't portable. A bundle format could make the cache shareable without re-scanning.
- **Effort:** Medium — export LanceDB tables + petgraph edges as portable format
- **Payoff:** Skip cold-start scan on fresh clones, CI environments
- **Ref:** `src/codegraphcontext/core/cgc_bundle.py`

### 3. KuzuDB as embedded graph database
KuzuDB is zero-config, cross-platform (Windows native!), embedded in-process. No Docker, no server. CGC documented all the Cypher dialect gotchas in `KUZUDB_FIXES.md`.
- **Why it fits:** RNA's LanceDB is already embedded. But if RNA ever needs a real graph query language (Cypher), KuzuDB is the embeddable option. The pitfall documentation alone is valuable.
- **Effort:** Large — would require significant query layer changes
- **Payoff:** Real Cypher queries without Neo4j dependency. Windows support.
- **Ref:** `KUZUDB_FIXES.md`, `src/codegraphcontext/core/database_kuzu.py`

---

## MID-BAR — Inventory, probably won't do

| Feature | What CGC does | Why skip |
|---------|--------------|----------|
| VS Code extension | 2,500 LOC, D3.js graphs, code lens, tree views | MCP works with any client; extension is maintenance burden |
| Web UI + marketing site | React + Sigma.js + Vercel | RNA's value is infrastructure, not a product website |
| Multi-database abstraction | Neo4j/FalkorDB/KuzuDB runtime switching | LanceDB + petgraph is the right choice for embedded |
| Live file watching (watchdog) | Debounced re-index on file change | RNA's event-driven scanner + background loop sufficient |
| Neo4j Browser visualization | URL generation with pre-filled Cypher | No external DB dependency |
| Interactive graph visualization | Standalone HTML with D3/Vis.js | Interesting but orthogonal to outcome alignment |
| Package dependency resolution | `importlib.import_module` to find installed deps | Privacy concerns; outcome-scoped doesn't need dep internals |
| Decorator querying | `find_functions_by_decorator` tool | Niche; RNA extracts decorators but querying them adds marginal value |
| Cyclomatic complexity exposure | Pre-computed during parse, stored as node property | RNA computes but doesn't need to expose as tool |
| K8s deployment manifests | Full production K8s setup (6 manifests) | RNA is a local binary, not a deployed service |
| Docker quickstart | One-command setup with Docker Compose | RNA is `cargo install` — simpler |
| Variable shadowing analysis | Scope tracking for variable reassignment | Niche analysis, not aligned with outcome joins |
| 17 MCP tools | Broad coverage of code exploration | RNA's 7 focused tools is a feature, not a gap |

---

## Key Architectural Insight

CGC and RNA share the same foundation (tree-sitter parsing → graph DB → MCP tools) but diverge sharply on purpose. CGC is **breadth-optimized** (18 languages, 17 tools, 4 DBs, VS Code extension, web UI). RNA is **depth-optimized** (outcome→code joins, LSP enrichment, topology detection, semantic embeddings).

The most transferable ideas are the ones that improve **graph correctness** (SCIP indexing, dead code detection) or **graph portability** (bundle format). The UX features (VS Code, web UI, visualization) are impressive engineering but orthogonal to RNA's mission as infrastructure.

## KuzuDB Pitfalls (reference from KUZUDB_FIXES.md)
If RNA ever evaluates KuzuDB:
- No polymorphic MERGE — need type-specific queries per node/edge combo
- No union labels `(n:Function|Class)` — use separate queries + union
- Variable-length path binding broken for named end nodes — use anonymous + `nodes(path)` + `list_extract`
- ORDER BY scope issues with DISTINCT — use aliases
- 97% of CGC's test suite passes on KuzuDB (3% need workarounds)
