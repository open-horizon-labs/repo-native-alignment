//! Business-context producer admission and disposable-cache compatibility.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const CACHE_MODE_MARKER: &str = "business-context-mode";

/// Controls whether RNA-specific business and Git-history producers may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusinessContextMode {
    #[default]
    Enabled,
    Disabled,
}

impl BusinessContextMode {
    pub fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

impl fmt::Display for BusinessContextMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enabled => f.write_str("enabled"),
            Self::Disabled => f.write_str("disabled"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseBusinessContextModeError(String);

impl fmt::Display for ParseBusinessContextModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid business context mode {:?}; expected enabled or disabled",
            self.0
        )
    }
}

impl std::error::Error for ParseBusinessContextModeError {}

impl FromStr for BusinessContextMode {
    type Err = ParseBusinessContextModeError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "enabled" => Ok(Self::Enabled),
            "disabled" => Ok(Self::Disabled),
            other => Err(ParseBusinessContextModeError(other.to_owned())),
        }
    }
}

/// Counts decisions made at producer-admission time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessContextExclusionCounts {
    pub business_artifact_files: usize,
    pub git_history_producers: usize,
}

/// Shared producer-admission policy and diagnostics accumulator.
#[derive(Debug, Clone)]
pub struct BusinessContextAdmission {
    mode: BusinessContextMode,
    counts: Arc<RwLock<BusinessContextExclusionCounts>>,
}

impl Default for BusinessContextAdmission {
    fn default() -> Self {
        Self::new(BusinessContextMode::Enabled)
    }
}

impl BusinessContextAdmission {
    pub fn new(mode: BusinessContextMode) -> Self {
        Self {
            mode,
            counts: Arc::new(RwLock::new(BusinessContextExclusionCounts::default())),
        }
    }

    pub fn mode(&self) -> BusinessContextMode {
        self.mode
    }

    pub fn counts(&self) -> BusinessContextExclusionCounts {
        *self
            .counts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Return whether a repository file may be produced, recording disabled `.oh` paths.
    pub fn admit_repository_file(&self, path: &Path) -> bool {
        if !self.mode.is_disabled() || !path_has_component(path, ".oh") {
            return true;
        }

        let mut counts = self
            .counts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts.business_artifact_files = counts.business_artifact_files.saturating_add(1);
        false
    }

    /// Remove `.oh` paths before a producer constructs an extraction event.
    pub fn retain_repository_files(&self, files: &mut Vec<PathBuf>) -> usize {
        let before = files.len();
        files.retain(|path| self.admit_repository_file(path));
        before.saturating_sub(files.len())
    }

    /// Return whether a Git-history producer may run, recording disabled decisions.
    pub fn admit_git_history_producer(&self) -> bool {
        if !self.mode.is_disabled() {
            return true;
        }

        let mut counts = self
            .counts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts.git_history_producers = counts.git_history_producers.saturating_add(1);
        false
    }

    /// Validate the exact disposable repo cache before any cache-backed read.
    pub fn prepare_cache(&self, repo_root: &Path) -> Result<CacheModeDisposition> {
        prepare_cache_for_mode(repo_root, self.mode)
    }

    /// Admit an already-built cache without creating, deleting, or rewriting it.
    pub fn validate_existing_cache(&self, repo_root: &Path) -> Result<()> {
        validate_existing_cache_for_mode(repo_root, self.mode)
    }
}

/// Result of validating the disposable cache's selected context mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheModeDisposition {
    Compatible,
    Initialized,
    Rebuilt { previous: Option<String> },
}

impl CacheModeDisposition {
    pub fn rebuilt(&self) -> bool {
        matches!(self, Self::Rebuilt { .. })
    }

    pub fn requires_fresh_graph(&self) -> bool {
        matches!(self, Self::Initialized | Self::Rebuilt { .. })
    }
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components().any(
        |component| matches!(component, Component::Normal(value) if value == OsStr::new(expected)),
    )
}

fn validated_cache_path(repo_root: &Path) -> Result<PathBuf> {
    if !repo_root.is_dir() {
        bail!(
            "repository root is not a directory: {}",
            repo_root.display()
        );
    }

    let oh_dir = repo_root.join(".oh");
    if let Ok(metadata) = std::fs::symlink_metadata(&oh_dir)
        && metadata.file_type().is_symlink()
    {
        bail!(
            "refusing to manage disposable cache through symlinked .oh directory: {}",
            oh_dir.display()
        );
    }

    let cache_dir = oh_dir.join(".cache");
    if cache_dir.file_name() != Some(OsStr::new(".cache"))
        || cache_dir.parent().and_then(Path::file_name) != Some(OsStr::new(".oh"))
    {
        bail!(
            "refusing unsafe disposable cache path: {}",
            cache_dir.display()
        );
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&cache_dir)
        && metadata.file_type().is_symlink()
    {
        bail!(
            "refusing to delete symlinked disposable cache: {}",
            cache_dir.display()
        );
    }
    Ok(cache_dir)
}

fn write_mode_marker(cache_dir: &Path, mode: BusinessContextMode) -> Result<()> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("failed to create cache directory {}", cache_dir.display()))?;
    let marker = cache_dir.join(CACHE_MODE_MARKER);
    let temporary = cache_dir.join(format!("{CACHE_MODE_MARKER}.tmp"));
    std::fs::write(&temporary, format!("{mode}\n"))
        .with_context(|| format!("failed to write cache mode marker {}", temporary.display()))?;
    std::fs::rename(&temporary, &marker)
        .with_context(|| format!("failed to install cache mode marker {}", marker.display()))?;
    Ok(())
}

fn prepare_cache_for_mode(
    repo_root: &Path,
    mode: BusinessContextMode,
) -> Result<CacheModeDisposition> {
    let cache_dir = validated_cache_path(repo_root)?;
    if !cache_dir.exists() {
        write_mode_marker(&cache_dir, mode)?;
        return Ok(CacheModeDisposition::Initialized);
    }

    let marker = cache_dir.join(CACHE_MODE_MARKER);
    let persisted = std::fs::read_to_string(&marker).ok();
    if persisted
        .as_deref()
        .map(str::trim)
        .and_then(|value| value.parse::<BusinessContextMode>().ok())
        == Some(mode)
    {
        return Ok(CacheModeDisposition::Compatible);
    }

    std::fs::remove_dir_all(&cache_dir).with_context(|| {
        format!(
            "failed to delete incompatible disposable cache {}",
            cache_dir.display()
        )
    })?;
    write_mode_marker(&cache_dir, mode)?;
    Ok(CacheModeDisposition::Rebuilt {
        previous: persisted.map(|value| value.trim().to_owned()),
    })
}

fn validate_existing_cache_for_mode(repo_root: &Path, mode: BusinessContextMode) -> Result<()> {
    let cache_dir = validated_cache_path(repo_root)?;
    anyhow::ensure!(
        cache_dir.is_dir(),
        "cache-only mode requires an existing cache at {}",
        cache_dir.display()
    );
    let marker = cache_dir.join(CACHE_MODE_MARKER);
    let persisted = std::fs::read_to_string(&marker)
        .with_context(|| format!("cache-only mode requires {}", marker.display()))?;
    let observed = persisted
        .trim()
        .parse::<BusinessContextMode>()
        .with_context(|| {
            format!(
                "invalid cache business-context marker at {}",
                marker.display()
            )
        })?;
    anyhow::ensure!(
        observed == mode,
        "cache business-context mode mismatch: requested {mode}, persisted {observed}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_is_strict() {
        assert_eq!(
            "disabled".parse::<BusinessContextMode>().unwrap(),
            BusinessContextMode::Disabled
        );
        assert!("DISABLED".parse::<BusinessContextMode>().is_err());
        assert!("unknown".parse::<BusinessContextMode>().is_err());
    }

    #[test]
    fn disabled_admission_removes_only_dot_oh_components() {
        let admission = BusinessContextAdmission::new(BusinessContextMode::Disabled);
        let mut files = vec![
            PathBuf::from(".oh/outcomes/leak.md"),
            PathBuf::from("docs/.oh/guardrail.md"),
            PathBuf::from("docs/oh/guide.md"),
            PathBuf::from("README.md"),
        ];

        assert_eq!(admission.retain_repository_files(&mut files), 2);
        assert_eq!(
            files,
            vec![
                PathBuf::from("docs/oh/guide.md"),
                PathBuf::from("README.md")
            ]
        );
        assert!(!admission.admit_git_history_producer());
        assert_eq!(
            admission.counts(),
            BusinessContextExclusionCounts {
                business_artifact_files: 2,
                git_history_producers: 1,
            }
        );
    }

    #[test]
    fn single_file_admission_shares_dot_oh_policy_and_counts() {
        let admission = BusinessContextAdmission::new(BusinessContextMode::Disabled);

        assert!(admission.admit_repository_file(Path::new("README.md")));
        assert!(admission.admit_repository_file(Path::new("docs/oh/guide.md")));
        assert!(!admission.admit_repository_file(Path::new(".oh/outcomes/leak.md")));
        assert!(!admission.admit_repository_file(Path::new("docs/.oh/leak.md")));
        assert_eq!(admission.counts().business_artifact_files, 2);
    }

    #[test]
    fn cache_disposition_separates_deletion_from_fresh_graph_requirements() {
        assert!(!CacheModeDisposition::Compatible.rebuilt());
        assert!(!CacheModeDisposition::Compatible.requires_fresh_graph());

        assert!(!CacheModeDisposition::Initialized.rebuilt());
        assert!(CacheModeDisposition::Initialized.requires_fresh_graph());

        let rebuilt = CacheModeDisposition::Rebuilt {
            previous: Some("enabled".to_string()),
        };
        assert!(rebuilt.rebuilt());
        assert!(rebuilt.requires_fresh_graph());
    }

    #[test]
    fn incompatible_or_legacy_cache_is_rebuilt_exactly() {
        let repo = tempfile::tempdir().unwrap();
        let admission = BusinessContextAdmission::new(BusinessContextMode::Disabled);
        let cache = repo.path().join(".oh/.cache");
        std::fs::create_dir_all(cache.join("lance")).unwrap();
        std::fs::write(cache.join("lance/sentinel"), "legacy").unwrap();

        assert!(admission.prepare_cache(repo.path()).unwrap().rebuilt());
        assert!(!cache.join("lance/sentinel").exists());
        assert_eq!(
            std::fs::read_to_string(cache.join(CACHE_MODE_MARKER)).unwrap(),
            "disabled\n"
        );
        assert_eq!(
            admission.prepare_cache(repo.path()).unwrap(),
            CacheModeDisposition::Compatible
        );

        let enabled = BusinessContextAdmission::new(BusinessContextMode::Enabled);
        std::fs::write(cache.join("disabled-only-row"), "derived").unwrap();
        assert!(enabled.prepare_cache(repo.path()).unwrap().rebuilt());
        assert!(!cache.join("disabled-only-row").exists());
        assert_eq!(
            std::fs::read_to_string(cache.join(CACHE_MODE_MARKER)).unwrap(),
            "enabled\n"
        );
    }

    #[test]
    fn existing_cache_validation_is_fail_closed_and_non_mutating() {
        let repo = tempfile::tempdir().unwrap();
        let admission = BusinessContextAdmission::new(BusinessContextMode::Disabled);
        let cache = repo.path().join(".oh/.cache");

        assert!(admission.validate_existing_cache(repo.path()).is_err());
        assert!(!cache.exists());

        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(CACHE_MODE_MARKER), "enabled\n").unwrap();
        std::fs::write(cache.join("preserved"), "evidence").unwrap();
        assert!(admission.validate_existing_cache(repo.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(cache.join("preserved")).unwrap(),
            "evidence"
        );
        assert_eq!(
            std::fs::read_to_string(cache.join(CACHE_MODE_MARKER)).unwrap(),
            "enabled\n"
        );

        std::fs::write(cache.join(CACHE_MODE_MARKER), "disabled\n").unwrap();
        admission.validate_existing_cache(repo.path()).unwrap();
    }
}
