//! Git co-change ("logical coupling") mining.
//!
//! Walks first-parent commit history (mirroring `src/git/pr_merges.rs`'s
//! `simplify_first_parent()` walk) and counts how often pairs of files change
//! together. This surfaces coupling that isn't visible from static analysis:
//! two files with no import/call relationship may nonetheless need to change
//! together in practice (e.g. a handler and its test, or a schema and its
//! serializer), and vice versa.
//!
//! # Bounding
//!
//! The walk is bounded by [`CoChangeConfig::max_commits`] and
//! [`CoChangeConfig::max_age_days`] so mining stays cheap on large repos.
//! Commits touching more than [`CoChangeConfig::max_files_per_commit`] files
//! (large refactors, vendoring drops, generated-file regeneration) are
//! skipped entirely -- they're noise for logical coupling, and without this
//! cutoff a single such commit would produce O(files^2) low-signal pairs.
//!
//! # Scoring
//!
//! For each unordered file pair `(a, b)` that appears together in at least
//! one mined commit:
//! - `support` = number of mined commits where both `a` and `b` changed.
//! - `changes_a` / `changes_b` = number of mined commits where `a` (resp. `b`)
//!   changed at all (not just together).
//! - `confidence` = `support / min(changes_a, changes_b)` -- how often the
//!   pair changes together relative to how often the less-frequently-changed
//!   file changes overall. A file that always changes alongside another,
//!   rarely-changed file will have confidence close to 1.0.
//!
//! # Incremental mining
//!
//! [`mine_cochanges`] takes an optional `since_sha` (exclusive lower bound).
//! When set, the walk stops at that commit instead of applying the
//! commit-count/age window -- this is how incremental scans mine only new
//! commits. The two new LanceDB columns (`cochange_support`,
//! `cochange_confidence`, see `src/graph/store.rs`) store only the two
//! aggregate numbers, not `changes_a`/`changes_b`, so merging a delta with
//! previously-persisted pairs would require reconstructing per-file totals
//! that aren't persisted. The chosen implementer decision (documented in the
//! #884 PR): incremental scans re-run a full bounded-window mining pass from
//! HEAD and replace all `CoChanges` edges wholesale, using the watermark only
//! to *skip* mining entirely when HEAD hasn't moved -- not to merge partial
//! deltas. This avoids introducing a second persisted data store for
//! per-file totals purely to support incremental accumulation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use git2::Repository;

use crate::graph::{CoChangeStats, CoChangeStatsMap, Confidence, Edge, EdgeKind, ExtractionSource};
use crate::scanner::CoChangeConfig;

/// Result of a co-change mining pass.
pub struct CoChangeMiningResult {
    /// Unordered file pairs that changed together at least once, with mined stats.
    pub pairs: Vec<CoChangePair>,
    /// HEAD SHA at the time of mining (for watermark update), if resolvable.
    pub head_sha: Option<String>,
    /// Number of commits actually walked (after age/count bounding, before the
    /// per-commit file-count cutoff).
    pub commits_walked: usize,
}

/// One mined file pair with its co-change stats.
#[derive(Debug, Clone, PartialEq)]
pub struct CoChangePair {
    pub file_a: PathBuf,
    pub file_b: PathBuf,
    pub support: u32,
    pub changes_a: u32,
    pub changes_b: u32,
}

impl CoChangePair {
    /// `support / min(changes_a, changes_b)`, clamped to `[0.0, 1.0]`.
    pub fn confidence(&self) -> f64 {
        let denom = self.changes_a.min(self.changes_b);
        if denom == 0 {
            0.0
        } else {
            (self.support as f64 / denom as f64).min(1.0)
        }
    }
}

/// Mine co-change pairs from git history.
///
/// * `repo_root` -- must be a git working directory (or inside one).
/// * `config` -- bounds on window size/age and the per-commit file-count cutoff.
/// * `since_sha` -- when `Some`, only commits strictly newer than this SHA are
///   walked (the commit-count/age window from `config` does not apply in this
///   mode); when `None`, the bounded window applies from HEAD.
pub fn mine_cochanges(
    repo_root: &Path,
    config: &CoChangeConfig,
    since_sha: Option<&str>,
) -> Result<CoChangeMiningResult> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;
    let mut revwalk = repo.revwalk().context("Failed to create revwalk")?;
    revwalk
        .push_head()
        .context("Failed to push HEAD to revwalk")?;
    revwalk.simplify_first_parent()?;

    let head_sha = repo.head().ok().and_then(|h| h.target()).map(|oid| oid.to_string());

    let since_oid = match since_sha {
        Some(sha) => match git2::Oid::from_str(sha) {
            Ok(oid) => Some(oid),
            Err(e) => {
                tracing::warn!("cochange: invalid watermark SHA '{}': {}", sha, e);
                None
            }
        },
        None => None,
    };

    let cutoff_secs = if since_oid.is_none() {
        let max_age = std::time::Duration::from_secs(config.max_age_days as u64 * 86_400);
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|now| now.checked_sub(max_age))
            .map(|d| d.as_secs() as i64)
    } else {
        None
    };

    // support[(a,b)] with a < b lexicographically -> co-change count.
    let mut support: HashMap<(PathBuf, PathBuf), u32> = HashMap::new();
    // total commits touching a given file (within the mined window).
    let mut file_changes: HashMap<PathBuf, u32> = HashMap::new();

    let mut commits_walked = 0usize;

    for oid_result in revwalk {
        let oid = oid_result.context("Failed to get commit oid from revwalk")?;

        if let Some(since) = since_oid
            && oid == since
        {
            break;
        }
        if since_oid.is_none() {
            if commits_walked >= config.max_commits {
                break;
            }
        }

        let commit = repo.find_commit(oid).context("Failed to find commit")?;

        if let Some(cutoff) = cutoff_secs
            && commit.time().seconds() < cutoff
        {
            break;
        }

        let new_tree = commit.tree().context("Failed to get commit tree")?;
        let parent_tree = if commit.parent_count() > 0 {
            let parent = commit.parent(0).context("Failed to get first parent")?;
            Some(parent.tree().context("Failed to get parent tree")?)
        } else {
            None
        };
        let changed = crate::git::changed_files_between(&repo, parent_tree.as_ref(), &new_tree)
            .context("Failed to diff commit")?;

        commits_walked += 1;

        // Dedup within a commit (a rename shows up as two delta entries for
        // the same logical change; a HashSet keeps the per-commit file count
        // meaningful).
        let files: HashSet<PathBuf> = changed.into_iter().collect();

        if files.len() > config.max_files_per_commit {
            continue;
        }
        if files.len() < 2 {
            // Still counts toward this file's total individual change count.
            for f in &files {
                *file_changes.entry(f.clone()).or_insert(0) += 1;
            }
            continue;
        }

        for f in &files {
            *file_changes.entry(f.clone()).or_insert(0) += 1;
        }

        let mut sorted: Vec<&PathBuf> = files.iter().collect();
        sorted.sort();
        for i in 0..sorted.len() {
            for j in (i + 1)..sorted.len() {
                let key = (sorted[i].clone(), sorted[j].clone());
                *support.entry(key).or_insert(0) += 1;
            }
        }
    }

    let pairs: Vec<CoChangePair> = support
        .into_iter()
        .map(|((file_a, file_b), support)| {
            let changes_a = *file_changes.get(&file_a).unwrap_or(&0);
            let changes_b = *file_changes.get(&file_b).unwrap_or(&0);
            CoChangePair {
                file_a,
                file_b,
                support,
                changes_a,
                changes_b,
            }
        })
        .collect();

    Ok(CoChangeMiningResult {
        pairs,
        head_sha,
        commits_walked,
    })
}

/// Build the `stable_id -> node` deterministic ID for a synthetic file anchor
/// node (`NodeKind::Other("file")`).
pub fn file_anchor_node_id(root_id: &str, file: &Path) -> crate::graph::NodeId {
    crate::graph::NodeId {
        root: root_id.to_string(),
        file: file.to_path_buf(),
        name: file.display().to_string(),
        kind: crate::graph::NodeKind::Other("file".to_string()),
    }
}

/// Convert mined pairs into `CoChanges` edges (between file anchor nodes) plus
/// a stable_id-keyed stats map for the two nullable LanceDB columns.
///
/// Pairs with zero confidence (denominator 0 -- shouldn't happen since a pair
/// only exists when both files changed together at least once) are still
/// emitted; callers filtering by `min_confidence` at query time handle that.
pub fn build_cochange_edges(
    root_id: &str,
    pairs: &[CoChangePair],
) -> (Vec<Edge>, CoChangeStatsMap) {
    let mut edges = Vec::with_capacity(pairs.len());
    let mut stats = CoChangeStatsMap::with_capacity(pairs.len());

    for pair in pairs {
        let from = file_anchor_node_id(root_id, &pair.file_a);
        let to = file_anchor_node_id(root_id, &pair.file_b);
        let edge = Edge {
            from,
            to,
            kind: EdgeKind::CoChanges,
            source: ExtractionSource::Git,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        };
        stats.insert(
            edge.stable_id(),
            CoChangeStats {
                support: pair.support,
                confidence: pair.confidence(),
            },
        );
        edges.push(edge);
    }

    (edges, stats)
}

/// Emit one `NodeKind::Other("file")` anchor node per unique file referenced
/// by `pairs`, skipping files that already have a node in `existing_stable_ids`
/// (dedup by stable ID -- avoids duplicate virtual anchors on rescans or when
/// another pass already anchors that exact file).
pub fn build_file_anchor_nodes(
    root_id: &str,
    pairs: &[CoChangePair],
    existing_stable_ids: &HashSet<String>,
) -> Vec<crate::graph::Node> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut nodes = Vec::new();

    let mut files: Vec<&PathBuf> = Vec::new();
    for pair in pairs {
        files.push(&pair.file_a);
        files.push(&pair.file_b);
    }

    for file in files {
        if !seen.insert(file.clone()) {
            continue;
        }
        let id = file_anchor_node_id(root_id, file);
        if existing_stable_ids.contains(&id.to_stable_id()) {
            continue;
        }
        nodes.push(crate::graph::Node {
            id,
            language: String::new(),
            line_start: 0,
            line_end: 0,
            signature: format!("file {}", file.display()),
            body: String::new(),
            metadata: std::collections::BTreeMap::new(),
            source: ExtractionSource::Git,
        });
    }

    nodes
}

/// Resolve a changed-file set for `mode="cochange_gaps"` (#884).
///
/// `scope` is one of:
/// - `"working_tree"` (default) -- HEAD tree vs. working directory (covers
///   both staged and unstaged changes, mirroring the semantics of
///   `git status`/`git diff HEAD`).
/// - `"staged"` -- HEAD tree vs. the index only.
/// - `"<base>..<head>"` -- an explicit two-ref diff (e.g. `main..HEAD`).
///
/// # Implementer note
///
/// `src/server/changed_file_plan.rs` already resolves working-tree/staged/
/// `base..head` diffs, but its `discover_git_worktree_changes` is embedded in
/// a much larger LSP-scheduling data model (`ChangedFilePlan`, per-node
/// operation-fanout bounds, `ChangedFileProvenance`) that isn't a fit for "just
/// give me the changed path list" here. This is a small, deliberately parallel
/// implementation rather than a forced shared helper -- see #884 PR notes.
pub fn resolve_changed_file_set(repo_root: &Path, scope: &str) -> Result<HashSet<PathBuf>> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;

    if let Some((base, head)) = scope.split_once("..") {
        let base_obj = repo
            .revparse_single(base)
            .with_context(|| format!("Failed to resolve base ref '{base}'"))?;
        let head_obj = repo
            .revparse_single(head)
            .with_context(|| format!("Failed to resolve head ref '{head}'"))?;
        let base_tree = base_obj.peel_to_tree().context("base ref is not a tree-ish")?;
        let head_tree = head_obj.peel_to_tree().context("head ref is not a tree-ish")?;
        let files = crate::git::changed_files_between(&repo, Some(&base_tree), &head_tree)?;
        return Ok(files.into_iter().collect());
    }

    let head_tree = repo
        .head()
        .context("Failed to resolve HEAD")?
        .peel_to_tree()
        .context("Failed to peel HEAD to tree")?;

    let mut opts = git2::DiffOptions::new();
    let diff = if scope == "staged" {
        repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut opts))
            .context("Failed to diff HEAD tree to index")?
    } else {
        repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut opts))
            .context("Failed to diff HEAD tree to working directory")?
    };

    let mut paths = HashSet::new();
    diff.foreach(
        &mut |delta, _| {
            if let Some(path) = delta.new_file().path() {
                paths.insert(path.to_path_buf());
            } else if let Some(path) = delta.old_file().path() {
                paths.insert(path.to_path_buf());
            }
            true
        },
        None,
        None,
        None,
    )
    .context("Failed to iterate diff deltas")?;

    Ok(paths)
}

/// Convenience wrapper: mine, then build file anchor nodes + `CoChanges` edges
/// + stats map in one call. Returns `None` if the repo has no `.git` (mining
/// is not attempted; callers should already gate on `admit_git_history_producer()`
/// and log a "not available (no .git)" message per #884's non-git-root handling).
pub fn mine_and_build(
    repo_root: &Path,
    root_id: &str,
    config: &CoChangeConfig,
    since_sha: Option<&str>,
    existing_stable_ids: &HashSet<String>,
) -> Result<(Vec<crate::graph::Node>, Vec<Edge>, CoChangeStatsMap, Option<String>)> {
    let result = mine_cochanges(repo_root, config, since_sha)?;
    let nodes = build_file_anchor_nodes(root_id, &result.pairs, existing_stable_ids);
    let (edges, stats) = build_cochange_edges(root_id, &result.pairs);
    Ok((nodes, edges, stats, result.head_sha))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn init_repo(dir: &Path) -> Repository {
        let repo = Repository::init(dir).expect("init repo");
        let mut config = repo.config().expect("config");
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        repo
    }

    fn commit_files(repo: &Repository, dir: &Path, files: &[(&str, &str)], message: &str) -> git2::Oid {
        for (name, content) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        let mut index = repo.index().unwrap();
        for (name, _) in files {
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    #[test]
    fn test_mine_cochanges_finds_pair() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);

        commit_files(&repo, dir, &[("a.rs", "1"), ("b.rs", "1")], "first: a+b");
        commit_files(&repo, dir, &[("a.rs", "2"), ("b.rs", "2")], "second: a+b again");
        commit_files(&repo, dir, &[("c.rs", "1")], "third: c alone");

        let config = CoChangeConfig::default();
        let result = mine_cochanges(dir, &config, None).expect("mine");

        assert_eq!(result.commits_walked, 3);
        assert_eq!(result.pairs.len(), 1, "only a<->b pair should exist");
        let pair = &result.pairs[0];
        assert_eq!(pair.file_a, PathBuf::from("a.rs"));
        assert_eq!(pair.file_b, PathBuf::from("b.rs"));
        assert_eq!(pair.support, 2);
        assert_eq!(pair.changes_a, 2);
        assert_eq!(pair.changes_b, 2);
        assert_eq!(pair.confidence(), 1.0);
    }

    #[test]
    fn test_confidence_scoring_partial_overlap() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);

        // a+b together once, a alone twice more.
        commit_files(&repo, dir, &[("a.rs", "1"), ("b.rs", "1")], "a+b");
        commit_files(&repo, dir, &[("a.rs", "2")], "a alone 1");
        commit_files(&repo, dir, &[("a.rs", "3")], "a alone 2");

        let config = CoChangeConfig::default();
        let result = mine_cochanges(dir, &config, None).expect("mine");
        assert_eq!(result.pairs.len(), 1);
        let pair = &result.pairs[0];
        assert_eq!(pair.support, 1);
        assert_eq!(pair.changes_a, 3);
        assert_eq!(pair.changes_b, 1);
        // support / min(changes_a, changes_b) = 1 / 1 = 1.0
        assert_eq!(pair.confidence(), 1.0);
    }

    #[test]
    fn test_max_files_per_commit_skips_large_commits() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);

        let files: Vec<(String, String)> = (0..10)
            .map(|i| (format!("f{i}.rs"), "x".to_string()))
            .collect();
        let file_refs: Vec<(&str, &str)> = files.iter().map(|(n, c)| (n.as_str(), c.as_str())).collect();
        commit_files(&repo, dir, &file_refs, "big commit");

        let config = CoChangeConfig {
            max_files_per_commit: 5,
            ..CoChangeConfig::default()
        };
        let result = mine_cochanges(dir, &config, None).expect("mine");
        assert_eq!(result.commits_walked, 1);
        assert!(
            result.pairs.is_empty(),
            "commit touching more files than the cutoff should be skipped entirely"
        );
    }

    #[test]
    fn test_max_commits_window_bounds_walk() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);

        for i in 0..5 {
            let content = format!("x{i}");
            commit_files(
                &repo,
                dir,
                &[("a.rs", content.as_str()), ("b.rs", content.as_str())],
                &format!("commit {i}"),
            );
        }

        let config = CoChangeConfig {
            max_commits: 2,
            ..CoChangeConfig::default()
        };
        let result = mine_cochanges(dir, &config, None).expect("mine");
        assert_eq!(result.commits_walked, 2);
        assert_eq!(result.pairs[0].support, 2);
    }

    #[test]
    fn test_since_sha_only_walks_new_commits() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);

        let first = commit_files(&repo, dir, &[("a.rs", "1"), ("b.rs", "1")], "first");
        commit_files(&repo, dir, &[("a.rs", "2"), ("b.rs", "2")], "second");
        commit_files(&repo, dir, &[("a.rs", "3"), ("b.rs", "3")], "third");

        let config = CoChangeConfig::default();
        let result = mine_cochanges(dir, &config, Some(&first.to_string())).expect("mine");

        // Only "second" and "third" should be walked (stops at `first`, exclusive).
        assert_eq!(result.commits_walked, 2);
        assert_eq!(result.pairs[0].support, 2);
    }

    #[test]
    fn test_build_cochange_edges_and_stats() {
        let pairs = vec![CoChangePair {
            file_a: PathBuf::from("a.rs"),
            file_b: PathBuf::from("b.rs"),
            support: 3,
            changes_a: 4,
            changes_b: 5,
        }];
        let (edges, stats) = build_cochange_edges("repo", &pairs);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::CoChanges);
        let stat = stats.get(&edges[0].stable_id()).expect("stats present");
        assert_eq!(stat.support, 3);
        assert!((stat.confidence - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_resolve_changed_file_set_working_tree() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        commit_files(&repo, dir, &[("a.rs", "1"), ("b.rs", "1")], "initial");
        fs::write(dir.join("a.rs"), "2").unwrap();

        let files = resolve_changed_file_set(dir, "working_tree").unwrap();
        assert!(files.contains(&PathBuf::from("a.rs")));
        assert!(!files.contains(&PathBuf::from("b.rs")));
    }

    #[test]
    fn test_resolve_changed_file_set_base_head_range() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let first = commit_files(&repo, dir, &[("a.rs", "1")], "first");
        commit_files(&repo, dir, &[("b.rs", "1")], "second");

        let scope = format!("{}..HEAD", first);
        let files = resolve_changed_file_set(dir, &scope).unwrap();
        assert!(files.contains(&PathBuf::from("b.rs")));
        assert!(!files.contains(&PathBuf::from("a.rs")));
    }

    #[test]
    fn test_build_file_anchor_nodes_dedups() {
        let pairs = vec![
            CoChangePair {
                file_a: PathBuf::from("a.rs"),
                file_b: PathBuf::from("b.rs"),
                support: 1,
                changes_a: 1,
                changes_b: 1,
            },
            CoChangePair {
                file_a: PathBuf::from("a.rs"),
                file_b: PathBuf::from("c.rs"),
                support: 1,
                changes_a: 1,
                changes_b: 1,
            },
        ];
        let nodes = build_file_anchor_nodes("repo", &pairs, &HashSet::new());
        // a.rs, b.rs, c.rs -- three unique files, "a.rs" not duplicated.
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn test_build_file_anchor_nodes_skips_existing() {
        let pairs = vec![CoChangePair {
            file_a: PathBuf::from("a.rs"),
            file_b: PathBuf::from("b.rs"),
            support: 1,
            changes_a: 1,
            changes_b: 1,
        }];
        let existing_id = file_anchor_node_id("repo", Path::new("a.rs")).to_stable_id();
        let mut existing = HashSet::new();
        existing.insert(existing_id);
        let nodes = build_file_anchor_nodes("repo", &pairs, &existing);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.file, PathBuf::from("b.rs"));
    }
}
