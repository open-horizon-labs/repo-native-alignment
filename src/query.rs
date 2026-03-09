use std::path::Path;

use anyhow::Result;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::git;
use crate::graph::{Edge, EdgeKind, Node, NodeKind};
use crate::markdown;
use crate::oh;
use crate::types::{OhArtifactKind, QueryResult};

/// The real intersection query: given an outcome ID, find related commits,
/// code symbols, and markdown by following structural links — not keyword matching.
///
/// 1. Find the outcome by ID
/// 2. Find commits tagged `[outcome:{id}]` in their message
/// 3. Find commits touching files matching the outcome's `files:` patterns
/// 4. Deduplicate commits
/// 5. For changed files in those commits, find code symbols defined there
/// 6. Find markdown sections mentioning the outcome ID
pub fn outcome_progress(repo_root: &Path, outcome_id: &str, graph_nodes: &[Node]) -> Result<QueryResult> {
    // 1. Find the outcome
    let all_artifacts = oh::load_oh_artifacts(repo_root)?;
    let outcome = all_artifacts
        .iter()
        .find(|a| a.kind == OhArtifactKind::Outcome && a.id() == outcome_id);

    let outcome = match outcome {
        Some(o) => o.clone(),
        None => anyhow::bail!("Outcome '{}' not found in .oh/outcomes/", outcome_id),
    };

    // Extract file patterns from frontmatter
    let file_patterns: Vec<String> = outcome
        .frontmatter
        .get("files")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // 2. Find commits tagged with this outcome
    let tagged_commits = git::search_by_outcome_tag(repo_root, outcome_id, 100)
        .unwrap_or_default();

    // 3. Find commits touching outcome's declared files
    let pattern_commits = if file_patterns.is_empty() {
        Vec::new()
    } else {
        git::commits_touching_patterns(repo_root, &file_patterns, 100)
            .unwrap_or_default()
    };

    // 4. Deduplicate commits by hash, preserving order (tagged first)
    let mut seen_hashes = HashSet::new();
    let mut commits = Vec::new();
    for c in tagged_commits.into_iter().chain(pattern_commits.into_iter()) {
        if seen_hashes.insert(c.hash.clone()) {
            commits.push(c);
        }
    }

    // 5. Collect changed files from all commits, find symbols in those files
    //    from the pre-built graph nodes (all 22 languages, real stable_id()).
    let code_symbols: Vec<Node> = {
        let changed_files: HashSet<PathBuf> = commits
            .iter()
            .flat_map(|c| c.changed_files.iter().cloned())
            .collect();

        graph_nodes
            .iter()
            .filter(|node| {
                let node_rel = node.id.file.strip_prefix(repo_root).unwrap_or(&node.id.file);
                changed_files.contains(node_rel)
            })
            .filter(|node| node.id.kind != NodeKind::Import)
            .cloned()
            .collect()
    };

    // 6. Find markdown mentioning this outcome
    let all_chunks = markdown::extract_markdown_chunks(repo_root).unwrap_or_default();
    let markdown_chunks = markdown::search_chunks(&all_chunks, outcome_id)
        .into_iter()
        .cloned()
        .collect();

    Ok(QueryResult {
        query: format!("outcome_progress({})", outcome_id),
        outcomes: vec![outcome],
        markdown_chunks,
        code_symbols,
        commits,
    })
}

/// Find PR merge nodes relevant to an outcome.
///
/// Two sources of relevance:
/// 1. Serves edges: PR merge nodes with `EdgeKind::Serves` edges pointing to
///    an outcome node whose name matches `outcome_id`.
/// 2. File pattern matching: PR merge nodes whose `files_changed` metadata
///    overlaps with the outcome's declared `file_patterns`.
///
/// Results are deduplicated by node stable ID.
pub fn find_pr_merges_for_outcome<'a>(
    nodes: &'a [Node],
    edges: &[Edge],
    outcome_id: &str,
    file_patterns: &[String],
) -> Vec<&'a Node> {
    let mut matched_stable_ids = HashSet::new();

    // 1. Find PR merge nodes with Serves edges to this outcome
    for edge in edges {
        if edge.kind == EdgeKind::Serves
            && edge.to.name == outcome_id
            && edge.from.kind == NodeKind::PrMerge
        {
            matched_stable_ids.insert(edge.from.to_stable_id());
        }
    }

    // 2. Find PR merge nodes whose files_changed match the outcome's file patterns
    if !file_patterns.is_empty() {
        for node in nodes {
            if node.id.kind != NodeKind::PrMerge {
                continue;
            }
            let stable_id = node.stable_id();
            if matched_stable_ids.contains(&stable_id) {
                continue;
            }
            if let Some(files_json) = node.metadata.get("files_changed") {
                if let Ok(files) = serde_json::from_str::<Vec<String>>(files_json) {
                    let matches = files.iter().any(|f| {
                        file_patterns
                            .iter()
                            .any(|pat| git::glob_match_public(pat, f))
                    });
                    if matches {
                        matched_stable_ids.insert(stable_id);
                    }
                }
            }
        }
    }

    // Collect the actual Node references, preserving the order from the nodes slice
    nodes
        .iter()
        .filter(|n| n.id.kind == NodeKind::PrMerge && matched_stable_ids.contains(&n.stable_id()))
        .collect()
}

/// Format PR merge nodes as a markdown section for outcome_progress output.
pub fn format_pr_merges_markdown(pr_nodes: &[&Node]) -> String {
    if pr_nodes.is_empty() {
        return String::new();
    }

    let mut out = String::from("## PR Merges serving this outcome\n\n");

    for node in pr_nodes {
        let title = &node.signature;
        let author = node.metadata.get("author").map(|s| s.as_str()).unwrap_or("unknown");
        let branch = node.metadata.get("branch_name").map(|s| s.as_str()).unwrap_or("unknown");
        let commit_count = node.metadata.get("commit_count").map(|s| s.as_str()).unwrap_or("?");
        let merged_at = node
            .metadata
            .get("merged_at")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let files_str = node
            .metadata
            .get("files_changed")
            .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
            .map(|files| files.join(", "))
            .unwrap_or_default();

        out.push_str(&format!(
            "- **{}** (merged {} by {})\n  Branch: {} | {} commit(s) | Modified: {}\n\n",
            title, merged_at, author, branch, commit_count, files_str,
        ));
    }

    out
}

