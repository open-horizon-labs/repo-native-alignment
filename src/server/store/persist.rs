//! Graph persistence: full persist, incremental upsert, compaction, and root pruning.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use arrow_array::RecordBatchIterator;
use lancedb::expr::{DfExpr, col, lit};

use crate::graph::store::SCHEMA_VERSION;
use crate::graph::{Edge, Node};

use super::batch::{build_edges_batch, build_symbols_batch};
use super::migrate::{
    check_and_migrate_schema, drop_all_lance_tables, is_conflict_error, is_schema_mismatch_error,
    read_committed_scan_version, write_committed_scan_version,
};
use super::{PREDICATE_BATCH_SIZE, graph_lance_path, string_isin};

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
        committed_version,
        new_version
    );

    // -- Append symbols (nodes) with new_version --
    {
        let batch = build_symbols_batch(nodes, new_version)?;

        match db.open_table("symbols").execute().await {
            Ok(tbl) => {
                // Table exists -- append the new-version rows.
                let mut attempts: u64 = 0;
                loop {
                    match tbl.add(batch.clone()).execute().await {
                        Ok(_) => break,
                        Err(e) => {
                            let err = anyhow::anyhow!("{}", e);
                            if is_conflict_error(&err) && attempts < 3 {
                                attempts += 1;
                                tracing::warn!(
                                    "LanceDB conflict on symbols append (attempt {}), retrying in {}ms",
                                    attempts,
                                    100 * attempts
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
                db.create_table("symbols", batch)
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
        let batch = build_edges_batch(edges, new_version)?;

        match db.open_table("edges").execute().await {
            Ok(tbl) => {
                let mut attempts: u64 = 0;
                loop {
                    match tbl.add(batch.clone()).execute().await {
                        Ok(_) => break,
                        Err(e) => {
                            let err = anyhow::anyhow!("{}", e);
                            if is_conflict_error(&err) && attempts < 3 {
                                attempts += 1;
                                tracing::warn!(
                                    "LanceDB conflict on edges append (attempt {}), retrying in {}ms",
                                    attempts,
                                    100 * attempts
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
                db.create_table("edges", batch)
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
        nodes.len(),
        edges.len(),
        new_version
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
pub(crate) async fn compact_stale_versions(
    db_path: &Path,
    committed_version: u64,
) -> anyhow::Result<()> {
    // Keep committed_version and committed_version - 1 (one buffer).
    // Delete everything older.
    if committed_version < 2 {
        // Nothing to compact on first or second write.
        return Ok(());
    }
    let cutoff = committed_version - 1; // delete scan_version < cutoff
    let predicate = col("scan_version").lt(lit(cutoff));

    let db = lancedb::connect(db_path.to_str().unwrap_or_default())
        .execute()
        .await
        .context("compact_stale_versions: failed to connect to LanceDB")?;

    let mut deleted_symbols = 0u64;
    let mut deleted_edges = 0u64;

    for (table_name, deleted_count) in [
        ("symbols", &mut deleted_symbols),
        ("edges", &mut deleted_edges),
    ] {
        if let Ok(tbl) = db.open_table(table_name).execute().await {
            // Count rows before deletion for logging.
            match tbl.delete(&predicate).await {
                Ok(_) => {
                    *deleted_count = 1; // deletion succeeded (LanceDB doesn't return count)
                    tracing::debug!(
                        "compact_stale_versions: deleted stale rows from {} (scan_version < {})",
                        table_name,
                        cutoff
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "compact_stale_versions: delete from {} failed: {}",
                        table_name,
                        e
                    );
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
/// - `deleted_files`: `(root_id, file_path)` pairs whose symbols should be deleted from LanceDB
pub(crate) async fn persist_graph_incremental(
    repo_root: &Path,
    upsert_nodes: &[Node],
    upsert_edges: &[Edge],
    deleted_edge_ids: &[String],
    deleted_files: &[(String, PathBuf)],
) -> anyhow::Result<bool> {
    #[cfg(test)]
    let retry_limit = std::env::var("RNA_TEST_LANCE_RETRY_LIMIT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3);
    #[cfg(not(test))]
    let retry_limit = 3;

    persist_graph_incremental_with_retry_limit(
        repo_root,
        upsert_nodes,
        upsert_edges,
        deleted_edge_ids,
        deleted_files,
        retry_limit,
    )
    .await
}

async fn persist_graph_incremental_with_retry_limit(
    repo_root: &Path,
    upsert_nodes: &[Node],
    upsert_edges: &[Edge],
    deleted_edge_ids: &[String],
    deleted_files: &[(String, PathBuf)],
    retry_limit: u64,
) -> anyhow::Result<bool> {
    let db_path = graph_lance_path(repo_root);
    std::fs::create_dir_all(&db_path)?;

    // Pre-flight: ensure schema version matches before any LanceDB writes.
    if check_and_migrate_schema(&db_path).await? {
        tracing::info!(
            "Schema migrated to v{} during incremental update -- cache rebuilt; caller should do a full persist",
            SCHEMA_VERSION
        );
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
            && let Ok(tbl) = db.open_table("symbols").execute().await
        {
            for chunk in deleted_files.chunks(PREDICATE_BATCH_SIZE) {
                let predicate = chunk
                    .iter()
                    .map(|(root, path)| {
                        col("root_id")
                            .eq(lit(root.clone()))
                            .and(col("file_path").eq(lit(path.display().to_string())))
                    })
                    .reduce(DfExpr::or)
                    .expect("non-empty deleted-files chunk");
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!("Failed to delete symbols for removed files: {}", e);
                }
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
                        let batches =
                            RecordBatchIterator::new(vec![Ok(batch.clone())], schema.clone());
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
                                } else if is_conflict_error(&err) && attempts < retry_limit {
                                    attempts += 1;
                                    tracing::warn!(
                                        "LanceDB conflict on symbols merge_insert (attempt {}), retrying in {}ms",
                                        attempts,
                                        100 * attempts
                                    );
                                    tokio::time::sleep(Duration::from_millis(100 * attempts)).await;
                                } else {
                                    return Err(err).context(
                                        "Failed to merge_insert symbols table after retries",
                                    );
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Table doesn't exist yet -- create it (first incremental run after a fresh repo)
                    db.create_table("symbols", batch)
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
            && let Ok(tbl) = db.open_table("edges").execute().await
        {
            for chunk in deleted_edge_ids.chunks(PREDICATE_BATCH_SIZE) {
                let predicate = string_isin("id", chunk.iter().cloned());
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!("Failed to delete edges for removed files: {}", e);
                }
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
                        let batches =
                            RecordBatchIterator::new(vec![Ok(batch.clone())], schema.clone());
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
                                } else if is_conflict_error(&err) && attempts < retry_limit {
                                    attempts += 1;
                                    tracing::warn!(
                                        "LanceDB conflict on edges merge_insert (attempt {}), retrying in {}ms",
                                        attempts,
                                        100 * attempts
                                    );
                                    tokio::time::sleep(Duration::from_millis(100 * attempts)).await;
                                } else {
                                    return Err(err).context(
                                        "Failed to merge_insert edges table after retries",
                                    );
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Table doesn't exist yet -- create it
                    db.create_table("edges", batch)
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
    use arrow_array::Array;
    use arrow_array::StringArray;
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
                && let Some(arr) = col.as_any().downcast_ref::<StringArray>()
            {
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
pub(crate) async fn delete_nodes_for_roots(
    repo_root: &Path,
    slugs: &[String],
) -> anyhow::Result<()> {
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

    // Delete from all tables that carry a root_id column.
    for table_name in ["symbols", "edges", "file_index", "pr_merges"] {
        if let Ok(tbl) = db.open_table(table_name).execute().await {
            for chunk in slugs.chunks(PREDICATE_BATCH_SIZE) {
                let predicate = string_isin("root_id", chunk.iter().cloned());
                if let Err(e) = tbl.delete(&predicate).await {
                    tracing::warn!(
                        "Failed to delete {} for removed worktrees: {}",
                        table_name,
                        e
                    );
                }
            }
        }
    }

    tracing::info!("Pruned LanceDB rows for stale roots: {}", slugs.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use super::super::graph_lance_path;
    use super::super::load::load_graph_from_lance;
    use super::super::migrate::{
        drop_all_lance_tables, read_committed_scan_version, scan_version_path,
    };
    use super::*;
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

    /// Child-process entry point for the adversarial writer test below. Keeping
    /// this ignored prevents normal test runs from writing without a parent-
    /// supplied shared repository and writer identity.
    #[test]
    #[ignore]
    fn cross_process_incremental_writer_helper() {
        let repo_root = PathBuf::from(
            std::env::var("RNA_TEST_LANCE_REPO").expect("parent supplies shared repo"),
        );
        let writer = std::env::var("RNA_TEST_LANCE_WRITER").expect("parent supplies writer id");
        let writes = std::env::var("RNA_TEST_LANCE_WRITES")
            .expect("parent supplies write count")
            .parse::<usize>()
            .expect("write count is numeric");
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async {
            for index in 0..writes {
                persist_graph_incremental(
                    &repo_root,
                    &[make_test_node(&format!("writer_{writer}_{index}"))],
                    &[],
                    &[],
                    &[],
                )
                .await
                .unwrap_or_else(|error| panic!("writer {writer} failed at {index}: {error:#}"));
            }
        });
    }

    fn writer_command(
        test_binary: &std::path::Path,
        repo_root: &Path,
        writer: &str,
        writes: usize,
        retry_limit: u64,
    ) -> Command {
        let mut command = Command::new(test_binary);
        command
            .arg("--exact")
            .arg("server::store::persist::tests::cross_process_incremental_writer_helper")
            .arg("--ignored")
            .arg("--nocapture")
            .env("RNA_TEST_LANCE_REPO", repo_root)
            .env("RNA_TEST_LANCE_WRITER", writer)
            .env("RNA_TEST_LANCE_WRITES", writes.to_string())
            .env("RNA_TEST_LANCE_RETRY_LIMIT", retry_limit.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Reproduces the historical failure boundary with genuine OS processes,
    /// not Tokio tasks sharing an in-process mutex. The matrix varies process
    /// serialization and merge-conflict retries independently and verifies the
    /// final store rather than accepting successful exit codes as correctness.
    #[test]
    fn cross_process_write_matrix_preserves_all_rows() {
        const WRITES_PER_PROCESS: usize = 12;
        let test_binary = std::env::current_exe().expect("current test binary");

        for (label, concurrent, retry_limit) in [
            ("serialized_with_retries", false, 3),
            ("serialized_without_retries", false, 0),
            ("concurrent_with_retries", true, 3),
            ("concurrent_without_retries", true, 0),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo_root = dir.path();
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            runtime
                .block_on(persist_graph_to_lance(
                    repo_root,
                    &[make_test_node("baseline")],
                    &[],
                ))
                .expect("baseline persist");

            let started = Instant::now();
            let outputs = if concurrent {
                let first = writer_command(
                    &test_binary,
                    repo_root,
                    &format!("{label}_a"),
                    WRITES_PER_PROCESS,
                    retry_limit,
                )
                .spawn()
                .expect("spawn first writer");
                let second = writer_command(
                    &test_binary,
                    repo_root,
                    &format!("{label}_b"),
                    WRITES_PER_PROCESS,
                    retry_limit,
                )
                .spawn()
                .expect("spawn second writer");
                vec![
                    first.wait_with_output().expect("wait for first writer"),
                    second.wait_with_output().expect("wait for second writer"),
                ]
            } else {
                vec![
                    writer_command(
                        &test_binary,
                        repo_root,
                        &format!("{label}_a"),
                        WRITES_PER_PROCESS,
                        retry_limit,
                    )
                    .output()
                    .expect("run first writer"),
                    writer_command(
                        &test_binary,
                        repo_root,
                        &format!("{label}_b"),
                        WRITES_PER_PROCESS,
                        retry_limit,
                    )
                    .output()
                    .expect("run second writer"),
                ]
            };
            let elapsed = started.elapsed();

            let all_writers_succeeded = outputs.iter().all(|output| output.status.success());
            if !all_writers_succeeded {
                assert_eq!(
                    retry_limit,
                    0,
                    "{label}: a protected writer failed: {}",
                    outputs
                        .iter()
                        .enumerate()
                        .filter(|(_, output)| !output.status.success())
                        .map(|(index, output)| format!(
                            "writer {index}\nstdout:\n{}\nstderr:\n{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr),
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                assert!(
                    concurrent,
                    "{label}: serialized writers must not need conflict retries"
                );
                eprintln!(
                    "LanceDB write matrix: scenario={label} observed an unprotected concurrent-writer failure after {}ms",
                    elapsed.as_millis()
                );
                continue;
            }

            let state = runtime
                .block_on(load_graph_from_lance(repo_root))
                .expect("load final graph");
            let expected = 1 + 2 * WRITES_PER_PROCESS;
            assert_eq!(
                state.nodes.len(),
                expected,
                "{label}: every stable ID must remain unique and visible"
            );
            let unique_ids: std::collections::HashSet<_> =
                state.nodes.iter().map(Node::stable_id).collect();
            assert_eq!(unique_ids.len(), expected, "{label}: duplicate stable IDs");
            eprintln!(
                "LanceDB write matrix: scenario={label} processes=2 writes={} retry_limit={retry_limit} elapsed_ms={} final_rows={}",
                2 * WRITES_PER_PROCESS,
                elapsed.as_millis(),
                state.nodes.len(),
            );
        }
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
        assert!(
            result.is_ok(),
            "persist after drop should succeed, got: {:?}",
            result
        );
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
        assert_eq!(
            read_committed_scan_version(&db_path),
            1,
            "first persist should write version 1"
        );

        let node_b = make_test_node("fn_b");
        persist_graph_to_lance(repo_root, &[node_b], &[])
            .await
            .expect("second persist failed");
        assert_eq!(
            read_committed_scan_version(&db_path),
            2,
            "second persist should write version 2"
        );
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

        let state = load_graph_from_lance(repo_root).await.expect("load failed");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_v2"), "fn_v2 should be present");
        assert!(
            !names.contains(&"fn_v1"),
            "fn_v1 (old version) should not be present"
        );
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
        assert!(
            names.contains(&"fn_v3"),
            "fn_v3 should be present (current version)"
        );
        assert!(
            !names.contains(&"fn_v1"),
            "fn_v1 should have been compacted"
        );
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

        persist_graph_incremental(
            repo_root,
            &[make_test_node("fn_incremental")],
            &[],
            &[],
            &[],
        )
        .await
        .expect("incremental write");
        assert_eq!(
            read_committed_scan_version(&db_path),
            1,
            "incremental must not change version pointer"
        );

        let state = load_graph_from_lance(repo_root)
            .await
            .expect("load after incremental");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"fn_base"), "fn_base should be present");
        assert!(
            names.contains(&"fn_incremental"),
            "fn_incremental should be present"
        );

        persist_graph_to_lance(repo_root, &[make_test_node("fn_v2")], &[])
            .await
            .expect("second full rebuild");
        assert_eq!(read_committed_scan_version(&db_path), 2);

        let state2 = load_graph_from_lance(repo_root)
            .await
            .expect("load after second rebuild");
        let names2: Vec<&str> = state2.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names2.contains(&"fn_v2"), "fn_v2 should be present");
        assert!(
            !names2.contains(&"fn_incremental"),
            "fn_incremental (version 1) should not appear after version 2 rebuild"
        );
    }

    /// LanceDB 0.31 does not select MemWAL automatically. An explicit primary
    /// key and LSM write spec route this upsert to MemWAL (reported as version
    /// zero), but the ordinary Table query API used by RNA does not yet merge
    /// MemWAL generations into reads. Production therefore keeps the standard
    /// merge path until the read side can deliver those rows.
    #[tokio::test]
    async fn test_lancedb_031_memwal_path_requires_explicit_spec() {
        use lancedb::table::LsmWriteSpec;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        persist_graph_to_lance(repo_root, &[make_test_node("base")], &[])
            .await
            .expect("create symbols table");

        let db_path = graph_lance_path(repo_root);
        let db = lancedb::connect(db_path.to_str().expect("utf-8 path"))
            .execute()
            .await
            .expect("connect");
        let table = db
            .open_table("symbols")
            .execute()
            .await
            .expect("open symbols");
        table
            .set_unenforced_primary_key(["id"])
            .await
            .expect("set primary key");
        table
            .set_lsm_write_spec(LsmWriteSpec::unsharded())
            .await
            .expect("set explicit LSM spec");

        let batch =
            build_symbols_batch(&[make_test_node("memwal_only")], 1).expect("build symbols batch");
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        let mut merge = table.merge_insert(&["id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        let result = merge.execute(Box::new(batches)).await.expect("LSM merge");
        assert_eq!(result.num_rows, 1);
        assert_eq!(result.version, 0, "version zero identifies the MemWAL path");

        let state = load_graph_from_lance(repo_root)
            .await
            .expect("ordinary RNA read");
        assert!(
            state.nodes.iter().all(|node| node.id.name != "memwal_only"),
            "ordinary Table queries do not yet deliver MemWAL-only rows"
        );
        table
            .close_lsm_writers()
            .await
            .expect("close MemWAL writer");
    }

    #[tokio::test]
    async fn test_typed_predicates_handle_quotes_in_ids_roots_and_paths() {
        use crate::graph::{Confidence, Edge, EdgeKind};

        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let mut quoted = make_test_node("fn_'quoted");
        quoted.id.root = "root_'quoted".to_string();
        quoted.id.file = PathBuf::from("src/path_'quoted.rs");
        let target = make_test_node("target");
        let edge = Edge {
            from: quoted.id.clone(),
            to: target.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
        };
        let edge_id = edge.stable_id();

        persist_graph_to_lance(repo_root, &[quoted.clone(), target], &[edge])
            .await
            .expect("full persist with quoted identifiers");
        persist_graph_incremental(
            repo_root,
            &[],
            &[],
            &[edge_id],
            &[(quoted.id.root.clone(), quoted.id.file.clone())],
        )
        .await
        .expect("typed incremental deletes");

        let state = load_graph_from_lance(repo_root)
            .await
            .expect("load after typed deletes");
        assert!(
            state.nodes.iter().all(|node| node.id != quoted.id),
            "quoted root/path node should be deleted exactly"
        );
        assert!(state.edges.is_empty(), "quoted edge ID should be deleted");

        persist_graph_to_lance(repo_root, &[quoted.clone()], &[])
            .await
            .expect("re-persist quoted root");
        delete_nodes_for_roots(repo_root, &[quoted.id.root.clone()])
            .await
            .expect("typed root delete");
        let state = load_graph_from_lance(repo_root)
            .await
            .expect("load after root delete");
        assert!(
            state
                .nodes
                .iter()
                .all(|node| node.id.root != quoted.id.root),
            "quoted root should be deleted exactly"
        );
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
        assert_eq!(
            read_committed_scan_version(&db_path),
            0,
            "missing file should read as 0"
        );

        let state = load_graph_from_lance(repo_root)
            .await
            .expect("load without version file");
        let names: Vec<&str> = state.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(
            names.contains(&"fn_any"),
            "fn_any should be visible with no version filter"
        );
    }

    /// Regression: PR #644 added `attr_refs` metadata on functions and
    /// `ReferencedBy` edges from import_calls_pass step 6. Both must survive
    /// `persist_graph_to_lance` -> `load_graph_from_lance` so incremental
    /// scans can keep emitting these edges from the cached graph.
    ///
    /// See `.oh/guardrails/computed-but-not-delivered.md`: every new metadata
    /// field must wire through extraction, the Arrow schema, and the read
    /// path. Skipping any layer drops the value silently on the next load.
    #[tokio::test]
    async fn test_attr_refs_and_referenced_by_round_trip() {
        use crate::graph::{Confidence, Edge, EdgeKind};

        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        let mut caller_meta = BTreeMap::new();
        caller_meta.insert(
            super::super::metadata_keys::ATTR_REFS.to_string(),
            "persist,load,render".to_string(),
        );
        let caller = Node {
            id: NodeId {
                root: "local".to_string(),
                file: PathBuf::from("src/caller.rs"),
                name: "caller_fn".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            signature: "fn caller_fn()".to_string(),
            line_start: 1,
            line_end: 5,
            body: "fn caller_fn() { obj.persist(); }".to_string(),
            metadata: caller_meta,
            source: ExtractionSource::TreeSitter,
        };
        let callee = make_test_node("persist");

        let edge = Edge {
            from: caller.id.clone(),
            to: callee.id.clone(),
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
        };

        persist_graph_to_lance(
            repo_root,
            &[caller.clone(), callee.clone()],
            &[edge.clone()],
        )
        .await
        .expect("persist failed");

        let state = load_graph_from_lance(repo_root).await.expect("load failed");

        let reloaded_caller = state
            .nodes
            .iter()
            .find(|n| n.id.name == "caller_fn")
            .expect("caller node missing after reload");
        assert_eq!(
            reloaded_caller
                .metadata
                .get(super::super::metadata_keys::ATTR_REFS)
                .map(String::as_str),
            Some("persist,load,render"),
            "attr_refs metadata dropped on round-trip; import_calls_pass step 6 \
             would silently stop emitting edges on cached loads",
        );

        let referenced_by_count = state
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::ReferencedBy)
            .count();
        assert_eq!(
            referenced_by_count,
            1,
            "ReferencedBy edge dropped on round-trip; got edges {:?}",
            state
                .edges
                .iter()
                .map(|e| e.kind.to_string())
                .collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn test_custom_edge_kind_round_trips_as_graph_edge_not_metadata() {
        use crate::graph::{Confidence, Edge, EdgeKind};

        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();

        let mut claimant = make_test_node("claimant");
        claimant.id.file = PathBuf::from("docs/claim.md");
        let mut evidence = make_test_node("evidence");
        evidence.id.file = PathBuf::from("docs/evidence.md");

        claimant
            .metadata
            .insert("relationship".to_string(), "supports".to_string());
        let supports_kind = EdgeKind::Other("supports".to_string());
        let wrong_workaround = Edge {
            from: claimant.id.clone(),
            to: evidence.id.clone(),
            kind: EdgeKind::References,
            source: ExtractionSource::Markdown,
            confidence: Confidence::Detected,
        };
        let custom_edge = Edge {
            from: claimant.id.clone(),
            to: evidence.id.clone(),
            kind: supports_kind.clone(),
            source: ExtractionSource::Markdown,
            confidence: Confidence::Detected,
        };

        persist_graph_to_lance(
            repo_root,
            &[claimant.clone(), evidence.clone()],
            &[custom_edge.clone(), wrong_workaround],
        )
        .await
        .expect("persist failed");

        let state = load_graph_from_lance(repo_root).await.expect("load failed");

        let supports_edges: Vec<_> = state
            .edges
            .iter()
            .filter(|e| e.kind == supports_kind)
            .collect();
        assert_eq!(
            supports_edges.len(),
            1,
            "custom relationship must reload as an actual supports edge; got {:?}",
            state
                .edges
                .iter()
                .map(|e| e.kind.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(supports_edges[0].from.name, "claimant");
        assert_eq!(supports_edges[0].to.name, "evidence");

        let unfiltered = state.index.neighbors_grouped(
            &claimant.stable_id(),
            None,
            petgraph::Direction::Outgoing,
        );
        assert!(
            unfiltered.contains_key(&EdgeKind::References),
            "the same-source generic-edge workaround must be present for this regression to be adversarial"
        );

        let supports_filter = [supports_kind.clone()];
        let groups = state.index.neighbors_grouped(
            &claimant.stable_id(),
            Some(&supports_filter),
            petgraph::Direction::Outgoing,
        );
        let supports_group = groups
            .get(&supports_kind)
            .expect("custom supports edge missing from grouped traversal");
        assert!(
            supports_group.contains(&evidence.stable_id()),
            "custom supports traversal should reach evidence; groups={groups:?}"
        );
        assert!(
            !groups.contains_key(&EdgeKind::References),
            "metadata-only workaround must not satisfy supports traversal"
        );
    }
}
