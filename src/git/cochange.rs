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
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Repository};

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
    /// Per-file commit counts within the mined window -- "churn" (#889). This is
    /// the same map already built to compute `CoChangePair::changes_a/changes_b`;
    /// surfaced here rather than discarded so callers can render it directly.
    pub file_changes: HashMap<PathBuf, u32>,
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
        // `max_commits` is a hard ceiling on every walk, including watermark
        // walks. A watermark that parses but is not on the current first-parent
        // chain (history rewrite, or a watermark carried over from another
        // line) never satisfies the `oid == since` stop condition, and
        // `cutoff_secs` is `None` for watermark walks -- without this ceiling
        // the miner would walk all of history (#884 review).
        if commits_walked >= config.max_commits {
            break;
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
        file_changes,
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
/// by `pairs` or with a non-zero entry in `file_changes` (#889 -- churn needs
/// an anchor on every changed file, not only files that appear in a
/// co-change pair), skipping files that already have a node in
/// `existing_stable_ids` (dedup by stable ID -- avoids duplicate virtual
/// anchors on rescans or when another pass already anchors that exact file).
///
/// Anchors for files with known churn carry a `churn` metadata key (total
/// commits touching that file within the mined window).
pub fn build_file_anchor_nodes(
    root_id: &str,
    pairs: &[CoChangePair],
    file_changes: &HashMap<PathBuf, u32>,
    existing_stable_ids: &HashSet<String>,
) -> Vec<crate::graph::Node> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut nodes = Vec::new();

    let mut files: Vec<&PathBuf> = Vec::new();
    for pair in pairs {
        files.push(&pair.file_a);
        files.push(&pair.file_b);
    }
    for file in file_changes.keys() {
        files.push(file);
    }

    for file in files {
        if !seen.insert(file.clone()) {
            continue;
        }
        let id = file_anchor_node_id(root_id, file);
        if existing_stable_ids.contains(&id.to_stable_id()) {
            continue;
        }
        let mut metadata = std::collections::BTreeMap::new();
        if let Some(churn) = file_changes.get(file)
            && *churn > 0
        {
            metadata.insert("churn".to_string(), churn.to_string());
        }
        nodes.push(crate::graph::Node {
            id,
            // Synthetic anchors have no source language. Language aggregation
            // filters empty strings, so this contributes nothing to a root's
            // language list (#884 review); asserted by
            // `file_anchor_nodes_carry_no_language`.
            language: String::new(),
            line_start: 0,
            line_end: 0,
            signature: format!("file {}", file.display()),
            body: String::new(),
            metadata,
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

    if scope.contains("...") {
        anyhow::bail!(
            "cochange_gaps: three-dot symmetric-difference ranges ('{scope}') are not supported -- \
             use a two-dot range ('<base>..<head>'), 'staged', or 'working_tree'."
        );
    }

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

/// Kind of change for a file entry in a `mode="change"` bundle (#899).
///
/// Mirrors `server::changed_file_plan::ChangedFileKind` conceptually, but is
/// defined here rather than imported: that type lives in a `server`-private
/// module built around a much larger LSP-scheduling data model, the same
/// reason `resolve_changed_file_set` above is "a small, deliberately
/// parallel implementation rather than a forced shared helper" instead of
/// reusing `discover_git_worktree_changes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeFileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

impl fmt::Display for ChangeFileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ChangeFileKind::Added => "added",
            ChangeFileKind::Modified => "modified",
            ChangeFileKind::Deleted => "deleted",
            ChangeFileKind::Renamed => "renamed",
            ChangeFileKind::Copied => "copied",
            ChangeFileKind::TypeChanged => "type-changed",
            ChangeFileKind::Untracked => "untracked",
        };
        write!(f, "{s}")
    }
}

fn change_file_kind(delta: Delta) -> Option<ChangeFileKind> {
    match delta {
        Delta::Added => Some(ChangeFileKind::Added),
        Delta::Modified => Some(ChangeFileKind::Modified),
        Delta::Deleted => Some(ChangeFileKind::Deleted),
        Delta::Renamed => Some(ChangeFileKind::Renamed),
        Delta::Copied => Some(ChangeFileKind::Copied),
        Delta::Typechange => Some(ChangeFileKind::TypeChanged),
        Delta::Untracked => Some(ChangeFileKind::Untracked),
        Delta::Conflicted | Delta::Unreadable => Some(ChangeFileKind::Modified),
        Delta::Unmodified | Delta::Ignored => None,
    }
}

/// One changed file in a `mode="change"` bundle: its kind, old/new paths
/// (renames carry both), and the new-side line ranges touched by
/// added/modified hunks.
///
/// `hunks` ranges are computed with zero context lines (see
/// [`resolve_changed_file_entries`]), so a range is exactly the lines git
/// considers changed on the new side -- not the +/-3 lines of surrounding
/// context a normal unified diff would include. This is what makes
/// hunk-intersection precise: without zero context, editing one line in a
/// large function would make the hunk range bleed into whichever unrelated
/// symbols happen to sit within 3 lines of it.
#[derive(Debug, Clone)]
pub struct ChangedFileEntry {
    pub kind: ChangeFileKind,
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    /// New-side line ranges (1-based, inclusive) touched by added/modified
    /// hunks.
    pub hunks: Vec<(usize, usize)>,
}

impl ChangedFileEntry {
    /// The path to key node/graph lookups on: `new_path` when present
    /// (added/modified/renamed/copied/typechanged/untracked), else
    /// `old_path` (deleted files have no new-side path).
    pub fn path(&self) -> &Path {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .expect("changed file entry has neither old_path nor new_path")
    }

    pub fn is_deleted(&self) -> bool {
        matches!(self.kind, ChangeFileKind::Deleted)
    }

    /// Whether every symbol in this file counts as changed -- true for newly
    /// created files (added/untracked), where there is no "old" version to
    /// diff hunks against.
    pub fn all_symbols_changed(&self) -> bool {
        matches!(self.kind, ChangeFileKind::Added | ChangeFileKind::Untracked)
    }

    /// Whether a node spanning `[line_start, line_end]` (1-based, inclusive)
    /// intersects any added/modified hunk in this file.
    pub fn touches_span(&self, line_start: usize, line_end: usize) -> bool {
        self.hunks
            .iter()
            .any(|(hunk_start, hunk_end)| *hunk_start <= line_end && line_start <= *hunk_end)
    }
}

/// Build the git2 diff for a `mode="change"` scope (#899): `"working_tree"`
/// (HEAD vs. working directory + index), `"staged"` (HEAD vs. index), or
/// `"<base>..<head>"` (explicit two-ref diff). Three-dot ranges are
/// rejected, matching [`resolve_changed_file_set`]'s message.
///
/// Zero context lines: hunk ranges from this diff are exactly the
/// added/modified new-side lines, not lines +/- surrounding context.
fn open_change_scope_diff<'repo>(repo: &'repo Repository, scope: &str) -> Result<Diff<'repo>> {
    if scope.contains("...") {
        anyhow::bail!(
            "change: three-dot symmetric-difference ranges ('{scope}') are not supported -- \
             use a two-dot range ('<base>..<head>'), 'staged', or 'working_tree'."
        );
    }

    let mut opts = DiffOptions::new();
    opts.context_lines(0).include_typechange(true);

    let diff = if let Some((base, head)) = scope.split_once("..") {
        let base_obj = repo
            .revparse_single(base)
            .with_context(|| format!("Failed to resolve base ref '{base}'"))?;
        let head_obj = repo
            .revparse_single(head)
            .with_context(|| format!("Failed to resolve head ref '{head}'"))?;
        let base_tree = base_obj
            .peel_to_tree()
            .context("base ref is not a tree-ish")?;
        let head_tree = head_obj
            .peel_to_tree()
            .context("head ref is not a tree-ish")?;
        repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut opts))
            .context("Failed to diff base..head trees")?
    } else {
        let head_tree = repo
            .head()
            .context("Failed to resolve HEAD")?
            .peel_to_tree()
            .context("Failed to peel HEAD to tree")?;
        match scope {
            "staged" => repo
                .diff_tree_to_index(Some(&head_tree), None, Some(&mut opts))
                .context("Failed to diff HEAD tree to index")?,
            "working_tree" => {
                opts.include_untracked(true).recurse_untracked_dirs(true);
                repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut opts))
                    .context("Failed to diff HEAD tree to working directory")?
            }
            other => anyhow::bail!(
                "change: unknown scope '{other}' -- use 'working_tree', 'staged', or '<base>..<head>'."
            ),
        }
    };
    Ok(diff)
}

/// Resolve a scope (see [`open_change_scope_diff`]) into per-file change
/// entries with kind, old/new paths, and hunk-level new-side line ranges
/// (#899). This is the hunk-intersection piece nothing else in the codebase
/// computes: `grep -rn "hunk|changed_lines" src` before this change was
/// empty -- every existing consumer (`resolve_changed_file_set`,
/// `changed_file_plan.rs`) works at file granularity.
pub fn resolve_changed_file_entries(
    repo_root: &Path,
    scope: &str,
) -> Result<Vec<ChangedFileEntry>> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;
    let mut diff = open_change_scope_diff(&repo, scope)?;

    let mut find_opts = DiffFindOptions::new();
    // `for_untracked` matters for `scope="working_tree"`: a rename that has
    // not been `git add`-ed shows up as a `Deleted` delta (old path) plus an
    // `Untracked` delta (new path) rather than one `Renamed` delta unless
    // untracked files are included in similarity detection.
    find_opts.renames(true).copies(true).for_untracked(true);
    diff.find_similar(Some(&mut find_opts))
        .context("Failed to detect renamed/copied files")?;

    let mut entries: Vec<ChangedFileEntry> = Vec::new();
    // Both maps point at the same `entries` index; a delta is keyed by its
    // new path when one exists (added/modified/renamed/copied/typechanged/
    // untracked), falling back to the old path only for deletions.
    let mut index_by_new_path: HashMap<PathBuf, usize> = HashMap::new();
    let mut index_by_old_path: HashMap<PathBuf, usize> = HashMap::new();

    for delta in diff.deltas() {
        let Some(kind) = change_file_kind(delta.status()) else {
            continue;
        };
        let old_path = delta.old_file().path().map(|p| p.to_path_buf());
        let new_path = delta.new_file().path().map(|p| p.to_path_buf());
        if old_path.is_none() && new_path.is_none() {
            continue;
        }
        let idx = entries.len();
        if let Some(np) = &new_path {
            index_by_new_path.insert(np.clone(), idx);
        }
        if let Some(op) = &old_path {
            index_by_old_path.insert(op.clone(), idx);
        }
        entries.push(ChangedFileEntry {
            kind,
            old_path,
            new_path,
            hunks: Vec::new(),
        });
    }

    diff.foreach(
        &mut |_delta, _progress| true,
        None,
        Some(&mut |delta, hunk| {
            let path = delta.new_file().path().or_else(|| delta.old_file().path());
            let Some(path) = path else {
                return true;
            };
            let key = path.to_path_buf();
            let idx = index_by_new_path
                .get(&key)
                .or_else(|| index_by_old_path.get(&key));
            if let Some(&idx) = idx {
                // A deleted file has no new side at all -- new-side line
                // numbers from its hunks are meaningless, and callers never
                // consult `hunks` for a deleted entry (they list its graphed
                // symbols directly, flagged, via `is_deleted()`).
                if entries[idx].kind != ChangeFileKind::Deleted {
                    let start = hunk.new_start() as usize;
                    let lines = hunk.new_lines() as usize;
                    if lines > 0 {
                        entries[idx].hunks.push((start, start + lines - 1));
                    } else {
                        // Pure-deletion hunk within an otherwise-present file:
                        // `new_lines() == 0`, `new_start()` is the new-side
                        // line the deletion sits after (0 if at the very start
                        // of the file). Anchor on that boundary line so a
                        // symbol immediately adjacent to a deletion still
                        // counts as touched, rather than silently dropping
                        // deletions from hunk-intersection entirely.
                        let anchor = start.max(1);
                        entries[idx].hunks.push((anchor, anchor));
                    }
                }
            }
            true
        }),
        None,
    )
    .context("Failed to iterate diff hunks")?;

    Ok(entries)
}

/// Result of [`mine_and_build`]: file anchor nodes, `CoChanges` edges, the
/// stable-id-keyed stats map for those edges, the HEAD SHA mined (for the
/// caller to update the watermark), and the per-file churn map (#889).
pub type MineAndBuildResult = (
    Vec<crate::graph::Node>,
    Vec<Edge>,
    CoChangeStatsMap,
    Option<String>,
    HashMap<PathBuf, u32>,
);

/// Convenience wrapper: mine, then build file anchor nodes and `CoChanges`
/// edges and stats map in one call.
///
/// Note: if the repo has no `.git`, mining is not attempted -- callers should
/// already gate on `admit_git_history_producer()` and log a "not available
/// (no .git)" message per #884's non-git-root handling.
pub fn mine_and_build(
    repo_root: &Path,
    root_id: &str,
    config: &CoChangeConfig,
    since_sha: Option<&str>,
    existing_stable_ids: &HashSet<String>,
) -> Result<MineAndBuildResult> {
    let result = mine_cochanges(repo_root, config, since_sha)?;
    let nodes = build_file_anchor_nodes(
        root_id,
        &result.pairs,
        &result.file_changes,
        existing_stable_ids,
    );
    let (edges, stats) = build_cochange_edges(root_id, &result.pairs);
    Ok((nodes, edges, stats, result.head_sha, result.file_changes))
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
    fn offchain_watermark_walk_stays_bounded() {
        // A watermark that parses but is not on the current first-parent chain
        // (history rewrite, or a watermark from another line) never satisfies
        // the `oid == since` stop condition, and age cutoff is disabled for
        // watermark walks. `max_commits` must still cap the walk (#884 review).
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        for i in 0..8 {
            commit_files(
                &repo,
                dir,
                &[("a.rs", &format!("{i}")), ("b.rs", &format!("{i}"))],
                &format!("commit {i}"),
            );
        }

        // A well-formed but unrelated SHA: parses, never matches any commit.
        let offchain = "0123456789abcdef0123456789abcdef01234567";
        let config = CoChangeConfig {
            max_commits: 3,
            max_age_days: 365,
            max_files_per_commit: 50,
        };
        let result = mine_cochanges(dir, &config, Some(offchain)).unwrap();
        assert_eq!(
            result.commits_walked, 3,
            "off-chain watermark must still respect max_commits, walked {}",
            result.commits_walked
        );
    }

    #[test]
    fn file_anchor_nodes_carry_no_language() {
        // Synthetic anchors must not contribute a bogus language to per-root
        // language aggregation in `service::roots` (#884 review).
        let pairs = vec![CoChangePair {
            file_a: PathBuf::from("src/a.rs"),
            file_b: PathBuf::from("src/b.rs"),
            support: 3,
            changes_a: 4,
            changes_b: 4,
        }];
        let file_changes = HashMap::from([
            (PathBuf::from("src/a.rs"), 4u32),
            (PathBuf::from("src/b.rs"), 4u32),
        ]);
        let nodes = build_file_anchor_nodes("repo", &pairs, &file_changes, &HashSet::new());
        assert_eq!(nodes.len(), 2);
        for node in &nodes {
            assert!(
                node.language.is_empty(),
                "anchor {} leaked language {:?}",
                node.id.name,
                node.language
            );
            assert!(matches!(&node.id.kind, crate::graph::NodeKind::Other(k) if k == "file"));
        }
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
        let file_changes = HashMap::from([
            (PathBuf::from("a.rs"), 2u32),
            (PathBuf::from("b.rs"), 1u32),
            (PathBuf::from("c.rs"), 1u32),
        ]);
        let nodes = build_file_anchor_nodes("repo", &pairs, &file_changes, &HashSet::new());
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
        let file_changes =
            HashMap::from([(PathBuf::from("a.rs"), 1u32), (PathBuf::from("b.rs"), 1u32)]);
        let existing_id = file_anchor_node_id("repo", Path::new("a.rs")).to_stable_id();
        let mut existing = HashSet::new();
        existing.insert(existing_id);
        let nodes = build_file_anchor_nodes("repo", &pairs, &file_changes, &existing);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.file, PathBuf::from("b.rs"));
    }

    #[test]
    fn test_mine_cochanges_reports_file_changes_for_solo_commits() {
        // #889: file_changes must count every commit touching a file, including
        // single-file commits that never form a co-change pair.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);

        commit_files(&repo, dir, &[("a.rs", "1"), ("b.rs", "1")], "first: a+b");
        commit_files(&repo, dir, &[("a.rs", "2")], "second: a alone");
        commit_files(&repo, dir, &[("c.rs", "1")], "third: c alone");

        let config = CoChangeConfig::default();
        let result = mine_cochanges(dir, &config, None).expect("mine");

        assert_eq!(result.file_changes.get(&PathBuf::from("a.rs")), Some(&2));
        assert_eq!(result.file_changes.get(&PathBuf::from("b.rs")), Some(&1));
        assert_eq!(result.file_changes.get(&PathBuf::from("c.rs")), Some(&1));
    }

    #[test]
    fn test_build_file_anchor_nodes_covers_unpaired_churned_files() {
        // #889: a file with churn but no co-change partner still gets an
        // anchor node carrying its churn metadata.
        let pairs: Vec<CoChangePair> = Vec::new();
        let file_changes = HashMap::from([(PathBuf::from("solo.rs"), 7u32)]);
        let nodes = build_file_anchor_nodes("repo", &pairs, &file_changes, &HashSet::new());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.file, PathBuf::from("solo.rs"));
        assert_eq!(
            nodes[0].metadata.get("churn").map(String::as_str),
            Some("7")
        );
    }

    #[test]
    fn test_build_file_anchor_nodes_zero_churn_gets_no_metadata() {
        // A file with a zero entry in file_changes (shouldn't normally occur,
        // but defensive) gets an anchor without a churn key rather than "0".
        let pairs = vec![CoChangePair {
            file_a: PathBuf::from("a.rs"),
            file_b: PathBuf::from("b.rs"),
            support: 1,
            changes_a: 1,
            changes_b: 1,
        }];
        let file_changes =
            HashMap::from([(PathBuf::from("a.rs"), 1u32), (PathBuf::from("b.rs"), 0u32)]);
        let nodes = build_file_anchor_nodes("repo", &pairs, &file_changes, &HashSet::new());
        let b_node = nodes
            .iter()
            .find(|n| n.id.file == PathBuf::from("b.rs"))
            .expect("b.rs anchor present");
        assert!(b_node.metadata.get("churn").is_none());
    }

    // ── mode="change" hunk-intersection tests (#899) ────────────────────────

    #[test]
    fn touches_span_symbol_fully_inside_hunk() {
        let entry = ChangedFileEntry {
            kind: ChangeFileKind::Modified,
            old_path: Some(PathBuf::from("a.rs")),
            new_path: Some(PathBuf::from("a.rs")),
            hunks: vec![(10, 30)],
        };
        assert!(
            entry.touches_span(15, 20),
            "symbol fully inside hunk must touch"
        );
    }

    #[test]
    fn touches_span_symbol_straddles_hunk_boundary() {
        let entry = ChangedFileEntry {
            kind: ChangeFileKind::Modified,
            old_path: Some(PathBuf::from("a.rs")),
            new_path: Some(PathBuf::from("a.rs")),
            hunks: vec![(10, 30)],
        };
        // Symbol starts before the hunk and ends inside it.
        assert!(entry.touches_span(5, 12), "straddling symbol must touch");
        // Symbol starts inside the hunk and ends after it.
        assert!(entry.touches_span(25, 40), "straddling symbol must touch");
    }

    #[test]
    fn touches_span_adjacent_symbol_does_not_touch() {
        let entry = ChangedFileEntry {
            kind: ChangeFileKind::Modified,
            old_path: Some(PathBuf::from("a.rs")),
            new_path: Some(PathBuf::from("a.rs")),
            hunks: vec![(10, 30)],
        };
        // Immediately before, no overlap.
        assert!(
            !entry.touches_span(1, 9),
            "adjacent symbol before hunk must not touch"
        );
        // Immediately after, no overlap.
        assert!(
            !entry.touches_span(31, 40),
            "adjacent symbol after hunk must not touch"
        );
    }

    #[test]
    fn all_symbols_changed_true_for_added_and_untracked() {
        let added = ChangedFileEntry {
            kind: ChangeFileKind::Added,
            old_path: None,
            new_path: Some(PathBuf::from("new.rs")),
            hunks: vec![],
        };
        let untracked = ChangedFileEntry {
            kind: ChangeFileKind::Untracked,
            old_path: None,
            new_path: Some(PathBuf::from("scratch.rs")),
            hunks: vec![],
        };
        let modified = ChangedFileEntry {
            kind: ChangeFileKind::Modified,
            old_path: Some(PathBuf::from("a.rs")),
            new_path: Some(PathBuf::from("a.rs")),
            hunks: vec![],
        };
        assert!(added.all_symbols_changed());
        assert!(untracked.all_symbols_changed());
        assert!(!modified.all_symbols_changed());
    }

    #[test]
    fn deleted_entry_path_falls_back_to_old_path() {
        let entry = ChangedFileEntry {
            kind: ChangeFileKind::Deleted,
            old_path: Some(PathBuf::from("gone.rs")),
            new_path: None,
            hunks: vec![],
        };
        assert!(entry.is_deleted());
        assert_eq!(entry.path(), Path::new("gone.rs"));
    }

    #[test]
    fn resolve_changed_file_entries_rejects_three_dot_scope() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        commit_files(&repo, dir, &[("a.rs", "1")], "initial");
        let err = resolve_changed_file_entries(dir, "main...HEAD").unwrap_err();
        assert!(
            err.to_string().contains("three-dot"),
            "expected three-dot rejection, got: {err}"
        );
    }

    #[test]
    fn resolve_changed_file_entries_added_file_working_tree() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        commit_files(&repo, dir, &[("a.rs", "1\n2\n3\n")], "initial");
        fs::write(dir.join("b.rs"), "fn new_fn() {}\n").unwrap();

        let entries = resolve_changed_file_entries(dir, "working_tree").unwrap();
        let added = entries
            .iter()
            .find(|e| e.new_path.as_deref() == Some(Path::new("b.rs")))
            .expect("b.rs entry present");
        assert!(
            added.all_symbols_changed(),
            "new file: kind={:?}",
            added.kind
        );
    }

    #[test]
    fn resolve_changed_file_entries_deleted_file_working_tree() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        commit_files(
            &repo,
            dir,
            &[("a.rs", "1"), ("b.rs", "1\n2\n3\n")],
            "initial",
        );
        fs::remove_file(dir.join("b.rs")).unwrap();

        let entries = resolve_changed_file_entries(dir, "working_tree").unwrap();
        let deleted = entries
            .iter()
            .find(|e| e.old_path.as_deref() == Some(Path::new("b.rs")))
            .expect("b.rs entry present");
        assert!(deleted.is_deleted());
        assert!(deleted.hunks.is_empty());
    }

    #[test]
    fn resolve_changed_file_entries_modified_hunk_intersects_touched_line_only() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let original = "line1\nline2\nline3\nline4\nline5\n";
        commit_files(&repo, dir, &[("a.rs", original)], "initial");
        let modified = "line1\nline2\nCHANGED\nline4\nline5\n";
        fs::write(dir.join("a.rs"), modified).unwrap();

        let entries = resolve_changed_file_entries(dir, "working_tree").unwrap();
        let entry = entries
            .iter()
            .find(|e| e.new_path.as_deref() == Some(Path::new("a.rs")))
            .expect("a.rs entry present");
        assert_eq!(entry.kind, ChangeFileKind::Modified);
        // Zero-context diff: the only touched new-side line is line 3.
        assert!(
            entry.touches_span(3, 3),
            "line 3 must be touched, hunks={:?}",
            entry.hunks
        );
        assert!(
            !entry.touches_span(1, 2),
            "line 1-2 must not be touched, hunks={:?}",
            entry.hunks
        );
        assert!(
            !entry.touches_span(4, 5),
            "line 4-5 must not be touched, hunks={:?}",
            entry.hunks
        );
    }

    #[test]
    fn resolve_changed_file_entries_detects_rename() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        // A large-enough body for git's default rename similarity threshold
        // (50%) to pair an unmodified rename.
        let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
        commit_files(&repo, dir, &[("old_name.rs", &body)], "initial");
        fs::remove_file(dir.join("old_name.rs")).unwrap();
        fs::write(dir.join("new_name.rs"), &body).unwrap();

        let entries = resolve_changed_file_entries(dir, "working_tree").unwrap();
        let renamed = entries
            .iter()
            .find(|e| e.new_path.as_deref() == Some(Path::new("new_name.rs")));
        match renamed {
            Some(e) => {
                assert_eq!(e.kind, ChangeFileKind::Renamed);
                assert_eq!(e.old_path.as_deref(), Some(Path::new("old_name.rs")));
            }
            None => panic!("expected a renamed entry for new_name.rs, got: {entries:?}"),
        }
    }

    #[test]
    fn resolve_changed_file_entries_staged_scope() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        commit_files(&repo, dir, &[("a.rs", "1\n2\n")], "initial");
        fs::write(dir.join("a.rs"), "1\nCHANGED\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.rs")).unwrap();
        index.write().unwrap();

        let entries = resolve_changed_file_entries(dir, "staged").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ChangeFileKind::Modified);
    }

    #[test]
    fn resolve_changed_file_entries_base_head_scope() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let first = commit_files(&repo, dir, &[("a.rs", "1")], "first");
        commit_files(&repo, dir, &[("b.rs", "1")], "second");

        let scope = format!("{first}..HEAD");
        let entries = resolve_changed_file_entries(dir, &scope).unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.new_path.as_deref() == Some(Path::new("b.rs")))
        );
        assert!(
            !entries
                .iter()
                .any(|e| e.new_path.as_deref() == Some(Path::new("a.rs")))
        );
    }
}
