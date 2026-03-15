//! Shared service layer for CLI and MCP.
//!
//! Both interfaces are thin dispatchers to these functions. The service layer
//! defines the full capability surface -- adding a parameter here automatically
//! makes it available in both CLI and MCP.

use std::collections::HashSet;
use std::path::Path;

use crate::embed::{EmbeddingIndex, SearchMode, SearchOutcome};
use crate::graph::{Node, NodeKind};
use crate::ranking;
use crate::server::helpers::{
    format_freshness, format_neighbors_grouped, format_node_entry,
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

    // Try embedding-ranked code symbol search first; fall back to name/signature matching.
    let matches: Vec<&Node> = flat_code_symbol_search(
        query_str, search_mode, limit, params, graph_state, ctx,
        sort_by_complexity, sort_by_importance,
    ).await;

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

/// Find code symbols for flat search, using embedding index when available.
///
/// Strategy:
/// 1. If query is non-empty and embed index is available, use `search_with_mode`
///    to get semantically-ranked code symbols, then resolve to graph nodes.
/// 2. Fall back to name/signature string matching if embed is unavailable or not ready.
/// 3. Apply post-filters (kind, language, file, root, synthetic, min_complexity).
/// 4. Apply sort_by overrides (complexity, importance) if requested; otherwise
///    preserve embed ranking or use name-match ranking for fallback results.
#[allow(clippy::too_many_arguments)]
async fn flat_code_symbol_search<'a>(
    query_str: &str,
    search_mode: SearchMode,
    limit: usize,
    params: &SearchParams,
    graph_state: &'a GraphState,
    ctx: &SearchContext<'_>,
    sort_by_complexity: bool,
    sort_by_importance: bool,
) -> Vec<&'a Node> {
    let query_lower = query_str.to_lowercase();
    let complexity_search = params.min_complexity.is_some() || sort_by_complexity;

    // Closure: does a node pass all active filters?
    let node_passes_filters = |n: &Node| -> bool {
        if complexity_search && n.id.kind != NodeKind::Function { return false; }
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
    };

    // Try embed-ranked search for code symbols when query is non-empty.
    let mut used_embed = false;
    let mut matches: Vec<&Node> = if !query_str.is_empty() {
        if let Some(embed_idx) = ctx.embed_index {
            // Over-fetch to allow for post-filtering.
            let over_fetch = limit * 3;
            match embed_idx.search_with_mode(query_str, None, over_fetch, search_mode).await {
                Ok(SearchOutcome::Results(results)) => {
                    used_embed = true;
                    // Keep only code results, resolve to graph nodes, apply filters.
                    results.iter()
                        .filter(|r| r.kind.starts_with("code:"))
                        .filter_map(|r| graph_state.nodes.iter().find(|n| n.stable_id() == r.id))
                        .filter(|n| node_passes_filters(n))
                        .take(limit)
                        .collect()
                }
                // Embedding index not ready -- fall through to name/signature fallback.
                Ok(SearchOutcome::NotReady) => Vec::new(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Fallback: name/signature matching (when embed is unavailable, not ready, or query is empty).
    if !used_embed {
        matches = graph_state.nodes.iter().filter(|n| {
            if complexity_search && n.id.kind != NodeKind::Function { return false; }
            if !query_lower.is_empty() {
                let name_match = n.id.name.to_lowercase().contains(&query_lower) || n.signature.to_lowercase().contains(&query_lower);
                if !name_match { return false; }
            }
            node_passes_filters(n)
        }).collect();
    }

    // Apply sort overrides or default ranking.
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
    } else if !used_embed {
        // Only apply name-match ranking for fallback results; embed results
        // are already ranked by the embedding index.
        ranking::sort_symbol_matches(&mut matches, &query_lower, &graph_state.index);
    }
    matches.truncate(limit);
    matches
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

    // Collect grouped results across all entry nodes
    use crate::server::handlers::run_traversal_grouped;
    let mut merged_groups: std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>> = std::collections::BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    let entry_set: HashSet<&str> = valid_entry_ids.iter().map(|s| s.as_str()).collect();

    for node_id in &valid_entry_ids {
        match run_traversal_grouped(&gs.index, node_id, mode, params.hops, params.direction.as_deref(), edge_filter_slice) {
            Ok(groups) => {
                for (kind, ids) in groups {
                    let entry = merged_groups.entry(kind).or_default();
                    for id in ids {
                        if !entry_set.contains(id.as_str()) && seen.insert(id.clone()) {
                            entry.push(id);
                        }
                    }
                }
            }
            Err(msg) => return msg,
        }
    }

    // Apply tests_for filtering
    if mode == "tests_for" {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| gs.nodes.iter().find(|n| n.stable_id() == *id).map(ranking::is_test_file).unwrap_or(false));
        }
    }
    // Remove empty groups after filtering
    merged_groups.retain(|_, ids| !ids.is_empty());

    // Count total displayable results
    let total_count: usize = merged_groups.values().map(|ids| {
        ids.iter().filter(|id| {
            gs.nodes.iter().find(|n| n.stable_id() == **id)
                .map(|n| !crate::server::helpers::is_hidden_traversal_kind(&n.id.kind))
                .unwrap_or(true)
        }).count()
    }).sum();

    let entry_label = if valid_entry_ids.len() == 1 { format!("`{}`", valid_entry_ids[0]) } else { format!("{} entry nodes", valid_entry_ids.len()) };
    let direction = params.direction.as_deref().unwrap_or("outgoing");
    let freshness = format_freshness(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status);

    if total_count == 0 {
        let mode_desc = match mode {
            "neighbors" => format!("No {} neighbors for {}.", direction, entry_label),
            "impact" => format!("No dependents found for {} within {} hops.", entry_label, params.hops.unwrap_or(3)),
            "reachable" => format!("No reachable nodes from {} within {} hops.", entry_label, params.hops.unwrap_or(3)),
            "tests_for" => format!("No test functions found calling {}. Either no tests exist for this symbol, or the call edges haven't been extracted (check LSP status).", entry_label),
            _ => format!("No results for {}.", entry_label),
        };
        format!("{}{}{}", entry_header, mode_desc, freshness)
    } else {
        let md = format_neighbors_grouped(&gs.nodes, &merged_groups, &gs.index, params.compact);
        let heading = match mode {
            "neighbors" => format!("## Graph neighbors ({}) of {}\n\n{} result(s)\n\n", direction, entry_label, total_count),
            "impact" => format!("## Impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n", entry_label, total_count, params.hops.unwrap_or(3)),
            "reachable" => format!("## Reachable from {}\n\n{} node(s) within {} hop(s)\n\n", entry_label, total_count, params.hops.unwrap_or(3)),
            "tests_for" => format!("## Test coverage for {}\n\n{} test function(s)\n\n", entry_label, total_count),
            _ => String::new(),
        };
        format!("{}{}{}{}", entry_header, heading, md, freshness)
    }
}

fn search_batch(node_ids: &[&str], params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    use crate::server::handlers::run_traversal_grouped;
    let gs = ctx.graph_state;
    let freshness = format_freshness(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status);
    if params.mode.is_some() {
        let mode = params.mode.as_deref().unwrap_or("neighbors");
        let edge_filter = params.edge_types.as_ref().map(|types| types.iter().filter_map(|t| parse_edge_kind(t)).collect::<Vec<_>>());
        let edge_filter_slice = edge_filter.as_deref();
        let mut sections: Vec<String> = Vec::new();
        for &nid in node_ids {
            if gs.index.get_node(nid).is_none() { sections.push(format!("### `{}`\n\nNode not found in graph.", nid)); continue; }
            match run_traversal_grouped(&gs.index, nid, mode, params.hops, params.direction.as_deref(), edge_filter_slice) {
                Ok(mut groups) => {
                    // Remove self-references
                    for ids in groups.values_mut() {
                        ids.retain(|id| id != nid);
                    }
                    if mode == "tests_for" {
                        for ids in groups.values_mut() {
                            ids.retain(|id| gs.nodes.iter().find(|n| n.stable_id() == *id).map(ranking::is_test_file).unwrap_or(false));
                        }
                    }
                    groups.retain(|_, ids| !ids.is_empty());
                    let total: usize = groups.values().map(|ids| ids.len()).sum();
                    if total == 0 { sections.push(format!("### `{}`\n\nNo {} results.", nid, mode)); }
                    else { let md = format_neighbors_grouped(&gs.nodes, &groups, &gs.index, params.compact); sections.push(format!("### `{}`\n\n{} result(s)\n\n{}", nid, total, md)); }
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use crate::graph::{NodeId, ExtractionSource};
    use crate::graph::index::GraphIndex;

    fn make_node(name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: NodeId { kind, name: name.to_string(), file: PathBuf::from(file), root: "local".to_string() },
            language: "rust".to_string(), signature: format!("fn {}", name),
            line_start: 0, line_end: 10, body: String::new(),
            metadata: BTreeMap::new(), source: ExtractionSource::TreeSitter,
        }
    }

    fn make_graph_state(nodes: Vec<Node>) -> GraphState {
        let index = GraphIndex::new();
        GraphState { nodes, edges: vec![], index, last_scan_completed_at: None }
    }

    fn make_search_context<'a>(graph_state: &'a GraphState, repo_root: &'a Path) -> SearchContext<'a> {
        SearchContext {
            graph_state, embed_index: None, repo_root,
            lsp_status: None, root_filter: None, non_code_slugs: HashSet::new(),
        }
    }

    #[test] fn test_search_params_default() { let p = SearchParams::default(); assert!(p.query.is_none()); assert!(!p.compact); }
    #[test] fn test_node_passes_root_filter_all() { assert!(node_passes_root_filter("any", &None, &HashSet::new())); }
    #[test] fn test_node_passes_root_filter_match() { assert!(node_passes_root_filter("my-root", &Some("my-root".into()), &HashSet::new())); }
    #[test] fn test_node_passes_root_filter_external() { assert!(node_passes_root_filter("external", &Some("my-root".into()), &HashSet::new())); }
    #[test] fn test_node_passes_root_filter_reject() { assert!(!node_passes_root_filter("other", &Some("my-root".into()), &HashSet::new())); }

    // ── flat_code_symbol_search tests ──────────────────────────────────

    /// Without embed index, flat search falls back to name/signature matching.
    #[tokio::test]
    async fn test_flat_search_fallback_name_matching() {
        let nodes = vec![
            make_node("auth_handler", NodeKind::Function, "src/auth.rs"),
            make_node("db_connect", NodeKind::Function, "src/db.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams { query: Some("auth".into()), ..Default::default() };

        let results = flat_code_symbol_search(
            "auth", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "auth_handler");
    }

    /// Fallback matches against signature too.
    #[tokio::test]
    async fn test_flat_search_fallback_signature_matching() {
        let mut node = make_node("process", NodeKind::Function, "src/proc.rs");
        node.signature = "fn process(auth_token: &str)".to_string();
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams { query: Some("auth_token".into()), ..Default::default() };

        let results = flat_code_symbol_search(
            "auth_token", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "process");
    }

    /// Kind filter works with fallback path.
    #[tokio::test]
    async fn test_flat_search_fallback_kind_filter() {
        let nodes = vec![
            make_node("Config", NodeKind::Struct, "src/config.rs"),
            make_node("config_init", NodeKind::Function, "src/config.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("config".into()),
            kind: Some("struct".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "config", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "Config");
    }

    /// Language filter works with fallback path.
    #[tokio::test]
    async fn test_flat_search_fallback_language_filter() {
        let mut py_node = make_node("handler", NodeKind::Function, "src/handler.py");
        py_node.language = "python".to_string();
        let rs_node = make_node("handler", NodeKind::Function, "src/handler.rs");
        let gs = make_graph_state(vec![py_node, rs_node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("handler".into()),
            language: Some("python".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].language, "python");
    }

    /// File filter works with fallback path.
    #[tokio::test]
    async fn test_flat_search_fallback_file_filter() {
        let nodes = vec![
            make_node("parse", NodeKind::Function, "src/parser.rs"),
            make_node("parse", NodeKind::Function, "src/config.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("parse".into()),
            file: Some("parser".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "parse", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1);
        assert!(results[0].id.file.to_string_lossy().contains("parser"));
    }

    /// sort_by=complexity works with fallback path.
    #[tokio::test]
    async fn test_flat_search_sort_by_complexity() {
        let mut low = make_node("simple", NodeKind::Function, "a.rs");
        low.metadata.insert("cyclomatic".into(), "2".into());
        let mut high = make_node("complex", NodeKind::Function, "b.rs");
        high.metadata.insert("cyclomatic".into(), "15".into());
        let gs = make_graph_state(vec![low, high]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            kind: Some("function".into()),
            sort_by: Some("complexity".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "", SearchMode::Hybrid, 10, &params, &gs, &ctx, true, false,
        ).await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.name, "complex");
        assert_eq!(results[1].id.name, "simple");
    }

    /// sort_by=importance works with fallback path.
    #[tokio::test]
    async fn test_flat_search_sort_by_importance() {
        let mut low = make_node("leaf", NodeKind::Function, "a.rs");
        low.metadata.insert("importance".into(), "0.01".into());
        let mut high = make_node("hub", NodeKind::Function, "b.rs");
        high.metadata.insert("importance".into(), "0.95".into());
        let gs = make_graph_state(vec![low, high]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("".into()),
            kind: Some("function".into()),
            sort_by: Some("importance".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, true,
        ).await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.name, "hub");
    }

    /// search_mode is parsed correctly for all variants.
    #[test]
    fn test_search_mode_parsing_coverage() {
        assert!(matches!(parse_search_mode(None), SearchMode::Hybrid));
        assert!(matches!(parse_search_mode(Some("hybrid")), SearchMode::Hybrid));
        assert!(matches!(parse_search_mode(Some("keyword")), SearchMode::Keyword));
        assert!(matches!(parse_search_mode(Some("semantic")), SearchMode::Semantic));
        assert!(matches!(parse_search_mode(Some("SEMANTIC")), SearchMode::Semantic));
        assert!(matches!(parse_search_mode(Some("unknown")), SearchMode::Hybrid));
    }

    /// Empty query with no filters returns empty results (via the search function).
    #[tokio::test]
    async fn test_flat_search_empty_query_no_filters() {
        let gs = make_graph_state(vec![make_node("foo", NodeKind::Function, "a.rs")]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let result = search(&params, &ctx).await;
        assert!(result.contains("Empty query"), "Should reject empty query without filters");
    }

    /// Verify full search function respects search_mode parameter in output
    /// (no error, produces results via fallback when embed is absent).
    #[tokio::test]
    async fn test_flat_search_with_search_mode_no_embed() {
        let nodes = vec![make_node("auth_handler", NodeKind::Function, "src/auth.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            search_mode: Some("semantic".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(result.contains("auth_handler"), "Fallback should find by name even with search_mode=semantic");
        assert!(result.contains("Code symbols"), "Should have code symbols section");
    }

    /// min_complexity filter works with the new code path.
    #[tokio::test]
    async fn test_flat_search_min_complexity_filter() {
        let mut simple = make_node("simple", NodeKind::Function, "a.rs");
        simple.metadata.insert("cyclomatic".into(), "2".into());
        let mut complex = make_node("complex", NodeKind::Function, "b.rs");
        complex.metadata.insert("cyclomatic".into(), "10".into());
        let gs = make_graph_state(vec![simple, complex]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            min_complexity: Some(5),
            kind: Some("function".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "complex");
    }

    // ── Adversarial tests (seeded from dissent) ──────────────────────

    /// Dissent #1: Multiple filters stacked -- kind + language + file should
    /// all compose correctly in fallback path.
    #[tokio::test]
    async fn test_flat_search_stacked_filters() {
        let mut target = make_node("handler", NodeKind::Function, "src/api/handler.rs");
        target.language = "rust".to_string();
        let mut wrong_kind = make_node("handler", NodeKind::Struct, "src/api/handler.rs");
        wrong_kind.language = "rust".to_string();
        let mut wrong_lang = make_node("handler", NodeKind::Function, "src/api/handler.py");
        wrong_lang.language = "python".to_string();
        let wrong_file = make_node("handler", NodeKind::Function, "src/db/handler.rs");
        let gs = make_graph_state(vec![target, wrong_kind, wrong_lang, wrong_file]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("handler".into()),
            kind: Some("function".into()),
            language: Some("rust".into()),
            file: Some("api".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1, "Only one node should pass all three filters");
        assert_eq!(results[0].id.name, "handler");
        assert!(results[0].id.file.to_string_lossy().contains("api"));
    }

    /// Dissent #2: Limit respected when more results available.
    #[tokio::test]
    async fn test_flat_search_limit_respected() {
        let nodes: Vec<Node> = (0..20)
            .map(|i| make_node(&format!("fn_{}", i), NodeKind::Function, "src/lib.rs"))
            .collect();
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("fn".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "fn", SearchMode::Hybrid, 5, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 5, "Should respect limit of 5 even with 20 matches");
    }

    /// Dissent #3: Root filter rejects non-matching roots in fallback.
    #[tokio::test]
    async fn test_flat_search_root_filter_fallback() {
        let mut local = make_node("handler", NodeKind::Function, "src/handler.rs");
        local.id.root = "my-project".to_string();
        let mut other = make_node("handler", NodeKind::Function, "src/handler.rs");
        other.id.root = "other-project".to_string();
        let gs = make_graph_state(vec![local, other]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = SearchContext {
            graph_state: &gs, embed_index: None, repo_root: &repo_root,
            lsp_status: None, root_filter: Some("my-project".into()),
            non_code_slugs: HashSet::new(),
        };
        let params = SearchParams {
            query: Some("handler".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1, "Should only return nodes from matching root");
        assert_eq!(results[0].id.root, "my-project");
    }

    /// Dissent #3: synthetic filter works correctly.
    #[tokio::test]
    async fn test_flat_search_synthetic_filter() {
        let mut synth = make_node("CONSTANT", NodeKind::Const, "src/lib.rs");
        synth.metadata.insert("synthetic".into(), "true".into());
        let real = make_node("real_fn", NodeKind::Function, "src/lib.rs");
        let gs = make_graph_state(vec![synth, real]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Only synthetic
        let params = SearchParams {
            kind: Some("const".into()),
            synthetic: Some(true),
            ..Default::default()
        };
        let results = flat_code_symbol_search(
            "", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "CONSTANT");

        // Only non-synthetic
        let params2 = SearchParams {
            synthetic: Some(false),
            kind: Some("function".into()),
            ..Default::default()
        };
        let results2 = flat_code_symbol_search(
            "", SearchMode::Hybrid, 10, &params2, &gs, &ctx, false, false,
        ).await;
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].id.name, "real_fn");
    }
}

// ── Outcome progress ───────────────────────────────────────────────

#[derive(Debug)]
pub struct OutcomeProgressParams {
    pub outcome_id: String,
    pub include_impact: bool,
    pub root_filter: Option<String>,
    pub non_code_slugs: HashSet<String>,
}

pub struct OutcomeProgressContext<'a> {
    pub graph_state: &'a crate::server::state::GraphState,
    pub repo_root: &'a Path,
}

pub fn outcome_progress(params: &OutcomeProgressParams, ctx: &OutcomeProgressContext<'_>) -> String {
    let graph_nodes: Vec<crate::graph::Node> = ctx.graph_state.nodes.iter()
        .filter(|n| node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs))
        .cloned().collect();
    match crate::query::outcome_progress(ctx.repo_root, &params.outcome_id, &graph_nodes) {
        Ok(result) => {
            let mut md = result.to_summary_markdown();
            let file_patterns: Vec<String> = result.outcomes.first()
                .and_then(|o| o.frontmatter.get("files")).and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            let pr_nodes = crate::query::find_pr_merges_for_outcome(&ctx.graph_state.nodes, &ctx.graph_state.edges, &params.outcome_id, &file_patterns);
            if !pr_nodes.is_empty() { md.push_str(&format!("\n## PR Merges\n\n{} PR merge(s) serving this outcome\n", pr_nodes.len())); }
            if params.include_impact && !result.code_symbols.is_empty() {
                let impacted = crate::query::compute_impact_risk(&result.code_symbols, &graph_nodes, &ctx.graph_state.index, 3);
                md.push('\n'); md.push_str(&crate::query::format_impact_markdown(&impacted));
            } else if params.include_impact && result.code_symbols.is_empty() {
                md.push_str("\n## Change Impact\n\nNo changed symbols found -- cannot compute blast radius.\n");
            }
            md
        }
        Err(e) => format!("Error: {}", e),
    }
}

// ── List roots ──────────────────────────────────────────────────────

pub fn list_roots(repo_root: &Path) -> String {
    let workspace = crate::roots::WorkspaceConfig::load().with_primary_root(repo_root.to_path_buf()).with_worktrees(repo_root).with_claude_memory(repo_root);
    let resolved = workspace.resolved_roots();
    if resolved.is_empty() { return "No workspace roots configured.".to_string(); }
    let md: String = resolved.iter().enumerate()
        .map(|(i, r)| { let primary = if i == 0 { " (primary)" } else { "" }; format!("- **{}**{}: `{}` (type: {}, git: {})", r.slug, primary, r.path.display(), r.config.root_type, r.config.git_aware) })
        .collect::<Vec<_>>().join("\n");
    format!("## Workspace Roots\n\n{} root(s)\n\n{}", resolved.len(), md)
}

// ── Repo map ────────────────────────────────────────────────────────

const IMPORTANCE_THRESHOLD: f64 = 0.001;

#[derive(Debug)]
pub struct RepoMapParams { pub top_n: usize, pub root_filter: Option<String>, pub non_code_slugs: HashSet<String> }
pub struct RepoMapContext<'a> { pub graph_state: &'a crate::server::state::GraphState, pub repo_root: &'a Path, pub lsp_status: Option<&'a LspEnrichmentStatus> }

pub fn repo_map(params: &RepoMapParams, ctx: &RepoMapContext<'_>) -> String {
    let graph_state = ctx.graph_state;
    let mut sections: Vec<String> = Vec::new();
    {
        let mut swi: Vec<(&Node, f64)> = graph_state.nodes.iter()
            .filter(|n| !matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field))
            .filter(|n| n.id.root != "external")
            .filter(|n| node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs))
            .filter_map(|n| { let imp = n.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let imp = if ranking::is_test_file(n) { imp * 0.1 } else { imp };
                if imp > IMPORTANCE_THRESHOLD { Some((n, imp)) } else { None } }).collect();
        swi.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        swi.truncate(params.top_n);
        if !swi.is_empty() {
            let md: String = swi.iter().map(|(n, imp)| { let mut line = format!("- **{}** `{}` ({}) [{}] `{}`:{}-{} -- importance: {:.3}", n.id.kind, n.id.name, n.language, n.id.root, n.id.file.display(), n.line_start, n.line_end, imp);
                if let Some(cc) = n.metadata.get("cyclomatic") { line.push_str(&format!(", complexity: {}", cc)); } line }).collect::<Vec<_>>().join("\n");
            sections.push(format!("## Top {} symbols by importance\n\n{}", swi.len(), md));
        }
    }
    { let mut fc: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
        for n in &graph_state.nodes { if matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field) { continue; }
            if n.id.root == "external" { continue; }
            if !node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs) { continue; }
            *fc.entry((n.id.root.clone(), n.id.file.display().to_string())).or_default() += 1; }
        let mut sf: Vec<_> = fc.into_iter().collect(); sf.sort_by(|a, b| b.1.cmp(&a.1)); sf.truncate(10);
        if !sf.is_empty() { let md: String = sf.iter().map(|((root, f), count)| format!("- [{}] `{}` -- {} definitions", root, f, count)).collect::<Vec<_>>().join("\n"); sections.push(format!("## Hotspot files\n\n{}", md)); } }
    { let outcomes = crate::oh::load_oh_artifacts(ctx.repo_root).unwrap_or_default().into_iter().filter(|a| a.kind == crate::types::OhArtifactKind::Outcome).collect::<Vec<_>>();
        if !outcomes.is_empty() { let md: String = outcomes.iter().map(|o| { let files: Vec<String> = o.frontmatter.get("files").and_then(|v| v.as_sequence()).map(|seq| seq.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            let fs = if files.is_empty() { String::new() } else { format!(" (files: {})", files.join(", ")) }; format!("- **{}**{}", o.id(), fs) }).collect::<Vec<_>>().join("\n"); sections.push(format!("## Active outcomes\n\n{}", md)); } }
    { let mut ep: Vec<&Node> = graph_state.nodes.iter().filter(|n| n.id.kind == NodeKind::Function && n.id.root != "external")
            .filter(|n| node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs))
            .filter(|n| { let name = n.id.name.to_lowercase(); name == "main" || name.starts_with("handle_") || name.starts_with("handler") || name.ends_with("_handler") || name.contains("endpoint") }).collect();
        ep.sort_by(|a, b| { let ia = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0); let ib = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0); ib.partial_cmp(&ia).unwrap_or(std::cmp::Ordering::Equal) });
        ep.truncate(10);
        if !ep.is_empty() { let md: String = ep.iter().map(|n| format!("- **{}** [{}] `{}`:{}-{}", n.id.name, n.id.root, n.id.file.display(), n.line_start, n.line_end)).collect::<Vec<_>>().join("\n"); sections.push(format!("## Entry points\n\n{}", md)); } }
    let freshness = format_freshness(graph_state.nodes.len(), graph_state.last_scan_completed_at, ctx.lsp_status);
    if sections.is_empty() { format!("No repository data available yet.{}", freshness) } else { format!("# Repository Map\n\n{}{}", sections.join("\n\n"), freshness) }
}
