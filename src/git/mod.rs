pub mod pr_merges;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use git2::{DiffOptions, Repository, Tree};

use crate::types::GitCommitInfo;

/// Opens the git repo at `repo_root`, walks commit history from HEAD,
/// and returns up to `max_count` commits with their changed files.
pub fn load_commits(repo_root: &Path, max_count: usize) -> Result<Vec<GitCommitInfo>> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;

    let mut revwalk = repo.revwalk().context("Failed to create revwalk")?;
    revwalk
        .push_head()
        .context("Failed to push HEAD to revwalk")?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut commits = Vec::new();

    for oid_result in revwalk {
        if commits.len() >= max_count {
            break;
        }
        let oid = oid_result.context("Failed to get commit oid from revwalk")?;
        let commit = repo.find_commit(oid).context("Failed to find commit")?;

        let info = commit_to_info(&repo, &commit)?;
        commits.push(info);
    }

    Ok(commits)
}

/// Searches commit messages for `query` (case-insensitive substring match)
/// and returns up to `max_count` matching commits.
pub fn search_commits(
    repo_root: &Path,
    query: &str,
    max_count: usize,
) -> Result<Vec<GitCommitInfo>> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;

    let mut revwalk = repo.revwalk().context("Failed to create revwalk")?;
    revwalk
        .push_head()
        .context("Failed to push HEAD to revwalk")?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let query_lower = query.to_lowercase();
    let mut commits = Vec::new();

    for oid_result in revwalk {
        if commits.len() >= max_count {
            break;
        }
        let oid = oid_result.context("Failed to get commit oid from revwalk")?;
        let commit = repo.find_commit(oid).context("Failed to find commit")?;

        let message = commit.message().unwrap_or("");
        if message.to_lowercase().contains(&query_lower) {
            let info = commit_to_info(&repo, &commit)?;
            commits.push(info);
        }
    }

    Ok(commits)
}

/// Returns up to `max_count` commits that modified the given `file_path`.
/// The `file_path` should be relative to the repository root.
pub fn file_history(
    repo_root: &Path,
    file_path: &Path,
    max_count: usize,
) -> Result<Vec<GitCommitInfo>> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;

    let mut revwalk = repo.revwalk().context("Failed to create revwalk")?;
    revwalk
        .push_head()
        .context("Failed to push HEAD to revwalk")?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut commits = Vec::new();

    for oid_result in revwalk {
        if commits.len() >= max_count {
            break;
        }
        let oid = oid_result.context("Failed to get commit oid from revwalk")?;
        let commit = repo.find_commit(oid).context("Failed to find commit")?;

        let changed = changed_files_for_commit(&repo, &commit)?;
        if changed.iter().any(|p| p == file_path) {
            let info = build_commit_info(&commit, changed);
            commits.push(info);
        }
    }

    Ok(commits)
}

/// Diffs two trees and returns the list of changed file paths.
/// If `old_tree` is `None`, all files in `new_tree` are considered added.
pub fn changed_files_between(
    repo: &Repository,
    old_tree: Option<&Tree>,
    new_tree: &Tree,
) -> Result<Vec<PathBuf>> {
    let mut opts = DiffOptions::new();
    let diff = repo
        .diff_tree_to_tree(old_tree, Some(new_tree), Some(&mut opts))
        .context("Failed to diff trees")?;

    let mut paths = Vec::new();
    diff.foreach(
        &mut |delta, _progress| {
            if let Some(path) = delta.new_file().path() {
                paths.push(path.to_path_buf());
            } else if let Some(path) = delta.old_file().path() {
                paths.push(path.to_path_buf());
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

/// Returns commits whose messages contain `[outcome:{outcome_id}]` tag.
pub fn search_by_outcome_tag(
    repo_root: &Path,
    outcome_id: &str,
    max_count: usize,
) -> Result<Vec<GitCommitInfo>> {
    let tag = format!("[outcome:{}]", outcome_id);
    search_commits(repo_root, &tag, max_count)
}

/// Returns commits that modified any file matching the given glob patterns.
/// Patterns are matched against the relative file path (e.g. "src/oh/*").
pub fn commits_touching_patterns(
    repo_root: &Path,
    patterns: &[String],
    max_count: usize,
) -> Result<Vec<GitCommitInfo>> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;

    let mut revwalk = repo.revwalk().context("Failed to create revwalk")?;
    revwalk
        .push_head()
        .context("Failed to push HEAD to revwalk")?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut commits = Vec::new();

    for oid_result in revwalk {
        if commits.len() >= max_count {
            break;
        }
        let oid = oid_result.context("Failed to get commit oid")?;
        let commit = repo.find_commit(oid).context("Failed to find commit")?;
        let changed = changed_files_for_commit(&repo, &commit)?;

        let matches = changed.iter().any(|f| {
            let f_str = f.to_string_lossy();
            patterns.iter().any(|pat| glob_match_public(pat, &f_str))
        });

        if matches {
            commits.push(build_commit_info(&commit, changed));
        }
    }

    Ok(commits)
}

/// Simple glob matching: supports `*` (any segment chars).
pub fn glob_match_public(pattern: &str, path: &str) -> bool {
    // Simple implementation: convert glob to a check
    // "src/oh/*" matches "src/oh/mod.rs"
    // "src/server.rs" matches exactly
    if !pattern.contains('*') {
        return path == pattern;
    }
    // Replace * with regex-like matching
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        // Single wildcard: prefix*suffix
        path.starts_with(parts[0]) && path.ends_with(parts[1])
    } else {
        // Fallback: just check prefix
        path.starts_with(parts[0])
    }
}

// ── shared utilities ────────────────────────────────────────────────

/// Returns the current HEAD commit OID as a hex string, or `None` if the
/// path is not a git repository or HEAD cannot be resolved.
///
/// Useful for cache-invalidation: enrichers can skip re-processing when
/// HEAD hasn't changed since the last run.
pub fn head_oid(repo_root: &Path) -> Option<String> {
    let repo = Repository::open(repo_root).ok()?;
    let head = repo.head().ok()?;
    head.target().map(|oid| oid.to_string())
}

// ── internal helpers ────────────────────────────────────────────────

/// Extracts changed files for a single commit by diffing against its first parent
/// (or against an empty tree for the root commit).
fn changed_files_for_commit(repo: &Repository, commit: &git2::Commit) -> Result<Vec<PathBuf>> {
    let new_tree = commit.tree().context("Failed to get commit tree")?;

    let parent_tree = if commit.parent_count() > 0 {
        let parent = commit.parent(0).context("Failed to get first parent")?;
        Some(parent.tree().context("Failed to get parent tree")?)
    } else {
        None
    };

    changed_files_between(repo, parent_tree.as_ref(), &new_tree)
}

/// Builds a `GitCommitInfo` from a `git2::Commit` and a pre-computed list of
/// changed files.
fn build_commit_info(commit: &git2::Commit, changed_files: Vec<PathBuf>) -> GitCommitInfo {
    let hash = commit.id().to_string();
    let short_hash = hash[..7.min(hash.len())].to_string();
    let message = commit.message().unwrap_or("").trim().to_string();
    let author = commit.author().name().unwrap_or("unknown").to_string();
    let timestamp = commit.time().seconds();

    GitCommitInfo {
        hash,
        short_hash,
        message,
        author,
        timestamp,
        changed_files,
    }
}

/// Convenience: converts a commit to `GitCommitInfo`, computing changed files
/// on the fly.
fn commit_to_info(repo: &Repository, commit: &git2::Commit) -> Result<GitCommitInfo> {
    let changed = changed_files_for_commit(repo, commit)?;
    Ok(build_commit_info(commit, changed))
}
