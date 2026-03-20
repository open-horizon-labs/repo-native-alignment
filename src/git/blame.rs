//! Git blame enricher: annotates graph nodes with churn and authorship data.
//!
//! For each node, finds commits touching its `[line_start..=line_end]` range
//! and records:
//! - `churn_count` — number of distinct commits in the node's line range
//! - `last_author` — author name of the most recent commit touching the range
//! - `last_commit_sha` — 7-char abbreviated SHA of that commit
//!
//! Performance: blame is O(commits × file_lines), so we cache the full
//! `BlameFile` per file path and map all nodes in that file in one pass.
//!
//! Failure modes: if the repository cannot be opened, blame fails for a file,
//! or a node has no line range, the enricher silently skips that node.
//! It never returns `Err` — it returns `Ok` with partial results so the
//! rest of the pipeline continues unaffected.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::extract::{EnrichmentResult, Enricher};
use crate::graph::{Node};
use crate::graph::index::GraphIndex;

/// Enricher that annotates nodes with git blame metadata.
///
/// Language-agnostic — runs on all nodes regardless of language.
/// Registered in `EnricherRegistry::with_builtins()`.
pub struct BlameEnricher;

impl BlameEnricher {
    pub fn new() -> Self {
        BlameEnricher
    }
}

impl Default for BlameEnricher {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-line blame entry extracted from `git2::BlameHunk`.
#[derive(Debug, Clone)]
struct LineBlame {
    /// 7-char SHA of the commit that last touched this line.
    sha7: String,
    /// Commit timestamp (Unix epoch), used to find the most recent commit.
    commit_time: i64,
    /// Author name from the final-commit hunk.
    author: String,
}

/// Walk a `git2::Blame` for one file and return a `Vec<LineBlame>` indexed
/// by 0-based line number (line N → `result[N]`).
///
/// Lines with no associated hunk (blank lines at EOF) are filled with a
/// sentinel entry so the index is always safe to subscript.
fn blame_to_line_vec(blame: &git2::Blame<'_>) -> Vec<Option<LineBlame>> {
    // Pre-allocate with a reasonable capacity; we'll grow if needed.
    let mut lines: Vec<Option<LineBlame>> = Vec::new();

    for hunk in blame.iter() {
        let start_line = hunk.final_start_line(); // 1-based
        let line_count = hunk.lines_in_hunk();

        let commit_id = hunk.final_commit_id();
        let sha7 = format!("{:.7}", commit_id);
        let commit_time = hunk.final_signature().when().seconds();
        let author = hunk
            .final_signature()
            .name()
            .unwrap_or("<unknown>")
            .to_string();

        let entry = LineBlame {
            sha7,
            commit_time,
            author,
        };

        // Ensure vec is large enough (convert to 0-based)
        let last_idx = start_line + line_count; // exclusive, 1-based → last 0-based slot +1
        if lines.len() < last_idx {
            lines.resize_with(last_idx, || None);
        }

        for line_idx in (start_line - 1)..(start_line - 1 + line_count) {
            lines[line_idx] = Some(entry.clone());
        }
    }

    lines
}

/// Given a slice of `LineBlame` entries covering `[line_start..=line_end]`
/// (1-based, inclusive), compute:
/// - distinct commit count (churn)
/// - the most recent commit's SHA and author
///
/// Returns `None` if the range is empty or all entries are missing.
fn summarize_range(
    line_data: &[Option<LineBlame>],
    line_start: usize,
    line_end: usize,
) -> Option<(usize, String, String)> {
    if line_start == 0 || line_end == 0 || line_start > line_end {
        return None;
    }

    // Convert 1-based inclusive to 0-based slice range
    let lo = line_start.saturating_sub(1);
    let hi = line_end.min(line_data.len()); // exclusive

    if lo >= hi {
        return None;
    }

    let mut shas: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut newest: Option<&LineBlame> = None;

    for entry in line_data[lo..hi].iter().flatten() {
        shas.insert(entry.sha7.as_str());
        match newest {
            None => newest = Some(entry),
            Some(prev) if entry.commit_time > prev.commit_time => newest = Some(entry),
            _ => {}
        }
    }

    if shas.is_empty() {
        return None;
    }

    let best = newest?;
    Some((shas.len(), best.sha7.clone(), best.author.clone()))
}

#[async_trait::async_trait]
impl Enricher for BlameEnricher {
    fn name(&self) -> &str {
        "git-blame"
    }

    /// Blame is language-agnostic — return the wildcard sentinel.
    ///
    /// `enrich_all` checks `runs_regardless_of_language()` and skips the
    /// language filter for this enricher.
    fn languages(&self) -> &[&str] {
        &[]
    }

    /// Always run — blame is not tied to any specific language.
    fn runs_regardless_of_language(&self) -> bool {
        true
    }

    fn is_ready(&self) -> bool {
        true
    }

    async fn enrich(
        &self,
        nodes: &[Node],
        _index: &GraphIndex,
        repo_root: &Path,
    ) -> Result<EnrichmentResult> {
        let repo = match git2::Repository::open(repo_root) {
            Ok(r) => r,
            Err(e) => {
                tracing::info!("git-blame enricher: cannot open repo at {}: {}", repo_root.display(), e);
                return Ok(EnrichmentResult::default());
            }
        };

        // Group nodes by their file path to walk blame once per file.
        let mut file_nodes: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (idx, node) in nodes.iter().enumerate() {
            let file = PathBuf::from(&node.id.file);
            file_nodes.entry(file).or_default().push(idx);
        }

        let mut updated_nodes: Vec<(String, BTreeMap<String, String>)> = Vec::new();

        for (rel_path, node_indices) in &file_nodes {
            // git2::blame_file requires a path relative to the workdir root.
            let blame_result = repo.blame_file(rel_path, None);
            let blame = match blame_result {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(
                        "git-blame: skipping {} — blame failed: {}",
                        rel_path.display(),
                        e
                    );
                    continue;
                }
            };

            let line_data = blame_to_line_vec(&blame);

            for &idx in node_indices {
                let node = &nodes[idx];
                let line_start = node.line_start;
                let line_end = node.line_end;

                if let Some((churn, sha7, author)) =
                    summarize_range(&line_data, line_start, line_end)
                {
                    let mut patches = BTreeMap::new();
                    patches.insert("churn_count".to_string(), churn.to_string());
                    patches.insert("last_commit_sha".to_string(), sha7);
                    patches.insert("last_author".to_string(), author);
                    updated_nodes.push((node.stable_id(), patches));
                }
            }
        }

        tracing::info!(
            "git-blame enricher: annotated {} / {} nodes",
            updated_nodes.len(),
            nodes.len(),
        );

        Ok(EnrichmentResult {
            updated_nodes,
            any_enricher_ran: true,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_range_empty() {
        let data: Vec<Option<LineBlame>> = vec![];
        assert!(summarize_range(&data, 0, 0).is_none());
        assert!(summarize_range(&data, 1, 0).is_none()); // start > end
    }

    #[test]
    fn test_summarize_range_single_commit() {
        let entry = LineBlame {
            sha7: "abc1234".to_string(),
            commit_time: 1000,
            author: "Alice".to_string(),
        };
        let data = vec![Some(entry.clone()), Some(entry.clone())];
        // 1-based: lines 1..=2
        let result = summarize_range(&data, 1, 2).unwrap();
        assert_eq!(result.0, 1); // 1 distinct commit
        assert_eq!(result.1, "abc1234");
        assert_eq!(result.2, "Alice");
    }

    #[test]
    fn test_summarize_range_multiple_commits() {
        let a = LineBlame {
            sha7: "aaa0001".to_string(),
            commit_time: 500,
            author: "Alice".to_string(),
        };
        let b = LineBlame {
            sha7: "bbb0002".to_string(),
            commit_time: 1500, // more recent
            author: "Bob".to_string(),
        };
        let data = vec![Some(a), Some(b.clone()), Some(b)];
        // lines 1..=3 — 2 distinct SHAs, Bob is most recent
        let result = summarize_range(&data, 1, 3).unwrap();
        assert_eq!(result.0, 2);
        assert_eq!(result.2, "Bob");
    }

    #[test]
    fn test_summarize_range_out_of_bounds() {
        let entry = LineBlame {
            sha7: "abc1234".to_string(),
            commit_time: 1000,
            author: "Alice".to_string(),
        };
        let data = vec![Some(entry)];
        // Requesting lines 1..=10 but only 1 line of data — should not panic
        let result = summarize_range(&data, 1, 10);
        // Should return Some with data from the one available line
        assert!(result.is_some());
        let (churn, _, _) = result.unwrap();
        assert_eq!(churn, 1);
    }
}
