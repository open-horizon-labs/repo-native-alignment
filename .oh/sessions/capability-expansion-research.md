# Capability Expansion Research
**Date:** 2026-03-20

Three research tracks: (1) unused LSP capabilities, (2) new extractors/metadata sources, (3) graph+vector analysis.

---

## Track 1: Unused LSP Capabilities

### Priority 1 — `relatedTests` (rust-analyzer)
`TestedBy` edges from test functions to the production symbols they cover. **No other code-context tool does this as queryable graph edges.** Today `mode="tests_for"` approximates via call hierarchy; `rust-analyzer/relatedTests` gives it directly per symbol.
- Effort: 2 (per non-test Function node)
- Outcome: context-assembly (risk reasoning near changes), agent-alignment

### Priority 2 — `inlayHint`
Inferred types (return types, `let` bindings, closures) appended to embedding text. Dramatically improves semantic search: "functions returning `Result<Node>`" would actually work.
- Effort: 2 (per-file, metadata enrichment)
- Outcome: context-assembly

### Priority 3 — `parentModule` (rust-analyzer)
Module ancestry edges: `fn process_request` → `mod handler` → `mod server`. Files are in the graph; their position in the crate module tree is not.
- Effort: 1 (one request per file)
- Outcome: subsystem-detection (semantic boundary), context-assembly

### Priority 4 — `viewCrateGraph` (rust-analyzer)
Crate-level `DependsOn` edges. Single request, returns DOT. RNA has function-to-function edges; it does not have `rna-mcp → lancedb (external)`.
- Effort: 1 (one request per workspace)
- Outcome: subsystem-detection (coarsest natural subsystem boundary)

### Priority 5 — `documentSymbol`
`Contains`/`ContainedBy` edges between struct → method, module → fn. Increases intra-cluster edge density for Louvain.
- Effort: 2 (per-file)
- Outcome: subsystem-detection, context-assembly

### Defer
- `semanticTokens`: High novelty (`is_async`, `is_unsafe` as queryable metadata) but delta-encoded parsing is gnarly. Effort 3.
- `hover`: Doc comments via LSP are a lower-effort win from tree-sitter (see Track 2).
- `workspace/symbol`: Low novelty (gap-fill for re-exports only).

---

## Track 2: New Extractors / Metadata Sources

### Priority 1 — Doc comments (`doc_comment` metadata field)
rustdoc / JSDoc / docstrings extracted as node metadata, included in embedding text. Doc comments describe **intent**; code describes **implementation**. Embedding improves for all 22 languages at once.
- Effort: Small (sibling-node walk in `generic.rs`)
- Outcome: context-assembly

### Priority 2 — Git blame → `churn_count` + `last_author`
Per-node metadata from git blame. Enables: "which functions in outcome X have highest churn?" No new EdgeKind, pure metadata patch.
- Effort: Small (`repo.blame_file()` → line range → node metadata)
- Outcome: context-assembly, human-led-curation

### Priority 3 — GitHub Actions extractor
`.github/workflows/*.yml` → `ci_workflow`, `ci_job`, `ci_step` nodes. `needs:` edges between jobs. Connects CI delivery gates to the code subsystems they test.
- Effort: Small-Medium (specialize YamlExtractor on `.github/workflows/` path)
- Outcome: domain-context-compiler, context-assembly

### Priority 4 — Co-change coupling edges
`EdgeKind::CoChanges` between files that change together in ≥N% of commits. Reveals implicit coupling without graph edges — the most actionable undeclared dependency signal.
- Effort: Medium (reduction pass on existing `GitCommitInfo.changed_files`)
- Outcome: context-assembly, subsystem-detection

### Priority 5 — API endpoint → handler linking
Match `ApiEndpoint.path` nodes (from OpenAPI extractor) against route string `Const` nodes (from `string_literals.rs`). **This bridge already exists but has never been connected.** "Which function handles `POST /payments`?" becomes answerable.
- Effort: Medium (post-extraction enricher, no external deps)
- Outcome: domain-context-compiler, context-assembly

### Priority 6 — Git tags as release nodes
`NodeKind::Other("release")` from `git tag -l`. Edges: tag → commits in range. Enables "what shipped in v2.0 for outcome X?"
- Effort: Small
- Outcome: context-assembly

### Priority 7 — GraphQL schema extractor
`tree-sitter-graphql` crate exists. Types, queries, mutations, subscriptions as nodes.
- Effort: Small (one new extractor following existing pattern)
- Outcome: domain-context-compiler

### Priority 8 — TODO/FIXME nodes
Cross-language comment scan → `Other("todo")` nodes linked to enclosing symbol. "Technical debt in outcome X's files" query.
- Effort: Small
- Outcome: context-assembly, human-led-curation

### Defer
- Merge conflict history: weaker signal than co-change coupling, much higher effort
- GitHub REST API: violates offline-first principle
- External tool binaries (tokei, semgrep): optional opt-in enricher pattern, not blocking

---

## Track 3: Graph + Vector Analysis

### Priority 1 — Scalar pre-filtering in search
Add `only_if()` pre-filters to `search_with_mode()` for `file_path`, `kind`, `subsystem`. Currently filtering is post-fetch in Rust (3x over-fetch). Moving it into the LanceDB query cuts noise.
- Effort: Very small (query builder change)
- Outcome: context-assembly, subsystem-detection (#316 follow-on)

### Priority 2 — Shortest path / call chain
Dijkstra on `Calls` edge subgraph. Makes `impact()` actionable: "here's **how** X reaches Y, not just that it does."
- Effort: Low (~40 lines, Dijkstra is in petgraph)
- Outcome: context-assembly

### Priority 3 — Strongly connected components (Tarjan)
`petgraph::algo::tarjan_scc()` — one function call. SCCs with size > 1 are circular dependency rings. Complements Louvain (which gives soft communities) with hard coupling facts.
- Effort: Very low (~20 lines)
- Outcome: subsystem-detection, context-assembly

### Priority 4 — PageRank × diagnostics join
No new algorithm. Join PageRank importance scores with diagnostic nodes: "which high-importance symbols have active errors?" First-class triage query.
- Effort: Very low (in-memory join on existing data)
- Outcome: context-assembly, agent-alignment

### Priority 5 — Topological sort
`petgraph::algo::toposort()` on `DependsOn`/`Defines` subgraph. "In what order should I apply these changes?" Useful for dependency ordering in refactoring tasks.
- Effort: Low (~30 lines, requires SCC first to detect cycles)
- Outcome: context-assembly

### Priority 6 — FTS on `file_path` column
Add FTS index on `file_path` in LanceDB. Enables `search("src/embed.rs")` by path as a keyword query.
- Effort: Very low (one `create_index` call)
- Outcome: context-assembly

### Defer
- Betweenness centrality: Manual Brandes algorithm (~150 lines). PageRank covers most of the same use case.
- k-core decomposition: Additive to Louvain but not replacing it.
- IVF index: Premature optimization until >10K nodes is consistently slow.
- Multi-vector embeddings: Schema migration cost too high relative to gain.
- Max flow/min cut: Niche, high effort.

---

## Combined Top 10 (cross-track, no deprioritized items)

| # | What | Track | Effort | Key query unlocked |
|---|------|-------|--------|-------------------|
| 1 | Doc comments in embedding text | Extractor | Small | Semantic search over API intent, not just code |
| 2 | `relatedTests` edges | LSP | 2 | "What tests cover this function?" (direct, not approximated) |
| 3 | Scalar pre-filtering in search | Graph | Tiny | Subsystem-scoped search, file-scoped search |
| 4 | Git blame → churn metadata | Extractor | Small | "Highest-churn functions in outcome X?" |
| 5 | SCC / circular dependency | Graph | Tiny | "Which modules have circular deps?" |
| 6 | PageRank × diagnostics | Graph | Tiny | "Most important broken things?" |
| 7 | `parentModule` edges | LSP | 1 | Module-path-qualified search and subsystem boundaries |
| 8 | Shortest path / call chain | Graph | Low | "How does X reach Y?" |
| 9 | API endpoint→handler linking | Extractor | Medium | "Which function handles POST /payments?" |
| 10 | GitHub Actions extractor | Extractor | Small-Med | "What CI gates this delivery?" |

---

## Key Architectural Insights

**The biggest underexploited connection in RNA today:** `string_literals.rs` creates `"/api/payments"` Const nodes. `openapi.rs` creates `ApiEndpoint("/api/payments")` nodes. They're never linked. A post-extraction enricher connecting these requires zero external dependencies and produces endpoint→handler edges.

**Doc comments are higher-value than code for embedding.** A function named `process_payment` with doc `/// Charges the card and records the transaction` embeds dramatically better than the code alone. This affects all 22 languages with one change in `generic.rs`.

**`relatedTests` is RNA-unique.** No other graph-based code tool exposes test-coverage as queryable edges. This is a genuine differentiator, not a catch-up feature.

**The module hierarchy gap.** RNA knows functions exist in files. It does NOT know that `src/server/handler.rs` contains `crate::server::handler::process_request`. `parentModule` + `viewCrateGraph` fill this from above (crate) and from within (module ancestry). Together they complete the abstraction hierarchy: symbol → module → crate → workspace.
