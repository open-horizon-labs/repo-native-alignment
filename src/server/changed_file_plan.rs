use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Repository};

use crate::extract::lsp::{
    MAX_INCREMENTAL_LSP_NODES, MAX_INCREMENTAL_LSP_OPERATIONS, planned_operations_for_node,
    planned_operations_for_node_with_broad_references,
};
use crate::graph::Node;

pub(crate) const MAX_CHANGED_LSP_NODES: usize = MAX_INCREMENTAL_LSP_NODES;
pub(crate) const MAX_CHANGED_LSP_OPERATIONS: usize = MAX_INCREMENTAL_LSP_OPERATIONS;

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
    pub allow_broad_references: bool,
}

#[cfg(test)]
pub(crate) fn discover_and_plan_changed_files(
    repo_root: &Path,
    root_slug: &str,
    cached_nodes: &[Node],
) -> Result<ChangedFilePlan> {
    discover_and_plan_changed_files_inner(repo_root, root_slug, cached_nodes, false)
}

pub(crate) fn discover_and_plan_changed_files_with_broad_references(
    repo_root: &Path,
    root_slug: &str,
    cached_nodes: &[Node],
) -> Result<ChangedFilePlan> {
    discover_and_plan_changed_files_inner(repo_root, root_slug, cached_nodes, true)
}

fn discover_and_plan_changed_files_inner(
    repo_root: &Path,
    root_slug: &str,
    cached_nodes: &[Node],
    allow_broad_references: bool,
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
        allow_broad_references,
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
            || node.source == crate::graph::ExtractionSource::Lsp
            || crate::ranking::is_test_function(node)
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
                let requested_operations = if input.allow_broad_references {
                    planned_operations_for_node_with_broad_references(node)
                } else {
                    planned_operations_for_node(node)
                };
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

pub(crate) fn plan_lsp_node_ids_for_touched_files(
    touched_files: &HashSet<(String, PathBuf)>,
    cached_nodes: &[Node],
) -> Result<Arc<HashSet<String>>> {
    plan_lsp_node_ids_for_touched_files_with_partition_rebuilds(
        touched_files,
        cached_nodes,
        &BTreeSet::new(),
    )
}

pub(crate) fn plan_lsp_node_ids_for_touched_files_with_partition_rebuilds(
    touched_files: &HashSet<(String, PathBuf)>,
    cached_nodes: &[Node],
    rebuilt_partitions: &BTreeSet<String>,
) -> Result<Arc<HashSet<String>>> {
    plan_lsp_node_ids_for_touched_files_with_bounds(
        touched_files,
        cached_nodes,
        rebuilt_partitions,
        ChangedFileNodeBound::Enforce,
    )
}

pub(crate) fn plan_lsp_node_ids_for_verified_structural_cache(
    authorization: &crate::structural_cache::VerifiedStructuralCacheAuthorization,
    plan: &crate::structural_cache::IncrementalImpactPlan,
    cached_nodes: &[Node],
) -> Result<Arc<HashSet<String>>> {
    crate::structural_cache::validate_runtime_plan_handoff(authorization, plan)?;
    let signed_budget = authorization.signed_operation_budget()?;
    let touched_files = plan
        .executed_paths
        .iter()
        .cloned()
        .map(|path| (authorization.authorization.root_slug.clone(), path))
        .collect::<HashSet<_>>();
    plan_lsp_node_ids_for_touched_files_with_bounds(
        &touched_files,
        cached_nodes,
        &plan.escalated_partitions,
        ChangedFileNodeBound::AuthorizedStructuralCache {
            signed_operation_count: usize::try_from(signed_budget.executed_estimate)
                .context("signed structural-cache operation estimate does not fit usize")?,
            authorized_operations_by_language: signed_budget
                .authorized_operations_by_language
                .clone(),
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChangedFileNodeBound {
    Enforce,
    AuthorizedStructuralCache {
        signed_operation_count: usize,
        authorized_operations_by_language: BTreeMap<String, Vec<String>>,
    },
}

fn authorized_operations_for_node(node: &Node, authorized: &[String]) -> Vec<String> {
    let mut requested = planned_operations_for_node(node);
    if node.id.kind == crate::graph::NodeKind::Function
        && requested
            .iter()
            .any(|operation| operation == "call_hierarchy")
        && authorized
            .binary_search_by(|operation| operation.as_str().cmp("call_hierarchy"))
            .is_err()
        && authorized
            .binary_search_by(|operation| operation.as_str().cmp("references"))
            .is_ok()
    {
        // Runtime uses references when the negotiated server capabilities do
        // not include call hierarchy. The signed operation set records those
        // negotiated operations, so preserve the same fallback at the
        // verifier-to-runtime planning handoff.
        requested.retain(|operation| operation != "call_hierarchy");
        requested.push("references".to_string());
    }
    requested.retain(|operation| authorized.binary_search(operation).is_ok());
    requested
}

fn plan_lsp_node_ids_for_touched_files_with_bounds(
    touched_files: &HashSet<(String, PathBuf)>,
    cached_nodes: &[Node],
    rebuilt_partitions: &BTreeSet<String>,
    node_bound: ChangedFileNodeBound,
) -> Result<Arc<HashSet<String>>> {
    let supported_languages =
        crate::extract::EnricherRegistry::with_builtins().supported_languages();
    let touched_roots: HashSet<&str> = touched_files
        .iter()
        .map(|(root, _)| root.as_str())
        .collect();
    let mut planned_node_ids = HashSet::new();
    let mut planned_languages = BTreeSet::new();
    let mut document_symbol_files = BTreeSet::<(String, String, PathBuf)>::new();
    let mut eligible_rebuild_node_ids = BTreeMap::<String, BTreeSet<String>>::new();
    let mut operation_count = 0usize;

    for node in cached_nodes {
        if !touched_roots.contains(node.id.root.as_str())
            || node.id.file.as_os_str().is_empty()
            || node.language.is_empty()
            || !supported_languages.contains(&node.language)
            // LSP-produced nodes are persisted results, not query seeds. They
            // may remain in the graph as evidence and edge endpoints, but
            // scheduling them recursively re-queries carried output and
            // inflates the operation bound with work that is not executable.
            || node.source == crate::graph::ExtractionSource::Lsp
        {
            continue;
        }
        let file = normalize_relative(&node.id.file)?;
        let touched = touched_files.contains(&(node.id.root.clone(), file.clone()));
        let stable_id = node.stable_id();
        let mut requested_operations = planned_operations_for_node(node);
        if let ChangedFileNodeBound::AuthorizedStructuralCache {
            authorized_operations_by_language,
            ..
        } = &node_bound
        {
            let authorized = authorized_operations_by_language
                .get(&node.language)
                .context("signed structural-cache operation budget lacks a target language")?;
            if touched
                && authorized
                    .binary_search_by(|operation| operation.as_str().cmp("document_symbols"))
                    .is_ok()
            {
                let key = (node.id.root.clone(), node.language.clone(), file.clone());
                document_symbol_files.insert(key);
            }
            requested_operations = authorized_operations_for_node(node, authorized);
        }
        if crate::ranking::is_test_function(node) || requested_operations.is_empty() {
            continue;
        }
        if rebuilt_partitions.contains(&node.language) {
            eligible_rebuild_node_ids
                .entry(node.language.clone())
                .or_default()
                .insert(stable_id.clone());
        }
        if !touched || !planned_node_ids.insert(stable_id) {
            continue;
        }
        planned_languages.insert(node.language.clone());
        operation_count = operation_count
            .checked_add(requested_operations.len())
            .context("changed-file LSP operation count overflowed")?;
    }

    // Runtime Pass 1 emits one document-symbol work item for each admitted file.
    // Count the signed per-file document-symbol work independently of query
    // node IDs. Endpoint-only source/test context is attached by the language
    // accumulator without making those nodes schedulable.
    if !document_symbol_files.is_empty() {
        let document_symbol_count = document_symbol_files.len();
        for (_root, language, _file) in document_symbol_files {
            planned_languages.insert(language);
        }
        operation_count = operation_count
            .checked_add(document_symbol_count)
            .context("changed-file LSP document-symbol operation count overflowed")?;
    }

    for (partition, expected_node_ids) in eligible_rebuild_node_ids {
        if let Some(missing_node_id) = expected_node_ids
            .iter()
            .find(|node_id| !planned_node_ids.contains(*node_id))
        {
            anyhow::bail!(
                "descriptor-owned LSP partition rebuild for {partition} is incomplete (missing {missing_node_id}); {SCOPE_HELP}"
            );
        }
    }

    // The structural-cache verifier already bounds the exact authorized file
    // plan by operations. A dense file may legitimately contain more nodes
    // than the ordinary changed-file ceiling while remaining below that same
    // operation ceiling, so only the verifier-bound path may skip the
    // redundant node-count check.
    let (exceeds_node_bound, bounded_operation_count) = match &node_bound {
        ChangedFileNodeBound::Enforce => (
            planned_node_ids.len() > MAX_CHANGED_LSP_NODES,
            operation_count,
        ),
        ChangedFileNodeBound::AuthorizedStructuralCache {
            signed_operation_count,
            ..
        } => (false, operation_count.max(*signed_operation_count)),
    };
    if exceeds_node_bound || bounded_operation_count > MAX_CHANGED_LSP_OPERATIONS {
        let unrebuilt_languages = planned_languages
            .difference(rebuilt_partitions)
            .cloned()
            .collect::<Vec<_>>();
        if !unrebuilt_languages.is_empty() {
            anyhow::bail!(
                "changed-file LSP plan exceeds its bound (max {} nodes / {} operations) and affected descriptor partition(s) were not rebuilt: {}; {SCOPE_HELP}",
                MAX_CHANGED_LSP_NODES,
                MAX_CHANGED_LSP_OPERATIONS,
                unrebuilt_languages.join(", ")
            );
        }
    }

    Ok(Arc::new(planned_node_ids))
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
    let mut worktree_options = DiffOptions::new();
    worktree_options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_typechange(true);
    let mut worktree = repo
        .diff_tree_to_workdir(Some(&head_tree), Some(&mut worktree_options))
        .context("failed to diff HEAD against git worktree")?;
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
    use crate::graph::{ExtractionSource, NodeId, NodeKind};

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
            allow_broad_references: false,
        })
        .unwrap();

        let ids = plan.planned_node_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| id.contains("src/changed.rs")));
        assert!(!ids.iter().any(|id| id.contains("unrelated")));
        assert_eq!(plan.operation_count, 2);
        let broad_type = plan
            .planned_nodes
            .iter()
            .find(|node| node.stable_id.contains("Thing"))
            .expect("type hierarchy remains high-signal default work");
        assert_eq!(broad_type.requested_operations, vec!["type_hierarchy"]);
    }

    #[test]
    fn carried_lsp_output_is_not_planned_as_fresh_changed_file_work() {
        let seed = node("src/changed.rs", "changed", NodeKind::Function);
        let seed_id = seed.stable_id();
        let mut carried_output = node("src/changed.rs", "carried", NodeKind::Function);
        carried_output.source = ExtractionSource::Lsp;
        let nodes = vec![seed, carried_output];

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
            allow_broad_references: true,
        })
        .unwrap();

        assert_eq!(plan.planned_node_ids().as_ref(), &HashSet::from([seed_id]));
        assert_eq!(plan.operation_count, 1);
    }

    #[test]
    fn test_file_function_fanout_above_operation_ceiling_is_not_planned() {
        let nodes = (0..=MAX_CHANGED_LSP_OPERATIONS)
            .map(|index| {
                let mut test = node(
                    "tests/test_dense.py",
                    &format!("test_symbol_{index}"),
                    NodeKind::Function,
                );
                test.language = "python".to_string();
                test
            })
            .collect::<Vec<_>>();

        let plan = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes: vec![ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: Some(PathBuf::from("tests/test_dense.py")),
                new_path: Some(PathBuf::from("tests/test_dense.py")),
            }],
            cached_nodes: &nodes,
            max_nodes: MAX_CHANGED_LSP_NODES,
            max_operations: MAX_CHANGED_LSP_OPERATIONS,
            allow_broad_references: false,
        })
        .unwrap();

        assert!(plan.planned_nodes.is_empty());
        assert_eq!(plan.operation_count, 0);
        assert_eq!(
            plan.unmapped_files,
            vec![PathBuf::from("tests/test_dense.py")]
        );
    }

    #[test]
    fn scanner_touched_files_plan_only_stable_ids_from_touched_paths() {
        let nodes = vec![
            node("src/changed.rs", "changed", NodeKind::Function),
            node("src/unrelated.rs", "unrelated", NodeKind::Function),
            node("src/changed.rs", "Thing", NodeKind::Struct),
        ];
        let touched_files =
            HashSet::from([("fixture".to_string(), PathBuf::from("src/changed.rs"))]);

        let ids = plan_lsp_node_ids_for_touched_files(&touched_files, &nodes).unwrap();

        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| id.contains("src/changed.rs")));
        assert!(!ids.iter().any(|id| id.contains("unrelated")));
    }

    #[test]
    fn verified_plan_counts_one_signed_document_symbol_operation_per_file() {
        let file_count = (MAX_CHANGED_LSP_OPERATIONS / 2) + 1;
        let nodes = (0..file_count)
            .map(|index| {
                node(
                    &format!("src/file_{index}.rs"),
                    &format!("symbol_{index}"),
                    NodeKind::Function,
                )
            })
            .collect::<Vec<_>>();
        let touched_files = nodes
            .iter()
            .map(|node| ("fixture".to_string(), node.id.file.clone()))
            .collect::<HashSet<_>>();
        let authorized_operations_by_language = BTreeMap::from([(
            "rust".to_string(),
            vec!["call_hierarchy".to_string(), "document_symbols".to_string()],
        )]);

        let error = plan_lsp_node_ids_for_touched_files_with_bounds(
            &touched_files,
            &nodes,
            &BTreeSet::new(),
            ChangedFileNodeBound::AuthorizedStructuralCache {
                signed_operation_count: 0,
                authorized_operations_by_language,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds its bound"));
    }

    #[test]
    fn verified_plan_preserves_negotiated_reference_fallback_and_test_document_symbols() {
        let mut source = node("src/app.py", "greet", NodeKind::Function);
        source.language = "python".to_string();
        let source_id = source.stable_id();
        let mut test = node("tests/test_app.py", "test_greet", NodeKind::Function);
        test.language = "python".to_string();
        let test_id = test.stable_id();
        let mut import = node(
            "tests/test_app.py",
            "from src.app import greet",
            NodeKind::Import,
        );
        import.language = "python".to_string();
        let import_id = import.stable_id();
        let nodes = vec![source.clone(), test, import];
        let touched_files = nodes
            .iter()
            .map(|node| ("fixture".to_string(), node.id.file.clone()))
            .collect::<HashSet<_>>();
        let authorized = vec!["document_symbols".to_string(), "references".to_string()];

        assert_eq!(
            authorized_operations_for_node(&source, &authorized),
            vec!["references"]
        );
        let ids = plan_lsp_node_ids_for_touched_files_with_bounds(
            &touched_files,
            &nodes,
            &BTreeSet::new(),
            ChangedFileNodeBound::AuthorizedStructuralCache {
                signed_operation_count: 3,
                authorized_operations_by_language: BTreeMap::from([(
                    "python".to_string(),
                    authorized,
                )]),
            },
        )
        .unwrap();

        assert_eq!(ids.as_ref(), &HashSet::from([source_id]));
        assert!(!ids.contains(&test_id));
        assert!(!ids.contains(&import_id));
    }

    #[test]
    fn over_bound_plan_requires_and_accepts_complete_descriptor_partition_rebuild() {
        let mut nodes = (0..=MAX_CHANGED_LSP_NODES)
            .map(|index| {
                node(
                    "src/dense.rs",
                    &format!("symbol_{index}"),
                    NodeKind::Function,
                )
            })
            .collect::<Vec<_>>();
        let touched_files = HashSet::from([("fixture".to_string(), PathBuf::from("src/dense.rs"))]);

        let bounded_error =
            plan_lsp_node_ids_for_touched_files(&touched_files, &nodes).unwrap_err();
        assert!(bounded_error.to_string().contains("exceeds its bound"));
        assert!(bounded_error.to_string().contains("rust"));

        let rebuilt_partitions = BTreeSet::from(["rust".to_string()]);
        let ids = plan_lsp_node_ids_for_touched_files_with_partition_rebuilds(
            &touched_files,
            &nodes,
            &rebuilt_partitions,
        )
        .unwrap();
        assert_eq!(ids.len(), MAX_CHANGED_LSP_NODES + 1);

        nodes.push(node(
            "src/not-selected.rs",
            "not_selected",
            NodeKind::Function,
        ));
        let incomplete_error = plan_lsp_node_ids_for_touched_files_with_partition_rebuilds(
            &touched_files,
            &nodes,
            &rebuilt_partitions,
        )
        .unwrap_err();
        assert!(incomplete_error.to_string().contains("is incomplete"));
        assert!(incomplete_error.to_string().contains("not-selected.rs"));
    }

    #[test]
    fn planner_uses_shared_profile_for_synthetic_const_and_language_restrictions() {
        let mut synthetic = node("src/changed.rs", "literal", NodeKind::Const);
        synthetic
            .metadata
            .insert("synthetic".to_string(), "true".to_string());
        let declared_const = node("src/changed.rs", "DECLARED", NodeKind::Const);
        let mut python_struct = node("src/changed.rs", "Model", NodeKind::Struct);
        python_struct.language = "python".to_string();
        let mut python_function = node("src/changed.rs", "handler", NodeKind::Function);
        python_function.language = "python".to_string();
        let nodes = vec![synthetic, declared_const, python_struct, python_function];

        let plan = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes: vec![ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: None,
                new_path: Some(PathBuf::from("src/changed.rs")),
            }],
            cached_nodes: &nodes,
            max_nodes: 16,
            max_operations: 16,
            allow_broad_references: false,
        })
        .unwrap();

        assert_eq!(plan.planned_nodes.len(), 1);
        let handler = plan
            .planned_nodes
            .iter()
            .find(|node| node.stable_id.contains("handler"))
            .expect("Python function remains admitted");
        assert_eq!(handler.requested_operations, vec!["call_hierarchy"]);
        assert!(
            plan.planned_nodes
                .iter()
                .all(|node| !node.stable_id.contains("literal")
                    && !node.stable_id.contains("DECLARED")
                    && !node.stable_id.contains("Model"))
        );
    }

    #[test]
    fn explicit_changed_scope_adds_broad_references_without_unrelated_nodes() {
        let nodes = vec![
            node("src/changed.rs", "Thing", NodeKind::Struct),
            node("src/changed.rs", "Alias", NodeKind::TypeAlias),
            node("src/unrelated.rs", "Elsewhere", NodeKind::Struct),
        ];
        let plan = plan_changed_files(ChangedFilePlanInput {
            provenance: provenance(),
            root_slug: "fixture",
            changes: vec![ChangedFile {
                kind: ChangedFileKind::Modified,
                old_path: None,
                new_path: Some(PathBuf::from("src/changed.rs")),
            }],
            cached_nodes: &nodes,
            max_nodes: 16,
            max_operations: 48,
            allow_broad_references: true,
        })
        .unwrap();

        assert_eq!(plan.planned_nodes.len(), 2);
        assert!(
            plan.planned_nodes
                .iter()
                .all(|node| node.file == Path::new("src/changed.rs"))
        );
        assert!(plan.planned_nodes.iter().all(|node| {
            node.requested_operations
                .contains(&"references".to_string())
        }));
        assert!(
            plan.planned_nodes
                .iter()
                .all(|node| !node.stable_id.contains("unrelated"))
        );
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
            allow_broad_references: false,
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
            allow_broad_references: false,
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
            allow_broad_references: false,
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

    #[test]
    fn staged_edit_restored_in_worktree_is_not_scheduled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let original = "fn changed() {}\n";
        std::fs::write(dir.path().join("src/changed.rs"), original).unwrap();

        let repo = Repository::init(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("src/changed.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("RNA test", "rna@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        drop(tree);

        std::fs::write(
            dir.path().join("src/changed.rs"),
            "fn changed() { println!(\"staged\"); }\n",
        )
        .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("src/changed.rs")).unwrap();
        index.write().unwrap();
        std::fs::write(dir.path().join("src/changed.rs"), original).unwrap();

        let (_, changes) = discover_git_worktree_changes(dir.path()).unwrap();
        assert!(changes.is_empty());
    }
}
