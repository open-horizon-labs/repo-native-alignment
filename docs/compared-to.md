# RNA Compared To Similar Tools

RNA builds a better code graph — more languages, compiler-grade edges from LSP, in-process microsecond queries — and then does something none of the others attempt: connects that graph to declared business outcomes.

## The Baseline: Raw LSP

LSP (Language Server Protocol) is what your editor already uses. It's the null state — what agents get if you install nothing else. Understanding LSP's limits explains why tools like RNA exist.

**What LSP gives you:** Single-symbol, single-hop, single-language queries. `textDocument/references` returns direct references to one symbol. `callHierarchy/incomingCalls` returns one level of callers. Each request is one symbol, one hop.

**What LSP doesn't give you:**
- **No multi-hop traversal** — "what's the blast radius of changing this?" requires N sequential round-trips, one per hop, with the agent assembling the graph in its context window
- **No cross-language queries** — each LSP server handles one language; connecting a TypeScript caller to a Python service requires separate servers and manual stitching
- **No semantic search** — LSP finds exact symbols, not "functions related to payment processing"
- **No history** — LSP reflects current state only; no git integration, no "who changed this and why"
- **No business context** — LSP has no concept of outcomes, signals, or constraints

**The practical cost:** Early testing shows agents take ~120s and ~2x the tokens to answer structural questions with raw LSP available, vs ~50s and ~half the tokens with RNA — because LSP forces many small round-trips where RNA pre-assembles the graph for single-call traversal.

RNA uses LSP internally as one enrichment source (call hierarchy, type hierarchy, implements edges), fuses the results with tree-sitter, embeddings, git, and business artifacts, and exposes it all through multi-hop graph queries. For agents, RNA replaces the need for separate LSP plugins. LSP enrichment uses adaptive wait (no fixed timeout — waits for `serverStatus/quiescent` when available, probes otherwise, 10-minute circuit breaker as safety net). Misconfigured servers abort early if they produce 0 edges after 1,000 nodes, with a clear diagnostic message.

## At a Glance

| | **LSP (baseline)** | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** | **GitNexus** | **codeTree** | **Serena** |
|---|---|---|---|---|---|---|---|
| **Install** | Editor plugin or PATH binary | CI/release artifact binary | Docker + uv + Memgraph + API key | `pip install` + (KuzuDB\|Neo4j) | `npm install -g gitnexus` | `pip install mcp-server-codetree` | `pip install mcp-server-serena` |
| **External deps** | One server per language | None | Docker, Memgraph, LLM API | Graph DB (embedded or Docker) | None (LadybugDB embedded) | None | None (LSP servers auto-downloaded) |
| **Parsed languages/formats** | One per server | Broad code/schema/config coverage | Tree-sitter coverage | Tree-sitter/SCIP coverage | Broad graph coverage | Tree-sitter coverage | Broad LSP-backed coverage |
| **Graph storage** | In-memory per session | embedded graph + vector index | Memgraph (Docker) | KuzuDB/FalkorDB/Neo4j | LadybugDB (embedded graph + vector + FTS) | SQLite (embedded) | None (LSP only, no persistent graph) |
| **Embeddings** | None | MiniLM-L6-v2 on Metal GPU (local) | UniXcoder (local) | None | Snowflake Arctic Embed XS (opt-in `--embeddings`) | None | None |
| **LSP integration** | Is LSP | Auto-detected batch enrichment | None | None | None | None | solidlsp-backed navigation |
| **Query model** | Single-symbol, single-hop | Multi-hop, cross-language | Multi-hop | Multi-hop | Multi-hop (Cypher) | Multi-hop | Single-symbol (LSP calls, not graph) |
| **MCP surface** | N/A (protocol, not MCP) | Consolidated read/alignment tools | Broad read/write/admin tools | Broad analysis tools | Search/context/impact workflow tools | Broad structural analysis tools | Broad LSP navigation/editing tools |
| **CLI parity** | N/A | Full (shared service layer) | N/A | N/A | Yes (`query`, `context`, `impact`, `cypher`) | N/A | N/A |
| **Cross-encoder reranking** | None | Jina Reranker v1 Turbo (opt-in) | None | None | None | None | None |
| **Business context** | None | Outcomes, signals, guardrails, metis | None | None | None | None | Agent memories (markdown files, agent-written) |

## Architecture Trade-offs

| Axis | LSP | RNA | CGR | CGC | GitNexus | CT | Serena |
|------|-----|-----|-----|-----|----------|-----|--------|
| **Cold start** | Server init (seconds) | ~5-10s scan, ~2min embed (adaptive LSP wait) | Index + Docker startup | Index + DB setup | Requires `gitnexus analyze` first (~251s for large repos) | ~1s scan + SQLite index | LSP server init (seconds) |
| **Warm restart** | Server re-init | <1s (on-disk cache). `scan --full` is incremental — re-extracts only changed files, re-runs LSP only on changed nodes. ~0.1s on no-change runs. | Memgraph persists | DB persists | Fast (LadybugDB on disk) | SQLite persists (mtime invalidation) | LSP server re-init |
| **Memory** | Per-server process | in-process (no external DB) | Docker container | External or embedded DB | In-process (no external DB) | In-process (SQLite) | Per-server process |
| **Query latency** | ms per hop (N round-trips) | ms total (in-process, single call) | Network hop to Memgraph | Network hop or embedded | ms (embedded, Cypher overhead per traversal) | ms (embedded SQLite) | ms per hop (N LSP round-trips) |
| **Offline capable** | Yes | Fully offline | Needs Docker | Depends on DB choice | Fully offline | Fully offline | Yes |

RNA's zero-dependency design is a deliberate architectural choice. The shipped binary from CI/release artifacts runs without Docker, an external DB, or an API key. Development source builds still use normal Rust checks/tests, but release readiness is validated against the CI-produced artifact.

## Graph Quality

| Edge source | LSP | RNA | CGR | CGC | GitNexus | CT | Serena |
|-------------|-----|-----|-----|-----|----------|-----|--------|
| Tree-sitter (syntactic) | None | Built-in code, config, schema, and docs extractors | Yes | Yes | Yes | Yes | None (uses LSP not tree-sitter) |
| LSP (semantic) | Is the source | Auto-detected call + reference + type hierarchy | None | None | None | None | Is the source |
| SCIP (compiler) | None | Not needed — LSP covers the same edges | None | Pyright, tsc, scip-go, scip-rust | None | None | None |
| Embedding similarity | None | MiniLM-L6-v2, cosine distance | UniXcoder | None | Snowflake Arctic Embed XS (opt-in) | None | None |
| Cross-language | No (one server per language) | Yes (unified graph) | Yes | Yes | Yes | Yes | Yes (separate servers) |
| Multi-hop | No (agent must loop) | Yes (single call) | Yes | Yes | Yes (Cypher) | Yes | No (agent must loop) |

LSP provides the raw semantic data — call hierarchy, type hierarchy, references — but only for one language at a time, one hop at a time. RNA consumes LSP as one enrichment source among several, fuses the results into a cross-language graph, and exposes multi-hop traversal. CGR and CGC skip LSP entirely, relying on tree-sitter (syntactic) or SCIP (compiler) for edges. RNA's two-tier approach (tree-sitter + LSP) gives broad coverage with high accuracy: tree-sitter extracts code/config/schema/doc structure, then LSP enrichment adds compiler-grade call, reference, and type relationships that neither CGR nor CGC have.

> **Why not SCIP?** RNA spiked SCIP as a third enrichment tier ([#114](https://github.com/open-horizon-labs/repo-native-alignment/pull/114)). SCIP and LSP produce the same semantic edges — call hierarchy, type hierarchy, implements — because SCIP indexers (rust-analyzer, scip-python, scip-typescript, scip-go) are often the same tools as LSP servers, run in batch mode instead of live. The difference: SCIP requires installing a separate indexer per language, running a batch index step that produces a protobuf file, and parsing that file. LSP servers are already on most developers' PATH (your editor uses them), start on demand, and return the same edges through a standard protocol. SCIP adds a build step and a dependency for no additional edge coverage. RNA salvaged the reusable patterns from the spike (process timeout, file-line index, edge deduplication — [#122](https://github.com/open-horizon-labs/repo-native-alignment/pull/122)) and closed the SCIP enricher as unnecessary. CGC's use of SCIP makes sense if you don't have LSP integration — but RNA does.

## Semantic Search

| | LSP | RNA | CGR | CGC | GitNexus | CT | Serena |
|---|---|---|---|---|---|---|---|
| **Model** | N/A | MiniLM-L6-v2 + Jina Reranker v1 Turbo | UniXcoder (code-specific) | None | Snowflake Arctic Embed XS (opt-in) | None | N/A |
| **Hardware** | N/A | Metal GPU on Apple Silicon, CPU fallback | CPU | N/A | GPU optional (transformers.js), CPU fallback | N/A | N/A |
| **What's embedded** | N/A | Function bodies, all markdown, commits | Function bodies | Nothing | Function bodies, node content | Nothing | N/A |
| **Indexed together** | N/A | Code + markdown + git history | Code only | N/A | Code + limited docs | N/A | N/A |
| **Score normalization** | N/A | relevance-ranked, test files demoted | Raw similarity | N/A | RRF (BM25 + semantic) | N/A | N/A |
| **Body retrieval** | Reference only | `include_body` (requires `node`/`nodes`) returns full source; `minify_body` strips comments + shortens locals via tree-sitter AST (TS/JS, Rust, Python, Go) with legend | None | None | `include_content: true` (binary on/off, no minification) | None | Reference only |
| **Markdown** | N/A | Heading-scoped chunks with hierarchy | None | None | Limited | None | N/A |

RNA's unique advantage: semantic search spans code AND business artifacts in the same vector space. "Find functions related to our payment reliability outcome" is a query only RNA can answer. Results are relevance-ranked: exact name > contains > signature-only, definitions before imports, production code before tests. CGR's UniXcoder is a code-specific model (better at pure code semantics), but RNA embeds function bodies, all markdown (chunked by heading), and commit messages together — breadth over specialization.

## MCP Tool Philosophy

**RNA** — one `search` tool handles code symbols, artifacts, markdown, commits, and graph traversal. Supports subsystem-scoped search (`subsystem=`), cross-subsystem edge filtering (`target_subsystem=`), body retrieval with optional tree-sitter minification (`include_body` + `minify_body`, requires `node`/`nodes`), and impact mode that auto-summarizes high-connectivity results into a subsystem-grouped breakdown. Empty or whitespace-only `mode` is treated as omitted, so MCP clients do not accidentally request an invalid traversal. `outcome_progress` handles business alignment. `repo_map` handles orientation with automatically detected architectural subsystems, cohesion scores, and interfaces. `list_roots` handles workspace management plus live scan stats and recent OperationReport history. CLI and MCP share a service layer — every capability available in both interfaces.

**CGR** — mix of read + write + admin capabilities (file editing, database wipes, project deletion).

**CGC** — broad coverage including visualization, dead code detection, complexity analysis, file watching.

**GitNexus** — hybrid search over execution flows, caller/callee context, blast-radius impact, uncommitted-change detection, graph-aware rename, ad-hoc Cypher, and multi-repo discovery. Tools accept `task_context` and `goal` parameters for ranking — the same query returns different results depending on what the agent is working on.

**codeTree** — breadth-first structural analysis covering graph queries, skeleton views, clone detection, taint/dataflow analysis, dead code detection, doc suggestions, change impact, and repository map.

**Serena** — LSP-backed symbol navigation plus symbol-level code editing (`replace_symbol_body`, `rename_symbol`). Agents can not only read but write code at the symbol level.

RNA's MCP surface is deliberately consolidated. RNA is read/align infrastructure; agents have their own editors. codeTree takes the opposite approach — broader tool coverage for security, docs, and code quality, but without embeddings, LSP enrichment, or business context.

**RNA plugin skills** — setup, record, ADR sync, architecture diagrams, dead-code detection, and review-readiness. The post-v0.2.6 additions are intentionally agent-process skills rather than MCP surface expansion: `/rna-mcp:dead-code` requires completed LSP call/reference enrichment before global dead-code claims, while `/rna-mcp:review-readiness` starts from the diff and pulls graph context only when it changes review decisions.

## What RNA Does Better

1. **More accurate graph** — Tree-sitter extraction plus LSP enrichment for compiler-grade call/reference/type hierarchy and `Implements` edges. Neither CGR nor CGC has LSP enrichment. RNA's edges come from the same language servers your editor uses. Multiple language servers run concurrently (EventBus `LanguageDetected` events), so Python + TypeScript + Rust LSP enrichment all start in parallel rather than sequentially.
2. **Faster queries** — In-process, embedded index. No network hop, no Docker. Microsecond graph traversal, millisecond semantic search.
3. **Deeper semantic search** — Function bodies (not just names), all markdown (chunked by heading with hierarchy), and commits in one vector space. Results are relevance-ranked with test file demotion. CGR embeds function bodies but with raw scores. CGC doesn't embed at all.
4. **Semantic graph entry points** — `search(query="database pool", mode="impact")` works directly. No need to look up a `node_id` first. CGR and CGC require exact node identifiers.
5. **Cross-encoder reranking** — Jina Reranker v1 Turbo re-scores top candidates for precise NL query results. On by default for MCP (agents benefit from precision), opt-in for CLI (`--rerank`). None of the others have reranking.
6. **Token-efficient body retrieval** — `search(include_body: true, minify_body: true)` returns function bodies with comments stripped and locals shortened via tree-sitter AST walks (TS/JS, Rust, Python, Go) plus a deterministic legend. Agents get full implementation context with less raw source noise. No other tool minifies bodies for LLM consumption.
7. **Subsystem detection** — `repo_map` automatically clusters the codebase into architectural subsystems using Louvain community detection on actual call edges. No configuration required. Agents can scope search to a subsystem, filter cross-subsystem edges, and see the architecture on first call. CGR, CGC, and codeTree return flat symbol or file lists.
8. **Impact summaries that don't flood context** — `search(mode="impact")` on high-connectivity nodes auto-summarizes into a subsystem-grouped breakdown instead of returning raw node listings that overflow context.
9. **No analysis-gate** — RNA indexes incrementally on first use. GitNexus and CGC require an upfront analysis step before any query works. RNA's fast path (tree-sitter extraction) is visible to agents quickly; LSP and embedding readiness is exposed through capability reports instead of pretending all metadata is immediately complete.

## What RNA Does That Others Don't

1. **Outcome-to-code structural joins** — `outcome_progress` traces declared business outcomes through tagged commits to symbols. No other tool connects "why" to "what."
2. **ADR validation primitive** — ADRs can declare executable validation references (`cargo_tests`, audits, smoke fixtures, scripts), compile them into repo-native derived manifests, and fail validation when an implemented decision is no longer backed by passing checks.
3. **Cross-session learning** — `.oh/metis/` persists practical wisdom across agent sessions, searchable via the same embedding index.
4. **Staleness awareness** — `list_roots` reads live scan stats during active scans (not just file sentinels after completion): symbols extracted, languages in-flight for LSP, edge counts per language, scan phase. It also surfaces recent OperationReport history from `.oh/.cache/operation_reports.json`, so agents can see completed, degraded, failed, or stale operation state without scraping logs.
5. **Capability-specific readiness control plane** — ADR-003's enrichment job ledger and ADR-004's OperationReport model record capability states, degraded query classes, next-step commands, diagnostics, timings, and related enrichment job IDs. Dead-code and global impact workflows can fail closed when call/reference coverage is missing instead of trusting a lossy `ready: true` flag.
6. **Self-tuning performance** — Parallel tree-sitter extraction (rayon, all cores), parallel LSP enrichment (all language servers start concurrently via EventBus `LanguageDetected` events, pipelined transport with adaptive concurrency per server), lock-free double-buffered embedding index. `scan --full` is incremental when a cache exists — ~0.1s on no-change runs. Per-consumer content-addressed cache keys mean changing one consumer's logic only invalidates that consumer's results, not the entire cache. Dirty-slugs filtering skips LSP enrichment entirely for unchanged roots in multi-root workspaces.
7. **Zero external dependencies** — Single release artifact binary, no Docker, no DB server, no API key.
8. **Architecture-aware queries** — Subsystem detection (Louvain, phase-2 contraction) clusters symbols by actual coupling. Framework detection adds first-class `NodeKind::Other("framework")` nodes (e.g., `lancedb`, `tokio`, `fastapi`). Subsystem name, cohesion score, and interface list are available to agents on the first `repo_map` call — no tuning or config required.
9. **Config-driven topology extraction** — Drop `.oh/extractors/*.toml` in any repo to add custom `Produces`/`Consumes` edges for any message broker, event bus, or RPC pattern. Glob patterns, optional topic-arg (function name as channel when omitted). No Rust, no build, no RNA release required.

## What Others Do That RNA Doesn't

RNA is read-only infrastructure — it serves agents, it doesn't act as one. Things RNA deliberately doesn't do:

- **File editing / code generation** — CGR has tools for writing files and wiping databases. RNA doesn't touch your code; agents have their own editors.
- **Dedicated core dead-code tool / visualization** — CGC and codeTree both have dedicated tools for these. RNA exposes the graph and ships `/rna-mcp:dead-code` as an agent skill that checks LSP call/reference readiness before reporting candidates. It is not a core MCP tool, and visualization remains outside RNA's agent-first focus. (RNA does compute cyclomatic complexity per function, surfaced via `search` with `min_complexity` / `sort_by="complexity"`.)
- **Clone detection** — codeTree uses AST normalization to find structural duplicates. RNA doesn't do clone detection; code quality metrics are outside its alignment focus.
- **Taint / dataflow analysis** — codeTree traces source-to-sink data flows with sanitizer detection. RNA is alignment infrastructure, not a security scanner.
- **Doc suggestions** — codeTree identifies undocumented functions. RNA treats this as the agent's job, not the context server's.
- **Repository map** — codeTree generates a compact codebase overview with entry points, hotspot files, and a suggested exploration path. RNA's `repo_map` covers this ground and adds automatically detected architectural subsystems (via Louvain community detection), cohesion scores, and subsystem interfaces — `repo_map` provides a structural architecture view, not just a symbol list.
- **Code-specific embedding model** — CGR uses UniXcoder (trained on code). RNA uses MiniLM-L6-v2 (general-purpose text model) because it needs to embed code, markdown, and business artifacts in the same space. Trade-off: slightly less code-specific precision, much broader coverage.
- **SCIP indexing** — CGC supports SCIP-backed compiler precision. RNA spiked SCIP (#114) and concluded LSP provides the same semantic edges without requiring separate build-time indexers.
- **Symbol-aware code editing** — Serena has `replace_symbol_body` and `rename_symbol` tools that edit code at the symbol level, not just read it. RNA is read-only infrastructure; agents use their own editors for writes.
- **Execution flow detection** — GitNexus detects "processes": multi-hop call chains traced from entry points (functions with no internal callers) forward via BFS through CALLS edges, grouped and labeled heuristically ("HandleLogin → CreateSession"). This gives agents an intermediate abstraction between individual symbols and architectural subsystems. RNA has the graph data to compute this (entry-point detection, reachable traversal, Calls edges) but doesn't yet surface pre-computed flows as first-class objects. See the [GitNexus salvage](.oh/sessions/gitnexus-salvage.md) for the porting analysis.
- **Multi-file rename with confidence** — GitNexus's `rename` tool combines graph-derived references (high confidence) with regex text search (lower confidence) and tags each edit accordingly. RNA is read-only; rename is the agent's job using its own editor tools.
- **Web-based graph visualization** — GitNexus ships a React/Sigma.js UI for visual graph exploration and AI chat. RNA serves agents, not humans browsing a graph.

## A Note on Storage Architecture

RNA, GitNexus, and CGC all use embedded graph databases — no Docker required — but make different bets:

**RNA (LanceDB + petgraph):** Two purpose-built stores. LanceDB (Apache-licensed, DataBricks-backed, Arrow-native) is the persistent canonical source for vectors, FTS, and edges. petgraph is an in-memory throwaway index rebuilt from LanceDB on startup — pure Rust for microsecond BFS, Dijkstra, PageRank, Tarjan SCC. Incremental writes go to LanceDB first; petgraph is always derived. This split tolerates drift gracefully and puts graph algorithms in pure Rust memory with no query overhead.

**GitNexus (LadybugDB):** One store for everything — graph (Cypher), vectors (HNSW), and BM25 (FTS extension) — via LadybugDB, an embedded property graph database. Unified storage means cross-modality joins happen in one Cypher query. The cost: a single-writer bottleneck (132s for 5.67M edges in benchmarks), and vector index requires a full rebuild to update.

**CGC (KuzuDB/Neo4j/FalkorDB):** Plugin-selectable backends. Flexible, but no embeddings — semantic search isn't in the design.

## Summary

RNA does code graph queries better (more languages, LSP edges, in-process speed, no external deps) and then adds a layer the others don't have (business outcome alignment).

## Sources

- [Code-Graph-RAG salvage analysis](../.oh/sessions/cgr-salvage.md)
- [CodeGraphContext salvage analysis](../.oh/sessions/codegraphcontext-salvage.md)
- [codeTree salvage analysis](../.oh/sessions/codetree-salvage.md)
- [Serena salvage analysis](../.oh/sessions/serena-salvage.md)
- [GitNexus salvage analysis](../.oh/sessions/gitnexus-salvage.md)
