//! Schema migration and version management for LanceDB.

use std::path::Path;

use anyhow::Context;

use crate::graph::store::SCHEMA_VERSION;

/// Path to the committed scan_version pointer file.
///
/// This file contains the `u64` scan version that is currently live for reads.
/// Written atomically after a successful full-rebuild append. Until this file is
/// updated, the previous version remains the source of truth for queries.
pub(crate) fn scan_version_path(db_path: &Path) -> std::path::PathBuf {
    db_path.join("scan_version")
}

/// Read the committed scan version from the pointer file.
///
/// Returns `0` if the file is absent or unparseable (first-run default that matches
/// rows written by the very first persist, which also writes version `1`).
pub(crate) fn read_committed_scan_version(db_path: &Path) -> u64 {
    std::fs::read_to_string(scan_version_path(db_path))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Atomically write the committed scan_version pointer.
pub(crate) fn write_committed_scan_version(db_path: &Path, version: u64) -> anyhow::Result<()> {
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

    // Version mismatch (or missing file) -- nuke the LanceDB directory contents
    // and rewrite the version file.
    //
    // Stale data exists if we had a version file (normal case) OR if the lance
    // directory contains data files (legacy state without version file).
    let had_stale_data = stored_version.is_some() || has_lance_data(db_path);

    if had_stale_data {
        tracing::info!(
            "Schema version mismatch (stored={:?}, current={}) -- dropping all LanceDB data",
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
            // Preserve version-tracking files: schema_version is rewritten below.
            // scan_version is deleted (reset to 0 on next read) since all rows are gone.
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name == "schema_version" {
                continue;
            }
            if path.is_dir() {
                std::fs::remove_dir_all(&path).with_context(|| {
                    format!(
                        "check_and_migrate_schema: failed to remove directory {}",
                        path.display()
                    )
                })?;
            } else {
                std::fs::remove_file(&path).with_context(|| {
                    format!(
                        "check_and_migrate_schema: failed to remove file {}",
                        path.display()
                    )
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
        for table_name in &[
            "symbols",
            "edges",
            "pr_merges",
            "file_index",
            "_schema_meta",
        ] {
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
            entries.flatten().any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                // LanceDB stores tables as directories; skip our version file
                name != "schema_version" && e.path().is_dir()
            })
        })
        .unwrap_or(false)
}

/// Drop all LanceDB table directories inside `db_path` and reset the schema_version file.
///
/// Called when a schema mismatch is detected at runtime (Arrow rejection during merge_insert).
/// After dropping tables the caller returns `Ok(true)` so the graph layer triggers a full
/// `persist_graph_to_lance` rebuild.  Errors here are non-fatal -- worst case the next scan
/// retries and eventually succeeds.
pub(super) fn drop_all_lance_tables(db_path: &Path) {
    if let Ok(entries) = std::fs::read_dir(db_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Preserve schema_version — it is rewritten below.
            if path
                .file_name()
                .map(|n| n == "schema_version")
                .unwrap_or(false)
            {
                continue;
            }
            if path.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!(
                        "drop_all_lance_tables: failed to remove {}: {}",
                        path.display(),
                        e
                    );
                }
            } else if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(
                    "drop_all_lance_tables: failed to remove {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
    // Reset the version file so check_and_migrate_schema re-initialises cleanly.
    let version_file = db_path.join("schema_version");
    if let Err(e) = std::fs::write(&version_file, SCHEMA_VERSION.to_string()) {
        tracing::warn!(
            "drop_all_lance_tables: failed to write schema_version: {}",
            e
        );
    }
}

/// Returns `true` if the error looks like a LanceDB concurrent-write conflict.
///
/// LanceDB uses optimistic concurrency and surfaces conflicts as errors whose
/// messages contain "conflict" or "concurrent" in their description.  We match on
/// the string representation because the upstream error types are not exposed as a
/// public enum.
///
/// Note: "commit" is intentionally excluded -- it appears in many non-conflict contexts
/// (git history, schema version strings, log messages) and would cause false positives.
pub(super) fn is_conflict_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("conflict") || msg.contains("concurrent")
}

/// Returns `true` if the error looks like an Arrow/LanceDB schema mismatch.
///
/// This happens when the on-disk LanceDB table was written with a different schema
/// than what the code expects -- e.g. after an RNA upgrade where the LanceDB cache
/// was not cleared.  The pre-flight `check_and_migrate_schema` guard uses a version
/// *file* to detect mismatches, but if that file is missing or out of sync the file
/// check passes while the actual table rejects the write with a schema error.
///
/// We detect this defensively by matching error message substrings from LanceDB
/// and Arrow, because the upstream error types are not part of a stable public enum.
pub(super) fn is_schema_mismatch_error(e: &anyhow::Error) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_conflict_error_detection() {
        let conflict = anyhow::anyhow!("write conflict detected");
        assert!(is_conflict_error(&conflict));

        let concurrent = anyhow::anyhow!("concurrent modification");
        assert!(is_conflict_error(&concurrent));

        let both = anyhow::anyhow!("concurrent write conflict");
        assert!(is_conflict_error(&both));

        let commit_only = anyhow::anyhow!("failed to commit transaction");
        assert!(!is_conflict_error(&commit_only));

        let unrelated = anyhow::anyhow!("table not found");
        assert!(!is_conflict_error(&unrelated));

        let io_err = anyhow::anyhow!("IO error: permission denied");
        assert!(!is_conflict_error(&io_err));
    }

    #[test]
    fn test_is_schema_mismatch_error_detection() {
        let schema_err = anyhow::anyhow!("Arrow schema mismatch: expected 10 fields, got 8");
        assert!(is_schema_mismatch_error(&schema_err));

        let column_err = anyhow::anyhow!("column not found: diagnostic_severity");
        assert!(is_schema_mismatch_error(&column_err));

        let field_err = anyhow::anyhow!("field not found: http_method");
        assert!(is_schema_mismatch_error(&field_err));

        let type_err = anyhow::anyhow!("type mismatch: expected Int32, got Utf8");
        assert!(is_schema_mismatch_error(&type_err));

        let batch_err =
            anyhow::anyhow!("Invalid RecordBatch: number of columns does not match schema");
        assert!(is_schema_mismatch_error(&batch_err));

        let conflict = anyhow::anyhow!("write conflict detected");
        assert!(!is_schema_mismatch_error(&conflict));

        let io_err = anyhow::anyhow!("IO error: permission denied");
        assert!(!is_schema_mismatch_error(&io_err));

        let not_found = anyhow::anyhow!("table not found");
        assert!(!is_schema_mismatch_error(&not_found));
    }

    #[test]
    fn test_drop_all_lance_tables_clears_dirs_and_resets_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();

        std::fs::create_dir_all(db_path.join("symbols.lance")).unwrap();
        std::fs::create_dir_all(db_path.join("edges.lance")).unwrap();
        std::fs::write(db_path.join("schema_version"), "9").unwrap();

        drop_all_lance_tables(&db_path);

        assert!(
            !db_path.join("symbols.lance").exists(),
            "symbols.lance should be removed"
        );
        assert!(
            !db_path.join("edges.lance").exists(),
            "edges.lance should be removed"
        );

        let written = std::fs::read_to_string(db_path.join("schema_version")).unwrap();
        assert_eq!(written, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn test_read_committed_scan_version_missing_returns_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();
        assert_eq!(read_committed_scan_version(&db_path), 0);
    }

    #[test]
    fn test_scan_version_write_read_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();

        write_committed_scan_version(&db_path, 42).expect("write failed");
        assert_eq!(read_committed_scan_version(&db_path), 42);

        write_committed_scan_version(&db_path, 100).expect("write failed");
        assert_eq!(read_committed_scan_version(&db_path), 100);
    }

    #[test]
    fn test_drop_all_lance_tables_removes_scan_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("lance");
        std::fs::create_dir_all(&db_path).unwrap();

        std::fs::write(db_path.join("schema_version"), "16").unwrap();
        // extraction_version is a legacy file — no longer written or preserved (#620).
        // Seed one to verify drop_all_lance_tables cleans it up.
        std::fs::write(db_path.join("extraction_version"), "5").unwrap();
        std::fs::write(db_path.join("scan_version"), "7").unwrap();
        std::fs::create_dir_all(db_path.join("symbols.lance")).unwrap();

        drop_all_lance_tables(&db_path);

        assert!(
            !db_path.join("scan_version").exists(),
            "scan_version should be removed by drop_all_lance_tables"
        );
        assert!(
            !db_path.join("extraction_version").exists(),
            "extraction_version is a legacy file and should be removed by drop_all_lance_tables"
        );
    }
}
