//! Incremental file scanner with mtime-based subtree skipping and git optimization.
//!
//! The scanner detects changed, new, and deleted files since the last scan.
//! It persists state to `.oh/.cache/scan-state.json` so subsequent scans
//! skip unchanged subtrees entirely.
//!
//! Git is an optimization layer, not a requirement. The scanner works on
//! arbitrary directories (no `.git` needed) using mtime-based detection.
//! When `.git` is present, `git2` provides precise changed-file lists
//! to narrow the scan.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Default excludes ────────────────────────────────────────────────

pub const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules/",
    ".venv/",
    "target/",
    "build/",
    "__pycache__/",
    ".git/",
    ".claude/",
    ".omp/",
    "dist/",
    "vendor/",
    ".build/",
    ".cache/",
    "*.pyc",
    "*.o",
    "*.so",
    "*.dylib",
    ".DS_Store",
];

/// Scanner configuration loaded from `.oh/config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanConfig {
    /// Additional exclude patterns (merged with defaults).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Patterns to remove from default excludes (opt back in).
    #[serde(default)]
    pub include: Vec<String>,
}

impl ScanConfig {
    /// Load from `.oh/config.toml` if it exists, otherwise return defaults.
    pub fn load(repo_root: &Path) -> Self {
        let config_path = repo_root.join(".oh").join("config.toml");
        match std::fs::read_to_string(&config_path) {
            Ok(content) => {
                // Parse [scanner] section from TOML
                match toml::from_str::<TomlConfig>(&content) {
                    Ok(parsed) => parsed.scanner.unwrap_or_default(),
                    Err(e) => {
                        tracing::warn!("Failed to parse {}: {}", config_path.display(), e);
                        Self::default()
                    }
                }
            }
            Err(_) => Self::default(),
        }
    }

    /// Merge config with default excludes to produce final exclude list.
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut excludes: Vec<String> = DEFAULT_EXCLUDES
            .iter()
            .filter(|d| !self.include.iter().any(|inc| inc == *d))
            .map(|s| s.to_string())
            .collect();
        for extra in &self.exclude {
            if !excludes.contains(extra) {
                excludes.push(extra.clone());
            }
        }
        excludes
    }
}

#[derive(Debug, Deserialize)]
struct TomlConfig {
    scanner: Option<ScanConfig>,
}

// ── Public types ────────────────────────────────────────────────────

/// Persistent scan state, serialized to `.oh/.cache/scan-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanState {
    /// Per-directory mtime at last scan (relative paths from repo root).
    #[serde(default)]
    pub dir_mtimes: HashMap<PathBuf, SystemTimeWrapper>,
    /// Per-file mtime at last scan (relative paths from repo root).
    #[serde(default)]
    pub file_mtimes: HashMap<PathBuf, SystemTimeWrapper>,
    /// Last indexed git commit SHA (if `.git` was present).
    #[serde(default)]
    pub last_commit_sha: Option<String>,
    /// Timestamp of last successful scan.
    #[serde(default)]
    pub last_scan: Option<SystemTimeWrapper>,
}

/// Wrapper for SystemTime that serializes as seconds since UNIX_EPOCH.
/// SystemTime doesn't implement Serialize/Deserialize natively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTimeWrapper(
    #[serde(
        serialize_with = "serialize_system_time",
        deserialize_with = "deserialize_system_time"
    )]
    pub SystemTime,
);

impl From<SystemTime> for SystemTimeWrapper {
    fn from(t: SystemTime) -> Self {
        SystemTimeWrapper(t)
    }
}

fn serialize_system_time<S: serde::Serializer>(
    time: &SystemTime,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeTuple;
    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let mut tup = serializer.serialize_tuple(2)?;
    tup.serialize_element(&duration.as_secs())?;
    tup.serialize_element(&duration.subsec_nanos())?;
    tup.end()
}

fn deserialize_system_time<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<SystemTime, D::Error> {
    let (secs, nanos): (u64, u32) = Deserialize::deserialize(deserializer)?;
    Ok(SystemTime::UNIX_EPOCH + Duration::new(secs, nanos))
}

/// Result of a scan operation.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// Files whose mtime changed since last scan, or found via git diff.
    pub changed_files: Vec<PathBuf>,
    /// Files not present in previous state (first scan or new files).
    pub new_files: Vec<PathBuf>,
    /// Files in previous state that no longer exist on disk.
    pub deleted_files: Vec<PathBuf>,
    /// Wall-clock duration of the scan.
    pub scan_duration: Duration,
}

impl ScanResult {
    /// All files that need (re-)extraction: changed + new.
    pub fn files_to_extract(&self) -> Vec<&PathBuf> {
        self.changed_files
            .iter()
            .chain(self.new_files.iter())
            .collect()
    }
}


/// The scanner: detects file changes incrementally.
pub struct Scanner {
    repo_root: PathBuf,
    excludes: Vec<String>,
    state: ScanState,
    /// Override for state persistence path. When `None`, uses the default
    /// `.oh/.cache/scan-state.json` under repo_root.
    custom_state_path: Option<PathBuf>,
}

impl Scanner {
    /// Create a new scanner for the given root directory.
    /// Loads exclude config from `.oh/config.toml` if it exists,
    /// and persisted state from `.oh/.cache/scan-state.json`.
    pub fn new(repo_root: PathBuf) -> Result<Self> {
        let config = ScanConfig::load(&repo_root);
        Self::with_excludes(repo_root, config.effective_excludes())
    }

    /// Create a scanner with custom exclude patterns.
    pub fn with_excludes(repo_root: PathBuf, excludes: Vec<String>) -> Result<Self> {
        let state = load_state(&repo_root).unwrap_or_default();
        Ok(Scanner {
            repo_root,
            excludes,
            state,
            custom_state_path: None,
        })
    }

    /// Create a scanner with custom excludes and a custom state persistence path.
    /// Used for multi-root workspace scanning where each root stores state separately.
    pub fn with_excludes_and_state_path(
        repo_root: PathBuf,
        excludes: Vec<String>,
        state_path_override: PathBuf,
    ) -> Result<Self> {
        let state = load_state_from_path(&state_path_override).unwrap_or_default();
        Ok(Scanner {
            repo_root,
            excludes,
            state,
            custom_state_path: Some(state_path_override),
        })
    }

    /// All files known to the scanner (from persisted state after scan).
    /// Use this to populate the graph on startup — the scan delta
    /// only returns changed files, but the graph needs everything.
    pub fn all_known_files(&self) -> Vec<PathBuf> {
        self.state.file_mtimes.keys().cloned().collect()
    }

    /// Perform an incremental scan. Returns changed/new/deleted file lists.
    ///
    /// Flow:
    /// 1. Load state (already done in constructor)
    /// 2. Git optimization: if `.git` present and last_commit_sha set,
    ///    get precise changed files via git2
    /// 3. mtime walk: compare directory mtimes, skip unchanged subtrees
    /// 4. Detect deleted files (in state but not on disk)
    /// 5. Save updated state
    /// 6. Return ScanResult
    pub fn scan(&mut self) -> Result<ScanResult> {
        let start = Instant::now();

        let mut changed_files = Vec::new();
        let mut new_files = Vec::new();

        // ── Step 2: Git optimization layer ──────────────────────────
        let git_changed = self.git_diff_changed();

        // ── Step 3: mtime walk ──────────────────────────────────────
        let mut new_dir_mtimes: HashMap<PathBuf, SystemTimeWrapper> = HashMap::new();
        let mut new_file_mtimes: HashMap<PathBuf, SystemTimeWrapper> = HashMap::new();

        self.walk_dir_mtime(
            &self.repo_root.clone(),
            &mut changed_files,
            &mut new_files,
            &mut new_dir_mtimes,
            &mut new_file_mtimes,
        )?;

        // Merge git-detected changes (may include files the mtime walk
        // already found — deduplicate via the file_mtimes map check).
        if let Some(git_files) = git_changed {
            for rel_path in git_files {
                let abs_path = self.repo_root.join(&rel_path);
                if abs_path.is_file() && !self.is_excluded(&rel_path) {
                    // If not already captured by mtime walk
                    if !changed_files.contains(&rel_path) && !new_files.contains(&rel_path) {
                        if self.state.file_mtimes.contains_key(&rel_path) {
                            changed_files.push(rel_path);
                        } else {
                            new_files.push(rel_path);
                        }
                    }
                }
            }
        }

        // ── Step 4: Detect deleted files ────────────────────────────
        let deleted_files: Vec<PathBuf> = self
            .state
            .file_mtimes
            .keys()
            .filter(|rel_path| {
                let abs = self.repo_root.join(rel_path);
                !abs.exists()
            })
            .cloned()
            .collect();

        // ── Step 5: Update and save state ───────────────────────────
        // Remove deleted files from new state
        for del in &deleted_files {
            new_file_mtimes.remove(del);
        }

        self.state.dir_mtimes = new_dir_mtimes;
        self.state.file_mtimes = new_file_mtimes;
        self.state.last_scan = Some(SystemTime::now().into());

        // Update last_commit_sha to current HEAD
        if let Ok(sha) = self.current_head_sha() {
            self.state.last_commit_sha = Some(sha);
        }

        if let Some(ref custom_path) = self.custom_state_path {
            save_state_to_path(custom_path, &self.state)?;
        } else {
            save_state(&self.repo_root, &self.state)?;
        }

        Ok(ScanResult {
            changed_files,
            new_files,
            deleted_files,
            scan_duration: start.elapsed(),
        })
    }

    /// Walk a directory tree with mtime-based subtree skipping.
    ///
    /// For each directory:
    /// - If its mtime matches stored state, skip the entire subtree.
    /// - If mtime changed (or not in state), enumerate files and compare
    ///   individual file mtimes.
    fn walk_dir_mtime(
        &self,
        dir: &Path,
        changed: &mut Vec<PathBuf>,
        new: &mut Vec<PathBuf>,
        new_dir_mtimes: &mut HashMap<PathBuf, SystemTimeWrapper>,
        new_file_mtimes: &mut HashMap<PathBuf, SystemTimeWrapper>,
    ) -> Result<()> {
        let rel_dir = dir
            .strip_prefix(&self.repo_root)
            .unwrap_or(dir)
            .to_path_buf();

        // Check exclude patterns for this directory
        if !rel_dir.as_os_str().is_empty() && self.is_excluded_dir(&rel_dir) {
            return Ok(());
        }

        let dir_mtime = dir_modified_time(dir)?;

        // Record this directory's mtime in new state
        if !rel_dir.as_os_str().is_empty() {
            new_dir_mtimes.insert(rel_dir.clone(), dir_mtime.into());
        }

        // Check if directory mtime is unchanged — skip subtree if so
        let dir_changed = if rel_dir.as_os_str().is_empty() {
            // Always descend into root
            true
        } else if let Some(stored) = self.state.dir_mtimes.get(&rel_dir) {
            dir_mtime != stored.0
        } else {
            // Directory not in previous state — it's new, must enumerate
            true
        };

        if !dir_changed {
            // Directory unchanged: no files added/removed in this dir. But file
            // contents may have been modified in place. Re-check file mtimes
            // while skipping subdirectories whose mtimes are also unchanged.
            self.carry_forward_subtree(dir, changed, new, new_dir_mtimes, new_file_mtimes)?;
            return Ok(());
        }

        // Directory mtime changed: enumerate contents
        let entries =
            fs::read_dir(dir).with_context(|| format!("read_dir: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;

            if ft.is_dir() {
                self.walk_dir_mtime(&path, changed, new, new_dir_mtimes, new_file_mtimes)?;
            } else if ft.is_file() || ft.is_symlink() {
                let rel_file = path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf();

                if self.is_excluded(&rel_file) {
                    continue;
                }

                let file_mtime = file_modified_time(&path)?;
                new_file_mtimes.insert(rel_file.clone(), file_mtime.into());

                if let Some(stored) = self.state.file_mtimes.get(&rel_file) {
                    if file_mtime != stored.0 {
                        changed.push(rel_file);
                    }
                } else {
                    new.push(rel_file);
                }
            }
        }

        Ok(())
    }

    /// When a directory mtime is unchanged, no files were added/removed in it.
    /// But file contents may have been modified in place (which changes file
    /// mtime but NOT directory mtime on Unix). So we still need to stat each
    /// file to detect content modifications.
    ///
    /// The optimization here: we skip subdirectories whose directory mtime
    /// is also unchanged, avoiding deep tree traversal when entire subtrees
    /// are untouched.
    fn carry_forward_subtree(
        &self,
        dir: &Path,
        changed: &mut Vec<PathBuf>,
        new: &mut Vec<PathBuf>,
        new_dir_mtimes: &mut HashMap<PathBuf, SystemTimeWrapper>,
        new_file_mtimes: &mut HashMap<PathBuf, SystemTimeWrapper>,
    ) -> Result<()> {
        let entries =
            fs::read_dir(dir).with_context(|| format!("read_dir: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;

            if ft.is_dir() {
                let rel_dir = path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf();

                if self.is_excluded_dir(&rel_dir) {
                    continue;
                }

                // Check if this subdirectory's mtime changed
                let subdir_mtime = dir_modified_time(&path)?;
                let subdir_changed = if let Some(stored) = self.state.dir_mtimes.get(&rel_dir) {
                    subdir_mtime != stored.0
                } else {
                    true
                };

                new_dir_mtimes.insert(rel_dir.clone(), subdir_mtime.into());

                if subdir_changed {
                    // Subdirectory changed: files may have been added/removed.
                    // Fall through to full enumeration via walk_dir_mtime.
                    self.walk_dir_mtime(&path, changed, new, new_dir_mtimes, new_file_mtimes)?;
                } else {
                    // Subdirectory also unchanged: recurse with same logic
                    self.carry_forward_subtree(
                        &path,
                        changed,
                        new,
                        new_dir_mtimes,
                        new_file_mtimes,
                    )?;
                }
            } else if ft.is_file() || ft.is_symlink() {
                let rel_file = path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf();

                if self.is_excluded(&rel_file) {
                    continue;
                }

                // Always re-check file mtime — content modifications change
                // file mtime without changing directory mtime.
                let file_mtime = file_modified_time(&path)?;
                new_file_mtimes.insert(rel_file.clone(), file_mtime.into());

                if let Some(stored) = self.state.file_mtimes.get(&rel_file) {
                    if file_mtime != stored.0 {
                        changed.push(rel_file);
                    }
                } else {
                    new.push(rel_file);
                }
            }
        }

        Ok(())
    }

    // ── Git optimization ────────────────────────────────────────────

    /// Use git2 to find files changed since last_commit_sha.
    /// Returns None if git is unavailable or no previous SHA is stored.
    fn git_diff_changed(&self) -> Option<Vec<PathBuf>> {
        let last_sha = self.state.last_commit_sha.as_ref()?;
        let repo = git2::Repository::open(&self.repo_root).ok()?;

        let old_oid = git2::Oid::from_str(last_sha).ok()?;
        let old_commit = repo.find_commit(old_oid).ok()?;
        let old_tree = old_commit.tree().ok()?;

        let head_ref = repo.head().ok()?;
        let head_commit = head_ref.peel_to_commit().ok()?;
        let head_tree = head_commit.tree().ok()?;

        if old_commit.id() == head_commit.id() {
            // No new commits
            return Some(Vec::new());
        }

        let mut opts = git2::DiffOptions::new();
        let diff = repo
            .diff_tree_to_tree(Some(&old_tree), Some(&head_tree), Some(&mut opts))
            .ok()?;

        let mut paths = Vec::new();
        diff.foreach(
            &mut |delta, _| {
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
        .ok()?;

        Some(paths)
    }

    /// Get the current HEAD commit SHA.
    fn current_head_sha(&self) -> Result<String> {
        let repo =
            git2::Repository::open(&self.repo_root).context("Failed to open git repository")?;
        let head = repo.head().context("Failed to get HEAD")?;
        let commit = head
            .peel_to_commit()
            .context("Failed to peel HEAD to commit")?;
        Ok(commit.id().to_string())
    }

    // ── Exclude matching ────────────────────────────────────────────

    /// Check if a relative file path matches any exclude pattern.
    fn is_excluded(&self, rel_path: &Path) -> bool {
        let path_str = rel_path.to_string_lossy();
        for pattern in &self.excludes {
            if is_file_excluded(pattern, &path_str) {
                return true;
            }
        }
        false
    }

    /// Check if a relative directory path matches any exclude pattern.
    fn is_excluded_dir(&self, rel_dir: &Path) -> bool {
        let dir_str = rel_dir.to_string_lossy();
        for pattern in &self.excludes {
            if is_dir_excluded(pattern, &dir_str) {
                return true;
            }
        }
        false
    }
}

// ── Exclude pattern matching ────────────────────────────────────────

/// Check if a file path matches an exclude pattern.
///
/// Pattern types:
/// - `dirname/` — matches any path component equal to `dirname`
/// - `*.ext` — matches files ending in `.ext`
/// - `filename` — matches the exact filename (last component)
fn is_file_excluded(pattern: &str, path: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        // Extension pattern: *.pyc, *.o, etc.
        return path.ends_with(suffix);
    }
    if let Some(dirname) = pattern.strip_suffix('/') {
        // Directory pattern: check if any path component matches
        return path.split('/').any(|comp| comp == dirname);
    }
    // Exact filename match against last component
    if let Some(filename) = path.rsplit('/').next() {
        return filename == pattern;
    }
    path == pattern
}

/// Check if a directory path matches an exclude pattern.
fn is_dir_excluded(pattern: &str, dir_path: &str) -> bool {
    if let Some(dirname) = pattern.strip_suffix('/') {
        // Directory pattern: any component matches
        return dir_path.split('/').any(|comp| comp == dirname);
    }
    false
}

// ── State persistence ───────────────────────────────────────────────

fn state_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("scan-state.json")
}

fn load_state(repo_root: &Path) -> Result<ScanState> {
    let path = state_path(repo_root);
    load_state_from_path(&path)
}

fn load_state_from_path(path: &Path) -> Result<ScanState> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("reading scan state from {}", path.display()))?;
    let state: ScanState = serde_json::from_str(&data).context("parsing scan-state.json")?;
    Ok(state)
}

fn save_state(repo_root: &Path, state: &ScanState) -> Result<()> {
    let path = state_path(repo_root);
    save_state_to_path(&path, state)
}

fn save_state_to_path(path: &Path, state: &ScanState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(state).context("serializing scan state")?;
    fs::write(path, data).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ── Filesystem helpers ──────────────────────────────────────────────

fn dir_modified_time(path: &Path) -> Result<SystemTime> {
    fs::metadata(path)
        .with_context(|| format!("metadata for {}", path.display()))?
        .modified()
        .with_context(|| format!("modified time for {}", path.display()))
}

fn file_modified_time(path: &Path) -> Result<SystemTime> {
    fs::metadata(path)
        .with_context(|| format!("metadata for {}", path.display()))?
        .modified()
        .with_context(|| format!("modified time for {}", path.display()))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use tempfile::TempDir;

    /// Helper: create a file with content, ensuring parent dirs exist.
    fn create_file(root: &Path, rel_path: &str, content: &str) {
        let path = root.join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    /// Helper: ensure mtime changes are detectable (some filesystems
    /// have 1-second mtime granularity).
    fn bump_mtime() {
        thread::sleep(Duration::from_millis(1100));
    }

    // ── Exclude pattern tests ───────────────────────────────────────

    #[test]
    fn test_exclude_directory_pattern() {
        assert!(is_file_excluded("node_modules/", "node_modules/foo/bar.js"));
        assert!(is_file_excluded(".git/", ".git/objects/abc"));
        assert!(is_file_excluded("target/", "some/deep/target/debug/build"));
        assert!(!is_file_excluded("node_modules/", "src/main.rs"));
    }

    #[test]
    fn test_exclude_extension_pattern() {
        assert!(is_file_excluded("*.pyc", "foo/bar.pyc"));
        assert!(is_file_excluded("*.o", "build/main.o"));
        assert!(is_file_excluded("*.so", "lib/thing.so"));
        assert!(!is_file_excluded("*.pyc", "foo/bar.py"));
    }

    #[test]
    fn test_exclude_filename_pattern() {
        assert!(is_file_excluded(".DS_Store", "some/dir/.DS_Store"));
        assert!(is_file_excluded(".DS_Store", ".DS_Store"));
        assert!(!is_file_excluded(".DS_Store", "src/main.rs"));
    }

    #[test]
    fn test_dir_excluded() {
        assert!(is_dir_excluded("node_modules/", "node_modules"));
        assert!(is_dir_excluded("target/", "some/target"));
        assert!(!is_dir_excluded("target/", "src"));
        assert!(!is_dir_excluded("*.pyc", "some_dir"));
    }

    // ── ScanState serialization roundtrip ───────────────────────────

    #[test]
    fn test_scan_state_roundtrip() {
        let mut state = ScanState::default();
        state
            .dir_mtimes
            .insert(PathBuf::from("src"), SystemTime::now().into());
        state
            .file_mtimes
            .insert(PathBuf::from("src/main.rs"), SystemTime::now().into());
        state.last_commit_sha = Some("abc123def456".to_string());
        state.last_scan = Some(SystemTime::now().into());

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: ScanState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.dir_mtimes.len(), 1);
        assert_eq!(restored.file_mtimes.len(), 1);
        assert_eq!(restored.last_commit_sha.as_deref(), Some("abc123def456"));
        assert!(restored.last_scan.is_some());
    }

    // ── First scan: everything is new ───────────────────────────────

    #[test]
    fn test_first_scan_all_new() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "src/lib.rs", "pub mod foo;");
        create_file(root, "README.md", "# Hello");
        // Create .oh directory (needed for state persistence)
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result = scanner.scan().unwrap();

        assert!(
            result.changed_files.is_empty(),
            "First scan should have no changed files"
        );
        assert_eq!(
            result.new_files.len(),
            3,
            "First scan should find 3 new files, found: {:?}",
            result.new_files
        );
        assert!(result.deleted_files.is_empty());

        // State should be persisted
        assert!(state_path(root).exists());
    }

    // ── Incremental scan: detect changes ────────────────────────────

    #[test]
    fn test_incremental_scan_detects_changes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "README.md", "# Hello");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result = scanner.scan().unwrap();
        assert_eq!(result.new_files.len(), 2);

        // Wait for mtime granularity, then modify a file
        bump_mtime();
        create_file(root, "src/main.rs", "fn main() { println!(\"hi\"); }");

        // Second scan — reload state from disk
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        assert!(
            result2
                .changed_files
                .contains(&PathBuf::from("src/main.rs")),
            "Should detect modified file. changed: {:?}, new: {:?}",
            result2.changed_files,
            result2.new_files,
        );
        assert!(result2.deleted_files.is_empty());
    }

    // ── Detect new files ────────────────────────────────────────────

    #[test]
    fn test_incremental_scan_detects_new_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();

        // Add a new file
        bump_mtime();
        create_file(root, "src/scanner.rs", "pub struct Scanner;");

        // Second scan
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        assert!(
            result2.new_files.contains(&PathBuf::from("src/scanner.rs")),
            "Should detect new file. new: {:?}",
            result2.new_files,
        );
    }

    // ── Detect deleted files ────────────────────────────────────────

    #[test]
    fn test_incremental_scan_detects_deleted_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "src/old.rs", "// will be deleted");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();

        // Delete a file
        fs::remove_file(root.join("src/old.rs")).unwrap();

        // Second scan
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        assert!(
            result2.deleted_files.contains(&PathBuf::from("src/old.rs")),
            "Should detect deleted file. deleted: {:?}",
            result2.deleted_files,
        );
    }

    // ── Excludes are respected ──────────────────────────────────────

    #[test]
    fn test_excludes_skip_directories() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "node_modules/pkg/index.js", "module.exports = {};");
        create_file(root, "target/debug/build.rs", "// build artifact");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result = scanner.scan().unwrap();

        // Only src/main.rs should appear (node_modules/ and target/ excluded)
        let all: Vec<_> = result.files_to_extract().into_iter().cloned().collect();
        assert_eq!(all.len(), 1, "Should only find 1 file, found: {:?}", all);
        assert!(all.contains(&PathBuf::from("src/main.rs")));
    }

    #[test]
    fn test_excludes_skip_file_patterns() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "src/.DS_Store", "binary junk");
        create_file(root, "lib/thing.so", "binary");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result = scanner.scan().unwrap();

        let all: Vec<_> = result.files_to_extract().into_iter().cloned().collect();
        assert_eq!(all.len(), 1, "Should only find 1 file, found: {:?}", all);
        assert!(all.contains(&PathBuf::from("src/main.rs")));
    }

    // ── mtime skip optimization ─────────────────────────────────────

    #[test]
    fn test_mtime_skip_unchanged_directory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "docs/guide.md", "# Guide");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result1 = scanner.scan().unwrap();
        assert_eq!(result1.new_files.len(), 2);

        // Second scan without any changes: nothing should appear
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        assert!(
            result2.changed_files.is_empty(),
            "Unchanged repo should have no changed files: {:?}",
            result2.changed_files
        );
        assert!(
            result2.new_files.is_empty(),
            "Unchanged repo should have no new files: {:?}",
            result2.new_files
        );
        assert!(result2.deleted_files.is_empty());
    }

    // ── Git diff integration (uses tempdir + git init) ──────────────

    #[test]
    fn test_git_diff_integration() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Initialize a git repo
        let repo = git2::Repository::init(root).unwrap();

        // Create initial file and commit
        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // Stage and commit
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("src/main.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let commit1 = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // First scan — captures HEAD sha
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result1 = scanner.scan().unwrap();
        assert_eq!(result1.new_files.len(), 1);

        // Make a new commit with a new file
        bump_mtime();
        create_file(root, "src/lib.rs", "pub mod scanner;");
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("src/lib.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.find_commit(commit1).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add lib", &tree, &[&parent])
            .unwrap();

        // Second scan — git diff should detect lib.rs
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        let all_detected: Vec<_> = result2
            .changed_files
            .iter()
            .chain(result2.new_files.iter())
            .collect();
        assert!(
            all_detected.iter().any(|p| p.ends_with("lib.rs")),
            "Git diff should detect new file. changed: {:?}, new: {:?}",
            result2.changed_files,
            result2.new_files,
        );
    }

    // ── State persistence location ──────────────────────────────────

    #[test]
    fn test_state_persistence_creates_cache_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Don't pre-create .oh/.cache — scanner should create it
        create_file(root, "hello.txt", "world");
        fs::create_dir_all(root.join(".oh")).unwrap();

        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();

        assert!(root.join(".oh/.cache/scan-state.json").exists());
    }

    #[test]
    fn test_files_to_extract_helper() {
        let result = ScanResult {
            changed_files: vec![PathBuf::from("a.rs")],
            new_files: vec![PathBuf::from("b.rs"), PathBuf::from("c.rs")],
            deleted_files: vec![PathBuf::from("old.rs")],
            scan_duration: Duration::from_millis(10),
        };

        let to_extract = result.files_to_extract();
        assert_eq!(to_extract.len(), 3);
    }
}
