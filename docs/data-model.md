# RNA Data Model

This document describes the actual data model in RNA as of schema version 18. It covers the LanceDB column store, the in-memory graph structure, and how data flows from extraction through to MCP tool rendering.

Authoritative sources: `src/graph/mod.rs`, `src/graph/store.rs`, `src/server/store.rs`, `src/embed.rs`, `src/server/state.rs`, `src/graph/index.rs`, `src/extract/event_bus.rs`, `src/extract/consumers.rs`, `src/extract/scan_stats.rs`, `src/extract/cache.rs`.

**Keeping this doc in sync:** When you bump `SCHEMA_VERSION` in `src/graph/store.rs`, update the tables in section 1 of this file to match. The schema version number appears in the first paragraph of section 1.

---

## 1. LanceDB Column Store

RNA persists data in LanceDB at `.oh/.cache/lance/` relative to the repository root. The current schema version is tracked in `.oh/.cache/lance/schema_version`. When this file does not match `SCHEMA_VERSION` (currently `18`), the graph tables (`symbols`, `edges`, `pr_merges`, `file_index`) are dropped and rebuilt from scratch. The `artifacts` embedding table is managed separately and is not covered by `SCHEMA_VERSION` — it has its own schema validation at startup (see `artifacts` table section below).

A separate `.oh/.cache/lance/extraction_version` file tracks the global extraction logic version (`EXTRACTION_VERSION`, currently `14`). This integer is **deprecated** as of v0.1.15 (#526) — per-consumer content-addressed cache keys have replaced the single global sentinel (see [Section 6](#6-content-addressed-consumer-cache)). The file is still read for backward-compatible sentinel detection on cold start, but new consumer invalidation is driven by `ExtractionConsumer::version()` return values, not this file.

### `symbols` table

Stores code symbols (functions, structs, traits, enums, etc.) and other node types (markdown sections, diagnostics, API endpoints). This is the primary table for symbol search and graph traversal.

| Column | Type | Nullable | Description |
|--------|------|----------|-------------|
| `id` | UTF8 | no | Stable ID: `root:file:name:kind` |
| `root_id` | UTF8 | no | Repository root slug |
| `file_path` | UTF8 | no | Path relative to repo root |
| `name` | UTF8 | no | Symbol name |
| `kind` | UTF8 | no | Node kind string (see NodeKind) |
| `line_start` | UInt32 | no | Starting line (1-indexed) |
| `line_end` | UInt32 | no | Ending line (inclusive) |
| `signature` | UTF8 | no | Declaration/signature line(s) |
| `body` | UTF8 | no | Full body text of the node |
| `meta_virtual` | Boolean | yes | `true` for external virtual nodes produced by LSP enrichment (e.g., `tokio::spawn`) |
| `meta_package` | UTF8 | yes | Package/crate name for virtual nodes |
| `meta_name_col` | Int32 | yes | LSP cursor column for go-to-definition disambiguation |
| `value` | UTF8 | yes | Constant value text for `Const` nodes |
| `synthetic` | Boolean | yes | `true` for inferred constants (e.g., YAML scalar key-values) |
| `cyclomatic` | Int32 | yes | Cyclomatic complexity score (functions only) |
| `importance` | Float64 | yes | PageRank importance score (0.0–1.0), weighted by edge type |
| `storage` | UTF8 | yes | `"static"` (Rust) or `"var"` (Go) |
| `mutable` | Boolean | yes | `true` for `static mut` declarations |
| `decorators` | UTF8 | yes | Comma-separated decorator/attribute text |
| `type_params` | UTF8 | yes | Generic type parameters (e.g., `<T: Clone + Send>`) |
| `pattern_hint` | UTF8 | yes | Design pattern from naming conventions (e.g., `"factory"`, `"observer"`) |
| `is_static` | Boolean | yes | `true` for static/associated methods; `false` for instance methods; `null` for top-level functions |
| `is_async` | Boolean | yes | `true` for async functions |
| `is_test` | Boolean | yes | `true` for test functions |
| `visibility` | UTF8 | yes | `"pub"` for public re-exports |
| `exported` | Boolean | yes | `true` for Python `__all__` exports |
| `diagnostic_severity` | UTF8 | yes | `"error"` or `"warning"` — only for `NodeKind::Other("diagnostic")` nodes |
| `diagnostic_source` | UTF8 | yes | LSP server name (e.g., `"rust-analyzer"`) |
| `diagnostic_message` | UTF8 | yes | Full diagnostic message text |
| `diagnostic_range` | UTF8 | yes | `"line:col-end_line:end_col"` |
| `diagnostic_timestamp` | UTF8 | yes | Unix timestamp (seconds) when diagnostic was captured |
| `http_method` | UTF8 | yes | HTTP verb — only for `NodeKind::ApiEndpoint` nodes (e.g., `"GET"`, `"POST"`) |
| `http_path` | UTF8 | yes | HTTP path pattern — only for `ApiEndpoint` nodes (e.g., `"/users/{id}"`) |
| `vector` | FixedSizeList(Float32, dim) | yes | Semantic embedding vector. Dimension depends on the model (384 for MiniLM-L6-v2). Present only after embeddings are computed; absent in the base schema. Added by `symbols_schema_with_vector(dim)`. |
| `updated_at` | Int64 | no | Unix timestamp (seconds) of last write |

**FTS index:** An FTS (BM25) index is created on `name` after each full persist. This enables keyword search over symbol names.

**Note on language:** The `language` field is not stored in this table. It is inferred at load time from the file extension via `infer_language_from_path`.

**Note on source:** The extraction source (`tree_sitter`, `lsp`, etc.) is not stored in this table. Loaded nodes default to `ExtractionSource::TreeSitter`.

**Note on subsystem:** The `subsystem` metadata value (e.g., `"scanner"`, `"server"`) is not a column in this table. It lives in `Node.metadata["subsystem"]` and is re-computed after each graph rebuild. Subsystem-based filtering is only available through the `artifacts` table (via the `search()` MCP tool's `subsystem` parameter). Do not expect a `subsystem` column when querying `symbols` directly.

### `edges` table

Stores directed relationships between nodes. This is the source of truth for the petgraph in-memory index, which is rebuilt from this table at startup.

| Column | Type | Nullable | Description |
|--------|------|----------|-------------|
| `id` | UTF8 | no | Stable edge ID: `from_stable_id->kind->to_stable_id` |
| `source_id` | UTF8 | no | Stable ID of the source (from) node |
| `source_type` | UTF8 | no | NodeKind string of the source node |
| `target_id` | UTF8 | no | Stable ID of the target (to) node |
| `target_type` | UTF8 | no | NodeKind string of the target node |
| `edge_type` | UTF8 | no | EdgeKind string (e.g., `"calls"`, `"implements"`) |
| `edge_source` | UTF8 | no | ExtractionSource string (e.g., `"tree_sitter"`, `"lsp"`) |
| `edge_confidence` | UTF8 | no | `"detected"` or `"confirmed"` |
| `root_id` | UTF8 | no | Root slug (from the source node's root) |
| `updated_at` | Int64 | no | Unix timestamp (seconds) of last write |

### `pr_merges` table

Stores PR-level change summaries extracted from merge commits on the base branch.

| Column | Type | Nullable | Description |
|--------|------|----------|-------------|
| `id` | UTF8 | no | `root:merge_commit_sha` |
| `root_id` | UTF8 | no | Repository root slug |
| `merge_sha` | UTF8 | no | The merge commit SHA |
| `branch_name` | UTF8 | yes | Branch name from commit message |
| `title` | UTF8 | no | First line of the merge commit message |
| `description` | UTF8 | yes | Remaining lines of the merge commit message |
| `author` | UTF8 | no | Commit author |
| `merged_at` | Int64 | no | Unix timestamp (seconds) of the merge |
| `commit_count` | UInt32 | no | Number of commits in the PR |
| `files_changed` | UTF8 | no | JSON array of file paths changed by the PR |
| `updated_at` | Int64 | no | Unix timestamp (seconds) of last write |

### `file_index` table

Tracks which files have been indexed and by which extractors, enabling incremental re-indexing on file changes.

| Column | Type | Nullable | Description |
|--------|------|----------|-------------|
| `path` | UTF8 | no | File path relative to repo root |
| `root_id` | UTF8 | no | Repository root slug |
| `mtime` | Int64 | no | File modification time (Unix timestamp seconds) |
| `size` | UInt64 | no | File size in bytes |
| `last_indexed` | Int64 | no | Unix timestamp (seconds) when this file was last indexed |
| `extractors_used` | UTF8 | no | Comma-separated list of extractor names that processed this file |

### `artifacts` table (embedding index)

The embedding index lives in the same LanceDB directory as the graph tables, stored under the table name `artifacts`. It is managed separately by `EmbeddingIndex` in `src/embed.rs`. This table is NOT covered by `SCHEMA_VERSION` — it has its own schema validation at startup.

| Column | Type | Nullable | Description |
|--------|------|----------|-------------|
| `id` | UTF8 | no | Stable node ID (matches `symbols.id`) or commit/PR ID |
| `kind` | UTF8 | no | `"code:{kind}"` for code nodes, `"commit"` for git commits, or the `.oh/` artifact kind |
| `title` | UTF8 | no | Display title (e.g., `"function search_chunks (rust)"`) |
| `body` | UTF8 | no | Signature + file location for code nodes; commit message for commits |
| `text_hash` | UTF8 | yes | BLAKE3 hash of embedding input text + scalar filter values. Used to skip re-embedding unchanged items. |
| `file_path` | UTF8 | yes | Source file path — enables `.only_if(file_path = '...')` pre-filtering before vector ranking |
| `language` | UTF8 | yes | Programming language string |
| `subsystem` | UTF8 | yes | Detected subsystem cluster name (see Subsystem Metadata) |
| `cyclomatic` | Int32 | yes | Cyclomatic complexity — enables `min_complexity` pre-filtering |
| `vector` | FixedSizeList(Float32, dim) | no | Semantic embedding vector (384 dimensions for the default model) |

**Indexes on `artifacts`:** FTS indexes on `title`, `body`, and `file_path`. Combined with the vector column, this enables hybrid search (BM25 + vector + RRF fusion).

---

## 2. In-Memory Graph Structure

The in-memory representation lives in `GraphState` (defined in `src/server/state.rs`). It is populated from LanceDB at startup and kept in sync via incremental updates.

```rust
pub struct GraphState {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub index: GraphIndex,
    pub last_scan_completed_at: Option<std::time::Instant>,
}
```

### NodeId

`NodeId` is the four-tuple that uniquely identifies a node. Its string form is the stable ID stored in LanceDB.

```rust
pub struct NodeId {
    pub root: String,      // root slug (e.g., "repo-native-alignment")
    pub file: PathBuf,     // file path relative to repo root
    pub name: String,      // symbol name
    pub kind: NodeKind,    // see NodeKind enum
}
```

**Stable ID format:** `root:file:name:kind`

Example: `repo-native-alignment:src/graph/mod.rs:NodeId:struct`

**Stability guarantees:**
- Stable across builds as long as `(root, file, name, kind)` don't change.
- Breaks on: file rename, symbol rename, kind change, or root slug change.
- Does NOT encode line numbers — renaming or adding imports above a function does not invalidate its stable ID.
- Known collision: two symbols with the same name, kind, and file get the same stable ID. This is tracked as issue #119.

### Node

```rust
pub struct Node {
    pub id: NodeId,
    pub language: String,         // inferred from file extension at load time
    pub line_start: usize,
    pub line_end: usize,
    pub signature: String,        // declaration line(s)
    pub body: String,             // full AST node text
    pub metadata: BTreeMap<String, String>,  // extractor-specific key-value pairs
    pub source: ExtractionSource,
}
```

Key metadata keys stored in `Node.metadata` (all optional):

| Key | Description |
|-----|-------------|
| `subsystem` | Detected subsystem cluster (see Subsystem Metadata) |
| `importance` | PageRank score as a float string |
| `cyclomatic` | Cyclomatic complexity as an integer string |
| `virtual` | `"true"` for external nodes produced by LSP enrichment |
| `package` | Crate/package name for virtual nodes |
| `value` | Constant value for `Const` nodes |
| `synthetic` | `"true"` for inferred constants |
| `is_async` | `"true"` for async functions |
| `is_test` | `"true"` for test functions |
| `is_static` | `"true"` or `"false"` for methods |
| `decorators` | Comma-separated decorator/attribute text |
| `type_params` | Generic type parameters |
| `oh_kind` | Present on `.oh/` artifact nodes; determines embedding text builder |
| `doc_comment` | Extracted doc comment for the symbol |
| `diagnostic_severity` | For `NodeKind::Other("diagnostic")` nodes |
| `diagnostic_source` | LSP server name for diagnostics |
| `diagnostic_message` | Full diagnostic text |
| `http_method` | For `NodeKind::ApiEndpoint` nodes |
| `http_path` | HTTP path for API endpoint nodes (full path after router prefix resolution) |
| `http_path_local` | Original local path fragment before router prefix concatenation (e.g., `"/list"` when a FastAPI `APIRouter(prefix="/orders")` is applied) — set once on first prefix application; subsequent applications read this value instead of re-prefixing |

### NodeKind

```rust
pub enum NodeKind {
    Function,         // function, method, closure
    Struct,           // struct, class, record
    Trait,            // trait, interface, protocol
    Enum,             // enum type
    TypeAlias,        // type alias (type Foo = Bar)
    Module,           // module, namespace, package declaration
    Import,           // use/import statement
    Const,            // constant declaration
    Impl,             // impl block (Rust) or class body
    ProtoMessage,     // Protocol Buffer message
    SqlTable,         // SQL table definition
    ApiEndpoint,      // HTTP API endpoint
    Macro,            // macro definition (macro_rules!, #define)
    Field,            // struct/class field or record member
    PrMerge,          // merged PR — the natural unit of meaningful change
    EnumVariant,      // enum variant (e.g., Option::Some)
    MarkdownSection,  // markdown heading with its content body
    Other(String),    // escape hatch for new node types (e.g., "diagnostic", "yaml_mapping")
}
```

**Embeddable kinds** (worth including in semantic search):
Function, Struct, Trait, Enum, TypeAlias, Macro, ProtoMessage, SqlTable, ApiEndpoint, MarkdownSection, Other(_)

**Non-embeddable kinds** (structural noise):
Import, Const, Module, Impl, Field, EnumVariant, PrMerge

### Edge

```rust
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub source: ExtractionSource,
    pub confidence: Confidence,
}
```

The stable edge ID is `from_stable_id->kind->to_stable_id`. Edges are directional: A→B and B→A are distinct.

### EdgeKind

| Variant | String | Directionality | Description |
|---------|--------|----------------|-------------|
| `Calls` | `calls` | caller → callee | Function/method call |
| `Implements` | `implements` | implementor → trait/interface | Trait implementation |
| `DependsOn` | `depends_on` | dependent → dependency | Module/package dependency |
| `ConnectsTo` | `connects_to` | source → target | Topology boundary connection |
| `Defines` | `defines` | container → member | Module defines function |
| `HasField` | `has_field` | struct → field | Struct has a field |
| `Evolves` | `evolves` | PR → schema/component | PR evolved this entity |
| `ReferencedBy` | `referenced_by` | symbol → referencing site | Symbol referenced at a location |
| `References` | `references` | source → target | Markdown link or code reference |
| `TopologyBoundary` | `topology_boundary` | service A → service B | Architectural boundary crossing |
| `Modified` | `modified` | PR merge → symbol | PR modified this symbol |
| `Affected` | `affected` | PR merge → topology component | PR affected this component |
| `Serves` | `serves` | PR merge → outcome | PR serves a business outcome |
| `TestedBy` | `tested_by` | test fn → production fn | Test function covers production code |
| `BelongsTo` | `belongs_to` | symbol → module | Symbol belongs to a module |
| `ReExports` | `re_exports` | module → symbol | Public re-export (`pub use`, `__all__`) |
| `UsesFramework` | `uses_framework` | subsystem/symbol → framework node | A subsystem or symbol uses a detected framework |
| `Produces` | `produces` | producer → channel | A symbol/handler produces events to a channel/topic |
| `Consumes` | `consumes` | consumer → channel | A symbol/handler consumes events from a channel/topic |

**PageRank weights by edge kind** (used for importance scoring):

| EdgeKind | Weight |
|----------|--------|
| Calls | 1.0 |
| Implements | 0.8 |
| DependsOn, ReferencedBy, References | 0.5 |
| TestedBy, BelongsTo, ReExports, ConnectsTo | 0.2–0.3 |
| Produces, Consumes | 0.4 |
| Defines, HasField, UsesFramework | 0.1 |
| Evolves, TopologyBoundary, Modified, Affected, Serves | 0.05 |

### Confidence

```rust
pub enum Confidence {
    Detected,   // automatically detected by an extractor — used for all tree-sitter and LSP edges
    Confirmed,  // confirmed by a human or higher-confidence source — not yet used in practice
}
```

### ExtractionSource

```rust
pub enum ExtractionSource {
    TreeSitter,  // tree-sitter AST parsing
    Lsp,         // LSP call hierarchy, type hierarchy, references
    Schema,      // schema extractors (proto, SQL, OpenAPI)
    Git,         // git history (merge commits, diff analysis)
    Markdown,    // pulldown-cmark markdown parsing
}
```

### GraphIndex

`GraphIndex` is a derived index rebuilt from `Vec<Edge>` at load time. It provides O(1) traversal operations that would be expensive as LanceDB columnar joins.

```rust
pub struct GraphIndex {
    graph: DiGraph<NodeRef, EdgeRef>,
    node_lookup: HashMap<String, NodeIndex>,  // stable_id -> petgraph NodeIndex
}
```

`NodeRef` stores only `(id: String, node_type: String)` — enough to identify a node and look it up in LanceDB. The full `Node` data lives in `GraphState.nodes`.

`EdgeRef` stores only `(edge_type: EdgeKind)`.

The `GraphIndex` supports: neighbors, BFS/DFS traversal, impact analysis (reachability), Dijkstra shortest path, Tarjan SCC (strongly connected components), and PageRank via custom weighted random walks.

---

## 3. The Pipeline: Extraction to MCP Rendering

As of v0.1.15, the extraction pipeline is event-driven. `build_full_graph_inner` fires `RootDiscovered` seed events into an `EventBus` pre-loaded with built-in consumers. Each consumer reacts to events and may emit follow-on events. The bus drains depth-first until no events remain.

```text
build_full_graph_inner
        |
        v
EventBus.emit_all(RootDiscovered { slug, path })
        |
        v
ScanStatsConsumer (subscribes: all event kinds — registered first)
  -- maintains live ScanStats for list_roots queries (no file I/O)

ManifestConsumer (subscribes: RootDiscovered)
  -- reads package.json/Cargo.toml for dependency edges

TreeSitterConsumer (subscribes: RootDiscovered)
  -- rayon parallel per-file extraction --
  -- fires: RootExtracted { slug, nodes, edges, dirty_slugs }
        |
        v
LanguageAccumulatorConsumer (subscribes: RootExtracted)
  -- groups nodes by detected language, filtered by dirty_slugs --
  -- fires: LanguageDetected { slug, language, nodes } (once per language)

AllEnrichmentsGate (subscribes: RootExtracted + EnrichmentComplete)
  -- counts expected vs received enrichments --
  -- registered before LspConsumers so it captures language count first

OpenApiConsumer (subscribes: RootExtracted)
  -- bidirectional endpoint → handler linking

GrpcConsumer (subscribes: RootExtracted)
  -- proto RPC → caller stub matching

EmbeddingIndexerConsumer (subscribes: RootExtracted)
  -- streams embed tasks in parallel with LSP enrichment --
  -- re-embeds nodes whose text_hash changed
        |
        v
LspConsumer × N (subscribes: LanguageDetected)
  -- one consumer per language, 38 servers auto-detected --
  -- adaptive wait (serverStatus/quiescent or probe, 10-min circuit breaker) --
  -- fires: EnrichmentComplete { slug, language, added_edges, new_nodes, updated_nodes }

AllEnrichmentsGate fires: AllEnrichmentsDone { slug, nodes, edges, lsp_edges, lsp_nodes }
        |
        v
ApiLinkConsumer (subscribes: AllEnrichmentsDone)
TestedByConsumer (subscribes: AllEnrichmentsDone)
EnrichmentFinalizer (subscribes: AllEnrichmentsDone)
  -- runs post-extraction passes over full LSP-enriched graph --
  -- fires: PassesComplete { slug, nodes, edges, detected_frameworks }
        |
        v
FrameworkDetectionConsumer (subscribes: PassesComplete)
  -- may fire: FrameworkDetected { framework, nodes } (one per detected framework)

FastapiRouterPrefixConsumer (subscribes: PassesComplete)
SdkPathInferenceConsumer (subscribes: PassesComplete)
NextjsRoutingConsumer (subscribes: PassesComplete) — monorepo-aware
PubSubConsumer (subscribes: PassesComplete)
WebSocketConsumer (subscribes: PassesComplete)
        |
        v
LanceDBConsumer (subscribes: PassesComplete)
  -- background persist: persist_graph_to_lance or persist_graph_incremental

SubsystemConsumer (subscribes: CommunityDetectionComplete)
  -- Louvain community detection, PageRank --
  -- updates node.metadata["subsystem"] and node.metadata["importance"]
        |
        v
MCP tool call (e.g., search, repo_map, list_roots)
        |
        v
src/server/handlers.rs
  -- hybrid search: vector + FTS + RRF fusion via EmbeddingIndex --
  -- graph traversal: neighbors/impact/reachable via GraphIndex --
  -- list_roots: reads ScanStats (live) or sentinel files (cold-start fallback)
        |
        v
MCP response (JSON)
```

### Key call sites

| Operation | Location |
|-----------|----------|
| Full graph build | `src/server/graph.rs::build_full_graph_inner` |
| EventBus construction | `src/extract/consumers.rs::build_builtin_bus` |
| Persist full | `src/server/store.rs::persist_graph_to_lance` |
| Persist incremental | `src/server/store.rs::persist_graph_incremental` |
| Load from LanceDB | `src/server/store.rs::load_graph_from_lance` |
| Rebuild petgraph index | `src/graph/index.rs::GraphIndex::rebuild_from_edges` |
| Embed symbols | `src/embed.rs::EmbeddingIndex::index_all_with_symbols` |
| Hybrid search | `src/embed.rs::EmbeddingIndex::search` |
| MCP search handler | `src/server/handlers.rs::handle_search` |
| Live scan stats | `src/extract/scan_stats.rs::ScanStatsConsumer` |

---

## 4. NodeId Stability

The stable ID `root:file:name:kind` is designed to be stable across git history as long as the symbol doesn't change its identity.

**What makes a stable ID stable:**
- `root` — the root slug assigned in the workspace config. Stable unless the config changes.
- `file` — relative file path. Stable unless the file is renamed or moved.
- `name` — the symbol name. Stable unless the symbol is renamed.
- `kind` — the NodeKind. Stable unless the symbol changes type (e.g., function to macro).

**What breaks stability:**
- File rename or move — the `file` component changes.
- Symbol rename — the `name` component changes.
- Adding a line number anywhere — line numbers are NOT part of the stable ID.
- Changing root slug in workspace config — rare.

**Short ID vs stable ID:**
MCP tool output displays short IDs with the root prefix stripped (e.g., `src/scanner.rs:scan:function`). Graph lookups always use full stable IDs. `GraphState::resolve_node_id` handles prefix resolution: it tries the input as-is first, then prepends each known root slug.

**Collision edge case:** Two nodes with identical `(root, file, name, kind)` get the same stable ID. This can happen with same-named types in the same file. See issue #119.

---

## 5. Subsystem Metadata

Subsystems are detected automatically via community detection on the in-memory graph. The algorithm groups nodes that are heavily connected to each other relative to the rest of the graph.

**Storage:** The subsystem name is stored in `Node.metadata["subsystem"]` (the key is the constant `SUBSYSTEM_KEY = "subsystem"` defined in `src/server/graph.rs`).

**Persistence to LanceDB:** The `subsystem` value flows through two paths:
1. `symbols` table — the `subsystem` key in `node.metadata` is not a first-class column. It is stored only implicitly through the metadata map, which is reconstructed at load time from the typed columns (none of which is `subsystem`). This means subsystem is re-computed after each graph rebuild.
2. `artifacts` table — the `subsystem` column is explicitly stored and used for pre-filtering in hybrid search. The `text_hash` includes the subsystem value so that reassignments force re-embedding.

**Querying by subsystem:** The `search()` MCP tool accepts a `subsystem` parameter that pushes a scalar filter (`subsystem = '...'`) into LanceDB before vector ranking. The `repo_map` tool reports detected subsystems with their member counts and cohesion scores.

**How subsystems are named:** After community detection assigns cluster IDs, the cluster is named after the most-connected node in the cluster (typically a module or frequently-called function). Names are deterministic given the same graph topology.

---

## 6. Content-Addressed Consumer Cache

As of v0.1.15 (#526, #533), per-consumer cache keys replace the single global `EXTRACTION_VERSION` integer for incremental extraction invalidation.

### ConsumerCacheKey

Defined in `src/extract/cache.rs`.

```rust
pub struct ConsumerCacheKey {
    /// blake3 hash (hex string) of `event.canonical_bytes()`.
    pub payload_hash: String,
    /// Consumer's self-declared version (see `ExtractionConsumer::version()`).
    pub consumer_version: u64,
}
```

**Cache key semantics:**
- `cache_key = (blake3(event.canonical_bytes()), consumer.version())`
- Upstream output changes → payload hash changes → downstream cache misses automatically.
- Consumer logic version bumps → only that consumer's entries miss.
- Config-driven consumers (`CustomExtractorConsumer`) compute `version()` as `blake3(toml_file_contents)[..8]` as a `u64` — no manual bump needed when config changes.

**`ExtractionConsumer::version()` trait method** (defined in `src/extract/event_bus.rs`):

```rust
fn version(&self) -> u64 {
    0  // default: stable logic, never needs invalidation
}
```

Bump when the consumer's extraction logic changes in a way that would produce different output for the same input event. Leave at `0` for consumers with stable, never-changing logic.

**`is_cacheable()` trait method:**

```rust
fn is_cacheable(&self) -> bool {
    true  // default: pure/stateless transformational consumers
}
```

Override to `false` for consumers that:
- Accumulate state across multiple `on_event` calls (e.g., `AllEnrichmentsGate`, `ScanStatsConsumer`).
- Trigger external side-effects that must run every time (e.g., `LanceDBConsumer`, `EmbeddingIndexerConsumer`).
- Read filesystem state beyond the event payload (e.g., `ManifestConsumer`, `TreeSitterConsumer`).

**Migration from `EXTRACTION_VERSION`:** The global `EXTRACTION_VERSION` integer (`src/graph/store.rs`) is deprecated and kept only for backward-compatible sentinel reads on cold start. New invalidation is driven exclusively by `ConsumerCacheKey`.

---

## 7. EventBus Pipeline Types

Defined in `src/extract/event_bus.rs` and `src/extract/consumers.rs`.

### ExtractionConsumer trait

```rust
pub trait ExtractionConsumer: Send + Sync {
    fn name(&self) -> &str;
    fn subscribes_to(&self) -> &[ExtractionEventKind];
    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>>;
    fn version(&self) -> u64 { 0 }
    fn is_cacheable(&self) -> bool { true }
}
```

### EventBus

Ordered consumer registry. Dispatches events depth-first. Holds an in-memory content-addressed cache keyed by `(consumer_name, ConsumerCacheKey)`.

```rust
pub struct EventBus {
    // (internal)
}
// Key methods:
// EventBus::new()
// EventBus::register(consumer: Box<dyn ExtractionConsumer>)
// EventBus::emit(event: ExtractionEvent)
// EventBus::emit_all(seeds: Vec<ExtractionEvent>)
```

### BusOptions

Groups optional dependency overrides for `build_builtin_bus` and `run_post_passes_via_bus`. Pass `BusOptions::default()` in test/stub mode.

```rust
pub struct BusOptions {
    /// Shared ScanStats handle. When None, a fresh throwaway Arc is created.
    pub scan_stats: Option<Arc<RwLock<ScanStats>>>,
    /// When Some, EmbeddingIndexerConsumer streams embed tasks per RootExtracted.
    pub embed_idx: Option<Arc<EmbeddingIndex>>,
    /// When Some, LanceDBConsumer fires a background persist on PassesComplete.
    pub lance_repo_root: Option<Arc<PathBuf>>,
}
```

### ScanStats / ScanStatsConsumer

Defined in `src/extract/scan_stats.rs`.

`ScanStats` is live scan state maintained by `ScanStatsConsumer`. `list_roots` reads from this singleton for in-progress and complete status. Sentinel files (`extract_completed.json`) remain as a cold-start fallback.

```rust
pub struct ScanStats {
    pub roots_queued: usize,
    pub roots_extracted: HashMap<String, RootExtractedStats>,
    pub languages_in_flight: HashMap<String, Vec<String>>,
    pub languages_done: HashMap<String, Vec<String>>,
    pub lsp_edge_counts: HashMap<String, HashMap<String, usize>>,
    pub roots_complete: HashMap<String, RootCompleteStats>,
}
```

`ScanStatsConsumer` subscribes to: `RootDiscovered`, `RootExtracted`, `LanguageDetected`, `EnrichmentComplete`, `PassesComplete`. It is non-cacheable (`is_cacheable() = false`) because it accumulates state across calls.

### AllEnrichmentsDone / AllEnrichmentsGate

`AllEnrichmentsGate` (defined in `src/extract/consumers.rs`) subscribes to both `RootExtracted` (to count expected languages) and `EnrichmentComplete` (to count completions). When all expected enrichments are received for a root, it emits `AllEnrichmentsDone { slug, nodes, edges, lsp_edges, lsp_nodes, updated_nodes }`.

`PostExtractionConsumer` subscribes to `AllEnrichmentsDone` so post-extraction passes see the full LSP-enriched graph — not just the tree-sitter output. This replaces the former sequential pipeline where passes ran before LSP enrichment was available.

### dirty_slugs on RootExtracted

The `RootExtracted` event carries an `Option<HashSet<String>>` field called `dirty_slugs`:

- `None` — all roots are dirty (first-run or cache-hit LSP paths where no prior LSP edges exist). All downstream consumers process all nodes.
- `Some(set)` — only the listed root slugs have changed files. Consumers like `LanguageAccumulatorConsumer` filter nodes to only emit `LanguageDetected` events for languages with nodes in dirty roots, avoiding redundant LSP enrichment on unchanged roots.
- `Some(empty set)` — no roots are dirty. Downstream consumers may short-circuit entirely.

The `dirty_slugs` value is included in `canonical_bytes()` for cache key computation, so different dirty sets produce different cache keys and consumers are re-invoked appropriately.
