use std::path::{Path, PathBuf};
use anyhow::Result;

/// Walk the repo directory tree, respecting .gitignore rules.
/// Returns files matching the given extension filter.
/// Falls back to basic walk (skipping .git/ and target/) if git2 fails.
pub fn walk_repo_files(repo_root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    // Try git2-based walk first (respects .gitignore)
    match git2_walk(repo_root, extensions) {
        Ok(files) => Ok(files),
        Err(e) => {
            tracing::warn!("git2 walk failed, falling back to basic walk: {}", e);
            basic_walk(repo_root, extensions)
        }
    }
}

/// Returns `true` if `dir` is a git worktree that maintains its own RNA cache.
///
/// A directory is considered an independently-indexed worktree when both:
/// 1. `dir/.git` is a **file** (the pointer git writes for linked worktrees), and
/// 2. `dir/.oh/.cache/lance/` exists (an agent is maintaining a separate index).
///
/// Either condition alone is insufficient: condition 1 would skip all worktrees
/// regardless of whether they have their own index; condition 2 could match a
/// legitimately separate project that happens to live under the repo root.
///
/// The check is intentionally just two `stat` calls — no git subprocess needed.
pub fn is_worktree_with_own_cache(dir: &Path) -> bool {
    // Condition 1: .git must be a file (worktree pointer), not a directory.
    let git_path = dir.join(".git");
    match std::fs::metadata(&git_path) {
        Ok(m) if m.is_file() => {}
        _ => return false,
    }

    // Condition 2: the worktree must have its own RNA LanceDB cache directory.
    let cache_path = dir.join(".oh").join(".cache").join("lance");
    cache_path.is_dir()
}

fn git2_walk(repo_root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let repo = git2::Repository::open(repo_root)?;
    let mut files = Vec::new();
    walk_dir_git2(&repo, repo_root, repo_root, extensions, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_dir_git2(
    repo: &git2::Repository,
    dir: &Path,
    repo_root: &Path,
    extensions: &[&str],
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Always skip .git
        if name_str == ".git" {
            continue;
        }

        // Check if git ignores this path
        let rel = path.strip_prefix(repo_root).unwrap_or(&path);
        if repo.is_path_ignored(rel).unwrap_or(false) {
            continue;
        }

        if path.is_dir() {
            // Skip subdirectories that are independently-indexed git worktrees.
            if is_worktree_with_own_cache(&path) {
                tracing::info!(
                    "skipping worktree {}: has own RNA cache",
                    path.display()
                );
                continue;
            }
            walk_dir_git2(repo, &path, repo_root, extensions, files)?;
        } else if path.is_file()
            && let Some(ext) = path.extension().and_then(|e| e.to_str())
                && extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                    files.push(path);
                }
    }
    Ok(())
}

fn basic_walk(repo_root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir_basic(repo_root, extensions, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_dir_basic(dir: &Path, extensions: &[&str], files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip symlinks — broken symlinks crash fs::metadata, and valid symlinks
        // point to paths that will be scanned via their real location anyway.
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if name_str == ".git" || name_str == "target" || name_str == "node_modules" || name_str == "vendor" || name_str == ".build" || name_str == "dist" {
                continue;
            }
            // Skip subdirectories that are independently-indexed git worktrees.
            if is_worktree_with_own_cache(&path) {
                tracing::info!(
                    "skipping worktree {}: has own RNA cache",
                    path.display()
                );
                continue;
            }
            walk_dir_basic(&path, extensions, files)?;
        } else if path.is_file()
            && let Some(ext) = path.extension().and_then(|e| e.to_str())
                && extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                    files.push(path);
                }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_is_worktree_with_own_cache_both_conditions_required() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // Neither condition: not a worktree, no cache.
        assert!(!is_worktree_with_own_cache(path));

        // Only condition 1: .git is a file but no cache.
        fs::write(path.join(".git"), "gitdir: ../main/.git/worktrees/wt1\n").unwrap();
        assert!(!is_worktree_with_own_cache(path));

        // Both conditions: .git is a file AND cache exists.
        let cache = path.join(".oh").join(".cache").join("lance");
        fs::create_dir_all(&cache).unwrap();
        assert!(is_worktree_with_own_cache(path));
    }

    #[test]
    fn test_is_worktree_with_own_cache_git_dir_not_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // .git is a directory (main repo, not a worktree) — must not skip.
        fs::create_dir(path.join(".git")).unwrap();
        let cache = path.join(".oh").join(".cache").join("lance");
        fs::create_dir_all(&cache).unwrap();
        assert!(!is_worktree_with_own_cache(path));
    }

    #[test]
    fn test_is_worktree_with_own_cache_nonexistent_dir() {
        let path = Path::new("/nonexistent/path/that/does/not/exist");
        assert!(!is_worktree_with_own_cache(path));
    }
}
