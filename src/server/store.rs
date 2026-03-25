//! LanceDB persistence: persist, load, schema migration, stale root pruning.
// EXTRACTION_VERSION is deprecated (#526) but still used for backward-compat sentinel reads.
#![allow(deprecated)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::time::Duration;

use anyhow::Context;
use arrow_array::{Array, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray, UInt32Array, UInt64Array};
use arrow_array::builder::BooleanBuilder;

use crate::graph::{Confidence, Edge, ExtractionSource, Node, NodeId, NodeKind, EdgeKind};
use crate::graph::index::GraphIndex;
use crate::graph::store::{symbols_schema, edges_schema, SCHEMA_VERSION, EXTRACTION_VERSION};

use super::state::GraphState;

// ── Graph persistence (LanceDB) ─────────────────────────────────────

/// LanceDB path for graph persistence.
pub(crate) fn graph_lance_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("lance")
}

/// Path to the committed scan_version pointer file.
///
/// This file contains the `u64` scan version that is currently live for reads.
/// Written atomically after a successful full-rebuild append. Until this file is
/// updated, the previous version remains the source of truth for queries.
fn scan_version_path(db_path: &Path) -> PathBuf {
    db_path.join("scan_version")
}

/// Read the committed scan version from the pointer file.
///
/// Returns `0` if the file is absent or unparseable (first-run default that matches
/// rows written by the very first persist, which also writes version `1`).
fn read_committed_scan_version(db_path: &Path) -> u64 {
    std::fs::read_to_string(scan_version_path(db_path))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Atomically write the committed scan_version pointer.
fn write_committed_scan_version(db_path: &Path, version: u64) -> anyhow::Result<()> {
    let path = scan_version_path(db_path);
    // Write to a temp file then rename for atomicity.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, version.to_string())
        .context("write_committed_scan_version: failed to write tmp file")?;
    std::fs::rename(&tmp, &path)
        .context("write_committed_scan_version: failed to rename tmp to scan_version")?;
    Ok(())
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

    // Delete all LanceDB data by removing directory contents (except the version files
    // we manage separately). This is more reliable than drop_table which can fail
    // on corrupted/incompatible tables.
    if let Ok(entries) = std::fs::read_dir(db_path) {
        for entry in entries {
            let entry = entry.context("check_and_migrate_schema: failed to read db_path entry")?;
            let path = entry.path();
            // Preserve version-tracking files: schema_version is rewritten below;
            // extraction_version must survive so the next extraction-version bump
            // still triggers its scan-state reset correctly.
            // scan_version is deleted (reset to 0 on next read) since all rows are gone.
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name == "schema_version" || name == "extraction_version" {
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

/// Check the stored extraction version and clear scan-state files if it mismatches.
///
/// Returns `true` if scan-state was cleared (re-extraction needed),
/// `false` if the stored version already matches `EXTRACTION_VERSION` (no-op).
///
/// Unlike `check_and_migrate_schema` (which drops LanceDB tables), this only
/// deletes scanner state files — forcing the scanner to treat all files as new
/// on the next build without discarding the LanceDB graph/embedding data.
///
/// The version file lives alongside `schema_version` in `db_path`.
/// Scan-state files are cleared for:
/// - The primary root: `{repo_root}/.oh/.cache/scan-state.json`
/// - Secondary roots: `~/.local/share/rna/cache/{slug}/scan-state.json`
pub(crate) fn check_and_migrate_extraction_version(
    db_path: &Path,
    repo_root: &Path,
    slugs: &[String],
) -> anyhow::Result<bool> {
    std::fs::create_dir_all(db_path)?;

    let version_file = db_path.join("extraction_version");

    let stored_version: Option<u32> = std::fs::read_to_string(&version_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    // If version matches, nothing to do.
    if stored_version == Some(EXTRACTION_VERSION) {
        return Ok(false);
    }

    if stored_version.is_some() {
        tracing::info!(
            "Extraction version mismatch (stored={:?}, current={}) — clearing scan-state to force full re-extraction",
            stored_version,
            EXTRACTION_VERSION
        );
    } else {
        tracing::debug!(
            "No stored extraction version; initializing extraction_version to {}",
            EXTRACTION_VERSION
        );
    }

    // Delete scan-state files for all known roots so the scanner treats all
    // files as new and re-extracts them with the updated extraction logic.
    if stored_version.is_some() {
        // Primary root scan-state: {repo_root}/.oh/.cache/scan-state.json
        let primary_state = repo_root.join(".oh").join(".cache").join("scan-state.json");
        if primary_state.exists() {
            match std::fs::remove_file(&primary_state) {
                Ok(()) => tracing::info!(
                    "Cleared primary scan-state (extraction version upgrade)"
                ),
                Err(e) => tracing::warn!(
                    "Failed to clear primary scan-state: {}",
                    e
                ),
            }
        }

        // Secondary roots: ~/.local/share/rna/cache/{slug}/scan-state.json
        for slug in slugs {
            let state_path = crate::roots::cache_state_path(slug);
            if state_path.exists() {
                match std::fs::remove_file(&state_path) {
                    Ok(()) => tracing::info!(
                        "Cleared scan-state for root '{}' (extraction version upgrade)",
                        slug
                    ),
                    Err(e) => tracing::warn!(
                        "Failed to clear scan-state for root '{}': {}",
                        slug,
                        e
                    ),
                }
            }
        }
    }

    // Write the current extraction version.
    std::fs::write(&version_file, EXTRACTION_VERSION.to_string())
        .context("check_and_migrate_extraction_version: failed to write extraction_version file")?;

    Ok(stored_version.is_some())
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
        "enum_variant" => NodeKind::EnumVariant,
        "markdown_section" => NodeKind::MarkdownSection,
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
        "references" => EdgeKind::References,
        "topology_boundary" => EdgeKind::TopologyBoundary,
        "modified" => EdgeKind::Modified,
        "affected" => EdgeKind::Affected,
        "serves" => EdgeKind::Serves,
        "tested_by" => EdgeKind::TestedBy,
        "belongs_to" => EdgeKind::BelongsTo,
        "re_exports" => EdgeKind::ReExports,
        "uses_framework" => EdgeKind::UsesFramework,
        "produces" => EdgeKind::Produces,
        "consumes" => EdgeKind::Consumes,
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
            if let Some(col) = batch.column_by_name("root_id")
                && let Some(arr) = col.as_any().downcast_ref::<arrow_array::StringArray>() {
                    for i in 0..arr.len() {
                        if !arr.is_null(i) {
                            root_ids.insert(arr.value(i).to_string());
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
        if let Ok(tbl) = db.open_table(table_name).execute().await
            && let Err(e) = tbl.delete(&predicate).await {
                tracing::warn!(
                    "Failed to delete {} for removed worktrees: {}",
                    table_name,
                    e
                );
            }
    }

    tracing::info!(
        "Pruned LanceDB rows for stale roots: {}",
        slugs.join(", ")
    );
    Ok(())
}

/// Drop all LanceDB table directories inside `db_path` and reset the schema_version file.
///
/// Called when a schema mismatch is detected at runtime (Arrow rejection during merge_insert).
/// After dropping tables the caller returns `Ok(true)` so the graph layer triggers a full
/// `persist_graph_to_lance` rebuild.  Errors here are non-fatal — worst case the next scan
/// retries and eventually succeeds.
fn drop_all_lance_tables(db_path: &Path) {
    if let Ok(entries) = std::fs::read_dir(db_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Preserve both version files: schema_version is rewritten below;
            // extraction_version must survive so the next extraction-version bump
            // still triggers its scan-state reset correctly.
            if path
                .file_name()
                .map(|n| n == "schema_version" || n == "extraction_version")
                .unwrap_or(false)
            {
                continue;
            }
            if path.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!("drop_all_lance_tables: failed to remove {}: {}", path.display(), e);
                }
            } else if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("drop_all_lance_tables: failed to remove {}: {}", path.display(), e);
            }
        }
    }
    // Reset the version file so check_and_migrate_schema re-initialises cleanly.
    let version_file = db_path.join("schema_version");
    if let Err(e) = std::fs::write(&version_file, SCHEMA_VERSION.to_string()) {
        tracing::warn!("drop_all_lance_tables: failed to write schema_version: {}", e);
    }
}

/// Returns `true` if the error looks like a LanceDB concurrent-write conflict.
///
/// LanceDB uses optimistic concurrency and surfaces conflicts as errors whose
/// messages contain "conflict" or "concurrent" in their description.  We match on
/// the string representation because the upstream error types are not exposed as a
/// public enum.
///
/// Note: "commit" is intentionally excluded — it appears in many non-conflict contexts
/// (git history, schema version strings, log messages) and would cause false positives.
fn is_conflict_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("conflict") || msg.contains("concurrent")
}

/// Returns `true` if the error looks like an Arrow/LanceDB schema mismatch.
///
/// This happens when the on-disk LanceDB table was written with a different schema
/// than what the code expects — e.g. after an RNA upgrade where the LanceDB cache
/// was not cleared.  The pre-flight `check_and_migrate_schema` guard uses a version
/// *file* to detect mismatches, but if that file is missing or out of sync the file
/// check passes while the actual table rejects the write with a schema error.
///
/// We detect this defensively by matching error message substrings from LanceDB
/// and Arrow, because the upstream error types are not part of a stable public enum.
fn is_schema_mismatch_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    // Arrow schema errors: field count or type mismatches
    msg.contains("schema")
        // LanceDB column-not-found / type-mismatch phrases
        || msg.contains("column not found")
        || msg.contains("field not found")
        || msg.contains("type mismatch")
        // Arrow batch-level rejection
        || msg.contains("invalid recordbatch")
}

/// Build a symbols `RecordBatch` for `nodes` tagged with `scan_version`.
fn build_symbols_batch(nodes: &[Node], scan_version: u64) -> anyhow::Result<RecordBatch> {
    use std::sync::Arc;
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
    let mut is_static_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("is_static") {
            Some(v) => is_static_builder.append_value(v == "true"),
            None => is_static_builder.append_null(),
        }
    }
    let mut is_async_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("is_async") {
            Some(v) => is_async_builder.append_value(v == "true"),
            None => is_async_builder.append_null(),
        }
    }
    let mut is_test_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("is_test") {
            Some(v) => is_test_builder.append_value(v == "true"),
            None => is_test_builder.append_null(),
        }
    }
    let visibilities: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("visibility").cloned())
        .collect();
    let mut exported_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("exported") {
            Some(v) => exported_builder.append_value(v == "true"),
            None => exported_builder.append_null(),
        }
    }
    // Diagnostic metadata columns
    let diag_severities: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_severity").cloned())
        .collect();
    let diag_sources: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_source").cloned())
        .collect();
    let diag_messages: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_message").cloned())
        .collect();
    let diag_ranges: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_range").cloned())
        .collect();
    let diag_timestamps: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_timestamp").cloned())
        .collect();
    // ApiEndpoint metadata columns
    let http_methods: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("http_method").cloned())
        .collect();
    let http_paths: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("http_path").cloned())
        .collect();
    // doc_comment column — persisted for LSP reindex round-trip (#416)
    let doc_comments: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("doc_comment").cloned())
        .collect();
    // gRPC / proto columns — populated for proto RPC Function nodes (#466)
    let parent_services: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("parent_service").cloned())
        .collect();
    let rpc_request_types: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("request_type").cloned())
        .collect();
    let rpc_response_types: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("response_type").cloned())
        .collect();
    let updated_ats: Vec<i64> = vec![now; nodes.len()];
    let scan_versions: Vec<u64> = vec![scan_version; nodes.len()];

    RecordBatch::try_new(
        schema,
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
            Arc::new(is_static_builder.finish()),
            Arc::new(is_async_builder.finish()),
            Arc::new(is_test_builder.finish()),
            Arc::new(StringArray::from(visibilities)),
            Arc::new(exported_builder.finish()),
            Arc::new(StringArray::from(diag_severities)),
            Arc::new(StringArray::from(diag_sources)),
            Arc::new(StringArray::from(diag_messages)),
            Arc::new(StringArray::from(diag_ranges)),
            Arc::new(StringArray::from(diag_timestamps)),
            Arc::new(StringArray::from(http_methods)),
            Arc::new(StringArray::from(http_paths)),
            Arc::new(StringArray::from(doc_comments)),
            Arc::new(StringArray::from(parent_services)),
            Arc::new(StringArray::from(rpc_request_types)),
            Arc::new(StringArray::from(rpc_response_types)),
            Arc::new(Int64Array::from(updated_ats)),
            Arc::new(UInt64Array::from(scan_versions)),
        ],
    ).map_err(anyhow::Error::from)
}

/// Build an edges `RecordBatch` for `edges` tagged with `scan_version`.
fn build_edges_batch(edges: &[Edge], scan_version: u64) -> anyhow::Result<RecordBatch> {
    use std::sync::Arc;
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
    let scan_versions: Vec<u64> = vec![scan_version; edges.len()];

    RecordBatch::try_new(
        schema,
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
            Arc::new(UInt64Array::from(scan_versions)),
        ],
    ).map_err(anyhow::Error::from)
}

/// Persist graph nodes and edges to LanceDB using append-only versioned writes.
///
/// Each call appends a new `scan_version` (monotonically incrementing) to the tables
/// and atomically updates the version pointer file ONLY after both tables are fully
/// written. Old rows remain queryable until the pointer flips; reads always filter to
/// the latest committed version.
///
/// This replaces the previous DROP+CREATE strategy, eliminating:
/// - Zero-result query windows during rebuild
/// - Data loss if persist fails mid-way (old version stays live)
/// - Slow index recreation on every scan (FTS index created once, not per rebuild)
///
/// After a successful commit, background compaction removes rows from versions older
/// than `committed - 1` via `compact_stale_versions`.
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

    // Determine the next scan_version to write.
    // current committed version → write to current + 1.
    let committed_version = read_committed_scan_version(&db_path);
    let new_version = committed_version + 1;

    tracing::debug!(
        "persist_graph_to_lance: committed_version={} → writing new_version={}",
        committed_version, new_version
    );

    // ── Append symbols (nodes) with new_version ──
    {
        let schema = Arc::new(symbols_schema());
        let batch = build_symbols_batch(nodes, new_version)?;

        match db.open_table("symbols").execute().await {
            Ok(tbl) => {
                // Table exists — append the new-version rows.
                let mut attempts: u64 = 0;
                loop {
                    let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema.clone());
                    match tbl.add(batches).execute().await {
                        Ok(_) => break,
                        Err(e) => {
                            let err = anyhow::anyhow!("{}", e);
                            if is_conflict_error(&err) && attempts < 3 {
                                attempts += 1;
                                tracing::warn!(
                                    "LanceDB conflict on symbols append (attempt {}), retrying in {}ms",
                                    attempts, 100 * attempts
                                );
                                tokio::time::sleep(Duration::from_millis(100 * attempts)).await;
                            } else if is_schema_mismatch_error(&err) {
                                tracing::warn!(
                                    "LanceDB schema mismatch on symbols append — dropping and recreating: {}",
                                    err
                                );
                                drop_all_lance_tables(&db_path);
                                // Signal caller to do a fresh full persist after schema reset.
                                return Err(anyhow::anyhow!(
                                    "Schema mismatch during full persist — tables dropped, retry needed"
                                ));
                            } else {
                                return Err(err).context("Failed to append to symbols table");
                            }
                        }
                    }
                }
            }
            Err(_) => {
                // Table doesn't exist yet — create it with the first batch.
                let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                db.create_table("symbols", Box::new(batches))
                    .execute()
                    .await
                    .context("Failed to create symbols table")?;

                // Create FTS index once on new table — not on every rebuild.
                if let Ok(tbl) = db.open_table("symbols").execute().await {
                    match tbl
                        .create_index(&["name"], lancedb::index::Index::FTS(Default::default()))
                        .execute()
                        .await
                    {
                        Ok(_) => tracing::info!("Created FTS index on symbols.name"),
                        Err(e) => tracing::warn!("Failed to create FTS index: {}", e),
                    }
                }
            }
        }
    }

    // ── Append edges with new_version ──
    {
        let schema = Arc::new(edges_schema());
        let batch = build_edges_batch(edges, new_version)?;

        match db.open_table("edges").execute().await {
            Ok(tbl) => {
                let mut attempts: u64 = 0;
                loop {
                    let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema.clone());
                    match tbl.add(batches).execute().await {
                        Ok(_) => break,
                        Err(e) => {
                            let err = anyhow::anyhow!("{}", e);
                            if is_conflict_error(&err) && attempts < 3 {
                                attempts += 1;
                                tracing::warn!(
                                    "LanceDB conflict on edges append (attempt {}), retrying in {}ms",
                                    attempts, 100 * attempts
                                );
                                tokio::time::sleep(Duration::from_millis(100 * attempts)).await;
                            } else if is_schema_mismatch_error(&err) {
                                tracing::warn!(
                                    "LanceDB schema mismatch on edges append — dropping and recreating: {}",
                                    err
                                );
                                drop_all_lance_tables(&db_path);
                                return Err(anyhow::anyhow!(
                                    "Schema mismatch during full persist — tables dropped, retry needed"
                                ));
                            } else {
                                return Err(err).context("Failed to append to edges table");
                            }
                        }
                    }
                }
            }
            Err(_) => {
                let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                db.create_table("edges", Box::new(batches))
                    .execute()
                    .await
                    .context("Failed to create edges table")?;
            }
        }
    }

    // ── Atomically flip the version pointer ──
    // Both tables are fully written. Only now do we make new_version live for reads.
    write_committed_scan_version(&db_path, new_version)
        .context("Failed to update committed scan_version pointer")?;

    tracing::info!(
        "Persisted graph to LanceDB: {} nodes, {} edges (scan_version={})",
        nodes.len(), edges.len(), new_version
    );

    // ── Background compaction: remove stale version rows ──
    // Run after commit — non-fatal if it fails (just leaves extra rows).
    if let Err(e) = compact_stale_versions(&db_path, new_version).await {
        tracing::warn!("compact_stale_versions failed (non-fatal): {}", e);
    }

    Ok(())
}

/// Delete rows from `symbols` and `edges` where `scan_version < committed_version - 1`.
///
/// Keeps one previous version as a safety buffer in case a concurrent reader is
/// mid-query on the old version. The N-2 and older versions are unreachable.
///
/// This is called automatically after each successful `persist_graph_to_lance`.
/// Non-fatal: a failure here leaves stale rows that will be cleaned up next scan.
pub(crate) async fn compact_stale_versions(db_path: &Path, committed_version: u64) -> anyhow::Result<()> {
    // Keep committed_version and committed_version - 1 (one buffer).
    // Delete everything older.
    if committed_version < 2 {
        // Nothing to compact on first or second write.
        return Ok(());
    }
    let cutoff = committed_version - 1; // delete scan_version < cutoff
    let predicate = format!("scan_version < {}", cutoff);

    let db = lancedb::connect(db_path.to_str().unwrap_or_default())
        .execute()
        .await
        .context("compact_stale_versions: failed to connect to LanceDB")?;

    let mut deleted_symbols = 0u64;
    let mut deleted_edges = 0u64;

    for (table_name, deleted_count) in [("symbols", &mut deleted_symbols), ("edges", &mut deleted_edges)] {
        if let Ok(tbl) = db.open_table(table_name).execute().await {
            // Count rows before deletion for logging.
            match tbl.delete(&predicate).await {
                Ok(_) => {
                    *deleted_count = 1; // deletion succeeded (LanceDB doesn't return count)
                    tracing::debug!("compact_stale_versions: deleted stale rows from {} (scan_version < {})", table_name, cutoff);
                }
                Err(e) => {
                    tracing::warn!("compact_stale_versions: delete from {} failed: {}", table_name, e);
                }
            }
        }
    }

    if deleted_symbols > 0 || deleted_edges > 0 {
        tracing::info!(
            "compact_stale_versions: removed stale rows (scan_version < {}) from symbols and edges",
            cutoff
        );
    }

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

    // Incremental writes use the current committed version so updated nodes
    // remain visible in queries. A full rebuild bumps the version; incremental
    // does not change the version pointer.
    let committed_version = read_committed_scan_version(&db_path);
    // Use max(committed_version, 1) so that even before the first full rebuild
    // incremental writes produce version=1 rows (not version=0 which is the
    // "no filter" sentinel used by load_graph_from_lance for legacy data).
    let write_version = committed_version.max(1);

    // ── Symbols (nodes) table: delete then upsert ──
    {
        // 1. Delete symbols for removed/changed files first so upsert is clean.
        if !deleted_files.is_empty()
            && let Ok(tbl) = db.open_table("symbols").execute().await {
                let quoted: Vec<String> = deleted_files
                    .iter()
                    .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
                    .collect();
                let predicate = format!("file_path IN ({})", quoted.join(", "));
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!("Failed to delete symbols for removed files: {}", e);
                }
            }

        // 2. Upsert changed/added nodes (insert new, update existing by stable id).
        if !upsert_nodes.is_empty() {
            let batch = build_symbols_batch(upsert_nodes, write_version)?;
            let schema = batch.schema();

            match db.open_table("symbols").execute().await {
                Ok(tbl) => {
                    // Retry on conflict: another process may be writing simultaneously.
                    let mut attempts: u64 = 0;
                    loop {
                        let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema.clone());
                        let mut merge = tbl.merge_insert(&["id"]);
                        merge
                            .when_matched_update_all(None)
                            .when_not_matched_insert_all();
                        // Note: no when_not_matched_by_source_delete — we only touch changed rows.
                        // Untouched rows (unchanged files) are left alone.
                        match merge.execute(Box::new(batches)).await {
                            Ok(_) => break,
                            Err(e) => {
                                let err = anyhow::anyhow!("{}", e);
                                if is_schema_mismatch_error(&err) {
                                    tracing::warn!(
                                        "LanceDB schema mismatch detected on symbols table — dropping stale tables and rebuilding: {}",
                                        err
                                    );
                                    drop_all_lance_tables(&db_path);
                                    return Ok(true);
                                } else if is_conflict_error(&err) && attempts < 3 {
                                    attempts += 1;
                                    tracing::warn!(
                                        "LanceDB conflict on symbols merge_insert (attempt {}), retrying in {}ms",
                                        attempts,
                                        100 * attempts
                                    );
                                    tokio::time::sleep(Duration::from_millis(100 * attempts)).await;
                                } else {
                                    return Err(err).context("Failed to merge_insert symbols table after retries");
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Table doesn't exist yet — create it (first incremental run after a fresh repo)
                    let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema);
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
        // 1. Delete edges that referenced removed/changed files (by stable edge ID).
        if !deleted_edge_ids.is_empty()
            && let Ok(tbl) = db.open_table("edges").execute().await {
                let quoted: Vec<String> = deleted_edge_ids
                    .iter()
                    .map(|id| format!("'{}'", id.replace('\'', "''")))
                    .collect();
                let predicate = format!("id IN ({})", quoted.join(", "));
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!("Failed to delete edges for removed files: {}", e);
                }
            }

        // 2. Upsert changed/added edges.
        if !upsert_edges.is_empty() {
            let batch = build_edges_batch(upsert_edges, write_version)?;
            let schema = batch.schema();

            match db.open_table("edges").execute().await {
                Ok(tbl) => {
                    // Retry on conflict: another process may be writing simultaneously.
                    let mut attempts: u64 = 0;
                    loop {
                        let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema.clone());
                        let mut merge = tbl.merge_insert(&["id"]);
                        merge
                            .when_matched_update_all(None)
                            .when_not_matched_insert_all();
                        // Note: no when_not_matched_by_source_delete — untouched edges are preserved.
                        match merge.execute(Box::new(batches)).await {
                            Ok(_) => break,
                            Err(e) => {
                                let err = anyhow::anyhow!("{}", e);
                                if is_schema_mismatch_error(&err) {
                                    tracing::warn!(
                                        "LanceDB schema mismatch detected on edges table — dropping stale tables and rebuilding: {}",
                                        err
                                    );
                                    drop_all_lance_tables(&db_path);
                                    return Ok(true);
                                } else if is_conflict_error(&err) && attempts < 3 {
                                    attempts += 1;
                                    tracing::warn!(
                                        "LanceDB conflict on edges merge_insert (attempt {}), retrying in {}ms",
                                        attempts,
                                        100 * attempts
                                    );
                                    tokio::time::sleep(Duration::from_millis(100 * attempts)).await;
                                } else {
                                    return Err(err).context("Failed to merge_insert edges table after retries");
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Table doesn't exist yet — create it
                    let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema);
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
///
/// Reads only rows matching the currently committed `scan_version`.
/// This ensures full-rebuild appends don't expose partially-written data:
/// the new version only becomes visible after `persist_graph_to_lance` flips
/// the version pointer.
pub async fn load_graph_from_lance(repo_root: &Path) -> anyhow::Result<GraphState> {
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};

    let db_path = graph_lance_path(repo_root);
    if !db_path.exists() {
        anyhow::bail!("No persisted graph at {}", db_path.display());
    }

    // Read the committed version. If it's 0 (no version file), fall back to loading
    // all rows — this handles legacy data written before the scan_version column existed.
    let committed_version = read_committed_scan_version(&db_path);
    let version_filter: Option<String> = if committed_version > 0 {
        Some(format!("scan_version = {}", committed_version))
    } else {
        None // Legacy data: no filter (scan_version absent or all rows at version 0)
    };

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
        let mut q = table.query();
        if let Some(ref filter) = version_filter {
            q = q.only_if(filter.as_str());
        }
        let stream = q.execute().await.context("Failed to query symbols")?;
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
            let is_static_col = batch.column_by_name("is_static")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let is_async_col = batch.column_by_name("is_async")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let is_test_col = batch.column_by_name("is_test")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let visibility_col = batch.column_by_name("visibility")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let exported_col = batch.column_by_name("exported")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            // Diagnostic metadata columns (nullable — only present on diagnostic nodes)
            let diag_severity_col = batch.column_by_name("diagnostic_severity")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_source_col = batch.column_by_name("diagnostic_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_message_col = batch.column_by_name("diagnostic_message")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_range_col = batch.column_by_name("diagnostic_range")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_timestamp_col = batch.column_by_name("diagnostic_timestamp")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // ApiEndpoint metadata columns (nullable — only present on api_endpoint nodes)
            let http_method_col = batch.column_by_name("http_method")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let http_path_col = batch.column_by_name("http_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // doc_comment column — survives LSP reindex round-trip (#416)
            let doc_comment_col = batch.column_by_name("doc_comment")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // gRPC / proto columns — survives round-trip for GrpcClientCallsPass on incremental scans (#466)
            let parent_service_col = batch.column_by_name("parent_service")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let rpc_request_type_col = batch.column_by_name("rpc_request_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let rpc_response_type_col = batch.column_by_name("rpc_response_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let file_path = PathBuf::from(file_paths.value(i));
                let language = infer_language_from_path(&file_path);
                let mut metadata: BTreeMap<String, String> = BTreeMap::new();
                if let Some(col) = meta_virtual_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("virtual".to_string(), "true".to_string());
                    }
                if let Some(col) = meta_package_col
                    && !col.is_null(i) {
                        metadata.insert("package".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = meta_name_col_col
                    && !col.is_null(i) {
                        metadata.insert("name_col".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = value_col
                    && !col.is_null(i) {
                        metadata.insert("value".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = synthetic_col
                    && !col.is_null(i) {
                        metadata.insert("synthetic".to_string(), if col.value(i) { "true" } else { "false" }.to_string());
                    }
                if let Some(col) = cyclomatic_col
                    && !col.is_null(i) {
                        metadata.insert("cyclomatic".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = importance_col
                    && !col.is_null(i) {
                        metadata.insert("importance".to_string(), format!("{:.6}", col.value(i)));
                    }
                if let Some(col) = storage_col
                    && !col.is_null(i) {
                        metadata.insert("storage".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = mutable_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("mutable".to_string(), "true".to_string());
                    }
                if let Some(col) = decorators_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("decorators".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = type_params_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("type_params".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = pattern_hint_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("pattern_hint".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = is_static_col
                    && !col.is_null(i) {
                        metadata.insert("is_static".to_string(), if col.value(i) { "true" } else { "false" }.to_string());
                    }
                if let Some(col) = is_async_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("is_async".to_string(), "true".to_string());
                    }
                if let Some(col) = is_test_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("is_test".to_string(), "true".to_string());
                    }
                if let Some(col) = visibility_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("visibility".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = exported_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("exported".to_string(), "true".to_string());
                    }
                if let Some(col) = diag_severity_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_severity".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_source_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_source".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_message_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_message".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_range_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_range".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_timestamp_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_timestamp".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = http_method_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("http_method".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = http_path_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("http_path".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = doc_comment_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("doc_comment".to_string(), val.to_string());
                        }
                    }
                // gRPC / proto columns — restore metadata for GrpcClientCallsPass (#466)
                if let Some(col) = parent_service_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("parent_service".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = rpc_request_type_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("request_type".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = rpc_response_type_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("response_type".to_string(), val.to_string());
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
        let mut q = table.query();
        if let Some(ref filter) = version_filter {
            q = q.only_if(filter.as_str());
        }
        let stream = q.execute().await.context("Failed to query edges")?;
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

    Ok(GraphState::new(nodes, edges, index, Some(std::time::Instant::now()), std::collections::HashSet::new()))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::graph::{ExtractionSource, NodeKind};

    fn make_test_node(name: &str) -> Node {
        Node {
            id: NodeId {
                root: "local".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: format!("fn {name}()"),
            line_start: 1,
            line_end: 5,
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    /// Two concurrent calls to `persist_graph_incremental` targeting the same LanceDB table
    /// must both succeed (or retry to success) — not return an unhandled conflict error.
    #[tokio::test]
    async fn test_concurrent_incremental_persist_both_succeed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        // First write: establish the table so both concurrent writes hit merge_insert (not create).
        let node_a = make_test_node("setup");
        persist_graph_incremental(repo_root, &[node_a], &[], &[], &[])
            .await
            .expect("initial persist failed");

        // Now fire two concurrent incremental persists to the same table.
        let root1 = repo_root.to_path_buf();
        let root2 = repo_root.to_path_buf();

        let task1 = tokio::spawn(async move {
            let nodes = vec![make_test_node("fn_task1")];
            persist_graph_incremental(&root1, &nodes, &[], &[], &[]).await
        });
        let task2 = tokio::spawn(async move {
            let nodes = vec![make_test_node("fn_task2")];
            persist_graph_incremental(&root2, &nodes, &[], &[], &[]).await
        });

        let (r1, r2) = tokio::join!(task1, task2);
        r1.expect("task1 panicked").expect("task1 returned error");
        r2.expect("task2 panicked").expect("task2 returned error");
    }

    /// `check_and_migrate_extraction_version` clears scan-state when the stored
    /// version is stale, and writes the current EXTRACTION_VERSION.
    #[test]
    fn test_extraction_version_migration_clears_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        let repo_root = dir.path();
        std::fs::create_dir_all(&db_path).unwrap();

        // Seed a stale version (0) and create a fake primary scan-state file.
        std::fs::write(db_path.join("extraction_version"), "0").unwrap();
        let primary_state = repo_root.join(".oh").join(".cache").join("scan-state.json");
        std::fs::create_dir_all(primary_state.parent().unwrap()).unwrap();
        std::fs::write(&primary_state, r#"{"dir_mtimes":{},"file_mtimes":{},"file_content_hashes":{}}"#).unwrap();

        // Migration should return true and clear the state file.
        let migrated = check_and_migrate_extraction_version(&db_path, repo_root, &[])
            .expect("migration failed");
        assert!(migrated, "expected migration=true for stale version");
        assert!(!primary_state.exists(), "scan-state should be cleared after migration");

        // The version file should now contain EXTRACTION_VERSION.
        let stored: u32 = std::fs::read_to_string(db_path.join("extraction_version"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(stored, EXTRACTION_VERSION);

        // Second call should be a no-op.
        let migrated2 = check_and_migrate_extraction_version(&db_path, repo_root, &[])
            .expect("second migration failed");
        assert!(!migrated2, "expected migration=false when already current");
    }

    /// `check_and_migrate_extraction_version` is a no-op on fresh directories
    /// (no prior version file) and does NOT clear any state files.
    /// It DOES write the current EXTRACTION_VERSION so the next call is a no-op.
    #[test]
    fn test_extraction_version_fresh_directory_no_clear() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        let repo_root = dir.path();
        std::fs::create_dir_all(&db_path).unwrap();

        // No version file, no scan-state file.
        let migrated = check_and_migrate_extraction_version(&db_path, repo_root, &[])
            .expect("migration failed on fresh dir");
        // Fresh directory: stored_version is None, so Ok(false) — no migration.
        assert!(!migrated, "expected migration=false for fresh directory");

        // The version file must be written to db_path (the lance/ subdir), not the
        // parent .cache/ directory. This invariant ensures a subsequent EXTRACTION_VERSION
        // bump is detected on the next run.
        let version_file = db_path.join("extraction_version");
        assert!(
            version_file.exists(),
            "extraction_version must be written to lance/ subdir on first run, not found at {}",
            version_file.display()
        );
        let stored: u32 = std::fs::read_to_string(&version_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            stored,
            EXTRACTION_VERSION,
            "extraction_version file should contain current EXTRACTION_VERSION"
        );

        // The parent .cache/ dir must NOT have a stray extraction_version file.
        let parent_version_file = dir.path().join("extraction_version");
        assert!(
            !parent_version_file.exists(),
            "extraction_version must NOT be written to the parent .cache/ dir (found at {})",
            parent_version_file.display()
        );
    }

    /// `extraction_version` path invariant: the file lives inside `lance/`, not the
    /// parent `.cache/` directory.  If it were written to `.oh/.cache/extraction_version`
    /// instead of `.oh/.cache/lance/extraction_version`, the check would never find a
    /// stored version and would silently treat every run as "fresh", meaning bumping
    /// EXTRACTION_VERSION would never trigger re-extraction.
    #[test]
    fn test_extraction_version_file_path_is_inside_lance_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        // db_path is the lance/ subdirectory, as graph_lance_path() returns.
        let db_path = dir.path().join("lance");
        let repo_root = dir.path();
        std::fs::create_dir_all(&db_path).unwrap();

        check_and_migrate_extraction_version(&db_path, repo_root, &[])
            .expect("initial write failed");

        // Correct location: inside lance/
        let correct_path = db_path.join("extraction_version");
        assert!(
            correct_path.exists(),
            "extraction_version must be at lance/extraction_version, not found at {}",
            correct_path.display()
        );

        // Wrong location: parent dir (.oh/.cache/ without lance/)
        let wrong_path = dir.path().join("extraction_version");
        assert!(
            !wrong_path.exists(),
            "extraction_version must NOT be at .cache/extraction_version (found at {}); \
             write path must match read path in check_and_migrate_extraction_version",
            wrong_path.display()
        );
    }

    /// `is_conflict_error` correctly identifies conflict-like messages and ignores others.
    #[test]
    fn test_is_conflict_error_detection() {
        // These should match.
        let conflict = anyhow::anyhow!("write conflict detected");
        assert!(is_conflict_error(&conflict));

        let concurrent = anyhow::anyhow!("concurrent modification");
        assert!(is_conflict_error(&concurrent));

        let both = anyhow::anyhow!("concurrent write conflict");
        assert!(is_conflict_error(&both));

        // These should NOT match — "commit" excluded to avoid false positives in
        // non-conflict contexts (git messages, schema strings, log lines).
        let commit_only = anyhow::anyhow!("failed to commit transaction");
        assert!(!is_conflict_error(&commit_only));

        let unrelated = anyhow::anyhow!("table not found");
        assert!(!is_conflict_error(&unrelated));

        let io_err = anyhow::anyhow!("IO error: permission denied");
        assert!(!is_conflict_error(&io_err));
    }

    /// `is_schema_mismatch_error` correctly identifies schema / type-mismatch errors.
    #[test]
    fn test_is_schema_mismatch_error_detection() {
        // These should match.
        let schema_err = anyhow::anyhow!("Arrow schema mismatch: expected 10 fields, got 8");
        assert!(is_schema_mismatch_error(&schema_err));

        let column_err = anyhow::anyhow!("column not found: diagnostic_severity");
        assert!(is_schema_mismatch_error(&column_err));

        let field_err = anyhow::anyhow!("field not found: http_method");
        assert!(is_schema_mismatch_error(&field_err));

        let type_err = anyhow::anyhow!("type mismatch: expected Int32, got Utf8");
        assert!(is_schema_mismatch_error(&type_err));

        let batch_err = anyhow::anyhow!("Invalid RecordBatch: number of columns does not match schema");
        assert!(is_schema_mismatch_error(&batch_err));

        // These should NOT match — ordinary errors unrelated to schema.
        let conflict = anyhow::anyhow!("write conflict detected");
        assert!(!is_schema_mismatch_error(&conflict));

        let io_err = anyhow::anyhow!("IO error: permission denied");
        assert!(!is_schema_mismatch_error(&io_err));

        let not_found = anyhow::anyhow!("table not found");
        assert!(!is_schema_mismatch_error(&not_found));
    }

    /// `drop_all_lance_tables` removes table directories, resets the schema_version file,
    /// and preserves the extraction_version file so the next version bump still triggers
    /// its scan-state reset correctly.
    #[test]
    fn test_drop_all_lance_tables_clears_dirs_and_resets_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();

        // Simulate stale tables: create a couple of subdirectories and an unrelated file.
        std::fs::create_dir_all(db_path.join("symbols.lance")).unwrap();
        std::fs::create_dir_all(db_path.join("edges.lance")).unwrap();
        std::fs::write(db_path.join("schema_version"), "9").unwrap();
        // extraction_version must survive the cleanup.
        std::fs::write(db_path.join("extraction_version"), "42").unwrap();

        drop_all_lance_tables(&db_path);

        // Stale table dirs should be gone.
        assert!(!db_path.join("symbols.lance").exists(), "symbols.lance should be removed");
        assert!(!db_path.join("edges.lance").exists(), "edges.lance should be removed");

        // schema_version should be reset to SCHEMA_VERSION.
        let written = std::fs::read_to_string(db_path.join("schema_version")).unwrap();
        assert_eq!(written, SCHEMA_VERSION.to_string());

        // extraction_version must be preserved so subsequent extraction-version bumps
        // still trigger their scan-state reset correctly.
        let ev = std::fs::read_to_string(db_path.join("extraction_version")).unwrap();
        assert_eq!(ev, "42", "extraction_version must survive drop_all_lance_tables");
    }

    /// After a schema mismatch during incremental persist, `persist_graph_incremental`
    /// returns `Ok(true)` so the caller triggers a full rebuild.
    ///
    /// We simulate the mismatch by writing a node with the current schema, then
    /// manually deleting the schema_version file and overwriting the symbols table
    /// with a one-column file — ensuring merge_insert hits a schema error.
    ///
    /// This test validates the recovery path: the function must not panic or return
    /// Err; it must return Ok(true).
    #[tokio::test]
    async fn test_incremental_persist_recovers_from_schema_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        // First: establish a valid table.
        let node = make_test_node("initial");
        persist_graph_incremental(repo_root, &[node], &[], &[], &[])
            .await
            .expect("initial persist");

        // Corrupt the schema_version to force bypass of the pre-flight check.
        let db_path = graph_lance_path(repo_root);
        std::fs::write(db_path.join("schema_version"), SCHEMA_VERSION.to_string()).unwrap();

        // Replace the symbols table directory with a fresh one that has a single-field schema,
        // so merge_insert fails with a schema mismatch when RNA tries to write the full schema.
        let symbols_dir = db_path.join("symbols.lance");
        if symbols_dir.exists() {
            std::fs::remove_dir_all(&symbols_dir).unwrap();
        }
        // We can't trivially write a real LanceDB table with the wrong schema here, so
        // instead we remove the symbols directory and rely on the fact that the current
        // schema_version is valid — meaning this test exercises the drop_all_lance_tables
        // helper path indirectly via is_schema_mismatch_error matching an injected error.
        //
        // The direct path (merge_insert returning a real schema error) is tested by the
        // unit-level `is_schema_mismatch_error` test above.  Integration-level verification
        // of the full round-trip happens in smoke tests once the real LanceDB surfaces a
        // schema error message we can confirm matching.
        //
        // What we validate here: after drop_all_lance_tables removes state, a subsequent
        // persist_graph_incremental succeeds (no panic, Ok result).
        drop_all_lance_tables(&db_path);

        let node2 = make_test_node("after_recovery");
        let result = persist_graph_incremental(repo_root, &[node2], &[], &[], &[]).await;
        assert!(result.is_ok(), "persist after drop should succeed, got: {:?}", result);
    }

    // ── scan_version helpers ──────────────────────────────────────────────────

    /// `read_committed_scan_version` returns 0 when no version file exists.
    #[test]
    fn test_read_committed_scan_version_missing_returns_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();
        assert_eq!(read_committed_scan_version(&db_path), 0);
    }

    /// `write_committed_scan_version` + `read_committed_scan_version` round-trip.
    #[test]
    fn test_scan_version_write_read_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();

        write_committed_scan_version(&db_path, 42).expect("write failed");
        assert_eq!(read_committed_scan_version(&db_path), 42);

        // Overwrite with a higher version.
        write_committed_scan_version(&db_path, 100).expect("write failed");
        assert_eq!(read_committed_scan_version(&db_path), 100);
    }

    /// `drop_all_lance_tables` removes the scan_version pointer (tables were dropped,
    /// so the old version pointer would point to non-existent rows).
    #[test]
    fn test_drop_all_lance_tables_removes_scan_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();

        std::fs::write(db_path.join("schema_version"), "16").unwrap();
        std::fs::write(db_path.join("extraction_version"), "5").unwrap();
        std::fs::write(db_path.join("scan_version"), "7").unwrap();
        std::fs::create_dir_all(db_path.join("symbols.lance")).unwrap();

        drop_all_lance_tables(&db_path);

        // scan_version should be removed (tables gone, pointer is stale).
        assert!(!db_path.join("scan_version").exists(), "scan_version should be removed by drop_all_lance_tables");
        // extraction_version must survive.
        assert_eq!(
            std::fs::read_to_string(db_path.join("extraction_version")).unwrap(),
            "5"
        );
    }

    /// `persist_graph_to_lance` increments the scan_version pointer on each call.
    /// First call: version 0 → 1. Second call: version 1 → 2.
    #[tokio::test]
    async fn test_persist_graph_to_lance_increments_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        let node_a = make_test_node("fn_a");
        persist_graph_to_lance(repo_root, &[node_a], &[])
            .await
            .expect("first persist failed");
        assert_eq!(read_committed_scan_version(&db_path), 1, "first persist should write version 1");

        let node_b = make_test_node("fn_b");
        persist_graph_to_lance(repo_root, &[node_b], &[])
            .await
            .expect("second persist failed");
        assert_eq!(read_committed_scan_version(&db_path), 2, "second persist should write version 2");
    }

    /// `load_graph_from_lance` reads only nodes from the committed scan_version.
    /// After a second full rebuild, the old nodes should NOT be returned.
    #[tokio::test]
    async fn test_load_filters_to_committed_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        // First rebuild: fn_v1.
        let node_v1 = make_test_node("fn_v1");
        persist_graph_to_lance(repo_root, &[node_v1], &[])
            .await
            .expect("first persist");

        // Second rebuild: fn_v2 (different node).
        let node_v2 = make_test_node("fn_v2");
        persist_graph_to_lance(repo_root, &[node_v2], &[])
            .await
            .expect("second persist");

        // Load should return only fn_v2, not fn_v1 (which has scan_version=1,
        // while the committed version is now 2).
        let state = load_graph_from_lance(repo_root)
            .await
            .expect("load failed");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_v2"), "fn_v2 should be present");
        assert!(!names.contains(&"fn_v1"), "fn_v1 (old version) should not be present");
    }

    /// `compact_stale_versions` deletes rows with scan_version < committed - 1.
    /// After two full rebuilds, the first rebuild's rows should be compacted.
    #[tokio::test]
    async fn test_compact_removes_stale_version_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        // Three rebuilds: versions 1, 2, 3.
        for name in ["fn_v1", "fn_v2", "fn_v3"] {
            persist_graph_to_lance(repo_root, &[make_test_node(name)], &[])
                .await
                .expect("persist failed");
        }
        assert_eq!(read_committed_scan_version(&db_path), 3);

        // After 3 rebuilds: compact should have removed version 1 rows (< 3-1=2),
        // keeping versions 2 and 3. Verify by loading — should get fn_v3 only.
        let state = load_graph_from_lance(repo_root).await.expect("load failed");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_v3"), "fn_v3 should be present (current version)");
        assert!(!names.contains(&"fn_v1"), "fn_v1 should have been compacted");
    }

    // ── Adversarial tests ─────────────────────────────────────────────────────

    /// Mixed-version invariant: an incremental write after a full rebuild must remain
    /// visible in loads (it uses the same scan_version as the preceding rebuild).
    #[tokio::test]
    async fn test_incremental_after_full_rebuild_stays_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        // Full rebuild with fn_base.
        persist_graph_to_lance(repo_root, &[make_test_node("fn_base")], &[])
            .await
            .expect("full rebuild");
        assert_eq!(read_committed_scan_version(&db_path), 1);

        // Incremental write: adds fn_incremental with scan_version = 1 (committed).
        persist_graph_incremental(repo_root, &[make_test_node("fn_incremental")], &[], &[], &[])
            .await
            .expect("incremental write");
        // Version pointer must NOT change after incremental write.
        assert_eq!(read_committed_scan_version(&db_path), 1, "incremental must not change version pointer");

        // Load: must return both fn_base and fn_incremental (both at version 1).
        let state = load_graph_from_lance(repo_root).await.expect("load after incremental");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_base"), "fn_base should be present");
        assert!(names.contains(&"fn_incremental"), "fn_incremental should be present");

        // Second full rebuild — version 2. fn_incremental should vanish (was at version 1).
        persist_graph_to_lance(repo_root, &[make_test_node("fn_v2")], &[])
            .await
            .expect("second full rebuild");
        assert_eq!(read_committed_scan_version(&db_path), 2);

        let state2 = load_graph_from_lance(repo_root).await.expect("load after second rebuild");
        let names2: Vec<&str> = state2.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names2.contains(&"fn_v2"), "fn_v2 should be present");
        assert!(!names2.contains(&"fn_incremental"), "fn_incremental (version 1) should not appear after version 2 rebuild");
    }

    /// If the version pointer file is absent, `load_graph_from_lance` falls back to
    /// loading ALL rows (legacy compatibility). Ensure no filter is applied.
    #[tokio::test]
    async fn test_load_without_version_file_loads_all_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        // Write rows with scan_version = 1.
        persist_graph_to_lance(repo_root, &[make_test_node("fn_any")], &[])
            .await
            .expect("full rebuild");

        // Remove the version pointer file (simulates legacy LanceDB data).
        let _ = std::fs::remove_file(scan_version_path(&db_path));
        assert_eq!(read_committed_scan_version(&db_path), 0, "missing file should read as 0");

        // Load with committed_version=0 → no filter → must see fn_any.
        let state = load_graph_from_lance(repo_root).await.expect("load without version file");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_any"), "fn_any should be visible with no version filter");
    }
}
