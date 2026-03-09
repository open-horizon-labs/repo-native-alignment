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
            walk_dir_git2(repo, &path, repo_root, extensions, files)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                    files.push(path);
                }
            }
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

        // Skip symlinks — they cause broken-link crashes and duplicate scanning
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if name_str == ".git" || name_str == "target" || name_str == "node_modules" || name_str == "vendor" || name_str == ".build" || name_str == "dist" {
                continue;
            }
            walk_dir_basic(&path, extensions, files)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                    files.push(path);
                }
            }
        }
    }
    Ok(())
}
