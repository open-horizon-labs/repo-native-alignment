//! Write-ahead log sentinels for scan pipeline phases.
//!
//! After each major pipeline phase completes and its data is durably persisted,
//! a sentinel file is written to `.oh/.cache/`. On startup, the server checks
//! sentinels instead of heuristics (like `has_call_edges`) to determine what
//! needs to re-run.
//!
//! If a phase runs but the subsequent persist fails, the sentinel is NOT written,
//! so the next startup correctly re-runs that phase.
//!
//! ## Sentinel paths (relative to repo_root)
//! - `.oh/.cache/extract_completed.json` — tree-sitter extraction + initial persist done
//! - `.oh/.cache/lsp_completed.json`     — LSP enrichment + persist done
//!
//! ## Schema invalidation
//! Sentinels embed `schema_version`. If it changes (new binary deployed),
//! the sentinel is treated as absent and the phase re-runs.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::graph::store::SCHEMA_VERSION;

/// Data stored in each sentinel file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelData {
    /// Unix timestamp (seconds) when the phase completed.
    pub timestamp: u64,
    /// LanceDB schema version at time of write.
    pub schema_version: u32,
    /// Node count after the phase completed.
    pub node_count: usize,
    /// Edge count after the phase completed.
    pub edge_count: usize,
}

impl SentinelData {
    fn new(node_count: usize, edge_count: usize) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            timestamp,
            schema_version: SCHEMA_VERSION,
            node_count,
            edge_count,
        }
    }

    /// Returns true if this sentinel is valid for the current binary version.
    pub fn is_current(&self) -> bool {
        self.schema_version == SCHEMA_VERSION
    }
}

fn extract_sentinel_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".oh")
        .join(".cache")
        .join("extract_completed.json")
}

fn lsp_sentinel_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".oh")
        .join(".cache")
        .join("lsp_completed.json")
}

/// Write the extraction sentinel after tree-sitter extraction + initial persist succeed.
///
/// Call this only after `persist_graph_to_lance` returns `Ok(())`.
pub fn write_extract_sentinel(repo_root: &Path, node_count: usize, edge_count: usize) {
    let path = extract_sentinel_path(repo_root);
    let data = SentinelData::new(node_count, edge_count);
    write_sentinel(&path, &data, "extract");
}

/// Write the LSP sentinel after LSP enrichment + persist succeed.
///
/// Call this only after `persist_graph_to_lance` returns `Ok(())` following LSP enrichment.
pub fn write_lsp_sentinel(repo_root: &Path, node_count: usize, edge_count: usize) {
    let path = lsp_sentinel_path(repo_root);
    let data = SentinelData::new(node_count, edge_count);
    write_sentinel(&path, &data, "lsp");
}

/// Read the extraction sentinel. Returns `None` if absent, stale, or corrupt.
pub fn read_extract_sentinel(repo_root: &Path) -> Option<SentinelData> {
    read_sentinel(&extract_sentinel_path(repo_root), "extract")
}

/// Read the LSP sentinel. Returns `None` if absent, stale, or corrupt.
///
/// `None` means LSP enrichment has not completed for this schema version
/// and should be re-run. `Some(_)` means LSP completed and its results are in LanceDB.
pub fn read_lsp_sentinel(repo_root: &Path) -> Option<SentinelData> {
    read_sentinel(&lsp_sentinel_path(repo_root), "lsp")
}

/// Delete both sentinels. Called when a full rebuild is triggered: schema migration,
/// explicit `--full` with cache invalidation, or when cached graph enrichment output
/// is stale/missing (via `cache_needs_enrichment` paths in `enrichment.rs` and `graph.rs`).
pub fn clear_sentinels(repo_root: &Path) {
    clear_sentinel(&extract_sentinel_path(repo_root), "extract");
    clear_sentinel(&lsp_sentinel_path(repo_root), "lsp");
}

/// Delete only the LSP sentinel. Called before an extraction-only persist so the
/// old LSP sentinel (describing the previous graph) cannot be trusted by the next
/// startup when LanceDB now holds a fresh tree-sitter-only snapshot.
pub fn clear_lsp_sentinel(repo_root: &Path) {
    clear_sentinel(&lsp_sentinel_path(repo_root), "lsp");
}

fn clear_sentinel(path: &Path, name: &str) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!("Failed to clear {} sentinel: {}", name, e);
        } else {
            tracing::debug!("Cleared {} sentinel", name);
        }
    }
}

fn write_sentinel(path: &Path, data: &SentinelData, name: &str) {
    // Ensure parent directory exists.
    let parent = match path.parent() {
        Some(p) => p,
        None => {
            tracing::warn!("Sentinel path {} has no parent directory", path.display());
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(parent) {
        tracing::warn!("Failed to create sentinel directory for {}: {}", name, e);
        return;
    }

    // Use a temp file + atomic rename so readers never observe a partial write.
    // If the process crashes between write and rename, the previous sentinel survives.
    let tmp_path = path.with_extension("tmp");
    match serde_json::to_string_pretty(data) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&tmp_path, &json) {
                tracing::warn!("Failed to write {} sentinel temp file: {}", name, e);
                return;
            }
            if let Err(e) = std::fs::rename(&tmp_path, path) {
                tracing::warn!("Failed to rename {} sentinel into place: {}", name, e);
                // Best effort: remove the temp file so it doesn't clutter the cache.
                let _ = std::fs::remove_file(&tmp_path);
            } else {
                tracing::debug!(
                    "Wrote {} sentinel: {} nodes, {} edges",
                    name,
                    data.node_count,
                    data.edge_count
                );
            }
        }
        Err(e) => tracing::warn!("Failed to serialize {} sentinel: {}", name, e),
    }
}

fn read_sentinel(path: &Path, name: &str) -> Option<SentinelData> {
    let content = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<SentinelData>(&content) {
        Ok(data) => {
            if data.is_current() {
                tracing::debug!(
                    "Found valid {} sentinel: {} nodes, {} edges (ts={})",
                    name,
                    data.node_count,
                    data.edge_count,
                    data.timestamp
                );
                Some(data)
            } else {
                tracing::debug!(
                    "{} sentinel is stale (schema {} vs current {})",
                    name,
                    data.schema_version,
                    SCHEMA_VERSION,
                );
                None
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to parse {} sentinel (treating as absent): {}",
                name,
                e
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_repo(tmp: &TempDir) {
        std::fs::create_dir_all(tmp.path().join(".oh").join(".cache")).unwrap();
    }

    #[test]
    fn test_write_and_read_extract_sentinel() {
        let tmp = TempDir::new().unwrap();
        setup_repo(&tmp);

        assert!(read_extract_sentinel(tmp.path()).is_none());

        write_extract_sentinel(tmp.path(), 100, 200);

        let sentinel = read_extract_sentinel(tmp.path()).expect("sentinel should exist");
        assert_eq!(sentinel.node_count, 100);
        assert_eq!(sentinel.edge_count, 200);
        assert_eq!(sentinel.schema_version, SCHEMA_VERSION);
        assert!(sentinel.is_current());
    }

    #[test]
    fn test_write_and_read_lsp_sentinel() {
        let tmp = TempDir::new().unwrap();
        setup_repo(&tmp);

        assert!(read_lsp_sentinel(tmp.path()).is_none());

        write_lsp_sentinel(tmp.path(), 300, 500);

        let sentinel = read_lsp_sentinel(tmp.path()).expect("sentinel should exist");
        assert_eq!(sentinel.node_count, 300);
        assert_eq!(sentinel.edge_count, 500);
        assert!(sentinel.is_current());
    }

    #[test]
    fn test_stale_sentinel_returns_none() {
        let tmp = TempDir::new().unwrap();
        setup_repo(&tmp);

        // Write a sentinel with a wrong schema_version.
        let stale = SentinelData {
            timestamp: 0,
            schema_version: SCHEMA_VERSION.wrapping_sub(1),
            node_count: 50,
            edge_count: 80,
        };
        let path = tmp
            .path()
            .join(".oh")
            .join(".cache")
            .join("lsp_completed.json");
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

        assert!(
            read_lsp_sentinel(tmp.path()).is_none(),
            "stale sentinel should return None"
        );
    }

    #[test]
    fn test_clear_sentinels() {
        let tmp = TempDir::new().unwrap();
        setup_repo(&tmp);

        write_extract_sentinel(tmp.path(), 10, 20);
        write_lsp_sentinel(tmp.path(), 10, 20);

        assert!(read_extract_sentinel(tmp.path()).is_some());
        assert!(read_lsp_sentinel(tmp.path()).is_some());

        clear_sentinels(tmp.path());

        assert!(read_extract_sentinel(tmp.path()).is_none());
        assert!(read_lsp_sentinel(tmp.path()).is_none());
    }

    #[test]
    fn test_corrupt_sentinel_returns_none() {
        let tmp = TempDir::new().unwrap();
        setup_repo(&tmp);

        let path = tmp
            .path()
            .join(".oh")
            .join(".cache")
            .join("extract_completed.json");
        std::fs::write(&path, b"not valid json!!!").unwrap();

        assert!(read_extract_sentinel(tmp.path()).is_none());
    }
}
