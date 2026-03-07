# Solution Space: Extractor Stack (Issues #8, #9, #10)

**Updated:** 2026-03-07

## Problem

We need a pluggable extraction stack that processes source files into graph nodes + edges. Three extractors share this interface: tree-sitter (instant, multi-language), LSP (background semantic upgrade), and schema extraction (structured input). Today's code (`src/code/mod.rs`) is Rust-only, hardcoded tree-sitter, returns flat `Vec<CodeSymbol>` with no edges, no topology, no graph model. The gap is: no common interface, no edge model, no multi-language support, no LSP integration, no schema parsing.

**Key Constraint:** Extractors are pluggable (guardrail). Don't hardcode per file type. Tree-sitter must not require external processes. LSP must not block MCP server startup.

**Success:** An agent can query the graph for "what calls function X?", "what would break if I change this schema?", and get answers that span languages and extraction methods.

---

## Candidates Considered

| # | Option | Level | Approach | Trade-off |
|---|--------|-------|----------|-----------|
| A | Trait-per-extractor with shared graph model | Local Optimum | Each extractor implements `Extractor` trait, returns `ExtractionResult { nodes, edges }`. Registry dispatches by file extension. | Clean but rigid -- the trait signature must be right from the start or everything needs changing |
| B | Event-stream extractors (emit facts) | Reframe | Extractors don't return a result struct. They emit typed facts (`SymbolDefined`, `ImportFound`, `TopologyDetected`) into a fact sink. The graph is built from accumulated facts. | More flexible for merging/upgrading but more infrastructure |
| C | Two-phase extract + enrich | Reframe | Phase 1 extractors produce nodes. Phase 2 enrichers (including LSP) add edges to existing nodes. Separate traits for each phase. | Directly models the tree-sitter-then-LSP flow. Risk: two traits = more API surface |
| D | Language-centric plugins (one plugin per language, does everything) | Band-Aid | Each language has a single module that handles tree-sitter, LSP, and schema for that language. No shared trait. | Fast to build initially but violates pluggability guardrail, doesn't scale |

---

## Evaluation

### Option A: Trait-per-extractor with shared graph model

**Approach:** Define `trait Extractor { fn can_handle(&self, path: &Path) -> bool; fn extract(&self, path: &Path, content: &str) -> ExtractionResult; }` where `ExtractionResult` contains `Vec<Node>` and `Vec<Edge>`. A registry holds `Vec<Box<dyn Extractor>>` and dispatches.

- **Solves stated problem:** Yes -- common interface, pluggable, multi-extractor per file
- **Implementation cost:** Medium -- requires graph model design upfront but trait is simple
- **Maintenance burden:** Low -- adding a new extractor = implementing one trait
- **Second-order effects:** The hard question is merge semantics. When tree-sitter produces nodes and LSP later produces edges referencing those nodes, how are node identities matched? The trait says "return results" but doesn't address "update existing results." This works for independent extractors but not for the LSP-upgrade pattern.
- **Handles LSP upgrade?** Poorly. LSP doesn't produce a fresh `ExtractionResult` -- it enriches existing data. Forcing it through the same trait means either (a) LSP re-extracts everything (wasteful, defeats incremental upgrade) or (b) the trait signature gets stretched to handle both modes.

### Option B: Event-stream extractors (emit facts)

**Approach:** Extractors emit typed events: `SymbolDefined { id, name, kind, location }`, `EdgeDiscovered { from, to, kind }`, `TopologyDetected { pattern, location, confidence }`. A fact store accumulates them. The graph is a materialized view of accumulated facts.

- **Solves stated problem:** Yes -- and naturally handles the LSP-upgrade pattern (LSP emits `EdgeDiscovered` facts that reference existing `SymbolDefined` facts)
- **Implementation cost:** High -- requires fact store, event types, materialization logic
- **Maintenance burden:** Medium -- fact types grow, but each is independent
- **Second-order effects:** Naturally temporal (facts have sources and timestamps). Enables "what did tree-sitter see vs what did LSP resolve?" comparisons. But it's a lot of infrastructure for what is fundamentally three extractors. This is a system design for a platform, not for a focused tool.
- **Handles LSP upgrade?** Excellently. LSP emits new facts; store merges.

### Option C: Two-phase extract + enrich (RECOMMENDED)

**Approach:** Two traits:

```rust
trait Extractor {
    fn can_handle(&self, path: &Path) -> bool;
    fn extract(&self, path: &Path, content: &str) -> ExtractionResult;
}

trait Enricher {
    fn can_enrich(&self, lang: &Language) -> bool;
    async fn enrich(&self, graph: &mut Graph) -> EnrichmentResult;
}
```

Phase 1 (startup, synchronous): tree-sitter and schema extractors run on all files, producing nodes + edges. Phase 2 (background, async): LSP enricher spawns language servers, resolves cross-file references, adds edges to existing graph nodes.

`ExtractionResult` contains nodes and edges. `EnrichmentResult` contains only edges (and possibly node metadata patches). The graph owns node identity; enrichers reference nodes by stable IDs (file + name + kind).

- **Solves stated problem:** Yes -- clean separation of instant vs background work
- **Implementation cost:** Medium -- two small traits, shared graph model
- **Maintenance burden:** Low -- the two-phase model matches the actual data flow
- **Second-order effects:** Makes the non-blocking startup guarantee structural, not behavioral. Tree-sitter extractors are sync; LSP enrichers are async. The type system prevents an enricher from being registered as a startup extractor. Future enrichers (AI-based analysis, etc.) slot into Phase 2 naturally.
- **Handles LSP upgrade?** By design. That is literally what Phase 2 is for.

### Option D: Language-centric plugins

**Approach:** `mod rust { fn extract_ts(...); fn extract_lsp(...); fn extract_schema(...); }` per language.

- **Solves stated problem:** Partially -- works but hardcodes everything
- **Implementation cost:** Low initially
- **Maintenance burden:** High -- each language duplicates the extraction pipeline
- **Second-order effects:** Violates the pluggability guardrail directly. Adding Kotlin means copying the entire Rust module structure.
- **Handles LSP upgrade?** Per-language, no shared pattern.

---

## Recommendation

**Selected:** Option C -- Two-phase extract + enrich
**Level:** Reframe

### Why this one

1. **Models the actual data flow.** Tree-sitter is instant and file-local. LSP is slow and cross-file. These are fundamentally different operations, not variations of the same thing. Two traits reflect that truth instead of papering over it.

2. **Makes the non-blocking guarantee structural.** `Extractor` is sync, runs at startup. `Enricher` is async, runs in background. You cannot accidentally block startup with an LSP call because the type won't let you register it as an `Extractor`.

3. **Shared graph model is the real design decision.** Both traits operate on the same `Node` and `Edge` types. The graph is the integration point, not the trait. This means extractors and enrichers are loosely coupled -- they share data, not behavior.

4. **Right-sized for three extractors.** Option B (event stream) is infrastructure for a platform. We have three extractors and one enricher. Two traits and a shared graph model is proportional.

### Why not the others

- **Option A:** Forcing LSP through the same `Extractor` trait creates an impedance mismatch. LSP doesn't "extract from a file" -- it "resolves relationships across files." Different operation, deserves different interface.
- **Option B:** Over-engineered. We'd build a fact store to serve three consumers. The abstraction isn't earned yet. If we later have 10+ extractors emitting facts at different rates, upgrade then.
- **Option D:** Violates the pluggability guardrail. Dead on arrival.

### Accepted trade-offs

- Two traits instead of one. Slightly more API surface. Justified by modeling the real constraint (sync startup vs async background).
- Node identity must be stable across phases. We need a `NodeId` scheme that tree-sitter creates and LSP can reference. File path + symbol name + kind is the natural key but may collide (overloaded function names). We'll handle this when it surfaces, starting with the simple key.
- Schema extractors could be either Extractors or Enrichers (they produce both nodes and edges). Decision: they are Extractors because they run instantly on structured input. If schema cross-referencing (matching a .proto message to a serde struct) becomes important, that becomes an Enricher.

---

## Design Details

### The Graph Model

```rust
/// Stable identity for a node in the workspace graph
#[derive(Clone, Hash, Eq, PartialEq)]
struct NodeId {
    file: PathBuf,
    name: String,
    kind: NodeKind,
}

enum NodeKind {
    Function, Struct, Trait, Enum, Module, Import,
    ProtoMessage, ProtoService, SqlTable, ApiEndpoint,
    // Extensible via Other(String)
    Other(String),
}

struct Node {
    id: NodeId,
    language: Language,
    line_start: usize,
    line_end: usize,
    signature: String,
    body: String,
    metadata: BTreeMap<String, String>,  // extensible key-value
}

enum EdgeKind {
    Imports,          // use/import/require
    Calls,            // function call (LSP-resolved)
    Implements,       // trait/interface implementation
    TypeOf,           // type annotation
    FieldOf,          // struct field, proto field, SQL column
    Evolves,          // migration sequence
    TopologyBoundary, // subprocess, network, async boundary
}

struct Edge {
    from: NodeId,
    to: NodeId,
    kind: EdgeKind,
    source: ExtractionSource,  // which extractor produced this
    confidence: Confidence,     // Detected vs Confirmed
}

enum ExtractionSource { TreeSitter, Lsp, Schema }
enum Confidence { Detected, Confirmed }
```

The `source` and `confidence` fields on edges solve the "tree-sitter detects, LSP disambiguates" requirement. A tree-sitter topology detection creates an edge with `Detected` confidence. When LSP resolves the type, it upgrades the edge to `Confirmed` (or removes it if the detection was wrong).

### The Extractor Trait

```rust
struct ExtractionResult {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    diagnostics: Vec<Diagnostic>,  // warnings, parse errors
}

trait Extractor: Send + Sync {
    /// File extensions this extractor handles (e.g., ["rs"], ["proto"])
    fn extensions(&self) -> &[&str];

    /// More precise check (e.g., only .rs files with #[derive(Serialize)])
    fn can_handle(&self, path: &Path, content: &str) -> bool;

    /// Extract nodes and edges from a single file
    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult>;
}
```

Using `extensions()` for fast dispatch (the registry filters by extension first, then calls `can_handle()` for fine-grained checks). This avoids calling every extractor on every file.

### The Enricher Trait

```rust
struct EnrichmentResult {
    added_edges: Vec<Edge>,
    updated_nodes: Vec<(NodeId, BTreeMap<String, String>)>,  // metadata patches
    diagnostics: Vec<Diagnostic>,
}

#[async_trait]
trait Enricher: Send + Sync {
    /// Languages this enricher supports
    fn languages(&self) -> &[Language];

    /// Whether the enricher is ready (LSP server warmed up)
    fn is_ready(&self) -> bool;

    /// Enrich the graph with cross-file semantic information
    async fn enrich(&self, graph: &Graph) -> Result<EnrichmentResult>;
}
```

The `is_ready()` method lets the system check whether LSP has finished indexing before attempting enrichment. The MCP server serves tree-sitter results immediately; when `is_ready()` returns true, enrichment runs and upgrades the graph.

### The Registry

```rust
struct ExtractorRegistry {
    extractors: Vec<Box<dyn Extractor>>,
    enrichers: Vec<Box<dyn Enricher>>,
}

impl ExtractorRegistry {
    fn register_extractor(&mut self, e: Box<dyn Extractor>);
    fn register_enricher(&mut self, e: Box<dyn Enricher>);

    /// Run all matching extractors on a file, merge results
    fn extract_file(&self, path: &Path, content: &str) -> ExtractionResult;

    /// Run all ready enrichers, return combined enrichments
    async fn enrich(&self, graph: &Graph) -> Vec<EnrichmentResult>;
}
```

Multiple extractors can handle the same file. For example, a `.rs` file with `#[derive(Serialize)]` gets both the tree-sitter Rust extractor (symbols) and the schema extractor (serde struct shapes). Results are merged by the registry.

### Tree-sitter Specifics (#8)

**Grammar management:** Bundle grammars as Cargo dependencies. The tree-sitter ecosystem has crates for each language (`tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-typescript`, `tree-sitter-go`). These compile to native parsers at build time. No runtime download.

For the four required languages:

| Language | Crate | Node types to extract |
|----------|-------|-----------------------|
| Rust | `tree-sitter-rust` (already in Cargo.toml) | function_item, struct_item, trait_item, impl_item, enum_item, use_declaration, mod_item |
| Python | `tree-sitter-python` | function_definition, class_definition, import_statement, import_from_statement |
| TypeScript | `tree-sitter-typescript` | function_declaration, class_declaration, interface_declaration, import_statement, type_alias_declaration |
| Go | `tree-sitter-go` | function_declaration, method_declaration, type_declaration, import_declaration |

**Topology pattern detection:** Hardcoded patterns per language, registered as tree-sitter queries (S-expression patterns). Example for Rust:

```scheme
;; Detect subprocess spawn
(call_expression
  function: (field_expression
    value: (identifier) @recv
    field: (field_identifier) @method)
  (#eq? @recv "Command")
  (#eq? @method "new")) @topology.subprocess

;; Detect async spawn
(call_expression
  function: (scoped_identifier
    path: (identifier) @ns
    name: (identifier) @fn)
  (#eq? @ns "tokio")
  (#eq? @fn "spawn")) @topology.async_boundary
```

This uses tree-sitter's query language rather than manual AST walking. Queries are declarative, per-language, and could be user-extensible (load from `.oh/queries/` in the future).

**Performance:** Parse per-file, on demand. No batch mode needed at current scale. Tree-sitter is fast enough (microseconds per file) that parsing all files on startup is fine for repos under 10K files. For incremental updates, git diff drives the file list -- only changed files get re-parsed.

**Migration from current code:** The existing `src/code/mod.rs` becomes the internals of `RustTreeSitterExtractor`. The `extract_symbols()` function maps to `Extractor::extract()`. The `CodeSymbol` type maps to `Node`. The `SymbolKind` enum merges into `NodeKind`. Current `search_code` MCP tool queries the graph instead of re-parsing every call.

### LSP Specifics (#9)

**Client implementation:** Use `tower-lsp` (client mode) + `lsp-types` for protocol types. These are the standard Rust crates for LSP. Communication via JSON-RPC over stdio (spawn language server as child process, pipe stdin/stdout).

**Server lifecycle:**

1. On MCP server startup: do nothing. Serve tree-sitter results.
2. On first query for a language: spawn the language server in background.
3. Track readiness via `Enricher::is_ready()`. LSP servers signal readiness via `initialized` notification + completion of initial indexing (rust-analyzer emits progress notifications).
4. When ready: run `enrich()`, which calls `textDocument/documentSymbol`, `textDocument/references`, `textDocument/implementation` for each file.
5. Keep server alive for the MCP session. Kill on MCP server exit.

**What to extract:** References (call graph), implementations (trait graph), type resolution (for topology disambiguation). Not diagnostics or completions -- those are IDE features, not graph features.

**Cost model:** rust-analyzer takes 10-30 seconds on medium projects. This is why it is an Enricher, not an Extractor. The agent gets tree-sitter data in <1 second. LSP data arrives when it arrives. The MCP tools should indicate enrichment status: "Results from tree-sitter. LSP enrichment: pending/ready."

**Which servers to spawn:**

| Language | Server | Install | Notes |
|----------|--------|---------|-------|
| Rust | rust-analyzer | cargo component | Heavy, slow startup |
| Python | pyright | npm install | Fast |
| TypeScript | typescript-language-server | npm install | Fast |
| Go | gopls | go install | Fast |

Servers are discovered via PATH. If not found, that language gets tree-sitter only with a diagnostic message ("rust-analyzer not found; serving tree-sitter data only").

### Schema Specifics (#10)

**Implementation as Extractors (not Enrichers):**

| Format | Crate | Extractor |
|--------|-------|-----------|
| .proto | `protobuf-parse` or tree-sitter-protobuf | `ProtoExtractor` -- messages become nodes, fields become FieldOf edges, services become nodes with RPC method edges |
| SQL migrations | `sqlparser-rs` | `SqlExtractor` -- tables become nodes, columns become FieldOf edges, foreign keys become edges, migration files ordered by name/timestamp for Evolves edges |
| OpenAPI / JSON Schema | `serde_yaml` + `serde_json` | `OpenApiExtractor` -- endpoints become nodes, request/response schemas become nodes with edges |
| Serde structs | tree-sitter (detect `#[derive(Serialize)]`) | Handled by tree-sitter Rust extractor with `can_handle` checking for serde derives. Schema shape inferred from struct fields. |
| TS interfaces | tree-sitter-typescript | Handled by tree-sitter TS extractor. Interface declarations naturally become nodes. |

**Temporal schema evolution:** SQL migrations are ordered by filename convention (e.g., `001_create_users.sql`, `002_add_email.sql`). Each migration produces nodes with `Evolves` edges pointing from the previous migration's version of a table to the current one. This gives "what did the users table look like in migration 3?" for free.

**Cross-format duplicate detection:** This is an Enricher concern, not an Extractor concern. A future `SchemaCrossRefEnricher` could match `.proto` message names against serde struct names against SQL table names and create `SameAs` edges. Not in scope for initial implementation -- it requires heuristics (name similarity, field overlap) that need tuning.

---

## Implementation Sequence

Given the "validate before building" guardrail, the sequence prioritizes getting something useful fast:

### Step 1: Graph model + Extractor trait + Registry (foundation)
- Define `Node`, `Edge`, `NodeId`, `NodeKind`, `EdgeKind` in `src/types.rs`
- Define `Extractor` trait and `ExtractorRegistry` in new `src/extract/mod.rs`
- Migrate existing `src/code/mod.rs` into `RustTreeSitterExtractor`
- `search_code` MCP tool queries graph instead of re-parsing

### Step 2: Multi-language tree-sitter (#8 core)
- Add `tree-sitter-python`, `tree-sitter-typescript`, `tree-sitter-go` to Cargo.toml
- Implement `PythonTreeSitterExtractor`, `TypeScriptTreeSitterExtractor`, `GoTreeSitterExtractor`
- Each follows the same pattern: language-specific node-type-to-NodeKind mapping
- Import graph edges: `use`/`import`/`require` statements become `Imports` edges
- Language detection by file extension in the registry

### Step 3: Topology detection (#8 extension)
- Add tree-sitter query patterns for topology idioms per language
- Produce `TopologyBoundary` edges with `Detected` confidence
- MCP tool: expose topology in `search_code` results

### Step 4: Schema extractors (#10)
- `ProtoExtractor` using protobuf parsing
- `SqlExtractor` using `sqlparser-rs`
- `OpenApiExtractor` using serde_yaml
- Each produces domain-specific node kinds and edges

### Step 5: Enricher trait + LSP integration (#9)
- Define `Enricher` trait in `src/extract/mod.rs`
- Implement `LspEnricher` using `tower-lsp` client + `lsp-types`
- Background spawning with readiness tracking
- Cross-file reference resolution produces `Calls` and `Implements` edges
- Topology disambiguation upgrades `Detected` edges to `Confirmed`

### Step 6: LanceDB storage
- Store graph nodes and edges in LanceDB
- Embeddings on node bodies for semantic code search
- Incremental updates driven by git diff

---

## Local Maximum Check

- **Did I defend my first idea?** No. I initially assumed Option A (single trait) would be sufficient. The LSP-upgrade pattern forced the two-phase split. I explored Option B (event stream) seriously and rejected it on proportionality grounds, not on principle.
- **Is there a higher-level approach I dismissed too quickly?** Option B (event stream/fact store) is genuinely more powerful and would handle future scenarios (AI enrichers, distributed extraction). But "validate before building" says don't build platform infrastructure before proving the core value. If we later need a fact store, the graph model is compatible with that evolution.
- **Am I optimizing the wrong thing?** The "validate before building" guardrail says we should measure whether agents behave differently with tree-sitter data before building LSP integration. The architecture should support LSP but we should not implement it until tree-sitter value is proven. The implementation sequence reflects this: LSP is Step 5, after tree-sitter and schema extractors.
