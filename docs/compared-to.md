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

RNA uses LSP internally as one enrichment source (call hierarchy, type hierarchy, implements edges), fuses the results with tree-sitter, embeddings, git, and business artifacts, and exposes it all through multi-hop graph queries. For agents, RNA replaces the need for separate LSP plugins.

## At a Glance

| | **LSP (baseline)** | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** | **codeTree** |
|---|---|---|---|---|---|
| **Install** | Editor plugin or PATH binary | `cargo install` / binary | Docker + uv + Memgraph + API key | `pip install` + (KuzuDB\|Neo4j) | `pip install mcp-server-codetree` |
| **External deps** | One server per language | None | Docker, Memgraph, LLM API | Graph DB (embedded or Docker) | None |
| **Languages parsed** | 1 per server | 22 | 11 | 14 | 10 |
| **Graph storage** | In-memory per session | LanceDB + petgraph (embedded) | Memgraph (Docker) | KuzuDB/FalkorDB/Neo4j | SQLite (embedded) |
| **Embeddings** | None | MiniLM-L6-v2 on Metal GPU (local) | UniXcoder (local) | None | None |
| **LSP integration** | Is LSP | 37 servers, batch enrichment | None | None | None |
| **Query model** | Single-symbol, single-hop | Multi-hop, cross-language | Multi-hop | Multi-hop | Multi-hop |
| **MCP tools** | N/A (protocol, not MCP) | 4 | 10 | 17 | 23 |
| **CLI parity** | N/A | Full (shared service layer) | N/A | N/A | N/A |
| **Cross-encoder reranking** | None | Jina Reranker v1 Turbo (opt-in) | None | None | None |
| **Business context** | None | Outcomes, signals, guardrails, metis | None | None | None |

## Architecture Trade-offs

| Axis | LSP | RNA | CGR | CGC | CT |
|------|-----|-----|-----|-----|-----|
| **Cold start** | Server init (seconds) | ~5-10s scan, ~2min embed | Index + Docker startup | Index + DB setup | ~1s scan + SQLite index |
| **Warm restart** | Server re-init | <1s (LanceDB cache) | Memgraph persists | DB persists | SQLite persists (mtime invalidation) |
| **Memory** | Per-server process | In-process (petgraph + LanceDB) | Docker container | External or embedded DB | In-process (SQLite) |
| **Query latency** | ms per hop (N round-trips) | ms total (in-process, single call) | Network hop to Memgraph | Network hop or embedded | ms (embedded SQLite) |
| **Offline capable** | Yes | Fully offline | Needs Docker | Depends on DB choice | Fully offline |

RNA's zero-dependency design is a deliberate architectural choice. `cargo install` → works. No Docker, no external DB, no API key.

## Graph Quality

| Edge source | LSP | RNA | CGR | CGC | CT |
|-------------|-----|-----|-----|-----|-----|
| Tree-sitter (syntactic) | None | 22 languages | 11 languages | 14 languages | 10 languages |
| LSP (semantic) | Is the source (1 lang/server) | 37 language servers, call + type hierarchy | None | None | None |
| SCIP (compiler) | None | Not needed — LSP covers the same edges | None | Pyright, tsc, scip-go, scip-rust | None |
| Embedding similarity | None | MiniLM-L6-v2, cosine distance | UniXcoder | None | None |
| Cross-language | No (one server per language) | Yes (unified graph) | Yes | Yes | Yes |
| Multi-hop | No (agent must loop) | Yes (single call) | Yes | Yes | Yes |

LSP provides the raw semantic data — call hierarchy, type hierarchy, references — but only for one language at a time, one hop at a time. RNA consumes LSP as one enrichment source among several, fuses the results into a cross-language graph, and exposes multi-hop traversal. CGR and CGC skip LSP entirely, relying on tree-sitter (syntactic) or SCIP (compiler) for edges. RNA's two-tier approach (tree-sitter + LSP) gives the broadest coverage with the highest accuracy. 22 languages from tree-sitter, then 37 LSP servers add compiler-grade call hierarchies and type relationships that neither CGR nor CGC have.

> **Why not SCIP?** RNA spiked SCIP as a third enrichment tier ([#114](https://github.com/open-horizon-labs/repo-native-alignment/pull/114)). SCIP and LSP produce the same semantic edges — call hierarchy, type hierarchy, implements — because SCIP indexers (rust-analyzer, scip-python, scip-typescript, scip-go) are often the same tools as LSP servers, run in batch mode instead of live. The difference: SCIP requires installing a separate indexer per language, running a batch index step that produces a protobuf file, and parsing that file. LSP servers are already on most developers' PATH (your editor uses them), start on demand, and return the same edges through a standard protocol. SCIP adds a build step and a dependency for no additional edge coverage. RNA salvaged the reusable patterns from the spike (process timeout, file-line index, edge deduplication — [#122](https://github.com/open-horizon-labs/repo-native-alignment/pull/122)) and closed the SCIP enricher as unnecessary. CGC's use of SCIP makes sense if you don't have LSP integration — but RNA does.

## Semantic Search

| | LSP | RNA | CGR | CGC | CT |
|---|---|---|---|---|---|
| **Model** | N/A | MiniLM-L6-v2 (384-dim) + Jina Reranker v1 Turbo | UniXcoder (768-dim, code-specific) | None | None |
| **Hardware** | N/A | Metal GPU on Apple Silicon, CPU fallback | CPU | N/A | N/A |
| **What's embedded** | N/A | Function bodies, all markdown, commits | Function bodies | Nothing | Nothing |
| **Indexed together** | N/A | Code + markdown + git history | Code only | N/A | N/A |
| **Score normalization** | N/A | 0-1 cosine, 5-tier ranking, test files demoted | Raw similarity | N/A | N/A |
| **Markdown** | N/A | Heading-scoped chunks with hierarchy | None | None | None |

RNA's unique advantage: semantic search spans code AND business artifacts in the same vector space. "Find functions related to our payment reliability outcome" is a query only RNA can answer. Results are ranked 0-1 with a 5-tier system: exact name > contains > signature-only, definitions before imports, production code before tests. CGR's UniXcoder is a code-specific model (better at pure code semantics), but RNA embeds function bodies, all markdown (chunked by heading), and commit messages together — breadth over specialization.

## MCP Tool Philosophy

**RNA: 4 tools** — one `search` tool handles code symbols, artifacts, markdown, commits, and graph traversal. `outcome_progress` for business alignment. `repo_map` for orientation. `list_roots` for workspace management. CLI and MCP share a service layer — every capability available in both interfaces.

**CGR: 10 tools** — mix of read + write + admin (file editing, database wipes, project deletion).

**CGC: 17 tools** — broad coverage including visualization, dead code detection, complexity analysis, file watching.

**codeTree: 23 tools** — breadth-first structural analysis covering graph queries, skeleton views, clone detection, taint/dataflow analysis, dead code detection, doc suggestions, change impact, and repository map.

RNA's tool count is deliberately lower. RNA is read/align infrastructure; agents have their own editors. codeTree takes the opposite approach — more tools covering more use cases (security, docs, code quality) but without embeddings, LSP enrichment, or business context.

## What RNA Does Better

1. **More accurate graph** — 22-language tree-sitter extraction + 37 LSP servers for compiler-grade call/type hierarchy and `Implements` edges. Neither CGR nor CGC has LSP enrichment. RNA's edges come from the same language servers your editor uses.
2. **Faster queries** — In-process petgraph + LanceDB. No network hop, no Docker, no external DB. Microsecond graph traversal, millisecond semantic search.
3. **Deeper semantic search** — Function bodies (not just names), all markdown (chunked by heading with hierarchy), and commits in one vector space. Results ranked 0-1 with test file demotion. CGR embeds function bodies but with raw scores. CGC doesn't embed at all.
4. **Semantic graph entry points** — `search(query="database pool", mode="impact")` works directly. No need to look up a `node_id` first. CGR and CGC require exact node identifiers.
5. **Cross-encoder reranking** — `search(rerank: true)` re-scores top candidates with a Jina cross-encoder for precise NL query results. None of the others have reranking.

## What RNA Does That Others Don't

1. **Outcome-to-code structural joins** — `outcome_progress` traces declared business outcomes through tagged commits to symbols. No other tool connects "why" to "what."
2. **Cross-session learning** — `.oh/metis/` persists practical wisdom across agent sessions, searchable via the same embedding index.
3. **Staleness awareness** — `LSP: pending`, `LSP: enriched (N edges)`, "Embedding index: building" — agents know when to trust results and when to retry.
4. **Self-tuning performance** — Adaptive batch sizing, background reindexing, lock-free double-buffered embedding index.
5. **Zero external dependencies** — Single binary, no Docker, no DB server, no API key.

## What Others Do That RNA Doesn't

RNA is read-only infrastructure — it serves agents, it doesn't act as one. Things RNA deliberately doesn't do:

- **File editing / code generation** — CGR has tools for writing files and wiping databases. RNA doesn't touch your code; agents have their own editors.
- **Dead code detection, visualization** — CGC and codeTree both have dedicated tools for these. RNA exposes the graph and lets agents reason about it themselves. (RNA does compute cyclomatic complexity per function, surfaced via `search` with `min_complexity` / `sort_by="complexity"`.)
- **Clone detection** — codeTree uses AST normalization to find structural duplicates. RNA doesn't do clone detection; code quality metrics are outside its alignment focus.
- **Taint / dataflow analysis** — codeTree traces source-to-sink data flows with sanitizer detection. RNA is alignment infrastructure, not a security scanner.
- **Doc suggestions** — codeTree identifies undocumented functions. RNA treats this as the agent's job, not the context server's.
- **Repository map** — codeTree generates a compact codebase overview with entry points, hotspot files, and a suggested exploration path. RNA now has `repo_map` which provides top symbols by PageRank importance, hotspot files, active outcomes, and entry points.
- **Code-specific embedding model** — CGR uses UniXcoder (768-dim, trained on code). RNA uses MiniLM-L6-v2 (384-dim, general-purpose) because it needs to embed code, markdown, and business artifacts in the same space. Trade-off: slightly less code-specific precision, much broader coverage.
- **SCIP indexing** — CGC supports Pyright, tsc, scip-go, scip-rust for compiler-grade precision in 4 languages. RNA spiked SCIP (#114) and concluded LSP provides the same semantic edges without requiring separate build-time indexers.

## Summary

RNA does code graph queries better (more languages, LSP edges, in-process speed, no external deps) and then adds a layer the others don't have (business outcome alignment).

## Sources

Sources:
- [Code-Graph-RAG salvage analysis](../.oh/sessions/cgr-salvage.md)
- [CodeGraphContext salvage analysis](../.oh/sessions/codegraphcontext-salvage.md)
- [codeTree salvage analysis](../.oh/sessions/codetree-salvage.md)
