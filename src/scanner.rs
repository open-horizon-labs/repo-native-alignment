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
    "target*/",
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
    "*.rmeta",
    "*.rlib",
    "*.bc",
    "*.a",
    "*.dll",
    "*.exe",
    "*.wasm",
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

    /// Merge config with a base exclude list to produce the final exclude list.
    fn apply_to_base_excludes(&self, mut excludes: Vec<String>) -> Vec<String> {
        let include_set: std::collections::HashSet<&str> = self.include.iter()
            .map(|s| s.as_str())
            .collect();
        excludes.retain(|pattern| !include_set.contains(pattern.as_str()));
        let mut exclude_set: std::collections::HashSet<String> = excludes.iter()
            .cloned()
            .collect();
        for extra in &self.exclude {
            if exclude_set.insert(extra.clone()) {
                excludes.push(extra.clone());
            }
        }
        excludes
    }

    /// Merge config with default excludes to produce final exclude list.
    pub fn effective_excludes(&self) -> Vec<String> {
        let excludes: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        self.apply_to_base_excludes(excludes)
    }
}

// ── Pattern hint configuration ──────────────────────────────────────

/// Configuration for design pattern detection via naming conventions.
///
/// Loaded from `.oh/config.toml` under `[patterns]`. Controls which
/// suffix/hint pairs are used by `detect_pattern_hint` to annotate
/// symbols with `metadata["pattern_hint"]`.
///
/// # Example `.oh/config.toml`
///
/// ```toml
/// [patterns]
/// # Add custom suffix -> hint mappings (merged with built-in defaults)
/// extra = [
///   ["gateway", "gateway"],
///   ["interactor", "interactor"],
///   ["usecase", "use_case"],
/// ]
/// # Disable specific built-in patterns by hint name
/// disable = ["manager", "service"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatternConfig {
    /// Additional suffix -> hint pairs to detect beyond the built-in defaults.
    /// Each entry is `[suffix, hint]` where suffix is matched case-insensitively.
    #[serde(default)]
    pub extra: Vec<[String; 2]>,
    /// Built-in pattern hints to disable (by hint name, e.g. "manager").
    #[serde(default)]
    pub disable: Vec<String>,
}

/// Built-in pattern suffixes shipped with RNA. These are the defaults
/// from PR #184 that apply when no `.oh/config.toml` exists.
pub const DEFAULT_PATTERN_SUFFIXES: &[(&str, &str)] = &[
    ("factory", "factory"),
    ("builder", "builder"),
    ("handler", "handler"),
    ("adapter", "adapter"),
    ("proxy", "proxy"),
    ("observer", "observer"),
    ("repository", "repository"),
    ("strategy", "strategy"),
    ("singleton", "singleton"),
    ("decorator", "decorator"),
    ("middleware", "middleware"),
    ("provider", "provider"),
    ("service", "service"),
    ("controller", "controller"),
    ("manager", "manager"),
];

impl PatternConfig {
    /// Load from `.oh/config.toml` if it exists, otherwise return defaults.
    pub fn load(repo_root: &Path) -> Self {
        let config_path = repo_root.join(".oh").join("config.toml");
        match std::fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str::<TomlConfig>(&content) {
                Ok(parsed) => parsed.patterns.unwrap_or_default(),
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", config_path.display(), e);
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Compute the effective suffix list: built-in defaults minus disabled,
    /// plus extra custom patterns.
    pub fn effective_suffixes(&self) -> Vec<(String, String)> {
        // Normalize disable list once for case-insensitive comparison.
        let disable_lower: std::collections::HashSet<String> = self.disable.iter()
            .map(|d| d.to_ascii_lowercase())
            .collect();

        let mut seen_suffixes: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut suffixes: Vec<(String, String)> = DEFAULT_PATTERN_SUFFIXES
            .iter()
            .filter(|(_, hint)| !disable_lower.contains(*hint))
            .map(|(suffix, hint)| {
                seen_suffixes.insert(suffix.to_string());
                (suffix.to_string(), hint.to_string())
            })
            .collect();

        for pair in &self.extra {
            let suffix = pair[0].to_ascii_lowercase();
            let hint = pair[1].to_ascii_lowercase();
            // Skip if disabled (disable applies to extras too, not just built-ins)
            if disable_lower.contains(&hint) {
                continue;
            }
            // Avoid duplicates: skip if suffix already present
            if seen_suffixes.insert(suffix.clone()) {
                suffixes.push((suffix, hint));
            }
        }

        suffixes
    }
}

/// Minimum LSP diagnostic severity to store as graph nodes.
///
/// Corresponds to LSP DiagnosticSeverity integers:
///   1 = Error, 2 = Warning, 3 = Information, 4 = Hint
///
/// The variant name is the floor — all severities with integer ≤ the floor are stored.
/// Default is `Warning` (store Error + Warning, filter Information + Hint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticMinSeverity {
    /// Store only errors (severity 1).
    Error,
    /// Store errors and warnings (severity ≤ 2). **Default.**
    Warning,
    /// Store errors, warnings, and information (severity ≤ 3).
    Information,
    /// Store all diagnostics including hints (severity ≤ 4).
    Hint,
}

impl Default for DiagnosticMinSeverity {
    fn default() -> Self {
        Self::Warning
    }
}

impl DiagnosticMinSeverity {
    /// Return the maximum LSP severity integer that should be stored.
    ///
    /// LSP encodes severity as ascending integers where 1 = most severe.
    /// A diagnostic is kept when `severity_int <= self.max_severity_int()`.
    pub fn max_severity_int(&self) -> u64 {
        match self {
            Self::Error => 1,
            Self::Warning => 2,
            Self::Information => 3,
            Self::Hint => 4,
        }
    }
}

/// LSP-specific configuration loaded from `.oh/config.toml` under `[lsp]`.
///
/// # Example `.oh/config.toml`
///
/// ```toml
/// [lsp]
/// # "error"       — errors only
/// # "warning"     — errors + warnings (default)
/// # "information" — errors + warnings + information
/// # "hint"        — all diagnostics
/// diagnostic_min_severity = "hint"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LspConfig {
    /// Minimum severity to store as diagnostic nodes. Defaults to `Warning`.
    #[serde(default)]
    pub diagnostic_min_severity: DiagnosticMinSeverity,
}

impl LspConfig {
    /// Load from `.oh/config.toml` if it exists, otherwise return defaults.
    pub fn load(repo_root: &Path) -> Self {
        let config_path = repo_root.join(".oh").join("config.toml");
        match std::fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str::<TomlConfig>(&content) {
                Ok(parsed) => parsed.lsp.unwrap_or_default(),
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", config_path.display(), e);
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TomlConfig {
    scanner: Option<ScanConfig>,
    patterns: Option<PatternConfig>,
    workspace: Option<WorkspaceSection>,
    lsp: Option<LspConfig>,
}

/// The `[workspace]` table in `.oh/config.toml`.
#[derive(Debug, Deserialize, Default)]
struct WorkspaceSection {
    /// Slug → path mapping under `[workspace.roots]`.
    #[serde(default)]
    roots: std::collections::HashMap<String, String>,
}

/// Load the `[workspace.roots]` entries from `<repo_root>/.oh/config.toml`.
///
/// Returns a `Vec<(slug, path)>` in the order they appear in the TOML file
/// (HashMap iteration is arbitrary, but stable enough for deterministic
/// registration since each slug is unique within the map).
///
/// Returns an empty vec if the file does not exist or has no `[workspace.roots]`.
pub fn load_declared_roots(repo_root: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let config_path = repo_root.join(".oh").join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    match toml::from_str::<TomlConfig>(&content) {
        Ok(parsed) => {
            let section = match parsed.workspace {
                Some(w) => w,
                None => return Vec::new(),
            };
            section
                .roots
                .into_iter()
                .map(|(slug, path_str)| (slug, std::path::PathBuf::from(path_str)))
                .collect()
        }
        Err(e) => {
            tracing::warn!(
                "Failed to parse [workspace.roots] in {}: {}",
                config_path.display(),
                e
            );
            Vec::new()
        }
    }
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
    /// BLAKE3 content hashes per file (hex-encoded). Used to detect mtime
    /// false positives: if mtime changed but content hash is identical,
    /// the file is not truly changed and extraction can be skipped.
    #[serde(default)]
    pub file_content_hashes: HashMap<PathBuf, String>,
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
        let excludes: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        Self::with_excludes(repo_root, excludes)
    }

    /// Create a scanner with custom exclude patterns.
    pub fn with_excludes(repo_root: PathBuf, excludes: Vec<String>) -> Result<Self> {
        let config = ScanConfig::load(&repo_root);
        let excludes = config.apply_to_base_excludes(excludes);
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
        let config = ScanConfig::load(&repo_root);
        let excludes = config.apply_to_base_excludes(excludes);
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

    /// Persist the scanner's in-memory state to disk.
    ///
    /// Call this **after** the caller has successfully processed the scan results
    /// (e.g., graph update + LanceDB persist). This ensures that if processing
    /// fails, the next scan will re-detect the same changes instead of silently
    /// losing them.
    pub fn commit_state(&self) -> Result<()> {
        if let Some(ref custom_path) = self.custom_state_path {
            save_state_to_path(custom_path, &self.state)?;
        } else {
            save_state(&self.repo_root, &self.state)?;
        }
        Ok(())
    }

    /// Perform an incremental scan. Returns changed/new/deleted file lists.
    ///
    /// The scan updates internal state (mtimes, hashes) in memory but does
    /// **not** persist to disk. The caller must call [`commit_state()`] after
    /// successfully processing the results to persist the state. This ensures
    /// that if processing fails, the next scan re-detects the same changes.
    ///
    /// Flow:
    /// 1. Load state (already done in constructor)
    /// 2. Git optimization: if `.git` present and last_commit_sha set,
    ///    get precise changed files via git2
    /// 3. mtime walk: compare directory mtimes, skip unchanged subtrees
    /// 4. Detect deleted files (in state but not on disk)
    /// 5. Return ScanResult (state NOT persisted -- caller must commit)
    pub fn scan(&mut self) -> Result<ScanResult> {
        let start = Instant::now();

        tracing::info!(
            "Scanner: starting incremental scan for {}",
            self.repo_root.display()
        );
        tracing::debug!(
            "Scanner: active excludes ({}): {:?}",
            self.excludes.len(),
            self.excludes
        );

        let mut changed_files = Vec::new();
        let mut new_files = Vec::new();

        // ── Step 2: Git optimization layer ──────────────────────────
        let git_changed = self.git_diff_changed();
        match &git_changed {
            Some(paths) if paths.is_empty() => {
                tracing::debug!("Scanner: git diff found no changed files");
            }
            Some(paths) => {
                tracing::debug!(
                    "Scanner: git diff yielded {} changed path(s): {:?}",
                    paths.len(),
                    paths
                );
            }
            None => {
                tracing::debug!(
                    "Scanner: git diff optimization unavailable for {}",
                    self.repo_root.display()
                );
            }
        }

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
                            tracing::debug!(
                                "Scanner: git diff marked changed file {}",
                                rel_path.display()
                            );
                            changed_files.push(rel_path);
                        } else {
                            tracing::debug!(
                                "Scanner: git diff marked new file {}",
                                rel_path.display()
                            );
                            new_files.push(rel_path);
                        }
                    }
                }
            }
        }

        // ── Step 3b: BLAKE3 content hash to filter mtime false positives ──
        let mut new_content_hashes: HashMap<PathBuf, String> = HashMap::new();
        let pre_hash_changed_count = changed_files.len();

        // Hash changed files: if content hash matches previous, it's a false positive
        changed_files.retain(|rel_path| {
            let abs_path = self.repo_root.join(rel_path);
            match fs::read(&abs_path) {
                Ok(bytes) => {
                    let hash = blake3::hash(&bytes).to_hex().to_string();
                    let is_truly_changed = self
                        .state
                        .file_content_hashes
                        .get(rel_path)
                        .is_none_or(|prev| *prev != hash);
                    new_content_hashes.insert(rel_path.clone(), hash);
                    if !is_truly_changed {
                        tracing::debug!(
                            "Scanner: content hash unchanged, skipping {}",
                            rel_path.display()
                        );
                    }
                    is_truly_changed
                }
                Err(e) => {
                    tracing::warn!(
                        "Scanner: failed to read {} for content hash: {}",
                        rel_path.display(),
                        e
                    );
                    true // assume changed if we can't read
                }
            }
        });

        let hash_filtered = pre_hash_changed_count - changed_files.len();
        if hash_filtered > 0 {
            tracing::info!(
                "Scanner: BLAKE3 content hash filtered {} mtime false positive(s)",
                hash_filtered
            );
        }

        // Hash new files for future comparisons
        for rel_path in &new_files {
            let abs_path = self.repo_root.join(rel_path);
            if let Ok(bytes) = fs::read(&abs_path) {
                let hash = blake3::hash(&bytes).to_hex().to_string();
                new_content_hashes.insert(rel_path.clone(), hash);
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
        if !deleted_files.is_empty() {
            tracing::debug!(
                "Scanner: detected {} deleted file(s): {:?}",
                deleted_files.len(),
                deleted_files
            );
        }

        // ── Step 5: Update and save state ───────────────────────────
        // Remove deleted files from new state
        for del in &deleted_files {
            new_file_mtimes.remove(del);
            new_content_hashes.remove(del);
        }

        // Carry forward content hashes for unchanged files (not in changed or new)
        for (path, hash) in &self.state.file_content_hashes {
            if !new_content_hashes.contains_key(path) && new_file_mtimes.contains_key(path) {
                new_content_hashes.insert(path.clone(), hash.clone());
            }
        }

        self.state.dir_mtimes = new_dir_mtimes;
        self.state.file_mtimes = new_file_mtimes;
        self.state.file_content_hashes = new_content_hashes;
        self.state.last_scan = Some(SystemTime::now().into());

        // Update last_commit_sha to current HEAD
        if let Ok(sha) = self.current_head_sha() {
            self.state.last_commit_sha = Some(sha);
        }

        // NOTE: State is NOT persisted here. The caller must call
        // `commit_state()` after successfully processing the scan results.
        // This prevents silent data loss if graph update fails.

        let scan_duration = start.elapsed();
        tracing::info!(
            "Scanner: completed scan for {} in {:?} ({} new, {} changed, {} deleted, {} hash-skipped, {} tracked files)",
            self.repo_root.display(),
            scan_duration,
            new_files.len(),
            changed_files.len(),
            deleted_files.len(),
            hash_filtered,
            self.state.file_mtimes.len()
        );

        Ok(ScanResult {
            changed_files,
            new_files,
            deleted_files,
            scan_duration,
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
        let dir_start = Instant::now();
        let rel_dir = dir
            .strip_prefix(&self.repo_root)
            .unwrap_or(dir)
            .to_path_buf();
        let rel_dir_display = if rel_dir.as_os_str().is_empty() {
            String::from(".")
        } else {
            rel_dir.display().to_string()
        };

        // Check exclude patterns for this directory
        if !rel_dir.as_os_str().is_empty() && self.is_excluded_dir(&rel_dir) {
            tracing::debug!("Scanner: skipping excluded directory {}", rel_dir_display);
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
            tracing::debug!(
                "Scanner: directory unchanged, carrying subtree {}",
                rel_dir_display
            );
            // Directory unchanged: no files added/removed in this dir. But file
            // contents may have been modified in place. Re-check file mtimes
            // while skipping subdirectories whose mtimes are also unchanged.
            self.carry_forward_subtree(dir, changed, new, new_dir_mtimes, new_file_mtimes)?;
            tracing::debug!(
                "Scanner: carried subtree {} in {:?}",
                rel_dir_display,
                dir_start.elapsed()
            );
            return Ok(());
        }

        tracing::debug!("Scanner: enumerating directory {}", rel_dir_display);

        // Directory mtime changed: enumerate contents
        let entries =
            fs::read_dir(dir).with_context(|| format!("read_dir: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;

            if ft.is_dir() {
                self.walk_dir_mtime(&path, changed, new, new_dir_mtimes, new_file_mtimes)?;
            } else if ft.is_file() {
                let file_start = Instant::now();
                let rel_file = path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf();

                if self.is_excluded(&rel_file) {
                    tracing::debug!(
                        "Scanner: skipping excluded file {}",
                        rel_file.display()
                    );
                    continue;
                }

                let file_mtime = file_modified_time(&path)?;
                new_file_mtimes.insert(rel_file.clone(), file_mtime.into());

                let status = if let Some(stored) = self.state.file_mtimes.get(&rel_file) {
                    if file_mtime != stored.0 {
                        changed.push(rel_file.clone());
                        "changed"
                    } else {
                        "unchanged"
                    }
                } else {
                    new.push(rel_file.clone());
                    "new"
                };

                tracing::debug!(
                    "Scanner: {} file {} in {:?}",
                    status,
                    rel_file.display(),
                    file_start.elapsed()
                );
            }
        }

        tracing::debug!(
            "Scanner: finished directory {} in {:?}",
            rel_dir_display,
            dir_start.elapsed()
        );

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
        let subtree_start = Instant::now();
        let rel_dir = dir
            .strip_prefix(&self.repo_root)
            .unwrap_or(dir)
            .to_path_buf();
        let rel_dir_display = if rel_dir.as_os_str().is_empty() {
            String::from(".")
        } else {
            rel_dir.display().to_string()
        };
        tracing::debug!(
            "Scanner: rechecking unchanged subtree {}",
            rel_dir_display
        );

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
                    tracing::debug!(
                        "Scanner: skipping excluded directory {} during subtree carry",
                        rel_dir.display()
                    );
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
                    tracing::debug!(
                        "Scanner: subtree directory changed, rescanning {}",
                        rel_dir.display()
                    );
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
            } else if ft.is_file() {
                let file_start = Instant::now();
                let rel_file = path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf();

                if self.is_excluded(&rel_file) {
                    tracing::debug!(
                        "Scanner: skipping excluded file {} during subtree carry",
                        rel_file.display()
                    );
                    continue;
                }

                // Always re-check file mtime — content modifications change
                // file mtime without changing directory mtime.
                let file_mtime = file_modified_time(&path)?;
                new_file_mtimes.insert(rel_file.clone(), file_mtime.into());

                let status = if let Some(stored) = self.state.file_mtimes.get(&rel_file) {
                    if file_mtime != stored.0 {
                        changed.push(rel_file.clone());
                        "changed"
                    } else {
                        "unchanged"
                    }
                } else {
                    new.push(rel_file.clone());
                    "new"
                };

                tracing::debug!(
                    "Scanner: rechecked {} file {} in {:?}",
                    status,
                    rel_file.display(),
                    file_start.elapsed()
                );
            }
        }

        tracing::debug!(
            "Scanner: finished subtree {} in {:?}",
            rel_dir_display,
            subtree_start.elapsed()
        );

        Ok(())
    }

    // ── Git optimization ────────────────────────────────────────────

    /// Use git2 to find files changed since last_commit_sha.
    /// Returns None if git is unavailable or no previous SHA is stored.
    fn git_diff_changed(&self) -> Option<Vec<PathBuf>> {
        let last_sha = match self.state.last_commit_sha.as_ref() {
            Some(sha) => sha,
            None => {
                tracing::debug!("Scanner: git diff unavailable (no previous HEAD SHA)");
                return None;
            }
        };
        let repo = match git2::Repository::open(&self.repo_root) {
            Ok(repo) => repo,
            Err(err) => {
                tracing::debug!(
                    "Scanner: git diff unavailable for {}: {}",
                    self.repo_root.display(),
                    err
                );
                return None;
            }
        };

        let old_oid = match git2::Oid::from_str(last_sha) {
            Ok(oid) => oid,
            Err(err) => {
                tracing::debug!("Scanner: invalid stored HEAD SHA {}: {}", last_sha, err);
                return None;
            }
        };
        let old_commit = match repo.find_commit(old_oid) {
            Ok(commit) => commit,
            Err(err) => {
                tracing::debug!(
                    "Scanner: previous commit {} no longer available: {}",
                    last_sha,
                    err
                );
                return None;
            }
        };
        let old_tree = match old_commit.tree() {
            Ok(tree) => tree,
            Err(err) => {
                tracing::debug!(
                    "Scanner: failed to load previous tree for {}: {}",
                    old_commit.id(),
                    err
                );
                return None;
            }
        };

        let head_ref = match repo.head() {
            Ok(head) => head,
            Err(err) => {
                tracing::debug!("Scanner: failed to resolve HEAD: {}", err);
                return None;
            }
        };
        let head_commit = match head_ref.peel_to_commit() {
            Ok(commit) => commit,
            Err(err) => {
                tracing::debug!("Scanner: failed to peel HEAD to commit: {}", err);
                return None;
            }
        };
        let head_tree = match head_commit.tree() {
            Ok(tree) => tree,
            Err(err) => {
                tracing::debug!(
                    "Scanner: failed to load HEAD tree for {}: {}",
                    head_commit.id(),
                    err
                );
                return None;
            }
        };

        if old_commit.id() == head_commit.id() {
            tracing::debug!("Scanner: git HEAD unchanged at {}", head_commit.id());
            // No new commits
            return Some(Vec::new());
        }

        let mut opts = git2::DiffOptions::new();
        let diff = match repo.diff_tree_to_tree(Some(&old_tree), Some(&head_tree), Some(&mut opts)) {
            Ok(diff) => diff,
            Err(err) => {
                tracing::debug!(
                    "Scanner: git diff failed from {} to {}: {}",
                    old_commit.id(),
                    head_commit.id(),
                    err
                );
                return None;
            }
        };

        let mut paths = Vec::new();
        if diff
            .foreach(
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
            .is_err()
        {
            tracing::debug!(
                "Scanner: failed to iterate git diff entries from {} to {}",
                old_commit.id(),
                head_commit.id()
            );
            return None;
        }

        tracing::debug!(
            "Scanner: git diff from {} to {} produced {} path(s)",
            old_commit.id(),
            head_commit.id(),
            paths.len()
        );

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
/// - `prefix*/` — matches any path component starting with `prefix`
/// - `*.ext` — matches files ending in `.ext`
/// - `filename` — matches the exact filename (last component)
fn is_file_excluded(pattern: &str, path: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        // Extension pattern: *.pyc, *.o, etc.
        return path.ends_with(suffix);
    }
    if let Some(dirname) = pattern.strip_suffix('/') {
        // Directory pattern: check if any path component matches
        return dir_component_matches(dirname, path);
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
        return dir_component_matches(dirname, dir_path);
    }
    false
}

/// Check if any path component matches a directory name pattern.
/// Supports trailing `*` glob: `target*` matches `target`, `target-194`, etc.
fn dir_component_matches(dirname: &str, path: &str) -> bool {
    if let Some(prefix) = dirname.strip_suffix('*') {
        path.split('/').any(|comp| comp.starts_with(prefix))
    } else {
        path.split('/').any(|comp| comp == dirname)
    }
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
    match fs::symlink_metadata(path) {
        Ok(meta) => meta.modified()
            .with_context(|| format!("modified time for {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("Skipping inaccessible path: {}", path.display());
            Ok(SystemTime::UNIX_EPOCH)
        }
        Err(e) => Err(e).with_context(|| format!("metadata for {}", path.display())),
    }
}

fn file_modified_time(path: &Path) -> Result<SystemTime> {
    match fs::symlink_metadata(path) {
        Ok(meta) => meta.modified()
            .with_context(|| format!("modified time for {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("Skipping inaccessible path: {}", path.display());
            Ok(SystemTime::UNIX_EPOCH)
        }
        Err(e) => Err(e).with_context(|| format!("metadata for {}", path.display())),
    }
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

    #[test]
    fn test_glob_dir_pattern_excludes_prefixed_dirs() {
        // target*/ should match target, target-194, target-worktree, etc.
        assert!(is_dir_excluded("target*/", "target"));
        assert!(is_dir_excluded("target*/", "target-194"));
        assert!(is_dir_excluded("target*/", "target-worktree"));
        assert!(is_dir_excluded("target*/", "some/target-194"));
        assert!(!is_dir_excluded("target*/", "src"));
        assert!(!is_dir_excluded("target*/", "not-target"));

        // file exclude also respects glob dir patterns
        assert!(is_file_excluded("target*/", "target-194/debug/build/foo.rmeta"));
        assert!(is_file_excluded("target*/", "target/release/libfoo.rlib"));
        assert!(!is_file_excluded("target*/", "src/main.rs"));
    }

    #[test]
    fn test_binary_extension_excludes() {
        assert!(is_file_excluded("*.rmeta", "target/debug/deps/libfoo.rmeta"));
        assert!(is_file_excluded("*.rlib", "target/release/libfoo.rlib"));
        assert!(is_file_excluded("*.bc", "target/debug/deps/foo.bc"));
        assert!(is_file_excluded("*.a", "lib/libfoo.a"));
        assert!(is_file_excluded("*.dll", "bin/foo.dll"));
        assert!(is_file_excluded("*.exe", "bin/foo.exe"));
        assert!(is_file_excluded("*.wasm", "pkg/foo.wasm"));
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
        state
            .file_content_hashes
            .insert(PathBuf::from("src/main.rs"), "a".repeat(64));
        state.last_commit_sha = Some("abc123def456".to_string());
        state.last_scan = Some(SystemTime::now().into());

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: ScanState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.dir_mtimes.len(), 1);
        assert_eq!(restored.file_mtimes.len(), 1);
        assert_eq!(restored.file_content_hashes.len(), 1);
        assert_eq!(restored.last_commit_sha.as_deref(), Some("abc123def456"));
        assert!(restored.last_scan.is_some());
    }

    #[test]
    fn test_scan_state_backward_compat_no_content_hashes() {
        // Old scan-state.json without file_content_hashes field should deserialize fine
        let old_json = r#"{
            "dir_mtimes": {},
            "file_mtimes": {},
            "last_commit_sha": "abc123",
            "last_scan": [1700000000, 0]
        }"#;
        let state: ScanState = serde_json::from_str(old_json).unwrap();
        assert!(state.file_content_hashes.is_empty());
        assert_eq!(state.last_commit_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_content_hash_deterministic_across_reads() {
        // Same content always produces the same BLAKE3 hash
        let content = b"fn main() { println!(\"hello\"); }";
        let h1 = blake3::hash(content).to_hex().to_string();
        let h2 = blake3::hash(content).to_hex().to_string();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // 256-bit = 64 hex chars
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

        // State should be persisted after commit
        scanner.commit_state().unwrap();
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
        scanner.commit_state().unwrap();

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
        scanner.commit_state().unwrap();

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
        scanner.commit_state().unwrap();

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

    #[test]
    fn test_project_config_applies_to_custom_excludes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}\n");
        create_file(root, ".fastembed_cache/model.onnx", "model bytes");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        fs::write(
            root.join(".oh/config.toml"),
            "[scanner]\nexclude = [\".fastembed_cache/\"]\n",
        )
        .unwrap();

        let excludes = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        let mut scanner = Scanner::with_excludes(root.to_path_buf(), excludes).unwrap();
        let result = scanner.scan().unwrap();

        let all: Vec<_> = result.files_to_extract().into_iter().cloned().collect();
        assert!(
            !all.contains(&PathBuf::from(".fastembed_cache/model.onnx")),
            "Config exclude should apply to custom scanners: {:?}",
            all
        );
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
        scanner.commit_state().unwrap();

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
        scanner.commit_state().unwrap();

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
        scanner.commit_state().unwrap();

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

    // ── BLAKE3 content hash tests ───────────────────────────────────

    #[test]
    fn test_content_hash_filters_mtime_false_positive() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan: file is new
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result1 = scanner.scan().unwrap();
        assert_eq!(result1.new_files.len(), 1);
        assert!(result1.changed_files.is_empty());
        scanner.commit_state().unwrap();

        // Touch the file without changing content (mtime false positive)
        bump_mtime();
        create_file(root, "src/main.rs", "fn main() {}"); // same content

        // Second scan: mtime changed but content hash should filter it out
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        assert!(
            result2.changed_files.is_empty(),
            "Content hash should filter mtime false positive. changed: {:?}",
            result2.changed_files,
        );
        assert!(result2.new_files.is_empty());
    }

    #[test]
    fn test_content_hash_allows_real_changes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();
        scanner.commit_state().unwrap();

        // Modify with different content
        bump_mtime();
        create_file(root, "src/main.rs", "fn main() { println!(\"changed\"); }");

        // Second scan: content truly changed, should appear
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();

        assert!(
            result2.changed_files.contains(&PathBuf::from("src/main.rs")),
            "Real content change should be detected. changed: {:?}",
            result2.changed_files,
        );
    }

    #[test]
    fn test_content_hash_persisted_in_state() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();
        scanner.commit_state().unwrap();

        // Load persisted state and check content hashes
        let state = load_state(root).unwrap();
        assert!(
            state.file_content_hashes.contains_key(&PathBuf::from("src/main.rs")),
            "Content hash should be persisted. hashes: {:?}",
            state.file_content_hashes.keys().collect::<Vec<_>>()
        );

        // Verify it's a valid BLAKE3 hex hash (64 chars)
        let hash = &state.file_content_hashes[&PathBuf::from("src/main.rs")];
        assert_eq!(hash.len(), 64, "BLAKE3 hex hash should be 64 chars, got: {}", hash);
    }

    #[test]
    fn test_content_hash_removed_for_deleted_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "src/old.rs", "// will delete");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();
        scanner.commit_state().unwrap();

        // Delete a file
        fs::remove_file(root.join("src/old.rs")).unwrap();

        // Second scan
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        scanner2.scan().unwrap();
        scanner2.commit_state().unwrap();

        // Load state — deleted file's hash should be gone
        let state = load_state(root).unwrap();
        assert!(
            !state.file_content_hashes.contains_key(&PathBuf::from("src/old.rs")),
            "Deleted file hash should be removed from state"
        );
        // But existing file's hash should remain
        assert!(state.file_content_hashes.contains_key(&PathBuf::from("src/main.rs")));
    }

    // ── Deferred commit tests ─────────────────────────────────────

    #[test]
    fn test_uncommitted_scan_state_is_re_detected() {
        // Core correctness property: if commit_state() is NOT called after
        // scan(), the next scan must re-detect the same files. This prevents
        // silent data loss when graph update fails.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan -- do NOT commit state
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result1 = scanner.scan().unwrap();
        assert_eq!(result1.new_files.len(), 1);
        // Intentionally skip: scanner.commit_state().unwrap();

        // Second scan from fresh scanner -- should re-detect the same file
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();
        assert_eq!(
            result2.new_files.len(),
            1,
            "Uncommitted scan state should cause re-detection. new: {:?}",
            result2.new_files,
        );
    }

    #[test]
    fn test_committed_scan_state_is_not_re_detected() {
        // When commit_state() IS called, the next scan should see no changes.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        // First scan -- commit state
        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        let result1 = scanner.scan().unwrap();
        assert_eq!(result1.new_files.len(), 1);
        scanner.commit_state().unwrap();

        // Second scan -- should see no changes
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();
        assert!(
            result2.new_files.is_empty() && result2.changed_files.is_empty(),
            "Committed scan state should prevent re-detection. new: {:?}, changed: {:?}",
            result2.new_files,
            result2.changed_files,
        );
    }

    // ── Adversarial deferred-commit tests ────────────────────────────

    #[test]
    fn test_uncommitted_modified_files_re_detected() {
        // Adversarial (dissent: in-memory graph diverges from LanceDB on persist failure):
        // Modify after commit, scan without committing, verify re-detection.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "src/main.rs", "fn main() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();
        scanner.commit_state().unwrap();

        bump_mtime();
        create_file(root, "src/main.rs", "fn main() { changed(); }");

        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();
        assert!(result2.changed_files.contains(&PathBuf::from("src/main.rs")));
        // Intentionally skip commit (simulates failed graph update)

        let mut scanner3 = Scanner::new(root.to_path_buf()).unwrap();
        let result3 = scanner3.scan().unwrap();
        assert!(
            result3.changed_files.contains(&PathBuf::from("src/main.rs")),
            "Uncommitted modified file must be re-detected. changed: {:?}, new: {:?}",
            result3.changed_files, result3.new_files,
        );
    }

    #[test]
    fn test_uncommitted_deleted_files_re_detected() {
        // Adversarial (dissent: partial commit on multi-root failure):
        // Delete after commit, scan without committing, verify re-detection.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "src/lib.rs", "pub fn lib() {}");
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        let mut scanner = Scanner::new(root.to_path_buf()).unwrap();
        scanner.scan().unwrap();
        scanner.commit_state().unwrap();

        fs::remove_file(root.join("src/lib.rs")).unwrap();

        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();
        assert!(result2.deleted_files.contains(&PathBuf::from("src/lib.rs")));
        // Intentionally skip commit

        let mut scanner3 = Scanner::new(root.to_path_buf()).unwrap();
        let result3 = scanner3.scan().unwrap();
        assert!(
            result3.deleted_files.contains(&PathBuf::from("src/lib.rs")),
            "Uncommitted deletion must be re-detected. deleted: {:?}",
            result3.deleted_files,
        );
    }

    #[test]
    fn test_uncommitted_new_files_across_multiple_scans() {
        // Adversarial (dissent: commit_state fails after successful persist):
        // Add files in waves, never committing. Must see all files.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".oh/.cache")).unwrap();

        create_file(root, "src/a.rs", "fn a() {}");
        let mut scanner1 = Scanner::new(root.to_path_buf()).unwrap();
        let result1 = scanner1.scan().unwrap();
        assert_eq!(result1.new_files.len(), 1);

        bump_mtime();
        create_file(root, "src/b.rs", "fn b() {}");
        let mut scanner2 = Scanner::new(root.to_path_buf()).unwrap();
        let result2 = scanner2.scan().unwrap();
        assert_eq!(
            result2.new_files.len(), 2,
            "Both uncommitted new files must be detected. new: {:?}",
            result2.new_files,
        );
    }

    // ── PatternConfig tests ─────────────────────────────────────────

    #[test]
    fn test_pattern_config_default_has_all_builtins() {
        let config = PatternConfig::default();
        let suffixes = config.effective_suffixes();
        assert_eq!(suffixes.len(), DEFAULT_PATTERN_SUFFIXES.len());
        assert!(suffixes.iter().any(|(s, h)| s == "factory" && h == "factory"));
        assert!(suffixes.iter().any(|(s, h)| s == "manager" && h == "manager"));
    }

    #[test]
    fn test_pattern_config_disable_removes_patterns() {
        let config = PatternConfig {
            extra: vec![],
            disable: vec!["manager".to_string(), "service".to_string()],
        };
        let suffixes = config.effective_suffixes();
        assert!(!suffixes.iter().any(|(_, h)| h == "manager"));
        assert!(!suffixes.iter().any(|(_, h)| h == "service"));
        assert!(suffixes.iter().any(|(_, h)| h == "factory"));
        assert_eq!(
            suffixes.len(),
            DEFAULT_PATTERN_SUFFIXES.len() - 2,
        );
    }

    #[test]
    fn test_pattern_config_extra_adds_custom_patterns() {
        let config = PatternConfig {
            extra: vec![
                ["gateway".to_string(), "gateway".to_string()],
                ["interactor".to_string(), "interactor".to_string()],
            ],
            disable: vec![],
        };
        let suffixes = config.effective_suffixes();
        assert!(suffixes.iter().any(|(s, h)| s == "gateway" && h == "gateway"));
        assert!(suffixes.iter().any(|(s, h)| s == "interactor" && h == "interactor"));
        assert_eq!(
            suffixes.len(),
            DEFAULT_PATTERN_SUFFIXES.len() + 2,
        );
    }

    #[test]
    fn test_pattern_config_extra_deduplicates() {
        let config = PatternConfig {
            extra: vec![
                // "factory" already exists as a built-in
                ["factory".to_string(), "custom_factory".to_string()],
            ],
            disable: vec![],
        };
        let suffixes = config.effective_suffixes();
        // Should NOT add a duplicate; built-in takes priority
        let factory_count = suffixes.iter().filter(|(s, _)| s == "factory").count();
        assert_eq!(factory_count, 1);
        // The built-in hint is retained
        assert!(suffixes.iter().any(|(s, h)| s == "factory" && h == "factory"));
    }

    #[test]
    fn test_pattern_config_disable_and_extra_combined() {
        let config = PatternConfig {
            extra: vec![
                ["gateway".to_string(), "gateway".to_string()],
            ],
            disable: vec!["manager".to_string()],
        };
        let suffixes = config.effective_suffixes();
        assert!(!suffixes.iter().any(|(_, h)| h == "manager"));
        assert!(suffixes.iter().any(|(s, h)| s == "gateway" && h == "gateway"));
    }

    #[test]
    fn test_pattern_config_extra_normalizes_case() {
        let config = PatternConfig {
            extra: vec![
                ["Gateway".to_string(), "GATEWAY".to_string()],
            ],
            disable: vec![],
        };
        let suffixes = config.effective_suffixes();
        assert!(suffixes.iter().any(|(s, h)| s == "gateway" && h == "gateway"));
    }

    #[test]
    fn test_pattern_config_load_from_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".oh")).unwrap();
        fs::write(
            root.join(".oh/config.toml"),
            r#"
[patterns]
extra = [["gateway", "gateway"], ["usecase", "use_case"]]
disable = ["manager"]
"#,
        )
        .unwrap();

        let config = PatternConfig::load(root);
        assert_eq!(config.extra.len(), 2);
        assert_eq!(config.disable, vec!["manager"]);

        let suffixes = config.effective_suffixes();
        assert!(!suffixes.iter().any(|(_, h)| h == "manager"));
        assert!(suffixes.iter().any(|(s, h)| s == "gateway" && h == "gateway"));
        assert!(suffixes.iter().any(|(s, h)| s == "usecase" && h == "use_case"));
    }

    #[test]
    fn test_pattern_config_load_missing_file_returns_defaults() {
        let tmp = TempDir::new().unwrap();
        let config = PatternConfig::load(tmp.path());
        assert!(config.extra.is_empty());
        assert!(config.disable.is_empty());
    }

    #[test]
    fn test_pattern_config_load_no_patterns_section() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".oh")).unwrap();
        fs::write(
            root.join(".oh/config.toml"),
            r#"
[scanner]
exclude = ["dist/"]
"#,
        )
        .unwrap();

        let config = PatternConfig::load(root);
        assert!(config.extra.is_empty());
        assert!(config.disable.is_empty());
    }

    #[test]
    fn test_pattern_config_disable_applies_to_extras_too() {
        let config = PatternConfig {
            extra: vec![
                ["gateway".to_string(), "gateway".to_string()],
                ["interactor".to_string(), "blocked".to_string()],
            ],
            disable: vec!["blocked".to_string()],
        };
        let suffixes = config.effective_suffixes();
        // "gateway" extra should be present
        assert!(suffixes.iter().any(|(s, h)| s == "gateway" && h == "gateway"));
        // "interactor" extra should be blocked because its hint "blocked" is in disable
        assert!(!suffixes.iter().any(|(s, _)| s == "interactor"));
    }

    #[test]
    fn test_pattern_config_disable_case_insensitive() {
        let config = PatternConfig {
            extra: vec![],
            disable: vec!["Manager".to_string(), "SERVICE".to_string()],
        };
        let suffixes = config.effective_suffixes();
        // Mixed-case disable should still remove lowercase built-in hints
        assert!(!suffixes.iter().any(|(_, h)| h == "manager"));
        assert!(!suffixes.iter().any(|(_, h)| h == "service"));
        assert!(suffixes.iter().any(|(_, h)| h == "factory"));
    }

    // ── load_declared_roots tests ───────────────────────────────────

    #[test]
    fn test_load_declared_roots_from_config_toml() {
        let tmp = TempDir::new().unwrap();
        let oh_dir = tmp.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            "[workspace.roots]\ninfra = \"../k8s-configs\"\nprotos = \"/abs/protos\"\n",
        )
        .unwrap();

        let roots = load_declared_roots(tmp.path());
        assert_eq!(roots.len(), 2);
        let slugs: Vec<&str> = roots.iter().map(|(s, _)| s.as_str()).collect();
        assert!(slugs.contains(&"infra"), "Expected 'infra' slug");
        assert!(slugs.contains(&"protos"), "Expected 'protos' slug");

        let infra_path = roots.iter().find(|(s, _)| s == "infra").unwrap();
        assert_eq!(infra_path.1, std::path::PathBuf::from("../k8s-configs"));
        let protos_path = roots.iter().find(|(s, _)| s == "protos").unwrap();
        assert_eq!(protos_path.1, std::path::PathBuf::from("/abs/protos"));
    }

    #[test]
    fn test_load_declared_roots_no_workspace_section() {
        let tmp = TempDir::new().unwrap();
        let oh_dir = tmp.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(oh_dir.join("config.toml"), "[scanner]\nexclude = [\"benchmark/\"]\n").unwrap();

        let roots = load_declared_roots(tmp.path());
        assert!(roots.is_empty(), "No [workspace.roots] section should yield empty vec");
    }

    #[test]
    fn test_load_declared_roots_no_config_file() {
        let tmp = TempDir::new().unwrap();
        // No .oh/ directory at all
        let roots = load_declared_roots(tmp.path());
        assert!(roots.is_empty(), "Missing config file should yield empty vec");
    }

    // ── LspConfig integration tests ─────────────────────────────────

    #[test]
    fn test_lsp_config_load_hint_from_toml_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".oh")).unwrap();
        fs::write(
            root.join(".oh/config.toml"),
            "[lsp]\ndiagnostic_min_severity = \"hint\"\n",
        ).unwrap();

        let config = LspConfig::load(root);
        assert_eq!(config.diagnostic_min_severity, DiagnosticMinSeverity::Hint);
        assert_eq!(config.diagnostic_min_severity.max_severity_int(), 4);
    }

    #[test]
    fn test_lsp_config_load_missing_file_returns_warning_default() {
        let tmp = TempDir::new().unwrap();
        let config = LspConfig::load(tmp.path());
        assert_eq!(config.diagnostic_min_severity, DiagnosticMinSeverity::Warning);
    }

    #[test]
    fn test_lsp_config_load_no_lsp_section_returns_warning_default() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".oh")).unwrap();
        fs::write(
            root.join(".oh/config.toml"),
            "[scanner]\nexclude = [\"benchmark/\"]\n",
        ).unwrap();

        let config = LspConfig::load(root);
        assert_eq!(config.diagnostic_min_severity, DiagnosticMinSeverity::Warning,
            "missing [lsp] section should default to Warning");
    }

    #[test]
    fn test_lsp_config_load_all_severity_levels() {
        for (toml_value, expected_max) in &[
            ("error", 1u64),
            ("warning", 2),
            ("information", 3),
            ("hint", 4),
        ] {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            fs::create_dir_all(root.join(".oh")).unwrap();
            fs::write(
                root.join(".oh/config.toml"),
                format!("[lsp]\ndiagnostic_min_severity = \"{toml_value}\"\n"),
            ).unwrap();

            let config = LspConfig::load(root);
            assert_eq!(
                config.diagnostic_min_severity.max_severity_int(),
                *expected_max,
                "severity '{toml_value}' should map to max_severity_int={expected_max}"
            );
        }
    }
}
