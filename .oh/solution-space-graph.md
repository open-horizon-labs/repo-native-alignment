# Solution Space: Unified LanceDB Graph Model

**Updated:** 2026-03-07
**Issue:** #11 — Graph: unified LanceDB graph model for code, topology, schema, and business context

## Problem Statement

**Problem:** Multiple extractors (tree-sitter, LSP, schema, topology) produce heterogeneous node and edge types that must be stored in LanceDB, queried structurally (graph traversal) AND semantically (vector search), and updated incrementally as files change.

**Key Constraint:** LanceDB is columnar + vector, not a graph database. Graph traversal (N-hop, path queries) is not a native operation. The model must make traversal feasible without abandoning LanceDB's strengths (vector search, batch columnar writes, Arrow interop).

**Success looks like:** A single storage layer where `outcome_progress`-style structural joins generalize to any node/edge traversal, compound queries combine traversal + semantic search, and adding a new extractor means registering new node/edge types without schema migration pain.

## Current State

What exists today:
- **`EmbeddingIndex`** in `src/embed.rs`: single flat LanceDB table (`artifacts`) with columns `id, kind, title, body, vector`. Full rebuild on each index. No incremental update.
- **`outcome_progress`** in `src/query.rs`: hand-coded structural join (outcome -> tagged commits -> changed files -> symbols). This is the pattern we want to generalize.
- **In-memory data:** `CodeSymbol`, `MarkdownChunk`, `OhArtifact`, `GitCommitInfo` are Rust structs loaded fresh each query. No persistent graph.
- **LanceDB version:** 0.26 with arrow-array/arrow-schema 57.

The gap: today's "graph" is ad-hoc Rust code joining in-memory vectors. There is no persistent edge storage, no general traversal, and no way to combine vector similarity with structural relationships.

## Candidates Considered

| Option | Level | Approach | Trade-off |
|--------|-------|----------|-----------|
| A | Local Optimum | Single `nodes` + single `edges` table, all types in one table | Simple schema, sparse columns, awkward per-type queries |
| B | Local Optimum | Per-type node tables + unified `edges` table | Clean per-type schemas, more tables to manage, edges still unified |
| C | Reframe | Per-type node tables + edge adjacency lists stored ON nodes (no edges table) | No join for 1-hop, but multi-hop requires recursive self-joins |
| D | Redesign | Per-type node tables + unified edges table + in-memory petgraph index for traversal | Best of both: LanceDB for storage/search, petgraph for graph ops |

### Option A: Single Nodes + Single Edges (Flat Universal Tables)

**Approach:** Two tables. `nodes` has columns: `id, node_type, name, file_path, line_start, line_end, signature, body, properties_json, vector, root_id, updated_at`. `edges` has: `id, source_id, target_id, edge_type, properties_json, root_id, updated_at`. Every node type goes in `nodes`, every edge type goes in `edges`. Type-specific data lives in `properties_json`.

**Level:** Local Optimum

- **Solves stated problem:** Yes. All node/edge types fit. Traversal via SQL-style joins on edges table.
- **Implementation cost:** Low. Two tables, two Arrow schemas.
- **Maintenance burden:** Medium. JSON properties are opaque to columnar queries -- cannot filter/sort by `protocol` without parsing JSON. Sparse columns waste space (a `field` node has no `line_start`).
- **Second-order effects:** Vector search works well (single table = single ANN index). But queries like "find all gRPC connections" require deserializing `properties_json` for every edge, defeating columnar benefits.
- **Schema evolution:** Easy to add new types -- just new values in `node_type`/`edge_type` strings. But no compile-time safety.

### Option B: Per-Type Node Tables + Unified Edges Table

**Approach:** Separate tables per node category: `symbols` (function, struct, trait, etc.), `components`, `schemas`, `fields`, `artifacts`, `files`. Each table has typed columns appropriate to that node type. One `edges` table with `source_id, source_type, target_id, target_type, edge_type, properties_json`. Embeddings stored inline in each node table.

**Level:** Local Optimum

- **Solves stated problem:** Yes. Per-type tables give clean schemas. Edges table enables traversal.
- **Implementation cost:** Medium. More Arrow schemas to define, but each is simple and typed.
- **Maintenance burden:** Medium. Adding a new node type = new table + new Arrow schema. But each table is self-contained.
- **Second-order effects:** Vector search requires knowing which table to search (or searching all and merging). N-hop traversal requires: query edges -> resolve target type -> query appropriate node table. This is 2 queries per hop.
- **Schema evolution:** New node type = new table (no migration of existing tables). New edge type = new `edge_type` value. Clean separation.
- **Incremental update:** Easy. File changes -> delete nodes with that `file_path` from relevant tables -> delete edges with those node IDs -> re-extract and insert.

### Option C: Edge Adjacency on Nodes (No Edges Table)

**Approach:** Per-type node tables as in B, but instead of a separate edges table, each node stores its outgoing edges as a JSON array: `edges: [{target_id, edge_type, properties}]`. Traversal reads a node's edges field to find neighbors.

**Level:** Reframe -- questions whether we need a separate edges table at all.

- **Solves stated problem:** Partially. 1-hop is a single read (get node, parse edges). But multi-hop requires reading each intermediate node. No way to query "all edges of type X" without scanning all nodes across all tables.
- **Implementation cost:** Low (fewer tables).
- **Maintenance burden:** High. Edge queries are expensive. "What calls this function?" (reverse traversal) requires scanning ALL nodes' edge arrays. No reverse index.
- **Second-order effects:** Makes the compound queries from the workspace-context-engine doc impractical. "What would break if I add a field to SearchRequest?" needs reverse traversal (who references this schema?) which is O(all-nodes).
- **Schema evolution:** Same as B for nodes. Edges are untyped JSON.

### Option D: LanceDB Storage + In-Memory petgraph Index (Hybrid)

**Approach:** Per-type node tables in LanceDB (as in B) for durable storage, embeddings, and semantic search. On startup (or lazily on first graph query), load all edges into an in-memory `petgraph::DiGraph`. Graph traversal (BFS, DFS, shortest path, N-hop neighbors) runs on petgraph. LanceDB handles: persistence, vector search, columnar filtering. petgraph handles: structural traversal, path queries, reachability.

**Level:** Redesign -- separates storage concern from traversal concern.

- **Solves stated problem:** Yes. Vector search on LanceDB, graph traversal on petgraph. Compound queries: petgraph finds structurally-related node IDs, then LanceDB fetches their data / runs vector search within that subset.
- **Implementation cost:** Medium-High. Need to maintain consistency between LanceDB edges table and petgraph index. Need startup loading. petgraph is a well-known Rust crate (zero new risk).
- **Maintenance burden:** Medium. petgraph index is derived (rebuilt from edges table). If it drifts, rebuild. No new persistence concern.
- **Second-order effects:**
  - Enables queries impossible with pure LanceDB: "all nodes within 3 hops of X", "shortest path from outcome to symbol", "connected components".
  - Memory cost: for a typical repo with ~10K nodes and ~50K edges, petgraph uses ~2-4 MB. Negligible.
  - Startup cost: loading edges from LanceDB into petgraph is a single table scan. Sub-second for expected volumes.
  - Incremental update: update LanceDB tables, then patch petgraph (add/remove edges) or rebuild lazily.
- **Schema evolution:** Same as B. petgraph just stores node IDs and edge types -- adding new types requires no petgraph changes.

## Evaluation Summary

| Criterion | A (Flat) | B (Per-Type) | C (On-Node Edges) | D (Hybrid petgraph) |
|-----------|----------|--------------|--------------------|-----------------------|
| Solves problem | Yes | Yes | Partially | Yes |
| Implementation cost | Low | Medium | Low | Medium-High |
| Maintenance burden | Medium | Medium | High | Medium |
| N-hop traversal | Slow (N joins) | Slow (2N queries) | Very slow (scan) | Fast (in-memory) |
| Vector search | Good (1 table) | OK (per-table) | OK (per-table) | Good (per-table + scoped) |
| Reverse traversal | OK (query edges) | OK (query edges) | Terrible | Fast (petgraph) |
| Incremental update | Easy | Easy | Easy | Easy (+ petgraph patch) |
| Compound queries | Possible but slow | Possible but slow | Impractical | Natural |
| Schema evolution | Easy | Easy | Easy | Easy |

## Recommendation

**Selected:** Option D -- Per-type LanceDB tables + in-memory petgraph index
**Level:** Redesign

### Rationale

The core insight is that LanceDB and graph traversal are different concerns that should not be forced into one mechanism.

1. **LanceDB is excellent at what it does:** columnar storage, vector ANN search, batch writes, Arrow interop. Asking it to also do graph traversal (recursive joins, path queries) fights its design.

2. **petgraph is excellent at what it does:** in-memory directed graphs with BFS/DFS/Dijkstra/connected-components in microseconds. It is a mature, well-maintained Rust crate. Adding it as a dependency is low-risk.

3. **The compound queries that justify this entire system** -- "what would break if I change this schema?" "what code serves this outcome?" -- are inherently graph traversal problems. Doing them with SQL-style joins on LanceDB is possible but slow and painful to maintain. petgraph makes them trivial.

4. **Memory cost is negligible.** A repo with 10K nodes and 50K edges uses ~2-4 MB in petgraph. The MCP server already loads embedding models (~50 MB). This is noise.

5. **The existing `outcome_progress` pattern proves the need.** That function is a hand-coded 3-hop traversal (outcome -> commits -> files -> symbols). With petgraph, it becomes: `bfs_within(outcome_node, 3)` filtered by edge type. Every new traversal query is a few lines, not a new bespoke function.

### Why not the others

- **Option A (Flat):** JSON properties defeat columnar advantages. Single nodes table with sparse columns is messy. Traversal still requires N LanceDB queries per hop.
- **Option B (Per-Type without petgraph):** Good storage design (we adopt the per-type tables part), but graph traversal via LanceDB joins is the bottleneck. B is the storage half of D.
- **Option C (On-Node Edges):** Reverse traversal is O(all-nodes). The most important queries (impact analysis: "what depends on X?") are reverse traversals. Disqualifying.

### Accepted trade-offs

1. **Two systems to keep in sync.** LanceDB is the source of truth for edges; petgraph is a derived index. On any edge mutation, petgraph must be updated or marked stale. Mitigation: petgraph is cheap to rebuild from the edges table (sub-second), so "mark stale + lazy rebuild" is the simplest correct approach.

2. **petgraph is in-memory only.** If the MCP server restarts, petgraph rebuilds from LanceDB on first graph query. This is acceptable because (a) startup is fast, (b) LanceDB is the durable store.

3. **New dependency.** petgraph is well-established (300M+ downloads, used by cargo itself). Low risk.

## Implementation Notes

### Proposed Table Schema

```
Table: symbols
  id: Utf8 (deterministic: "{root_id}:{file_path}:{name}:{line_start}")
  root_id: Utf8 (multi-root support)
  file_path: Utf8
  name: Utf8
  kind: Utf8 (function, struct, trait, enum, const, module, import)
  line_start: UInt32
  line_end: UInt32
  signature: Utf8
  parent_scope: Utf8 (nullable)
  body: Utf8
  vector: FixedSizeList<Float32> (embedding of signature + body context)
  updated_at: Int64 (unix timestamp)

Table: components
  id: Utf8
  root_id: Utf8
  name: Utf8
  kind: Utf8 (service, process, binary, task)
  file_path: Utf8 (primary definition file)
  properties_json: Utf8 (protocol, transport, sync/async)
  vector: FixedSizeList<Float32>
  updated_at: Int64

Table: schemas
  id: Utf8
  root_id: Utf8
  file_path: Utf8
  name: Utf8
  kind: Utf8 (protobuf_message, sql_table, openapi_schema, serde_struct, ts_interface)
  source_format: Utf8 (proto, sql, yaml, rust, typescript)
  version: Utf8 (nullable, for migration tracking)
  vector: FixedSizeList<Float32>
  updated_at: Int64

Table: fields
  id: Utf8
  root_id: Utf8
  schema_id: Utf8 (parent schema)
  name: Utf8
  field_type: Utf8
  required: Boolean
  ordinal: UInt32
  updated_at: Int64

Table: artifacts (existing, extended)
  id: Utf8
  root_id: Utf8
  kind: Utf8 (outcome, signal, guardrail, metis)
  title: Utf8
  body: Utf8
  file_path: Utf8
  vector: FixedSizeList<Float32>
  updated_at: Int64

Table: edges
  id: Utf8 (deterministic: "{source_id}->{edge_type}->{target_id}")
  source_id: Utf8
  source_type: Utf8 (symbol, component, schema, field, artifact)
  target_id: Utf8
  target_type: Utf8
  edge_type: Utf8 (calls, implements, depends_on, connects_to, defines, has_field, evolves, relates_to, shaped)
  properties_json: Utf8 (nullable, for edge-specific data like protocol, serialization)
  root_id: Utf8
  updated_at: Int64
```

### petgraph Integration

```rust
// Conceptual structure
struct GraphIndex {
    graph: petgraph::DiGraph<NodeRef, EdgeRef>,
    node_lookup: HashMap<String, petgraph::NodeIndex>,  // node_id -> index
}

struct NodeRef {
    id: String,
    node_type: String,  // "symbol", "component", etc.
}

struct EdgeRef {
    edge_type: String,
    properties: Option<serde_json::Value>,
}
```

Key operations:
- `neighbors(node_id, edge_types, direction)` -- 1-hop filtered by edge type
- `reachable(node_id, max_hops, edge_types)` -- BFS within N hops
- `path(source_id, target_id)` -- shortest path
- `impact(node_id)` -- reverse BFS ("what depends on this?")

### Incremental Update Strategy

Follows the reverse delta model from fsPulse:

1. **File changes detected** (mtime or git diff).
2. **Delete stale nodes:** query each node table for `file_path = changed_file`, collect their IDs.
3. **Delete stale edges:** query edges table for `source_id IN stale_ids OR target_id IN stale_ids`.
4. **Re-extract:** run relevant extractors on changed files, producing new nodes and edges.
5. **Upsert:** insert new nodes and edges into LanceDB tables.
6. **Invalidate petgraph:** mark the in-memory index as stale. Next graph query triggers rebuild.

For the initial implementation, full petgraph rebuild on any change is acceptable. Incremental petgraph patching is an optimization for later.

### Node ID Strategy

Deterministic IDs enable upsert semantics:
- Symbols: `{root}:{file}:{name}:{line}` (line disambiguates overloads)
- Components: `{root}:component:{name}`
- Schemas: `{root}:{file}:{name}`
- Fields: `{schema_id}:{field_name}`
- Artifacts: `{root}:.oh/{kind}/{slug}`
- Edges: `{source_id}->{edge_type}->{target_id}`

### Migration Path from Current Code

1. Extend existing `EmbeddingIndex` to manage multiple tables (or replace with a `GraphStore` struct).
2. Keep the current `artifacts` table schema as-is for backward compatibility during transition.
3. Add `symbols`, `edges` tables first (these enable generalizing `outcome_progress`).
4. Add `components`, `schemas`, `fields` tables as their respective extractors are built.
5. Add petgraph after edges table exists. Until then, structural joins continue to work as hand-coded queries (current approach).

### Embedding Strategy

- **Per-node vectors** stored inline in each node table (not a separate table).
- **What to embed:** for symbols, embed `signature + body context`; for schemas, embed `name + field definitions`; for artifacts, embed `title + body` (current behavior).
- **When to embed:** during extraction, as a batch operation. fastembed processes batches efficiently.
- **Cross-table semantic search:** query each table's vector index, merge results by score. This is 3-5 vector queries -- fast enough given LanceDB's ANN performance.

### Dependency Addition

```toml
# In-memory graph index (traversal, path queries, impact analysis)
petgraph = "0.7"
```

petgraph 0.7 is stable, widely used (cargo, rustc, etc.), and has no transitive dependency conflicts with the existing stack.
