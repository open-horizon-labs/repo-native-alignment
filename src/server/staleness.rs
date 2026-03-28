//! Index staleness detection.
//!
//! Computes a staleness hint for MCP tool responses so agents know when
//! the index is behind the current codebase state. Two strategies:
//!
//! - **Git roots:** Compare `last_commit_sha` from scan-state.json against
//!   the current HEAD. Report commits behind via `git2` rev-walk.
//! - **Non-git roots:** Compare `last_scan` timestamp against current time.
//!   Report elapsed duration when significantly stale.
//!
//! Respects the `git-is-optimization-not-requirement` guardrail: git is
//! used when available, but non-git directories get time-based staleness.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::scanner::ScanState;

/// Threshold for non-git roots: only show staleness if older than this.
const NON_GIT_STALE_THRESHOLD: Duration = Duration::from_secs(30 * 60); // 30 minutes

/// Check index staleness for a given repo root.
///
/// Returns `Some(hint)` with a human-readable warning when the index is stale,
/// or `None` when the index is fresh (no noise).
///
/// This function is intentionally infallible: if scan state cannot be read or
/// git operations fail, it returns `None` rather than propagating errors. Tool
/// responses should not break because of staleness detection failures.
pub fn check_staleness(repo_root: &Path) -> Option<String> {
    let state_path = repo_root.join(".oh").join(".cache").join("scan-state.json");
    let state = load_scan_state(&state_path)?;

    // Try git-based staleness first.
    if let Some(hint) = check_git_staleness(repo_root, &state) {
        return Some(hint);
    }

    // Fall back to time-based staleness for non-git roots.
    check_time_staleness(&state)
}

/// Load scan state from disk. Returns `None` on any error (file missing,
/// parse failure, etc.) — staleness detection is best-effort.
fn load_scan_state(path: &Path) -> Option<ScanState> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Git-based staleness: compare indexed commit SHA against HEAD.
///
/// Returns `None` if:
/// - The repo is not a git repository
/// - No `last_commit_sha` was recorded (first scan, or non-git root)
/// - The indexed SHA matches HEAD (fresh)
/// - Any git2 operation fails (graceful degradation)
fn check_git_staleness(repo_root: &Path, state: &ScanState) -> Option<String> {
    let indexed_sha = state.last_commit_sha.as_deref()?;

    let repo = git2::Repository::open(repo_root).ok()?;
    let head = repo.head().ok()?;
    let head_commit = head.peel_to_commit().ok()?;
    let head_sha = head_commit.id().to_string();

    if head_sha == indexed_sha {
        return None; // fresh
    }

    // Count commits between indexed SHA and HEAD via rev-walk.
    let commits_behind = count_commits_between(&repo, indexed_sha, &head_sha);

    Some(format!(
        "\n\n**Warning: Index is {} commit{} behind HEAD.** Restart server or re-scan to update.",
        commits_behind,
        if commits_behind == 1 { "" } else { "s" },
    ))
}

/// Count commits reachable from `new_sha` that are not reachable from `old_sha`.
///
/// This is equivalent to `git rev-list --count old_sha..new_sha`.
/// Returns 1 as a minimum if the SHAs differ but the walk fails or returns 0,
/// since we already know they differ.
fn count_commits_between(repo: &git2::Repository, old_sha: &str, new_sha: &str) -> usize {
    let count = (|| -> Option<usize> {
        let new_oid = git2::Oid::from_str(new_sha).ok()?;
        let old_oid = git2::Oid::from_str(old_sha).ok()?;

        let mut revwalk = repo.revwalk().ok()?;
        revwalk.push(new_oid).ok()?;
        revwalk.hide(old_oid).ok()?;

        Some(revwalk.count())
    })();

    // If SHAs differ, at least 1 commit behind even if walk fails.
    count.unwrap_or(1).max(1)
}

/// Time-based staleness for non-git roots (or git roots without `last_commit_sha`).
///
/// Returns a hint when the last scan was more than 30 minutes ago.
fn check_time_staleness(state: &ScanState) -> Option<String> {
    let last_scan = state.last_scan.as_ref()?;
    let elapsed = SystemTime::now()
        .duration_since(last_scan.0)
        .unwrap_or_default();

    if elapsed < NON_GIT_STALE_THRESHOLD {
        return None; // fresh enough
    }

    let formatted = format_duration(elapsed);
    Some(format!(
        "\n\n**Warning: Index last updated {} ago.** Re-scan to update.",
        formatted,
    ))
}

/// Format a duration as a human-readable string (e.g., "2h 14m", "45m").
fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;

    if hours > 0 && minutes > 0 {
        format!("{}h {}m", hours, minutes)
    } else if hours > 0 {
        format!("{}h", hours)
    } else {
        format!("{}m", minutes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::SystemTimeWrapper;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn write_scan_state(dir: &Path, state: &ScanState) {
        let cache_dir = dir.join(".oh").join(".cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let data = serde_json::to_string_pretty(state).unwrap();
        std::fs::write(cache_dir.join("scan-state.json"), data).unwrap();
    }

    #[test]
    fn test_no_scan_state_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(check_staleness(tmp.path()).is_none());
    }

    #[test]
    fn test_non_git_fresh_returns_none() {
        let tmp = TempDir::new().unwrap();
        let state = ScanState {
            last_scan: Some(SystemTimeWrapper::from(SystemTime::now())),
            ..Default::default()
        };
        write_scan_state(tmp.path(), &state);
        assert!(check_staleness(tmp.path()).is_none());
    }

    #[test]
    fn test_non_git_stale_returns_hint() {
        let tmp = TempDir::new().unwrap();
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3600 + 14 * 60);
        let state = ScanState {
            last_scan: Some(SystemTimeWrapper::from(two_hours_ago)),
            ..Default::default()
        };
        write_scan_state(tmp.path(), &state);
        let hint = check_staleness(tmp.path()).expect("should return hint");
        assert!(hint.contains("Index last updated"), "got: {}", hint);
        assert!(hint.contains("2h 14m"), "got: {}", hint);
    }

    #[test]
    fn test_non_git_no_last_scan_returns_none() {
        let tmp = TempDir::new().unwrap();
        let state = ScanState::default();
        write_scan_state(tmp.path(), &state);
        assert!(check_staleness(tmp.path()).is_none());
    }

    #[test]
    fn test_git_fresh_returns_none() {
        // Create a git repo with a commit, then write scan state with that SHA.
        let tmp = TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let state = ScanState {
            last_commit_sha: Some(commit_oid.to_string()),
            last_scan: Some(SystemTimeWrapper::from(SystemTime::now())),
            ..Default::default()
        };
        write_scan_state(tmp.path(), &state);

        assert!(check_staleness(tmp.path()).is_none());
    }

    #[test]
    fn test_git_stale_returns_commit_count() {
        // Create a git repo with 3 commits; index at first commit.
        let tmp = TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        // Commit 1 (indexed)
        let c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
            .unwrap();
        let c1_commit = repo.find_commit(c1).unwrap();

        // Commit 2
        let c2 = repo
            .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&c1_commit])
            .unwrap();
        let c2_commit = repo.find_commit(c2).unwrap();

        // Commit 3 (HEAD)
        repo.commit(Some("HEAD"), &sig, &sig, "third", &tree, &[&c2_commit])
            .unwrap();

        let state = ScanState {
            last_commit_sha: Some(c1.to_string()),
            last_scan: Some(SystemTimeWrapper::from(SystemTime::now())),
            ..Default::default()
        };
        write_scan_state(tmp.path(), &state);

        let hint = check_staleness(tmp.path()).expect("should return hint");
        assert!(hint.contains("2 commits behind HEAD"), "got: {}", hint);
    }

    #[test]
    fn test_format_duration_hours_and_minutes() {
        assert_eq!(format_duration(Duration::from_secs(7440)), "2h 4m");
    }

    #[test]
    fn test_format_duration_hours_only() {
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h");
    }

    #[test]
    fn test_format_duration_minutes_only() {
        assert_eq!(format_duration(Duration::from_secs(2700)), "45m");
    }

    #[test]
    fn test_count_commits_returns_minimum_1() {
        // If old_sha is garbage, count should still be at least 1.
        let tmp = TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
            .unwrap();
        // Use a bogus old SHA
        let count = count_commits_between(&repo, "0000000000000000000000000000000000000000", &c1.to_string());
        assert!(count >= 1, "count should be at least 1, got {}", count);
    }
}
