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
use crate::walk::is_worktree_with_own_cache;

// ── Root types ──────────────────────────────────────────────────────

/// The type of a workspace root, determining default exclude presets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum RootType {
    #[default]
    CodeProject,
    Notes,
    General,
    Custom,
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
    /// When true, this root is a subdirectory of the primary root.
    ///
    /// Subdirectory roots (e.g. `client = "client"` where `client/` lives inside
    /// the primary repo) are handled differently:
    /// - **No tree-sitter extraction** — files are already covered by the primary root scan.
    ///   Skipping re-extraction prevents duplicate nodes in search results.
    /// - **LSP working directory** — the subdirectory path is passed as `rootUri` when
    ///   starting language servers for nodes whose files live under this subdirectory.
    ///   This lets typescript-language-server find `client/tsconfig.json` instead of
    ///   failing to find it at the repo root.
    #[serde(default, skip_serializing_if = "is_false")]
    pub lsp_only: bool,
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
            lsp_only: false,
        }
    }

    /// Resolve the path, expanding `~` to the user's home directory.
    pub fn resolved_path(&self) -> PathBuf {
        expand_tilde(&self.path)
    }

    /// Generate a URL-safe slug for use as root_id.
    ///
    /// If the root was declared with an explicit slug (e.g., `infra = "../k8s"` in
    /// `.oh/config.toml`), that slug is sanitized (path separators and unsafe chars
    /// replaced with `-`) and returned. Otherwise the slug is derived from the
    /// resolved path.
    ///
    /// Sanitization prevents path separators or `..` sequences from escaping the
    /// cache directory (`cache_state_path()` uses the slug as a directory component).
    pub fn slug(&self) -> String {
        if let Some(ref s) = self.slug_override {
            let sanitized = sanitize_slug(s);
            if !sanitized.is_empty() {
                return sanitized;
            }
            // Override sanitizes to empty (e.g. `slug_override = ".."`).
            // Fall through to path-derived slug so the root still gets a valid ID.
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
            if self
                .roots
                .iter()
                .any(|r| r.resolved_path() == worktree_path)
            {
                continue;
            }

            // Skip worktrees that maintain their own RNA index — they manage
            // their own scan lifecycle and should not be indexed by the main repo.
            if is_worktree_with_own_cache(&worktree_path) {
                tracing::info!(
                    "skipping worktree {}: has own RNA cache",
                    worktree_path.display()
                );
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
                lsp_only: false,
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
        use std::collections::{HashMap, HashSet};

        let declared = load_declared_roots(repo_root);

        // Canonicalize the primary root once for subdirectory detection.
        let canonical_primary =
            std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());

        // Build seen-paths and seen-slugs sets once (O(N) up-front rather than O(N²)
        // per-entry). Canonicalize existing roots once so we don't redo it per iteration.
        let mut seen_paths: HashMap<PathBuf, String> = self
            .roots
            .iter()
            .map(|r| {
                let canonical =
                    std::fs::canonicalize(r.resolved_path()).unwrap_or_else(|_| r.resolved_path());
                (canonical, r.slug())
            })
            .collect();
        let mut seen_slugs: HashSet<String> = self.roots.iter().map(|r| r.slug()).collect();

        for (slug, raw_path) in declared {
            // Sanitize the slug to prevent path-traversal via cache_state_path().
            let slug = sanitize_slug(&slug);
            if slug.is_empty() {
                tracing::warn!(
                    "Declared workspace root has empty slug after sanitization — skipping"
                );
                continue;
            }

            // Expand `~` first, then decide absolute vs. relative.
            // PathBuf::from("~/foo").is_absolute() is false on all platforms, so we
            // must expand tilde before the absolute check.
            let expanded = expand_tilde(&raw_path);
            let resolved = if expanded.is_absolute() {
                expanded
            } else {
                repo_root.join(&expanded)
            };

            // Warn and skip missing paths (not an error).
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
            let canonical = std::fs::canonicalize(&resolved).unwrap_or(resolved);

            // Skip duplicate paths (O(1) with pre-built map).
            if let Some(existing_slug) = seen_paths.get(&canonical) {
                tracing::debug!(
                    "Declared workspace root '{}' at '{}' already registered as '{}' — skipping",
                    slug,
                    canonical.display(),
                    existing_slug
                );
                continue;
            }

            // Skip duplicate slugs (O(1) with pre-built set).
            if seen_slugs.contains(&slug) {
                tracing::warn!(
                    "Declared workspace root slug '{}' is already in use — skipping duplicate",
                    slug
                );
                continue;
            }

            // Detect subdirectory roots: if the declared path is a subdirectory of the
            // primary root, mark it as `lsp_only`. Such roots already have their files
            // covered by the primary root's tree-sitter extraction, so we only need them
            // as an LSP working directory (e.g. so typescript-language-server can find
            // `client/tsconfig.json` by starting from `client/` instead of the repo root).
            let is_subdir =
                canonical.starts_with(&canonical_primary) && canonical != canonical_primary;
            if is_subdir {
                tracing::info!(
                    "Registering declared subdirectory root '{}' at '{}' as lsp-only (files covered by primary root)",
                    slug,
                    canonical.display()
                );
            } else {
                tracing::info!(
                    "Registering declared workspace root '{}' at '{}'",
                    slug,
                    canonical.display()
                );
            }
            seen_paths.insert(canonical.clone(), slug.clone());
            seen_slugs.insert(slug.clone());
            self.roots.push(RootConfig {
                path: canonical,
                root_type: RootType::CodeProject,
                git_aware: true,
                excludes: Vec::new(),
                slug_override: Some(slug),
                lsp_only: is_subdir,
            });
        }
        self
    }

    /// Get all lsp-only (subdirectory) roots as `(slug, path)` pairs.
    ///
    /// These are roots whose files are already covered by the primary root scan
    /// but need their own LSP working directory.
    pub fn lsp_only_roots(&self) -> Vec<(String, PathBuf)> {
        self.roots
            .iter()
            .filter(|r| r.lsp_only)
            .map(|r| (r.slug(), r.resolved_path()))
            .collect()
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

/// Returns `true` if `b` is `false`. Used as `skip_serializing_if` predicate for
/// `bool` fields that should be omitted when `false` (i.e. the default).
///
/// `serde(skip_serializing_if)` requires `fn(&T) -> bool`, so we cannot use
/// `std::ops::Not::not` directly (which takes `bool` by value, not `&bool`).
fn is_false(b: &bool) -> bool {
    !*b
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
    if (s.starts_with("~/") || s == "~")
        && let Some(home) = home_dir()
    {
        return home.join(s.strip_prefix("~/").unwrap_or(""));
    }
    path.to_path_buf()
}

/// Get the user's home directory.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Sanitize a user-supplied slug override so it is safe to use as a root_id
/// and as a filesystem directory component (e.g. in `cache_state_path()`).
///
/// Any character that is not alphanumeric or `-` (including `/`, `.`, `..`)
/// is replaced with `-` and empty segments collapsed. This prevents
/// path-traversal when the slug is used as a directory name.
///
/// e.g. `../evil` → `evil`, `my/slug` → `my-slug`, `ok-slug` → `ok-slug`
fn sanitize_slug(slug: &str) -> String {
    slug.replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
}

/// Convert a path to a URL-safe slug using the full canonical path.
/// Using the full path (not just basename) guarantees uniqueness even when
/// two worktrees share the same directory name.
/// e.g., `/Users/foo/src/my-project` -> `users-foo-src-my-project`
fn path_to_slug(path: &Path) -> String {
    // Use only the directory name (basename) so slugs are portable across
    // machines and user home directories.
    // e.g. /Users/muness/src/Innovation-Connector -> "innovation-connector"
    // NOTE: Two paths with the same basename get the same slug. For worktrees
    // with identical branch names under different projects this could collide;
    // users can set [workspace] name = "..." in .oh/config.toml to disambiguate.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    sanitize_slug(&name)
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
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let key = format!("-{}", encoded);
    Some(
        home.join(".claude")
            .join("projects")
            .join(key)
            .join("memory"),
    )
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
                lsp_only: false,
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
                lsp_only: false,
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
            lsp_only: false,
        };

        let excludes = root.effective_excludes();
        // Notes preset: .DS_Store + custom *.tmp
        assert!(excludes.contains(&".DS_Store".to_string()));
        assert!(excludes.contains(&"*.tmp".to_string()));
    }

    #[test]
    fn test_path_to_slug() {
        // Uses basename only — portable (no /Users/muness/ prefix)
        assert_eq!(
            path_to_slug(Path::new("/Users/foo/src/my-project")),
            "my-project"
        );
        assert_eq!(
            path_to_slug(Path::new("/home/user/zettelkasten")),
            "zettelkasten"
        );
        assert_eq!(
            path_to_slug(Path::new("/tmp/My Project Name")),
            "my-project-name"
        );
    }

    #[test]
    fn test_path_to_slug_uniqueness_for_same_basename() {
        // Two paths with the same basename get the SAME slug (known limitation).
        // Users can set [workspace] name = "..." in .oh/config.toml to disambiguate.
        let slug_a = path_to_slug(Path::new("/work/projectA/feat-x"));
        let slug_b = path_to_slug(Path::new("/work/projectB/feat-x"));
        assert_eq!(slug_a, slug_b); // both "feat-x" — use config name to disambiguate
    }

    #[test]
    fn test_with_worktrees_skips_nonexistent_path() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();

        // Create a fake .git/worktrees/<name>/gitdir pointing at a nonexistent path
        let wt_admin = repo_root.join(".git").join("worktrees").join("stale");
        std::fs::create_dir_all(&wt_admin).unwrap();
        std::fs::write(wt_admin.join("gitdir"), "/definitely/does/not/exist/.git").unwrap();

        let config = WorkspaceConfig::default().with_worktrees(&repo_root);
        // Stale entry should not appear in roots.
        assert!(config.roots.is_empty(), "Stale worktree must be skipped");
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
                lsp_only: false,
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
                lsp_only: false,
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
        fs::write(
            notes_root.join("journal/2024-01-01.md"),
            "# Today\nDid stuff.",
        )
        .unwrap();

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
        assert!(
            notes_state.exists(),
            "Notes scan state should be persisted at custom path"
        );
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
        let dir =
            claude_memory_dir(Path::new("/Users/foo/Downloads/improve-deployments.md")).unwrap();
        let dir_str = dir.to_string_lossy();
        assert!(
            dir_str
                .ends_with("/.claude/projects/-Users-foo-Downloads-improve-deployments-md/memory"),
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
            assert_eq!(
                resolved.len(),
                1,
                "Only primary root when memory dir missing"
            );
            return;
        }

        // Memory dir exists — verify it gets added
        let config = WorkspaceConfig::default()
            .with_primary_root(cwd.clone())
            .with_claude_memory(&cwd);
        let resolved = config.resolved_roots();
        assert!(resolved.len() >= 2, "Expected primary + memory root");
        let memory_root = resolved.iter().find(|r| r.path == memory_dir);
        assert!(
            memory_root.is_some(),
            "Memory root should be in resolved roots"
        );
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
                lsp_only: false,
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
        assert!(
            infra.is_some(),
            "Declared root 'infra' should appear in resolved roots"
        );
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
        assert_eq!(
            resolved.len(),
            2,
            "Expected primary + resolved relative root"
        );
        let infra = resolved.iter().find(|r| r.slug == "infra");
        assert!(
            infra.is_some(),
            "Declared relative root 'infra' should resolve"
        );
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
            lsp_only: false,
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
            lsp_only: false,
        };
        assert_eq!(root.slug(), "project"); // basename of /tmp/some/project
    }

    // ── Adversarial tests (dissent-seeded) ──────────────────────────

    #[test]
    fn test_with_declared_roots_two_slugs_same_path_second_skipped() {
        // Dissent finding: two declared slugs pointing at the same path.
        // The second should be silently skipped (duplicate-path detection).
        let repo_root = TempDir::new().unwrap();
        let shared_dir = TempDir::new().unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        // Both "alias1" and "alias2" point at the same directory
        fs::write(
            oh_dir.join("config.toml"),
            format!(
                "[workspace.roots]\nalias1 = \"{path}\"\nalias2 = \"{path}\"\n",
                path = shared_dir.path().display()
            ),
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        // Primary + first declared root; second is a duplicate and must be skipped
        assert_eq!(
            resolved.len(),
            2,
            "Second slug for same path must be deduped"
        );
        // The one that IS present should be one of the two declared slugs
        let declared: Vec<&str> = resolved
            .iter()
            .map(|r| r.slug.as_str())
            .filter(|s| *s == "alias1" || *s == "alias2")
            .collect();
        assert_eq!(
            declared.len(),
            1,
            "Exactly one of the two slugs should be registered"
        );
    }

    #[test]
    fn test_with_declared_roots_declared_slug_after_worktree_covers_same_path() {
        // Dissent finding: declared root slug that points to a path already
        // auto-discovered as a worktree should be deduped (same path).
        let repo_root = TempDir::new().unwrap();
        let sibling_dir = TempDir::new().unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            format!(
                "[workspace.roots]\nmy-alias = \"{}\"\n",
                sibling_dir.path().display()
            ),
        )
        .unwrap();

        // Manually add the sibling as an auto-discovered root first (simulates worktree)
        let config = WorkspaceConfig {
            roots: vec![
                RootConfig::code_project(repo_root.path().to_path_buf()),
                RootConfig::code_project(sibling_dir.path().to_path_buf()),
            ],
        }
        .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        // Primary + sibling (already present); declared "my-alias" is deduped
        assert_eq!(
            resolved.len(),
            2,
            "Declared root that duplicates worktree path must be skipped"
        );
        // The slug used is the auto-discovered path-derived slug (not the declared slug)
        assert!(
            resolved.iter().all(|r| r.slug != "my-alias"),
            "Declared slug must not override auto-discovered root's path-derived slug"
        );
    }

    #[test]
    fn test_sanitize_slug_strips_path_separators() {
        // Path traversal attempts must be neutralized
        assert_eq!(sanitize_slug("../evil"), "evil");
        assert_eq!(sanitize_slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_slug("my/slug"), "my-slug");
        assert_eq!(sanitize_slug("ok-slug"), "ok-slug");
        assert_eq!(sanitize_slug("UPPER"), "upper");
        assert_eq!(sanitize_slug("has.dots"), "has-dots");
    }

    #[test]
    fn test_slug_override_is_sanitized() {
        // slug() sanitizes the override so cache_state_path() cannot escape via path traversal
        let root = RootConfig {
            path: PathBuf::from("/tmp/project"),
            root_type: RootType::CodeProject,
            git_aware: true,
            excludes: vec![],
            slug_override: Some("../escape-attempt".to_string()),
            lsp_only: false,
        };
        // Path separator stripped — slug is safe to use as a directory component
        assert_eq!(root.slug(), "escape-attempt");
    }

    #[test]
    fn test_slug_override_empty_after_sanitization_falls_back_to_path() {
        // When slug_override sanitizes to "" (e.g. ".." or "///"), slug() must
        // fall back to the path-derived slug rather than returning an empty string.
        let root = RootConfig {
            path: PathBuf::from("/tmp/some-project"),
            root_type: RootType::CodeProject,
            git_aware: true,
            excludes: vec![],
            slug_override: Some("..".to_string()),
            lsp_only: false,
        };
        // ".." sanitizes to "" → falls back to path-derived slug
        assert_eq!(root.slug(), "some-project"); // basename of /tmp/some-project
    }

    #[test]
    fn test_with_declared_roots_tilde_path() {
        // A declared path starting with `~` must be resolved as home-relative,
        // not as `<repo_root>/~/foo`. We only run this test when HOME is set.
        let home = match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h),
            None => return,
        };

        let repo_root = TempDir::new().unwrap();

        // Use a unique temp dir under $HOME to avoid stomping on any real path.
        let tilde_subdir = match tempfile::Builder::new()
            .prefix(".rna-test-tilde-")
            .tempdir_in(&home)
        {
            Ok(d) => d,
            Err(_) => return, // Can't create temp dir in HOME — skip
        };
        // Get the dir name to build the `~/...` path
        let dir_name = tilde_subdir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            format!("[workspace.roots]\nhome-test = \"~/{}\"\n", dir_name),
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        let home_root = resolved.iter().find(|r| r.slug == "home-test");
        assert!(
            home_root.is_some(),
            "~/... declared root should resolve via HOME, not be treated as repo-relative"
        );
    }

    #[test]
    fn test_with_declared_roots_duplicate_slug_second_skipped() {
        // Two declared entries that sanitize to the same slug but point to different
        // paths — the second must be skipped.
        let repo_root = TempDir::new().unwrap();
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        // "my-slug" and "my.slug" both sanitize to "my-slug"
        fs::write(
            oh_dir.join("config.toml"),
            format!(
                "[workspace.roots]\n\"my-slug\" = \"{}\"\n\"my.slug\" = \"{}\"\n",
                dir_a.path().display(),
                dir_b.path().display()
            ),
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let resolved = config.resolved_roots();
        let my_slug_roots: Vec<_> = resolved.iter().filter(|r| r.slug == "my-slug").collect();
        assert_eq!(
            my_slug_roots.len(),
            1,
            "Duplicate sanitized slug must register exactly 1 root"
        );
    }

    // ── Subdirectory (lsp_only) root tests ──────────────────────────

    #[test]
    fn test_subdirectory_root_is_marked_lsp_only() {
        // A declared root whose path is a subdirectory of the primary root
        // must be marked `lsp_only = true` so it doesn't re-extract files.
        let repo_root = TempDir::new().unwrap();
        let client_dir = repo_root.path().join("client");
        fs::create_dir_all(&client_dir).unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        // Relative path "client" resolves to <repo_root>/client — a subdirectory
        fs::write(
            oh_dir.join("config.toml"),
            "[workspace.roots]\nclient = \"client\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        // The declared root should be present and marked lsp_only
        let client_root = config
            .roots
            .iter()
            .find(|r| r.slug_override.as_deref() == Some("client"));
        assert!(
            client_root.is_some(),
            "Declared subdirectory root 'client' must be registered"
        );
        assert!(
            client_root.unwrap().lsp_only,
            "Subdirectory root 'client' must be marked lsp_only"
        );
    }

    #[test]
    fn test_sibling_root_is_not_lsp_only() {
        // A declared root whose path is a sibling directory (not inside primary root)
        // must NOT be marked `lsp_only` — it needs its own extraction.
        let parent = TempDir::new().unwrap();
        let repo_dir = parent.path().join("my-service");
        let sibling_dir = parent.path().join("k8s-configs");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::create_dir_all(&sibling_dir).unwrap();

        let oh_dir = repo_dir.join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            "[workspace.roots]\ninfra = \"../k8s-configs\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_dir.clone())
            .with_declared_roots(&repo_dir);

        let infra_root = config
            .roots
            .iter()
            .find(|r| r.slug_override.as_deref() == Some("infra"));
        assert!(
            infra_root.is_some(),
            "Declared sibling root 'infra' must be registered"
        );
        assert!(
            !infra_root.unwrap().lsp_only,
            "Sibling root 'infra' must NOT be marked lsp_only"
        );
    }

    #[test]
    fn test_lsp_only_roots_method_returns_subdirectory_roots() {
        let repo_root = TempDir::new().unwrap();
        let client_dir = repo_root.path().join("client");
        let ai_service_dir = repo_root.path().join("ai_service");
        fs::create_dir_all(&client_dir).unwrap();
        fs::create_dir_all(&ai_service_dir).unwrap();

        let oh_dir = repo_root.path().join(".oh");
        fs::create_dir_all(&oh_dir).unwrap();
        fs::write(
            oh_dir.join("config.toml"),
            "[workspace.roots]\nclient = \"client\"\nai_service = \"ai_service\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig::default()
            .with_primary_root(repo_root.path().to_path_buf())
            .with_declared_roots(repo_root.path());

        let lsp_roots = config.lsp_only_roots();
        assert_eq!(
            lsp_roots.len(),
            2,
            "Both subdirectory roots should be lsp_only"
        );
        let slugs: Vec<&str> = lsp_roots.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            slugs.contains(&"client"),
            "client should be in lsp_only_roots"
        );
        assert!(
            slugs.contains(&"ai-service"),
            "ai_service (sanitized) should be in lsp_only_roots"
        );
    }

    // ── with_worktrees RNA-cache skip tests ────────────────────────────────

    /// A linked worktree that has its own `.oh/.cache/lance/` must NOT be
    /// added as a root by `with_worktrees()`.
    #[test]
    fn test_with_worktrees_skips_worktree_with_own_cache() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();

        // Create a worktree checkout with .git file + own RNA cache
        let wt_path = tmp.path().join("wt-with-cache");
        std::fs::create_dir_all(&wt_path).unwrap();
        std::fs::write(
            wt_path.join(".git"),
            "gitdir: ../.git/worktrees/wt-with-cache\n",
        )
        .unwrap();
        std::fs::create_dir_all(wt_path.join(".oh").join(".cache").join("lance")).unwrap();

        // Register it in .git/worktrees/<name>/gitdir
        let wt_admin = repo_root
            .join(".git")
            .join("worktrees")
            .join("wt-with-cache");
        std::fs::create_dir_all(&wt_admin).unwrap();
        std::fs::write(
            wt_admin.join("gitdir"),
            format!("{}/.git", wt_path.display()),
        )
        .unwrap();

        let config = WorkspaceConfig::default().with_worktrees(repo_root);
        assert!(
            config.roots.is_empty(),
            "Worktree with own RNA cache must be skipped by with_worktrees(); got: {:?}",
            config.roots
        );
    }

    /// A linked worktree WITHOUT its own cache must still be added as a root.
    #[test]
    fn test_with_worktrees_includes_worktree_without_cache() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();

        // Create a worktree checkout with .git file but NO cache
        let wt_path = tmp.path().join("wt-no-cache");
        std::fs::create_dir_all(&wt_path).unwrap();
        std::fs::write(
            wt_path.join(".git"),
            "gitdir: ../.git/worktrees/wt-no-cache\n",
        )
        .unwrap();
        // Note: no .oh/.cache/lance/ created

        // Register it in .git/worktrees/<name>/gitdir
        let wt_admin = repo_root.join(".git").join("worktrees").join("wt-no-cache");
        std::fs::create_dir_all(&wt_admin).unwrap();
        std::fs::write(
            wt_admin.join("gitdir"),
            format!("{}/.git", wt_path.display()),
        )
        .unwrap();

        let config = WorkspaceConfig::default().with_worktrees(repo_root);
        assert_eq!(
            config.roots.len(),
            1,
            "Worktree WITHOUT its own cache must be added as a root; got: {:?}",
            config.roots
        );
        assert_eq!(config.roots[0].resolved_path(), wt_path);
    }
}
