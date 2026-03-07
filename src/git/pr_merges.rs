//! PR merge extraction from git history.
//!
//! Walks merge commits on the main line (first-parent) and produces
//! `PrMerge` graph nodes plus `Modified` edges to symbol nodes that
//! live in the changed files, and `Serves` edges to outcomes referenced
//! via `[outcome:X]` tags in commit messages.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use git2::Repository;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

/// Extract PR merge nodes and edges from git history.
///
/// Walks first-parent merge commits from HEAD, producing up to `limit`
/// `PrMerge` nodes. For each merge commit, also produces:
/// - `Serves` edges to outcome nodes (from `[outcome:X]` tags)
///
/// Symbol-level `Modified` edges are created separately via
/// `link_pr_to_symbols`, since symbol nodes may not exist yet at
/// extraction time.
pub fn extract_pr_merges(
    repo_root: &Path,
    limit: Option<usize>,
) -> Result<(Vec<Node>, Vec<Edge>)> {
    let repo = Repository::open(repo_root).context("Failed to open git repository")?;
    let mut revwalk = repo.revwalk().context("Failed to create revwalk")?;
    revwalk
        .push_head()
        .context("Failed to push HEAD to revwalk")?;
    revwalk.simplify_first_parent()?; // only merge commits on main line

    let limit = limit.unwrap_or(100);
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for oid_result in revwalk {
        if nodes.len() >= limit {
            break;
        }
        let oid = oid_result.context("Failed to get commit oid from revwalk")?;
        let commit = repo
            .find_commit(oid)
            .context("Failed to find commit")?;

        // Only merge commits (2+ parents)
        if commit.parent_count() < 2 {
            continue;
        }

        let title = commit.summary().unwrap_or("").to_string();
        let description = commit.message().unwrap_or("").to_string();
        let author = commit.author().name().unwrap_or("").to_string();
        let merged_at = commit.time().seconds();
        let merge_sha = oid.to_string();

        // Extract branch name from merge commit message
        let branch_name = extract_branch_name(&description);

        // Get changed files: diff merge^1..merge
        let parent = commit.parent(0).context("Failed to get first parent")?;
        let parent_tree = parent.tree().context("Failed to get parent tree")?;
        let merge_tree = commit.tree().context("Failed to get merge commit tree")?;
        let diff = repo
            .diff_tree_to_tree(Some(&parent_tree), Some(&merge_tree), None)
            .context("Failed to diff merge commit")?;

        let mut files_changed: Vec<PathBuf> = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if let Some(path) = delta.new_file().path() {
                    files_changed.push(path.to_path_buf());
                } else if let Some(path) = delta.old_file().path() {
                    files_changed.push(path.to_path_buf());
                }
                true
            },
            None,
            None,
            None,
        )
        .context("Failed to iterate diff deltas")?;

        // Count commits in the PR (between merge parents)
        let commit_count = count_pr_commits(&repo, &commit);

        // Build metadata
        let mut metadata = BTreeMap::new();
        metadata.insert("merge_sha".to_string(), merge_sha.clone());
        if let Some(ref branch) = branch_name {
            metadata.insert("branch_name".to_string(), branch.clone());
        }
        metadata.insert("author".to_string(), author);
        metadata.insert("merged_at".to_string(), merged_at.to_string());
        metadata.insert("commit_count".to_string(), commit_count.to_string());
        let files_json = serde_json::to_string(
            &files_changed
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        metadata.insert("files_changed".to_string(), files_json);

        // Create PrMerge node
        let node_id = pr_merge_node_id(&merge_sha);
        let node = Node {
            id: node_id.clone(),
            language: "git".to_string(),
            line_start: 0,
            line_end: 0,
            signature: title,
            body: description.clone(),
            metadata,
            source: ExtractionSource::Git,
        };
        nodes.push(node);

        // Extract [outcome:X] tags and create Serves edges
        let outcome_tags = extract_outcome_tags(&description);
        for tag in &outcome_tags {
            // Also check all commits in the PR for outcome tags
            let outcome_node_id = NodeId {
                root: String::new(),
                file: PathBuf::from(format!(".oh/outcomes/{}.md", tag)),
                name: tag.clone(),
                kind: NodeKind::Other("outcome".to_string()),
            };
            edges.push(Edge {
                from: node_id.clone(),
                to: outcome_node_id,
                kind: EdgeKind::Serves,
                source: ExtractionSource::Git,
                confidence: Confidence::Detected,
            });
        }

        // Also walk individual PR commits for outcome tags
        let pr_commit_tags = collect_pr_commit_outcome_tags(&repo, &commit);
        for tag in &pr_commit_tags {
            // Skip if already found in merge commit message
            if outcome_tags.contains(tag) {
                continue;
            }
            let outcome_node_id = NodeId {
                root: String::new(),
                file: PathBuf::from(format!(".oh/outcomes/{}.md", tag)),
                name: tag.clone(),
                kind: NodeKind::Other("outcome".to_string()),
            };
            edges.push(Edge {
                from: node_id.clone(),
                to: outcome_node_id,
                kind: EdgeKind::Serves,
                source: ExtractionSource::Git,
                confidence: Confidence::Detected,
            });
        }
    }

    Ok((nodes, edges))
}

/// Create `Modified` edges linking PR merge nodes to symbol nodes
/// that exist in the files changed by each PR.
pub fn link_pr_to_symbols(pr_nodes: &[Node], symbol_nodes: &[Node]) -> Vec<Edge> {
    let mut edges = Vec::new();

    for pr_node in pr_nodes {
        // Get files_changed from metadata
        let files_json = match pr_node.metadata.get("files_changed") {
            Some(json) => json,
            None => continue,
        };
        let files: Vec<String> = match serde_json::from_str(files_json) {
            Ok(f) => f,
            Err(_) => continue,
        };

        // Find symbol nodes whose file is in the changed files list
        for symbol in symbol_nodes {
            let symbol_file = symbol.id.file.display().to_string();
            if files.iter().any(|f| f == &symbol_file) {
                edges.push(Edge {
                    from: pr_node.id.clone(),
                    to: symbol.id.clone(),
                    kind: EdgeKind::Modified,
                    source: ExtractionSource::Git,
                    confidence: Confidence::Detected,
                });
            }
        }
    }

    edges
}

/// Build a deterministic `NodeId` for a PR merge commit.
fn pr_merge_node_id(merge_sha: &str) -> NodeId {
    let short_sha = &merge_sha[..8.min(merge_sha.len())];
    NodeId {
        root: String::new(),
        file: PathBuf::from(format!("git:merge:{}", short_sha)),
        name: merge_sha.to_string(),
        kind: NodeKind::PrMerge,
    }
}

/// Extract branch name from merge commit message.
///
/// Handles common patterns:
/// - `Merge branch 'feature-x'`
/// - `Merge branch 'feature-x' into main`
/// - `Merge pull request #N from user/branch`
pub fn extract_branch_name(message: &str) -> Option<String> {
    let first_line = message.lines().next().unwrap_or("");

    // "Merge pull request #N from user/branch"
    if first_line.starts_with("Merge pull request") {
        if let Some(from_idx) = first_line.find(" from ") {
            return Some(first_line[from_idx + 6..].trim().to_string());
        }
    }

    // "Merge branch 'feature-x'" or "Merge branch 'feature-x' into main"
    if first_line.starts_with("Merge branch '") {
        let rest = &first_line[14..]; // after "Merge branch '"
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }

    None
}

/// Extract `[outcome:X]` tags from a commit message.
pub fn extract_outcome_tags(message: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut search_from = 0;

    while let Some(start) = message[search_from..].find("[outcome:") {
        let abs_start = search_from + start + 9; // skip "[outcome:"
        if let Some(end) = message[abs_start..].find(']') {
            let tag = message[abs_start..abs_start + end].trim().to_string();
            if !tag.is_empty() && !tags.contains(&tag) {
                tags.push(tag);
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    tags
}

/// Count the number of commits in a PR by walking from merge^2 back
/// to the merge base with merge^1.
fn count_pr_commits(repo: &Repository, merge_commit: &git2::Commit) -> u32 {
    if merge_commit.parent_count() < 2 {
        return 0;
    }

    let parent1 = match merge_commit.parent_id(0) {
        Ok(oid) => oid,
        Err(_) => return 0,
    };
    let parent2 = match merge_commit.parent_id(1) {
        Ok(oid) => oid,
        Err(_) => return 0,
    };

    // Find merge base
    let merge_base = match repo.merge_base(parent1, parent2) {
        Ok(oid) => oid,
        Err(_) => return 1, // can't find merge base, assume at least 1
    };

    // Walk from parent2 back to merge_base, counting commits
    let mut revwalk = match repo.revwalk() {
        Ok(rw) => rw,
        Err(_) => return 1,
    };
    if revwalk.push(parent2).is_err() {
        return 1;
    }
    if revwalk.hide(merge_base).is_err() {
        return 1;
    }

    revwalk.count() as u32
}

/// Collect outcome tags from all individual commits within a PR merge.
/// Walks from merge^2 back to the merge base with merge^1.
fn collect_pr_commit_outcome_tags(repo: &Repository, merge_commit: &git2::Commit) -> Vec<String> {
    let mut tags = Vec::new();

    if merge_commit.parent_count() < 2 {
        return tags;
    }

    let parent1 = match merge_commit.parent_id(0) {
        Ok(oid) => oid,
        Err(_) => return tags,
    };
    let parent2 = match merge_commit.parent_id(1) {
        Ok(oid) => oid,
        Err(_) => return tags,
    };

    let merge_base = match repo.merge_base(parent1, parent2) {
        Ok(oid) => oid,
        Err(_) => return tags,
    };

    let mut revwalk = match repo.revwalk() {
        Ok(rw) => rw,
        Err(_) => return tags,
    };
    if revwalk.push(parent2).is_err() {
        return tags;
    }
    if revwalk.hide(merge_base).is_err() {
        return tags;
    }

    for oid_result in revwalk {
        if let Ok(oid) = oid_result {
            if let Ok(commit) = repo.find_commit(oid) {
                let message = commit.message().unwrap_or("");
                for tag in extract_outcome_tags(message) {
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
            }
        }
    }

    tags
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_extract_branch_name_merge_branch() {
        assert_eq!(
            extract_branch_name("Merge branch 'feature-x'"),
            Some("feature-x".to_string())
        );
        assert_eq!(
            extract_branch_name("Merge branch 'feature-x' into main"),
            Some("feature-x".to_string())
        );
    }

    #[test]
    fn test_extract_branch_name_pr() {
        assert_eq!(
            extract_branch_name("Merge pull request #42 from user/my-branch"),
            Some("user/my-branch".to_string())
        );
    }

    #[test]
    fn test_extract_branch_name_unknown() {
        assert_eq!(extract_branch_name("Regular commit message"), None);
        assert_eq!(extract_branch_name(""), None);
    }

    #[test]
    fn test_extract_outcome_tags() {
        let tags = extract_outcome_tags("Fix bug [outcome:agent-alignment] and [outcome:perf]");
        assert_eq!(tags, vec!["agent-alignment", "perf"]);
    }

    #[test]
    fn test_extract_outcome_tags_empty() {
        let tags = extract_outcome_tags("No outcome tags here");
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_outcome_tags_deduplicated() {
        let tags =
            extract_outcome_tags("[outcome:foo] and again [outcome:foo]");
        assert_eq!(tags, vec!["foo"]);
    }

    #[test]
    fn test_link_pr_to_symbols() {
        let pr_node = Node {
            id: pr_merge_node_id("abcdef1234567890"),
            language: "git".to_string(),
            line_start: 0,
            line_end: 0,
            signature: "Merge branch 'feature'".to_string(),
            body: String::new(),
            metadata: {
                let mut m = BTreeMap::new();
                m.insert(
                    "files_changed".to_string(),
                    r#"["src/lib.rs","src/main.rs"]"#.to_string(),
                );
                m
            },
            source: ExtractionSource::Git,
        };

        let sym1 = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: "foo".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 10,
            signature: "fn foo()".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let sym2 = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/other.rs"),
                name: "bar".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 5,
            signature: "fn bar()".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let edges = link_pr_to_symbols(&[pr_node], &[sym1, sym2]);
        // Only sym1 is in src/lib.rs which was changed
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::Modified);
        assert_eq!(edges[0].to.name, "foo");
    }

    /// Helper: initialize a git repo with an initial commit on "main" branch.
    fn init_repo_with_commit(dir: &Path) -> Repository {
        // Use init_opts to set the initial branch to "main"
        let repo = Repository::init(dir).expect("Failed to init repo");

        // Configure committer identity
        let mut config = repo.config().expect("Failed to get config");
        config
            .set_str("user.name", "Test User")
            .expect("Failed to set user.name");
        config
            .set_str("user.email", "test@example.com")
            .expect("Failed to set user.email");

        // Create initial file and commit on HEAD (whatever the default branch is)
        let file_path = dir.join("README.md");
        fs::write(&file_path, "# Test\n").expect("Failed to write file");

        let commit_oid;
        {
            let mut index = repo.index().expect("Failed to get index");
            index
                .add_path(Path::new("README.md"))
                .expect("Failed to add file");
            index.write().expect("Failed to write index");

            let tree_oid = index.write_tree().expect("Failed to write tree");
            let tree = repo.find_tree(tree_oid).expect("Failed to find tree");
            let sig = repo.signature().expect("Failed to get signature");
            commit_oid = repo
                .commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
                .expect("Failed to create initial commit");
        }

        // Ensure we have a "main" branch pointing at the initial commit
        // (handles systems where default branch is "master")
        let needs_rename = {
            let head = repo.head().expect("Failed to get HEAD");
            let head_name = head.name().unwrap_or("");
            head_name != "refs/heads/main"
        };
        if needs_rename {
            let commit = repo.find_commit(commit_oid).expect("Failed to find commit");
            repo.branch("main", &commit, false)
                .expect("Failed to create main branch");
            repo.set_head("refs/heads/main")
                .expect("Failed to set HEAD to main");
        }

        repo
    }

    /// Helper: create a merge commit in the repo.
    /// Creates a branch from HEAD, adds a commit on it, then merges back.
    fn create_merge_commit(
        repo: &Repository,
        dir: &Path,
        branch_name: &str,
        file_name: &str,
        file_content: &str,
        commit_message: &str,
    ) -> git2::Oid {
        let sig = repo.signature().expect("Failed to get signature");

        // Get current HEAD commit
        let head = repo.head().expect("Failed to get HEAD");
        let head_commit = head.peel_to_commit().expect("Failed to peel to commit");

        // Create a branch
        repo.branch(branch_name, &head_commit, false)
            .expect("Failed to create branch");

        // Switch to branch and create a commit
        repo.set_head(&format!("refs/heads/{}", branch_name))
            .expect("Failed to set HEAD");

        let file_path = dir.join(file_name);
        fs::write(&file_path, file_content).expect("Failed to write file");

        let mut index = repo.index().expect("Failed to get index");
        index
            .add_path(Path::new(file_name))
            .expect("Failed to add file");
        index.write().expect("Failed to write index");

        let tree_oid = index.write_tree().expect("Failed to write tree");
        let tree = repo.find_tree(tree_oid).expect("Failed to find tree");

        let branch_commit_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                commit_message,
                &tree,
                &[&head_commit],
            )
            .expect("Failed to create branch commit");
        let branch_commit = repo
            .find_commit(branch_commit_oid)
            .expect("Failed to find branch commit");

        // Switch back to the base branch (could be "main" or "master")
        let base_branch = if repo.find_reference("refs/heads/main").is_ok() {
            "refs/heads/main"
        } else {
            "refs/heads/master"
        };
        repo.set_head(base_branch)
            .expect("Failed to set HEAD to base branch");

        // Merge: create a merge commit with both parents
        // Re-read HEAD after switching
        let main_head = repo.head().expect("Failed to get HEAD");
        let main_commit = main_head
            .peel_to_commit()
            .expect("Failed to peel to commit");

        // Build merge tree (use the branch tree since it has the new file)
        let merge_tree = repo
            .find_tree(tree_oid)
            .expect("Failed to find merge tree");

        let merge_msg = format!("Merge branch '{}'", branch_name);
        let merge_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                &merge_msg,
                &merge_tree,
                &[&main_commit, &branch_commit],
            )
            .expect("Failed to create merge commit");

        merge_oid
    }

    #[test]
    fn test_extract_pr_merges_from_repo() {
        let tmp = TempDir::new().expect("Failed to create temp dir");
        let dir = tmp.path();
        let repo = init_repo_with_commit(dir);

        // Create a merge commit
        create_merge_commit(
            &repo,
            dir,
            "feature-1",
            "feature.rs",
            "fn feature() {}",
            "Add feature [outcome:agent-alignment]",
        );

        let (nodes, edges) = extract_pr_merges(dir, Some(10)).expect("Failed to extract");

        assert_eq!(nodes.len(), 1, "Should find exactly 1 merge commit");
        assert_eq!(nodes[0].id.kind, NodeKind::PrMerge);
        assert!(nodes[0].signature.contains("Merge branch 'feature-1'"));
        assert_eq!(
            nodes[0].metadata.get("branch_name"),
            Some(&"feature-1".to_string())
        );

        // Should have a Serves edge to agent-alignment outcome
        // (from the branch commit's [outcome:agent-alignment] tag)
        let serves_edges: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Serves).collect();
        assert_eq!(serves_edges.len(), 1);
        assert_eq!(serves_edges[0].to.name, "agent-alignment");
    }

    #[test]
    fn test_extract_pr_merges_respects_limit() {
        let tmp = TempDir::new().expect("Failed to create temp dir");
        let dir = tmp.path();
        let repo = init_repo_with_commit(dir);

        // Create two merge commits
        create_merge_commit(&repo, dir, "feat-a", "a.rs", "// a", "feat a");
        create_merge_commit(&repo, dir, "feat-b", "b.rs", "// b", "feat b");

        let (nodes, _) = extract_pr_merges(dir, Some(1)).expect("Failed to extract");
        assert_eq!(nodes.len(), 1, "Limit should cap at 1");
    }

    #[test]
    fn test_extract_pr_merges_no_merges() {
        let tmp = TempDir::new().expect("Failed to create temp dir");
        let dir = tmp.path();
        let _repo = init_repo_with_commit(dir);

        // No merge commits, just the initial commit
        let (nodes, edges) = extract_pr_merges(dir, Some(10)).expect("Failed to extract");
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn test_extract_pr_merges_files_changed() {
        let tmp = TempDir::new().expect("Failed to create temp dir");
        let dir = tmp.path();
        let repo = init_repo_with_commit(dir);

        create_merge_commit(
            &repo,
            dir,
            "add-file",
            "new_file.rs",
            "fn new() {}",
            "Add new file",
        );

        let (nodes, _) = extract_pr_merges(dir, Some(10)).expect("Failed to extract");
        assert_eq!(nodes.len(), 1);

        let files_json = nodes[0].metadata.get("files_changed").unwrap();
        let files: Vec<String> = serde_json::from_str(files_json).unwrap();
        assert!(files.contains(&"new_file.rs".to_string()));
    }
}
