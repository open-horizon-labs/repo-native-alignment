use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Repository};

use crate::extract::lsp::requested_operations_for_node;
use crate::graph::{Node, NodeKind};

pub(crate) const MAX_CHANGED_LSP_NODES: usize = 4_096;
pub(crate) const MAX_CHANGED_LSP_OPERATIONS: usize = 12_288;

const SCOPE_HELP: &str = "use `--scope root --root <slug>` or `--scope repo` when bounded changed-file planning is unavailable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFileProvenance {
    pub repo: PathBuf,
    pub root: PathBuf,
    pub base_ref: String,
    pub target_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ChangedFileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ChangedFile {
    pub kind: ChangedFileKind,
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedLspNode {
    pub stable_id: String,
    pub file: PathBuf,
    pub requested_operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFilePlan {
    pub provenance: ChangedFileProvenance,
    pub changes: Vec<ChangedFile>,
    pub planned_nodes: Vec<PlannedLspNode>,
    pub deleted_files: Vec<PathBuf>,
    pub renamed_files: Vec<(PathBuf, PathBuf)>,
    pub unmapped_files: Vec<PathBuf>,
    pub operation_count: usize,
}

impl ChangedFilePlan {
    pub fn planned_node_ids(&self) -> Arc<HashSet<String>> {
        Arc::new(
            self.planned_nodes
                .iter()
                .map(|node| node.stable_id.clone())
                .collect(),
        )
    }

    pub fn render_progress(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Changed-file plan: {} -> {} in {} (root {})",
            self.provenance.base_ref,
            self.provenance.target_ref,
            self.provenance.repo.display(),
            self.provenance.root.display()
        )];
        lines.push(format!(
            "Changed-file plan: {} change(s), {} node(s), {} requested operation(s)",
            self.changes.len(),
            self.planned_nodes.len(),
            self.operation_count
        ));
        for (old, new) in &self.renamed_files {
            lines.push(format!(
                "Changed-file diagnostic: renamed {} -> {}",
                old.display(),
                new.display()
            ));
        }
        for file in &self.deleted_files {
            lines.push(format!(
                "Changed-file diagnostic: deleted {} (no LSP work scheduled)",
                file.display()
            ));
        }
        for file in &self.unmapped_files {
            lines.push(format!(
                "Changed-file diagnostic: unmapped {} (no eligible cached node)",
                file.display()
            ));
        }
        lines
    }

    fn require_schedulable(self) -> Result<Self> {
        if !self.planned_nodes.is_empty() {
            return Ok(self);
        }

        let diagnostics = self
            .render_progress()
            .into_iter()
            .skip(2)
            .collect::<Vec<_>>()
            .join("; ");
        let detail = if diagnostics.is_empty() {
            "no changed paths mapped to eligible cached nodes".to_string()
        } else {
            diagnostics
        };
        anyhow::bail!("changed-file LSP plan is empty: {detail}; {SCOPE_HELP}")
    }
}

#[derive(Debug)]
pub(crate) struct ChangedFilePlanInput<'a> {
    pub provenance: ChangedFileProvenance,
    pub root_slug: &'a str,
    pub changes: Vec<ChangedFile>,
    pub cached_nodes: &'a [Node],
    pub max_nodes: usize,
    pub max_operations: usize,
}

pub(crate) fn discover_and_plan_changed_files(
    repo_root: &Path,
    root_slug: &str,
    cached_nodes: &[Node],
) -> Result<ChangedFilePlan> {
    let (provenance, changes) = discover_git_worktree_changes(repo_root)?;
    if changes.is_empty() {
        anyhow::bail!(
            "changed-file LSP planning found no HEAD-to-working-tree changes; {SCOPE_HELP}"
        );
    }
    plan_changed_files(ChangedFilePlanInput {
        provenance,
        root_slug,
        changes,
        cached_nodes,
        max_nodes: MAX_CHANGED_LSP_NODES,
        max_operations: MAX_CHANGED_LSP_OPERATIONS,
    })?
    .require_schedulable()
}

pub(crate) fn plan_changed_files(input: ChangedFilePlanInput<'_>) -> Result<ChangedFilePlan> {
    let mut present_files = BTreeSet::new();
    let mut deleted_files = BTreeSet::new();
    let mut renamed_files = BTreeSet::new();

    let mut changes = input.changes;
    changes.sort();
    changes.dedup();

    for change in &changes {
        match change.kind {
            ChangedFileKind::Deleted => {
                if let Some(path) = change.old_path.as_deref() {
                    deleted_files.insert(normalize_relative(path)?);
                }
            }
            ChangedFileKind::Renamed => {
                let old = change
                    .old_path
                    .as_deref()
                    .context("renamed change is missing its old path")?;
                let new = change
                    .new_path
                    .as_deref()
                    .context("renamed change is missing its new path")?;
                let old = normalize_relative(old)?;
                let new = normalize_relative(new)?;
                renamed_files.insert((old, new.clone()));
                present_files.insert(new);
            }
            _ => {
                let path = change
                    .new_path
                    .as_deref()
                    .or(change.old_path.as_deref())
                    .context("present changed file has no path")?;
                present_files.insert(normalize_relative(path)?);
            }
        }
    }

    let supported_languages =
        crate::extract::EnricherRegistry::with_builtins().supported_languages();
    let mut nodes_by_file: BTreeMap<PathBuf, Vec<&Node>> = BTreeMap::new();
    for node in input.cached_nodes {
        if node.id.root != input.root_slug
            || node.language.is_empty()
            || !supported_languages.contains(&node.language)
        {
            continue;
        }
        let file = normalize_relative(&node.id.file)?;
        if present_files.contains(&file) {
            nodes_by_file.entry(file).or_default().push(node);
        }
    }

    let mut planned_nodes = Vec::new();
    let mut seen_node_ids = BTreeSet::new();
    let mut unmapped_files = Vec::new();
    let mut operation_count = 0usize;

    for file in present_files {
        let mut mapped = false;
        if let Some(nodes) = nodes_by_file.get_mut(&file) {
            nodes.sort_by_key(|node| node.stable_id());
            for node in nodes.iter() {
                if matches!(&node.id.kind, NodeKind::Other(kind) if kind == "diagnostic") {
                    continue;
                }
                let requested_operations = requested_operations_for_node(&node.id.kind, true, true)
                    .into_iter()
                    .filter(|operation| *operation != "skipped_no_supported_operation")
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if requested_operations.is_empty() {
                    continue;
                }
                let stable_id = node.stable_id();
                if !seen_node_ids.insert(stable_id.clone()) {
                    continue;
                }
                mapped = true;
                operation_count = operation_count
                    .checked_add(requested_operations.len())
                    .context("changed-file LSP operation count overflowed")?;
                if planned_nodes.len() + 1 > input.max_nodes
                    || operation_count > input.max_operations
                {
                    anyhow::bail!(
                        "changed-file LSP plan exceeds its bound (max {} nodes / {} operations); {SCOPE_HELP}",
                        input.max_nodes,
                        input.max_operations
                    );
                }
                planned_nodes.push(PlannedLspNode {
                    stable_id,
                    file: file.clone(),
                    requested_operations,
                });
            }
        }
        if !mapped {
            unmapped_files.push(file);
        }
    }

    planned_nodes.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
    Ok(ChangedFilePlan {
        provenance: input.provenance,
        changes,
        planned_nodes,
        deleted_files: deleted_files.into_iter().collect(),
        renamed_files: renamed_files.into_iter().collect(),
        unmapped_files,
        operation_count,
    })
}

fn discover_git_worktree_changes(
    repo_root: &Path,
) -> Result<(ChangedFileProvenance, Vec<ChangedFile>)> {
    let canonical_root = repo_root
        .canonicalize()
        .with_context(|| format!("failed to resolve repository root {}", repo_root.display()))?;
    let repo = Repository::discover(&canonical_root).with_context(|| {
        format!(
            "changed-file LSP planning requires a git worktree at {}; {SCOPE_HELP}",
            canonical_root.display()
        )
    })?;
    if repo.is_bare() {
        anyhow::bail!("changed-file LSP planning requires a non-bare git worktree; {SCOPE_HELP}");
    }
    let workdir = repo
        .workdir()
        .context("git repository has no worktree")?
        .canonicalize()
        .context("failed to resolve git worktree")?;
    let root_prefix = canonical_root.strip_prefix(&workdir).with_context(|| {
        format!(
            "root {} is outside git worktree {}; {SCOPE_HELP}",
            canonical_root.display(),
            workdir.display()
        )
    })?;
    let head = repo
        .head()
        .and_then(|reference| reference.peel_to_commit())
        .with_context(|| {
            format!("changed-file LSP planning requires a resolved HEAD; {SCOPE_HELP}")
        })?;
    let head_tree = head.tree().context("failed to load HEAD tree")?;
    let head_name = repo
        .head()
        .ok()
        .and_then(|reference| reference.name().ok().map(str::to_string))
        .unwrap_or_else(|| "HEAD".to_string());

    let mut changes = BTreeSet::new();

    let mut staged_options = DiffOptions::new();
    staged_options.include_typechange(true);
    let mut staged = repo
        .diff_tree_to_index(Some(&head_tree), None, Some(&mut staged_options))
        .context("failed to diff HEAD against git index")?;
    detect_renames(&mut staged)?;
    collect_diff_changes(&staged, root_prefix, &mut changes)?;

    let mut worktree_options = DiffOptions::new();
    worktree_options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_typechange(true);
    let mut worktree = repo
        .diff_index_to_workdir(None, Some(&mut worktree_options))
        .context("failed to diff git index against worktree")?;
    detect_renames(&mut worktree)?;
    collect_diff_changes(&worktree, root_prefix, &mut changes)?;

    Ok((
        ChangedFileProvenance {
            repo: workdir,
            root: canonical_root,
            base_ref: format!("{}@{}", head_name, head.id()),
            target_ref: "working-tree".to_string(),
        },
        changes.into_iter().collect(),
    ))
}

fn detect_renames(diff: &mut Diff<'_>) -> Result<()> {
    let mut options = DiffFindOptions::new();
    options.renames(true).copies(true);
    diff.find_similar(Some(&mut options))
        .context("failed to detect renamed changed files")
}

fn collect_diff_changes(
    diff: &Diff<'_>,
    root_prefix: &Path,
    changes: &mut BTreeSet<ChangedFile>,
) -> Result<()> {
    for delta in diff.deltas() {
        let Some(kind) = changed_kind(delta.status()) else {
            continue;
        };
        let old_path = delta
            .old_file()
            .path()
            .and_then(|path| path.strip_prefix(root_prefix).ok())
            .map(normalize_relative)
            .transpose()?;
        let new_path = delta
            .new_file()
            .path()
            .and_then(|path| path.strip_prefix(root_prefix).ok())
            .map(normalize_relative)
            .transpose()?;
        if old_path.is_none() && new_path.is_none() {
            continue;
        }
        changes.insert(ChangedFile {
            kind,
            old_path,
            new_path,
        });
    }
    Ok(())
}

fn changed_kind(delta: Delta) -> Option<ChangedFileKind> {
    match delta {
        Delta::Added => Some(ChangedFileKind::Added),
        Delta::Modified => Some(ChangedFileKind::Modified),
        Delta::Deleted => Some(ChangedFileKind::Deleted),
        Delta::Renamed => Some(ChangedFileKind::Renamed),
        Delta::Copied => Some(ChangedFileKind::Copied),
        Delta::Typechange => Some(ChangedFileKind::TypeChanged),
        Delta::Untracked => Some(ChangedFileKind::Untracked),
        Delta::Conflicted | Delta::Unreadable => Some(ChangedFileKind::Modified),
        Delta::Unmodified | Delta::Ignored => None,
    }
}

fn normalize_relative(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "changed-file path {} is not repository-relative; {SCOPE_HELP}",
                    path.display()
                );
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        anyhow::bail!("changed-file path is empty; {SCOPE_HELP}");
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::graph::{ExtractionSource, NodeId};

    fn node(file: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 2,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn provenance() -> ChangedFileProvenance {
        ChangedFileProvenance {
            repo: PathBuf::from("/repo"),
            root: PathBuf::from("/repo"),
            base_ref: "refs/heads/main@abc".to_string(),
            target_ref: "working-tree".to_string(),
        }
    }

    #[test]
    fn one_file_change_never_schedules_unrelated_nodes() {
        let nodes = vec![
            node("src/changed.rs", "changed", NodeKind::Function),
            node("src/unrelated.rs", "unrelated", NodeKind::Function),
            node("src/changed.rs", "Thing", NodeKind::Struct),
        ];
        let plan = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes: vec![ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: Some(PathBuf::from("src/changed.rs")),
                new_path: Some(PathBuf::from("src/changed.rs")),
            }],
            cached_nodes: &nodes,
            max_nodes: 16,
            max_operations: 48,
        })
        .unwrap();

        let ids = plan.planned_node_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| id.contains("src/changed.rs")));
        assert!(!ids.iter().any(|id| id.contains("unrelated")));
        assert_eq!(plan.operation_count, 4);
    }

    #[test]
    fn rename_delete_and_unmapped_diagnostics_are_sorted_and_deterministic() {
        let nodes = vec![node("src/new.rs", "moved", NodeKind::Function)];
        let changes = vec![
            ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: Some(PathBuf::from("src/z.rs")),
                new_path: Some(PathBuf::from("src/z.rs")),
            },
            ChangedFile {
                kind: ChangedFileKind::Deleted,
                old_path: Some(PathBuf::from("src/deleted.rs")),
                new_path: None,
            },
            ChangedFile {
                kind: ChangedFileKind::Renamed,
                old_path: Some(PathBuf::from("src/old.rs")),
                new_path: Some(PathBuf::from("src/new.rs")),
            },
        ];
        let plan = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes,
            cached_nodes: &nodes,
            max_nodes: 16,
            max_operations: 48,
        })
        .unwrap();

        assert_eq!(
            plan.renamed_files,
            vec![(PathBuf::from("src/old.rs"), PathBuf::from("src/new.rs"))]
        );
        assert_eq!(plan.deleted_files, vec![PathBuf::from("src/deleted.rs")]);
        assert_eq!(plan.unmapped_files, vec![PathBuf::from("src/z.rs")]);
        assert_eq!(plan.planned_nodes.len(), 1);
    }

    #[test]
    fn planner_rejects_node_or_operation_fanout_over_bound() {
        let nodes = vec![
            node("src/changed.rs", "first", NodeKind::Function),
            node("src/changed.rs", "second", NodeKind::Function),
        ];
        let error = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes: vec![ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: None,
                new_path: Some(PathBuf::from("src/changed.rs")),
            }],
            cached_nodes: &nodes,
            max_nodes: 1,
            max_operations: 3,
        })
        .unwrap_err();

        assert!(error.to_string().contains("exceeds its bound"));
        assert!(error.to_string().contains("--scope root"));
    }

    #[test]
    fn unsupported_language_is_reported_as_unmapped_not_scheduled() {
        let mut unsupported = node("src/changed.wat", "changed", NodeKind::Function);
        unsupported.language = "wat".to_string();
        let plan = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes: vec![ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: None,
                new_path: Some(PathBuf::from("src/changed.wat")),
            }],
            cached_nodes: &[unsupported],
            max_nodes: 16,
            max_operations: 48,
        })
        .unwrap();

        assert!(plan.planned_nodes.is_empty());
        assert_eq!(plan.unmapped_files, vec![PathBuf::from("src/changed.wat")]);
    }

    #[test]
    fn non_git_discovery_rejects_with_scope_help() {
        let dir = tempfile::tempdir().unwrap();
        let error = discover_and_plan_changed_files(dir.path(), "fixture", &[]).unwrap_err();
        assert!(error.to_string().contains("requires a git worktree"));
        assert!(error.to_string().contains("--scope repo"));
    }

    #[test]
    fn git_discovery_records_head_to_worktree_provenance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/changed.rs"), "fn changed() {}\n").unwrap();
        std::fs::write(dir.path().join("src/unrelated.rs"), "fn unrelated() {}\n").unwrap();

        let repo = Repository::init(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("src/changed.rs")).unwrap();
        index.add_path(Path::new("src/unrelated.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("RNA test", "rna@example.invalid").unwrap();
        let commit_id = repo
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        drop(tree);

        std::fs::write(
            dir.path().join("src/changed.rs"),
            "fn changed() { println!(\"changed\"); }\n",
        )
        .unwrap();
        let nodes = vec![
            node("src/changed.rs", "changed", NodeKind::Function),
            node("src/unrelated.rs", "unrelated", NodeKind::Function),
        ];

        let plan = discover_and_plan_changed_files(dir.path(), "fixture", &nodes).unwrap();
        assert!(plan.provenance.base_ref.ends_with(&commit_id.to_string()));
        assert_eq!(plan.provenance.target_ref, "working-tree");
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.planned_nodes.len(), 1);
        assert!(plan.planned_nodes[0].stable_id.contains("src/changed.rs"));
    }
}
