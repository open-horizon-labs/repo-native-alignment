use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::Result;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::git;
use crate::graph::index::GraphIndex;
use crate::graph::{Edge, EdgeKind, Node, NodeKind};
use crate::markdown;
use crate::oh;
use crate::ranking;
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

// ── Change impact with risk classification ──────────────────────────

/// Risk tier for a symbol affected by changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskTier {
    Critical,
    High,
    Medium,
    Low,
}

impl fmt::Display for RiskTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskTier::Critical => write!(f, "CRITICAL"),
            RiskTier::High => write!(f, "HIGH"),
            RiskTier::Medium => write!(f, "MEDIUM"),
            RiskTier::Low => write!(f, "LOW"),
        }
    }
}

/// A symbol affected by changes, with its risk classification.
#[derive(Debug, Clone)]
pub struct ImpactedSymbol {
    pub stable_id: String,
    pub name: String,
    pub kind: NodeKind,
    pub file: PathBuf,
    pub risk: RiskTier,
    /// Why this risk tier was assigned.
    pub reason: String,
}

/// Compute the blast radius for changed symbols: walk reverse edges to find
/// what depends on them, then classify each affected symbol by risk tier.
///
/// Risk classification uses:
/// - `ranking::kind_rank()` -- primary definitions vs secondary vs imports
/// - `ranking::is_test_file()` -- test files are lower risk
/// - PageRank importance from node metadata
/// - In-degree (connectivity) from the graph index
pub fn compute_impact_risk(
    changed_symbols: &[Node],
    all_nodes: &[Node],
    index: &GraphIndex,
    max_hops: usize,
) -> Vec<ImpactedSymbol> {
    // Build a lookup from stable_id -> &Node for all graph nodes
    let node_by_id: BTreeMap<String, &Node> = all_nodes
        .iter()
        .map(|n| (n.stable_id(), n))
        .collect();

    // Collect stable IDs of the changed symbols themselves (to exclude from impact)
    let changed_ids: HashSet<String> = changed_symbols
        .iter()
        .map(|n| n.stable_id())
        .collect();

    // For each changed symbol, find what depends on it (reverse traversal)
    let mut impacted_ids: HashSet<String> = HashSet::new();
    for sym in changed_symbols {
        let sid = sym.stable_id();
        let dependents = index.impact(&sid, max_hops);
        for dep_id in dependents {
            // Don't include the changed symbols themselves in the impact list
            if !changed_ids.contains(&dep_id) {
                impacted_ids.insert(dep_id);
            }
        }
    }

    // Classify each impacted symbol
    let mut results: Vec<ImpactedSymbol> = Vec::new();
    for imp_id in &impacted_ids {
        let Some(node) = node_by_id.get(imp_id.as_str()) else {
            continue;
        };

        let (risk, reason) = classify_risk(node, index);
        results.push(ImpactedSymbol {
            stable_id: imp_id.clone(),
            name: node.id.name.clone(),
            kind: node.id.kind.clone(),
            file: node.id.file.clone(),
            risk,
            reason,
        });
    }

    // Sort by risk tier (CRITICAL first), then by name for stability
    results.sort_by(|a, b| a.risk.cmp(&b.risk).then_with(|| a.name.cmp(&b.name)));

    // Cap output to avoid token explosion on large graphs
    results.truncate(MAX_IMPACT_SYMBOLS);
    results
}

/// Classify a single node's risk tier based on ranking signals.
fn classify_risk(node: &Node, index: &GraphIndex) -> (RiskTier, String) {
    let kind_r = ranking::kind_rank(node);
    let is_test = ranking::is_test_file(node);
    let pagerank: Option<f64> = node
        .metadata
        .get("importance")
        .and_then(|s| s.parse::<f64>().ok());

    let sid = node.stable_id();
    let in_degree = index
        .neighbors(&sid, None, petgraph::Direction::Incoming)
        .len();

    // Test files are always LOW risk
    if is_test {
        return (RiskTier::Low, "test file".to_string());
    }

    // CRITICAL: entry points (main, handlers, API endpoints) or high PageRank (>0.7)
    let is_entry_point = is_entry_point_name(&node.id.name, &node.id.kind);
    if is_entry_point {
        return (RiskTier::Critical, "entry point".to_string());
    }
    if let Some(pr) = pagerank {
        if pr > 0.7 {
            return (
                RiskTier::Critical,
                format!("high PageRank ({:.2})", pr),
            );
        }
    }

    // HIGH: high-degree hub symbols (many incoming edges) or moderate PageRank
    if let Some(pr) = pagerank {
        if pr > 0.4 {
            return (
                RiskTier::High,
                format!("moderate PageRank ({:.2})", pr),
            );
        }
    }
    if in_degree >= 5 {
        return (
            RiskTier::High,
            format!("hub ({} dependents)", in_degree),
        );
    }

    // MEDIUM: production symbols with some connectivity (primary definitions with edges)
    if kind_r == 0 && in_degree >= 1 {
        return (
            RiskTier::Medium,
            format!("production symbol ({} dependents)", in_degree),
        );
    }

    // LOW: leaf symbols, secondary definitions, no dependents
    let reason = if in_degree == 0 {
        "leaf (no dependents)".to_string()
    } else {
        format!("secondary ({} dependents)", in_degree)
    };
    (RiskTier::Low, reason)
}

/// Heuristic: is this symbol name likely an entry point?
fn is_entry_point_name(name: &str, kind: &NodeKind) -> bool {
    let lower = name.to_lowercase();
    // Rust/Go main
    if lower == "main" && *kind == NodeKind::Function {
        return true;
    }
    // Common handler/endpoint patterns
    if lower.starts_with("handle_") || lower.ends_with("_handler") {
        return true;
    }
    // API endpoint nodes are always entry points
    if *kind == NodeKind::ApiEndpoint {
        return true;
    }
    false
}

/// Cap the total number of impacted symbols to avoid output explosion.
const MAX_IMPACT_SYMBOLS: usize = 50;

/// Format impact results as a markdown section for appending to outcome_progress output.
pub fn format_impact_markdown(impacted: &[ImpactedSymbol]) -> String {
    if impacted.is_empty() {
        return "## Change Impact\n\nNo symbols in the blast radius (changed symbols have no dependents in the graph).\n".to_string();
    }

    let mut out = String::from("## Change Impact\n\n");

    // Summary counts by tier
    let mut tier_counts: BTreeMap<String, usize> = BTreeMap::new();
    for imp in impacted {
        *tier_counts.entry(imp.risk.to_string()).or_insert(0) += 1;
    }
    let summary: Vec<String> = tier_counts
        .iter()
        .map(|(tier, count)| format!("{} {}", count, tier))
        .collect();
    out.push_str(&format!(
        "{} affected symbols: {}\n\n",
        impacted.len(),
        summary.join(", ")
    ));

    // Group by tier, show up to 10 per tier
    let tiers = [RiskTier::Critical, RiskTier::High, RiskTier::Medium, RiskTier::Low];
    for tier in &tiers {
        let tier_symbols: Vec<&ImpactedSymbol> = impacted
            .iter()
            .filter(|s| s.risk == *tier)
            .collect();
        if tier_symbols.is_empty() {
            continue;
        }

        out.push_str(&format!("### {} Risk\n\n", tier));
        for sym in tier_symbols.iter().take(10) {
            out.push_str(&format!(
                "- `{}` ({}) in {} -- {}\n  ID: `{}`\n",
                sym.name,
                sym.kind,
                sym.file.display(),
                sym.reason,
                sym.stable_id,
            ));
        }
        if tier_symbols.len() > 10 {
            out.push_str(&format!("- ...and {} more\n", tier_symbols.len() - 10));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::index::GraphIndex;
    use crate::graph::{EdgeKind, ExtractionSource, Node, NodeId, NodeKind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: NodeId {
                kind,
                name: name.to_string(),
                file: PathBuf::from(file),
                root: "local".to_string(),
            },
            language: "rust".to_string(),
            signature: format!("fn {}", name),
            line_start: 0,
            line_end: 10,
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_node_with_pagerank(name: &str, kind: NodeKind, file: &str, importance: f64) -> Node {
        let mut n = make_node(name, kind, file);
        n.metadata
            .insert("importance".to_string(), format!("{:.2}", importance));
        n
    }

    /// Build a small graph: changed -> hub -> leaf, changed -> handler
    fn build_test_graph() -> (Vec<Node>, Vec<Node>, GraphIndex) {
        let changed = make_node("do_work", NodeKind::Function, "src/lib.rs");
        let hub = make_node_with_pagerank("process", NodeKind::Function, "src/core.rs", 0.5);
        let leaf = make_node("helper", NodeKind::Function, "src/util.rs");
        let handler = make_node("handle_request", NodeKind::Function, "src/api.rs");
        let test_fn = make_node("test_do_work", NodeKind::Function, "tests/test_lib.rs");

        let all_nodes = vec![
            changed.clone(),
            hub.clone(),
            leaf.clone(),
            handler.clone(),
            test_fn.clone(),
        ];

        let changed_symbols = vec![changed.clone()];

        let mut index = GraphIndex::new();
        // hub calls changed (so hub is in blast radius of changed)
        index.add_edge(
            &hub.stable_id(),
            "function",
            &changed.stable_id(),
            "function",
            EdgeKind::Calls,
        );
        // handler calls changed
        index.add_edge(
            &handler.stable_id(),
            "function",
            &changed.stable_id(),
            "function",
            EdgeKind::Calls,
        );
        // test calls changed
        index.add_edge(
            &test_fn.stable_id(),
            "function",
            &changed.stable_id(),
            "function",
            EdgeKind::Calls,
        );
        // leaf calls hub (leaf is 2 hops away from changed)
        index.add_edge(
            &leaf.stable_id(),
            "function",
            &hub.stable_id(),
            "function",
            EdgeKind::Calls,
        );

        (changed_symbols, all_nodes, index)
    }

    #[test]
    fn test_compute_impact_risk_basic() {
        let (changed, all_nodes, index) = build_test_graph();
        let impacted = compute_impact_risk(&changed, &all_nodes, &index, 3);

        // Should find hub, handler, test_fn, and leaf (2 hops)
        assert!(
            !impacted.is_empty(),
            "Should find impacted symbols"
        );

        // handler should be CRITICAL (entry point pattern)
        let handler = impacted.iter().find(|s| s.name == "handle_request");
        assert!(handler.is_some(), "handler should be in impact list");
        assert_eq!(handler.unwrap().risk, RiskTier::Critical);

        // test_do_work should be LOW (test file)
        let test = impacted.iter().find(|s| s.name == "test_do_work");
        assert!(test.is_some(), "test function should be in impact list");
        assert_eq!(test.unwrap().risk, RiskTier::Low);
    }

    #[test]
    fn test_compute_impact_risk_excludes_changed_symbols() {
        let (changed, all_nodes, index) = build_test_graph();
        let impacted = compute_impact_risk(&changed, &all_nodes, &index, 3);

        // do_work itself should NOT be in the impact list
        let changed_sym = impacted.iter().find(|s| s.name == "do_work");
        assert!(
            changed_sym.is_none(),
            "Changed symbol should not appear in its own impact list"
        );
    }

    #[test]
    fn test_compute_impact_risk_empty_graph() {
        let changed = vec![make_node("orphan", NodeKind::Function, "src/lib.rs")];
        let all_nodes = changed.clone();
        let index = GraphIndex::new();

        let impacted = compute_impact_risk(&changed, &all_nodes, &index, 3);
        assert!(impacted.is_empty(), "No edges means no impact");
    }

    #[test]
    fn test_classify_risk_entry_point() {
        let handler = make_node("handle_request", NodeKind::Function, "src/api.rs");
        let index = GraphIndex::new();
        let (risk, reason) = classify_risk(&handler, &index);
        assert_eq!(risk, RiskTier::Critical);
        assert!(reason.contains("entry point"));
    }

    #[test]
    fn test_classify_risk_main_function() {
        let main_fn = make_node("main", NodeKind::Function, "src/main.rs");
        let index = GraphIndex::new();
        let (risk, _) = classify_risk(&main_fn, &index);
        assert_eq!(risk, RiskTier::Critical);
    }

    #[test]
    fn test_classify_risk_api_endpoint() {
        let endpoint = make_node("get_users", NodeKind::ApiEndpoint, "src/routes.rs");
        let index = GraphIndex::new();
        let (risk, _) = classify_risk(&endpoint, &index);
        assert_eq!(risk, RiskTier::Critical);
    }

    #[test]
    fn test_classify_risk_high_pagerank() {
        let node = make_node_with_pagerank("core_fn", NodeKind::Function, "src/core.rs", 0.85);
        let index = GraphIndex::new();
        let (risk, reason) = classify_risk(&node, &index);
        assert_eq!(risk, RiskTier::Critical);
        assert!(reason.contains("PageRank"));
    }

    #[test]
    fn test_classify_risk_moderate_pagerank() {
        let node = make_node_with_pagerank("mid_fn", NodeKind::Function, "src/mid.rs", 0.5);
        let index = GraphIndex::new();
        let (risk, reason) = classify_risk(&node, &index);
        assert_eq!(risk, RiskTier::High);
        assert!(reason.contains("PageRank"));
    }

    #[test]
    fn test_classify_risk_test_file_always_low() {
        // Even with high PageRank, test files are LOW
        let node = make_node_with_pagerank("test_fn", NodeKind::Function, "tests/test_core.rs", 0.9);
        let index = GraphIndex::new();
        let (risk, reason) = classify_risk(&node, &index);
        assert_eq!(risk, RiskTier::Low);
        assert!(reason.contains("test"));
    }

    #[test]
    fn test_classify_risk_leaf_is_low() {
        let leaf = make_node("helper", NodeKind::Function, "src/util.rs");
        let index = GraphIndex::new();
        let (risk, reason) = classify_risk(&leaf, &index);
        assert_eq!(risk, RiskTier::Low);
        assert!(reason.contains("leaf"));
    }

    #[test]
    fn test_is_entry_point_name_patterns() {
        assert!(is_entry_point_name("main", &NodeKind::Function));
        assert!(is_entry_point_name("handle_request", &NodeKind::Function));
        assert!(is_entry_point_name("my_handler", &NodeKind::Function));
        assert!(is_entry_point_name("get_users", &NodeKind::ApiEndpoint));

        // Not entry points
        assert!(!is_entry_point_name("process", &NodeKind::Function));
        assert!(!is_entry_point_name("main", &NodeKind::Struct)); // main struct, not function
        assert!(!is_entry_point_name("helper", &NodeKind::Function));
    }

    #[test]
    fn test_format_impact_markdown_empty() {
        let md = format_impact_markdown(&[]);
        assert!(md.contains("No symbols in the blast radius"));
    }

    #[test]
    fn test_format_impact_markdown_with_results() {
        let impacted = vec![
            ImpactedSymbol {
                stable_id: "local::src/api.rs::handle_request::function".to_string(),
                name: "handle_request".to_string(),
                kind: NodeKind::Function,
                file: PathBuf::from("src/api.rs"),
                risk: RiskTier::Critical,
                reason: "entry point".to_string(),
            },
            ImpactedSymbol {
                stable_id: "local::tests/test.rs::test_fn::function".to_string(),
                name: "test_fn".to_string(),
                kind: NodeKind::Function,
                file: PathBuf::from("tests/test.rs"),
                risk: RiskTier::Low,
                reason: "test file".to_string(),
            },
        ];

        let md = format_impact_markdown(&impacted);
        assert!(md.contains("## Change Impact"));
        assert!(md.contains("2 affected symbols"));
        assert!(md.contains("CRITICAL"));
        assert!(md.contains("handle_request"));
        assert!(md.contains("LOW"));
        assert!(md.contains("test_fn"));
    }

    #[test]
    fn test_impact_sorted_by_risk_then_name() {
        let (changed, all_nodes, index) = build_test_graph();
        let impacted = compute_impact_risk(&changed, &all_nodes, &index, 3);

        // Verify sorted: CRITICAL before HIGH before MEDIUM before LOW
        let mut last_risk = RiskTier::Critical;
        for sym in &impacted {
            assert!(
                sym.risk >= last_risk,
                "Results should be sorted by risk tier, got {:?} after {:?}",
                sym.risk, last_risk
            );
            last_risk = sym.risk;
        }
    }

    #[test]
    fn test_impact_max_hops_1_limits_depth() {
        let (changed, all_nodes, index) = build_test_graph();
        // With max_hops=1, should NOT find leaf (2 hops away)
        let impacted = compute_impact_risk(&changed, &all_nodes, &index, 1);

        let leaf = impacted.iter().find(|s| s.name == "helper");
        assert!(
            leaf.is_none(),
            "With max_hops=1, leaf 2 hops away should not be found"
        );

        // But should still find direct dependents
        let handler = impacted.iter().find(|s| s.name == "handle_request");
        assert!(handler.is_some(), "Direct dependent should still be found");
    }

    #[test]
    fn test_hub_with_many_dependents_is_high_risk() {
        // Create a node with 5+ incoming edges (hub)
        let target = make_node("shared_util", NodeKind::Function, "src/util.rs");
        let callers: Vec<Node> = (0..6)
            .map(|i| make_node(&format!("caller_{}", i), NodeKind::Function, "src/lib.rs"))
            .collect();

        let mut all_nodes = vec![target.clone()];
        all_nodes.extend(callers.iter().cloned());

        let mut index = GraphIndex::new();
        for caller in &callers {
            index.add_edge(
                &caller.stable_id(),
                "function",
                &target.stable_id(),
                "function",
                EdgeKind::Calls,
            );
        }

        // shared_util has 6 incoming edges -- should be HIGH risk
        let (risk, reason) = classify_risk(&target, &index);
        assert_eq!(risk, RiskTier::High, "Hub with 6 dependents should be HIGH");
        assert!(reason.contains("hub") || reason.contains("dependents"));
    }
}
