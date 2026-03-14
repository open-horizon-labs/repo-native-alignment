//! Shared service layer for CLI and MCP.
//!
//! Both interfaces are thin dispatchers to these functions. The service layer
//! defines the full capability surface -- adding a parameter here automatically
//! makes it available in both CLI and MCP.

use std::collections::HashSet;
use std::path::Path;

use crate::embed::{EmbeddingIndex, SearchOutcome};
use crate::graph::{Node, NodeKind};
use crate::ranking;
use crate::server::helpers::{
    format_freshness, format_neighbor_nodes, format_node_entry, retain_displayable,
};
use crate::server::handlers::{parse_search_mode, run_traversal};
use crate::server::state::{GraphState, LspEnrichmentStatus};
use crate::server::store::parse_edge_kind;

/// Interface-agnostic search parameters.
#[derive(Debug, Default)]
pub struct SearchParams {
    pub query: Option<String>,
    pub node: Option<String>,
    pub mode: Option<String>,
    pub hops: Option<u32>,
    pub direction: Option<String>,
    pub edge_types: Option<Vec<String>>,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub sort_by: Option<String>,
    pub min_complexity: Option<u32>,
    pub synthetic: Option<bool>,
    pub compact: bool,
    pub nodes: Option<Vec<String>>,
    pub search_mode: Option<String>,
    pub include_artifacts: bool,
    pub include_markdown: bool,
    pub artifact_types: Option<Vec<String>>,
}

impl SearchParams {
    /// Convert from MCP `Search` tool struct.
    pub fn from_mcp_search(args: &crate::server::tools::Search) -> Self {
        Self {
            query: args.query.clone(),
            node: args.node.clone(),
            mode: args.mode.clone(),
            hops: args.hops,
            direction: args.direction.clone(),
            edge_types: args.edge_types.clone(),
            kind: args.kind.clone(),
            language: args.language.clone(),
            file: args.file.clone(),
            limit: args.top_k.map(|k| k as usize),
            sort_by: args.sort_by.clone(),
            min_complexity: args.min_complexity,
            synthetic: args.synthetic,
            compact: args.compact.unwrap_or(false),
            nodes: args.nodes.clone(),
            search_mode: args.search_mode.clone(),
            include_artifacts: args.include_artifacts.unwrap_or(true),
            include_markdown: args.include_markdown.unwrap_or(true),
            artifact_types: args.artifact_types.clone(),
        }
    }
}

/// Runtime context for search operations.
pub struct SearchContext<'a> {
    pub graph_state: &'a GraphState,
    pub embed_index: Option<&'a EmbeddingIndex>,
    pub repo_root: &'a Path,
    pub lsp_status: Option<&'a LspEnrichmentStatus>,
    pub root_filter: Option<String>,
    pub non_code_slugs: HashSet<String>,
}

/// Unified search entry point. Returns formatted markdown.
pub async fn search(params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    let query = params.query.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let node = params.node.as_deref().map(str::trim).filter(|s| !s.is_empty());

    if let Some(ref node_ids) = params.nodes {
        let node_ids: Vec<&str> = node_ids.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if node_ids.is_empty() {
            return "Empty nodes list. Provide at least one stable node ID.".to_string();
        }
        return search_batch(&node_ids, params, ctx);
    }

    if params.mode.is_some() {
        search_traversal(params, query, node, ctx).await
    } else if query.is_none() && node.is_some() {
        let node_ids = vec![node.unwrap()];
        search_batch(&node_ids, params, ctx)
    } else {
        search_flat(params, query, ctx).await
    }
}

async fn search_flat(params: &SearchParams, query: Option<&str>, ctx: &SearchContext<'_>) -> String {
    let sort_by_complexity = params.sort_by.as_deref() == Some("complexity");
    let sort_by_importance = params.sort_by.as_deref() == Some("importance");
    let complexity_search = params.min_complexity.is_some() || sort_by_complexity;
    let has_kind_filter = params.kind.is_some();

    let query_str = query.unwrap_or("");
    if query_str.is_empty() && !complexity_search && !sort_by_importance && !has_kind_filter {
        return "Empty query. Please describe what you're looking for (or use kind, min_complexity, sort_by=\"complexity\", or sort_by=\"importance\").".to_string();
    }

    let search_mode = parse_search_mode(params.search_mode.as_deref());
    let limit = params.limit.unwrap_or(10);
    let mut sections: Vec<String> = Vec::new();
    let graph_state = ctx.graph_state;
    let query_lower = query_str.to_lowercase();

    let mut matches: Vec<&Node> = graph_state.nodes.iter().filter(|n| {
        if complexity_search && n.id.kind != NodeKind::Function { return false; }
        if !query_lower.is_empty() {
            let name_match = n.id.name.to_lowercase().contains(&query_lower) || n.signature.to_lowercase().contains(&query_lower);
            if !name_match { return false; }
        }
        if let Some(ref kf) = params.kind { if n.id.kind.to_string().to_lowercase() != kf.to_lowercase() { return false; } }
        if let Some(ref lf) = params.language { if n.language.to_lowercase() != lf.to_lowercase() { return false; } }
        if let Some(ref ff) = params.file { if !n.id.file.to_string_lossy().contains(ff.as_str()) { return false; } }
        if !node_passes_root_filter(&n.id.root, &ctx.root_filter, &ctx.non_code_slugs) { return false; }
        if let Some(sf) = params.synthetic { if (n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false)) != sf { return false; } }
        if let Some(min_cc) = params.min_complexity {
            let Some(cc) = n.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()) else { return false; };
            if cc < min_cc { return false; }
        }
        true
    }).collect();

    if sort_by_complexity {
        matches.retain(|n| n.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).is_some());
        matches.sort_by(|a, b| {
            let ca = a.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let cb = b.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            cb.cmp(&ca)
        });
    } else if sort_by_importance {
        matches.sort_by(|a, b| {
            let ia = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok());
            let ib = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok());
            match (ia, ib) {
                (Some(a), Some(b)) => b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
    } else {
        ranking::sort_symbol_matches(&mut matches, &query_lower, &graph_state.index);
    }
    matches.truncate(limit);

    if !matches.is_empty() {
        let md: String = matches.iter().map(|n| format_node_entry(n, &graph_state.index, params.compact)).collect::<Vec<_>>().join("\n\n");
        sections.push(format!("### Code symbols ({} result(s))\n\n{}", matches.len(), md));
    }

    if params.include_artifacts && !query_str.is_empty() {
        if let Some(embed_idx) = ctx.embed_index {
            match embed_idx.search_with_mode(query_str, params.artifact_types.as_deref(), limit, search_mode).await {
                Ok(SearchOutcome::Results(results)) => {
                    let filtered: Vec<_> = results.into_iter()
                        .filter(|r| !r.kind.starts_with("code:"))
                        .filter(|r| search_result_passes_root_filter(r, &ctx.root_filter, &ctx.non_code_slugs))
                        .collect();
                    if !filtered.is_empty() {
                        let md: String = filtered.iter().map(|r| r.to_markdown()).collect::<Vec<_>>().join("\n");
                        sections.push(format!("### Artifacts ({} result(s))\n\n{}", filtered.len(), md));
                    }
                }
                Ok(SearchOutcome::NotReady) => { sections.push("Embedding index: building -- artifact results will appear shortly. Retry in a few seconds.".to_string()); }
                Err(e) => sections.push(format!("Artifact search error: {}", e)),
            }
        }
    }

    if params.include_markdown && !query_str.is_empty() {
        if let Ok(chunks) = crate::markdown::extract_markdown_chunks(ctx.repo_root) {
            let filtered_chunks: Vec<_> = if let Some(ref slug) = ctx.root_filter {
                let workspace = crate::roots::WorkspaceConfig::load().with_primary_root(ctx.repo_root.to_path_buf()).with_worktrees(ctx.repo_root).with_claude_memory(ctx.repo_root);
                let root_path = workspace.resolved_roots().into_iter().find(|r| r.slug == *slug).map(|r| r.path);
                if let Some(rp) = root_path { chunks.into_iter().filter(|c| c.file_path.starts_with(&rp)).collect() } else { Vec::new() }
            } else { chunks };
            let scored = crate::markdown::search_chunks_ranked(&filtered_chunks, query_str);
            if !scored.is_empty() {
                let md = scored.iter().take(limit).map(|sc| format!("- (score: {:.2}) {}", sc.score, sc.chunk.to_markdown())).collect::<Vec<_>>().join("\n\n---\n\n");
                sections.push(format!("### Markdown ({} result(s))\n\n{}", scored.len().min(limit), md));
            }
        }
    }

    let freshness = format_freshness(graph_state.nodes.len(), graph_state.last_scan_completed_at, ctx.lsp_status);
    if sections.is_empty() { format!("No results matching \"{}\".{}", query_str, freshness) }
    else { format!("## Search: \"{}\"\n\n{}{}", query_str, sections.join("\n\n"), freshness) }
}

async fn search_traversal(params: &SearchParams, query: Option<&str>, node: Option<&str>, ctx: &SearchContext<'_>) -> String {
    let mode = params.mode.as_deref().unwrap_or("neighbors");
    let top_k = params.limit.unwrap_or(1).clamp(1, 50);

    if node.is_none() && query.is_none() {
        return "Either query or node is required. Provide a search query or a stable node ID.".to_string();
    }

    let search_mode = parse_search_mode(params.search_mode.as_deref());
    let (entry_node_ids, entry_header): (Vec<String>, String) = if let Some(node_id) = node {
        (vec![node_id.to_string()], String::new())
    } else if let Some(query_text) = query {
        if let Some(embed_idx) = ctx.embed_index {
            match embed_idx.search_with_mode(query_text, None, top_k.min(50) * 3, search_mode).await {
                Ok(SearchOutcome::Results(results)) if !results.is_empty() => {
                    let code_results: Vec<_> = results.into_iter().filter(|r| r.kind.starts_with("code:")).filter(|r| search_result_passes_root_filter(r, &ctx.root_filter, &ctx.non_code_slugs)).take(top_k).collect();
                    if code_results.is_empty() { return format!("No code symbols matched query \"{}\". Try a different query or use node parameter.", query_text); }
                    let mut header = format!("### Matched entry nodes for \"{}\"\n\n", query_text);
                    let ids: Vec<String> = code_results.iter().map(|r| { header.push_str(&format!("- `{}` -- {} (score: {:.2})\n", r.id, r.title, r.score)); r.id.clone() }).collect();
                    header.push('\n');
                    (ids, header)
                }
                Ok(SearchOutcome::NotReady) => return "Embedding index: building -- semantic graph queries will work shortly. Use node parameter instead, or retry in a few seconds.".to_string(),
                Ok(_) => return format!("No code symbols matched query \"{}\". Try a different query or use node parameter.", query_text),
                Err(e) => return format!("Semantic search failed: {}. Use node parameter instead.", e),
            }
        } else {
            return "Embedding index not available. Use node parameter instead, or wait for the background index to build.".to_string();
        }
    } else { unreachable!() };

    let gs = ctx.graph_state;
    let valid_entry_ids: Vec<&String> = entry_node_ids.iter().filter(|id| gs.index.get_node(id).is_some()).collect();
    if valid_entry_ids.is_empty() {
        let id_list = entry_node_ids.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", ");
        return format!("{}No graph nodes found for {}. The node(s) may not have edges in the graph. Try search to find valid node IDs.", entry_header, id_list);
    }

    let edge_filter = params.edge_types.as_ref().map(|types| types.iter().filter_map(|t| parse_edge_kind(t)).collect::<Vec<_>>());
    let edge_filter_slice = edge_filter.as_deref();
    let mut all_ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node_id in &valid_entry_ids {
        match run_traversal(&gs.index, node_id, mode, params.hops, params.direction.as_deref(), edge_filter_slice) {
            Ok(ids) => { for id in ids { if seen.insert(id.clone()) { all_ids.push(id); } } }
            Err(msg) => return msg,
        }
    }
    let entry_set: HashSet<&str> = valid_entry_ids.iter().map(|s| s.as_str()).collect();
    all_ids.retain(|id| !entry_set.contains(id.as_str()));
    if mode == "tests_for" { all_ids.retain(|id| gs.nodes.iter().find(|n| n.stable_id() == *id).map(ranking::is_test_file).unwrap_or(false)); }
    retain_displayable(&mut all_ids, &gs.nodes);

    let entry_label = if valid_entry_ids.len() == 1 { format!("`{}`", valid_entry_ids[0]) } else { format!("{} entry nodes", valid_entry_ids.len()) };
    let direction = params.direction.as_deref().unwrap_or("outgoing");
    let freshness = format_freshness(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status);

    if all_ids.is_empty() {
        let mode_desc = match mode {
            "neighbors" => format!("No {} neighbors for {}.", direction, entry_label),
            "impact" => format!("No dependents found for {} within {} hops.", entry_label, params.hops.unwrap_or(3)),
            "reachable" => format!("No reachable nodes from {} within {} hops.", entry_label, params.hops.unwrap_or(3)),
            "tests_for" => format!("No test functions found calling {}. Either no tests exist for this symbol, or the call edges haven't been extracted (check LSP status).", entry_label),
            _ => format!("No results for {}.", entry_label),
        };
        format!("{}{}{}", entry_header, mode_desc, freshness)
    } else {
        let md = format_neighbor_nodes(&gs.nodes, &all_ids, &gs.index, params.compact);
        let heading = match mode {
            "neighbors" => format!("## Graph neighbors ({}) of {}\n\n{} result(s)\n\n", direction, entry_label, all_ids.len()),
            "impact" => format!("## Impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n", entry_label, all_ids.len(), params.hops.unwrap_or(3)),
            "reachable" => format!("## Reachable from {}\n\n{} node(s) within {} hop(s)\n\n", entry_label, all_ids.len(), params.hops.unwrap_or(3)),
            "tests_for" => format!("## Test coverage for {}\n\n{} test function(s)\n\n", entry_label, all_ids.len()),
            _ => String::new(),
        };
        format!("{}{}{}{}", entry_header, heading, md, freshness)
    }
}

fn search_batch(node_ids: &[&str], params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    let gs = ctx.graph_state;
    let freshness = format_freshness(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status);
    if params.mode.is_some() {
        let mode = params.mode.as_deref().unwrap_or("neighbors");
        let edge_filter = params.edge_types.as_ref().map(|types| types.iter().filter_map(|t| parse_edge_kind(t)).collect::<Vec<_>>());
        let edge_filter_slice = edge_filter.as_deref();
        let mut sections: Vec<String> = Vec::new();
        for &nid in node_ids {
            if gs.index.get_node(nid).is_none() { sections.push(format!("### `{}`\n\nNode not found in graph.", nid)); continue; }
            match run_traversal(&gs.index, nid, mode, params.hops, params.direction.as_deref(), edge_filter_slice) {
                Ok(mut ids) => {
                    ids.retain(|id| id != nid);
                    if mode == "tests_for" { ids.retain(|id| gs.nodes.iter().find(|n| n.stable_id() == *id).map(ranking::is_test_file).unwrap_or(false)); }
                    retain_displayable(&mut ids, &gs.nodes);
                    if ids.is_empty() { sections.push(format!("### `{}`\n\nNo {} results.", nid, mode)); }
                    else { let md = format_neighbor_nodes(&gs.nodes, &ids, &gs.index, params.compact); sections.push(format!("### `{}`\n\n{} result(s)\n\n{}", nid, ids.len(), md)); }
                }
                Err(msg) => sections.push(format!("### `{}`\n\n{}", nid, msg)),
            }
        }
        format!("## Batch {} for {} node(s)\n\n{}{}", mode, node_ids.len(), sections.join("\n\n"), freshness)
    } else {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for &nid in node_ids { if let Some(node) = gs.nodes.iter().find(|n| n.stable_id() == nid) { found.push(node); } else { missing.push(nid); } }
        if found.is_empty() { return format!("No nodes found for {}. Try search to find valid node IDs.{}", node_ids.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", "), freshness); }
        let md: String = found.iter().map(|n| format_node_entry(n, &gs.index, params.compact)).collect::<Vec<_>>().join("\n\n");
        let mut result = format!("## Batch retrieve: {} found\n\n{}", found.len(), md);
        if !missing.is_empty() { result.push_str(&format!("\n\n**Missing:** {}", missing.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", "))); }
        result.push_str(&freshness);
        result
    }
}

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
    let result_ids = run_traversal(&graph_state.index, &params.node, &params.mode, params.max_hops.map(|h| h as u32), Some(params.direction.as_str()), edge_filter_slice)?;
    if result_ids.is_empty() { return Ok(format!("No results for `{}` ({}).", params.node, params.mode)); }
    let mut lines = vec![format!("## {} `{}`\n\n{} result(s)\n", params.mode, params.node, result_ids.len())];
    for id in &result_ids {
        if let Some(node) = graph_state.nodes.iter().find(|n| n.stable_id() == *id) {
            if matches!(node.id.kind, NodeKind::Module | NodeKind::PrMerge) { continue; }
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
    let last_scan_age = std::fs::metadata(&lance_path).and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| { let s = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() - d.as_secs();
            if s < 60 { "just now".into() } else if s < 3600 { format!("{}m ago", s/60) } else if s < 86400 { format!("{}h ago", s/3600) } else { format!("{}d ago", s/86400) } })
        .unwrap_or_else(|| "unknown".to_string());
    StatsResult { node_count: graph_state.nodes.len(), edge_count: graph_state.edges.len(), embeddings_available: embed_available,
        languages: langs.into_iter().collect(), last_scan_age, artifact_count: artifacts.len(), outcome_count: outcomes, signal_count: signals, guardrail_count: guardrails, metis_count: metis }
}

pub fn node_passes_root_filter(node_root: &str, root_filter: &Option<String>, non_code_slugs: &HashSet<String>) -> bool {
    match root_filter { None => true, Some(slug) => node_root.eq_ignore_ascii_case(slug) || node_root == "external" || non_code_slugs.contains(node_root) }
}

pub fn search_result_passes_root_filter(result: &crate::embed::SearchResult, root_filter: &Option<String>, non_code_slugs: &HashSet<String>) -> bool {
    if root_filter.is_none() { return true; }
    if !result.kind.starts_with("code:") { return true; }
    node_passes_root_filter(result.id.split(':').next().unwrap_or(""), root_filter, non_code_slugs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_search_params_default() { let p = SearchParams::default(); assert!(p.query.is_none()); assert!(!p.compact); }
    #[test] fn test_node_passes_root_filter_all() { assert!(node_passes_root_filter("any", &None, &HashSet::new())); }
    #[test] fn test_node_passes_root_filter_match() { assert!(node_passes_root_filter("my-root", &Some("my-root".into()), &HashSet::new())); }
    #[test] fn test_node_passes_root_filter_external() { assert!(node_passes_root_filter("external", &Some("my-root".into()), &HashSet::new())); }
    #[test] fn test_node_passes_root_filter_reject() { assert!(!node_passes_root_filter("other", &Some("my-root".into()), &HashSet::new())); }
}
