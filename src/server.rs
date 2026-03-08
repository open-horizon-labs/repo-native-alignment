use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arrow_array::{Array, BooleanArray, Int32Array, RecordBatch, RecordBatchIterator, StringArray, UInt32Array, Int64Array};
use arrow_array::builder::BooleanBuilder;
use async_trait::async_trait;
use rust_mcp_sdk::macros::{self, JsonSchema};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CallToolError, CallToolRequestParams, CallToolResult, ListToolsResult,
    PaginatedRequestParams, RpcError, TextContent,
};
use serde::{Deserialize, Serialize};

use crate::embed::EmbeddingIndex;
use crate::extract::{ExtractorRegistry, EnricherRegistry};
use crate::graph::{self, EdgeKind, Node, Edge, Confidence, ExtractionSource, NodeId, NodeKind};
use crate::graph::index::GraphIndex;
use crate::graph::store::{symbols_schema, edges_schema, schema_meta_schema, SCHEMA_VERSION};
use crate::roots::{WorkspaceConfig, cache_state_path};
use crate::scanner::Scanner;
use crate::types::OhArtifactKind;
use crate::{code, git, markdown, oh, query};
use petgraph::Direction;
use tokio::sync::RwLock;

// ── Tool input structs ──────────────────────────────────────────────

#[macros::mcp_tool(
    name = "oh_get_context",
    description = "Returns concise business context: outcomes, signals, guardrails, metis. Use this to understand project aims and constraints. For code exploration use search_symbols instead."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetContext {}

#[macros::mcp_tool(
    name = "oh_search_context",
    description = "Use this INSTEAD OF Grep for searching business context, commits, and optionally code/markdown. Describe what you need in natural language. Set include_code=true to also search code symbols, include_markdown=true for markdown sections."
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
}

#[macros::mcp_tool(
    name = "oh_record",
    description = "Record a business artifact: metis (learning), signal (measurement), guardrail (constraint), or update an outcome. The 'type' field determines which fields are used."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhRecord {
    /// Artifact type: "metis", "signal", "guardrail", "outcome"
    #[serde(rename = "type")]
    pub record_type: String,
    /// Slug/ID (required for all types)
    pub slug: String,
    /// Title (metis, signal)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Body/description (metis, signal, guardrail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Related outcome ID (metis, signal, guardrail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Signal type: slo, metric, qualitative (signals only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_type: Option<String>,
    /// Threshold (signals only, e.g. "p95 < 200ms")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<String>,
    /// Guardrail severity: candidate, soft, hard (guardrails only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Guardrail statement (guardrails only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    /// Outcome status update: active, achieved, paused, abandoned (outcomes only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Outcome mechanism update (outcomes only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<String>,
    /// Outcome files patterns (outcomes only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

#[macros::mcp_tool(
    name = "oh_init",
    description = "Scaffolds .oh/ directory structure for a repo. Reads CLAUDE.md, README.md, and recent git history to propose an initial outcome, signal, and guardrails. Idempotent — won't overwrite existing files."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhInit {
    /// Optional: name for the primary outcome (auto-detected from README/CLAUDE.md if omitted)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_name: Option<String>,
}

#[macros::mcp_tool(
    name = "outcome_progress",
    description = "The real intersection query: given an outcome ID, finds related commits (by [outcome:X] tags and file pattern matches), code symbols in changed files, and markdown mentioning the outcome. This joins layers structurally, not by keyword."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OutcomeProgress {
    /// The outcome ID (e.g. 'agent-alignment') from .oh/outcomes/
    pub outcome_id: String,
}

#[macros::mcp_tool(
    name = "search_symbols",
    description = "Use this INSTEAD OF Grep/Read for finding code symbols. Searches functions, structs, traits, classes, interfaces across Rust, Python, TypeScript, Go, Markdown. Returns file location, line numbers, signatures, and graph edges. Faster and richer than grep."
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
}

#[macros::mcp_tool(
    name = "graph_query",
    description = "Use this INSTEAD OF Read/Grep for tracing code relationships. Find neighbors (what calls/depends on a symbol), impact analysis (what depends on this), or reachable nodes within N hops. Use after search_symbols to get a node_id."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GraphQuery {
    /// Stable ID from search_symbols results
    pub node_id: String,
    /// Query mode: "neighbors" (default), "impact" (reverse dependents), "reachable" (forward BFS)
    #[serde(default = "default_graph_mode")]
    pub mode: String,
    /// Direction for neighbors mode: "outgoing" (default), "incoming", "both"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Filter edge types: calls, depends_on, implements, defines, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Maximum hops to traverse (default: 1 for neighbors, 3 for impact/reachable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u32>,
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
        "pr_merge" => NodeKind::PrMerge,
        other => NodeKind::Other(other.to_string()),
    }
}

/// Parse an EdgeKind from its string representation.
fn parse_edge_kind(s: &str) -> Option<EdgeKind> {
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
pub(crate) async fn load_graph_from_lance(repo_root: &Path) -> anyhow::Result<GraphState> {
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

    // Try to reuse existing embedding table from cache; only rebuild if empty/missing
    let embed_index = match EmbeddingIndex::new(repo_root).await {
        Ok(idx) => {
            // Probe the existing table -- if it has data, skip the expensive rebuild
            match idx.search("_probe_", None, 1).await {
                Ok(_) => {
                    tracing::info!("Loaded existing embedding index from cache");
                    Some(idx)
                }
                Err(_) => {
                    // Table empty or missing -- rebuild from cached graph nodes
                    match idx.index_all_with_symbols(repo_root, &nodes).await {
                        Ok(count) => {
                            tracing::info!("Rebuilt embedding index: {} items from cached graph", count);
                            Some(idx)
                        }
                        Err(e) => {
                            tracing::warn!("Failed to embed cached graph: {}", e);
                            None
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to create embed index: {}", e);
            None
        }
    };

    Ok(GraphState { nodes, edges, index, embed_index })
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
    pub embed_index: Option<EmbeddingIndex>,
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
    pub graph: Arc<RwLock<Option<GraphState>>>,
    /// Whether business context has been injected into a tool response.
    pub context_injected: std::sync::atomic::AtomicBool,
    /// Cooldown: skip re-scanning if checked recently.
    pub last_scan: std::sync::Mutex<std::time::Instant>,
    /// Whether background scanner has been spawned.
    pub background_scanner_started: std::sync::atomic::AtomicBool,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            graph: Arc::new(RwLock::new(None)),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            last_scan: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
            background_scanner_started: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl RnaHandler {
    /// Ensure graph is built, check for file changes since last scan.
    /// Returns a read guard to the graph.
    async fn get_graph(&self) -> anyhow::Result<tokio::sync::RwLockReadGuard<'_, Option<GraphState>>> {
        // Fast path: graph exists and scan cooldown hasn't expired
        {
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
                // Changes detected — need write lock
                drop(guard);
            } else {
                drop(guard);
            }
        }

        // Slow path: build or update graph
        {
            let mut guard = self.graph.write().await;
            if guard.is_none() {
                // First build — full pipeline
                *guard = Some(self.build_full_graph().await?);
            } else {
                // Incremental update — only changed files
                let graph = guard.as_mut().unwrap();
                self.update_graph_incrementally(graph).await?;
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

                    // Resolve current roots (primary + any live worktrees).
                    let workspace = WorkspaceConfig::load()
                        .with_primary_root(repo_root.clone())
                        .with_worktrees(&repo_root);
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
                            for edge in &mut extraction.edges {
                                edge.from.root = root_slug.clone();
                                edge.to.root = root_slug.clone();
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
    async fn build_full_graph(&self) -> anyhow::Result<GraphState> {
        // Pre-flight: ensure schema version matches before any LanceDB reads/writes.
        let db_path = graph_lance_path(&self.repo_root);
        if check_and_migrate_schema(&db_path).await? {
            tracing::info!("Schema migrated to v{} — cache rebuilt", SCHEMA_VERSION);
        }

        // Load workspace config and merge with --repo as primary root.
        // Also auto-detect any live git worktrees so all roots are indexed
        // on the first full build (mirrors the background scanner path).
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root);
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
            for edge in &mut extraction.edges {
                edge.from.root = root_slug.clone();
                edge.to.root = root_slug.clone();
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

        // 6. Phase 2: Run enrichers (LSP) synchronously
        let languages: Vec<String> = all_nodes
            .iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let enricher_registry = EnricherRegistry::with_builtins();
        let enrichment = enricher_registry
            .enrich_all(&all_nodes, &index, &languages, &self.repo_root)
            .await;

        // Add virtual nodes synthesized for external symbols (e.g., tokio::spawn).
        // These must be persisted in the symbols table but must NOT be embedded.
        if !enrichment.new_nodes.is_empty() {
            tracing::info!(
                "Enrichment synthesized {} virtual external nodes",
                enrichment.new_nodes.len()
            );
            for vnode in &enrichment.new_nodes {
                index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
            }
            all_nodes.extend(enrichment.new_nodes);
        }

        if !enrichment.added_edges.is_empty() {
            tracing::info!(
                "Enrichment added {} edges",
                enrichment.added_edges.len()
            );
            for edge in &enrichment.added_edges {
                let from_id = edge.from.to_stable_id();
                let to_id = edge.to.to_stable_id();
                index.add_edge(
                    &from_id,
                    &edge.from.kind.to_string(),
                    &to_id,
                    &edge.to.kind.to_string(),
                    edge.kind.clone(),
                );
            }
            all_edges.extend(enrichment.added_edges);
        }

        for (node_id, patches) in &enrichment.updated_nodes {
            if let Some(node) = all_nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                for (key, value) in patches {
                    node.metadata.insert(key.clone(), value.clone());
                }
            }
        }

        tracing::info!(
            "Graph built: {} nodes, {} edges across {} root(s)",
            all_nodes.len(),
            all_edges.len(),
            resolved_roots.len()
        );

        // 7. Persist graph to LanceDB
        if let Err(e) = persist_graph_to_lance(&self.repo_root, &all_nodes, &all_edges).await {
            tracing::warn!("Failed to persist graph to LanceDB: {}", e);
        }

        // 8. Embed as part of the graph pipeline.
        // External virtuals have no body — skip them. NodeKind filtering
        // (imports, consts, etc.) happens inside index_all_inner.
        // Probe first — if a persisted table already exists, reuse it (fast path).
        let embeddable_nodes: Vec<Node> = all_nodes.iter()
            .filter(|n| n.id.root != "external")
            .cloned()
            .collect();
        let embed_index = match EmbeddingIndex::new(&self.repo_root).await {
            Ok(idx) => {
                match idx.search("_probe_", None, 1).await {
                    Ok(_) => {
                        tracing::info!("Reusing persisted embedding index (skipping rebuild)");
                        Some(idx)
                    }
                    Err(_) => {
                        match idx.index_all_with_symbols(&self.repo_root, &embeddable_nodes).await {
                            Ok(count) => {
                                tracing::info!("Embedded {} items", count);
                                Some(idx)
                            }
                            Err(e) => {
                                tracing::warn!("Failed to embed: {}", e);
                                None
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to create embed index: {}", e);
                None
            }
        };

        Ok(GraphState {
            nodes: all_nodes,
            edges: all_edges,
            index,
            embed_index,
        })
    }

    /// Incrementally update the graph for changed/new/deleted files.
    async fn update_graph_incrementally(&self, graph: &mut GraphState) -> anyhow::Result<()> {
        let mut scanner = Scanner::new(self.repo_root.clone())?;
        let scan = scanner.scan()?;

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

            let enricher_registry = EnricherRegistry::with_builtins();
            let enrichment = enricher_registry
                .enrich_all(&changed_nodes, &graph.index, &languages, &self.repo_root)
                .await;

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
                if let Some(ref embed_idx) = graph.embed_index {
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
        }

        // Re-embed changed-file symbols. Uses the updated graph nodes so enriched
        // metadata is included in the embedding text.
        if let Some(ref embed_idx) = graph.embed_index {
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

        Ok(())
    }
}

fn text_result(s: String) -> CallToolResult {
    CallToolResult::text_content(vec![TextContent::new(s, None, None)])
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
    out.push_str("**Code exploration:** use `search_symbols` (not Grep), `graph_query` (not Read), `oh_search_context` (not search_all)\n");
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
                OhGetContext::tool(),
                OhSearchContext::tool(),
                OhRecord::tool(),
                OhInit::tool(),
                OutcomeProgress::tool(),
                SearchSymbols::tool(),
                GraphQuery::tool(),
                ListRoots::tool(),
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
            "oh_get_context" => {
                // Return concise business context — not the entire repo.
                // Use oh_search_context for discovery, search_symbols for code.
                let artifacts = oh::load_oh_artifacts(root).unwrap_or_default();
                let mut md = String::from("# Business Context (.oh/)\n\n");

                for kind in &[OhArtifactKind::Outcome, OhArtifactKind::Signal, OhArtifactKind::Guardrail, OhArtifactKind::Metis] {
                    let filtered: Vec<_> = artifacts.iter().filter(|a| &a.kind == kind).collect();
                    if filtered.is_empty() { continue; }
                    md.push_str(&format!("## {}s\n\n", kind));
                    for a in &filtered {
                        md.push_str(&a.to_markdown());
                        md.push_str("\n---\n\n");
                    }
                }

                let commit_count = git::load_commits(root, 10)
                    .map(|c| c.len())
                    .unwrap_or(0);
                md.push_str(&format!("_Recent commits: {}. Use `oh_search_context` for semantic search; `git show <hash>` via Bash for diffs._\n", commit_count));
                md.push_str("_Use `search_symbols` for code, `oh_search_context` for semantic search._\n");

                Ok(text_result(md))
            },

            "oh_search_context" => {
                let args: OhSearchContext = parse_args(params.arguments)?;
                let limit = args.limit.unwrap_or(5) as usize;
                let include_code = args.include_code.unwrap_or(false);
                let include_markdown = args.include_markdown.unwrap_or(false);

                // Ensure graph is built first so symbols are embedded
                // (get_graph builds the graph + embeds symbols in the pipeline)
                let _ = self.get_graph().await;

                let mut sections: Vec<String> = Vec::new();

                // Search .oh/ artifacts + symbols via embedding index
                // Create a fresh EmbeddingIndex handle (cheap — just opens LanceDB connection)
                // to avoid holding the graph read guard during async search.
                match EmbeddingIndex::new(root).await {
                    Ok(index) => {
                        match index.search(&args.query, args.artifact_types.as_deref(), limit).await {
                            Ok(results) => {
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
                            Err(e) => sections.push(format!("Artifact search error: {}", e)),
                        }
                    }
                    Err(e) => sections.push(format!("Index error: {}", e)),
                }

                // Optionally search code symbols
                if include_code {
                    match code::extract_symbols(root) {
                        Ok(symbols) => {
                            let matches = code::search_symbols(&symbols, &args.query);
                            if !matches.is_empty() {
                                let md = matches
                                    .iter()
                                    .take(limit)
                                    .map(|s| s.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!(
                                    "### Code symbols ({} result(s))\n\n{}",
                                    matches.len().min(limit),
                                    md
                                ));
                            }
                        }
                        Err(e) => sections.push(format!("Code search error: {}", e)),
                    }
                }

                // Optionally search markdown
                if include_markdown {
                    match markdown::extract_markdown_chunks(root) {
                        Ok(chunks) => {
                            let matches = markdown::search_chunks(&chunks, &args.query);
                            if !matches.is_empty() {
                                let md = matches
                                    .iter()
                                    .take(limit)
                                    .map(|c| c.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n\n---\n\n");
                                sections.push(format!(
                                    "### Markdown ({} result(s))\n\n{}",
                                    matches.len().min(limit),
                                    md
                                ));
                            }
                        }
                        Err(e) => sections.push(format!("Markdown search error: {}", e)),
                    }
                }

                if sections.is_empty() {
                    Ok(text_result(format!(
                        "No results found matching \"{}\".",
                        args.query
                    )))
                } else {
                    Ok(text_result(format!(
                        "## Semantic search: \"{}\"\n\n{}",
                        args.query,
                        sections.join("\n\n")
                    )))
                }
            }

            "oh_record" => {
                let args: OhRecord = parse_args(params.arguments)?;
                match args.record_type.as_str() {
                    "metis" => {
                        let title = args.title.unwrap_or_else(|| args.slug.clone());
                        let body = args.body.unwrap_or_default();
                        let mut fm = BTreeMap::new();
                        fm.insert(
                            "id".to_string(),
                            serde_yaml::Value::String(args.slug.clone()),
                        );
                        fm.insert(
                            "title".to_string(),
                            serde_yaml::Value::String(title),
                        );
                        if let Some(ref outcome) = args.outcome {
                            fm.insert(
                                "outcome".to_string(),
                                serde_yaml::Value::String(outcome.clone()),
                            );
                        }
                        match oh::write_metis(root, &args.slug, &fm, &body) {
                            Ok(path) => Ok(text_result(format!(
                                "Recorded metis at `{}`",
                                path.display()
                            ))),
                            Err(e) => Ok(text_result(format!("Error writing metis: {}", e))),
                        }
                    }
                    "signal" => {
                        let body = args.body.unwrap_or_default();
                        let outcome = args.outcome.unwrap_or_default();
                        let signal_type = args.signal_type.unwrap_or_else(|| "slo".to_string());
                        let threshold = args.threshold.unwrap_or_default();
                        let mut fm = BTreeMap::new();
                        fm.insert("id".into(), serde_yaml::Value::String(args.slug.clone()));
                        fm.insert("outcome".into(), serde_yaml::Value::String(outcome));
                        fm.insert("type".into(), serde_yaml::Value::String(signal_type));
                        fm.insert("threshold".into(), serde_yaml::Value::String(threshold));
                        match oh::write_artifact(root, "signals", &args.slug, &fm, &body) {
                            Ok(path) => Ok(text_result(format!("Recorded signal at `{}`", path.display()))),
                            Err(e) => Ok(text_result(format!("Error: {}", e))),
                        }
                    }
                    "guardrail" => {
                        let body = args.body.unwrap_or_default();
                        let severity = args.severity.unwrap_or_else(|| "candidate".to_string());
                        let statement = args.statement.unwrap_or_else(|| args.slug.clone());
                        let mut fm = BTreeMap::new();
                        fm.insert("id".into(), serde_yaml::Value::String(args.slug.clone()));
                        fm.insert("severity".into(), serde_yaml::Value::String(severity));
                        fm.insert("statement".into(), serde_yaml::Value::String(statement));
                        if let Some(ref outcome) = args.outcome {
                            fm.insert("outcome".into(), serde_yaml::Value::String(outcome.clone()));
                        }
                        match oh::write_artifact(root, "guardrails", &args.slug, &fm, &body) {
                            Ok(path) => Ok(text_result(format!("Recorded guardrail at `{}`", path.display()))),
                            Err(e) => Ok(text_result(format!("Error: {}", e))),
                        }
                    }
                    "outcome" => {
                        let mut updates = BTreeMap::new();
                        if let Some(status) = args.status {
                            updates.insert("status".into(), serde_yaml::Value::String(status));
                        }
                        if let Some(mechanism) = args.mechanism {
                            updates.insert("mechanism".into(), serde_yaml::Value::String(mechanism));
                        }
                        if let Some(files) = args.files {
                            let seq: Vec<serde_yaml::Value> = files.into_iter().map(serde_yaml::Value::String).collect();
                            updates.insert("files".into(), serde_yaml::Value::Sequence(seq));
                        }
                        if updates.is_empty() {
                            return Ok(text_result("No fields to update.".into()));
                        }
                        match oh::update_artifact(root, "outcomes", &args.slug, &updates) {
                            Ok(path) => Ok(text_result(format!("Updated outcome at `{}`", path.display()))),
                            Err(e) => Ok(text_result(format!("Error: {}", e))),
                        }
                    }
                    other => Ok(text_result(format!(
                        "Unknown record type: \"{}\". Use \"metis\", \"signal\", \"guardrail\", or \"outcome\".",
                        other
                    ))),
                }
            }

            "oh_init" => {
                let args: OhInit = parse_args(params.arguments)?;
                match oh_init_impl(root, args.outcome_name.as_deref()) {
                    Ok(msg) => Ok(text_result(msg)),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "outcome_progress" => {
                let args: OutcomeProgress = parse_args(params.arguments)?;
                match query::outcome_progress(root, &args.outcome_id) {
                    Ok(result) => {
                        let mut md = result.to_markdown();

                        // Append PR merge section from the graph
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
                            let pr_md = query::format_pr_merges_markdown(&pr_nodes);
                            if !pr_md.is_empty() {
                                md.push('\n');
                                md.push_str(&pr_md);
                            }
                         }
                        }

                        Ok(text_result(md))
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "search_symbols" => {
                let args: SearchSymbols = parse_args(params.arguments)?;
                match self.get_graph().await {
                    Ok(guard) => {
                        let graph_state = guard.as_ref().unwrap();
                        let limit = args.limit.unwrap_or(20) as usize;
                        let query_lower = args.query.to_lowercase();

                        let mut matches: Vec<&Node> = graph_state
                            .nodes
                            .iter()
                            .filter(|n| {
                                let name_match = n.id.name.to_lowercase().contains(&query_lower)
                                    || n.signature.to_lowercase().contains(&query_lower);
                                if !name_match {
                                    return false;
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
                                true
                            })
                            .collect();

                        matches.truncate(limit);

                        if matches.is_empty() {
                            Ok(text_result(format!(
                                "No symbols matching \"{}\".",
                                args.query
                            )))
                        } else {
                            let md: String = matches
                                .iter()
                                .map(|n| {
                                    let stable_id = n.stable_id();
                                    // Find edges involving this node
                                    let outgoing = graph_state.index.neighbors(
                                        &stable_id,
                                        None,
                                        Direction::Outgoing,
                                    );
                                    let incoming = graph_state.index.neighbors(
                                        &stable_id,
                                        None,
                                        Direction::Incoming,
                                    );
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
                                    if let Some(val) = n.metadata.get("value") {
                                        entry.push_str(&format!("\n  Value: `{}`", val));
                                    }
                                    if n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false) {
                                        entry.push_str(" *(literal)*");
                                    }
                                    if !outgoing.is_empty() {
                                        entry.push_str(&format!("\n  Out: {} edge(s)", outgoing.len()));
                                    }
                                    if !incoming.is_empty() {
                                        entry.push_str(&format!("\n  In: {} edge(s)", incoming.len()));
                                    }
                                    entry
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            Ok(text_result(format!(
                                "## Symbol search: \"{}\"\n\n{} result(s)\n\n{}",
                                args.query,
                                matches.len(),
                                md
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Graph error: {}", e))),
                }
            }

            "graph_query" => {
                let args: GraphQuery = parse_args(params.arguments)?;
                match self.get_graph().await {
                    Ok(guard) => {
                        let graph_state = guard.as_ref().unwrap();
                        let edge_filter = args.edge_types.as_ref().map(|types| {
                            types
                                .iter()
                                .filter_map(|t| parse_edge_kind(t))
                                .collect::<Vec<_>>()
                        });
                        let edge_filter_slice = edge_filter.as_deref();

                        match args.mode.as_str() {
                            "neighbors" => {
                                let max_hops = args.max_hops.unwrap_or(1) as usize;
                                let direction = args.direction.as_deref().unwrap_or("outgoing");

                                let mut all_ids: Vec<String> = Vec::new();

                                match direction {
                                    "outgoing" => {
                                        if max_hops == 1 {
                                            all_ids = graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Outgoing,
                                            );
                                        } else {
                                            all_ids = graph_state.index.reachable(
                                                &args.node_id,
                                                max_hops,
                                                edge_filter_slice,
                                            );
                                        }
                                    }
                                    "incoming" => {
                                        if max_hops == 1 {
                                            all_ids = graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Incoming,
                                            );
                                        } else {
                                            all_ids = graph_state.index.impact(&args.node_id, max_hops);
                                        }
                                    }
                                    "both" => {
                                        let out = if max_hops == 1 {
                                            graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Outgoing,
                                            )
                                        } else {
                                            graph_state.index.reachable(
                                                &args.node_id,
                                                max_hops,
                                                edge_filter_slice,
                                            )
                                        };
                                        let inc = if max_hops == 1 {
                                            graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Incoming,
                                            )
                                        } else {
                                            graph_state.index.impact(&args.node_id, max_hops)
                                        };
                                        all_ids.extend(out);
                                        all_ids.extend(inc);
                                        all_ids.sort();
                                        all_ids.dedup();
                                    }
                                    _ => {
                                        return Ok(text_result(format!(
                                            "Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".",
                                            direction
                                        )));
                                    }
                                }

                                if all_ids.is_empty() {
                                    Ok(text_result(format!(
                                        "No {} neighbors for `{}`.",
                                        direction, args.node_id
                                    )))
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &all_ids);
                                    Ok(text_result(format!(
                                        "## Graph neighbors ({}) of `{}`\n\n{} result(s)\n\n{}",
                                        direction,
                                        args.node_id,
                                        all_ids.len(),
                                        md
                                    )))
                                }
                            }
                            "impact" => {
                                let max_hops = args.max_hops.unwrap_or(3) as usize;
                                let impacted = graph_state.index.impact(&args.node_id, max_hops);

                                if impacted.is_empty() {
                                    Ok(text_result(format!(
                                        "No dependents found for `{}` within {} hops.",
                                        args.node_id, max_hops
                                    )))
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &impacted);
                                    Ok(text_result(format!(
                                        "## Impact analysis for `{}`\n\n{} dependent(s) within {} hop(s)\n\n{}",
                                        args.node_id,
                                        impacted.len(),
                                        max_hops,
                                        md
                                    )))
                                }
                            }
                            "reachable" => {
                                let max_hops = args.max_hops.unwrap_or(3) as usize;
                                let reachable = graph_state.index.reachable(
                                    &args.node_id,
                                    max_hops,
                                    edge_filter_slice,
                                );

                                if reachable.is_empty() {
                                    Ok(text_result(format!(
                                        "No reachable nodes from `{}` within {} hops.",
                                        args.node_id, max_hops
                                    )))
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &reachable);
                                    Ok(text_result(format!(
                                        "## Reachable from `{}`\n\n{} node(s) within {} hop(s)\n\n{}",
                                        args.node_id,
                                        reachable.len(),
                                        max_hops,
                                        md
                                    )))
                                }
                            }
                            other => {
                                Ok(text_result(format!(
                                    "Unknown mode: \"{}\". Use \"neighbors\", \"impact\", or \"reachable\".",
                                    other
                                )))
                            }
                        }
                    }
                    Err(e) => Ok(text_result(format!("Graph error: {}", e))),
                }
            }

            "list_roots" => {
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(self.repo_root.clone())
                    .with_worktrees(&self.repo_root);
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
    let value = arguments
        .map(serde_json::Value::Object)
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value)
        .map_err(|e| CallToolError::from_message(format!("Invalid arguments: {}", e)))
}

fn oh_init_impl(repo_root: &Path, outcome_name: Option<&str>) -> anyhow::Result<String> {
    use std::fs;

    let oh_dir = repo_root.join(".oh");
    let mut created = Vec::new();
    let mut skipped = Vec::new();

    // Create directory structure
    for subdir in &["outcomes", "signals", "guardrails", "metis"] {
        let dir = oh_dir.join(subdir);
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
            created.push(format!(".oh/{}/", subdir));
        }
    }

    // Try to detect project name from README or CLAUDE.md
    let project_name = outcome_name
        .map(|s| s.to_string())
        .or_else(|| detect_project_name(repo_root))
        .unwrap_or_else(|| "project-goal".to_string());

    let slug = project_name
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string();

    // Scaffold outcome
    let outcome_path = oh_dir.join("outcomes").join(format!("{}.md", slug));
    if outcome_path.exists() {
        skipped.push(format!(".oh/outcomes/{}.md (exists)", slug));
    } else {
        let mut fm = BTreeMap::new();
        fm.insert("id".into(), serde_yaml::Value::String(slug.clone()));
        fm.insert("status".into(), serde_yaml::Value::String("active".into()));
        fm.insert(
            "mechanism".into(),
            serde_yaml::Value::String("(describe how this outcome is achieved)".into()),
        );
        fm.insert(
            "files".into(),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("src/*".into())]),
        );
        oh::write_artifact(
            repo_root,
            "outcomes",
            &slug,
            &fm,
            &format!("# {}\n\n(Describe the desired outcome here.)\n\n## Signals\n- (what signals indicate progress?)\n\n## Constraints\n- (what guardrails apply?)", project_name),
        )?;
        created.push(format!(".oh/outcomes/{}.md", slug));
    }

    // Scaffold signal
    let signal_slug = format!("{}-progress", slug);
    let signal_path = oh_dir.join("signals").join(format!("{}.md", signal_slug));
    if signal_path.exists() {
        skipped.push(format!(".oh/signals/{}.md (exists)", signal_slug));
    } else {
        let mut fm = BTreeMap::new();
        fm.insert("id".into(), serde_yaml::Value::String(signal_slug.clone()));
        fm.insert("outcome".into(), serde_yaml::Value::String(slug.clone()));
        fm.insert("type".into(), serde_yaml::Value::String("slo".into()));
        fm.insert(
            "threshold".into(),
            serde_yaml::Value::String("(define measurable threshold)".into()),
        );
        oh::write_artifact(
            repo_root,
            "signals",
            &signal_slug,
            &fm,
            &format!("# {} Progress\n\n(How do you measure progress toward this outcome?)", project_name),
        )?;
        created.push(format!(".oh/signals/{}.md", signal_slug));
    }

    // Scaffold lightweight guardrail
    let gr_path = oh_dir.join("guardrails").join("lightweight.md");
    if gr_path.exists() {
        skipped.push(".oh/guardrails/lightweight.md (exists)".into());
    } else {
        let mut fm = BTreeMap::new();
        fm.insert("id".into(), serde_yaml::Value::String("lightweight".into()));
        fm.insert("severity".into(), serde_yaml::Value::String("hard".into()));
        oh::write_artifact(
            repo_root,
            "guardrails",
            "lightweight",
            &fm,
            "# Lightweight Adoption\n\nAdding an outcome is writing a markdown file, not configuring a system. If this harness is heavier than adding a section to CLAUDE.md, adoption will fail.",
        )?;
        created.push(".oh/guardrails/lightweight.md".into());
    }

    // Build result message
    let mut msg = String::from("## .oh/ initialized\n\n");
    if !created.is_empty() {
        msg.push_str("### Created\n");
        for f in &created {
            msg.push_str(&format!("- `{}`\n", f));
        }
    }
    if !skipped.is_empty() {
        msg.push_str("\n### Skipped\n");
        for f in &skipped {
            msg.push_str(&format!("- `{}`\n", f));
        }
    }
    msg.push_str(&format!(
        "\n### Next steps\n1. Edit `.oh/outcomes/{}.md` — describe your outcome\n2. Edit `.oh/signals/{}.md` — define how to measure progress\n3. Add `files:` patterns to the outcome frontmatter\n4. Start tagging commits with `[outcome:{}]`\n",
        slug, signal_slug, slug
    ));
    Ok(msg)
}

fn detect_project_name(repo_root: &Path) -> Option<String> {
    // Try Cargo.toml name field
    let cargo_path = repo_root.join("Cargo.toml");
    if cargo_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo_path) {
            for line in content.lines() {
                if let Some(name) = line.strip_prefix("name") {
                    let name = name.trim().trim_start_matches('=').trim().trim_matches('"');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    // Try package.json name field
    let pkg_path = repo_root.join("package.json");
    if pkg_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    return Some(name.to_string());
                }
            }
        }
    }
    // Try directory name
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

/// Format a list of node IDs into markdown, enriched with node details if available.
fn format_neighbor_nodes(nodes: &[graph::Node], ids: &[String]) -> String {
    ids.iter()
        .map(|id| {
            if let Some(node) = nodes.iter().find(|n| n.stable_id() == *id) {
                format!(
                    "- **{}** `{}` ({}) `{}`:{}-{}",
                    node.id.kind,
                    node.id.name,
                    node.language,
                    node.id.file.display(),
                    node.line_start,
                    node.line_end,
                )
            } else {
                format!("- `{}`", id)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
