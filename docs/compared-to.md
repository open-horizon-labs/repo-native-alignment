# RNA Compared To Similar Tools

RNA builds a better code graph — more languages, compiler-grade edges from LSP, in-process microsecond queries — and then does something none of the others attempt: connects that graph to declared business outcomes.

## At a Glance

| | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** | **OpenDev** |
|---|---|---|---|---|
| **What it is** | Aim-conditioned MCP server | Code RAG system | Code graph toolkit + MCP | AI coding agent |
| **Layer** | Infrastructure (serves agents) | Infrastructure (serves agents) | Infrastructure (serves agents) | Agent harness (consumes tools) |
| **Language** | Rust | Python | Python | Python |
| **Install** | `cargo install` / binary | Docker + uv + Memgraph + API key | `pip install` + (KuzuDB\|Neo4j) | `pip install` + API key |
| **External deps** | None | Docker, Memgraph, LLM API | Graph DB (embedded or Docker) | LLM API (required) |
| **Languages parsed** | 22 | 11 | 14 | N/A (uses LSP per-query) |
| **Graph storage** | LanceDB + petgraph (embedded) | Memgraph (Docker) | KuzuDB/FalkorDB/Neo4j | None (ephemeral) |
| **Embeddings** | MiniLM-L6-v2 on Metal GPU (local) | UniXcoder (local) | None | None |
| **LSP integration** | 37 servers, batch enrichment | None | None | 37 servers, per-query |
| **MCP tools** | 7 | 10 | 17 | N/A (is an MCP client) |
| **Business context** | Outcomes, signals, guardrails, metis | None | None | None |
| **License** | MIT | MIT | MIT | MIT |

## Architecture Trade-offs

| Axis | RNA | CGR | CGC | OpenDev |
|------|-----|-----|-----|---------|
| **Cold start** | ~5-10s scan, ~2min embed | Index + Docker startup | Index + DB setup | Instant (no pre-processing) |
| **Warm restart** | <1s (LanceDB cache) | Memgraph persists | DB persists | N/A |
| **Memory** | In-process (petgraph + LanceDB) | Docker container | External or embedded DB | N/A |
| **Query latency** | ms (in-process) | Network hop to Memgraph | Network hop or embedded | Real-time LSP calls |
| **Offline capable** | Fully offline | Needs Docker | Depends on DB choice | Needs LLM API |

RNA's zero-dependency design is a deliberate architectural choice. `cargo install` → works. No Docker, no external DB, no API key. This matters for air-gapped environments, CI pipelines (the `test` subcommand runs 25 checks, exits 0/1), and laptops on planes.

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
| **What's embedded** | Function bodies, .oh/ artifacts, commits, markdown | Function bodies | Nothing |
| **Indexed together** | Code + business context + git history | Code only | N/A |
| **Score normalization** | 0-1 cosine, test files demoted | Raw similarity | N/A |

RNA's unique advantage: semantic search spans code AND business artifacts in the same vector space. "Find functions related to our payment reliability outcome" is a query only RNA can answer. CGR's UniXcoder is a code-specific model (better at pure code semantics), but RNA embeds function bodies, commit messages, markdown docs, and business artifacts together — breadth over specialization.

## MCP Tool Philosophy

**RNA: 7 focused tools** — "Do one thing per tool, document edge semantics, let the agent compose."

**CGR: 10 tools** — Mix of read + write + admin (file editing, database wipes, project deletion).

**CGC: 17 tools** — Broad coverage including visualization, dead code detection, complexity analysis, file watching.

RNA's tool count is deliberately lower. RNA is read/align infrastructure; agents have their own editors.

## What RNA Does Better

1. **More accurate graph** — 22-language tree-sitter extraction + 37 LSP servers for compiler-grade call/type hierarchy. Neither CGR nor CGC has LSP enrichment. RNA's edges come from the same language servers your editor uses.
2. **Faster queries** — In-process petgraph + LanceDB. No network hop, no Docker, no external DB. Microsecond graph traversal, millisecond semantic search.
3. **Broader semantic search** — Code, commits, markdown, and business artifacts in one vector space. CGR embeds code only. CGC doesn't embed at all.

## What RNA Does That Others Don't

1. **Outcome-to-code structural joins** — `outcome_progress` traces declared business outcomes through tagged commits to symbols. No other tool connects "why" to "what."
2. **Cross-session learning** — `.oh/metis/` persists practical wisdom across agent sessions, searchable via the same embedding index.
3. **Staleness awareness** — `LSP: pending`, `LSP: enriched (N edges)`, "Embedding index: building" — agents know when to trust results and when to retry.
4. **Self-tuning performance** — Adaptive batch sizing, background reindexing, lock-free double-buffered embedding index.
5. **Zero external dependencies** — Single binary, no Docker, no DB server, no API key.

## If You're Choosing

- **Need a coding agent?** → [OpenDev](https://github.com/opendev-to/opendev)
- **Need general-purpose code RAG?** → [Code-Graph-RAG](https://github.com/vitali87/code-graph-rag)
- **Need a code exploration toolkit with visualization?** → [CodeGraphContext](https://github.com/CodeGraphContext/CodeGraphContext)
- **Need agents that stay aligned to business outcomes across sessions?** → RNA

## Sources

This comparison was developed from hands-on analysis of each project's codebase and documentation:
- [OpenDev salvage analysis](.oh/sessions/opendev-paper-salvage.md)
- [Code-Graph-RAG salvage analysis](.oh/sessions/cgr-salvage.md)
- [CodeGraphContext salvage analysis](.oh/sessions/codegraphcontext-salvage.md)
