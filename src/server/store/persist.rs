//! Graph persistence: full persist, incremental upsert, compaction, and root pruning.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use arrow_array::RecordBatchIterator;

use crate::graph::{Node, Edge};
use crate::graph::store::{symbols_schema, edges_schema, SCHEMA_VERSION};

use super::batch::{build_symbols_batch, build_edges_batch};
use super::migrate::{
    check_and_migrate_schema, drop_all_lance_tables, is_conflict_error, is_schema_mismatch_error,
    read_committed_scan_version, write_committed_scan_version,
};
use super::graph_lance_path;

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
        tracing::info!("Schema migrated to v{} -- cache rebuilt", SCHEMA_VERSION);
    }

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for graph persistence")?;

    // Determine the next scan_version to write.
    // current committed version -> write to current + 1.
    let committed_version = read_committed_scan_version(&db_path);
    let new_version = committed_version + 1;

    tracing::debug!(
        "persist_graph_to_lance: committed_version={} -> writing new_version={}",
        committed_version, new_version
    );

    // -- Append symbols (nodes) with new_version --
    {
        let schema = Arc::new(symbols_schema());
        let batch = build_symbols_batch(nodes, new_version)?;

        match db.open_table("symbols").execute().await {
            Ok(tbl) => {
                // Table exists -- append the new-version rows.
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
                                    "LanceDB schema mismatch on symbols append -- dropping and recreating: {}",
                                    err
                                );
                                drop_all_lance_tables(&db_path);
                                // Signal caller to do a fresh full persist after schema reset.
                                return Err(anyhow::anyhow!(
                                    "Schema mismatch during full persist -- tables dropped, retry needed"
                                ));
                            } else {
                                return Err(err).context("Failed to append to symbols table");
                            }
                        }
                    }
                }
            }
            Err(_) => {
                // Table doesn't exist yet -- create it with the first batch.
                let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
                db.create_table("symbols", Box::new(batches))
                    .execute()
                    .await
                    .context("Failed to create symbols table")?;

                // Create FTS index once on new table -- not on every rebuild.
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

    // -- Append edges with new_version --
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
                                    "LanceDB schema mismatch on edges append -- dropping and recreating: {}",
                                    err
                                );
                                drop_all_lance_tables(&db_path);
                                return Err(anyhow::anyhow!(
                                    "Schema mismatch during full persist -- tables dropped, retry needed"
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

    // -- Atomically flip the version pointer --
    // Both tables are fully written. Only now do we make new_version live for reads.
    write_committed_scan_version(&db_path, new_version)
        .context("Failed to update committed scan_version pointer")?;

    tracing::info!(
        "Persisted graph to LanceDB: {} nodes, {} edges (scan_version={})",
        nodes.len(), edges.len(), new_version
    );

    // -- Background compaction: remove stale version rows --
    // Run after commit -- non-fatal if it fails (just leaves extra rows).
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
/// Unlike `persist_graph_to_lance` (DROP+CREATE), this keeps the tables alive during writes --
/// no query window with empty results.
///
/// # Parameters
/// - `upsert_nodes`: only the changed or newly added nodes (not the full graph)
/// - `upsert_edges`: only the changed or newly added edges (not the full graph)
/// - `deleted_edge_ids`: stable IDs of edges that reference removed/changed files -- collected
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
        tracing::info!("Schema migrated to v{} during incremental update -- cache rebuilt; caller should do a full persist", SCHEMA_VERSION);
        // Migration dropped stale tables -- incremental upsert against empty
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

    // -- Symbols (nodes) table: delete then upsert --
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
                        // Note: no when_not_matched_by_source_delete -- we only touch changed rows.
                        // Untouched rows (unchanged files) are left alone.
                        match merge.execute(Box::new(batches)).await {
                            Ok(_) => break,
                            Err(e) => {
                                let err = anyhow::anyhow!("{}", e);
                                if is_schema_mismatch_error(&err) {
                                    tracing::warn!(
                                        "LanceDB schema mismatch detected on symbols table -- dropping stale tables and rebuilding: {}",
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
                    // Table doesn't exist yet -- create it (first incremental run after a fresh repo)
                    let batches = RecordBatchIterator::new(vec![Ok(batch.clone())], schema);
                    db.create_table("symbols", Box::new(batches))
                        .execute()
                        .await
                        .context("Failed to create symbols table")?;
                }
            }
        }
    }

    // -- Edges table: delete then upsert --
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
                        // Note: no when_not_matched_by_source_delete -- untouched edges are preserved.
                        match merge.execute(Box::new(batches)).await {
                            Ok(_) => break,
                            Err(e) => {
                                let err = anyhow::anyhow!("{}", e);
                                if is_schema_mismatch_error(&err) {
                                    tracing::warn!(
                                        "LanceDB schema mismatch detected on edges table -- dropping stale tables and rebuilding: {}",
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
                    // Table doesn't exist yet -- create it
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

/// Query LanceDB for all distinct `root_id` values stored across all tables.
///
/// Scans the same set of tables that `delete_nodes_for_roots` prunes so that
/// stale roots present in any table are discovered (not just symbols).
pub(crate) async fn get_stored_root_ids(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    use arrow_array::StringArray;
    use arrow_array::Array;
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
                && let Some(arr) = col.as_any().downcast_ref::<StringArray>() {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use super::super::graph_lance_path;
    use super::super::migrate::{read_committed_scan_version, scan_version_path, drop_all_lance_tables};
    use super::super::load::load_graph_from_lance;
    use crate::graph::{ExtractionSource, NodeId, NodeKind};

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

    #[tokio::test]
    async fn test_concurrent_incremental_persist_both_succeed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        let node_a = make_test_node("setup");
        persist_graph_incremental(repo_root, &[node_a], &[], &[], &[])
            .await
            .expect("initial persist failed");

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

    #[tokio::test]
    async fn test_incremental_persist_recovers_from_schema_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        let node = make_test_node("initial");
        persist_graph_incremental(repo_root, &[node], &[], &[], &[])
            .await
            .expect("initial persist");

        let db_path = graph_lance_path(repo_root);
        std::fs::write(db_path.join("schema_version"), SCHEMA_VERSION.to_string()).unwrap();

        let symbols_dir = db_path.join("symbols.lance");
        if symbols_dir.exists() {
            std::fs::remove_dir_all(&symbols_dir).unwrap();
        }
        drop_all_lance_tables(&db_path);

        let node2 = make_test_node("after_recovery");
        let result = persist_graph_incremental(repo_root, &[node2], &[], &[], &[]).await;
        assert!(result.is_ok(), "persist after drop should succeed, got: {:?}", result);
    }

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

    #[tokio::test]
    async fn test_load_filters_to_committed_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        let node_v1 = make_test_node("fn_v1");
        persist_graph_to_lance(repo_root, &[node_v1], &[])
            .await
            .expect("first persist");

        let node_v2 = make_test_node("fn_v2");
        persist_graph_to_lance(repo_root, &[node_v2], &[])
            .await
            .expect("second persist");

        let state = load_graph_from_lance(repo_root)
            .await
            .expect("load failed");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_v2"), "fn_v2 should be present");
        assert!(!names.contains(&"fn_v1"), "fn_v1 (old version) should not be present");
    }

    #[tokio::test]
    async fn test_compact_removes_stale_version_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        for name in ["fn_v1", "fn_v2", "fn_v3"] {
            persist_graph_to_lance(repo_root, &[make_test_node(name)], &[])
                .await
                .expect("persist failed");
        }
        assert_eq!(read_committed_scan_version(&db_path), 3);

        let state = load_graph_from_lance(repo_root).await.expect("load failed");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_v3"), "fn_v3 should be present (current version)");
        assert!(!names.contains(&"fn_v1"), "fn_v1 should have been compacted");
    }

    #[tokio::test]
    async fn test_incremental_after_full_rebuild_stays_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        persist_graph_to_lance(repo_root, &[make_test_node("fn_base")], &[])
            .await
            .expect("full rebuild");
        assert_eq!(read_committed_scan_version(&db_path), 1);

        persist_graph_incremental(repo_root, &[make_test_node("fn_incremental")], &[], &[], &[])
            .await
            .expect("incremental write");
        assert_eq!(read_committed_scan_version(&db_path), 1, "incremental must not change version pointer");

        let state = load_graph_from_lance(repo_root).await.expect("load after incremental");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_base"), "fn_base should be present");
        assert!(names.contains(&"fn_incremental"), "fn_incremental should be present");

        persist_graph_to_lance(repo_root, &[make_test_node("fn_v2")], &[])
            .await
            .expect("second full rebuild");
        assert_eq!(read_committed_scan_version(&db_path), 2);

        let state2 = load_graph_from_lance(repo_root).await.expect("load after second rebuild");
        let names2: Vec<&str> = state2.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names2.contains(&"fn_v2"), "fn_v2 should be present");
        assert!(!names2.contains(&"fn_incremental"), "fn_incremental (version 1) should not appear after version 2 rebuild");
    }

    #[tokio::test]
    async fn test_load_without_version_file_loads_all_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db_path = graph_lance_path(repo_root);

        persist_graph_to_lance(repo_root, &[make_test_node("fn_any")], &[])
            .await
            .expect("full rebuild");

        let _ = std::fs::remove_file(scan_version_path(&db_path));
        assert_eq!(read_committed_scan_version(&db_path), 0, "missing file should read as 0");

        let state = load_graph_from_lance(repo_root).await.expect("load without version file");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_any"), "fn_any should be visible with no version filter");
    }
}
