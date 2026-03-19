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
    /// Optional user-specified slug. When set (e.g., for declared roots from
    /// `.oh/config.toml`), this overrides the path-derived slug so agents can
    /// reference the root by a stable name like "infra" or "protos".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug_override: Option<String>,
}

impl RootConfig {
    /// Create a code-project root from a path (the default for --repo).
    pub fn code_project(path: PathBuf) -> Self {
        Self {
            path,
            root_type: RootType::CodeProject,
            git_aware: true,
            excludes: Vec::new(),
            slug_override: None,
        }
    }

    /// Resolve the path, expanding `~` to the user's home directory.
    pub fn resolved_path(&self) -> PathBuf {
        expand_tilde(&self.path)
    }

    /// Generate a URL-safe slug for use as root_id.
    ///
    /// If the root was declared with an explicit slug (e.g., `infra = "../k8s"` in
    /// `.oh/config.toml`), that slug is returned as-is. Otherwise the slug is
    /// derived from the resolved path.
    pub fn slug(&self) -> String {
        if let Some(ref s) = self.slug_override {
            return s.clone();
        }
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

    /// Add the Claude Code auto-memory directory as a `Notes` root.
    ///
    /// Claude Code stores per-project memory at
    /// `~/.claude/projects/-{path-with-slashes-as-dashes}/memory/`.
    /// If that directory exists it is added as a Notes root so
    /// `search` can surface operational knowledge.
    pub fn with_claude_memory(mut self, repo_root: &Path) -> Self {
        if let Some(memory_dir) = claude_memory_dir(repo_root) {
            // Skip if already present (e.g. user added it in roots.toml).
            if self.roots.iter().any(|r| r.resolved_path() == memory_dir) {
                return self;
            }

            self.roots.push(RootConfig {
                path: memory_dir,
                root_type: RootType::Notes,
                git_aware: false,
                excludes: vec![],
                slug_override: None,
            });
        }
        self
    }

    /// Wire in agent memory locations that live **outside** the project root.
    ///
    /// Agent rule/memory files that live **inside** the project (`.cursorrules`,
    /// `.clinerules`, `.cursor/**`, `.serena/memories/**`,
    /// `.github/copilot-instructions.md`) are already picked up by the primary
    /// `CodeProject` root scan and tagged via `detect_oh_kind` in the markdown
    /// extractor. Adding them as a second root would double-index those files under
    /// a different root slug, causing duplicate search results.
    ///
    /// This method exists as the counterpart to `with_claude_memory` for the case
    /// where an agent stores its memory **outside** the repository (like Claude
    /// Code does at `~/.claude/projects/.../memory/`). If such a path is ever
    /// detected it is added as a `Notes` root here. Currently no external
    /// agent-memory locations are auto-detected, so this is a no-op — but it
    /// keeps the call chain consistent and provides the extension point.
    pub fn with_agent_memories(self, _repo_root: &Path) -> Self {
        // All known agent-memory locations (.cursorrules, .clinerules, .cursor/**,
        // .serena/memories/**, .github/copilot-instructions.md) are inside the
        // project root and already covered by the CodeProject scan. Adding them as
        // separate Notes roots would duplicate index entries. Tagging is handled
        // purely by detect_oh_kind in the markdown extractor.
        self
    }

    /// Register roots declared in `<repo_root>/.oh/config.toml` under
    /// `[workspace.roots]`.
    ///
    /// Each entry maps a user-chosen slug to a path:
    ///
    /// ```toml
    /// [workspace.roots]
    /// infra   = "../k8s-configs"
    /// protos  = "../shared-protos"
    /// ```
    ///
    /// Relative paths are resolved relative to `repo_root`. Missing paths are
    /// warned and skipped — they are not an error. Roots that are already present
    /// (same resolved path) are skipped to avoid duplicates.
    ///
    /// Declared roots use the TOML key as their slug so agents can reference them
    /// by stable name (`search(root="infra")`) regardless of path.
    ///
    /// The root type is `CodeProject` unless the path contains a `.oh/` directory,
    /// in which case it is treated as a `CodeProject` root with alignment context.
    /// (The type can be overridden later if needed; for now all declared roots are
    /// `CodeProject` with `git_aware = true`.)
    pub fn with_declared_roots(mut self, repo_root: &Path) -> Self {
        use crate::scanner::load_declared_roots;

        let declared = load_declared_roots(repo_root);
        for (slug, raw_path) in declared {
            // Resolve relative paths against repo_root
            let path = if raw_path.is_absolute() {
                raw_path.clone()
            } else {
                repo_root.join(&raw_path)
            };
            let resolved = expand_tilde(&path);

            // Warn and skip missing paths
            if !resolved.exists() {
                tracing::warn!(
                    "Declared workspace root '{}' at '{}' does not exist — skipping",
                    slug,
                    resolved.display()
                );
                continue;
            }

            // Canonicalize to remove any `..` components and resolve symlinks so
            // duplicate detection and display are stable
            // (e.g. `service/../k8s` → `k8s`, `/var/...` → `/private/var/...` on macOS).
            let resolved = std::fs::canonicalize(&resolved).unwrap_or(resolved);

            // Skip duplicates (same canonical path already registered).
            // We canonicalize both sides so /var and /private/var compare equal.
            if self.roots.iter().any(|r| {
                let existing = std::fs::canonicalize(r.resolved_path())
                    .unwrap_or_else(|_| r.resolved_path());
                existing == resolved
            }) {
                tracing::debug!(
                    "Declared workspace root '{}' at '{}' already registered — skipping",
                    slug,
                    resolved.display()
                );
                continue;
            }

            tracing::info!(
                "Registering declared workspace root '{}' at '{}'",
                slug,
                resolved.display()
            );
            self.roots.push(RootConfig {
                path: resolved,
                root_type: RootType::CodeProject,
                git_aware: true,
                excludes: Vec::new(),
                slug_override: Some(slug),
            });
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

/// Compute the Claude Code auto-memory directory for a given repo root.
///
/// Claude Code encodes the absolute path by stripping the leading `/`,
/// replacing non-alphanumeric non-hyphen characters (including `/` and `.`)
/// with `-`, and prefixing with `-`. Case is preserved.
///
/// e.g. `/Users/foo/src/bar` → `~/.claude/projects/-Users-foo-src-bar/memory/`
///      `/tmp/my.project`    → `~/.claude/projects/-tmp-my-project/memory/`
///
/// Returns `None` if `HOME` is not set.
pub fn claude_memory_dir(repo_root: &Path) -> Option<PathBuf> {
    let home = home_dir()?;
    let canonical = expand_tilde(repo_root);
    let encoded: String = canonical
        .to_string_lossy()
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let key = format!("-{}", encoded);
    Some(home.join(".claude").join("projects").join(key).join("memory"))
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
                slug_override: None,
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
                slug_override: None,
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
        assert!(excludes.contains(&"target*/".to_string()));
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
            slug_override: None,
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
                slug_override: None,
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
                slug_override: None,
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

        // Commit scanner state (scan no longer auto-persists)
        code_scanner.commit_state().unwrap();
        notes_scanner.commit_state().unwrap();

        // Verify state was persisted at the custom path
        assert!(notes_state.exists(), "Notes scan state should be persisted at custom path");
    }

    #[test]
    fn test_claude_memory_dir_format() {
        let dir = claude_memory_dir(Path::new("/Users/foo/src/bar"));
        assert!(dir.is_some());
        let dir = dir.unwrap();
        let dir_str = dir.to_string_lossy();
        assert!(
            dir_str.ends_with("/.claude/projects/-Users-foo-src-bar/memory"),
            "Got: {}",
            dir_str
        );
    }

    #[test]
    fn test_claude_memory_dir_replaces_dots() {
        // Claude Code replaces '.' with '-' in the project key
        let dir = claude_memory_dir(Path::new("/Users/foo/Downloads/improve-deployments.md"))
            .unwrap();
        let dir_str = dir.to_string_lossy();
        assert!(
            dir_str.ends_with("/.claude/projects/-Users-foo-Downloads-improve-deployments-md/memory"),
            "Dots should become dashes. Got: {}",
            dir_str
        );
    }

    #[test]
    fn test_with_claude_memory_adds_root_when_dir_exists() {
        // Use the real HOME to compute where claude_memory_dir would point,
        // then create that directory so the test can verify it gets added.
        let cwd = std::env::current_dir().unwrap();
        let memory_dir = match claude_memory_dir(&cwd) {
            Some(d) => d,
            None => return, // No HOME set, can't test
        };

        if !memory_dir.exists() {
            // If the real memory dir doesn't exist, we can't create it
            // without side effects. Just verify the method is a no-op.
            let config = WorkspaceConfig::default()
                .with_primary_root(cwd.clone())
                .with_claude_memory(&cwd);
            // Memory root added but resolved_roots filters non-existent
            let resolved = config.resolved_roots();
            assert_eq!(resolved.len(), 1, "Only primary root when memory dir missing");
            return;
        }

        // Memory dir exists — verify it gets added
        let config = WorkspaceConfig::default()
            .with_primary_root(cwd.clone())
            .with_claude_memory(&cwd);
        let resolved = config.resolved_roots();
        assert!(resolved.len() >= 2, "Expected primary + memory root");
        let memory_root = resolved.iter().find(|r| r.path == memory_dir);
        assert!(memory_root.is_some(), "Memory root should be in resolved roots");
        assert_eq!(memory_root.unwrap().config.root_type, RootType::Notes);
    }

    #[test]
    fn test_with_claude_memory_skips_when_dir_missing() {
        // Memory dir won't exist for a random path
        let tmp = TempDir::new().unwrap();
        let config = WorkspaceConfig::default()
            .with_primary_root(tmp.path().to_path_buf())
            .with_claude_memory(tmp.path());

        // resolved_roots filters non-existent, so memory root won't appear
        let resolved = config.resolved_roots();
        assert_eq!(resolved.len(), 1, "Only primary root should be present");
    }

    #[test]
    fn test_with_agent_memories_is_noop_for_in_project_paths() {
        // Agent memory files inside the project (.serena/memories/, .cursorrules, etc.)
        // are already indexed by the CodeProject root scan and tagged via detect_oh_kind.
        // with_agent_memories() must NOT add them as a second root (that would create
        // duplicate index entries under a different root slug).
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();

        // Even if .serena/memories/ exists, it should not be added as a Notes root.
        let serena_dir = repo_root.join(".serena").join("memories");
        fs::create_dir_all(&serena_dir).unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.to_path_buf())
            .with_agent_memories(repo_root);

        let resolved = config.resolved_roots();
        assert_eq!(
            resolved.len(),
            1,
            "with_agent_memories() must not add in-project paths as separate roots (causes duplicate indexing)"
        );
    }

    #[test]
    fn test_with_agent_memories_preserves_existing_roots() {
        // with_agent_memories() is a pass-through — it must not disturb the chain
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        let config = WorkspaceConfig {
            roots: vec![RootConfig {
                path: tmp2.path().to_path_buf(),
                root_type: RootType::Notes,
                git_aware: false,
                excludes: vec![],
                slug_override: None,
            }],
        }
        .with_primary_root(tmp1.path().to_path_buf())
        .with_agent_memories(tmp1.path());

        // Primary + pre-existing Notes root, nothing added or removed
        assert_eq!(config.resolved_roots().len(), 2);
    }

    // ── with_declared_roots tests ────────────────────────────────────

    #[test]
    fn test_with_declared_roots_registers_existing_path() {
        let repo_root = TempDir::new().unwrap();
        let declared_dir = TempDir::new().unwrap();

        // Write a .oh/config.toml with [workspace.roots]
        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            format!(
                "[workspace.roots]\ninfra = \"{}\"\n",
                declared_dir.path().display()
            ),
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        // Primary + declared "infra" root
        assert_eq!(resolved.len(), 2, "Expected primary + declared root");
        let infra = resolved.iter().find(|r| r.slug == "infra");
        assert!(infra.is_some(), "Declared root 'infra' should appear in resolved roots");
        // Canonicalize both sides to handle macOS symlinks (/var -> /private/var)
        let expected = std::fs::canonicalize(declared_dir.path())
            .unwrap_or_else(|_| declared_dir.path().to_path_buf());
        assert_eq!(infra.unwrap().path, expected);
    }

    #[test]
    fn test_with_declared_roots_skips_missing_path() {
        let repo_root = TempDir::new().unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            "[workspace.roots]\nprotos = \"/definitely/nonexistent/path/xyzzy\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        // Only the primary root; missing declared root was skipped
        let resolved = config.resolved_roots();
        assert_eq!(resolved.len(), 1, "Missing declared root must be skipped");
        assert!(
            resolved.iter().all(|r| r.slug != "protos"),
            "Missing declared root must not appear"
        );
    }

    #[test]
    fn test_with_declared_roots_relative_path_resolution() {
        // The declared root path is relative — resolved against repo_root
        let parent = TempDir::new().unwrap();
        let repo_dir = parent.path().join("my-service");
        let sibling_dir = parent.path().join("k8s-configs");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::create_dir_all(&sibling_dir).unwrap();

        let oh_dir = repo_dir.join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        // Use relative path "../k8s-configs"
        fs::write(
            oh_dir.join("config.toml"),
            "[workspace.roots]\ninfra = \"../k8s-configs\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_dir.clone())
            .with_declared_roots(&repo_dir);

        let resolved = config.resolved_roots();
        assert_eq!(resolved.len(), 2, "Expected primary + resolved relative root");
        let infra = resolved.iter().find(|r| r.slug == "infra");
        assert!(infra.is_some(), "Declared relative root 'infra' should resolve");
        // Canonicalize both sides to handle macOS symlinks (/var -> /private/var)
        let expected = std::fs::canonicalize(&sibling_dir).unwrap_or(sibling_dir);
        assert_eq!(infra.unwrap().path, expected);
    }

    #[test]
    fn test_with_declared_roots_no_config_file_is_noop() {
        let repo_root = TempDir::new().unwrap();
        // No .oh/config.toml at all

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        assert_eq!(resolved.len(), 1, "No config file should be a no-op");
    }

    #[test]
    fn test_with_declared_roots_skips_duplicate_path() {
        // A declared root pointing at the same path as the primary root should be skipped
        let repo_root = TempDir::new().unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        // Declare the repo root itself — duplicate
        fs::write(
            oh_dir.join("config.toml"),
            format!(
                "[workspace.roots]\nself-ref = \"{}\"\n",
                repo_root.path().display()
            ),
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        assert_eq!(resolved.len(), 1, "Duplicate path must be deduped");
    }

    #[test]
    fn test_root_config_slug_uses_override_when_set() {
        let root = RootConfig {
            path: PathBuf::from("/tmp/some/project"),
            root_type: RootType::CodeProject,
            git_aware: true,
            excludes: vec![],
            slug_override: Some("infra".to_string()),
        };
        assert_eq!(root.slug(), "infra");
    }

    #[test]
    fn test_root_config_slug_derives_from_path_when_no_override() {
        let root = RootConfig {
            path: PathBuf::from("/tmp/some/project"),
            root_type: RootType::CodeProject,
            git_aware: true,
            excludes: vec![],
            slug_override: None,
        };
        assert_eq!(root.slug(), "tmp-some-project");
    }
}
