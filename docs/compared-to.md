# RNA Compared To Similar Tools

RNA builds a better code graph — more languages, compiler-grade edges from LSP, in-process microsecond queries — and then does something none of the others attempt: connects that graph to declared business outcomes.

## At a Glance

| | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** |
|---|---|---|---|
| **Install** | `cargo install` / binary | Docker + uv + Memgraph + API key | `pip install` + (KuzuDB\|Neo4j) |
| **External deps** | None | Docker, Memgraph, LLM API | Graph DB (embedded or Docker) |
| **Languages parsed** | 22 | 11 | 14 |
| **Graph storage** | LanceDB + petgraph (embedded) | Memgraph (Docker) | KuzuDB/FalkorDB/Neo4j |
| **Embeddings** | MiniLM-L6-v2 on Metal GPU (local) | UniXcoder (local) | None |
| **LSP integration** | 37 servers, batch enrichment | None | None |
| **MCP tools** | 5 | 10 | 17 |
| **Business context** | Outcomes, signals, guardrails, metis | None | None |

## Architecture Trade-offs

| Axis | RNA | CGR | CGC |
|------|-----|-----|-----|
| **Cold start** | ~5-10s scan, ~2min embed | Index + Docker startup | Index + DB setup |
| **Warm restart** | <1s (LanceDB cache) | Memgraph persists | DB persists |
| **Memory** | In-process (petgraph + LanceDB) | Docker container | External or embedded DB |
| **Query latency** | ms (in-process) | Network hop to Memgraph | Network hop or embedded |
| **Offline capable** | Fully offline | Needs Docker | Depends on DB choice |

RNA's zero-dependency design is a deliberate architectural choice. `cargo install` → works. No Docker, no external DB, no API key.

## Graph Quality

| Edge source | RNA | CGR | CGC |
|-------------|-----|-----|-----|
| Tree-sitter (syntactic) | 22 languages | 11 languages | 14 languages |
| LSP (semantic) | 37 language servers, call + type hierarchy | None | None |
| SCIP (compiler) | Planned | None | Pyright, tsc, scip-go, scip-rust |
| Embedding similarity | MiniLM-L6-v2, cosine distance | UniXcoder | None |

RNA's two-tier approach (tree-sitter + LSP) gives the broadest coverage with the highest accuracy. 22 languages from tree-sitter, then 37 LSP servers add compiler-grade call hierarchies and type relationships that neither CGR nor CGC have. CGC's SCIP indexing gives deep accuracy for 4 languages, but only 4 — RNA covers the other 18 with LSP and plans to add SCIP as a third tier.

## Semantic Search

| | RNA | CGR | CGC |
|---|---|---|---|
| **Model** | MiniLM-L6-v2 (384-dim) | UniXcoder (768-dim, code-specific) | None |
| **Hardware** | Metal GPU on Apple Silicon, CPU fallback | CPU | N/A |
| **What's embedded** | Function bodies, all markdown, commits | Function bodies | Nothing |
| **Indexed together** | Code + markdown + git history | Code only | N/A |
| **Score normalization** | 0-1 cosine, 5-tier ranking, test files demoted | Raw similarity | N/A |
| **Markdown** | Heading-scoped chunks with hierarchy | None | None |

RNA's unique advantage: semantic search spans code AND business artifacts in the same vector space. "Find functions related to our payment reliability outcome" is a query only RNA can answer. Results are ranked 0-1 with a 5-tier system: exact name > contains > signature-only, definitions before imports, production code before tests. CGR's UniXcoder is a code-specific model (better at pure code semantics), but RNA embeds function bodies, all markdown (chunked by heading), and commit messages together — breadth over specialization.

## MCP Tool Philosophy

**RNA: 5 focused tools** — do one thing per tool, document edge semantics, let the agent compose.

**CGR: 10 tools** — mix of read + write + admin (file editing, database wipes, project deletion).

**CGC: 17 tools** — broad coverage including visualization, dead code detection, complexity analysis, file watching.

RNA's tool count is deliberately lower. RNA is read/align infrastructure; agents have their own editors.

## What RNA Does Better

1. **More accurate graph** — 22-language tree-sitter extraction + 37 LSP servers for compiler-grade call/type hierarchy and `Implements` edges. Neither CGR nor CGC has LSP enrichment. RNA's edges come from the same language servers your editor uses.
2. **Faster queries** — In-process petgraph + LanceDB. No network hop, no Docker, no external DB. Microsecond graph traversal, millisecond semantic search.
3. **Deeper semantic search** — Function bodies (not just names), all markdown (chunked by heading with hierarchy), and commits in one vector space. Results ranked 0-1 with test file demotion. CGR embeds function bodies but with raw scores. CGC doesn't embed at all.
4. **Semantic graph entry points** — `graph_query(query="database pool", mode="impact")` works directly. No need to look up a `node_id` first. CGR and CGC require exact node identifiers.

## What RNA Does That Others Don't

1. **Outcome-to-code structural joins** — `outcome_progress` traces declared business outcomes through tagged commits to symbols. No other tool connects "why" to "what."
2. **Cross-session learning** — `.oh/metis/` persists practical wisdom across agent sessions, searchable via the same embedding index.
3. **Staleness awareness** — `LSP: pending`, `LSP: enriched (N edges)`, "Embedding index: building" — agents know when to trust results and when to retry.
4. **Self-tuning performance** — Adaptive batch sizing, background reindexing, lock-free double-buffered embedding index.
5. **Zero external dependencies** — Single binary, no Docker, no DB server, no API key.

## What Others Do That RNA Doesn't

RNA is read-only infrastructure — it serves agents, it doesn't act as one. Things RNA deliberately doesn't do:

- **File editing / code generation** — CGR has tools for writing files and wiping databases. RNA doesn't touch your code; agents have their own editors.
- **Dead code detection, complexity analysis, visualization** — CGC has 17 tools covering these. RNA exposes the graph and lets agents reason about it themselves.
- **Code-specific embedding model** — CGR uses UniXcoder (768-dim, trained on code). RNA uses MiniLM-L6-v2 (384-dim, general-purpose) because it needs to embed code, markdown, and business artifacts in the same space. Trade-off: slightly less code-specific precision, much broader coverage.
- **SCIP indexing** — CGC supports Pyright, tsc, scip-go, scip-rust for compiler-grade precision in 4 languages. RNA plans to add SCIP as a third tier alongside tree-sitter and LSP.

## Summary

RNA does code graph queries better (more languages, LSP edges, in-process speed, no external deps) and then adds a layer the others don't have (business outcome alignment).

## Sources

This comparison was developed from hands-on analysis of each project's codebase and documentation:
- [Code-Graph-RAG salvage analysis](../.oh/sessions/cgr-salvage.md)
- [CodeGraphContext salvage analysis](../.oh/sessions/codegraphcontext-salvage.md)
