//! LanceDB persistence: persist, load, schema migration, stale root pruning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arrow_array::{Array, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray, UInt32Array};
use arrow_array::builder::BooleanBuilder;

use crate::graph::{Confidence, Edge, ExtractionSource, Node, NodeId, NodeKind, EdgeKind};
use crate::graph::index::GraphIndex;
use crate::graph::store::{symbols_schema, edges_schema, SCHEMA_VERSION};

use super::state::GraphState;

// ── Graph persistence (LanceDB) ─────────────────────────────────────

/// LanceDB path for graph persistence.
pub(crate) fn graph_lance_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("lance")
}

/// Check the stored schema version and migrate (drop + recreate) if it mismatches.
///
/// Returns `true` if tables were dropped (migration occurred),
/// `false` if the stored version already matches `SCHEMA_VERSION` (no-op).
///
/// Uses a simple file (`schema_version` alongside the LanceDB directory) instead
/// of a LanceDB table. This avoids circular dependency: checking LanceDB health
/// via LanceDB fails when the schema itself is incompatible.
///
/// Downstream callers (`build_full_graph`, `persist_graph_to_lance`) use this to ensure
/// stale LanceDB tables are discarded before any read or write.
pub(crate) async fn check_and_migrate_schema(db_path: &Path) -> anyhow::Result<bool> {
    std::fs::create_dir_all(db_path)?;

    let version_file = db_path.join("schema_version");

    // Read stored version from file (None if missing or unparseable).
    let stored_version: Option<u32> = std::fs::read_to_string(&version_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    // If version matches, nothing to do.
    if stored_version == Some(SCHEMA_VERSION) {
        return Ok(false);
    }

    // Version mismatch (or missing file) — nuke the LanceDB directory contents
    // and rewrite the version file.
    //
    // Stale data exists if we had a version file (normal case) OR if the lance
    // directory contains data files (legacy state without version file).
    let had_stale_data = stored_version.is_some() || has_lance_data(db_path);

    if had_stale_data {
        tracing::info!(
            "Schema version mismatch (stored={:?}, current={}) — dropping all LanceDB data",
            stored_version,
            SCHEMA_VERSION
        );
    } else {
        tracing::debug!(
            "No stored schema version found; initializing schema_version to {}",
            SCHEMA_VERSION
        );
    }

    // Delete all LanceDB data by removing directory contents (except the version file
    // we're about to write). This is more reliable than drop_table which can fail
    // on corrupted/incompatible tables.
    if let Ok(entries) = std::fs::read_dir(db_path) {
        for entry in entries {
            let entry = entry.context("check_and_migrate_schema: failed to read db_path entry")?;
            let path = entry.path();
            // Don't delete the version file yet — we'll overwrite it below.
            if path.file_name().map(|n| n == "schema_version").unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                std::fs::remove_dir_all(&path).with_context(|| {
                    format!("check_and_migrate_schema: failed to remove directory {}", path.display())
                })?;
            } else {
                std::fs::remove_file(&path).with_context(|| {
                    format!("check_and_migrate_schema: failed to remove file {}", path.display())
                })?;
            }
        }
    }

    // Also try LanceDB drop_table as a belt-and-suspenders cleanup for any
    // tables that survive directory deletion (e.g., external references).
    if let Ok(db) = lancedb::connect(db_path.to_str().unwrap_or_default())
        .execute()
        .await
    {
        for table_name in &["symbols", "edges", "pr_merges", "file_index", "_schema_meta"] {
            let _ = db.drop_table(table_name, &[]).await;
        }
    }

    // Write the current version to the file.
    std::fs::write(&version_file, SCHEMA_VERSION.to_string())
        .context("check_and_migrate_schema: failed to write schema_version file")?;

    // Return true only when stale data existed (real migration). Fresh directories
    // (stored_version == None) return false so incremental persist can proceed to
    // bootstrap the tables.
    Ok(had_stale_data)
}

/// Check whether the LanceDB directory contains any data files (tables).
///
/// Used to detect legacy state where tables exist without a version file.
fn has_lance_data(db_path: &Path) -> bool {
    std::fs::read_dir(db_path)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    // LanceDB stores tables as directories; skip our version file
                    name != "schema_version" && e.path().is_dir()
                })
        })
        .unwrap_or(false)
}

/// Parse a NodeKind from its string representation.
pub(crate) fn parse_node_kind(s: &str) -> NodeKind {
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
        "type_alias" => NodeKind::TypeAlias,
        "macro" => NodeKind::Macro,
        "field" => NodeKind::Field,
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
pub(crate) fn parse_extraction_source(s: &str) -> ExtractionSource {
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
pub(crate) fn parse_confidence(s: &str) -> Confidence {
    match s {
        "confirmed" => Confidence::Confirmed,
        "detected" => Confidence::Detected,
        _ => {
            tracing::warn!("Unknown confidence value: {}, defaulting to Detected", s);
            Confidence::Detected
        }
    }
}

/// Query LanceDB for all distinct `root_id` values stored across all tables.
///
/// Scans the same set of tables that `delete_nodes_for_roots` prunes so that
/// stale roots present in any table are discovered (not just symbols).
pub(crate) async fn get_stored_root_ids(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};

    let db_path = graph_lance_path(repo_root);
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for root discovery")?;

    let mut root_ids = std::collections::HashSet::new();

    for table_name in ["symbols", "edges", "file_index", "pr_merges"] {
        let tbl = match db.open_table(table_name).execute().await {
            Ok(t) => t,
            Err(_) => continue, // table doesn't exist yet -- skip
        };

        let stream = match tbl
            .query()
            .select(lancedb::query::Select::columns(&["root_id"]))
            .execute()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Could not query root_ids from {}: {}", table_name, e);
                continue;
            }
        };
        let batches: Vec<arrow_array::RecordBatch> = stream.try_collect().await?;

        for batch in &batches {
            if let Some(col) = batch.column_by_name("root_id") {
                if let Some(arr) = col.as_any().downcast_ref::<arrow_array::StringArray>() {
                    for i in 0..arr.len() {
                        if !arr.is_null(i) {
                            root_ids.insert(arr.value(i).to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(root_ids.into_iter().collect())
}

/// Delete all LanceDB rows for the given root slugs from all tables.
///
/// Called when a worktree is detected as removed (during background scan or
/// at startup when stale roots are found in LanceDB).
pub(crate) async fn delete_nodes_for_roots(repo_root: &Path, slugs: &[String]) -> anyhow::Result<()> {
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

    // Delete from all tables that carry a root_id column.
    for table_name in ["symbols", "edges", "file_index", "pr_merges"] {
        if let Ok(tbl) = db.open_table(table_name).execute().await {
            if let Err(e) = tbl.delete(&predicate).await {
                tracing::warn!(
                    "Failed to delete {} for removed worktrees: {}",
                    table_name,
                    e
                );
            }
        }
    }

    tracing::info!(
        "Pruned LanceDB rows for stale roots: {}",
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
        let pattern_hints: Vec<Option<String>> = nodes.iter()
            .map(|n| n.metadata.get("pattern_hint").cloned())
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
                Arc::new(StringArray::from(pattern_hints)),
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
) -> anyhow::Result<bool> {
    let db_path = graph_lance_path(repo_root);
    std::fs::create_dir_all(&db_path)?;

    // Pre-flight: ensure schema version matches before any LanceDB writes.
    if check_and_migrate_schema(&db_path).await? {
        tracing::info!("Schema migrated to v{} during incremental update — cache rebuilt; caller should do a full persist", SCHEMA_VERSION);
        // Migration dropped stale tables — incremental upsert against empty
        // tables is incorrect. Return true so the caller does a full persist.
        return Ok(true);
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
            let pattern_hints: Vec<Option<String>> = upsert_nodes.iter()
                .map(|n| n.metadata.get("pattern_hint").cloned())
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
                    Arc::new(StringArray::from(pattern_hints)),
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
    Ok(false)
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
            let pattern_hint_col = batch.column_by_name("pattern_hint")
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
                if let Some(col) = pattern_hint_col {
                    if !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("pattern_hint".to_string(), val.to_string());
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
pub(crate) fn parse_node_id_from_stable(stable_id: &str, kind_hint: &str, root_hint: &str) -> NodeId {
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
pub(crate) fn infer_language_from_path(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust".to_string(),
        Some("py") => "python".to_string(),
        Some("ts") | Some("tsx") => "typescript".to_string(),
        Some("js") | Some("jsx") => "javascript".to_string(),
        Some("go") => "go".to_string(),
        Some("java") => "java".to_string(),
        Some("c") | Some("h") | Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") | Some("hh") | Some("hxx") => "cpp".to_string(),
        Some("cs") => "csharp".to_string(),
        Some("rb") => "ruby".to_string(),
        Some("kt") | Some("kts") => "kotlin".to_string(),
        Some("swift") => "swift".to_string(),
        Some("zig") => "zig".to_string(),
        Some("lua") => "lua".to_string(),
        Some("sh") | Some("bash") => "bash".to_string(),
        Some("tf") | Some("hcl") | Some("tfvars") => "hcl".to_string(),
        Some("json") | Some("jsonc") => "json".to_string(),
        Some("proto") => "protobuf".to_string(),
        Some("sql") => "sql".to_string(),
        Some("md") => "markdown".to_string(),
        Some("toml") => "toml".to_string(),
        Some("yaml") | Some("yml") => "yaml".to_string(),
        _ => "unknown".to_string(),
    }
}
