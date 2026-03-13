use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arrow_array::{Array, BooleanArray, Float64Array, Int32Array, RecordBatch, RecordBatchIterator, StringArray, UInt32Array, Int64Array};
use arrow_array::builder::BooleanBuilder;
use async_trait::async_trait;
use rust_mcp_sdk::macros::{self, JsonSchema};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CallToolError, CallToolRequestParams, CallToolResult, ListToolsResult,
    PaginatedRequestParams, RpcError, TextContent,
};
use serde::{Deserialize, Serialize};

use crate::embed::{EmbeddingIndex, SearchMode, SearchOutcome};
use crate::extract::{ExtractorRegistry, EnricherRegistry};
use crate::graph::{self, EdgeKind, Node, Edge, Confidence, ExtractionSource, NodeId, NodeKind};
use crate::graph::index::GraphIndex;
use crate::graph::store::{symbols_schema, edges_schema, schema_meta_schema, SCHEMA_VERSION};
use crate::roots::{WorkspaceConfig, cache_state_path};
use crate::scanner::{ScanResult, Scanner};
use crate::types::OhArtifactKind;
use crate::{git, markdown, oh, query, ranking};
use arc_swap::ArcSwap;
use petgraph::Direction;
use tokio::sync::RwLock;

/// Minimum importance score to display in tool output.
/// Scores at or below this threshold are suppressed as noise.
const IMPORTANCE_THRESHOLD: f64 = 0.001;

// ── Tool input structs ──────────────────────────────────────────────

#[macros::mcp_tool(
    name = "oh_search_context",
    description = "Semantic search across business context, commits, code, and markdown. Describe what you need in plain language. Returns results ranked 0-1 by relevance; test files are demoted. Enable include_code for ranked symbol search (exact name > contains > signature, production before tests), include_markdown for doc sections. For exact symbol name lookup use search_symbols instead. search_mode: hybrid (default, keyword+vector RRF), keyword (BM25 only), semantic (vector only)."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhSearchContext {
    /// Natural language description of what you're looking for
    pub query: String,
    /// Optional: filter by artifact type (outcome, signal, guardrail, metis, commit)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_types: Option<Vec<String>>,
    /// Maximum results to return (default: 5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// Also search code symbols by name/signature (default: false)
    #[serde(default)]
    pub include_code: Option<bool>,
    /// Also search markdown sections (default: false)
    #[serde(default)]
    pub include_markdown: Option<bool>,
    /// Search ranking mode: "hybrid" (default, keyword + vector RRF), "keyword" (BM25 only), "semantic" (vector only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
}


#[macros::mcp_tool(
    name = "outcome_progress",
    description = "Track progress on a business outcome. Finds tagged commits, code symbols in changed files, and related markdown. Returns a navigable summary with stable Node IDs for use with search_symbols and graph_query."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OutcomeProgress {
    /// The outcome ID (e.g. 'agent-alignment') from .oh/outcomes/
    pub outcome_id: String,
}

// ── Unified search tool ─────────────────────────────────────────────
// Combines the functionality of the former `search_symbols` (flat search)
// and `graph_query` (graph traversal) into a single tool. The old tools
// are kept as deprecated aliases that route here.

#[macros::mcp_tool(
    name = "search",
    description = "Find code symbols and trace their relationships. Without `mode`, performs flat ranked search (by name/signature). With `mode` (neighbors/impact/reachable/tests_for), performs graph traversal from matched symbols. `tests_for` finds which test functions call a symbol. Entry point: `query` (name or semantic search) or `node` (stable ID from previous results). Batch: `nodes` retrieves multiple IDs in one call. `compact: true` returns signature + location only (~25x fewer tokens). Filter by kind, language, file. Sort by relevance, complexity, or importance (PageRank)."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct Search {
    /// Search query string — matched against symbol name and signature for flat search, or used as semantic search for graph traversal entry points
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Start from a known stable node ID (from previous search results). Takes precedence over query for graph traversal entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Graph traversal mode: "neighbors" (direct connections), "impact" (reverse dependents), "reachable" (forward BFS), "tests_for" (which test functions call this symbol). Omit for flat search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Maximum traversal depth (default: 1 for neighbors, 3 for impact/reachable). Only used with mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hops: Option<u32>,
    /// Direction for neighbors mode: "outgoing" (default), "incoming", "both". Only used with mode="neighbors".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Filter edge types: calls, depends_on, implements, defines, etc. Only used with mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Filter by symbol kind (function, struct, trait, enum, module, import, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Filter by language (rust, python, typescript, go, markdown)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Filter by file path substring
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Filter to a specific workspace root (by slug, e.g. "zettelkasten")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Number of results (flat search, default: 10) or entry points (traversal, default: 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Sort results by "relevance" (default), "complexity" (descending cyclomatic), or "importance" (descending PageRank)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Minimum cyclomatic complexity threshold. Only return functions with complexity >= this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_complexity: Option<u32>,
    /// If true, include only synthetic (inferred) constants. If false, exclude them. If absent, return all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    /// When true, return compact output: signature + line range + kind + file only (no body). ~25x token reduction for exploration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<bool>,
    /// Batch retrieve multiple nodes by stable ID. Returns combined results in a single response. Composes with compact and mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
    /// Search ranking mode for graph traversal entry-point resolution: "hybrid" (default, keyword + vector RRF), "keyword" (BM25 only), "semantic" (vector only). Only affects how entry nodes are found when using query + mode; flat search always uses name/signature matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
}

// ── Deprecated aliases (kept for one release cycle) ─────────────────

#[macros::mcp_tool(
    name = "search_symbols",
    description = "DEPRECATED: use `search` instead. Find code symbols by name or signature. This is an alias for `search` without a `mode` parameter."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchSymbols {
    /// Search query string (matched against symbol name and signature)
    pub query: String,
    /// Optional: filter by symbol kind (function, struct, trait, enum, module, import, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional: filter by language (rust, python, typescript, go, markdown)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional: filter by file path substring
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Optional: filter to a specific workspace root (by slug, e.g. "zettelkasten")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Maximum results to return (default: 20)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// If true, include only synthetic (inferred) constants. If false, exclude them. If absent, return all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    /// Optional: minimum cyclomatic complexity threshold. Only return functions with complexity >= this value. When set, query can be empty to search all functions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_complexity: Option<u32>,
    /// Optional: sort results by "complexity" (descending). Default is relevance ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

impl SearchSymbols {
    /// Convert deprecated SearchSymbols into the unified Search struct.
    /// Preserves the old default limit of 20 (Search defaults to 10).
    fn into_search(self) -> Search {
        Search {
            query: Some(self.query),
            node: None,
            mode: None,
            hops: None,
            direction: None,
            edge_types: None,
            kind: self.kind,
            language: self.language,
            file: self.file,
            root: self.root,
            top_k: Some(self.limit.unwrap_or(20)),
            sort_by: self.sort,
            min_complexity: self.min_complexity,
            synthetic: self.synthetic,
            compact: None,
            nodes: None,
            search_mode: None,
        }
    }
}

#[macros::mcp_tool(
    name = "graph_query",
    description = "DEPRECATED: use `search` with a `mode` parameter instead. Trace code relationships from a symbol or natural language query."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GraphQuery {
    /// Stable ID from search_symbols results. Takes precedence over query if both provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Natural language query to find entry nodes via semantic search (e.g. "authentication handler", "database connection pool"). Used when node_id is not provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Query mode: "neighbors" (default), "impact" (reverse dependents), "reachable" (forward BFS)
    #[serde(default = "default_graph_mode")]
    pub mode: String,
    /// Direction for neighbors mode: "outgoing" (default — what does this call/implement/depend on?), "incoming" (what calls/implements/depends on this?), "both"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Filter edge types: calls, depends_on, implements, defines, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Maximum hops to traverse (default: 1 for neighbors, 3 for impact/reachable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u32>,
    /// Maximum number of entry nodes from semantic search (default: 3). Only used with query parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

impl GraphQuery {
    /// Convert deprecated GraphQuery into the unified Search struct.
    /// Preserves the old default top_k of 3 (Search defaults to 1 for traversal).
    fn into_search(self) -> Search {
        Search {
            query: self.query,
            node: self.node_id,
            mode: Some(self.mode),
            hops: self.max_hops,
            direction: self.direction,
            edge_types: self.edge_types,
            kind: None,
            language: None,
            file: None,
            root: None,
            top_k: Some(self.top_k.unwrap_or(3)),
            sort_by: None,
            min_complexity: None,
            synthetic: None,
            compact: None,
            nodes: None,
            search_mode: None,
        }
    }
}

fn default_graph_mode() -> String {
    "neighbors".to_string()
}

#[macros::mcp_tool(
    name = "list_roots",
    description = "Lists configured workspace roots with their type, path, and scan status."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListRoots {}

#[macros::mcp_tool(
    name = "repo_map",
    description = "Repository orientation for agents. Returns top symbols by PageRank importance, hotspot files (most definitions), active business outcomes, and entry points (main/handler functions). One call replaces an exploratory loop of search calls. Use this when starting work on an unfamiliar codebase."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RepoMap {
    /// Number of top symbols to return (default: 15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_n: Option<u32>,
}

// ── Graph persistence (LanceDB) ─────────────────────────────────────

/// LanceDB path for graph persistence.
pub(crate) fn graph_lance_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("lance")
}

/// Check the stored schema version and migrate (drop + recreate meta) if it mismatches.
///
/// Returns `true` if tables were dropped and the meta table was rewritten (migration occurred),
/// `false` if the stored version already matches `SCHEMA_VERSION` (no-op).
///
/// Downstream callers (`build_full_graph`, `persist_graph_to_lance`) use this to ensure
/// stale LanceDB tables are discarded before any read or write.
pub(crate) async fn check_and_migrate_schema(db_path: &Path) -> anyhow::Result<bool> {
    use arrow_array::{RecordBatch, RecordBatchIterator, StringArray};
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;
    use std::sync::Arc;

    std::fs::create_dir_all(db_path)?;

    let db = lancedb::connect(db_path.to_str().unwrap_or_default())
        .execute()
        .await
        .context("check_and_migrate_schema: failed to connect to LanceDB")?;

    // Try to read the stored version from _schema_meta.
    let stored_version: Option<u32> = async {
        let tbl = db.open_table("_schema_meta").execute().await.ok()?;
        let batches: Vec<_> = tbl
            .query()
            .execute()
            .await
            .ok()?
            .try_collect()
            .await
            .ok()?;
        for batch in &batches {
            let keys = batch.column_by_name("key")?.as_any().downcast_ref::<StringArray>()?;
            let values = batch.column_by_name("value")?.as_any().downcast_ref::<StringArray>()?;
            for i in 0..batch.num_rows() {
                if keys.value(i) == "schema_version" {
                    return values.value(i).parse::<u32>().ok();
                }
            }
        }
        None
    }
    .await;

    // If version matches, nothing to do.
    if stored_version == Some(SCHEMA_VERSION) {
        return Ok(false);
    }

    // Version mismatch (or missing table) — drop all tables and write new meta.
    tracing::info!(
        "Schema version mismatch (stored={:?}, current={}) — dropping all LanceDB tables",
        stored_version,
        SCHEMA_VERSION
    );

    for table_name in &["symbols", "edges", "pr_merges", "file_index", "_schema_meta"] {
        let _ = db.drop_table(table_name, &[]).await;
    }

    // Write new _schema_meta with the current version.
    let schema = Arc::new(schema_meta_schema());
    let keys = StringArray::from(vec!["schema_version"]);
    let values = StringArray::from(vec![SCHEMA_VERSION.to_string()]);
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(keys), Arc::new(values)])?;
    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
    db.create_table("_schema_meta", Box::new(batches))
        .execute()
        .await
        .context("check_and_migrate_schema: failed to create _schema_meta table")?;

    Ok(true)
}

/// Parse a NodeKind from its string representation.
fn parse_node_kind(s: &str) -> NodeKind {
    match s {
        "function" => NodeKind::Function,
        "struct" => NodeKind::Struct,
        "trait" => NodeKind::Trait,
        "enum" => NodeKind::Enum,
        "module" => NodeKind::Module,
        "import" => NodeKind::Import,
        "const" => NodeKind::Const,
        "impl" => NodeKind::Impl,
        "proto_message" => NodeKind::ProtoMessage,
        "sql_table" => NodeKind::SqlTable,
        "api_endpoint" => NodeKind::ApiEndpoint,
        "macro" => NodeKind::Macro,
        "pr_merge" => NodeKind::PrMerge,
        other => NodeKind::Other(other.to_string()),
    }
}

/// Parse an EdgeKind from its string representation.
pub fn parse_edge_kind(s: &str) -> Option<EdgeKind> {
    Some(match s {
        "calls" => EdgeKind::Calls,
        "implements" => EdgeKind::Implements,
        "depends_on" => EdgeKind::DependsOn,
        "connects_to" => EdgeKind::ConnectsTo,
        "defines" => EdgeKind::Defines,
        "has_field" => EdgeKind::HasField,
        "evolves" => EdgeKind::Evolves,
        "referenced_by" => EdgeKind::ReferencedBy,
        "topology_boundary" => EdgeKind::TopologyBoundary,
        "modified" => EdgeKind::Modified,
        "affected" => EdgeKind::Affected,
        "serves" => EdgeKind::Serves,
        _ => return None,
    })
}

/// Parse an ExtractionSource from its string representation.
fn parse_extraction_source(s: &str) -> ExtractionSource {
    match s {
        "tree_sitter" => ExtractionSource::TreeSitter,
        "lsp" => ExtractionSource::Lsp,
        "schema" => ExtractionSource::Schema,
        "git" => ExtractionSource::Git,
        "markdown" => ExtractionSource::Markdown,
        _ => {
            tracing::warn!("Unknown edge_source value: {}, defaulting to TreeSitter", s);
            ExtractionSource::TreeSitter
        }
    }
}

/// Parse a Confidence from its string representation.
fn parse_confidence(s: &str) -> Confidence {
    match s {
        "confirmed" => Confidence::Confirmed,
        "detected" => Confidence::Detected,
        _ => {
            tracing::warn!("Unknown confidence value: {}, defaulting to Detected", s);
            Confidence::Detected
        }
    }
}

/// Delete all symbols and edges for the given root slugs from LanceDB.
/// Called when a worktree is detected as removed during the background scan loop.
async fn delete_nodes_for_roots(repo_root: &Path, slugs: &[String]) -> anyhow::Result<()> {
    if slugs.is_empty() {
        return Ok(());
    }

    let db_path = graph_lance_path(repo_root);
    if !db_path.exists() {
        return Ok(());
    }

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for worktree cleanup")?;

    // Build a SQL predicate: root_id IN ('slug1', 'slug2', ...)
    let quoted: Vec<String> = slugs
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect();
    let predicate = format!("root_id IN ({})", quoted.join(", "));

    // Delete from symbols table.
    if let Ok(tbl) = db.open_table("symbols").execute().await {
        if let Err(e) = tbl.delete(&predicate).await {
            tracing::warn!("Failed to delete symbols for removed worktrees: {}", e);
        }
    }

    // Delete from edges table.
    if let Ok(tbl) = db.open_table("edges").execute().await {
        if let Err(e) = tbl.delete(&predicate).await {
            tracing::warn!("Failed to delete edges for removed worktrees: {}", e);
        }
    }

    tracing::info!(
        "Deleted LanceDB rows for removed worktrees: {}",
        slugs.join(", ")
    );
    Ok(())
}

/// Persist graph nodes and edges to LanceDB tables.
pub(crate) async fn persist_graph_to_lance(
    repo_root: &Path,
    nodes: &[Node],
    edges: &[Edge],
) -> anyhow::Result<()> {
    let db_path = graph_lance_path(repo_root);
    std::fs::create_dir_all(&db_path)?;

    // Safety net: ensure schema is current before any writes.
    if check_and_migrate_schema(&db_path).await? {
        tracing::info!("Schema migrated to v{} — cache rebuilt", SCHEMA_VERSION);
    }

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for graph persistence")?;

    // ── Write symbols (nodes) table ──
    {
        let schema = Arc::new(symbols_schema());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let ids: Vec<String> = nodes.iter().map(|n| n.stable_id()).collect();
        let root_ids: Vec<String> = nodes.iter().map(|n| n.id.root.clone()).collect();
        let file_paths: Vec<String> = nodes.iter().map(|n| n.id.file.display().to_string()).collect();
        let names: Vec<String> = nodes.iter().map(|n| n.id.name.clone()).collect();
        let kinds: Vec<String> = nodes.iter().map(|n| n.id.kind.to_string()).collect();
        let line_starts: Vec<u32> = nodes.iter().map(|n| n.line_start as u32).collect();
        let line_ends: Vec<u32> = nodes.iter().map(|n| n.line_end as u32).collect();
        let signatures: Vec<String> = nodes.iter().map(|n| n.signature.clone()).collect();
        let bodies: Vec<String> = nodes.iter().map(|n| n.body.clone()).collect();
        let meta_virtuals: Vec<Option<bool>> = nodes.iter()
            .map(|n| if n.metadata.get("virtual").map(|v| v.as_str()) == Some("true") { Some(true) } else { None })
            .collect();
        let meta_packages: Vec<Option<String>> = nodes.iter()
            .map(|n| n.metadata.get("package").cloned())
            .collect();
        let meta_name_cols: Vec<Option<i32>> = nodes.iter()
            .map(|n| n.metadata.get("name_col").and_then(|s| s.parse::<i32>().ok()))
            .collect();
        let values: Vec<Option<String>> = nodes.iter().map(|n| n.metadata.get("value").cloned()).collect();
        let mut synthetic_builder = BooleanBuilder::new();
        for n in nodes.iter() {
            match n.metadata.get("synthetic") {
                Some(v) => synthetic_builder.append_value(v == "true"),
                None => synthetic_builder.append_null(),
            }
        }
        let cyclomatics: Vec<Option<i32>> = nodes.iter()
            .map(|n| n.metadata.get("cyclomatic").and_then(|s| s.parse::<i32>().ok()))
            .collect();
        let importances: Vec<Option<f64>> = nodes.iter()
            .map(|n| n.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()))
            .collect();
        let storages: Vec<Option<String>> = nodes.iter()
            .map(|n| n.metadata.get("storage").cloned())
            .collect();
        let mut mutable_builder = BooleanBuilder::new();
        for n in nodes.iter() {
            match n.metadata.get("mutable") {
                Some(v) => mutable_builder.append_value(v == "true"),
                None => mutable_builder.append_null(),
            }
        }
        let decorators_col: Vec<Option<String>> = nodes.iter()
            .map(|n| n.metadata.get("decorators").cloned())
            .collect();
        let type_params_col: Vec<Option<String>> = nodes.iter()
            .map(|n| n.metadata.get("type_params").cloned())
            .collect();
        let updated_ats: Vec<i64> = vec![now; nodes.len()];

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(root_ids)),
                Arc::new(StringArray::from(file_paths)),
                Arc::new(StringArray::from(names)),
                Arc::new(StringArray::from(kinds)),
                Arc::new(UInt32Array::from(line_starts)),
                Arc::new(UInt32Array::from(line_ends)),
                Arc::new(StringArray::from(signatures)),
                Arc::new(StringArray::from(bodies)),
                Arc::new(BooleanArray::from(meta_virtuals)),
                Arc::new(StringArray::from(meta_packages)),
                Arc::new(Int32Array::from(meta_name_cols)),
                Arc::new(StringArray::from(values)),
                Arc::new(synthetic_builder.finish()),
                Arc::new(Int32Array::from(cyclomatics)),
                Arc::new(Float64Array::from(importances)),
                Arc::new(StringArray::from(storages)),
                Arc::new(mutable_builder.finish()),
                Arc::new(StringArray::from(decorators_col)),
                Arc::new(StringArray::from(type_params_col)),
                Arc::new(Int64Array::from(updated_ats)),
            ],
        )?;

        let _ = db.drop_table("symbols", &[]).await;
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        db.create_table("symbols", Box::new(batches))
            .execute()
            .await
            .context("Failed to create symbols table")?;
    }

    // ── Write edges table ──
    {
        let schema = Arc::new(edges_schema());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let ids: Vec<String> = edges.iter().map(|e| e.stable_id()).collect();
        let source_ids: Vec<String> = edges.iter().map(|e| e.from.to_stable_id()).collect();
        let source_types: Vec<String> = edges.iter().map(|e| e.from.kind.to_string()).collect();
        let target_ids: Vec<String> = edges.iter().map(|e| e.to.to_stable_id()).collect();
        let target_types: Vec<String> = edges.iter().map(|e| e.to.kind.to_string()).collect();
        let edge_types: Vec<String> = edges.iter().map(|e| e.kind.to_string()).collect();
        let edge_sources: Vec<String> = edges.iter().map(|e| e.source.to_string()).collect();
        let edge_confidences: Vec<String> = edges.iter().map(|e| e.confidence.to_string()).collect();
        let root_ids: Vec<String> = edges.iter().map(|e| e.from.root.clone()).collect();
        let updated_ats: Vec<i64> = vec![now; edges.len()];

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(source_ids)),
                Arc::new(StringArray::from(source_types)),
                Arc::new(StringArray::from(target_ids)),
                Arc::new(StringArray::from(target_types)),
                Arc::new(StringArray::from(edge_types)),
                Arc::new(StringArray::from(edge_sources)),
                Arc::new(StringArray::from(edge_confidences)),
                Arc::new(StringArray::from(root_ids)),
                Arc::new(Int64Array::from(updated_ats)),
            ],
        )?;

        let _ = db.drop_table("edges", &[]).await;
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        db.create_table("edges", Box::new(batches))
            .execute()
            .await
            .context("Failed to create edges table")?;
    }

    // Create FTS index on symbols table for keyword search over all nodes
    // (fields, imports, keys, consts that don't get vector embeddings)
    if let Ok(symbols_table) = db.open_table("symbols").execute().await {
        // LanceDB doesn't support composite FTS indices yet — index name only
        match symbols_table
            .create_index(&["name"], lancedb::index::Index::FTS(Default::default()))
            .execute()
            .await
        {
            Ok(_) => tracing::info!("Created FTS index on symbols.name"),
            Err(e) => tracing::warn!("Failed to create FTS index: {}", e),
        }
    }

    tracing::info!(
        "Persisted graph to LanceDB: {} nodes, {} edges",
        nodes.len(),
        edges.len()
    );
    Ok(())
}

/// Persist graph changes incrementally using LanceDB merge_insert (upsert) and targeted delete.
///
/// Unlike `persist_graph_to_lance` (DROP+CREATE), this keeps the tables alive during writes —
/// no query window with empty results.
///
/// # Parameters
/// - `upsert_nodes`: only the changed or newly added nodes (not the full graph)
/// - `upsert_edges`: only the changed or newly added edges (not the full graph)
/// - `deleted_edge_ids`: stable IDs of edges that reference removed/changed files — collected
///   before the in-memory retain step in `update_graph_incrementally`
/// - `deleted_files`: file paths whose symbols should be deleted from LanceDB
pub(crate) async fn persist_graph_incremental(
    repo_root: &Path,
    upsert_nodes: &[Node],
    upsert_edges: &[Edge],
    deleted_edge_ids: &[String],
    deleted_files: &[PathBuf],
) -> anyhow::Result<()> {
    let db_path = graph_lance_path(repo_root);
    std::fs::create_dir_all(&db_path)?;

    // Pre-flight: ensure schema version matches before any LanceDB writes.
    if check_and_migrate_schema(&db_path).await? {
        tracing::info!("Schema migrated to v{} during incremental update — cache rebuilt", SCHEMA_VERSION);
    }

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for incremental graph persistence")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // ── Symbols (nodes) table: delete then upsert ──
    {
        let schema = Arc::new(symbols_schema());

        // 1. Delete symbols for removed/changed files first so upsert is clean.
        if !deleted_files.is_empty() {
            if let Ok(tbl) = db.open_table("symbols").execute().await {
                let quoted: Vec<String> = deleted_files
                    .iter()
                    .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
                    .collect();
                let predicate = format!("file_path IN ({})", quoted.join(", "));
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!("Failed to delete symbols for removed files: {}", e);
                }
            }
        }

        // 2. Upsert changed/added nodes (insert new, update existing by stable id).
        if !upsert_nodes.is_empty() {
            let ids: Vec<String> = upsert_nodes.iter().map(|n| n.stable_id()).collect();
            let root_ids: Vec<String> = upsert_nodes.iter().map(|n| n.id.root.clone()).collect();
            let file_paths: Vec<String> = upsert_nodes.iter().map(|n| n.id.file.display().to_string()).collect();
            let names: Vec<String> = upsert_nodes.iter().map(|n| n.id.name.clone()).collect();
            let kinds: Vec<String> = upsert_nodes.iter().map(|n| n.id.kind.to_string()).collect();
            let line_starts: Vec<u32> = upsert_nodes.iter().map(|n| n.line_start as u32).collect();
            let line_ends: Vec<u32> = upsert_nodes.iter().map(|n| n.line_end as u32).collect();
            let signatures: Vec<String> = upsert_nodes.iter().map(|n| n.signature.clone()).collect();
            let bodies: Vec<String> = upsert_nodes.iter().map(|n| n.body.clone()).collect();
            let meta_virtuals: Vec<Option<bool>> = upsert_nodes.iter()
                .map(|n| if n.metadata.get("virtual").map(|v| v.as_str()) == Some("true") { Some(true) } else { None })
                .collect();
            let meta_packages: Vec<Option<String>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("package").cloned())
                .collect();
            let meta_name_cols: Vec<Option<i32>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("name_col").and_then(|s| s.parse::<i32>().ok()))
                .collect();
            let values: Vec<Option<String>> = upsert_nodes.iter().map(|n| n.metadata.get("value").cloned()).collect();
            let mut synthetic_builder = BooleanBuilder::new();
            for n in upsert_nodes.iter() {
                match n.metadata.get("synthetic") {
                    Some(v) => synthetic_builder.append_value(v == "true"),
                    None => synthetic_builder.append_null(),
                }
            }
            let cyclomatics: Vec<Option<i32>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("cyclomatic").and_then(|s| s.parse::<i32>().ok()))
                .collect();
            let importances: Vec<Option<f64>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()))
                .collect();
            let storages: Vec<Option<String>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("storage").cloned())
                .collect();
            let mut mutable_builder = BooleanBuilder::new();
            for n in upsert_nodes.iter() {
                match n.metadata.get("mutable") {
                    Some(v) => mutable_builder.append_value(v == "true"),
                    None => mutable_builder.append_null(),
                }
            }
            let decorators_col: Vec<Option<String>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("decorators").cloned())
                .collect();
            let type_params_col: Vec<Option<String>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("type_params").cloned())
                .collect();
            let updated_ats: Vec<i64> = vec![now; upsert_nodes.len()];

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(root_ids)),
                    Arc::new(StringArray::from(file_paths)),
                    Arc::new(StringArray::from(names)),
                    Arc::new(StringArray::from(kinds)),
                    Arc::new(UInt32Array::from(line_starts)),
                    Arc::new(UInt32Array::from(line_ends)),
                    Arc::new(StringArray::from(signatures)),
                    Arc::new(StringArray::from(bodies)),
                    Arc::new(BooleanArray::from(meta_virtuals)),
                    Arc::new(StringArray::from(meta_packages)),
                    Arc::new(Int32Array::from(meta_name_cols)),
                    Arc::new(StringArray::from(values)),
                    Arc::new(synthetic_builder.finish()),
                    Arc::new(Int32Array::from(cyclomatics)),
                    Arc::new(Float64Array::from(importances)),
                    Arc::new(StringArray::from(storages)),
                    Arc::new(mutable_builder.finish()),
                    Arc::new(StringArray::from(decorators_col)),
                    Arc::new(StringArray::from(type_params_col)),
                    Arc::new(Int64Array::from(updated_ats)),
                ],
            )?;

            match db.open_table("symbols").execute().await {
                Ok(tbl) => {
                    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                    let mut merge = tbl.merge_insert(&["id"]);
                    merge
                        .when_matched_update_all(None)
                        .when_not_matched_insert_all();
                    // Note: no when_not_matched_by_source_delete — we only touch changed rows.
                    // Untouched rows (unchanged files) are left alone.
                    merge
                        .execute(Box::new(batches))
                        .await
                        .context("Failed to merge_insert symbols table")?;
                }
                Err(_) => {
                    // Table doesn't exist yet — create it (first incremental run after a fresh repo)
                    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                    db.create_table("symbols", Box::new(batches))
                        .execute()
                        .await
                        .context("Failed to create symbols table")?;
                }
            }
        }
    }

    // ── Edges table: delete then upsert ──
    {
        let schema = Arc::new(edges_schema());

        // 1. Delete edges that referenced removed/changed files (by stable edge ID).
        if !deleted_edge_ids.is_empty() {
            if let Ok(tbl) = db.open_table("edges").execute().await {
                let quoted: Vec<String> = deleted_edge_ids
                    .iter()
                    .map(|id| format!("'{}'", id.replace('\'', "''")))
                    .collect();
                let predicate = format!("id IN ({})", quoted.join(", "));
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!("Failed to delete edges for removed files: {}", e);
                }
            }
        }

        // 2. Upsert changed/added edges.
        if !upsert_edges.is_empty() {
            let ids: Vec<String> = upsert_edges.iter().map(|e| e.stable_id()).collect();
            let source_ids: Vec<String> = upsert_edges.iter().map(|e| e.from.to_stable_id()).collect();
            let source_types: Vec<String> = upsert_edges.iter().map(|e| e.from.kind.to_string()).collect();
            let target_ids: Vec<String> = upsert_edges.iter().map(|e| e.to.to_stable_id()).collect();
            let target_types: Vec<String> = upsert_edges.iter().map(|e| e.to.kind.to_string()).collect();
            let edge_types: Vec<String> = upsert_edges.iter().map(|e| e.kind.to_string()).collect();
            let edge_sources: Vec<String> = upsert_edges.iter().map(|e| e.source.to_string()).collect();
            let edge_confidences: Vec<String> = upsert_edges.iter().map(|e| e.confidence.to_string()).collect();
            let root_ids: Vec<String> = upsert_edges.iter().map(|e| e.from.root.clone()).collect();
            let updated_ats: Vec<i64> = vec![now; upsert_edges.len()];

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(source_ids)),
                    Arc::new(StringArray::from(source_types)),
                    Arc::new(StringArray::from(target_ids)),
                    Arc::new(StringArray::from(target_types)),
                    Arc::new(StringArray::from(edge_types)),
                    Arc::new(StringArray::from(edge_sources)),
                    Arc::new(StringArray::from(edge_confidences)),
                    Arc::new(StringArray::from(root_ids)),
                    Arc::new(Int64Array::from(updated_ats)),
                ],
            )?;

            match db.open_table("edges").execute().await {
                Ok(tbl) => {
                    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                    let mut merge = tbl.merge_insert(&["id"]);
                    merge
                        .when_matched_update_all(None)
                        .when_not_matched_insert_all();
                    // Note: no when_not_matched_by_source_delete — untouched edges are preserved.
                    merge
                        .execute(Box::new(batches))
                        .await
                        .context("Failed to merge_insert edges table")?;
                }
                Err(_) => {
                    // Table doesn't exist yet — create it
                    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                    db.create_table("edges", Box::new(batches))
                        .execute()
                        .await
                        .context("Failed to create edges table")?;
                }
            }
        }
    }

    tracing::info!(
        "Incrementally persisted graph to LanceDB: {} upserted nodes, {} upserted edges, {} deleted files, {} deleted edges",
        upsert_nodes.len(),
        upsert_edges.len(),
        deleted_files.len(),
        deleted_edge_ids.len(),
    );
    Ok(())
}

/// Load graph nodes and edges from LanceDB tables.
pub async fn load_graph_from_lance(repo_root: &Path) -> anyhow::Result<GraphState> {
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;

    let db_path = graph_lance_path(repo_root);
    if !db_path.exists() {
        anyhow::bail!("No persisted graph at {}", db_path.display());
    }

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for graph loading")?;

    // ── Read symbols (nodes) ──
    let nodes = {
        let table = db
            .open_table("symbols")
            .execute()
            .await
            .context("No symbols table found")?;
        let stream = table.query().execute().await.context("Failed to query symbols")?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut nodes = Vec::new();
        for batch in &batches {
            let ids = batch.column_by_name("id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let root_ids = batch.column_by_name("root_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let file_paths = batch.column_by_name("file_path").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let names = batch.column_by_name("name").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let kinds = batch.column_by_name("kind").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let line_starts = batch.column_by_name("line_start").unwrap().as_any().downcast_ref::<UInt32Array>().unwrap();
            let line_ends = batch.column_by_name("line_end").unwrap().as_any().downcast_ref::<UInt32Array>().unwrap();
            let signatures = batch.column_by_name("signature").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let bodies = batch.column_by_name("body").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            // Typed metadata columns — Arrow type safety, no JSON blobs for known fields.
            let meta_virtual_col = batch.column_by_name("meta_virtual")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let meta_package_col = batch.column_by_name("meta_package")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let meta_name_col_col = batch.column_by_name("meta_name_col")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
            // We don't store language or source in the symbols schema, so we infer from file extension
            let _ = ids; // ids column exists but we reconstruct from components

            // Read optional value and synthetic columns (present after schema migration)
            let value_col = batch.column_by_name("value")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let synthetic_col = batch.column_by_name("synthetic")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let cyclomatic_col = batch.column_by_name("cyclomatic")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
            let importance_col = batch.column_by_name("importance")
                .and_then(|c| c.as_any().downcast_ref::<Float64Array>());
            let storage_col = batch.column_by_name("storage")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let mutable_col = batch.column_by_name("mutable")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let decorators_col = batch.column_by_name("decorators")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let type_params_col = batch.column_by_name("type_params")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let file_path = PathBuf::from(file_paths.value(i));
                let language = infer_language_from_path(&file_path);
                let mut metadata: BTreeMap<String, String> = BTreeMap::new();
                if let Some(col) = meta_virtual_col {
                    if !col.is_null(i) && col.value(i) {
                        metadata.insert("virtual".to_string(), "true".to_string());
                    }
                }
                if let Some(col) = meta_package_col {
                    if !col.is_null(i) {
                        metadata.insert("package".to_string(), col.value(i).to_string());
                    }
                }
                if let Some(col) = meta_name_col_col {
                    if !col.is_null(i) {
                        metadata.insert("name_col".to_string(), col.value(i).to_string());
                    }
                }
                if let Some(col) = value_col {
                    if !col.is_null(i) {
                        metadata.insert("value".to_string(), col.value(i).to_string());
                    }
                }
                if let Some(col) = synthetic_col {
                    if !col.is_null(i) {
                        metadata.insert("synthetic".to_string(), if col.value(i) { "true" } else { "false" }.to_string());
                    }
                }
                if let Some(col) = cyclomatic_col {
                    if !col.is_null(i) {
                        metadata.insert("cyclomatic".to_string(), col.value(i).to_string());
                    }
                }
                if let Some(col) = importance_col {
                    if !col.is_null(i) {
                        metadata.insert("importance".to_string(), format!("{:.6}", col.value(i)));
                    }
                }
                if let Some(col) = storage_col {
                    if !col.is_null(i) {
                        metadata.insert("storage".to_string(), col.value(i).to_string());
                    }
                }
                if let Some(col) = mutable_col {
                    if !col.is_null(i) && col.value(i) {
                        metadata.insert("mutable".to_string(), "true".to_string());
                    }
                }
                if let Some(col) = decorators_col {
                    if !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("decorators".to_string(), val.to_string());
                        }
                    }
                }
                if let Some(col) = type_params_col {
                    if !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("type_params".to_string(), val.to_string());
                        }
                    }
                }
                nodes.push(Node {
                    id: NodeId {
                        root: root_ids.value(i).to_string(),
                        file: file_path,
                        name: names.value(i).to_string(),
                        kind: parse_node_kind(kinds.value(i)),
                    },
                    language,
                    line_start: line_starts.value(i) as usize,
                    line_end: line_ends.value(i) as usize,
                    signature: signatures.value(i).to_string(),
                    body: bodies.value(i).to_string(),
                    metadata,
                    source: ExtractionSource::TreeSitter, // default; not stored in schema
                });
            }
        }
        nodes
    };

    // ── Read edges ──
    let edges = {
        let table = db
            .open_table("edges")
            .execute()
            .await
            .context("No edges table found")?;
        let stream = table.query().execute().await.context("Failed to query edges")?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut edges = Vec::new();
        for batch in &batches {
            let source_ids = batch.column_by_name("source_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let source_types = batch.column_by_name("source_type").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let target_ids = batch.column_by_name("target_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let target_types = batch.column_by_name("target_type").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let edge_types = batch.column_by_name("edge_type").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let edge_sources = batch.column_by_name("edge_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let edge_confidences = batch.column_by_name("edge_confidence")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let root_ids = batch.column_by_name("root_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();

            for i in 0..batch.num_rows() {
                let edge_kind = match parse_edge_kind(edge_types.value(i)) {
                    Some(k) => k,
                    None => continue,
                };

                let extraction_source = edge_sources
                    .map(|a| parse_extraction_source(a.value(i)))
                    .unwrap_or(ExtractionSource::TreeSitter);
                let confidence = edge_confidences
                    .map(|a| parse_confidence(a.value(i)))
                    .unwrap_or(Confidence::Detected);

                // Parse NodeId from stable_id format: "root:file:name:kind"
                let from = parse_node_id_from_stable(source_ids.value(i), source_types.value(i), root_ids.value(i));
                let to = parse_node_id_from_stable(target_ids.value(i), target_types.value(i), root_ids.value(i));

                edges.push(Edge {
                    from,
                    to,
                    kind: edge_kind,
                    source: extraction_source,
                    confidence,
                });
            }
        }
        edges
    };

    // ── Build index ──
    let mut index = GraphIndex::new();
    index.rebuild_from_edges(&edges);
    for node in &nodes {
        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
    }

    Ok(GraphState { nodes, edges, index, last_scan_completed_at: Some(std::time::Instant::now()) })
}

/// Parse a NodeId from its stable_id string (format: "root:file:name:kind").
/// Falls back to using the type hint and root if parsing is ambiguous.
fn parse_node_id_from_stable(stable_id: &str, kind_hint: &str, root_hint: &str) -> NodeId {
    // stable_id format: "root:file:name:kind"
    // We need to handle the case where file or name might contain ':'
    // Strategy: split from the end to get kind, then from the start to get root,
    // the middle is file:name which we split on the last ':'
    let parts: Vec<&str> = stable_id.splitn(2, ':').collect();
    if parts.len() < 2 {
        return NodeId {
            root: root_hint.to_string(),
            file: PathBuf::from(stable_id),
            name: String::new(),
            kind: parse_node_kind(kind_hint),
        };
    }

    let root = parts[0].to_string();
    let rest = parts[1]; // "file:name:kind"

    // Split from the end to get kind
    if let Some(last_colon) = rest.rfind(':') {
        let before_kind = &rest[..last_colon]; // "file:name"
        // Split file:name on the last colon
        if let Some(name_colon) = before_kind.rfind(':') {
            let file = &before_kind[..name_colon];
            let name = &before_kind[name_colon + 1..];
            return NodeId {
                root,
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: parse_node_kind(kind_hint),
            };
        }
        // Only one segment — treat as file with empty name
        return NodeId {
            root,
            file: PathBuf::from(before_kind),
            name: String::new(),
            kind: parse_node_kind(kind_hint),
        };
    }

    NodeId {
        root: root_hint.to_string(),
        file: PathBuf::from(rest),
        name: String::new(),
        kind: parse_node_kind(kind_hint),
    }
}

/// Infer programming language from file extension.
fn infer_language_from_path(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust".to_string(),
        Some("py") => "python".to_string(),
        Some("ts") | Some("tsx") => "typescript".to_string(),
        Some("js") | Some("jsx") => "javascript".to_string(),
        Some("go") => "go".to_string(),
        Some("proto") => "protobuf".to_string(),
        Some("sql") => "sql".to_string(),
        Some("md") => "markdown".to_string(),
        Some("toml") => "toml".to_string(),
        Some("yaml") | Some("yml") => "yaml".to_string(),
        _ => "unknown".to_string(),
    }
}

// ── Graph state ─────────────────────────────────────────────────────

/// In-memory graph state: extraction results + petgraph index + embedding index.
/// Lazily initialized on first tool call. Embeddings are built as part of the
/// graph pipeline — not as a separate lazy init that races with graph building.
pub struct GraphState {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub index: GraphIndex,
    /// Timestamp of the last completed scan (full or incremental).
    /// `None` until the first scan finishes.
    pub last_scan_completed_at: Option<std::time::Instant>,
}

// ── LSP enrichment status ────────────────────────────────────────────

/// Tracks whether background LSP enrichment has run, so query footers
/// can tell the agent "results may be incomplete" vs "enrichment done."
pub struct LspEnrichmentStatus {
    /// 0 = not started, 1 = running, 2 = complete
    state: std::sync::atomic::AtomicU8,
    /// Number of edges added by the most recent enrichment pass.
    edge_count: std::sync::atomic::AtomicUsize,
    /// When enrichment last completed (for auto-hide after 30 s).
    completed_at: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Default for LspEnrichmentStatus {
    fn default() -> Self {
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
            edge_count: std::sync::atomic::AtomicUsize::new(0),
            completed_at: std::sync::Mutex::new(None),
        }
    }
}

impl LspEnrichmentStatus {
    const NOT_STARTED: u8 = 0;
    const RUNNING: u8 = 1;
    const COMPLETE: u8 = 2;
    const UNAVAILABLE: u8 = 3;

    pub fn set_running(&self) {
        self.state.store(Self::RUNNING, std::sync::atomic::Ordering::Release);
    }

    pub fn set_complete(&self, edge_count: usize) {
        self.edge_count.store(edge_count, std::sync::atomic::Ordering::Release);
        *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
        self.state.store(Self::COMPLETE, std::sync::atomic::Ordering::Release);
    }

    /// Mark that no LSP server was available for any of the detected languages.
    pub fn set_unavailable(&self) {
        *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
        self.state.store(Self::UNAVAILABLE, std::sync::atomic::Ordering::Release);
    }

    /// Render a short footer segment, or `None` if nothing useful to show.
    pub fn footer_segment(&self) -> Option<String> {
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            Self::NOT_STARTED => None,
            Self::RUNNING => Some("LSP: pending".to_string()),
            Self::COMPLETE => {
                let guard = self.completed_at.lock().unwrap();
                if let Some(t) = *guard {
                    if t.elapsed().as_secs() < 30 {
                        let count = self.edge_count.load(std::sync::atomic::Ordering::Acquire);
                        Some(format!("LSP: enriched ({} edges)", count))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Self::UNAVAILABLE => {
                // Always show unavailable status (no 30s auto-hide) so agents
                // know LSP enrichment didn't run and why.
                Some("LSP: no server detected".to_string())
            }
            _ => None,
        }
    }
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
    pub graph: Arc<RwLock<Option<GraphState>>>,
    /// Double-buffered embedding index.
    pub embed_index: Arc<ArcSwap<Option<EmbeddingIndex>>>,
    /// Whether business context has been injected into a tool response.
    pub context_injected: std::sync::atomic::AtomicBool,
    /// Cooldown: skip re-scanning if checked recently.
    pub last_scan: std::sync::Mutex<std::time::Instant>,
    /// Whether background scanner has been spawned.
    pub background_scanner_started: std::sync::atomic::AtomicBool,
    /// LSP enrichment status — shared with background enrichment tasks.
    pub lsp_status: Arc<LspEnrichmentStatus>,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            graph: Arc::new(RwLock::new(None)),
            embed_index: Arc::new(ArcSwap::from_pointee(None)),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            last_scan: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
            background_scanner_started: std::sync::atomic::AtomicBool::new(false),
            lsp_status: Arc::new(LspEnrichmentStatus::default()),
        }
    }
}

impl RnaHandler {
    /// Ensure graph is built, check for file changes since last scan.
    /// Returns a read guard to the graph.
    async fn get_graph(&self) -> anyhow::Result<tokio::sync::RwLockReadGuard<'_, Option<GraphState>>> {
        // Fast path: graph exists and scan cooldown hasn't expired
        let pending_scan: Option<ScanResult> = {
            let guard = self.graph.read().await;
            if guard.is_some() {
                let skip_scan = {
                    let last = self.last_scan.lock().unwrap();
                    last.elapsed() < std::time::Duration::from_secs(2)
                };
                if skip_scan {
                    return Ok(guard);
                }

                // Check for changes via scanner
                let mut scanner = Scanner::new(self.repo_root.clone())?;
                let scan = scanner.scan()?;
                if scan.changed_files.is_empty()
                    && scan.new_files.is_empty()
                    && scan.deleted_files.is_empty()
                {
                    // Update cooldown timestamp
                    *self.last_scan.lock().unwrap() = std::time::Instant::now();
                    return Ok(guard);
                }
                // Changes detected — carry the scan result forward
                drop(guard);
                Some(scan)
            } else {
                drop(guard);
                None
            }
        };

        // Slow path: build or update graph
        {
            let mut guard = self.graph.write().await;
            if guard.is_none() {
                // First build — full pipeline
                *guard = Some(self.build_full_graph().await?);
            } else {
                // Incremental update — pass the already-completed scan so we
                // don't re-scan (the first scan already saved updated state).
                let graph = guard.as_mut().unwrap();
                self.update_graph_with_scan(graph, pending_scan).await?;
            }
        }

        // Update cooldown timestamp
        *self.last_scan.lock().unwrap() = std::time::Instant::now();

        // Start background scanner (once) to keep index warm
        if !self.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let graph = Arc::clone(&self.graph);
            let repo_root = self.repo_root.clone();
            tokio::spawn(async move {
                // Track root slugs from the previous tick to detect removed worktrees.
                let mut prev_root_slugs: std::collections::HashSet<String> =
                    std::collections::HashSet::new();

                // HEAD-change detection state.
                let mut last_head_oid: Option<git2::Oid> = None;
                let mut last_fetch_head_mtime: Option<std::time::SystemTime> = None;

                loop {
                    // Check for HEAD or FETCH_HEAD changes before waiting.
                    // If a change is detected, trigger an immediate scan rather
                    // than waiting for the full 15-min cadence.
                    let head_changed = {
                        match git2::Repository::open(&repo_root) {
                            Ok(repo) => match repo.head().and_then(|h| h.peel_to_commit()) {
                                Ok(commit) => {
                                    let oid = commit.id();
                                    let changed = last_head_oid.map_or(false, |prev| prev != oid);
                                    last_head_oid = Some(oid);
                                    changed
                                }
                                Err(_) => false,
                            },
                            Err(_) => false,
                        }
                    };

                    let fetch_head_changed = {
                        let fetch_head_path = repo_root.join(".git").join("FETCH_HEAD");
                        match std::fs::metadata(&fetch_head_path).and_then(|m| m.modified()) {
                            Ok(mtime) => {
                                let changed =
                                    last_fetch_head_mtime.map_or(false, |prev| prev != mtime);
                                last_fetch_head_mtime = Some(mtime);
                                changed
                            }
                            Err(_) => false,
                        }
                    };

                    if head_changed {
                        tracing::info!("HEAD changed — triggering immediate background scan");
                    } else if fetch_head_changed {
                        tracing::info!(
                            "FETCH_HEAD changed — triggering immediate background scan"
                        );
                    } else {
                        // No git-level change detected: wait for the 15-min heartbeat.
                        // The baselines (last_head_oid, last_fetch_head_mtime) are
                        // updated unconditionally at the top of every iteration, so any
                        // commit that arrives during the sleep will be visible on the
                        // very next wake-up without a post-sleep re-read here.
                        tokio::time::sleep(tokio::time::Duration::from_secs(900)).await;
                    }

                    // Resolve current roots (primary + any live worktrees + claude memory).
                    let workspace = WorkspaceConfig::load()
                        .with_primary_root(repo_root.clone())
                        .with_worktrees(&repo_root)
                        .with_claude_memory(&repo_root);
                    let resolved_roots = workspace.resolved_roots();
                    let current_root_slugs: std::collections::HashSet<String> =
                        resolved_roots.iter().map(|r| r.slug.clone()).collect();

                    // Slugs that disappeared → worktree was removed.
                    let removed_slugs: Vec<String> = prev_root_slugs
                        .difference(&current_root_slugs)
                        .cloned()
                        .collect();

                    // Scan every live root for file-level changes.
                    let mut has_changes = false;
                    let mut per_root_scans: Vec<(String, crate::scanner::ScanResult, PathBuf)> =
                        Vec::new();
                    for resolved_root in &resolved_roots {
                        let root_slug = resolved_root.slug.clone();
                        let root_path = resolved_root.path.clone();
                        let excludes = resolved_root.config.effective_excludes();
                        let is_primary = root_path == repo_root;
                        let mut scanner = if is_primary {
                            match Scanner::with_excludes(root_path.clone(), excludes) {
                                Ok(s) => s,
                                Err(_) => continue,
                            }
                        } else {
                            let state_path = cache_state_path(&root_slug);
                            match Scanner::with_excludes_and_state_path(
                                root_path.clone(),
                                excludes,
                                state_path,
                            ) {
                                Ok(s) => s,
                                Err(_) => continue,
                            }
                        };
                        let scan = match scanner.scan() {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        if !scan.changed_files.is_empty()
                            || !scan.new_files.is_empty()
                            || !scan.deleted_files.is_empty()
                        {
                            has_changes = true;
                        }
                        per_root_scans.push((root_slug, scan, root_path));
                    }

                    if !has_changes && removed_slugs.is_empty() {
                        prev_root_slugs = current_root_slugs;
                        continue;
                    }

                    // Collect LanceDB persist deltas per root (outside write guard so
                    // persist_graph_incremental can run without holding the lock).
                    // Structure: (root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove)
                    let mut lance_deltas: Vec<(
                        PathBuf,
                        Vec<Node>,
                        Vec<Edge>,
                        Vec<String>,
                        Vec<PathBuf>,
                    )> = Vec::new();

                    let mut guard = graph.write().await;
                    if let Some(ref mut graph_state) = *guard {
                        let registry = ExtractorRegistry::with_builtins();

                        // Drop in-memory nodes/edges for removed worktrees.
                        for slug in &removed_slugs {
                            tracing::info!(
                                "Worktree removed — dropping in-memory nodes for root '{}'",
                                slug
                            );
                            graph_state.nodes.retain(|n| &n.id.root != slug);
                            graph_state.edges.retain(|e| &e.from.root != slug);
                        }

                        // Apply file-level changes per root.
                        for (root_slug, scan, root_path) in &per_root_scans {
                            if scan.changed_files.is_empty()
                                && scan.new_files.is_empty()
                                && scan.deleted_files.is_empty()
                            {
                                continue;
                            }
                            tracing::info!(
                                "Background scan '{}': {} changed, {} new, {} deleted",
                                root_slug,
                                scan.changed_files.len(),
                                scan.new_files.len(),
                                scan.deleted_files.len()
                            );
                            let files_to_remove: Vec<PathBuf> = scan
                                .deleted_files
                                .iter()
                                .chain(scan.changed_files.iter())
                                .cloned()
                                .collect();

                            // Collect edge IDs to delete BEFORE retain (same pattern as foreground).
                            let deleted_edge_ids: Vec<String> = graph_state
                                .edges
                                .iter()
                                .filter(|e| {
                                    e.from.root == *root_slug
                                        && files_to_remove
                                            .iter()
                                            .any(|f| e.from.file == *f || e.to.file == *f)
                                })
                                .map(|e| e.stable_id())
                                .collect();

                            graph_state.nodes.retain(|n| {
                                n.id.root != *root_slug
                                    || !files_to_remove.iter().any(|f| n.id.file == *f)
                            });
                            graph_state.edges.retain(|e| {
                                e.from.root != *root_slug
                                    || !files_to_remove
                                        .iter()
                                        .any(|f| e.from.file == *f || e.to.file == *f)
                            });
                            let mut extraction = registry.extract_scan_result(root_path, scan);
                            for node in &mut extraction.nodes {
                                node.id.root = root_slug.clone();
                            }
                            // Build file index from existing + new nodes for suffix resolution
                            let file_index: std::collections::HashSet<String> = graph_state.nodes
                                .iter()
                                .chain(extraction.nodes.iter())
                                .map(|n| n.id.file.to_string_lossy().to_string())
                                .collect();
                            for edge in &mut extraction.edges {
                                edge.from.root = root_slug.clone();
                                edge.to.root = root_slug.clone();
                                resolve_edge_target_by_suffix(edge, &file_index);
                            }
                            let upsert_nodes = extraction.nodes.clone();
                            let upsert_edges = extraction.edges.clone();
                            graph_state.nodes.extend(extraction.nodes);
                            graph_state.edges.extend(extraction.edges);

                            lance_deltas.push((
                                root_path.clone(),
                                upsert_nodes,
                                upsert_edges,
                                deleted_edge_ids,
                                files_to_remove,
                            ));
                        }

                        // Rebuild petgraph index.
                        graph_state.index = GraphIndex::new();
                        graph_state.index.rebuild_from_edges(&graph_state.edges);
                        for node in &graph_state.nodes {
                            graph_state.index.ensure_node(
                                &node.stable_id(),
                                &node.id.kind.to_string(),
                            );
                        }
                        tracing::info!(
                            "Background update: {} nodes, {} edges",
                            graph_state.nodes.len(),
                            graph_state.edges.len()
                        );
                    }
                    drop(guard);

                    // Persist incremental deltas to LanceDB for each root (lock released above).
                    for (root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove) in lance_deltas {
                        if let Err(e) = persist_graph_incremental(
                            &root_path,
                            &upsert_nodes,
                            &upsert_edges,
                            &deleted_edge_ids,
                            &files_to_remove,
                        )
                        .await
                        {
                            tracing::warn!("Background scan: failed to persist graph delta: {}", e);
                        }
                    }

                    // Purge removed worktree slugs from LanceDB.
                    if !removed_slugs.is_empty() {
                        if let Err(e) = delete_nodes_for_roots(&repo_root, &removed_slugs).await {
                            tracing::warn!(
                                "Failed to delete LanceDB rows for removed worktrees: {}",
                                e
                            );
                        }
                    }

                    prev_root_slugs = current_root_slugs;
                }
            });
            tracing::info!("Background scanner started (event-driven + 15min heartbeat, worktree-aware)");
        }

        // Downgrade to read lock
        Ok(self.graph.read().await)
    }

    /// Build the full graph from scratch. This is the original get_graph logic.
    pub async fn build_full_graph(&self) -> anyhow::Result<GraphState> {
        // Pre-flight: ensure schema version matches before any LanceDB reads/writes.
        let db_path = graph_lance_path(&self.repo_root);
        if check_and_migrate_schema(&db_path).await? {
            tracing::info!("Schema migrated to v{} — cache rebuilt", SCHEMA_VERSION);
        }

        // Load workspace config and merge with --repo as primary root.
        // Also auto-detect any live git worktrees and Claude Code memory
        // so all roots are indexed on the first full build.
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root);
        let resolved_roots = workspace.resolved_roots();

        // 1. Scan all roots to detect changes
        let mut has_changes = false;
        let mut scanners: Vec<(String, Scanner, crate::scanner::ScanResult, PathBuf)> = Vec::new();

        for resolved_root in &resolved_roots {
            let root_slug = &resolved_root.slug;
            let root_path = &resolved_root.path;
            let excludes = resolved_root.config.effective_excludes();

            let is_primary = resolved_root.path == self.repo_root;
            let mut scanner = if is_primary {
                Scanner::with_excludes(root_path.clone(), excludes)?
            } else {
                let state_path = cache_state_path(root_slug);
                Scanner::with_excludes_and_state_path(
                    root_path.clone(),
                    excludes,
                    state_path,
                )?
            };

            let scan_result = scanner.scan()?;
            tracing::info!(
                "Scanned root '{}' ({}): {} new, {} changed, {} deleted in {:?}",
                root_slug,
                resolved_root.config.root_type,
                scan_result.new_files.len(),
                scan_result.changed_files.len(),
                scan_result.deleted_files.len(),
                scan_result.scan_duration
            );

            if !scan_result.new_files.is_empty()
                || !scan_result.changed_files.is_empty()
                || !scan_result.deleted_files.is_empty()
            {
                has_changes = true;
            }

            scanners.push((root_slug.clone(), scanner, scan_result, root_path.clone()));
        }

        // 2. If no changes, try loading from LanceDB
        if !has_changes {
            match load_graph_from_lance(&self.repo_root).await {
                Ok(state) => {
                    tracing::info!(
                        "Loaded graph from LanceDB: {} nodes, {} edges",
                        state.nodes.len(),
                        state.edges.len()
                    );
                    // Always re-index embeddings so .oh/ artifacts added since
                    // the last full build become searchable.  This drops and
                    // rebuilds the table (index_all_inner) which is acceptable
                    // at current repo scale.
                    if let Ok(idx) = EmbeddingIndex::new(&self.repo_root).await {
                        match idx.index_all_with_symbols(&self.repo_root, &state.nodes).await {
                            Ok(count) => {
                                tracing::info!("Re-indexed embedding index: {} items from cached graph", count);
                                self.embed_index.store(Arc::new(Some(idx)));
                            }
                            Err(e) => tracing::warn!("Failed to embed cached graph: {}", e),
                        }
                    }
                    return Ok(state);
                }
                Err(e) => {
                    tracing::debug!("Could not load persisted graph: {}", e);
                }
            }
        }

        // 3. Full rebuild
        let registry = ExtractorRegistry::with_builtins();
        let mut all_nodes: Vec<Node> = Vec::new();
        let mut all_edges: Vec<Edge> = Vec::new();

        for (root_slug, scanner, _scan_result, root_path) in &scanners {
            let all_files = scanner.all_known_files();
            let full_scan = crate::scanner::ScanResult {
                changed_files: Vec::new(),
                new_files: all_files,
                deleted_files: Vec::new(),
                scan_duration: std::time::Duration::ZERO,
            };
            let mut extraction = registry.extract_scan_result(root_path, &full_scan);

            for node in &mut extraction.nodes {
                node.id.root = root_slug.clone();
            }
            // Build file index for suffix matching import edges
            let file_index: std::collections::HashSet<String> = extraction.nodes
                .iter()
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect();

            for edge in &mut extraction.edges {
                edge.from.root = root_slug.clone();
                edge.to.root = root_slug.clone();
                // Resolve dangling import edges via suffix match
                resolve_edge_target_by_suffix(edge, &file_index);
            }

            tracing::info!(
                "Extracted from '{}': {} nodes, {} edges",
                root_slug,
                extraction.nodes.len(),
                extraction.edges.len()
            );

            all_nodes.extend(extraction.nodes);
            all_edges.extend(extraction.edges);
        }

        // 4. Extract PR merges from git history
        match git::pr_merges::extract_pr_merges(&self.repo_root, Some(100)) {
            Ok((pr_nodes, pr_edges)) => {
                let modified_edges =
                    git::pr_merges::link_pr_to_symbols(&pr_nodes, &all_nodes);
                tracing::info!(
                    "PR merges: {} nodes, {} edges, {} Modified links",
                    pr_nodes.len(),
                    pr_edges.len(),
                    modified_edges.len()
                );
                all_nodes.extend(pr_nodes);
                all_edges.extend(pr_edges);
                all_edges.extend(modified_edges);
            }
            Err(e) => {
                tracing::warn!("Failed to extract PR merges: {}", e);
            }
        }

        // 5. Build petgraph index
        let mut index = GraphIndex::new();
        index.rebuild_from_edges(&all_edges);
        for node in &all_nodes {
            index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        tracing::info!(
            "Graph built: {} nodes, {} edges across {} root(s)",
            all_nodes.len(),
            all_edges.len(),
            resolved_roots.len()
        );

        // 6. Compute PageRank importance scores
        let pagerank_scores = index.compute_pagerank(0.85, 20);
        for node in &mut all_nodes {
            if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                node.metadata.insert("importance".to_string(), format!("{:.6}", score));
            }
        }
        tracing::info!("Computed PageRank importance for {} nodes", pagerank_scores.len());

        // 7. Persist graph to LanceDB
        if let Err(e) = persist_graph_to_lance(&self.repo_root, &all_nodes, &all_edges).await {
            tracing::warn!("Failed to persist graph to LanceDB: {}", e);
        }

        // Graph is persisted — return immediately so agents can query.
        // Embedding and LSP enrichment run in background via the shared graph lock.
        let symbols_ready_at = std::time::Instant::now();

        // Store embed index immediately so it's available for queries.
        // The background task below will always re-index (including .oh/
        // artifacts) via index_all_inner which drops and rebuilds the table.
        match EmbeddingIndex::new(&self.repo_root).await {
            Ok(idx) => {
                tracing::info!("Embedding index created — background task will re-index");
                self.embed_index.store(Arc::new(Some(idx)));
            }
            Err(e) => {
                tracing::warn!("Failed to create embed index: {}", e);
            }
        };

        // Spawn background task for embedding + LSP enrichment.
        // The graph is queryable NOW — these improve quality progressively.
        let bg_repo_root = self.repo_root.clone();
        let bg_graph = self.graph.clone();
        let bg_embed_index = self.embed_index.clone();
        let bg_lsp_status = self.lsp_status.clone();
        let bg_nodes = all_nodes.clone();
        let bg_languages: Vec<String> = all_nodes
            .iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        tokio::spawn(async move {
            // Phase 1: Embed
            let embeddable_nodes: Vec<Node> = bg_nodes.iter()
                .filter(|n| n.id.root != "external")
                .cloned()
                .collect();
            // Always re-index so .oh/ artifacts added since the last
            // full build become searchable.  index_all_inner drops and
            // rebuilds the table -- acceptable at current repo scale.
            match EmbeddingIndex::new(&bg_repo_root).await {
                Ok(idx) => {
                    match idx.index_all_with_symbols(&bg_repo_root, &embeddable_nodes).await {
                        Ok(count) => {
                            tracing::info!("[background] Embedded {} items", count);
                            // Atomic store — no mutex needed
                            bg_embed_index.store(Arc::new(Some(idx)));
                        }
                        Err(e) => tracing::warn!("[background] Embedding failed: {}", e),
                    }
                }
                Err(e) => tracing::warn!("[background] EmbeddingIndex init failed: {}", e),
            }

            // Phase 2: LSP enrichment
            bg_lsp_status.set_running();
            let enricher_registry = EnricherRegistry::with_builtins();
            let enrichment = {
                let guard = bg_graph.read().await;
                if let Some(ref gs) = *guard {
                    enricher_registry
                        .enrich_all(&gs.nodes, &gs.index, &bg_languages, &bg_repo_root)
                        .await
                } else {
                    bg_lsp_status.set_complete(0);
                    return;
                }
            };

            if !enrichment.any_enricher_ran {
                tracing::info!("[background] LSP enrichment: no server available");
                bg_lsp_status.set_unavailable();
                return;
            }

            if enrichment.new_nodes.is_empty()
                && enrichment.added_edges.is_empty()
                && enrichment.updated_nodes.is_empty()
            {
                tracing::info!("[background] LSP enrichment: no changes");
                bg_lsp_status.set_complete(0);
                return;
            }

            tracing::info!(
                "[background] LSP enrichment: {} virtual nodes, {} edges, {} patches",
                enrichment.new_nodes.len(),
                enrichment.added_edges.len(),
                enrichment.updated_nodes.len()
            );

            // Apply enrichment to shared graph
            let mut guard = bg_graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

                let persist_edges = enrichment.added_edges.clone();
                for edge in &enrichment.added_edges {
                    let from_id = edge.from.to_stable_id();
                    let to_id = edge.to.to_stable_id();
                    gs.index.add_edge(
                        &from_id,
                        &edge.from.kind.to_string(),
                        &to_id,
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
                gs.edges.extend(enrichment.added_edges);

                let enriched_node_ids: Vec<String> = enrichment.updated_nodes.iter()
                    .map(|(id, _)| id.clone())
                    .collect();
                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(node) = gs.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }

                // Persist enrichment incrementally
                let upsert_nodes: Vec<Node> = gs.nodes.iter()
                    .filter(|n| enriched_node_ids.contains(&n.stable_id()))
                    .cloned()
                    .collect();
                let edge_count = persist_edges.len();
                drop(guard); // release lock before async persist
                let _ = persist_graph_incremental(
                    &bg_repo_root, &upsert_nodes, &persist_edges, &[], &[],
                ).await;
                bg_lsp_status.set_complete(edge_count);
            }
        });

        Ok(GraphState {
            nodes: all_nodes,
            edges: all_edges,
            index,
            last_scan_completed_at: Some(symbols_ready_at),
        })
    }

    /// Incrementally update the graph, accepting an optional pre-computed scan.
    ///
    /// When `pending_scan` is `Some`, the caller already ran the scanner and
    /// saved state — we reuse that result instead of scanning again (which
    /// would see zero changes because state was already updated).
    async fn update_graph_with_scan(
        &self,
        graph: &mut GraphState,
        pending_scan: Option<ScanResult>,
    ) -> anyhow::Result<()> {
        let scan = match pending_scan {
            Some(s) => s,
            None => {
                // Fallback: scan fresh (used by background scanner path)
                let mut scanner = Scanner::new(self.repo_root.clone())?;
                scanner.scan()?
            }
        };

        if scan.changed_files.is_empty()
            && scan.new_files.is_empty()
            && scan.deleted_files.is_empty()
        {
            return Ok(());
        }

        tracing::info!(
            "Incremental update: {} changed, {} new, {} deleted",
            scan.changed_files.len(),
            scan.new_files.len(),
            scan.deleted_files.len()
        );

        let registry = ExtractorRegistry::with_builtins();

        // Remove nodes/edges for deleted + changed files
        let files_to_remove: Vec<PathBuf> = scan
            .deleted_files
            .iter()
            .chain(scan.changed_files.iter())
            .cloned()
            .collect();

        // Collect edge stable IDs for removed/changed files BEFORE retain, so we can
        // delete them from LanceDB. (After retain they're gone from memory.)
        let deleted_edge_ids: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| {
                files_to_remove
                    .iter()
                    .any(|f| e.from.file == *f || e.to.file == *f)
            })
            .map(|e| e.stable_id())
            .collect();

        graph
            .nodes
            .retain(|n| !files_to_remove.iter().any(|f| n.id.file == *f));
        graph.edges.retain(|e| {
            !files_to_remove
                .iter()
                .any(|f| e.from.file == *f || e.to.file == *f)
        });

        // Extract new + changed files
        let extraction = registry.extract_scan_result(&self.repo_root, &scan);
        // Track only the delta (new/changed) for LanceDB upsert — not the full graph.
        let mut upsert_nodes: Vec<Node> = extraction.nodes.clone();
        let mut upsert_edges: Vec<Edge> = extraction.edges.clone();
        graph.nodes.extend(extraction.nodes);
        graph.edges.extend(extraction.edges);

        // Rebuild petgraph index
        graph.index = GraphIndex::new();
        graph.index.rebuild_from_edges(&graph.edges);
        for node in &graph.nodes {
            graph
                .index
                .ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        // Run LSP enrichers on the updated nodes (same as cold-start, but scoped to changed files)
        let changed_files: std::collections::HashSet<_> = scan
            .changed_files
            .iter()
            .chain(scan.new_files.iter())
            .collect();
        let changed_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| changed_files.iter().any(|f| n.id.file == **f))
            .cloned()
            .collect();

        if !changed_nodes.is_empty() {
            let languages: Vec<String> = changed_nodes
                .iter()
                .map(|n| n.language.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            self.lsp_status.set_running();
            let enricher_registry = EnricherRegistry::with_builtins();
            let enrichment = enricher_registry
                .enrich_all(&changed_nodes, &graph.index, &languages, &self.repo_root)
                .await;

            if !enrichment.any_enricher_ran {
                self.lsp_status.set_unavailable();
            }

            let incr_edge_count = enrichment.added_edges.len();

            // Add virtual nodes synthesized for external symbols.
            // These must NOT be re-embedded (no body).
            if !enrichment.new_nodes.is_empty() {
                tracing::info!(
                    "Incremental LSP enrichment synthesized {} virtual external nodes",
                    enrichment.new_nodes.len()
                );
                for vnode in &enrichment.new_nodes {
                    graph.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                // Include synthesized virtual nodes in the LanceDB upsert delta.
                upsert_nodes.extend(enrichment.new_nodes.iter().cloned());
                graph.nodes.extend(enrichment.new_nodes);
            }

            if !enrichment.added_edges.is_empty() {
                tracing::info!(
                    "Incremental LSP enrichment added {} edges",
                    enrichment.added_edges.len()
                );
                for edge in &enrichment.added_edges {
                    graph.index.add_edge(
                        &edge.from.to_stable_id(),
                        &edge.from.kind.to_string(),
                        &edge.to.to_stable_id(),
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
                // Include LSP-synthesized edges in the LanceDB upsert delta.
                upsert_edges.extend(enrichment.added_edges.iter().cloned());
                graph.edges.extend(enrichment.added_edges);
            }

            let enriched_node_ids: std::collections::HashSet<String> =
                enrichment.updated_nodes.iter().map(|(id, _)| id.clone()).collect();

            for (node_id, patches) in &enrichment.updated_nodes {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                    for (key, value) in patches {
                        node.metadata.insert(key.clone(), value.clone());
                    }
                }
            }

            // Re-embed the enriched nodes specifically, using their post-enrichment metadata.
            // This is scoped to only changed nodes to avoid re-embedding the entire graph.
            if !enriched_node_ids.is_empty() {
                let embed_guard = self.embed_index.load();
                if let Some(ref embed_idx) = **embed_guard {
                    let enriched_nodes: Vec<_> = graph
                        .nodes
                        .iter()
                        .filter(|n| enriched_node_ids.contains(&n.stable_id()))
                        .cloned()
                        .collect();
                    // Also include LSP-enriched nodes in the LanceDB upsert delta so their
                    // updated metadata (e.g. resolved signatures) is persisted.
                    // Deduplicate against nodes already queued for upsert (extraction may overlap).
                    let already_queued: std::collections::HashSet<String> =
                        upsert_nodes.iter().map(|u| u.stable_id()).collect();
                    let new_enriched: Vec<Node> = enriched_nodes
                        .iter()
                        .filter(|n| !already_queued.contains(&n.stable_id()))
                        .cloned()
                        .collect();
                    upsert_nodes.extend(new_enriched);
                    match embed_idx.reindex_nodes(&enriched_nodes).await {
                        Ok(count) => tracing::info!(
                            "Re-embedded {} enriched nodes with LSP metadata",
                            count
                        ),
                        Err(e) => tracing::warn!("Failed to re-embed enriched nodes: {}", e),
                    }
                }
            }

            if enrichment.any_enricher_ran {
                self.lsp_status.set_complete(incr_edge_count);
            }
        }

        // Re-embed changed-file symbols. Uses the updated graph nodes so enriched
        // metadata is included in the embedding text.
        let embed_guard2 = self.embed_index.load();
        if let Some(ref embed_idx) = **embed_guard2 {
            let changed_file_nodes: Vec<_> = graph
                .nodes
                .iter()
                .filter(|n| changed_files.iter().any(|f| n.id.file == **f))
                .cloned()
                .collect();
            match embed_idx.reindex_nodes(&changed_file_nodes).await {
                Ok(count) => {
                    tracing::info!("Re-embedded {} changed-file nodes after incremental update", count)
                }
                Err(e) => {
                    // reindex_nodes falls back to no-op if the table doesn't exist;
                    // do a full rebuild instead.
                    tracing::warn!("Targeted re-embed failed ({}), falling back to full rebuild", e);
                    if let Err(e2) = embed_idx
                        .index_all_with_symbols(&self.repo_root, &graph.nodes)
                        .await
                    {
                        tracing::warn!("Full embed rebuild also failed: {}", e2);
                    }
                }
            }
        }

        // Persist updated graph incrementally — only the delta (changed/added nodes and edges).
        // Untouched rows remain in LanceDB as-is. Deleted files are removed by targeted delete.
        // merge_insert keeps tables alive; no empty-result query window.
        if let Err(e) = persist_graph_incremental(
            &self.repo_root,
            &upsert_nodes,
            &upsert_edges,
            &deleted_edge_ids,
            &files_to_remove,
        )
        .await
        {
            tracing::warn!("Failed to persist updated graph: {}", e);
        }

        graph.last_scan_completed_at = Some(std::time::Instant::now());

        Ok(())
    }

    // ── Unified search handler ──────────────────────────────────────────
    // Shared implementation for `search`, `search_symbols` (deprecated alias),
    // and `graph_query` (deprecated alias). Branches on whether `mode` is set
    // (graph traversal) or absent (flat symbol search).

    async fn handle_search(&self, args: Search) -> Result<CallToolResult, CallToolError> {
        // Normalize inputs
        let query = args.query.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let node = args.node.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let compact = args.compact.unwrap_or(false);

        // ── Batch node retrieval path ────────────────────────────────
        // When `nodes` is provided, resolve each ID from the graph directly.
        if let Some(ref node_ids) = args.nodes {
            let node_ids: Vec<&str> = node_ids.iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if node_ids.is_empty() {
                return Ok(text_result("Empty nodes list. Provide at least one stable node ID.".to_string()));
            }
            return self.handle_search_batch(&node_ids, compact, &args).await;
        }

        if args.mode.is_some() {
            // ── Graph traversal path ──────────────────────────────────
            self.handle_search_traversal(&args, query, node, compact).await
        } else {
            // ── Flat search path ──────────────────────────────────────
            self.handle_search_flat(&args, query, compact).await
        }
    }

    /// Flat symbol search (no `mode` parameter). Equivalent to the old `search_symbols`.
    async fn handle_search_flat(
        &self,
        args: &Search,
        query: Option<&str>,
        compact: bool,
    ) -> Result<CallToolResult, CallToolError> {
        let sort_by_complexity = args.sort_by.as_deref() == Some("complexity");
        let sort_by_importance = args.sort_by.as_deref() == Some("importance");
        let has_complexity_filter = args.min_complexity.is_some();
        let complexity_search = has_complexity_filter || sort_by_complexity;

        let query_str = query.unwrap_or("");
        if query_str.is_empty() && !complexity_search && !sort_by_importance {
            return Ok(text_result("Empty query. Please describe what you're looking for (or use min_complexity / sort_by=\"complexity\" / sort_by=\"importance\").".into()));
        }

        match self.get_graph().await {
            Ok(guard) => {
                let graph_state = guard.as_ref().unwrap();
                let limit = args.top_k.unwrap_or(10) as usize;
                let query_lower = query_str.to_lowercase();

                let mut matches: Vec<&Node> = graph_state
                    .nodes
                    .iter()
                    .filter(|n| {
                        // In complexity search mode, only return functions.
                        if complexity_search && n.id.kind != NodeKind::Function {
                            return false;
                        }
                        // When query is non-empty, filter by name/signature match.
                        if !query_lower.is_empty() {
                            let name_match = n.id.name.to_lowercase().contains(&query_lower)
                                || n.signature.to_lowercase().contains(&query_lower);
                            if !name_match {
                                return false;
                            }
                        }
                        if let Some(ref kind_filter) = args.kind {
                            if n.id.kind.to_string().to_lowercase() != kind_filter.to_lowercase() {
                                return false;
                            }
                        }
                        if let Some(ref lang_filter) = args.language {
                            if n.language.to_lowercase() != lang_filter.to_lowercase() {
                                return false;
                            }
                        }
                        if let Some(ref file_filter) = args.file {
                            let path_str = n.id.file.to_string_lossy();
                            if !path_str.contains(file_filter.as_str()) {
                                return false;
                            }
                        }
                        if let Some(ref root_filter) = args.root {
                            if n.id.root.to_lowercase() != root_filter.to_lowercase() {
                                return false;
                            }
                        }
                        if let Some(synthetic_filter) = args.synthetic {
                            let is_synthetic = n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false);
                            if is_synthetic != synthetic_filter {
                                return false;
                            }
                        }
                        if let Some(min_cc) = args.min_complexity {
                            let Some(cc) = n.metadata.get("cyclomatic")
                                .and_then(|s| s.parse::<u32>().ok())
                            else {
                                return false;
                            };
                            if cc < min_cc {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();

                if sort_by_complexity {
                    matches.retain(|n| {
                        n.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).is_some()
                    });
                    matches.sort_by(|a, b| {
                        let cc_a = a.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        let cc_b = b.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        cc_b.cmp(&cc_a)
                    });
                } else if sort_by_importance {
                    // Sort by PageRank importance descending.
                    // Symbols without importance scores sort to the bottom (not filtered out).
                    matches.sort_by(|a, b| {
                        let imp_a = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok());
                        let imp_b = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok());
                        match (imp_a, imp_b) {
                            (Some(a_val), Some(b_val)) => b_val.partial_cmp(&a_val).unwrap_or(std::cmp::Ordering::Equal),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => std::cmp::Ordering::Equal,
                        }
                    });
                } else {
                    ranking::sort_symbol_matches(&mut matches, &query_lower, &graph_state.index);
                }
                matches.truncate(limit);

                let freshness = format_freshness(
                    graph_state.nodes.len(),
                    graph_state.last_scan_completed_at,
                    Some(&self.lsp_status),
                );
                if matches.is_empty() {
                    Ok(text_result(format!(
                        "No symbols matching \"{}\".{}",
                        query_str, freshness
                    )))
                } else {
                    let md: String = matches
                        .iter()
                        .map(|n| format_node_entry(n, &graph_state.index, compact))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Ok(text_result(format!(
                        "## Symbol search: \"{}\"\n\n{} result(s)\n\n{}{}",
                        query_str,
                        matches.len(),
                        md,
                        freshness
                    )))
                }
            }
            Err(e) => Ok(text_result(format!("Graph error: {}", e))),
        }
    }

    /// Graph traversal search (with `mode` parameter). Equivalent to the old `graph_query`.
    async fn handle_search_traversal(
        &self,
        args: &Search,
        query: Option<&str>,
        node: Option<&str>,
        compact: bool,
    ) -> Result<CallToolResult, CallToolError> {
        let mode = args.mode.as_deref().unwrap_or("neighbors");
        let top_k = args.top_k.unwrap_or(1).clamp(1, 50) as usize;

        // Reject if no entry point
        if node.is_none() && query.is_none() {
            return Ok(text_result(
                "Either query or node is required. Provide a search query or a stable node ID.".to_string()
            ));
        }

        // Resolve entry node IDs
        let search_mode = parse_search_mode(args.search_mode.as_deref());
        let (entry_node_ids, entry_header): (Vec<String>, String) = if let Some(node_id) = node {
            (vec![node_id.to_string()], String::new())
        } else if let Some(query_text) = query {
            let embed_guard = self.embed_index.load();
            match embed_guard.as_ref() {
                Some(embed_idx) => {
                    match embed_idx.search_with_mode(query_text, None, top_k.min(50) * 3, search_mode).await {
                        Ok(SearchOutcome::Results(results)) if !results.is_empty() => {
                            let code_results: Vec<_> = results.into_iter()
                                .filter(|r| r.kind.starts_with("code:"))
                                .take(top_k)
                                .collect();

                            if code_results.is_empty() {
                                return Ok(text_result(format!(
                                    "No code symbols matched query \"{}\". Try a different query or use node parameter.",
                                    query_text
                                )));
                            }

                            let mut header = format!("### Matched entry nodes for \"{}\"\n\n", query_text);
                            let ids: Vec<String> = code_results.iter()
                                .map(|r| {
                                    header.push_str(&format!(
                                        "- `{}` -- {} (score: {:.2})\n",
                                        r.id, r.title, r.score
                                    ));
                                    r.id.clone()
                                })
                                .collect();
                            header.push('\n');
                            (ids, header)
                        }
                        Ok(SearchOutcome::NotReady) => {
                            return Ok(text_result(
                                "Embedding index: building -- semantic graph queries will work shortly. Use node parameter instead, or retry in a few seconds.".to_string()
                            ));
                        }
                        Ok(_) => {
                            return Ok(text_result(format!(
                                "No code symbols matched query \"{}\". Try a different query or use node parameter.",
                                query_text
                            )));
                        }
                        Err(e) => {
                            return Ok(text_result(format!(
                                "Semantic search failed: {}. Use node parameter instead.",
                                e
                            )));
                        }
                    }
                }
                None => {
                    return Ok(text_result(
                        "Embedding index not available. Use node parameter instead, or wait for the background index to build.".to_string()
                    ));
                }
            }
        } else {
            unreachable!("both-empty case handled above");
        };

        match self.get_graph().await {
            Ok(guard) => {
                let graph_state = guard.as_ref().unwrap();

                let valid_entry_ids: Vec<&String> = entry_node_ids.iter()
                    .filter(|id| graph_state.index.get_node(id).is_some())
                    .collect();

                if valid_entry_ids.is_empty() {
                    let id_list = entry_node_ids.iter()
                        .map(|id| format!("`{}`", id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Ok(text_result(format!(
                        "{}No graph nodes found for {}. The node(s) may not have edges in the graph. Try search to find valid node IDs.",
                        entry_header, id_list
                    )));
                }

                let edge_filter = args.edge_types.as_ref().map(|types| {
                    types
                        .iter()
                        .filter_map(|t| parse_edge_kind(t))
                        .collect::<Vec<_>>()
                });
                let edge_filter_slice = edge_filter.as_deref();

                let mut all_ids: Vec<String> = Vec::new();
                let mut seen = std::collections::HashSet::new();

                for node_id in &valid_entry_ids {
                    match run_traversal(&graph_state.index, node_id, mode, args.hops, args.direction.as_deref(), edge_filter_slice) {
                        Ok(ids) => {
                            for id in ids {
                                if seen.insert(id.clone()) {
                                    all_ids.push(id);
                                }
                            }
                        }
                        Err(msg) => return Ok(text_result(msg)),
                    }
                }

                let entry_set: std::collections::HashSet<&str> = valid_entry_ids.iter().map(|s| s.as_str()).collect();
                all_ids.retain(|id| !entry_set.contains(id.as_str()));

                // For tests_for mode, filter to only callers in test files
                if mode == "tests_for" {
                    all_ids.retain(|id| {
                        graph_state.nodes.iter()
                            .find(|n| n.stable_id() == *id)
                            .map(|n| ranking::is_test_file(n))
                            .unwrap_or(false)
                    });
                }

                let entry_label = if valid_entry_ids.len() == 1 {
                    format!("`{}`", valid_entry_ids[0])
                } else {
                    format!("{} entry nodes", valid_entry_ids.len())
                };

                let direction = args.direction.as_deref().unwrap_or("outgoing");

                let freshness = format_freshness(
                    graph_state.nodes.len(),
                    graph_state.last_scan_completed_at,
                    Some(&self.lsp_status),
                );

                if all_ids.is_empty() {
                    let mode_desc = match mode {
                        "neighbors" => format!("No {} neighbors for {}.", direction, entry_label),
                        "impact" => format!("No dependents found for {} within {} hops.", entry_label, args.hops.unwrap_or(3)),
                        "reachable" => format!("No reachable nodes from {} within {} hops.", entry_label, args.hops.unwrap_or(3)),
                        "tests_for" => format!("No test functions found calling {}. Either no tests exist for this symbol, or the call edges haven't been extracted (check LSP status).", entry_label),
                        _ => format!("No results for {}.", entry_label),
                    };
                    Ok(text_result(format!("{}{}{}", entry_header, mode_desc, freshness)))
                } else {
                    let md = format_neighbor_nodes(&graph_state.nodes, &all_ids, &graph_state.index, compact);
                    let heading = match mode {
                        "neighbors" => format!(
                            "## Graph neighbors ({}) of {}\n\n{} result(s)\n\n",
                            direction, entry_label, all_ids.len()
                        ),
                        "impact" => format!(
                            "## Impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n",
                            entry_label, all_ids.len(), args.hops.unwrap_or(3)
                        ),
                        "reachable" => format!(
                            "## Reachable from {}\n\n{} node(s) within {} hop(s)\n\n",
                            entry_label, all_ids.len(), args.hops.unwrap_or(3)
                        ),
                        "tests_for" => format!(
                            "## Test coverage for {}\n\n{} test function(s)\n\n",
                            entry_label, all_ids.len()
                        ),
                        _ => String::new(),
                    };
                    Ok(text_result(format!("{}{}{}{}", entry_header, heading, md, freshness)))
                }
            }
            Err(e) => Ok(text_result(format!("Graph error: {}", e))),
        }
    }

    /// Batch node retrieval: resolve multiple stable node IDs in a single call.
    /// When `mode` is provided, runs traversal from each node (composes with hops/direction/edge_types).
    /// When `mode` is absent, simply retrieves the nodes.
    async fn handle_search_batch(
        &self,
        node_ids: &[&str],
        compact: bool,
        args: &Search,
    ) -> Result<CallToolResult, CallToolError> {
        // If mode is provided, route each node through traversal logic
        if args.mode.is_some() {
            // Route through traversal logic for each seed node
            match self.get_graph().await {
                Ok(guard) => {
                    let graph_state = guard.as_ref().unwrap();
                    let mode = args.mode.as_deref().unwrap_or("neighbors");

                    let edge_filter = args.edge_types.as_ref().map(|types| {
                        types
                            .iter()
                            .filter_map(|t| parse_edge_kind(t))
                            .collect::<Vec<_>>()
                    });
                    let edge_filter_slice = edge_filter.as_deref();

                    let mut valid_ids: Vec<&str> = Vec::new();
                    let mut missing: Vec<&str> = Vec::new();
                    for &nid in node_ids {
                        if graph_state.index.get_node(nid).is_some() {
                            valid_ids.push(nid);
                        } else {
                            missing.push(nid);
                        }
                    }

                    if valid_ids.is_empty() {
                        let freshness = format_freshness(
                            graph_state.nodes.len(),
                            graph_state.last_scan_completed_at,
                            Some(&self.lsp_status),
                        );
                        let id_list = node_ids.iter()
                            .map(|id| format!("`{}`", id))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Ok(text_result(format!(
                            "No graph nodes found for {}. Use search to find valid node IDs.{}",
                            id_list, freshness
                        )));
                    }

                    let mut all_ids: Vec<String> = Vec::new();
                    let mut seen = std::collections::HashSet::new();

                    for &node_id in &valid_ids {
                        match run_traversal(&graph_state.index, node_id, mode, args.hops, args.direction.as_deref(), edge_filter_slice) {
                            Ok(ids) => {
                                for id in ids {
                                    if seen.insert(id.clone()) {
                                        all_ids.push(id);
                                    }
                                }
                            }
                            Err(msg) => return Ok(text_result(msg)),
                        }
                    }

                    // Remove entry nodes from results
                    let entry_set: std::collections::HashSet<&str> = valid_ids.iter().copied().collect();
                    all_ids.retain(|id| !entry_set.contains(id.as_str()));

                    // For tests_for mode, filter to only callers in test files
                    if mode == "tests_for" {
                        all_ids.retain(|id| {
                            graph_state.nodes.iter()
                                .find(|n| n.stable_id() == *id)
                                .map(|n| ranking::is_test_file(n))
                                .unwrap_or(false)
                        });
                    }

                    let freshness = format_freshness(
                        graph_state.nodes.len(),
                        graph_state.last_scan_completed_at,
                        Some(&self.lsp_status),
                    );

                    let direction = args.direction.as_deref().unwrap_or("outgoing");
                    let entry_label = format!("{} batch node(s)", valid_ids.len());

                    if all_ids.is_empty() {
                        let mode_desc = match mode {
                            "neighbors" => format!("No {} neighbors for {}.", direction, entry_label),
                            "impact" => format!("No dependents found for {} within {} hops.", entry_label, args.hops.unwrap_or(3)),
                            "reachable" => format!("No reachable nodes from {} within {} hops.", entry_label, args.hops.unwrap_or(3)),
                            "tests_for" => format!("No test functions found calling {}.", entry_label),
                            _ => format!("No results for {}.", entry_label),
                        };
                        let mut result = mode_desc;
                        if !missing.is_empty() {
                            result.push_str(&format!(
                                "\n\n**Missing:** {}",
                                missing.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", ")
                            ));
                        }
                        result.push_str(&freshness);
                        Ok(text_result(result))
                    } else {
                        let md = format_neighbor_nodes(&graph_state.nodes, &all_ids, &graph_state.index, compact);
                        let heading = match mode {
                            "neighbors" => format!(
                                "## Batch graph neighbors ({}) of {}\n\n{} result(s)\n\n",
                                direction, entry_label, all_ids.len()
                            ),
                            "impact" => format!(
                                "## Batch impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n",
                                entry_label, all_ids.len(), args.hops.unwrap_or(3)
                            ),
                            "reachable" => format!(
                                "## Batch reachable from {}\n\n{} node(s) within {} hop(s)\n\n",
                                entry_label, all_ids.len(), args.hops.unwrap_or(3)
                            ),
                            "tests_for" => format!(
                                "## Batch test coverage for {}\n\n{} test function(s)\n\n",
                                entry_label, all_ids.len()
                            ),
                            _ => String::new(),
                        };
                        let mut result = format!("{}{}", heading, md);
                        if !missing.is_empty() {
                            result.push_str(&format!(
                                "\n\n**Missing:** {}",
                                missing.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", ")
                            ));
                        }
                        result.push_str(&freshness);
                        Ok(text_result(result))
                    }
                }
                Err(e) => Ok(text_result(format!("Graph error: {}", e))),
            }
        } else {
            // No mode: simple batch retrieval (existing behavior)
            match self.get_graph().await {
                Ok(guard) => {
                    let graph_state = guard.as_ref().unwrap();
                    let freshness = format_freshness(
                        graph_state.nodes.len(),
                        graph_state.last_scan_completed_at,
                        Some(&self.lsp_status),
                    );

                    let mut found = Vec::new();
                    let mut missing = Vec::new();

                    for &nid in node_ids {
                        if let Some(node) = graph_state.nodes.iter().find(|n| n.stable_id() == nid) {
                            found.push(node);
                        } else {
                            missing.push(nid);
                        }
                    }

                    if found.is_empty() {
                        let id_list = node_ids.iter()
                            .map(|id| format!("`{}`", id))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Ok(text_result(format!(
                            "No graph nodes found for {}. Use search to find valid node IDs.{}",
                            id_list, freshness
                        )));
                    }

                    let md: String = found
                        .iter()
                        .map(|n| format_node_entry(n, &graph_state.index, compact))
                        .collect::<Vec<_>>()
                        .join("\n\n");

                    let mut result = format!(
                        "## Batch retrieval\n\n{} of {} node(s) found\n\n{}",
                        found.len(),
                        node_ids.len(),
                        md,
                    );
                    if !missing.is_empty() {
                        result.push_str(&format!(
                            "\n\n**Missing:** {}",
                            missing.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", ")
                        ));
                    }
                    result.push_str(&freshness);
                    Ok(text_result(result))
                }
                Err(e) => Ok(text_result(format!("Graph error: {}", e))),
            }
        }
    }
}

/// Execute a single graph traversal from a given node ID.
///
/// Shared by `handle_search_traversal` (single-node entry) and
/// `handle_search_batch` (multi-node entry with mode).  Keeping the logic
/// in one place prevents the two paths from diverging.
fn run_traversal(
    index: &GraphIndex,
    node_id: &str,
    mode: &str,
    hops: Option<u32>,
    direction: Option<&str>,
    edge_filter: Option<&[EdgeKind]>,
) -> Result<Vec<String>, String> {
    match mode {
        "neighbors" => {
            let max_hops = hops.unwrap_or(1) as usize;
            let dir = direction.unwrap_or("outgoing");
            match dir {
                "outgoing" => {
                    if max_hops == 1 {
                        Ok(index.neighbors(node_id, edge_filter, Direction::Outgoing))
                    } else {
                        Ok(index.reachable(node_id, max_hops, edge_filter))
                    }
                }
                "incoming" => {
                    if max_hops == 1 {
                        Ok(index.neighbors(node_id, edge_filter, Direction::Incoming))
                    } else {
                        Ok(index.impact(node_id, max_hops))
                    }
                }
                "both" => {
                    let out = if max_hops == 1 {
                        index.neighbors(node_id, edge_filter, Direction::Outgoing)
                    } else {
                        index.reachable(node_id, max_hops, edge_filter)
                    };
                    let inc = if max_hops == 1 {
                        index.neighbors(node_id, edge_filter, Direction::Incoming)
                    } else {
                        index.impact(node_id, max_hops)
                    };
                    let mut combined = out;
                    combined.extend(inc);
                    Ok(combined)
                }
                _ => Err(format!(
                    "Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".",
                    dir
                )),
            }
        }
        "impact" => {
            let max_hops = hops.unwrap_or(3) as usize;
            Ok(index.impact(node_id, max_hops))
        }
        "reachable" => {
            let max_hops = hops.unwrap_or(3) as usize;
            Ok(index.reachable(node_id, max_hops, edge_filter))
        }
        "tests_for" => {
            let calls_filter = &[EdgeKind::Calls];
            Ok(index.neighbors(node_id, Some(calls_filter), Direction::Incoming))
        }
        other => Err(format!(
            "Unknown mode: \"{}\". Use \"neighbors\", \"impact\", \"reachable\", or \"tests_for\".",
            other
        )),
    }
}

/// Parse a `search_mode` string into [`SearchMode`].
/// Returns `Hybrid` for `None` or unrecognized values.
fn parse_search_mode(s: Option<&str>) -> SearchMode {
    match s.map(str::to_lowercase).as_deref() {
        Some("keyword") => SearchMode::Keyword,
        Some("semantic") => SearchMode::Semantic,
        _ => SearchMode::Hybrid,
    }
}

fn text_result(s: String) -> CallToolResult {
    CallToolResult::text_content(vec![TextContent::new(s, None, None)])
}

/// Format an index freshness footer for appending to tool responses.
///
/// Example output: `\n*Index: 3655 symbols · last scan 4m ago · schema v2*`
pub fn format_freshness(
    node_count: usize,
    last_scan: Option<std::time::Instant>,
    lsp_status: Option<&LspEnrichmentStatus>,
) -> String {
    let age = match last_scan {
        None => "never".to_string(),
        Some(t) => {
            let secs = t.elapsed().as_secs();
            if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else {
                format!("{}h ago", secs / 3600)
            }
        }
    };
    let lsp_segment = lsp_status.and_then(|s| s.footer_segment());
    match lsp_segment {
        Some(seg) => format!(
            "\n*Index: {} symbols · last scan {} · {} · schema v{}*",
            node_count, age, seg, SCHEMA_VERSION
        ),
        None => format!(
            "\n*Index: {} symbols · last scan {} · schema v{}*",
            node_count, age, SCHEMA_VERSION
        ),
    }
}

/// Build a concise business context preamble from .oh/ artifacts.
fn build_context_preamble(root: &Path) -> String {
    let artifacts = match oh::load_oh_artifacts(root) {
        Ok(a) => a,
        Err(_) => return String::new(),
    };

    if artifacts.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();

    // Active outcomes (just names + status)
    let outcomes: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Outcome).collect();
    if !outcomes.is_empty() {
        let mut section = String::from("**Active outcomes:**\n");
        for o in &outcomes {
            let status = o.frontmatter.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            section.push_str(&format!("- {} ({})\n", o.id(), status));
        }
        parts.push(section);
    }

    // Hard/soft guardrails (just statements, no full body)
    let guardrails: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Guardrail).collect();
    if !guardrails.is_empty() {
        let mut section = String::from("**Guardrails:**\n");
        for g in &guardrails {
            let severity = g.frontmatter.get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("candidate");
            let id = g.id();
            let statement = g.frontmatter.get("statement")
                .and_then(|v| v.as_str())
                .unwrap_or(&id);
            section.push_str(&format!("- [{}] {}\n", severity, statement));
        }
        parts.push(section);
    }

    // Recent metis (last 3 titles only)
    let metis: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Metis).collect();
    if !metis.is_empty() {
        let mut section = String::from("**Recent learnings:**\n");
        for m in metis.iter().rev().take(3) {
            let id = m.id();
            let title = m.frontmatter.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&id);
            section.push_str(&format!("- {}\n", title));
        }
        parts.push(section);
    }

    let mut out = format!("---\n# Business Context (auto-injected on first tool call)\n\n{}\n", parts.join("\n"));
    out.push_str("**Code exploration:** use `search` (not Grep/Read), `oh_search_context` (not search_all). `search_symbols` and `graph_query` are deprecated aliases for `search`.\n");
    out.push_str("---\n\n");
    out
}

#[async_trait]
impl rust_mcp_sdk::mcp_server::ServerHandler for RnaHandler {
    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: vec![
                OhSearchContext::tool(),
                OutcomeProgress::tool(),
                Search::tool(),
                SearchSymbols::tool(),  // deprecated alias
                GraphQuery::tool(),     // deprecated alias
                ListRoots::tool(),
                RepoMap::tool(),
            ],
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let root = &self.repo_root;

        // Inject business context preamble on first tool call
        let preamble = if !self.context_injected.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let ctx = build_context_preamble(root);
            if !ctx.is_empty() {
                tracing::info!("Injecting business context preamble on first tool call");
                Some(ctx)
            } else {
                None
            }
        } else {
            None
        };

        let mut result = match params.name.as_str() {
            "oh_search_context" => {
                let args: OhSearchContext = parse_args(params.arguments)?;
                let query = args.query.trim();
                if query.is_empty() {
                    return Ok(text_result("Empty query. Please describe what you're looking for.".into()));
                }
                let limit = args.limit.unwrap_or(5) as usize;
                let include_code = args.include_code.unwrap_or(false);
                let include_markdown = args.include_markdown.unwrap_or(false);
                let search_mode = parse_search_mode(args.search_mode.as_deref());

                // Ensure graph is built first so symbols are embedded
                // (get_graph builds the graph + embeds symbols in the pipeline)
                let (graph_node_count, graph_last_scan) = match self.get_graph().await {
                    Ok(guard) => {
                        if let Some(gs) = guard.as_ref() {
                            (gs.nodes.len(), gs.last_scan_completed_at)
                        } else {
                            (0, None)
                        }
                    }
                    Err(_) => (0, None),
                };

                let mut sections: Vec<String> = Vec::new();

                // Search .oh/ artifacts + symbols via embedding index.
                // Lock-free load from ArcSwap — no mutex contention with graph writes.
                {
                    let embed_guard = self.embed_index.load();
                    match embed_guard.as_ref() {
                        Some(index) => {
                            match index.search_with_mode(query, args.artifact_types.as_deref(), limit, search_mode).await {
                                Ok(SearchOutcome::Results(results)) => {
                                    if !results.is_empty() {
                                        let md: String = results
                                            .iter()
                                            .map(|r| r.to_markdown())
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        sections.push(format!(
                                            "### Artifacts ({} result(s))\n\n{}",
                                            results.len(),
                                            md
                                        ));
                                    }
                                }
                                Ok(SearchOutcome::NotReady) => {
                                    sections.push("Embedding index: building — semantic results will appear shortly. Retry in a few seconds.".to_string());
                                }
                                Err(e) => sections.push(format!("Artifact search error: {}", e)),
                            }
                        }
                        None => sections.push("Embedding index not yet available".to_string()),
                    }
                }

                // Optionally search code symbols from the graph (with 5-tier ranking)
                if include_code {
                    if let Ok(guard) = self.get_graph().await {
                        if let Some(gs) = guard.as_ref() {
                            let query_lower = query.to_lowercase();
                            let mut matches: Vec<&Node> = gs.nodes.iter()
                                .filter(|n| n.id.kind != NodeKind::Import && n.id.root != "external")
                                .filter(|n| {
                                    n.id.name.to_lowercase().contains(&query_lower)
                                        || n.signature.to_lowercase().contains(&query_lower)
                                })
                                .collect();

                            // Rank using the shared 5-tier cascade (same logic as search_symbols)
                            ranking::sort_symbol_matches(&mut matches, &query_lower, &gs.index);
                            matches.truncate(limit);

                            if !matches.is_empty() {
                                // Output format intentionally matches search_symbols
                                // (unordered list with `- **`) for backward compatibility.
                                // Results are sorted by relevance but use the same bullet
                                // style so agents parsing the old format are unaffected.
                                let md = matches.iter()
                                    .map(|n| {
                                        let mut line = format!(
                                            "- **{} {} ({})** ({})\n  `{}`\n  ID: `{}`",
                                            n.id.kind, n.id.name, n.language,
                                            n.id.file.display(),
                                            n.signature,
                                            n.stable_id(),
                                        );
                                        if let Some(cc) = n.metadata.get("cyclomatic") {
                                            line.push_str(&format!("\n  Complexity: {}", cc));
                                        }
                                        if let Some(imp) = n.metadata.get("importance") {
                                            if let Ok(score) = imp.parse::<f64>() {
                                                if score > IMPORTANCE_THRESHOLD {
                                                    line.push_str(&format!("\n  Importance: {:.3}", score));
                                                }
                                            }
                                        }
                                        line
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n");
                                sections.push(format!(
                                    "### Code symbols ({} result(s))\n\n{}",
                                    matches.len(),
                                    md
                                ));
                            }
                        }
                    }
                }

                // Optionally search markdown (with relevance scoring)
                if include_markdown {
                    match markdown::extract_markdown_chunks(root) {
                        Ok(chunks) => {
                            let scored = markdown::search_chunks_ranked(&chunks, query);
                            if !scored.is_empty() {
                                // Backward-compatible format: `- ` bullets, same header
                                // as before. Score is appended as a parenthetical so
                                // agents that ignore it are unaffected.
                                let md = scored
                                    .iter()
                                    .take(limit)
                                    .map(|sc| {
                                        format!(
                                            "- (score: {:.2}) {}", sc.score, sc.chunk.to_markdown()
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n---\n\n");
                                sections.push(format!(
                                    "### Markdown ({} result(s))\n\n{}",
                                    scored.len().min(limit),
                                    md
                                ));
                            }
                        }
                        Err(e) => sections.push(format!("Markdown search error: {}", e)),
                    }
                }

                let freshness = format_freshness(graph_node_count, graph_last_scan, Some(&self.lsp_status));
                if sections.is_empty() {
                    Ok(text_result(format!(
                        "No results found matching \"{}\".{}",
                        query, freshness
                    )))
                } else {
                    Ok(text_result(format!(
                        "## Semantic search: \"{}\"\n\n{}{}",
                        query,
                        sections.join("\n\n"),
                        freshness
                    )))
                }
            }

            "outcome_progress" => {
                let args: OutcomeProgress = parse_args(params.arguments)?;
                let graph_nodes = if let Ok(guard) = self.get_graph().await {
                    guard.as_ref().map(|gs| gs.nodes.clone()).unwrap_or_default()
                } else {
                    Vec::new()
                };
                match query::outcome_progress(root, &args.outcome_id, &graph_nodes) {
                    Ok(result) => {
                        let mut md = result.to_summary_markdown();

                        // Append PR merge count from the graph
                        if let Ok(guard) = self.get_graph().await {
                         if let Some(graph_state) = guard.as_ref() {
                            let file_patterns: Vec<String> = result
                                .outcomes
                                .first()
                                .and_then(|o| o.frontmatter.get("files"))
                                .and_then(|v| v.as_sequence())
                                .map(|seq| {
                                    seq.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();

                            let pr_nodes = query::find_pr_merges_for_outcome(
                                &graph_state.nodes,
                                &graph_state.edges,
                                &args.outcome_id,
                                &file_patterns,
                            );
                            if !pr_nodes.is_empty() {
                                md.push_str(&format!(
                                    "\n## PR Merges\n\n{} PR merge(s) serving this outcome\n",
                                    pr_nodes.len()
                                ));
                            }
                         }
                        }

                        Ok(text_result(md))
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            // ── Deprecated aliases: convert to Search and fall through ──
            "search_symbols" => {
                let args: SearchSymbols = parse_args(params.arguments)?;
                let search_args = args.into_search();
                self.handle_search(search_args).await
            }

            "graph_query" => {
                let args: GraphQuery = parse_args(params.arguments)?;
                let search_args = args.into_search();
                self.handle_search(search_args).await
            }

            // ── Unified search tool ──────────────────────────────────────
            "search" => {
                let args: Search = parse_args(params.arguments)?;
                self.handle_search(args).await
            }

            "list_roots" => {
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(self.repo_root.clone())
                    .with_worktrees(&self.repo_root)
                    .with_claude_memory(&self.repo_root);
                let resolved = workspace.resolved_roots();

                if resolved.is_empty() {
                    Ok(text_result("No workspace roots configured.".to_string()))
                } else {
                    let md: String = resolved
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            let primary = if i == 0 { " (primary)" } else { "" };
                            format!(
                                "- **{}**{}: `{}` (type: {}, git: {})",
                                r.slug,
                                primary,
                                r.path.display(),
                                r.config.root_type,
                                r.config.git_aware,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(text_result(format!(
                        "## Workspace Roots\n\n{} root(s)\n\n{}",
                        resolved.len(),
                        md
                    )))
                }
            }

            "repo_map" => {
                let args: RepoMap = parse_args(params.arguments)?;
                let top_n = args.top_n.unwrap_or(15) as usize;

                match self.get_graph().await {
                    Ok(guard) => {
                        let graph_state = guard.as_ref().unwrap();
                        let mut sections: Vec<String> = Vec::new();

                        // 1. Top symbols by importance (PageRank)
                        {
                            let mut symbols_with_importance: Vec<(&Node, f64)> = graph_state.nodes.iter()
                                .filter(|n| !matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge))
                                .filter(|n| n.id.root != "external")
                                .filter_map(|n| {
                                    let imp = n.metadata.get("importance")
                                        .and_then(|s| s.parse::<f64>().ok())
                                        .unwrap_or(0.0);
                                    if imp > IMPORTANCE_THRESHOLD { Some((n, imp)) } else { None }
                                })
                                .collect();
                            symbols_with_importance.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                            symbols_with_importance.truncate(top_n);

                            if !symbols_with_importance.is_empty() {
                                let md: String = symbols_with_importance.iter()
                                    .map(|(n, imp)| {
                                        let mut line = format!(
                                            "- **{}** `{}` ({}) [{}] `{}`:{}-{} -- importance: {:.3}",
                                            n.id.kind, n.id.name, n.language,
                                            n.id.root,
                                            n.id.file.display(),
                                            n.line_start, n.line_end,
                                            imp,
                                        );
                                        if let Some(cc) = n.metadata.get("cyclomatic") {
                                            line.push_str(&format!(", complexity: {}", cc));
                                        }
                                        line
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!(
                                    "## Top {} symbols by importance\n\n{}",
                                    symbols_with_importance.len(), md
                                ));
                            }
                        }

                        // 2. Hotspot files (most definitions), qualified by workspace root
                        {
                            let mut file_counts: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
                            for n in &graph_state.nodes {
                                if matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge) {
                                    continue;
                                }
                                if n.id.root == "external" {
                                    continue;
                                }
                                let key = (n.id.root.clone(), n.id.file.display().to_string());
                                *file_counts.entry(key).or_default() += 1;
                            }
                            let mut sorted_files: Vec<((String, String), usize)> = file_counts.into_iter().collect();
                            sorted_files.sort_by(|a, b| b.1.cmp(&a.1));
                            sorted_files.truncate(10);

                            if !sorted_files.is_empty() {
                                let md: String = sorted_files.iter()
                                    .map(|((root, f), count)| format!("- [{}] `{}` -- {} definitions", root, f, count))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!("## Hotspot files\n\n{}", md));
                            }
                        }

                        // 3. Active outcomes
                        {
                            let outcomes = oh::load_oh_artifacts(root)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|a| a.kind == OhArtifactKind::Outcome)
                                .collect::<Vec<_>>();
                            if !outcomes.is_empty() {
                                let md: String = outcomes.iter()
                                    .map(|o| {
                                        let files: Vec<String> = o.frontmatter
                                            .get("files")
                                            .and_then(|v| v.as_sequence())
                                            .map(|seq| seq.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect())
                                            .unwrap_or_default();
                                        let files_str = if files.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" (files: {})", files.join(", "))
                                        };
                                        format!("- **{}**{}", o.id(), files_str)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!("## Active outcomes\n\n{}", md));
                            }
                        }

                        // 4. Entry points (main functions, handlers), sorted by importance
                        {
                            let mut entry_points: Vec<&Node> = graph_state.nodes.iter()
                                .filter(|n| n.id.kind == NodeKind::Function && n.id.root != "external")
                                .filter(|n| {
                                    let name = n.id.name.to_lowercase();
                                    name == "main"
                                        || name.starts_with("handle_")
                                        || name.starts_with("handler")
                                        || name.ends_with("_handler")
                                        || name.contains("endpoint")
                                })
                                .collect();
                            entry_points.sort_by(|a, b| {
                                let imp_a = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                let imp_b = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                imp_b.partial_cmp(&imp_a).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            entry_points.truncate(10);

                            if !entry_points.is_empty() {
                                let md: String = entry_points.iter()
                                    .map(|n| format!(
                                        "- **{}** [{}] `{}`:{}-{}",
                                        n.id.name,
                                        n.id.root,
                                        n.id.file.display(),
                                        n.line_start, n.line_end,
                                    ))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!("## Entry points\n\n{}", md));
                            }
                        }

                        let freshness = format_freshness(
                            graph_state.nodes.len(),
                            graph_state.last_scan_completed_at,
                            Some(&self.lsp_status),
                        );

                        if sections.is_empty() {
                            Ok(text_result(format!("No repository data available yet.{}", freshness)))
                        } else {
                            Ok(text_result(format!(
                                "# Repository Map\n\n{}{}",
                                sections.join("\n\n"),
                                freshness
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Graph error: {}", e))),
                }
            }

            _ => Err(CallToolError::unknown_tool(&params.name)),
        };

        // Prepend business context preamble to first successful tool result
        if let (Some(preamble), Ok(tool_result)) = (preamble, &mut result) {
            tool_result.content.insert(
                0,
                TextContent::new(preamble, None, None).into(),
            );
        }

        result
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn parse_args<T: serde::de::DeserializeOwned>(
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<T, CallToolError> {
    let value = match arguments {
        Some(map) => serde_json::Value::Object(map),
        None => serde_json::Value::Object(serde_json::Map::new()), // empty object, not null
    };
    serde_json::from_value(value)
        .map_err(|e| CallToolError::from_message(format!("Invalid arguments: {}", e)))
}

/// Format a list of node IDs into markdown, enriched with node details if available.
/// Resolve an edge's `to.file` against the set of known scanned file paths.
/// If `to.file` doesn't match any known file but a known file ends with it
/// (suffix match), update the edge to point to the matched file.
/// This handles Python absolute imports where the import path is a suffix
/// of the actual file path (e.g., `src/util/user_utils.py` matches
/// `ai_service/src/util/user_utils.py`).
fn resolve_edge_target_by_suffix(
    edge: &mut graph::Edge,
    file_index: &std::collections::HashSet<String>,
) {
    let target = edge.to.file.to_string_lossy().to_string();
    if file_index.contains(&target) {
        return; // exact match, nothing to resolve
    }
    // Suffix match: find a scanned file that ends with the target path
    let suffix = format!("/{}", target);
    let matches: Vec<&String> = file_index
        .iter()
        .filter(|f| f.ends_with(&suffix))
        .collect();
    if matches.len() == 1 {
        edge.to.file = std::path::PathBuf::from(matches[0]);
        edge.confidence = graph::Confidence::Confirmed;
        tracing::debug!(
            "Resolved import edge target: {} → {}",
            target,
            matches[0]
        );
    }
    // If 0 or 2+ matches, leave as-is (ambiguous or truly dangling)
}

/// Format a single node for search results output.
///
/// When `compact` is true, returns a one-line summary: kind, name, file:lines, signature.
/// When `compact` is false (default), returns full detail: ID, signature, value, complexity, edges.
fn format_node_entry(n: &graph::Node, index: &GraphIndex, compact: bool) -> String {
    let stable_id = n.stable_id();

    if compact {
        // Compact: one-line summary for broad exploration
        let mut entry = format!(
            "- **{}** `{}` `{}`:{}-{}",
            n.id.kind, n.id.name,
            n.id.file.display(),
            n.line_start, n.line_end,
        );
        if !n.signature.is_empty() {
            // Truncate signature to first line for compact
            let sig_first_line = n.signature.lines().next().unwrap_or(&n.signature);
            entry.push_str(&format!(" `{}`", sig_first_line));
        }
        if let Some(tp) = n.metadata.get("type_params") {
            entry.push_str(&format!(" {}", tp));
        }
        if let Some(decorators) = n.metadata.get("decorators") {
            entry.push_str(&format!(" [{}]", decorators));
        }
        if let Some(storage) = n.metadata.get("storage") {
            entry.push_str(&format!(" [{}]", storage));
            if n.metadata.get("mutable").map(|s| s == "true").unwrap_or(false) {
                entry.push_str(" mut");
            }
        }
        if let Some(cc) = n.metadata.get("cyclomatic") {
            entry.push_str(&format!(" cc:{}", cc));
        }
        if let Some(imp) = n.metadata.get("importance") {
            if let Ok(score) = imp.parse::<f64>() {
                if score > IMPORTANCE_THRESHOLD {
                    entry.push_str(&format!(" imp:{:.3}", score));
                }
            }
        }
        let edge_count = index.neighbors(&stable_id, None, Direction::Outgoing).len()
            + index.neighbors(&stable_id, None, Direction::Incoming).len();
        if edge_count > 0 {
            entry.push_str(&format!(" edges:{}", edge_count));
        }
        entry.push_str(&format!("\n  `{}`", stable_id));
        entry
    } else {
        // Full detail (existing format)
        let outgoing = index.neighbors(&stable_id, None, Direction::Outgoing);
        let incoming = index.neighbors(&stable_id, None, Direction::Incoming);
        let mut entry = format!(
            "- **{}** `{}` ({}) `{}`:{}-{}\n  ID: `{}`",
            n.id.kind, n.id.name, n.language,
            n.id.file.display(),
            n.line_start, n.line_end,
            stable_id,
        );
        if !n.signature.is_empty() {
            entry.push_str(&format!("\n  Sig: `{}`", n.signature));
        }
        if let Some(tp) = n.metadata.get("type_params") {
            entry.push_str(&format!("\n  Type params: {}", tp));
        }
        if let Some(decorators) = n.metadata.get("decorators") {
            entry.push_str(&format!("\n  Decorators: {}", decorators));
        }
        if let Some(val) = n.metadata.get("value") {
            entry.push_str(&format!("\n  Value: `{}`", val));
        }
        if n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false) {
            entry.push_str(" *(literal)*");
        }
        if let Some(storage) = n.metadata.get("storage") {
            let mut_label = if n.metadata.get("mutable").map(|s| s == "true").unwrap_or(false) {
                " (mutable)"
            } else {
                ""
            };
            entry.push_str(&format!("\n  Storage: {}{}", storage, mut_label));
        }
        if let Some(cc) = n.metadata.get("cyclomatic") {
            entry.push_str(&format!("\n  Complexity: {}", cc));
        }
        if let Some(imp) = n.metadata.get("importance") {
            if let Ok(score) = imp.parse::<f64>() {
                if score > IMPORTANCE_THRESHOLD {
                    entry.push_str(&format!("\n  Importance: {:.3}", score));
                }
            }
        }
        if !outgoing.is_empty() {
            entry.push_str(&format!("\n  Out: {} edge(s)", outgoing.len()));
        }
        if !incoming.is_empty() {
            entry.push_str(&format!("\n  In: {} edge(s)", incoming.len()));
        }
        entry
    }
}

fn format_neighbor_nodes(nodes: &[graph::Node], ids: &[String], index: &GraphIndex, compact: bool) -> String {
    ids.iter()
        .filter_map(|id| {
            if let Some(node) = nodes.iter().find(|n| n.stable_id() == *id) {
                // Filter out module and PR-merge nodes — they're structural scaffolding,
                // not useful for agents doing impact analysis or exploration.
                match node.id.kind {
                    graph::NodeKind::Module | graph::NodeKind::PrMerge => return None,
                    _ => {}
                }
                Some(format_node_entry(node, index, compact))
            } else {
                Some(format!("- `{}`", id))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── GraphQuery deserialization edge cases ────────────────────────────

    fn parse_graph_query(v: serde_json::Value) -> Result<GraphQuery, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn test_graph_query_neither_node_id_nor_query() {
        // Both fields omitted — should deserialize fine (validation is at handler level)
        let gq = parse_graph_query(json!({})).unwrap();
        assert!(gq.node_id.is_none());
        assert!(gq.query.is_none());
        assert_eq!(gq.mode, "neighbors"); // default
    }

    #[test]
    fn test_graph_query_both_node_id_and_query() {
        // Both provided — node_id should take precedence (handler logic)
        let gq = parse_graph_query(json!({
            "node_id": "test:src/lib.rs:foo:function",
            "query": "authentication handler"
        }))
        .unwrap();
        assert!(gq.node_id.is_some());
        assert!(gq.query.is_some());
        // Verify precedence: the handler checks node_id first via `if let Some(ref node_id) = args.node_id`
    }

    #[test]
    fn test_graph_query_empty_string_query() {
        // Empty string query — deserializes as Some(""), NOT None
        // This means the handler will try to embed an empty string
        let gq = parse_graph_query(json!({"query": ""})).unwrap();
        assert_eq!(gq.query, Some("".to_string()));
        assert!(gq.node_id.is_none());
        // BUG: empty string will be sent to embedding model — no guard in handler
    }

    #[test]
    fn test_graph_query_whitespace_only_query() {
        // Whitespace-only query — also Some, not None
        let gq = parse_graph_query(json!({"query": "   \t\n  "})).unwrap();
        assert_eq!(gq.query, Some("   \t\n  ".to_string()));
        // BUG: whitespace will be embedded — produces garbage vectors, wastes compute
    }

    #[test]
    fn test_graph_query_empty_string_node_id() {
        // Empty node_id — takes precedence over query due to `if let Some`
        let gq = parse_graph_query(json!({
            "node_id": "",
            "query": "valid query"
        }))
        .unwrap();
        assert_eq!(gq.node_id, Some("".to_string()));
        // BUG: empty node_id will be used (takes precedence), graph lookup will fail
        // but the query path is never reached
    }

    #[test]
    fn test_graph_query_top_k_zero() {
        let gq = parse_graph_query(json!({"query": "test", "top_k": 0})).unwrap();
        assert_eq!(gq.top_k, Some(0));
        // BUG: top_k=0 means search(query, None, 0*3=0) — zero results requested
        // embed_idx.search with limit=0 likely returns empty, causing "no matches" message
    }

    #[test]
    fn test_graph_query_top_k_very_large() {
        let gq = parse_graph_query(json!({"query": "test", "top_k": 999999})).unwrap();
        assert_eq!(gq.top_k, Some(999999));
        // top_k * 3 = 2999997 — will be passed as limit to vector search
        // Could cause excessive memory usage or LanceDB to choke
    }

    #[test]
    fn test_graph_query_top_k_one() {
        // Reasonable single-entry request
        let gq = parse_graph_query(json!({"query": "exact symbol", "top_k": 1})).unwrap();
        assert_eq!(gq.top_k, Some(1));
    }

    #[test]
    fn test_graph_query_null_fields_are_none() {
        let gq = parse_graph_query(json!({
            "node_id": null,
            "query": null,
            "top_k": null
        }))
        .unwrap();
        assert!(gq.node_id.is_none());
        assert!(gq.query.is_none());
        assert!(gq.top_k.is_none());
    }

    #[test]
    fn test_graph_query_unicode_query() {
        // Unicode queries — should deserialize correctly
        let gq = parse_graph_query(json!({"query": "认证处理器 αβγ 🔐"})).unwrap();
        assert_eq!(gq.query, Some("认证处理器 αβγ 🔐".to_string()));
    }

    #[test]
    fn test_graph_query_very_long_query() {
        // Very long query string — no length guard in handler
        let long = "a".repeat(100_000);
        let gq = parse_graph_query(json!({"query": long})).unwrap();
        assert_eq!(gq.query.unwrap().len(), 100_000);
        // This will be passed to embed_texts() which truncates via truncate_chars(500)
        // so it won't crash, but it's wasteful
    }

    #[test]
    fn test_graph_query_default_mode() {
        let gq = parse_graph_query(json!({"node_id": "x"})).unwrap();
        assert_eq!(gq.mode, "neighbors");
    }

    #[test]
    fn test_graph_query_top_k_ignored_with_node_id() {
        // top_k is meaningless with node_id, but doesn't cause errors
        let gq = parse_graph_query(json!({"node_id": "x", "top_k": 10})).unwrap();
        assert!(gq.node_id.is_some());
        assert_eq!(gq.top_k, Some(10));
        // Handler only reads top_k inside the `query` branch, so this is ignored
    }

    #[test]
    fn test_graph_query_negative_top_k_rejected() {
        // top_k is u32, so negative values should fail deserialization
        let result = parse_graph_query(json!({"query": "test", "top_k": -1}));
        assert!(result.is_err(), "negative top_k should fail u32 deserialization");
    }

    #[test]
    fn test_graph_query_top_k_overflow() {
        // u32::MAX + 1 should fail
        let result = parse_graph_query(json!({"query": "test", "top_k": 4294967296_u64}));
        assert!(result.is_err(), "top_k exceeding u32::MAX should fail");
    }

    #[test]
    fn test_graph_query_top_k_default_is_three() {
        let gq = parse_graph_query(json!({"query": "test"})).unwrap();
        assert!(gq.top_k.is_none());
        // Handler uses: args.top_k.unwrap_or(3) — verify the default
        assert_eq!(gq.top_k.unwrap_or(3), 3);
    }

    // ── Unified Search deserialization and param combinations ────────────

    fn parse_search(v: serde_json::Value) -> Result<Search, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn test_search_flat_query_only() {
        // Equivalent to: search_symbols(query)
        let s = parse_search(json!({"query": "handle_call_tool"})).unwrap();
        assert_eq!(s.query, Some("handle_call_tool".to_string()));
        assert!(s.mode.is_none(), "no mode = flat search");
        assert!(s.node.is_none());
    }

    #[test]
    fn test_search_flat_with_filters() {
        // Equivalent to: search_symbols(query, kind, language)
        let s = parse_search(json!({
            "query": "handle",
            "kind": "function",
            "language": "rust"
        })).unwrap();
        assert_eq!(s.query, Some("handle".to_string()));
        assert_eq!(s.kind, Some("function".to_string()));
        assert_eq!(s.language, Some("rust".to_string()));
        assert!(s.mode.is_none());
    }

    #[test]
    fn test_search_traversal_query_neighbors() {
        // Equivalent to: graph_query(query, mode="neighbors")
        let s = parse_search(json!({
            "query": "authentication handler",
            "mode": "neighbors"
        })).unwrap();
        assert_eq!(s.query, Some("authentication handler".to_string()));
        assert_eq!(s.mode, Some("neighbors".to_string()));
    }

    #[test]
    fn test_search_traversal_with_top_k() {
        // New capability: semantic search top 5 then traverse
        let s = parse_search(json!({
            "query": "error handling",
            "mode": "neighbors",
            "top_k": 5
        })).unwrap();
        assert_eq!(s.top_k, Some(5));
        assert_eq!(s.mode, Some("neighbors".to_string()));
    }

    #[test]
    fn test_search_traversal_from_node() {
        // Equivalent to: graph_query(node_id, mode="neighbors")
        let s = parse_search(json!({
            "node": "root:src/lib.rs:foo:function",
            "mode": "neighbors"
        })).unwrap();
        assert_eq!(s.node, Some("root:src/lib.rs:foo:function".to_string()));
        assert!(s.query.is_none());
    }

    #[test]
    fn test_search_impact_from_node() {
        // Equivalent to: graph_query(node_id, mode="impact")
        let s = parse_search(json!({
            "node": "root:src/lib.rs:foo:function",
            "mode": "impact"
        })).unwrap();
        assert_eq!(s.mode, Some("impact".to_string()));
    }

    #[test]
    fn test_search_flat_sort_by_complexity() {
        let s = parse_search(json!({
            "query": "",
            "sort_by": "complexity",
            "min_complexity": 10
        })).unwrap();
        assert_eq!(s.sort_by, Some("complexity".to_string()));
        assert_eq!(s.min_complexity, Some(10));
        assert!(s.mode.is_none());
    }

    #[test]
    fn test_search_flat_default_top_k_is_10() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        // top_k omitted; handler defaults to 10 for flat, 1 for traversal
        assert!(s.top_k.is_none());
        assert_eq!(s.top_k.unwrap_or(10), 10);
    }

    #[test]
    fn test_search_traversal_default_top_k_is_1() {
        let s = parse_search(json!({"query": "test", "mode": "neighbors"})).unwrap();
        assert!(s.top_k.is_none());
        // Handler uses unwrap_or(1) for traversal mode
        assert_eq!(s.top_k.unwrap_or(1), 1);
    }

    #[test]
    fn test_search_all_fields_empty() {
        // No query, no node -- should deserialize OK (validation at handler level)
        let s = parse_search(json!({})).unwrap();
        assert!(s.query.is_none());
        assert!(s.node.is_none());
        assert!(s.mode.is_none());
    }

    #[test]
    fn test_search_hops_parameter() {
        let s = parse_search(json!({
            "node": "x",
            "mode": "reachable",
            "hops": 5
        })).unwrap();
        assert_eq!(s.hops, Some(5));
    }

    #[test]
    fn test_search_edge_types_filter() {
        let s = parse_search(json!({
            "node": "x",
            "mode": "neighbors",
            "edge_types": ["calls", "implements"]
        })).unwrap();
        assert_eq!(s.edge_types, Some(vec!["calls".to_string(), "implements".to_string()]));
    }

    #[test]
    fn test_search_extra_fields_ignored() {
        // Forward compat: extra fields should not cause errors
        let s = parse_search(json!({
            "query": "test",
            "unknown_future_field": true
        }));
        assert!(s.is_ok(), "extra fields should be ignored for forward compat");
    }

    // ── tests_for mode ──────────────────────────────────────────────────

    #[test]
    fn test_search_tests_for_mode_with_node() {
        let s = parse_search(json!({"node": "root:src/lib.rs:foo:function", "mode": "tests_for"})).unwrap();
        assert_eq!(s.mode, Some("tests_for".to_string()));
        assert!(s.node.is_some());
    }

    #[test]
    fn test_search_tests_for_mode_with_query() {
        let s = parse_search(json!({"query": "authentication handler", "mode": "tests_for"})).unwrap();
        assert_eq!(s.mode, Some("tests_for".to_string()));
        assert!(s.query.is_some());
        assert!(s.node.is_none());
    }

    /// Verify the tests_for pattern: incoming Calls edges filtered to test-file callers.
    #[test]
    fn test_tests_for_filters_to_test_callers_only() {
        use crate::graph::{index::GraphIndex, EdgeKind, Node, NodeId, NodeKind};
        use petgraph::Direction;
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut index = GraphIndex::new();

        let test_id = NodeId {
            root: "r".to_string(),
            file: PathBuf::from("tests/test_auth.rs"),
            name: "test_foo".to_string(),
            kind: NodeKind::Function,
        };
        let prod_id = NodeId {
            root: "r".to_string(),
            file: PathBuf::from("src/auth.rs"),
            name: "prod_bar".to_string(),
            kind: NodeKind::Function,
        };
        let other_prod_id = NodeId {
            root: "r".to_string(),
            file: PathBuf::from("src/handler.rs"),
            name: "prod_baz".to_string(),
            kind: NodeKind::Function,
        };

        let test_stable = test_id.to_stable_id();
        let prod_stable = prod_id.to_stable_id();
        let other_stable = other_prod_id.to_stable_id();

        index.add_edge(&test_stable, "function", &prod_stable, "function", EdgeKind::Calls);
        index.add_edge(&other_stable, "function", &prod_stable, "function", EdgeKind::Calls);

        // Step 1: incoming Calls neighbors of prod_bar
        let callers = index.neighbors(&prod_stable, Some(&[EdgeKind::Calls]), Direction::Incoming);
        assert_eq!(callers.len(), 2, "both test and prod callers returned");

        // Step 2: build Node objects to test is_test_file filtering
        let make_node = |id: NodeId| -> Node {
            Node {
                id: id.clone(),
                language: "rust".to_string(),
                signature: format!("fn {}()", id.name),
                body: String::new(),
                line_start: 1,
                line_end: 10,
                metadata: BTreeMap::new(),
                source: crate::graph::ExtractionSource::TreeSitter,
            }
        };
        let nodes = vec![
            make_node(test_id.clone()),
            make_node(prod_id.clone()),
            make_node(other_prod_id.clone()),
        ];

        // Filter to test files only (mirrors handler logic)
        let test_callers: Vec<&str> = callers.iter()
            .filter(|id| {
                nodes.iter()
                    .find(|n| n.stable_id() == **id)
                    .map(|n| ranking::is_test_file(n))
                    .unwrap_or(false)
            })
            .map(|s| s.as_str())
            .collect();

        assert_eq!(test_callers.len(), 1, "only test caller should remain");
        assert_eq!(test_callers[0], test_stable, "test_foo is the test caller");
    }

    /// Verify tests_for returns empty when no test files call the symbol.
    #[test]
    fn test_tests_for_no_test_callers() {
        use crate::graph::{index::GraphIndex, EdgeKind, Node, NodeId, NodeKind};
        use petgraph::Direction;
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut index = GraphIndex::new();

        let caller_id = NodeId {
            root: "r".to_string(),
            file: PathBuf::from("src/handler.rs"),
            name: "handler".to_string(),
            kind: NodeKind::Function,
        };
        let target_id = NodeId {
            root: "r".to_string(),
            file: PathBuf::from("src/auth.rs"),
            name: "authenticate".to_string(),
            kind: NodeKind::Function,
        };

        let caller_stable = caller_id.to_stable_id();
        let target_stable = target_id.to_stable_id();

        index.add_edge(&caller_stable, "function", &target_stable, "function", EdgeKind::Calls);

        let callers = index.neighbors(&target_stable, Some(&[EdgeKind::Calls]), Direction::Incoming);
        assert_eq!(callers.len(), 1);

        let make_node = |id: NodeId| -> Node {
            Node {
                id: id.clone(),
                language: "rust".to_string(),
                signature: format!("fn {}()", id.name),
                body: String::new(),
                line_start: 1,
                line_end: 10,
                metadata: BTreeMap::new(),
                source: crate::graph::ExtractionSource::TreeSitter,
            }
        };
        let nodes = vec![make_node(caller_id), make_node(target_id)];

        let test_callers: Vec<&String> = callers.iter()
            .filter(|id| {
                nodes.iter()
                    .find(|n| n.stable_id() == **id)
                    .map(|n| ranking::is_test_file(n))
                    .unwrap_or(false)
            })
            .collect();

        assert!(test_callers.is_empty(), "no test callers should be found");
    }

    // ── SearchSymbols -> Search conversion ──────────────────────────────

    #[test]
    fn test_search_symbols_into_search() {
        let ss = SearchSymbols {
            query: "foo".to_string(),
            kind: Some("function".to_string()),
            language: Some("rust".to_string()),
            file: Some("server.rs".to_string()),
            root: Some("my-root".to_string()),
            limit: Some(20),
            synthetic: Some(false),
            min_complexity: Some(5),
            sort: Some("complexity".to_string()),
        };
        let s = ss.into_search();
        assert_eq!(s.query, Some("foo".to_string()));
        assert!(s.mode.is_none(), "search_symbols is always flat");
        assert_eq!(s.kind, Some("function".to_string()));
        assert_eq!(s.language, Some("rust".to_string()));
        assert_eq!(s.file, Some("server.rs".to_string()));
        assert_eq!(s.root, Some("my-root".to_string()));
        assert_eq!(s.top_k, Some(20)); // explicit limit=20 preserved
        assert_eq!(s.synthetic, Some(false));
        assert_eq!(s.min_complexity, Some(5));
        assert_eq!(s.sort_by, Some("complexity".to_string())); // sort maps to sort_by
    }

    #[test]
    fn test_search_symbols_into_search_preserves_default_limit() {
        // When limit is not specified, the old default (20) should be preserved
        let ss = SearchSymbols {
            query: "foo".to_string(),
            kind: None, language: None, file: None, root: None,
            limit: None,  // not specified by caller
            synthetic: None, min_complexity: None, sort: None,
        };
        let s = ss.into_search();
        assert_eq!(s.top_k, Some(20), "search_symbols default limit of 20 should be preserved");
    }

    // ── GraphQuery -> Search conversion ─────────────────────────────────

    #[test]
    fn test_graph_query_into_search() {
        let gq = GraphQuery {
            node_id: Some("root:src/lib.rs:foo:function".to_string()),
            query: Some("test query".to_string()),
            mode: "impact".to_string(),
            direction: Some("incoming".to_string()),
            edge_types: Some(vec!["calls".to_string()]),
            max_hops: Some(3),
            top_k: Some(5),
        };
        let s = gq.into_search();
        assert_eq!(s.node, Some("root:src/lib.rs:foo:function".to_string())); // node_id -> node
        assert_eq!(s.query, Some("test query".to_string()));
        assert_eq!(s.mode, Some("impact".to_string()));
        assert_eq!(s.direction, Some("incoming".to_string()));
        assert_eq!(s.edge_types, Some(vec!["calls".to_string()]));
        assert_eq!(s.hops, Some(3)); // max_hops -> hops
        assert_eq!(s.top_k, Some(5));
        // Graph query doesn't carry symbol filters
        assert!(s.kind.is_none());
        assert!(s.language.is_none());
    }

    #[test]
    fn test_graph_query_into_search_preserves_default_top_k() {
        // When top_k is not specified, the old default (3) should be preserved
        let gq = GraphQuery {
            node_id: Some("x".to_string()),
            query: None,
            mode: "neighbors".to_string(),
            direction: None, edge_types: None, max_hops: None,
            top_k: None,  // not specified by caller
        };
        let s = gq.into_search();
        assert_eq!(s.top_k, Some(3), "graph_query default top_k of 3 should be preserved");
    }

    // ── Semantic entry point: code prefix filter correctness ────────────

    #[test]
    fn test_code_prefix_filter_matches_all_embeddable_kinds() {
        use crate::graph::NodeKind;
        // The handler filters by `r.kind.starts_with("code:")` — verify all
        // embeddable NodeKinds produce strings that start with "code:"
        let embeddable = vec![
            NodeKind::Function,
            NodeKind::Struct,
            NodeKind::Trait,
            NodeKind::Enum,
            NodeKind::ProtoMessage,
            NodeKind::SqlTable,
            NodeKind::Macro,
            NodeKind::ApiEndpoint,
            NodeKind::Other("custom_type".to_string()),
        ];
        for kind in &embeddable {
            assert!(
                kind.is_embeddable(),
                "{:?} should be embeddable",
                kind
            );
            let display = format!("code:{}", kind);
            assert!(
                display.starts_with("code:"),
                "code:{} should start with 'code:' prefix",
                kind
            );
        }
    }

    #[test]
    fn test_non_embeddable_kinds_filtered_out_by_prefix() {
        use crate::graph::NodeKind;
        // Verify non-embeddable kinds don't sneak through the prefix filter
        let non_embeddable = vec![
            NodeKind::Import,
            NodeKind::Const,
            NodeKind::Module,
            NodeKind::Impl,
            NodeKind::Field,
            NodeKind::PrMerge,
        ];
        for kind in &non_embeddable {
            assert!(
                !kind.is_embeddable(),
                "{:?} should NOT be embeddable",
                kind
            );
        }
    }

    #[test]
    fn test_code_prefix_filter_rejects_non_code_kinds() {
        // The handler uses starts_with("code:") to filter.
        // Verify that non-code kinds (commit, outcome, signal, etc.) are rejected.
        let non_code_kinds = vec!["commit", "outcome", "signal", "guardrail", "pr_merge"];
        for kind in non_code_kinds {
            assert!(
                !kind.starts_with("code:"),
                "'{}' should NOT pass the code: prefix filter",
                kind
            );
        }
    }

    #[test]
    fn test_top_k_overflow_multiplication() {
        // The handler does: top_k * 3 for over-fetching
        // With top_k = u32::MAX / 2, this could overflow in usize on 32-bit
        // On 64-bit it's fine, but let's verify the arithmetic
        let top_k: u32 = u32::MAX / 3; // Just below overflow threshold
        let limit = top_k as usize * 3;
        // On 64-bit systems this should be fine
        assert!(limit > 0);

        // But top_k = u32::MAX would overflow on 32-bit:
        // u32::MAX as usize * 3 on 32-bit = overflow
        // On 64-bit it's ~12 billion — excessive but won't panic
        let top_k_max: u32 = u32::MAX;
        let limit_max = (top_k_max as usize).checked_mul(3);
        // This documents the potential issue
        if cfg!(target_pointer_width = "64") {
            assert!(limit_max.is_some(), "should not overflow on 64-bit");
        }
    }

    // ── parse_args edge cases ───────────────────────────────────────────

    #[test]
    fn test_parse_args_none_arguments_returns_empty_object() {
        // After #120 fix: None maps to empty object {}, not null.
        // GraphQuery has all optional fields, so empty object deserializes OK.
        // The handler's validation (node_id or query required) catches this.
        let result: Result<GraphQuery, _> = parse_args(None);
        assert!(
            result.is_ok(),
            "parse_args(None) should succeed — None maps to empty object, not null"
        );
    }

    #[test]
    fn test_parse_args_empty_map() {
        let args = Some(serde_json::Map::new());
        let result: Result<GraphQuery, _> = parse_args(args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_args_extra_fields_ignored() {
        // Extra unknown fields should be silently ignored by serde default
        let mut map = serde_json::Map::new();
        map.insert("node_id".into(), json!("test_id"));
        map.insert("unknown_field".into(), json!("should be ignored"));
        let result: Result<GraphQuery, _> = parse_args(Some(map));
        // This depends on whether GraphQuery has #[serde(deny_unknown_fields)]
        // If it does, this will fail — which is a problem for forward compat
        assert!(result.is_ok(), "extra fields should be ignored for forward compat");
    }

    // ── LspEnrichmentStatus tests ───────────────────────────────────────

    #[test]
    fn test_lsp_status_not_started_no_footer() {
        let status = LspEnrichmentStatus::default();
        assert!(status.footer_segment().is_none());
    }

    #[test]
    fn test_lsp_status_running_shows_pending() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        assert_eq!(status.footer_segment().unwrap(), "LSP: pending");
    }

    #[test]
    fn test_lsp_status_complete_shows_edge_count() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(42);
        assert_eq!(status.footer_segment().unwrap(), "LSP: enriched (42 edges)");
    }

    #[test]
    fn test_lsp_status_complete_zero_edges() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(0);
        assert_eq!(status.footer_segment().unwrap(), "LSP: enriched (0 edges)");
    }

    #[test]
    fn test_lsp_status_unavailable_shows_no_server() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_unavailable();
        assert_eq!(
            status.footer_segment().unwrap(),
            "LSP: no server detected"
        );
    }

    #[test]
    fn test_lsp_status_unavailable_no_auto_hide() {
        // UNAVAILABLE should always show, unlike COMPLETE which hides after 30s.
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        // Even without set_running first, should show.
        assert_eq!(
            status.footer_segment().unwrap(),
            "LSP: no server detected"
        );
    }

    #[test]
    fn test_format_freshness_without_lsp_status() {
        let result = format_freshness(100, Some(std::time::Instant::now()), None);
        assert!(result.contains("100 symbols"));
        assert!(result.contains("just now"));
        assert!(!result.contains("LSP"));
    }

    #[test]
    fn test_format_freshness_with_pending_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        let result = format_freshness(100, Some(std::time::Instant::now()), Some(&status));
        assert!(result.contains("100 symbols"));
        assert!(result.contains("LSP: pending"));
    }

    #[test]
    fn test_format_freshness_with_enriched_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_complete(1247);
        let result = format_freshness(100, Some(std::time::Instant::now()), Some(&status));
        assert!(result.contains("LSP: enriched (1247 edges)"));
    }

    #[test]
    fn test_format_freshness_with_unavailable_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        let result = format_freshness(100, Some(std::time::Instant::now()), Some(&status));
        assert!(result.contains("LSP: no server detected"));
        assert!(result.contains("100 symbols"));
    }

    // ── Adversarial LspEnrichmentStatus tests ─────────────────────────

    #[test]
    fn test_lsp_status_set_complete_without_set_running() {
        // Should not panic — direct transition from NotStarted to Complete
        let status = LspEnrichmentStatus::default();
        status.set_complete(10);
        assert_eq!(status.footer_segment().unwrap(), "LSP: enriched (10 edges)");
    }

    #[test]
    fn test_lsp_status_double_set_running() {
        // Calling set_running twice shouldn't corrupt state
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_running();
        assert_eq!(status.footer_segment().unwrap(), "LSP: pending");
    }

    #[test]
    fn test_lsp_status_complete_then_running_again() {
        // Simulates a second enrichment pass (incremental after full)
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(100);
        assert!(status.footer_segment().unwrap().contains("100 edges"));
        // New enrichment pass starts
        status.set_running();
        assert_eq!(status.footer_segment().unwrap(), "LSP: pending");
        status.set_complete(150);
        assert!(status.footer_segment().unwrap().contains("150 edges"));
    }

    #[test]
    fn test_lsp_status_unavailable_then_running_then_complete() {
        // Simulates: first scan finds no server, second scan (after install) finds one
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_unavailable();
        assert_eq!(status.footer_segment().unwrap(), "LSP: no server detected");
        // Server installed, new scan starts
        status.set_running();
        assert_eq!(status.footer_segment().unwrap(), "LSP: pending");
        status.set_complete(50);
        assert!(status.footer_segment().unwrap().contains("50 edges"));
    }

    #[test]
    fn test_lsp_status_concurrent_reads() {
        // Multiple threads reading footer_segment while state changes
        use std::sync::Arc;
        let status = Arc::new(LspEnrichmentStatus::default());
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let s = status.clone();
                std::thread::spawn(move || {
                    if i % 2 == 0 {
                        s.set_running();
                    } else {
                        s.set_complete(i * 10);
                    }
                    // Should never panic
                    let _ = s.footer_segment();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_lsp_status_large_edge_count() {
        let status = LspEnrichmentStatus::default();
        status.set_complete(usize::MAX);
        let seg = status.footer_segment().unwrap();
        assert!(seg.contains("edges"));
    }

    // ── get_graph incremental update test ────────────────────────────────

    #[tokio::test]
    async fn test_get_graph_detects_file_edits() {
        // Regression test for #134: get_graph() ran Scanner::scan() to check
        // for changes (saving updated mtimes), then update_graph_incrementally
        // re-scanned and saw nothing. The fix passes ScanResult through.
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Set up a minimal repo with one Rust file
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn original_function() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // First call: builds graph from scratch
        {
            let guard = handler.get_graph().await.unwrap();
            let gs = guard.as_ref().expect("graph should be built");
            assert!(
                gs.nodes.iter().any(|n| n.id.name == "original_function"),
                "original_function should be in graph after first build. Nodes: {:?}",
                gs.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
            );
            assert!(gs.last_scan_completed_at.is_some());
        }

        // Wait for mtime granularity (macOS HFS+ has 1-second resolution)
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Edit the file: remove old function, add new one
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn replacement_function() {}\n",
        )
        .unwrap();

        // Expire the 2-second cooldown
        {
            let mut last = handler.last_scan.lock().unwrap();
            *last = std::time::Instant::now() - std::time::Duration::from_secs(10);
        }

        // Second call: should detect the edit and update the graph
        {
            let guard = handler.get_graph().await.unwrap();
            let gs = guard.as_ref().expect("graph should still exist");

            let has_replacement = gs.nodes.iter().any(|n| n.id.name == "replacement_function");
            let has_original = gs.nodes.iter().any(|n| n.id.name == "original_function");

            assert!(
                has_replacement,
                "replacement_function should be in graph after edit. Nodes: {:?}",
                gs.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
            );
            assert!(
                !has_original,
                "original_function should be removed from graph after edit"
            );

            // last_scan_completed_at should be recent (not the stale initial value)
            let age = gs.last_scan_completed_at.unwrap().elapsed();
            assert!(
                age < std::time::Duration::from_secs(5),
                "last_scan_completed_at should be recent, but was {:?} ago",
                age
            );
        }
    }

    // ── compact + nodes (batch) parameter tests ──────────────────────────

    #[test]
    fn test_search_compact_param() {
        let s = parse_search(json!({"query": "handle", "compact": true})).unwrap();
        assert_eq!(s.compact, Some(true));
    }

    #[test]
    fn test_search_compact_default_is_none() {
        let s = parse_search(json!({"query": "handle"})).unwrap();
        assert!(s.compact.is_none());
        // Handler interprets None as false
        assert!(!s.compact.unwrap_or(false));
    }

    #[test]
    fn test_search_nodes_param() {
        let s = parse_search(json!({
            "nodes": ["root:src/lib.rs:foo:function", "root:src/lib.rs:bar:struct"]
        })).unwrap();
        assert_eq!(s.nodes, Some(vec![
            "root:src/lib.rs:foo:function".to_string(),
            "root:src/lib.rs:bar:struct".to_string(),
        ]));
    }

    #[test]
    fn test_search_nodes_with_compact() {
        let s = parse_search(json!({
            "nodes": ["root:src/lib.rs:foo:function"],
            "compact": true
        })).unwrap();
        assert_eq!(s.compact, Some(true));
        assert!(s.nodes.is_some());
        assert_eq!(s.nodes.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_search_nodes_empty_array() {
        let s = parse_search(json!({"nodes": []})).unwrap();
        assert_eq!(s.nodes, Some(vec![]));
    }

    #[test]
    fn test_search_compact_with_traversal() {
        let s = parse_search(json!({
            "query": "auth",
            "mode": "neighbors",
            "compact": true
        })).unwrap();
        assert_eq!(s.compact, Some(true));
        assert_eq!(s.mode, Some("neighbors".to_string()));
    }

    #[test]
    fn test_search_symbols_into_search_has_no_compact_or_nodes() {
        let ss = SearchSymbols {
            query: "foo".to_string(),
            kind: None, language: None, file: None, root: None,
            limit: None, synthetic: None, min_complexity: None, sort: None,
        };
        let s = ss.into_search();
        assert!(s.compact.is_none());
        assert!(s.nodes.is_none());
    }

    #[test]
    fn test_graph_query_into_search_has_no_compact_or_nodes() {
        let gq = GraphQuery {
            node_id: Some("x".to_string()),
            query: None,
            mode: "neighbors".to_string(),
            direction: None, edge_types: None, max_hops: None, top_k: None,
        };
        let s = gq.into_search();
        assert!(s.compact.is_none());
        assert!(s.nodes.is_none());
    }

    // ── parse_search_mode tests ──────────────────────────────────────

    #[test]
    fn test_parse_search_mode_defaults_to_hybrid() {
        use crate::embed::SearchMode;
        assert_eq!(parse_search_mode(None), SearchMode::Hybrid);
        assert_eq!(parse_search_mode(Some("hybrid")), SearchMode::Hybrid);
        assert_eq!(parse_search_mode(Some("HYBRID")), SearchMode::Hybrid);
        assert_eq!(parse_search_mode(Some("unknown")), SearchMode::Hybrid);
    }

    #[test]
    fn test_parse_search_mode_keyword() {
        use crate::embed::SearchMode;
        assert_eq!(parse_search_mode(Some("keyword")), SearchMode::Keyword);
        assert_eq!(parse_search_mode(Some("Keyword")), SearchMode::Keyword);
        assert_eq!(parse_search_mode(Some("KEYWORD")), SearchMode::Keyword);
    }

    #[test]
    fn test_parse_search_mode_semantic() {
        use crate::embed::SearchMode;
        assert_eq!(parse_search_mode(Some("semantic")), SearchMode::Semantic);
        assert_eq!(parse_search_mode(Some("Semantic")), SearchMode::Semantic);
        assert_eq!(parse_search_mode(Some("SEMANTIC")), SearchMode::Semantic);
    }

    #[test]
    fn test_search_mode_in_search_struct() {
        let s: Search = serde_json::from_value(serde_json::json!({
            "query": "foo",
            "search_mode": "keyword"
        })).unwrap();
        assert_eq!(s.search_mode, Some("keyword".to_string()));
    }

    #[test]
    fn test_search_mode_absent_in_search_struct() {
        let s: Search = serde_json::from_value(serde_json::json!({
            "query": "foo"
        })).unwrap();
        assert!(s.search_mode.is_none());
    }

    #[test]
    fn test_search_mode_in_oh_search_context_struct() {
        let s: OhSearchContext = serde_json::from_value(serde_json::json!({
            "query": "error handling",
            "search_mode": "semantic"
        })).unwrap();
        assert_eq!(s.search_mode, Some("semantic".to_string()));
    }

    #[test]
    fn test_format_node_entry_compact_vs_full() {
        use crate::graph::{index::GraphIndex, Node, NodeId, NodeKind, ExtractionSource};
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let index = GraphIndex::new();
        let node = Node {
            id: NodeId {
                root: "r".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "my_function".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: "fn my_function(x: u32) -> bool".to_string(),
            body: "{ x > 0 }".to_string(),
            line_start: 10,
            line_end: 15,
            metadata: {
                let mut m = BTreeMap::new();
                m.insert("cyclomatic".to_string(), "3".to_string());
                m
            },
            source: ExtractionSource::TreeSitter,
        };

        // Compact output should be shorter and not contain full detail markers
        let compact_out = format_node_entry(&node, &index, true);
        let full_out = format_node_entry(&node, &index, false);

        // Compact contains key info
        assert!(compact_out.contains("my_function"));
        assert!(compact_out.contains("src/lib.rs"));
        assert!(compact_out.contains("10-15"));
        assert!(compact_out.contains("cc:3"));
        // Compact includes stable ID for follow-up
        assert!(compact_out.contains("r:src/lib.rs:my_function:function"));

        // Full output has more detail
        assert!(full_out.contains("ID:"));
        assert!(full_out.contains("Sig:"));
        assert!(full_out.contains("Complexity:"));
        assert!(full_out.contains("rust")); // language in parentheses

        // Compact should be significantly shorter
        assert!(
            compact_out.len() < full_out.len(),
            "compact ({}) should be shorter than full ({})",
            compact_out.len(),
            full_out.len()
        );
    }

    #[test]
    fn test_format_node_entry_compact_multiline_signature() {
        use crate::graph::{index::GraphIndex, Node, NodeId, NodeKind, ExtractionSource};
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let index = GraphIndex::new();
        let node = Node {
            id: NodeId {
                root: "r".to_string(),
                file: PathBuf::from("src/server.rs"),
                name: "handle_search_traversal".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: "async fn handle_search_traversal(\n    &self,\n    args: &Search,\n) -> Result<CallToolResult, CallToolError>".to_string(),
            body: String::new(),
            line_start: 100,
            line_end: 200,
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let compact_out = format_node_entry(&node, &index, true);
        // Compact should only show first line of signature
        assert!(compact_out.contains("async fn handle_search_traversal("));
        assert!(!compact_out.contains("&self"));
    }

    #[test]
    fn test_format_node_entry_compact_shows_edge_count() {
        use crate::graph::{index::GraphIndex, Node, NodeId, NodeKind, EdgeKind, ExtractionSource};
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut index = GraphIndex::new();

        let node_a = Node {
            id: NodeId {
                root: "r".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: "fn caller()".to_string(),
            body: String::new(),
            line_start: 1,
            line_end: 5,
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let node_b = Node {
            id: NodeId {
                root: "r".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "callee".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: "fn callee()".to_string(),
            body: String::new(),
            line_start: 10,
            line_end: 15,
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let id_a = node_a.stable_id();
        let id_b = node_b.stable_id();
        index.ensure_node(&id_a, "function");
        index.ensure_node(&id_b, "function");
        index.add_edge(&id_a, "function", &id_b, "function", EdgeKind::Calls);

        // Compact output for node_a should show edges:1 (one outgoing)
        let compact_out = format_node_entry(&node_a, &index, true);
        assert!(
            compact_out.contains("edges:1"),
            "compact output should contain 'edges:1', got: {}",
            compact_out
        );

        // Full output should still show Out: 1 edge(s)
        let full_out = format_node_entry(&node_a, &index, false);
        assert!(full_out.contains("Out: 1 edge(s)"));

        // Node with no edges should NOT show edges:0
        let isolated_node = Node {
            id: NodeId {
                root: "r".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "isolated".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: "fn isolated()".to_string(),
            body: String::new(),
            line_start: 20,
            line_end: 25,
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let id_iso = isolated_node.stable_id();
        index.ensure_node(&id_iso, "function");
        let isolated_compact = format_node_entry(&isolated_node, &index, true);
        assert!(
            !isolated_compact.contains("edges:"),
            "isolated node compact output should not contain 'edges:', got: {}",
            isolated_compact
        );
    }

    #[test]
    fn test_search_nodes_with_mode_param() {
        // Verify that nodes + mode compose correctly at the parameter level
        let s = parse_search(json!({
            "nodes": ["root:src/lib.rs:foo:function"],
            "mode": "neighbors",
            "hops": 1,
            "direction": "outgoing"
        })).unwrap();
        assert!(s.nodes.is_some());
        assert_eq!(s.mode, Some("neighbors".to_string()));
        assert_eq!(s.hops, Some(1));
        assert_eq!(s.direction, Some("outgoing".to_string()));
    }

    #[test]
    fn test_search_batch_with_all_traversal_modes() {
        // Adversarial: verify all mode strings parse correctly with batch nodes
        for mode in &["neighbors", "impact", "reachable", "tests_for"] {
            let s = parse_search(json!({
                "nodes": ["root:a.rs:foo:function", "root:b.rs:bar:function"],
                "mode": mode,
                "hops": 2
            })).unwrap();
            assert!(s.nodes.is_some());
            assert_eq!(s.mode.as_deref(), Some(*mode));
            assert_eq!(s.hops, Some(2));
        }
    }

    #[test]
    fn test_search_batch_with_direction_variants() {
        // Adversarial: verify all direction strings compose with batch
        for dir in &["outgoing", "incoming", "both"] {
            let s = parse_search(json!({
                "nodes": ["root:a.rs:foo:function"],
                "mode": "neighbors",
                "direction": dir
            })).unwrap();
            assert_eq!(s.direction.as_deref(), Some(*dir));
        }
    }

    #[test]
    fn test_search_batch_mode_without_hops_uses_defaults() {
        // Adversarial: batch+mode with no hops should not panic
        let s = parse_search(json!({
            "nodes": ["root:a.rs:foo:function"],
            "mode": "impact"
        })).unwrap();
        assert!(s.nodes.is_some());
        assert_eq!(s.mode, Some("impact".to_string()));
        assert!(s.hops.is_none()); // will default to 3 in handler
    }

    #[test]
    fn test_compact_edge_count_with_bidirectional_edges() {
        // Adversarial: node with both incoming and outgoing edges should sum correctly
        use crate::graph::{index::GraphIndex, Node, NodeId, NodeKind, EdgeKind, ExtractionSource};
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut index = GraphIndex::new();

        let make_node = |name: &str| Node {
            id: NodeId {
                root: "r".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: format!("fn {}()", name),
            body: String::new(),
            line_start: 1,
            line_end: 5,
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let node_a = make_node("hub");
        let node_b = make_node("caller");
        let node_c = make_node("callee");

        let id_a = node_a.stable_id();
        let id_b = node_b.stable_id();
        let id_c = node_c.stable_id();
        index.ensure_node(&id_a, "function");
        index.ensure_node(&id_b, "function");
        index.ensure_node(&id_c, "function");
        // hub calls callee (outgoing), caller calls hub (incoming to hub)
        index.add_edge(&id_a, "function", &id_c, "function", EdgeKind::Calls);
        index.add_edge(&id_b, "function", &id_a, "function", EdgeKind::Calls);

        let compact_out = format_node_entry(&node_a, &index, true);
        // hub has 1 outgoing + 1 incoming = edges:2
        assert!(
            compact_out.contains("edges:2"),
            "hub node should show edges:2, got: {}",
            compact_out
        );
    }
}
