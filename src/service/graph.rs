//! CLI graph query and repository statistics.

use std::path::Path;

use crate::embed::SearchOutcome;
use crate::graph::NodeKind;
use crate::server::handlers::run_traversal;
use crate::server::state::GraphState;
use crate::server::store::parse_edge_kind;

/// Parameters for graph traversal (CLI `graph` command).
#[derive(Debug)]
pub struct GraphParams {
    pub node: String,
    pub mode: String,
    pub direction: String,
    pub edge_types: Option<Vec<String>>,
    pub max_hops: Option<usize>,
}

/// Run a graph query and return formatted markdown.
pub fn graph_query(params: &GraphParams, graph_state: &GraphState) -> Result<String, String> {
    let edge_filter = params.edge_types.as_ref().map(|types| types.iter().filter_map(|t| parse_edge_kind(t.trim())).collect::<Vec<_>>());
    let edge_filter_slice = edge_filter.as_deref();
    // Resolve short IDs (without root prefix) to full stable IDs.
    let resolved_node = graph_state.resolve_node_id(&params.node);
    let result_ids = run_traversal(&graph_state.index, &resolved_node, &params.mode, params.max_hops.map(|h| h as u32), Some(params.direction.as_str()), edge_filter_slice)?;
    let node_index_map = graph_state.node_index_map();
    // Filter out hidden node kinds (Module, PrMerge) before counting and rendering.
    // This ensures the reported count matches the displayed entries.
    let displayable: Vec<_> = result_ids.iter()
        .filter(|id| {
            graph_state.node_by_stable_id(id, &node_index_map)
                .map(|n| !matches!(n.id.kind, NodeKind::Module | NodeKind::PrMerge))
                .unwrap_or(true) // Unknown IDs: include (rendered as bare ID below)
        })
        .collect();
    if displayable.is_empty() { return Ok(format!("No results for `{}` ({}).", params.node, params.mode)); }
    let mut lines = vec![format!("## {} `{}`\n\n{} result(s)\n", params.mode, params.node, displayable.len())];
    for id in &displayable {
        if let Some(node) = graph_state.node_by_stable_id(id, &node_index_map) {
            lines.push(format!("- **{}** `{}` ({}) `{}`:{}-{}", node.id.kind, node.id.name, node.language, node.id.file.display(), node.line_start, node.line_end));
            if !node.signature.is_empty() { lines.push(format!("  Sig: `{}`", node.signature)); }
        } else { lines.push(format!("- `{}`", id)); }
    }
    Ok(lines.join("\n"))
}

/// Repository statistics result.
pub struct StatsResult {
    pub node_count: usize, pub edge_count: usize, pub embeddings_available: bool,
    pub languages: Vec<String>, pub last_scan_age: String,
    pub artifact_count: usize, pub outcome_count: usize, pub signal_count: usize,
    pub guardrail_count: usize, pub metis_count: usize,
}

pub async fn stats(repo_root: &Path, graph_state: &GraphState) -> StatsResult {
    let mut langs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for n in &graph_state.nodes { if !n.language.is_empty() && n.language != "unknown" { langs.insert(n.language.clone()); } }
    let artifacts = crate::oh::load_oh_artifacts(repo_root).unwrap_or_default();
    let outcomes = artifacts.iter().filter(|a| a.kind == crate::types::OhArtifactKind::Outcome).count();
    let signals = artifacts.iter().filter(|a| a.kind == crate::types::OhArtifactKind::Signal).count();
    let guardrails = artifacts.iter().filter(|a| a.kind == crate::types::OhArtifactKind::Guardrail).count();
    let metis = artifacts.iter().filter(|a| a.kind == crate::types::OhArtifactKind::Metis).count();
    let embed_available = matches!(crate::embed::EmbeddingIndex::new(repo_root).await, Ok(idx) if matches!(idx.search("_probe_", None, 1).await, Ok(SearchOutcome::Results(_))));
    let lance_path = repo_root.join(".oh").join(".cache").join("lance");
    // Use duration_since(modified) rather than epoch-second subtraction to avoid
    // underflow when the file has a future mtime (clock skew, restored caches).
    let last_scan_age = std::fs::metadata(&lance_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .map(|age| {
            let s = age.as_secs();
            if s < 60 { "just now".into() }
            else if s < 3600 { format!("{}m ago", s / 60) }
            else if s < 86400 { format!("{}h ago", s / 3600) }
            else { format!("{}d ago", s / 86400) }
        })
        .unwrap_or_else(|| "unknown".to_string());
    StatsResult { node_count: graph_state.nodes.len(), edge_count: graph_state.edges.len(), embeddings_available: embed_available,
        languages: langs.into_iter().collect(), last_scan_age, artifact_count: artifacts.len(), outcome_count: outcomes, signal_count: signals, guardrail_count: guardrails, metis_count: metis }
}
