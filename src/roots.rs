//! Multi-root workspace configuration.
//!
//! Supports scanning multiple directory roots (code projects, notes, downloads)
//! with per-root type presets and exclude patterns. Configuration lives at
//! `~/.config/rna/roots.toml`.
//!
//! The `--repo` CLI arg becomes the primary code-project root. Additional roots
//! are loaded from the user-level config file and merged.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scanner::DEFAULT_EXCLUDES;

// ── Root types ──────────────────────────────────────────────────────

/// The type of a workspace root, determining default exclude presets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootType {
    CodeProject,
    Notes,
    General,
    Custom,
}

impl Default for RootType {
    fn default() -> Self {
        RootType::CodeProject
    }
}

impl std::fmt::Display for RootType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RootType::CodeProject => write!(f, "code-project"),
            RootType::Notes => write!(f, "notes"),
            RootType::General => write!(f, "general"),
            RootType::Custom => write!(f, "custom"),
        }
    }
}

// ── Root config ─────────────────────────────────────────────────────

/// Configuration for a single workspace root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootConfig {
    /// Absolute or ~-relative path to the root directory.
    pub path: PathBuf,
    /// What kind of root this is (determines default excludes).
    #[serde(default)]
    pub root_type: RootType,
    /// Whether to use git optimization for change detection.
    #[serde(default)]
    pub git_aware: bool,
    /// Additional exclude patterns (merged with type presets).
    #[serde(default)]
    pub excludes: Vec<String>,
}

impl RootConfig {
    /// Create a code-project root from a path (the default for --repo).
    pub fn code_project(path: PathBuf) -> Self {
        Self {
            path,
            root_type: RootType::CodeProject,
            git_aware: true,
            excludes: Vec::new(),
        }
    }

    /// Resolve the path, expanding `~` to the user's home directory.
    pub fn resolved_path(&self) -> PathBuf {
        expand_tilde(&self.path)
    }

    /// Generate a URL-safe slug from the path for use as root_id.
    pub fn slug(&self) -> String {
        path_to_slug(&self.resolved_path())
    }

    /// Compute the effective exclude patterns for this root.
    /// Merges type-preset defaults with per-root custom excludes.
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut excludes = default_excludes_for_type(&self.root_type);
        for extra in &self.excludes {
            if !excludes.contains(extra) {
                excludes.push(extra.clone());
            }
        }
        excludes
    }
}

// ── Workspace config ────────────────────────────────────────────────

/// Multi-root workspace configuration.
/// Loaded from `~/.config/rna/roots.toml` and merged with the `--repo` arg.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub roots: Vec<RootConfig>,
}

impl WorkspaceConfig {
    /// Load workspace config from `~/.config/rna/roots.toml`.
    /// Returns an empty config if the file doesn't exist.
    pub fn load() -> Self {
        Self::load_from_path(&config_path())
    }

    /// Load from a specific path (for testing).
    pub fn load_from_path(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<WorkspaceConfig>(&content) {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", path.display(), e);
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Merge the primary `--repo` root with roots from config.
    /// The `--repo` arg becomes a code-project root and is always first.
    /// Duplicate paths are deduplicated (--repo wins).
    pub fn with_primary_root(mut self, repo_root: PathBuf) -> Self {
        let primary = RootConfig::code_project(repo_root.clone());
        let canonical_primary = primary.resolved_path();

        // Remove any config root that duplicates the primary
        self.roots
            .retain(|r| r.resolved_path() != canonical_primary);

        // Insert primary at the front
        self.roots.insert(0, primary);
        self
    }

    /// Detect active git worktrees from `.git/worktrees/` and add each as a
    /// `CodeProject` root. Stale entries (left behind briefly after
    /// `git worktree remove`) are skipped via `Path::exists()`.
    ///
    /// Slug uniqueness: `path_to_slug` encodes the **full path** so two
    /// worktrees sharing a basename get distinct slugs.
    pub fn with_worktrees(mut self, repo_root: &Path) -> Self {
        let worktrees_dir = repo_root.join(".git").join("worktrees");
        let entries = match std::fs::read_dir(&worktrees_dir) {
            Ok(e) => e,
            // No .git/worktrees directory means no linked worktrees.
            Err(_) => return self,
        };

        for entry in entries.flatten() {
            // Each subdirectory corresponds to one linked worktree.
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_dir() {
                continue;
            }

            // The `gitdir` file points back to the worktree's .git file.
            // Its parent is the worktree checkout root.
            let gitdir_file = entry.path().join("gitdir");
            let gitdir_content = match std::fs::read_to_string(&gitdir_file) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let gitdir_path = PathBuf::from(gitdir_content.trim());
            let worktree_path = match gitdir_path.parent() {
                Some(p) => p.to_path_buf(),
                None => continue,
            };

            // Guard against stale entries: paths persist briefly after removal.
            if !worktree_path.exists() {
                tracing::debug!(
                    "Skipping stale worktree entry {:?} — path does not exist: {}",
                    entry.file_name(),
                    worktree_path.display()
                );
                continue;
            }

            // Avoid duplicating the primary root.
            let canonical_primary = expand_tilde(&PathBuf::from(repo_root));
            if worktree_path == canonical_primary {
                continue;
            }

            // Skip if already present (e.g. from user's roots.toml).
            if self.roots.iter().any(|r| r.resolved_path() == worktree_path) {
                continue;
            }

            tracing::info!("Detected worktree: {}", worktree_path.display());
            self.roots.push(RootConfig::code_project(worktree_path));
        }

        self
    }

    /// Get all roots with their resolved paths and slugs.
    pub fn resolved_roots(&self) -> Vec<ResolvedRoot> {
        self.roots
            .iter()
            .filter_map(|config| {
                let path = config.resolved_path();
                if path.exists() {
                    Some(ResolvedRoot {
                        slug: config.slug(),
                        path,
                        config: config.clone(),
                    })
                } else {
                    tracing::warn!(
                        "Skipping root {} — path does not exist: {}",
                        config.slug(),
                        config.resolved_path().display()
                    );
                    None
                }
            })
            .collect()
    }
}

/// A root with its resolved path and slug, ready for scanning.
#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    pub slug: String,
    pub path: PathBuf,
    pub config: RootConfig,
}

// ── Default excludes by root type ───────────────────────────────────

/// Returns the default exclude patterns for a given root type.
pub fn default_excludes_for_type(root_type: &RootType) -> Vec<String> {
    match root_type {
        RootType::CodeProject => DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
        RootType::Notes => vec![".DS_Store".to_string()],
        RootType::General => vec![
            ".DS_Store".to_string(),
            "*.iso".to_string(),
            "*.dmg".to_string(),
        ],
        RootType::Custom => vec![],
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Default config file path: `~/.config/rna/roots.toml`.
fn config_path() -> PathBuf {
    dirs_path("rna", "roots.toml")
}

/// Build a path under `~/.config/<app>/<file>`.
fn dirs_path(app: &str, file: &str) -> PathBuf {
    if let Some(home) = home_dir() {
        home.join(".config").join(app).join(file)
    } else {
        PathBuf::from(file)
    }
}

/// Expand `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") || s == "~" {
        if let Some(home) = home_dir() {
            return home.join(s.strip_prefix("~/").unwrap_or(""));
        }
    }
    path.to_path_buf()
}

/// Get the user's home directory.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Convert a path to a URL-safe slug using the full canonical path.
/// Using the full path (not just basename) guarantees uniqueness even when
/// two worktrees share the same directory name.
/// e.g., `/Users/foo/src/my-project` -> `users-foo-src-my-project`
fn path_to_slug(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches('/')
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Path for per-root scan state cache.
/// `~/.local/share/rna/cache/{root-slug}/scan-state.json`
pub fn cache_state_path(root_slug: &str) -> PathBuf {
    if let Some(home) = home_dir() {
        home.join(".local")
            .join("share")
            .join("rna")
            .join("cache")
            .join(root_slug)
            .join("scan-state.json")
    } else {
        PathBuf::from(format!(".rna-cache/{}/scan-state.json", root_slug))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_workspace_config_load_from_toml() {
        let tmp = TempDir::new().unwrap();
        let config_file = tmp.path().join("roots.toml");
        fs::write(
            &config_file,
            r#"
[[roots]]
path = "/tmp/zettelkasten"
root_type = "notes"
git_aware = true

[[roots]]
path = "/tmp/downloads"
root_type = "general"
excludes = ["*.iso", "*.dmg"]
"#,
        )
        .unwrap();

        let config = WorkspaceConfig::load_from_path(&config_file);
        assert_eq!(config.roots.len(), 2);
        assert_eq!(config.roots[0].root_type, RootType::Notes);
        assert!(config.roots[0].git_aware);
        assert_eq!(config.roots[1].root_type, RootType::General);
        assert_eq!(config.roots[1].excludes, vec!["*.iso", "*.dmg"]);
    }

    #[test]
    fn test_workspace_config_missing_file_returns_default() {
        let config = WorkspaceConfig::load_from_path(Path::new("/nonexistent/roots.toml"));
        assert!(config.roots.is_empty());
    }

    #[test]
    fn test_workspace_config_invalid_toml_returns_default() {
        let tmp = TempDir::new().unwrap();
        let config_file = tmp.path().join("roots.toml");
        fs::write(&config_file, "this is not valid toml {{{{").unwrap();

        let config = WorkspaceConfig::load_from_path(&config_file);
        assert!(config.roots.is_empty());
    }

    #[test]
    fn test_with_primary_root_deduplicates() {
        let tmp = TempDir::new().unwrap();
        let primary = tmp.path().to_path_buf();

        let mut config = WorkspaceConfig {
            roots: vec![RootConfig {
                path: primary.clone(),
                root_type: RootType::Notes,
                git_aware: false,
                excludes: vec![],
            }],
        };

        config = config.with_primary_root(primary.clone());

        // Should have exactly 1 root (primary replaces the duplicate)
        assert_eq!(config.roots.len(), 1);
        assert_eq!(config.roots[0].root_type, RootType::CodeProject);
    }

    #[test]
    fn test_with_primary_root_preserves_others() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        let config = WorkspaceConfig {
            roots: vec![RootConfig {
                path: tmp2.path().to_path_buf(),
                root_type: RootType::Notes,
                git_aware: false,
                excludes: vec![],
            }],
        };

        let config = config.with_primary_root(tmp1.path().to_path_buf());
        assert_eq!(config.roots.len(), 2);
        assert_eq!(config.roots[0].root_type, RootType::CodeProject);
        assert_eq!(config.roots[1].root_type, RootType::Notes);
    }

    #[test]
    fn test_default_excludes_code_project() {
        let excludes = default_excludes_for_type(&RootType::CodeProject);
        assert!(excludes.contains(&"node_modules/".to_string()));
        assert!(excludes.contains(&"target/".to_string()));
        assert!(excludes.contains(&".git/".to_string()));
    }

    #[test]
    fn test_default_excludes_notes() {
        let excludes = default_excludes_for_type(&RootType::Notes);
        assert_eq!(excludes, vec![".DS_Store".to_string()]);
    }

    #[test]
    fn test_default_excludes_general() {
        let excludes = default_excludes_for_type(&RootType::General);
        assert!(excludes.contains(&".DS_Store".to_string()));
        assert!(excludes.contains(&"*.iso".to_string()));
        assert!(excludes.contains(&"*.dmg".to_string()));
    }

    #[test]
    fn test_default_excludes_custom() {
        let excludes = default_excludes_for_type(&RootType::Custom);
        assert!(excludes.is_empty());
    }

    #[test]
    fn test_root_config_effective_excludes_merges() {
        let root = RootConfig {
            path: PathBuf::from("/tmp/project"),
            root_type: RootType::Notes,
            git_aware: false,
            excludes: vec!["*.tmp".to_string()],
        };

        let excludes = root.effective_excludes();
        // Notes preset: .DS_Store + custom *.tmp
        assert!(excludes.contains(&".DS_Store".to_string()));
        assert!(excludes.contains(&"*.tmp".to_string()));
    }

    #[test]
    fn test_path_to_slug() {
        assert_eq!(path_to_slug(Path::new("/Users/foo/src/my-project")), "users-foo-src-my-project");
        assert_eq!(path_to_slug(Path::new("/home/user/zettelkasten")), "home-user-zettelkasten");
        assert_eq!(path_to_slug(Path::new("/tmp/My Project Name")), "tmp-my-project-name");
    }

    #[test]
    fn test_path_to_slug_uniqueness_for_same_basename() {
        // Two worktrees at different parent paths with the same directory name
        // must get distinct slugs.
        let slug_a = path_to_slug(Path::new("/work/projectA/feat-x"));
        let slug_b = path_to_slug(Path::new("/work/projectB/feat-x"));
        assert_ne!(slug_a, slug_b);
    }

    #[test]
    fn test_with_worktrees_skips_nonexistent_path() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();

        // Create a fake .git/worktrees/<name>/gitdir pointing at a nonexistent path
        let wt_admin = repo_root.join(".git").join("worktrees").join("stale");
        std::fs::create_dir_all(&wt_admin).unwrap();
        std::fs::write(
            wt_admin.join("gitdir"),
            "/definitely/does/not/exist/.git",
        )
        .unwrap();

        let config = WorkspaceConfig::default().with_worktrees(&repo_root);
        // Stale entry should not appear in roots.
        assert!(
            config.roots.is_empty(),
            "Stale worktree must be skipped"
        );
    }

    #[test]
    fn test_expand_tilde() {
        // Without HOME this is hard to test deterministically,
        // but we can verify non-tilde paths pass through
        let path = Path::new("/absolute/path");
        assert_eq!(expand_tilde(path), PathBuf::from("/absolute/path"));

        let path = Path::new("relative/path");
        assert_eq!(expand_tilde(path), PathBuf::from("relative/path"));
    }

    #[test]
    fn test_resolved_roots_skips_missing_paths() {
        let config = WorkspaceConfig {
            roots: vec![RootConfig {
                path: PathBuf::from("/definitely/nonexistent/path/xyzzy"),
                root_type: RootType::Notes,
                git_aware: false,
                excludes: vec![],
            }],
        };

        let resolved = config.resolved_roots();
        assert!(resolved.is_empty(), "Should skip nonexistent paths");
    }

    #[test]
    fn test_resolved_roots_includes_existing_paths() {
        let tmp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            roots: vec![RootConfig {
                path: tmp.path().to_path_buf(),
                root_type: RootType::CodeProject,
                git_aware: true,
                excludes: vec![],
            }],
        };

        let resolved = config.resolved_roots();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].path, tmp.path());
    }

    #[test]
    fn test_root_type_default() {
        let rt: RootType = Default::default();
        assert_eq!(rt, RootType::CodeProject);
    }

    #[test]
    fn test_root_config_code_project_constructor() {
        let root = RootConfig::code_project(PathBuf::from("/tmp/project"));
        assert_eq!(root.root_type, RootType::CodeProject);
        assert!(root.git_aware);
        assert!(root.excludes.is_empty());
    }

    #[test]
    fn test_multi_root_scanning_with_tempdir() {
        use crate::scanner::Scanner;

        // Create two roots: a code project and a notes directory
        let code_dir = TempDir::new().unwrap();
        let notes_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();

        // Populate code root
        let code_root = code_dir.path();
        fs::create_dir_all(code_root.join(".oh/.cache")).unwrap();
        fs::create_dir_all(code_root.join("src")).unwrap();
        fs::write(code_root.join("src/main.rs"), "fn main() {}").unwrap();

        // Populate notes root
        let notes_root = notes_dir.path();
        fs::create_dir_all(notes_root.join("journal")).unwrap();
        fs::write(notes_root.join("journal/2024-01-01.md"), "# Today\nDid stuff.").unwrap();

        // Scan code root with default scanner
        let mut code_scanner = Scanner::new(code_root.to_path_buf()).unwrap();
        let code_result = code_scanner.scan().unwrap();
        assert!(
            !code_result.new_files.is_empty(),
            "Code root should find files"
        );

        // Scan notes root with custom state path (simulating multi-root)
        let notes_state = cache_dir.path().join("notes").join("scan-state.json");
        let notes_excludes = default_excludes_for_type(&RootType::Notes);
        let mut notes_scanner = Scanner::with_excludes_and_state_path(
            notes_root.to_path_buf(),
            notes_excludes,
            notes_state.clone(),
        )
        .unwrap();
        let notes_result = notes_scanner.scan().unwrap();
        assert!(
            !notes_result.new_files.is_empty(),
            "Notes root should find files"
        );

        // Verify state was persisted at the custom path
        assert!(notes_state.exists(), "Notes scan state should be persisted at custom path");
    }
}
