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
    format_freshness_full, format_neighbors_grouped_with_root,
    format_node_entry_with_root, strip_root_prefix,
};
use crate::server::handlers::{parse_search_mode, run_traversal};
use crate::server::state::{EmbeddingStatus, GraphState, LspEnrichmentStatus, LspState};
use crate::server::store::parse_edge_kind;

/// When impact results exceed this node-count threshold, render a
/// subsystem-grouped summary instead of listing every node.
const IMPACT_SUMMARY_NODE_THRESHOLD: usize = 30;

/// Even when the node count is below the node threshold, if the rendered output
/// exceeds this character limit we retroactively switch to the summary view.
/// This catches cases where a small number of nodes with verbose bodies (non-
/// compact mode) still produce huge responses (e.g., 157K chars for ~80 nodes).
const IMPACT_SUMMARY_CHAR_THRESHOLD: usize = 40_000;

/// Interface-agnostic search parameters.
#[derive(Debug, Default)]
pub struct SearchParams {
    pub query: Option<String>,
    pub node: Option<String>,
    pub mode: Option<String>,
    pub hops: Option<u32>,
    pub depth: Option<u32>,
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
    pub rerank: bool,
    pub include_artifacts: bool,
    pub include_markdown: bool,
    pub artifact_types: Option<Vec<String>>,
    pub subsystem: Option<String>,
    pub target_subsystem: Option<String>,
}

impl SearchParams {
    /// Convert from MCP `Search` tool struct.
    pub fn from_mcp_search(args: &crate::server::tools::Search) -> Self {
        Self {
            query: args.query.clone(),
            node: args.node.clone(),
            mode: args.mode.clone(),
            hops: args.hops,
            depth: args.depth,
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
            rerank: args.rerank.unwrap_or(false),
            include_artifacts: args.include_artifacts.unwrap_or(true),
            include_markdown: args.include_markdown.unwrap_or(true),
            artifact_types: args.artifact_types.clone(),
            subsystem: args.subsystem.clone(),
            target_subsystem: args.target_subsystem.clone(),
        }
    }
}

/// Runtime context for search operations.
pub struct SearchContext<'a> {
    pub graph_state: &'a GraphState,
    pub embed_index: Option<&'a EmbeddingIndex>,
    pub repo_root: &'a Path,
    pub lsp_status: Option<&'a LspEnrichmentStatus>,
    pub embed_status: Option<&'a EmbeddingStatus>,
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
        // depth > 1 is not supported for batched traversal (nodes=[...]).
        // Use node= (single node) instead, or call search separately for each node.
        if params.depth.unwrap_or(1) > 1 && params.mode.as_deref() == Some("neighbors") {
            return "depth > 1 is not supported with nodes=[...] batched traversal. Use node= for a single entry point with depth traversal.".to_string();
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
    let has_file_filter = params.file.is_some();
    let has_synthetic_filter = params.synthetic.is_some();
    let has_subsystem_filter = params.subsystem.is_some();
    let has_browse_filter = has_kind_filter || has_file_filter || has_synthetic_filter || has_subsystem_filter;

    let query_str = query.unwrap_or("");
    if query_str.is_empty() && !complexity_search && !sort_by_importance && !has_browse_filter {
        return "Empty query. Please describe what you're looking for (or use kind, file, synthetic, min_complexity, sort_by=\"complexity\", or sort_by=\"importance\").".to_string();
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
        let strip = ctx.root_filter.as_deref();
        let md: String = matches.iter().map(|n| format_node_entry_with_root(n, &graph_state.index, params.compact, strip)).collect::<Vec<_>>().join("\n\n");
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
                let workspace = crate::roots::WorkspaceConfig::load().with_primary_root(ctx.repo_root.to_path_buf()).with_worktrees(ctx.repo_root).with_claude_memory(ctx.repo_root).with_agent_memories(ctx.repo_root).with_declared_roots(ctx.repo_root);
                let root_path = workspace.resolved_roots().into_iter().find(|r| r.slug.eq_ignore_ascii_case(slug)).map(|r| r.path);
                if let Some(rp) = root_path { chunks.into_iter().filter(|c| c.file_path.starts_with(&rp)).collect() } else { Vec::new() }
            } else { chunks };
            let scored = crate::markdown::search_chunks_ranked(&filtered_chunks, query_str);
            if !scored.is_empty() {
                let md = scored.iter().take(limit).map(|sc| format!("- (score: {:.2}) {}", sc.score, sc.chunk.to_markdown())).collect::<Vec<_>>().join("\n\n---\n\n");
                sections.push(format!("### Markdown ({} result(s))\n\n{}", scored.len().min(limit), md));
            }
        }
    }

    let freshness = format_freshness_full(graph_state.nodes.len(), graph_state.last_scan_completed_at, ctx.lsp_status, ctx.embed_status);
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

    // Detect path/name split query (e.g. "auth/handlers/validate" → path="auth/handlers", name="validate").
    // When present, embed search uses only the name part; name-matching filters by both.
    let path_name = parse_path_name_query(query_str);
    let (path_filter_lower, name_filter_lower): (Option<String>, Option<String>) =
        if let Some((p, n)) = path_name {
            (Some(p.to_lowercase()), Some(n.to_lowercase()))
        } else {
            (None, None)
        };
    // The string forwarded to the embed index: name-part only for path/name queries
    // so the embedding attends to the symbol name rather than the slash-separated path.
    let embed_query_str: &str = name_filter_lower.as_deref().unwrap_or(query_str);

    // Build O(1) lookup map: stable_id -> index into graph_state.nodes.
    // Replaces O(N) linear scans per result when resolving embed results.
    let node_index_map = graph_state.node_index_map();

    // Closure: does a node pass path/name + all active filters?
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
        if let Some(ref sub) = params.subsystem {
            let node_sub = n.metadata.get(crate::server::SUBSYSTEM_KEY).map(|s| s.as_str()).unwrap_or("");
            if !subsystem_matches(node_sub, sub) { return false; }
        }
        // Path/name split filter: when query contained `/`, require both file-path
        // and name to match their respective parts.
        if let (Some(pf), Some(nf)) = (&path_filter_lower, &name_filter_lower) {
            let file_match = n.id.file.to_string_lossy().to_lowercase().contains(pf.as_str());
            let name_match = n.id.name.to_lowercase().contains(nf.as_str());
            if !file_match || !name_match { return false; }
        }
        true
    };

    // When reranking is requested, over-fetch more candidates so the
    // cross-encoder has a wider pool to re-score.
    let rerank_over_fetch = if params.rerank { limit.max(20) } else { limit };

    // Try embed-ranked search for code symbols when query is non-empty.
    // For path/name queries use only the name part so the embedding attends to
    // the symbol name rather than a slash-delimited path string.
    let mut used_embed = false;
    let mut matches: Vec<&Node> = if !query_str.is_empty() {
        if let Some(embed_idx) = ctx.embed_index {
            // Over-fetch to allow for post-filtering (and reranking).
            let over_fetch = rerank_over_fetch * 3;
            match embed_idx.search_with_mode(embed_query_str, None, over_fetch, search_mode).await {
                Ok(SearchOutcome::Results(results)) => {
                    used_embed = true;
                    // Keep only code results, resolve to graph nodes via HashMap (O(1)), apply filters.
                    // node_passes_filters already handles the path/name split check.
                    results.iter()
                        .filter(|r| r.kind.starts_with("code:"))
                        .filter_map(|r| graph_state.node_by_stable_id(&r.id, &node_index_map))
                        .filter(|n| node_passes_filters(n))
                        .take(rerank_over_fetch)
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

    // Supplement or fallback: name/signature matching.
    //
    // When embed search was used, exact name matches that the embedding missed
    // are appended after the embed-ranked results. This ensures functions are
    // always findable by name regardless of embedding quality or index freshness.
    // (#275: without this, `search("embed_texts")` returned zero code results
    // because the embedding didn't surface the function and no fallback fired.)
    //
    // When embed search was NOT used (unavailable, not ready, or empty query),
    // name/signature matching is the sole source of results.
    if !used_embed {
        matches = graph_state.nodes.iter().filter(|n| {
            if complexity_search && n.id.kind != NodeKind::Function { return false; }
            if !query_lower.is_empty() && path_name.is_none() {
                // Plain query: check name/signature directly here for early exit.
                // Path/name queries are handled inside node_passes_filters.
                let name_match = n.id.name.to_lowercase().contains(&query_lower) || n.signature.to_lowercase().contains(&query_lower);
                if !name_match { return false; }
            }
            node_passes_filters(n)
        }).collect();
    } else if !query_lower.is_empty() {
        // Embed search was used -- supplement with name/signature matches
        // that the embedding missed. Deduplicate by stable_id so embed-ranked
        // results keep their position; supplements are appended at the end.
        //
        // Cap supplements to avoid blowing up the reranker candidate pool
        // and reserve slots so supplements survive the downstream truncate.
        let supplement_budget = limit.min(10);
        let seen: std::collections::HashSet<String> = matches.iter()
            .map(|n| n.stable_id())
            .collect();
        let name_supplements: Vec<&Node> = graph_state.nodes.iter().filter(|n| {
            if seen.contains(&n.stable_id()) { return false; }
            if path_name.is_none() {
                // Plain query: check name/signature for early exit.
                // Path/name queries are handled inside node_passes_filters.
                let name_match = n.id.name.to_lowercase().contains(&query_lower)
                    || n.signature.to_lowercase().contains(&query_lower);
                if !name_match { return false; }
            }
            node_passes_filters(n)
        }).collect();
        if !name_supplements.is_empty() {
            // Sort supplements by name-match quality, then cap to budget.
            // For path/name queries use only the name part for ranking.
            let sort_key = name_filter_lower.as_deref().unwrap_or(&query_lower);
            let mut sorted_supplements = name_supplements;
            ranking::sort_symbol_matches(&mut sorted_supplements, sort_key, &graph_state.index);
            sorted_supplements.truncate(supplement_budget);
            // Evict tail embed results to make room so supplements survive
            // the final truncate(limit).
            let reserved = sorted_supplements.len();
            if matches.len() + reserved > limit {
                matches.truncate(limit.saturating_sub(reserved));
            }
            matches.extend(sorted_supplements);
        }
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
        // For path/name queries use only the name part for ranking.
        let sort_key = name_filter_lower.as_deref().unwrap_or(&query_lower);
        ranking::sort_symbol_matches(&mut matches, sort_key, &graph_state.index);
    }

    // Cross-encoder reranking: re-score the top candidates using a cross-encoder
    // model that attends to (query, document) pairs jointly. This produces more
    // precise relevance scores than bi-encoder similarity alone.
    // Skip reranking when an explicit sort_by mode is active (complexity,
    // importance) -- the caller's sort request takes precedence.
    let use_relevance_sort = !sort_by_complexity && !sort_by_importance;
    if params.rerank && use_relevance_sort && !query_str.is_empty() && matches.len() > 1 {
        use crate::rerank::{RerankCandidate, rerank_results};

        let candidates: Vec<RerankCandidate> = matches
            .iter()
            .enumerate()
            .map(|(i, node)| {
                // Build reranking text from signature + body (the full context
                // the cross-encoder should attend to).
                let text = if node.body.is_empty() {
                    node.signature.clone()
                } else {
                    format!("{}\n{}", node.signature, node.body)
                };
                RerankCandidate {
                    text,
                    original_index: i,
                }
            })
            .collect();

        // Run reranking on a blocking thread to avoid blocking the Tokio
        // executor during ONNX model inference (and possible first-time
        // model download/initialization).
        let query_owned = query_str.to_string();
        let rerank_result = tokio::task::spawn_blocking(move || {
            rerank_results(&query_owned, &candidates)
        }).await;

        match rerank_result {
            Ok(Ok(reranked)) => {
                let original_matches = matches.clone();
                matches = reranked
                    .iter()
                    .filter_map(|r| original_matches.get(r.original_index).copied())
                    .collect();
                tracing::debug!(
                    "Reranked {} candidates for query \"{}\"",
                    reranked.len(),
                    query_str
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "Cross-encoder reranking failed, using original order: {}",
                    e
                );
                // Fall through with original ordering -- reranking is best-effort.
            }
            Err(e) => {
                tracing::warn!(
                    "Reranking task panicked, using original order: {}",
                    e
                );
            }
        }
    }

    matches.truncate(limit);
    matches
}

async fn search_traversal(params: &SearchParams, query: Option<&str>, node: Option<&str>, ctx: &SearchContext<'_>) -> String {
    let mode = params.mode.as_deref().unwrap_or("neighbors");
    let top_k = params.limit.unwrap_or(1).clamp(1, 50);

    // ── cycles mode ─────────────────────────────────────────────────────────
    // No entry-point resolution needed: we run tarjan_scc over the full graph.
    // If `node` is provided, return only the ring containing that node.
    // Otherwise return all rings (useful for a global circular-dependency audit).
    if mode == "cycles" {
        let gs = ctx.graph_state;
        let edge_filter = params.edge_types.as_ref().map(|types| {
            types.iter().filter_map(|t| parse_edge_kind(t)).collect::<Vec<_>>()
        });
        let edge_filter_slice = edge_filter.as_deref();
        let freshness = format_freshness_full(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status, ctx.embed_status);
        let strip = ctx.root_filter.as_deref();

        if let Some(node_id) = node {
            let resolved = gs.resolve_node_id(node_id);
            if gs.index.get_node(&resolved).is_none() {
                return format!(
                    "Node `{}` not found in graph. Use search to find valid node IDs.{freshness}",
                    strip_root_prefix(&resolved, strip),
                );
            }
            return match gs.index.cycle_for_node(&resolved, edge_filter_slice) {
                Some(ring) => {
                    let labels: Vec<String> = ring.iter()
                        .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                        .collect();
                    format!(
                        "## Cycle containing `{}`\n\n{} node(s) in ring\n\n{}{freshness}",
                        strip_root_prefix(&resolved, strip),
                        labels.len(),
                        labels.join(" → ") + " → ...",
                    )
                }
                None => format!(
                    "`{}` is not part of any circular dependency.{freshness}",
                    strip_root_prefix(&resolved, strip),
                ),
            };
        }

        // No node specified: return all rings.
        let rings = gs.index.detect_cycles(edge_filter_slice);
        if rings.is_empty() {
            let scope = match edge_filter_slice {
                Some(kinds) if !kinds.is_empty() => {
                    let labels: Vec<String> = kinds.iter().map(|k| format!("{k}")).collect();
                    format!("filtered edges: {}", labels.join(", "))
                }
                _ => "default coupling graph (Calls + DependsOn)".to_string(),
            };
            return format!("## Circular dependency analysis\n\nNo cycles detected in the {scope}.{freshness}");
        }
        let mut out = format!("## Circular dependency analysis\n\n{} ring(s) detected\n\n", rings.len());
        for (i, ring) in rings.iter().enumerate() {
            let labels: Vec<String> = ring.iter()
                .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                .collect();
            out.push_str(&format!("### Ring {}: {} nodes\n{}\n\n", i + 1, ring.len(), labels.join(" → ") + " → ..."));
        }
        out.push_str(&freshness);
        return out;
    }

    // ── path mode ────────────────────────────────────────────────────────────
    // Computes the shortest directed call path from `node` (start) to `query`
    // (destination). Both are resolved via the usual name-matching machinery.
    // Returns the ordered hop list: start → hop1 → hop2 → ... → destination.
    if mode == "path" {
        if node.is_none() || query.is_none() {
            return "path mode requires both node= (start) and query= (destination).".to_string();
        }
        let gs = ctx.graph_state;
        let from_raw = node.unwrap();
        let to_raw   = query.unwrap();
        let from_id  = gs.resolve_node_id(from_raw);
        let to_id    = gs.resolve_node_id(to_raw);
        let edge_filter = params.edge_types.as_ref().map(|types| {
            types.iter().filter_map(|t| parse_edge_kind(t)).collect::<Vec<_>>()
        });
        let edge_filter_slice = edge_filter.as_deref();
        let freshness = format_freshness_full(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status, ctx.embed_status);
        let strip = ctx.root_filter.as_deref();

        if gs.index.get_node(&from_id).is_none() {
            return format!(
                "Start node `{}` not found in graph. Use search to find valid node IDs.{freshness}",
                strip_root_prefix(&from_id, strip),
            );
        }
        if gs.index.get_node(&to_id).is_none() {
            return format!(
                "Destination node `{}` not found in graph. Use search to find valid node IDs.{freshness}",
                strip_root_prefix(&to_id, strip),
            );
        }

        return match gs.index.shortest_path(&from_id, &to_id, edge_filter_slice) {
            None => format!(
                "No directed call path from `{}` to `{}`.{freshness}",
                strip_root_prefix(&from_id, strip),
                strip_root_prefix(&to_id, strip),
            ),
            Some(hops) if hops.is_empty() => format!(
                "`{}` and `{}` are the same node — no path needed.{freshness}",
                strip_root_prefix(&from_id, strip),
                strip_root_prefix(&to_id, strip),
            ),
            Some(hops) => {
                let hop_count = hops.len(); // number of edges = number of directed calls
                let all_nodes: Vec<String> = std::iter::once(from_id.clone())
                    .chain(hops.iter().cloned())
                    .collect();
                let labels: Vec<String> = all_nodes.iter()
                    .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                    .collect();
                format!(
                    "## Call path: {} → {}\n\n{} hop(s)\n\n{}{freshness}",
                    strip_root_prefix(&from_id, strip),
                    strip_root_prefix(&to_id, strip),
                    hop_count,
                    labels.join(" → "),
                )
            }
        };
    }

    if node.is_none() && query.is_none() {
        return "Either query or node is required. Provide a search query or a stable node ID.".to_string();
    }

    let search_mode = parse_search_mode(params.search_mode.as_deref());
    let (entry_node_ids, entry_header): (Vec<String>, String) = if let Some(node_id) = node {
        // Resolve short IDs (without root prefix) to full stable IDs.
        // Search results display `src/file.rs:name:kind` but graph needs `root:src/file.rs:name:kind`.
        let resolved = ctx.graph_state.resolve_node_id(node_id);
        // If resolve_node_id couldn't find the node AND the node_id contains `/`,
        // try path/name resolution before falling through.  This lets callers use
        // `node="auth/handlers/validate"` without knowing the full stable ID.
        if ctx.graph_state.index.get_node(&resolved).is_none()
            && parse_path_name_query(node_id).is_some()
        {
            let name_matches = resolve_entry_points_by_name(node_id, top_k, params, ctx);
            if !name_matches.is_empty() {
                let mut header = format!("### Matched entry nodes for \"{}\" (path/name match)\n\n", node_id);
                let strip = ctx.root_filter.as_deref();
                let ids: Vec<String> = name_matches.iter().map(|n| {
                    let stable_id = n.id.to_stable_id();
                    let display = strip_root_prefix(&stable_id, strip);
                    header.push_str(&format!("- `{}` -- {} {}\n", display, n.id.kind, n.id.name));
                    stable_id
                }).collect();
                header.push('\n');
                (ids, header)
            } else {
                (vec![resolved], String::new())
            }
        } else {
            (vec![resolved], String::new())
        }
    } else if let Some(query_text) = query {
        // Try name matching against graph nodes first (#290).
        // This ensures `search("SearchParams", kind: "struct", mode: "neighbors")`
        // finds the struct by name, not by semantic similarity to random markdown.
        let name_matches = resolve_entry_points_by_name(query_text, top_k, params, ctx);
        if !name_matches.is_empty() {
            let mut header = format!("### Matched entry nodes for \"{}\" (name match)\n\n", query_text);
            let strip = ctx.root_filter.as_deref();
            let ids: Vec<String> = name_matches.iter().map(|n| {
                let stable_id = n.id.to_stable_id();
                let display = strip_root_prefix(&stable_id, strip);
                header.push_str(&format!("- `{}` -- {} {}\n", display, n.id.kind, n.id.name));
                stable_id
            }).collect();
            header.push('\n');
            (ids, header)
        } else if let Some(embed_idx) = ctx.embed_index {
            // Fall back to embed index for natural-language queries where name matching
            // finds nothing.
            match embed_idx.search_with_mode(query_text, None, top_k.min(50) * 3, search_mode).await {
                Ok(SearchOutcome::Results(results)) if !results.is_empty() => {
                    let node_index_map_for_entry = ctx.graph_state.node_index_map();
                    let code_results: Vec<_> = results.into_iter()
                        .filter(|r| r.kind.starts_with("code:"))
                        .filter(|r| search_result_passes_root_filter(r, &ctx.root_filter, &ctx.non_code_slugs))
                        .filter(|r| {
                            if let Some(ref sub) = params.subsystem {
                                ctx.graph_state.node_by_stable_id(&r.id, &node_index_map_for_entry)
                                    .and_then(|n| n.metadata.get(crate::server::SUBSYSTEM_KEY))
                                    .map(|s| subsystem_matches(s, sub))
                                    .unwrap_or(false)
                            } else {
                                true
                            }
                        })
                        .take(top_k).collect();
                    if code_results.is_empty() { return format!("No code symbols matched query \"{}\". Try a different query or use node parameter.", query_text); }
                    let mut header = format!("### Matched entry nodes for \"{}\"\n\n", query_text);
                    let strip = ctx.root_filter.as_deref();
                    let ids: Vec<String> = code_results.iter().map(|r| { let display = strip_root_prefix(&r.id, strip); header.push_str(&format!("- `{}` -- {} (score: {:.2})\n", display, r.title, r.score)); r.id.clone() }).collect();
                    header.push('\n');
                    (ids, header)
                }
                Ok(SearchOutcome::NotReady) => return "Embedding index: building -- semantic graph queries will work shortly. Use node parameter instead, or retry in a few seconds.".to_string(),
                Ok(_) => return format!("No code symbols matched query \"{}\". Try a different query or use node parameter.", query_text),
                Err(e) => return format!("Semantic search failed: {}. Use node parameter instead.", e),
            }
        } else {
            return "No matching graph nodes found and embedding index not available. Use node parameter instead.".to_string();
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

    // Collect grouped results across all entry nodes.
    // Deduplication is per-edge-kind: the same node may legitimately appear
    // under multiple relationship kinds, so we only deduplicate within a kind.
    use crate::server::handlers::run_traversal_grouped;
    let mut merged_groups: std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>> = std::collections::BTreeMap::new();
    // Per-kind seen sets for O(1) membership checks (avoids O(N²) Vec.contains in hot path).
    let mut merged_seen: std::collections::BTreeMap<crate::graph::EdgeKind, HashSet<String>> = std::collections::BTreeMap::new();
    let entry_set: HashSet<&str> = valid_entry_ids.iter().map(|s| s.as_str()).collect();

    // depth > 1 in neighbors mode: iterative BFS walking N levels deep.
    // Each level uses the previous level's results as the new frontier.
    // Nodes seen at earlier levels are not revisited (dedup across levels).
    let traversal_depth = if mode == "neighbors" { params.depth.unwrap_or(1).max(1) } else { 1 };

    if traversal_depth > 1 {
        // BFS: track visited nodes to avoid revisiting across levels.
        // Entry nodes are seeded into visited so they don't appear in results.
        let mut visited: HashSet<String> = valid_entry_ids.iter().map(|s| (*s).clone()).collect();
        let mut frontier: Vec<String> = valid_entry_ids.iter().map(|s| (*s).clone()).collect();

        for _ in 0..traversal_depth {
            if frontier.is_empty() { break; }
            let mut next_frontier: Vec<String> = Vec::new();
            for node_id in &frontier {
                match run_traversal_grouped(&gs.index, node_id, mode, Some(1), params.direction.as_deref(), edge_filter_slice) {
                    Ok(groups) => {
                        for (kind, ids) in groups {
                            let seen = merged_seen.entry(kind.clone()).or_default();
                            let entry = merged_groups.entry(kind).or_default();
                            for id in ids {
                                // visited: cross-level dedup; seen: intra-level O(1) per-kind dedup.
                                if !visited.contains(&id) && seen.insert(id.clone()) {
                                    entry.push(id.clone());
                                    next_frontier.push(id.clone());
                                }
                            }
                        }
                    }
                    Err(msg) => return msg,
                }
            }
            // Mark all newly-discovered nodes visited before next level.
            for id in &next_frontier {
                visited.insert(id.clone());
            }
            frontier = next_frontier;
        }
    } else {
        for node_id in &valid_entry_ids {
            match run_traversal_grouped(&gs.index, node_id, mode, params.hops, params.direction.as_deref(), edge_filter_slice) {
                Ok(groups) => {
                    for (kind, ids) in groups {
                        let seen = merged_seen.entry(kind.clone()).or_default();
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
    }

    // Build O(1) lookup map for stable_id -> node index.
    let node_index_map = gs.node_index_map();

    // Apply tests_for filtering
    if mode == "tests_for" {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| gs.node_by_stable_id(id, &node_index_map).map(ranking::is_test_file).unwrap_or(false));
        }
    }
    // Apply subsystem filter to traversal results (within-subsystem query).
    // When only `subsystem` is set, restrict neighbors to the same subsystem.
    if let Some(ref sub) = params.subsystem {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| {
                gs.node_by_stable_id(id, &node_index_map)
                    .and_then(|n| n.metadata.get(crate::server::SUBSYSTEM_KEY))
                    .map(|s| subsystem_matches(s, sub))
                    .unwrap_or(false)
            });
        }
    }
    // Apply target_subsystem filter (cross-subsystem query).
    // When set, keep only neighbors whose subsystem matches the target.
    // This enables queries like "what connects node X to the server subsystem?"
    if let Some(ref target_sub) = params.target_subsystem {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| {
                gs.node_by_stable_id(id, &node_index_map)
                    .and_then(|n| n.metadata.get(crate::server::SUBSYSTEM_KEY))
                    .map(|s| subsystem_matches(s, target_sub))
                    .unwrap_or(false)
            });
        }
    }
    // Remove empty groups after filtering
    merged_groups.retain(|_, ids| !ids.is_empty());

    // Count total displayable results
    let total_count: usize = merged_groups.values().map(|ids| {
        ids.iter().filter(|id| {
            gs.node_by_stable_id(id, &node_index_map)
                .map(|n| !crate::server::helpers::is_hidden_traversal_kind(&n.id.kind))
                .unwrap_or(true)
        }).count()
    }).sum();

    let strip = ctx.root_filter.as_deref();
    let entry_label = if valid_entry_ids.len() == 1 { format!("`{}`", strip_root_prefix(valid_entry_ids[0], strip)) } else { format!("{} entry nodes", valid_entry_ids.len()) };
    let direction = params.direction.as_deref().unwrap_or("outgoing");
    let freshness = format_freshness_full(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status, ctx.embed_status);

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
        // For large impact results (>100 unique nodes), show only the subsystem summary
        // instead of listing every node. This prevents 162K+ char responses that
        // overflow MCP response limits and are unreadable by agents.
        // Use unique node count (not per-bucket total_count) because the same node
        // can appear under multiple edge kinds in merged_groups.
        let unique_impact_count = if mode == "impact" {
            let mut seen: HashSet<&str> = HashSet::new();
            for ids in merged_groups.values() {
                for id in ids {
                    if let Some(node) = gs.node_by_stable_id(id, &node_index_map) {
                        if !crate::server::helpers::is_hidden_traversal_kind(&node.id.kind) {
                            seen.insert(id.as_str());
                        }
                    }
                }
            }
            seen.len()
        } else {
            0
        };
        let large_by_count = mode == "impact" && unique_impact_count > IMPACT_SUMMARY_NODE_THRESHOLD;

        // Helper: build the summary-only response for large impact results.
        let build_summary = |entry_header: &str, entry_label: &str, freshness: &str| -> String {
            let subsystem_breakdown = format_impact_subsystem_breakdown(&merged_groups, gs, &node_index_map, strip);
            let subsystem_count = count_affected_subsystems(&merged_groups, gs, &node_index_map);
            let heading = if subsystem_count == 0 {
                format!(
                    "## Impact of {}\n\n{} dependent(s) within {} hop(s) (result summarized — use `subsystem` filter to drill down)\n\n",
                    entry_label,
                    unique_impact_count,
                    params.hops.unwrap_or(3),
                )
            } else {
                format!(
                    "## Impact of {} ({} subsystems affected)\n\n{} dependent(s) within {} hop(s)\n{}\n",
                    entry_label,
                    subsystem_count,
                    unique_impact_count,
                    params.hops.unwrap_or(3),
                    subsystem_breakdown,
                )
            };
            format!("{}{}{}", entry_header, heading, freshness)
        };

        if large_by_count {
            // Node count alone exceeds threshold — skip rendering the full list.
            build_summary(&entry_header, &entry_label, &freshness)
        } else {
            let heading = match mode {
                "neighbors" => format!("## Graph neighbors ({}) of {}\n\n{} result(s)\n\n", direction, entry_label, total_count),
                "impact" => format!("## Impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n", entry_label, total_count, params.hops.unwrap_or(3)),
                "reachable" => format!("## Reachable from {}\n\n{} node(s) within {} hop(s)\n\n", entry_label, total_count, params.hops.unwrap_or(3)),
                "tests_for" => format!("## Test coverage for {}\n\n{} test function(s)\n\n", entry_label, total_count),
                _ => String::new(),
            };

            let md = format_neighbors_grouped_with_root(&gs.nodes, &merged_groups, &gs.index, params.compact, strip);

            // For impact mode, append a subsystem breakdown showing which subsystems
            // are affected and through which interface function the impact propagates.
            let subsystem_section = if mode == "impact" {
                format_impact_subsystem_breakdown(&merged_groups, gs, &node_index_map, strip)
            } else {
                String::new()
            };

            let full_output = format!("{}{}{}{}{}", entry_header, heading, md, subsystem_section, freshness);

            // Safety net: if the rendered output exceeds the character threshold,
            // retroactively switch to the summary view. This catches cases where
            // a moderate number of nodes (below the node threshold) still produce
            // enormous output due to verbose non-compact rendering.
            if mode == "impact" && full_output.len() > IMPACT_SUMMARY_CHAR_THRESHOLD {
                build_summary(&entry_header, &entry_label, &freshness)
            } else {
                full_output
            }
        }
    }
}

/// Group impact results by subsystem metadata and format as a summary section.
///
/// For each affected subsystem, reports the symbol count and the first node in
/// that subsystem (the "entry point" through which impact propagates).
fn format_impact_subsystem_breakdown(
    merged_groups: &std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>>,
    gs: &GraphState,
    node_index_map: &std::collections::HashMap<String, usize>,
    strip: Option<&str>,
) -> String {
    // Collect all unique result node IDs across edge-kind groups, deduplicated.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut subsystem_nodes: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for ids in merged_groups.values() {
        for id in ids {
            if !seen.insert(id.clone()) {
                continue; // Skip duplicates across edge-kind buckets
            }
            if let Some(node) = gs.node_by_stable_id(id, node_index_map) {
                if crate::server::helpers::is_hidden_traversal_kind(&node.id.kind) {
                    continue;
                }
                if let Some(sub) = node.metadata.get(crate::server::SUBSYSTEM_KEY) {
                    subsystem_nodes
                        .entry(sub.clone())
                        .or_default()
                        .push(id.clone());
                }
            }
        }
    }

    if subsystem_nodes.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    for (subsystem, ids) in &subsystem_nodes {
        // The first node in this subsystem is the interface through which impact enters
        let entry_point = ids
            .first()
            .and_then(|id| gs.node_by_stable_id(id, node_index_map))
            .map(|n| {
                let display = strip_root_prefix(&n.stable_id(), strip);
                format!(", entry point: `{}`", display)
            })
            .unwrap_or_default();
        lines.push(format!(
            "- **{}** ({} symbol(s){})",
            subsystem,
            ids.len(),
            entry_point
        ));
    }

    format!(
        "\n\n### Affected subsystems\n\n{}\n",
        lines.join("\n")
    )
}

/// Count the number of distinct subsystems affected by impact results.
fn count_affected_subsystems(
    merged_groups: &std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>>,
    gs: &GraphState,
    node_index_map: &std::collections::HashMap<String, usize>,
) -> usize {
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut subsystems: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for ids in merged_groups.values() {
        for id in ids {
            if !seen_ids.insert(id.as_str()) {
                continue;
            }
            if let Some(node) = gs.node_by_stable_id(id, node_index_map) {
                if crate::server::helpers::is_hidden_traversal_kind(&node.id.kind) {
                    continue;
                }
                if let Some(sub) = node.metadata.get(crate::server::SUBSYSTEM_KEY) {
                    subsystems.insert(sub.as_str());
                }
            }
        }
    }
    subsystems.len()
}

fn search_batch(node_ids: &[&str], params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    use crate::server::handlers::run_traversal_grouped;
    let gs = ctx.graph_state;
    let freshness = format_freshness_full(gs.nodes.len(), gs.last_scan_completed_at, ctx.lsp_status, ctx.embed_status);
    // Build O(1) lookup map and root slugs once for the entire batch.
    let node_index_map = gs.node_index_map();
    let roots = GraphState::root_slugs_from_index_map(&node_index_map);
    if params.mode.is_some() {
        let mode = params.mode.as_deref().unwrap_or("neighbors");
        let edge_filter = params.edge_types.as_ref().map(|types| types.iter().filter_map(|t| parse_edge_kind(t)).collect::<Vec<_>>());
        let edge_filter_slice = edge_filter.as_deref();
        let mut sections: Vec<String> = Vec::new();
        let strip = ctx.root_filter.as_deref();
        for &nid in node_ids {
            // Resolve short IDs (without root prefix) to full stable IDs.
            let resolved_nid = GraphState::resolve_node_id_fast(nid, &node_index_map, &roots);
            let display_nid = strip_root_prefix(&resolved_nid, strip);
            if gs.index.get_node(&resolved_nid).is_none() { sections.push(format!("### `{}`\n\nNode not found in graph.", display_nid)); continue; }
            match run_traversal_grouped(&gs.index, &resolved_nid, mode, params.hops, params.direction.as_deref(), edge_filter_slice) {
                Ok(mut groups) => {
                    // Remove self-references
                    for ids in groups.values_mut() {
                        ids.retain(|id| id != resolved_nid.as_str());
                    }
                    if mode == "tests_for" {
                        for ids in groups.values_mut() {
                            ids.retain(|id| gs.node_by_stable_id(id, &node_index_map).map(ranking::is_test_file).unwrap_or(false));
                        }
                    }
                    groups.retain(|_, ids| !ids.is_empty());
                    let total: usize = groups.values().map(|ids| {
                        ids.iter().filter(|id| {
                            gs.node_by_stable_id(id, &node_index_map)
                                .map(|n| !crate::server::helpers::is_hidden_traversal_kind(&n.id.kind))
                                .unwrap_or(true)
                        }).count()
                    }).sum();
                    if total == 0 { sections.push(format!("### `{}`\n\nNo {} results.", display_nid, mode)); }
                    else { let md = format_neighbors_grouped_with_root(&gs.nodes, &groups, &gs.index, params.compact, strip); sections.push(format!("### `{}`\n\n{} result(s)\n\n{}", display_nid, total, md)); }
                }
                Err(msg) => sections.push(format!("### `{}`\n\n{}", display_nid, msg)),
            }
        }
        format!("## Batch {} for {} node(s)\n\n{}{}", mode, node_ids.len(), sections.join("\n\n"), freshness)
    } else {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for &nid in node_ids {
            let resolved = GraphState::resolve_node_id_fast(nid, &node_index_map, &roots);
            if let Some(node) = gs.node_by_stable_id(&resolved, &node_index_map) { found.push(node); } else { missing.push(nid); }
        }
        let strip = ctx.root_filter.as_deref();
        if found.is_empty() { return format!("No nodes found for {}. Try search to find valid node IDs.{}", node_ids.iter().map(|id| format!("`{}`", strip_root_prefix(id, strip))).collect::<Vec<_>>().join(", "), freshness); }
        let md: String = found.iter().map(|n| format_node_entry_with_root(n, &gs.index, params.compact, strip)).collect::<Vec<_>>().join("\n\n");
        let mut result = format!("## Batch retrieve: {} found\n\n{}", found.len(), md);
        if !missing.is_empty() { result.push_str(&format!("\n\n**Missing:** {}", missing.iter().map(|id| format!("`{}`", strip_root_prefix(id, strip))).collect::<Vec<_>>().join(", "))); }
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
    // Resolve short IDs (without root prefix) to full stable IDs.
    let resolved_node = graph_state.resolve_node_id(&params.node);
    let result_ids = run_traversal(&graph_state.index, &resolved_node, &params.mode, params.max_hops.map(|h| h as u32), Some(params.direction.as_str()), edge_filter_slice)?;
    if result_ids.is_empty() { return Ok(format!("No results for `{}` ({}).", params.node, params.mode)); }
    let node_index_map = graph_state.node_index_map();
    let mut lines = vec![format!("## {} `{}`\n\n{} result(s)\n", params.mode, params.node, result_ids.len())];
    for id in &result_ids {
        if let Some(node) = graph_state.node_by_stable_id(id, &node_index_map) {
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

/// Parse a path/name query like `"auth/handlers/validate"` into
/// `Some(("auth/handlers", "validate"))`. Returns `None` if the query
/// contains no `/` — plain queries must be handled by normal name matching.
///
/// Splits at the **last** `/` so that deep paths like `"src/auth/handlers/validate"`
/// produce `path_part = "src/auth/handlers"` and `name_part = "validate"`.
fn parse_path_name_query(query: &str) -> Option<(&str, &str)> {
    let slash_pos = query.rfind('/')?;
    let path_part = &query[..slash_pos];
    let name_part = &query[slash_pos + 1..];
    // Reject degenerate splits (empty name or empty path) — fall back to
    // plain matching.
    if path_part.is_empty() || name_part.is_empty() {
        return None;
    }
    Some((path_part, name_part))
}

/// Resolve traversal entry points by exact name/signature matching against graph nodes.
///
/// Applies kind, language, file, and root filters. Returns matching nodes sorted
/// by name-match quality (exact > contains). This is used as the primary entry
/// point resolution strategy for traversal queries (#290), with the embed index
/// as a fallback for natural-language queries where name matching finds nothing.
///
/// When `query` contains `/`, the query is parsed as `path_part/name_part` and
/// both the file path and name are filtered simultaneously. Plain queries (no `/`)
/// behave identically to today.
fn resolve_entry_points_by_name<'a>(
    query: &str,
    limit: usize,
    params: &SearchParams,
    ctx: &SearchContext<'a>,
) -> Vec<&'a Node> {
    let gs = ctx.graph_state;

    // Detect path/name split query (e.g. "auth/handlers/validate").
    let path_name = parse_path_name_query(query);
    let (query_lower, path_filter_lower, name_filter_lower): (String, Option<String>, Option<String>) =
        if let Some((path_part, name_part)) = path_name {
            (
                query.to_lowercase(),
                Some(path_part.to_lowercase()),
                Some(name_part.to_lowercase()),
            )
        } else {
            (query.to_lowercase(), None, None)
        };

    let mut matches: Vec<&Node> = gs.nodes.iter().filter(|n| {
        // Name/file matching: path/name split vs. plain.
        if let (Some(pf), Some(nf)) = (&path_filter_lower, &name_filter_lower) {
            // Both file path and name must match.
            let file_match = n.id.file.to_string_lossy().to_lowercase().contains(pf.as_str());
            let name_match = n.id.name.to_lowercase().contains(nf.as_str());
            if !file_match || !name_match { return false; }
        } else {
            // Plain name or signature match.
            let name_match = n.id.name.to_lowercase().contains(&query_lower)
                || n.signature.to_lowercase().contains(&query_lower);
            if !name_match { return false; }
        }

        // Apply filters (kind, language, file, root).
        if let Some(ref kf) = params.kind {
            if n.id.kind.to_string().to_lowercase() != kf.to_lowercase() { return false; }
        }
        if let Some(ref lf) = params.language {
            if n.language.to_lowercase() != lf.to_lowercase() { return false; }
        }
        if let Some(ref ff) = params.file {
            if !n.id.file.to_string_lossy().contains(ff.as_str()) { return false; }
        }
        if !node_passes_root_filter(&n.id.root, &ctx.root_filter, &ctx.non_code_slugs) {
            return false;
        }
        if let Some(ref sub) = params.subsystem {
            let node_sub = n.metadata.get(crate::server::SUBSYSTEM_KEY).map(|s| s.as_str()).unwrap_or("");
            if !subsystem_matches(node_sub, sub) { return false; }
        }
        true
    }).collect();

    // Sort: exact name match first, then contains.
    // For path/name queries use the name part for exact-match comparison.
    let effective_query = name_filter_lower.as_deref().unwrap_or(&query_lower);
    matches.sort_by(|a, b| {
        let a_exact = a.id.name.to_lowercase() == effective_query
            || a.id.name.eq_ignore_ascii_case(query)
            || a.signature.eq_ignore_ascii_case(query);
        let b_exact = b.id.name.to_lowercase() == effective_query
            || b.id.name.eq_ignore_ascii_case(query)
            || b.signature.eq_ignore_ascii_case(query);
        b_exact.cmp(&a_exact)
    });

    matches.truncate(limit);
    matches
}

/// Match a node's subsystem metadata against a filter value.
///
/// Supports hierarchical matching: `subsystem="extract"` matches nodes whose
/// subsystem is exactly "extract" (case-insensitive) OR starts with "extract/"
/// (i.e., any child sub-module). `subsystem="extract/enrich"` matches only
/// nodes in that specific sub-module.
fn subsystem_matches(node_subsystem: &str, filter: &str) -> bool {
    if node_subsystem.eq_ignore_ascii_case(filter) {
        return true;
    }
    // Parent-level match: filter="extract" should match node_subsystem="extract/Node".
    // Check without allocating: node_subsystem must start with filter + "/" (case-insensitive).
    if node_subsystem.len() > filter.len() {
        let (head, tail) = node_subsystem.split_at(filter.len());
        return head.eq_ignore_ascii_case(filter) && tail.starts_with('/');
    }
    false
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

    fn make_graph_state_with_edges(nodes: Vec<Node>, edges: Vec<crate::graph::Edge>) -> GraphState {
        let mut index = GraphIndex::new();
        index.rebuild_from_edges(&edges);
        for node in &nodes {
            index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }
        GraphState { nodes, edges, index, last_scan_completed_at: None }
    }

    fn make_edge(from: &Node, to: &Node, kind: crate::graph::EdgeKind) -> crate::graph::Edge {
        crate::graph::Edge {
            from: from.id.clone(),
            to: to.id.clone(),
            kind,
            source: ExtractionSource::TreeSitter,
            confidence: crate::graph::Confidence::Detected,
        }
    }

    fn make_search_context<'a>(graph_state: &'a GraphState, repo_root: &'a Path) -> SearchContext<'a> {
        SearchContext {
            graph_state, embed_index: None, repo_root,
            lsp_status: None, embed_status: None, root_filter: None, non_code_slugs: HashSet::new(),
        }
    }

    #[test] fn test_search_params_default() { let p = SearchParams::default(); assert!(p.query.is_none()); assert!(!p.compact); assert!(!p.rerank); }
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
            lsp_status: None, embed_status: None, root_filter: Some("my-project".into()),
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

    // ── Adversarial: rerank parameter ──────────────────────────────────

    /// Rerank=true with only one match: the reranking block requires
    /// `matches.len() > 1`, so with a single match the reranker is not
    /// invoked, keeping this test hermetic (no model download in CI).
    /// This validates the over-fetch logic and parameter plumbing.
    #[tokio::test]
    async fn test_flat_search_rerank_true_no_embed() {
        let nodes = vec![
            make_node("auth_handler", NodeKind::Function, "src/auth.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            rerank: true,
            ..Default::default()
        };

        // Without embed index, falls back to name matching.
        // Single match means reranking block is skipped (len() > 1 guard),
        // so no model loading occurs.
        let results = flat_code_symbol_search(
            "auth", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;
        assert!(!results.is_empty(), "Rerank=true should not prevent results from appearing");
    }

    /// Rerank=false should not trigger any reranking code path.
    #[tokio::test]
    async fn test_flat_search_rerank_false_default() {
        let nodes = vec![make_node("foo", NodeKind::Function, "src/lib.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("foo".into()),
            rerank: false,
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "foo", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "foo");
    }

    // ── Empty query guard tests (#213) ──────────────────────────────

    /// Empty query with file filter should be allowed (not rejected as "Empty query").
    #[tokio::test]
    async fn test_search_empty_query_with_file_filter() {
        let nodes = vec![
            make_node("parse", NodeKind::Function, "src/parser.rs"),
            make_node("connect", NodeKind::Function, "src/db.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            file: Some("parser".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(!result.contains("Empty query"), "File filter should bypass empty query guard");
        assert!(result.contains("parse"), "Should find symbols in the filtered file");
    }

    /// Empty query with synthetic filter should be allowed.
    #[tokio::test]
    async fn test_search_empty_query_with_synthetic_filter() {
        let mut synth = make_node("MAGIC", NodeKind::Const, "src/lib.rs");
        synth.metadata.insert("synthetic".into(), "true".into());
        let real = make_node("real_fn", NodeKind::Function, "src/lib.rs");
        let gs = make_graph_state(vec![synth, real]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            synthetic: Some(false),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(!result.contains("Empty query"), "Synthetic filter should bypass empty query guard");
        assert!(result.contains("real_fn"), "Should include non-synthetic symbol when synthetic=false");
        assert!(!result.contains("MAGIC"), "Should exclude synthetic symbol when synthetic=false");
    }

    // ── repo_map root prefix tests (#270) ───────────────────────────

    /// In single-root mode, repo_map output should not contain root slug brackets.
    #[test]
    fn test_repo_map_single_root_no_prefix() {
        let long_slug = "users-muness1-src-open-horizon-labs-repo-native-alignment";
        let mut node = make_node("important_fn", NodeKind::Function, "src/main.rs");
        node.id.root = long_slug.to_string();
        node.metadata.insert("importance".into(), "0.5".into());
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = RepoMapContext {
            graph_state: &gs, repo_root: &repo_root,
            lsp_status: None, embed_status: None,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(long_slug.to_string()),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(!result.contains(&format!("[{}]", long_slug)),
            "Single-root mode should not show root slug prefix: {}", result);
        assert!(result.contains("important_fn"), "Should still show the symbol name");
    }

    /// In multi-root mode (root_filter=None), repo_map shows root slugs.
    #[test]
    fn test_repo_map_multi_root_shows_prefix() {
        let mut node = make_node("main", NodeKind::Function, "src/main.rs");
        node.id.root = "project-a".to_string();
        node.metadata.insert("importance".into(), "0.5".into());
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = RepoMapContext {
            graph_state: &gs, repo_root: &repo_root,
            lsp_status: None, embed_status: None,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: None,
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(result.contains("[project-a]"),
            "Multi-root mode should show root slug prefix: {}", result);
    }

    // ── resolve_entry_points_by_name tests (#290) ─────────────────────

    /// Name matching finds a struct by exact name.
    #[test]
    fn test_resolve_entry_points_by_name_exact_match() {
        let nodes = vec![
            make_node("SearchParams", NodeKind::Struct, "src/service.rs"),
            make_node("search_handler", NodeKind::Function, "src/handler.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams { kind: Some("struct".into()), ..Default::default() };

        let results = resolve_entry_points_by_name("SearchParams", 10, &params, &ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "SearchParams");
    }

    /// Name matching returns empty for unrelated query.
    #[test]
    fn test_resolve_entry_points_by_name_no_match() {
        let nodes = vec![make_node("Config", NodeKind::Struct, "src/config.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("nonexistent", 10, &params, &ctx);
        assert!(results.is_empty());
    }

    /// Kind filter is applied during name matching.
    #[test]
    fn test_resolve_entry_points_by_name_kind_filter() {
        let nodes = vec![
            make_node("Config", NodeKind::Struct, "src/config.rs"),
            make_node("config_init", NodeKind::Function, "src/config.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams { kind: Some("function".into()), ..Default::default() };

        let results = resolve_entry_points_by_name("config", 10, &params, &ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "config_init");
    }

    /// Exact name matches sort before substring matches.
    #[test]
    fn test_resolve_entry_points_by_name_exact_first() {
        let nodes = vec![
            make_node("search_handler", NodeKind::Function, "src/handler.rs"),
            make_node("search", NodeKind::Function, "src/search.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("search", 10, &params, &ctx);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.name, "search", "exact match should come first");
    }

    /// Exact signature match sorts before substring-only matches with limit=1.
    #[test]
    fn test_resolve_entry_points_exact_signature_first() {
        let mut node_a = make_node("foo", NodeKind::Function, "src/a.rs");
        node_a.signature = "fn foo(config: &SearchParams)".to_string();
        let mut node_b = make_node("bar", NodeKind::Function, "src/b.rs");
        node_b.signature = "fn bar()".to_string();
        let nodes = vec![node_a, node_b];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        // Query matches node_a by signature ("fn foo(config: &SearchParams)")
        // but not node_b. With limit=1, node_a must survive.
        let results = resolve_entry_points_by_name("fn foo(config: &SearchParams)", 1, &params, &ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "foo", "exact signature match should be kept with limit=1");
    }

    // ── parse_path_name_query tests ────────────────────────────────────

    #[test]
    fn test_parse_path_name_query_basic() {
        let result = parse_path_name_query("auth/handlers/validate");
        assert_eq!(result, Some(("auth/handlers", "validate")));
    }

    #[test]
    fn test_parse_path_name_query_single_slash() {
        let result = parse_path_name_query("src/validate");
        assert_eq!(result, Some(("src", "validate")));
    }

    #[test]
    fn test_parse_path_name_query_no_slash() {
        let result = parse_path_name_query("validate");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_path_name_query_trailing_slash() {
        // Empty name part — should return None (degenerate)
        let result = parse_path_name_query("auth/handlers/");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_path_name_query_leading_slash() {
        // Empty path part — should return None (degenerate)
        let result = parse_path_name_query("/validate");
        assert_eq!(result, None);
    }

    // ── Path/name split in resolve_entry_points_by_name ────────────────

    /// search("auth/handlers/validate") returns only `validate` in auth/handlers files.
    #[test]
    fn test_resolve_entry_points_path_name_basic() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
            make_node("parse", NodeKind::Function, "src/auth/handlers/parse.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("auth/handlers/validate", 10, &params, &ctx);
        assert_eq!(results.len(), 1, "Only auth/handlers validate should match");
        assert_eq!(results[0].id.name, "validate");
        assert!(results[0].id.file.to_string_lossy().contains("auth/handlers"));
    }

    /// Plain queries (no `/`) still work identically to today.
    #[test]
    fn test_resolve_entry_points_plain_query_unchanged() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("validate", 10, &params, &ctx);
        assert_eq!(results.len(), 2, "Plain query should return all matching nodes");
    }

    /// Path/name query with no matches returns empty.
    #[test]
    fn test_resolve_entry_points_path_name_no_match() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        // Path doesn't match: billing/validate vs src/auth/handlers/mod.rs
        let results = resolve_entry_points_by_name("billing/validate", 10, &params, &ctx);
        assert!(results.is_empty(), "No match when path doesn't fit");
    }

    // ── Path/name split in flat_code_symbol_search ─────────────────────

    /// flat search with path/name query returns only nodes where both file and name match.
    #[tokio::test]
    async fn test_flat_search_path_name_basic() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
            make_node("parse", NodeKind::Function, "src/auth/handlers/parse.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams { query: Some("auth/handlers/validate".into()), ..Default::default() };

        let results = flat_code_symbol_search(
            "auth/handlers/validate", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1, "Only auth/handlers validate should match");
        assert_eq!(results[0].id.name, "validate");
        assert!(results[0].id.file.to_string_lossy().contains("auth/handlers"));
    }

    /// Plain queries (no `/`) remain unchanged in flat search.
    #[tokio::test]
    async fn test_flat_search_plain_query_unchanged() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams { query: Some("validate".into()), ..Default::default() };

        let results = flat_code_symbol_search(
            "validate", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 2, "Plain query returns all matches");
    }

    // ── Adversarial path/name tests ────────────────────────────────────

    /// Adversarial: "//foo" — double slash degenerate case.
    /// rfind gives slash_pos=1, path_part="/", name_part="foo". Path part "/" is
    /// non-empty, so it parses. But every file path contains "/" (Unix paths), so
    /// this would match all nodes named "foo". This is an edge case worth asserting.
    #[test]
    fn test_parse_path_name_query_double_slash() {
        // "//foo" → slash_pos=1, path_part="/", name_part="foo"
        // "/" is non-empty, so Some(("/", "foo")) — this is intentional: every file
        // matches "/" as a path fragment. Document this by asserting the parsed result.
        let result = parse_path_name_query("//foo");
        assert_eq!(result, Some(("/", "foo")));
    }

    /// Adversarial: path/name fallback when path part matches nothing.
    /// When path filter eliminates all candidates, result should be empty (no
    /// silent fallback to plain name matching in resolve_entry_points_by_name).
    #[test]
    fn test_resolve_entry_points_path_name_empty_on_path_mismatch() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
            make_node("validate", NodeKind::Function, "src/payments/validate.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        // "auth/handlers/validate" → path="auth/handlers", name="validate"
        // Neither node is in auth/handlers → should return empty, not fall back to plain
        let results = resolve_entry_points_by_name("auth/handlers/validate", 10, &params, &ctx);
        assert!(results.is_empty(), "Must not fall back to plain name matching");
    }

    /// Adversarial: path/name where name part partially matches many nodes.
    /// Verify path discriminates correctly.
    #[tokio::test]
    async fn test_flat_search_path_name_path_discriminates() {
        let nodes = vec![
            make_node("new", NodeKind::Function, "src/auth/handlers.rs"),
            make_node("new", NodeKind::Function, "src/billing/invoice.rs"),
            make_node("new", NodeKind::Function, "src/payments/gateway.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = flat_code_symbol_search(
            "auth/new", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1, "Path filter should discriminate to only auth node");
        assert!(results[0].id.file.to_string_lossy().contains("auth"));
    }

    // ── Subsystem filter tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_flat_search_subsystem_filter() {
        let mut node_a = make_node("scan_files", NodeKind::Function, "src/scanner.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "scanner".to_string());
        let mut node_b = make_node("scan_config", NodeKind::Function, "src/config.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "config".to_string());
        let node_c = make_node("scan_other", NodeKind::Function, "src/other.rs");
        // node_c has no subsystem metadata

        let gs = make_graph_state(vec![node_a, node_b, node_c]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("scan".into()),
            subsystem: Some("scanner".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "scan", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1, "Only scanner-subsystem node should match");
        assert_eq!(results[0].id.name, "scan_files");
    }

    #[tokio::test]
    async fn test_flat_search_subsystem_filter_case_insensitive() {
        let mut node = make_node("handler", NodeKind::Function, "src/server.rs");
        node.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "Server".to_string());

        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("handler".into()),
            subsystem: Some("server".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false,
        ).await;

        assert_eq!(results.len(), 1, "Case-insensitive subsystem match should work");
    }

    #[tokio::test]
    async fn test_flat_search_subsystem_allows_empty_query_browse() {
        let mut node = make_node("extract", NodeKind::Function, "src/extract.rs");
        node.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "extractor".to_string());
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            subsystem: Some("extractor".into()),
            ..Default::default()
        };

        // Empty query with subsystem filter should be allowed (not rejected)
        let result = search(&params, &ctx).await;
        assert!(!result.contains("Empty query"), "Subsystem filter should act as browse filter");
    }

    #[test]
    fn test_subsystem_matches_exact() {
        assert!(super::subsystem_matches("extract", "extract"));
        assert!(super::subsystem_matches("Extract", "extract"));
        assert!(super::subsystem_matches("extract", "Extract"));
    }

    #[test]
    fn test_subsystem_matches_parent_prefix() {
        // Parent filter matches child sub-modules
        assert!(super::subsystem_matches("extract/Node", "extract"));
        assert!(super::subsystem_matches("extract/enrich", "extract"));
        assert!(super::subsystem_matches("Extract/Node", "extract"));
    }

    #[test]
    fn test_subsystem_matches_child_specific() {
        // Child-specific filter matches only that child
        assert!(super::subsystem_matches("extract/enrich", "extract/enrich"));
        assert!(!super::subsystem_matches("extract/Node", "extract/enrich"));
    }

    #[test]
    fn test_subsystem_matches_no_false_prefix() {
        // "extract" should NOT match "extraction" (not a `/`-separated prefix)
        assert!(!super::subsystem_matches("extraction", "extract"));
    }

    #[tokio::test]
    async fn test_flat_search_subsystem_parent_matches_children() {
        let mut node_a = make_node("enrich", NodeKind::Function, "src/extract/enrich.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "extract/enrich".to_string());
        let mut node_b = make_node("NodeId", NodeKind::Struct, "src/extract/mod.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "extract/Node".to_string());
        let mut node_c = make_node("embed_texts", NodeKind::Function, "src/embed.rs");
        node_c.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());
        let gs = make_graph_state(vec![node_a, node_b, node_c]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            subsystem: Some("extract".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // Both extract/enrich and extract/Node should match, but not embed
        assert!(result.contains("enrich"), "Should include extract/enrich child");
        assert!(result.contains("NodeId"), "Should include extract/Node child");
        assert!(!result.contains("embed_texts"), "Should NOT include embed subsystem");
    }

    // ── Cross-subsystem traversal tests ───────────────────────────────

    #[tokio::test]
    async fn test_traversal_target_subsystem_filters_neighbors() {
        use crate::graph::EdgeKind;

        // Create nodes in different subsystems
        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "server".to_string());
        let mut node_b = make_node("embed_text", NodeKind::Function, "src/embed.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());
        let mut node_c = make_node("scan_file", NodeKind::Function, "src/scanner.rs");
        node_c.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "scanner".to_string());

        // handler calls embed_text and scan_file
        let edge1 = make_edge(&node_a, &node_b, EdgeKind::Calls);
        let edge2 = make_edge(&node_a, &node_c, EdgeKind::Calls);

        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone(), node_c.clone()],
            vec![edge1, edge2],
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Query: neighbors of handler, filtered to embed subsystem only
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("embed".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(result.contains("embed_text"), "Should include embed neighbor");
        assert!(!result.contains("scan_file"), "Should NOT include scanner neighbor");
    }

    #[tokio::test]
    async fn test_traversal_target_subsystem_no_match_returns_empty() {
        use crate::graph::EdgeKind;

        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "server".to_string());
        let mut node_b = make_node("embed_text", NodeKind::Function, "src/embed.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());

        let edge = make_edge(&node_a, &node_b, EdgeKind::Calls);
        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone()],
            vec![edge],
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Query: neighbors of handler, filtered to nonexistent subsystem
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("nonexistent".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(result.contains("No outgoing neighbors"), "Should report no neighbors when target_subsystem matches nothing");
    }

    #[tokio::test]
    async fn test_traversal_subsystem_and_target_subsystem_combined() {
        use crate::graph::EdgeKind;

        // node_a (server) -> node_b (embed), node_c (scanner), node_d (server)
        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "server".to_string());
        let mut node_b = make_node("embed_text", NodeKind::Function, "src/embed.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());
        let mut node_c = make_node("scan_file", NodeKind::Function, "src/scanner.rs");
        node_c.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "scanner".to_string());
        let mut node_d = make_node("route", NodeKind::Function, "src/server/route.rs");
        node_d.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "server".to_string());

        let edges = vec![
            make_edge(&node_a, &node_b, EdgeKind::Calls),
            make_edge(&node_a, &node_c, EdgeKind::Calls),
            make_edge(&node_a, &node_d, EdgeKind::Calls),
        ];

        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone(), node_c.clone(), node_d.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Both subsystem (server) and target_subsystem (embed) set.
        // subsystem filters entry-point resolution (not relevant here since we use node ID).
        // target_subsystem filters the traversal results.
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("embed".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(result.contains("embed_text"), "Should include embed neighbor");
        assert!(!result.contains("scan_file"), "Should NOT include scanner neighbor");
        assert!(!result.contains("route"), "Should NOT include server neighbor");
    }

    #[tokio::test]
    async fn test_traversal_target_subsystem_hierarchical_match() {
        use crate::graph::EdgeKind;

        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "server".to_string());
        let mut node_b = make_node("enrich_node", NodeKind::Function, "src/extract/enrich.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "extract/enrich".to_string());
        let mut node_c = make_node("parse_node", NodeKind::Function, "src/extract/parse.rs");
        node_c.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "extract/parse".to_string());

        let edges = vec![
            make_edge(&node_a, &node_b, EdgeKind::Calls),
            make_edge(&node_a, &node_c, EdgeKind::Calls),
        ];

        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone(), node_c.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // target_subsystem="extract" should match both extract/enrich and extract/parse
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("extract".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(result.contains("enrich_node"), "Should include extract/enrich child");
        assert!(result.contains("parse_node"), "Should include extract/parse child");
    }

    #[test]
    fn test_format_impact_subsystem_breakdown_empty() {
        let groups = std::collections::BTreeMap::new();
        let gs = make_graph_state(vec![]);
        let node_index_map = gs.node_index_map();
        let result = format_impact_subsystem_breakdown(&groups, &gs, &node_index_map, None);
        assert!(result.is_empty(), "No subsystem data should produce empty string");
    }

    #[test]
    fn test_format_impact_subsystem_breakdown_groups_correctly() {
        use crate::graph::EdgeKind;
        let mut node_a = make_node("fn_a", NodeKind::Function, "src/alpha.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "alpha".to_string());
        let mut node_b = make_node("fn_b", NodeKind::Function, "src/beta.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let mut node_c = make_node("fn_c", NodeKind::Function, "src/beta.rs");
        node_c.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let gs = make_graph_state(vec![node_a.clone(), node_b.clone(), node_c.clone()]);
        let node_index_map = gs.node_index_map();

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(EdgeKind::Calls, vec![
            node_a.stable_id(),
            node_b.stable_id(),
            node_c.stable_id(),
        ]);

        let result = format_impact_subsystem_breakdown(&groups, &gs, &node_index_map, None);
        assert!(result.contains("alpha"), "Should contain alpha subsystem");
        assert!(result.contains("beta"), "Should contain beta subsystem");
        assert!(result.contains("2 symbol(s)"), "Beta should have 2 symbols");
        assert!(result.contains("1 symbol(s)"), "Alpha should have 1 symbol");
    }

    #[test]
    fn test_count_affected_subsystems() {
        use crate::graph::EdgeKind;
        let mut node_a = make_node("fn_a", NodeKind::Function, "src/alpha.rs");
        node_a.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "alpha".to_string());
        let mut node_b = make_node("fn_b", NodeKind::Function, "src/beta.rs");
        node_b.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let mut node_c = make_node("fn_c", NodeKind::Function, "src/beta.rs");
        node_c.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let gs = make_graph_state(vec![node_a.clone(), node_b.clone(), node_c.clone()]);
        let node_index_map = gs.node_index_map();

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(EdgeKind::Calls, vec![
            node_a.stable_id(),
            node_b.stable_id(),
            node_c.stable_id(),
        ]);

        assert_eq!(count_affected_subsystems(&groups, &gs, &node_index_map), 2);
    }

    #[test]
    fn test_count_affected_subsystems_empty() {
        let groups = std::collections::BTreeMap::new();
        let gs = make_graph_state(vec![]);
        let node_index_map = gs.node_index_map();
        assert_eq!(count_affected_subsystems(&groups, &gs, &node_index_map), 0);
    }

    #[test]
    fn test_impact_summary_thresholds_are_reasonable() {
        // Node threshold: low enough to catch moderate-count-but-verbose-output cases.
        // The old threshold of 100 was too high — 80 non-compact nodes produced 157K chars.
        assert!(IMPACT_SUMMARY_NODE_THRESHOLD >= 10, "Node threshold too low");
        assert!(IMPACT_SUMMARY_NODE_THRESHOLD <= 60, "Node threshold too high");
        // Character threshold: safety net for when node count is below the node threshold
        // but the rendered output is still too large.
        assert!(IMPACT_SUMMARY_CHAR_THRESHOLD >= 20_000, "Char threshold too low");
        assert!(IMPACT_SUMMARY_CHAR_THRESHOLD <= 100_000, "Char threshold too high");
    }

    /// Adversarial: verify large impact results produce summary, not full listing.
    /// Creates 150 nodes (across 3 subsystems) all calling one root node,
    /// then runs search(mode="impact") and verifies the output is compact.
    #[tokio::test]
    async fn test_large_impact_produces_subsystem_summary() {
        use crate::graph::EdgeKind;

        let root_node = make_node("RootType", NodeKind::Struct, "src/root.rs");
        let mut all_nodes = vec![root_node.clone()];
        let mut all_edges = Vec::new();

        // Impact traversal follows incoming Calls/ReferencedBy edges.
        // "fn_0 calls RootType" = edge from fn_0 to RootType = incoming edge on RootType.
        let subsystems = ["alpha", "beta", "gamma"];
        for i in 0..150 {
            let sub = subsystems[i % 3];
            let file = format!("src/{}/mod.rs", sub);
            let mut node = make_node(&format!("fn_{}", i), NodeKind::Function, &file);
            node.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), sub.to_string());
            all_edges.push(make_edge(&node, &root_node, EdgeKind::Calls));
            all_nodes.push(node);
        }

        let gs = make_graph_state_with_edges(all_nodes, all_edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // Should contain subsystem summary, not individual node listings
        assert!(result.contains("subsystems affected"), "Should show subsystem count in heading, got: {}", &result[..result.len().min(500)]);
        assert!(result.contains("alpha"), "Should list alpha subsystem");
        assert!(result.contains("beta"), "Should list beta subsystem");
        assert!(result.contains("gamma"), "Should list gamma subsystem");
        assert!(result.contains("50 symbol(s)"), "Each subsystem should have 50 nodes");

        // Should NOT contain full node listings (edge-kind grouped sections)
        assert!(!result.contains("#### Calls"), "Should NOT have edge-kind grouped sections in summary mode");

        // Output should be compact -- well under 10K chars for 150 nodes
        assert!(result.len() < 5000, "Summary should be compact, got {} chars", result.len());
    }

    /// Adversarial: verify small impact results still show full listing.
    #[tokio::test]
    async fn test_small_impact_preserves_full_listing() {
        use crate::graph::EdgeKind;

        let root_node = make_node("SmallRoot", NodeKind::Struct, "src/root.rs");
        let mut dep = make_node("one_dep", NodeKind::Function, "src/dep.rs");
        dep.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), "dep".to_string());
        let edge = make_edge(&dep, &root_node, EdgeKind::Calls);

        let gs = make_graph_state_with_edges(vec![root_node.clone(), dep.clone()], vec![edge]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // Should show full listing with edge-kind groups
        assert!(result.contains("Impact analysis for"), "Should use standard heading for small results, got: {}", &result[..result.len().min(500)]);
        assert!(result.contains("one_dep"), "Should list individual nodes");
        // Should also have subsystem breakdown appended
        assert!(result.contains("Affected subsystems"), "Should still have subsystem breakdown");
    }

    /// Adversarial: moderate node count (below node threshold) but verbose output
    /// that exceeds the character threshold should still trigger the summary view.
    /// This is the exact bug from #345 round 2: ~80 nodes producing 157K chars.
    #[tokio::test]
    async fn test_moderate_count_but_verbose_output_triggers_char_threshold() {
        use crate::graph::EdgeKind;

        let root_node = make_node("VerboseRoot", NodeKind::Struct, "src/root.rs");
        let mut all_nodes = vec![root_node.clone()];
        let mut all_edges = Vec::new();

        // Create 25 nodes (below node threshold of 30) but each with a very
        // long signature that inflates the non-compact output beyond 40K chars.
        let subsystems = ["verbose_a", "verbose_b"];
        for i in 0..25 {
            let sub = subsystems[i % 2];
            let file = format!("src/{}/mod.rs", sub);
            let mut node = make_node(&format!("verbose_fn_{}", i), NodeKind::Function, &file);
            // A 2000-char signature makes each node ~2KB+ in non-compact mode
            node.signature = format!("fn verbose_fn_{}({})", i, "x: SomeLongType, ".repeat(100));
            node.metadata.insert(crate::server::SUBSYSTEM_KEY.to_owned(), sub.to_string());
            all_edges.push(make_edge(&node, &root_node, EdgeKind::Calls));
            all_nodes.push(node);
        }

        let gs = make_graph_state_with_edges(all_nodes, all_edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // The char threshold should kick in and produce a summary
        assert!(
            result.contains("subsystems affected") || result.contains("result summarized"),
            "Should trigger summary via char threshold, got: {}",
            &result[..result.len().min(500)]
        );
        // Should be compact
        assert!(
            result.len() < IMPACT_SUMMARY_CHAR_THRESHOLD,
            "Summary should be well under char threshold, got {} chars",
            result.len()
        );
    }

    /// Adversarial: verify large impact with NO subsystem metadata handles gracefully.
    #[tokio::test]
    async fn test_large_impact_no_subsystem_metadata() {
        use crate::graph::EdgeKind;

        let root_node = make_node("OrphanRoot", NodeKind::Struct, "src/root.rs");
        let mut all_nodes = vec![root_node.clone()];
        let mut all_edges = Vec::new();

        // 150 nodes with NO subsystem metadata
        for i in 0..150 {
            let node = make_node(&format!("orphan_{}", i), NodeKind::Function, "src/orphan.rs");
            all_edges.push(make_edge(&node, &root_node, EdgeKind::Calls));
            all_nodes.push(node);
        }

        let gs = make_graph_state_with_edges(all_nodes, all_edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // Should fall back to count-only summary
        assert!(result.contains("150 dependent(s)"), "Should show total count, got: {}", &result[..result.len().min(500)]);
        assert!(result.contains("result summarized"), "Should indicate summarized output");
        assert!(result.contains("subsystem"), "Should hint to use subsystem filter");
        // Should NOT crash or produce empty output
        assert!(result.len() > 50, "Should produce meaningful output");
    }

    // ── Depth-aware traversal tests ────────────────────────────────────

    #[tokio::test]
    async fn test_depth_traversal_two_levels() {
        use crate::graph::EdgeKind;

        // Chain: module -> member -> sub_member
        let module = make_node("my_module", NodeKind::Module, "src/module.rs");
        let member = make_node("my_struct", NodeKind::Struct, "src/module.rs");
        let sub_member = make_node("my_field", NodeKind::Function, "src/module.rs");

        let edges = vec![
            make_edge(&module, &member, EdgeKind::Defines),
            make_edge(&member, &sub_member, EdgeKind::Defines),
        ];
        let gs = make_graph_state_with_edges(
            vec![module.clone(), member.clone(), sub_member.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // depth=2 should return both member and sub_member
        let params = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(2),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(result.contains("my_struct"), "depth=2 should include direct member");
        assert!(result.contains("my_field"), "depth=2 should include sub-member");
        // Entry node name appears in the header ("Graph neighbors of my_module") but should NOT
        // appear as a neighbor result (i.e., not as a backreference to itself in the result list).
        // Check that my_struct and my_field are present (they are the actual results).
        // We do NOT assert my_module is absent from the full output since it's in the section header.
    }

    #[tokio::test]
    async fn test_depth_one_same_as_default() {
        use crate::graph::EdgeKind;

        // Chain: module -> member -> sub_member
        let module = make_node("mod_a", NodeKind::Module, "src/mod_a.rs");
        let member = make_node("fn_b", NodeKind::Function, "src/mod_a.rs");
        let sub_member = make_node("fn_c", NodeKind::Function, "src/mod_a.rs");

        let edges = vec![
            make_edge(&module, &member, EdgeKind::Defines),
            make_edge(&member, &sub_member, EdgeKind::Defines),
        ];
        let gs = make_graph_state_with_edges(
            vec![module.clone(), member.clone(), sub_member.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // depth=1 should behave like default (no depth param)
        let params_depth1 = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(1),
            ..Default::default()
        };
        let params_default = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            ..Default::default()
        };
        let result_d1 = search(&params_depth1, &ctx).await;
        let result_default = search(&params_default, &ctx).await;

        // Both should contain fn_b but not fn_c (only 1 hop)
        assert!(result_d1.contains("fn_b"), "depth=1 should include direct member");
        assert!(!result_d1.contains("fn_c"), "depth=1 should NOT include sub-member");
        assert_eq!(result_d1, result_default, "depth=1 output should match default behavior");
    }

    #[tokio::test]
    async fn test_depth_traversal_deduplicates_across_levels() {
        use crate::graph::EdgeKind;

        // Diamond: module -> a -> c, module -> b -> c
        // c should appear only once even though both a and b point to it
        let module = make_node("diamond_mod", NodeKind::Module, "src/diamond.rs");
        let node_a = make_node("branch_a", NodeKind::Function, "src/diamond.rs");
        let node_b = make_node("branch_b", NodeKind::Function, "src/diamond.rs");
        let node_c = make_node("shared_leaf", NodeKind::Function, "src/diamond.rs");

        let edges = vec![
            make_edge(&module, &node_a, EdgeKind::Defines),
            make_edge(&module, &node_b, EdgeKind::Defines),
            make_edge(&node_a, &node_c, EdgeKind::Defines),
            make_edge(&node_b, &node_c, EdgeKind::Defines),
        ];
        let gs = make_graph_state_with_edges(
            vec![module.clone(), node_a.clone(), node_b.clone(), node_c.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(2),
            compact: true,
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // shared_leaf should appear as exactly one result entry.
        // Count stable ID occurrences (e.g., "local:src/diamond.rs:shared_leaf:function")
        // to avoid false positives from name appearing multiple times on one result line.
        let stable_id_occurrences = result.matches(":shared_leaf:").count();
        assert_eq!(stable_id_occurrences, 1, "shared_leaf stable ID should appear exactly once (dedup failed), got {} occurrences: {}", stable_id_occurrences, &result[..result.len().min(500)]);
        // branch_a and branch_b should both appear
        assert!(result.contains("branch_a"), "branch_a should be in results");
        assert!(result.contains("branch_b"), "branch_b should be in results");
    }

    #[tokio::test]
    async fn test_depth_batch_nodes_rejects_depth_greater_than_one() {
        use crate::graph::EdgeKind;

        // depth > 1 with nodes=[...] should return error message
        let node_a = make_node("fn_a", NodeKind::Function, "src/a.rs");
        let node_b = make_node("fn_b", NodeKind::Function, "src/b.rs");
        let edges = vec![make_edge(&node_a, &node_b, EdgeKind::Calls)];
        let gs = make_graph_state_with_edges(vec![node_a.clone(), node_b.clone()], edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            nodes: Some(vec![node_a.stable_id()]),
            mode: Some("neighbors".into()),
            depth: Some(2),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(result.contains("depth > 1 is not supported"), "Should return error for nodes+depth>1: {}", result);
    }

    // ── Adversarial depth traversal tests ────────────────────────────

    #[tokio::test]
    async fn test_depth_cyclic_graph_does_not_loop() {
        use crate::graph::EdgeKind;

        // Cycle: A -> B -> A (back-edge)
        // depth=3 should not loop infinitely; visited set must break cycle.
        let node_a = make_node("cycle_a", NodeKind::Module, "src/cycle.rs");
        let node_b = make_node("cycle_b", NodeKind::Function, "src/cycle.rs");

        let edges = vec![
            make_edge(&node_a, &node_b, EdgeKind::Calls),
            make_edge(&node_b, &node_a, EdgeKind::Calls), // back-edge creating cycle
        ];
        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // depth=3 should terminate (visited set breaks cycle after level 1)
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(3),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // cycle_b should appear (level 1); cycle_a should NOT re-appear (it's in visited)
        assert!(result.contains("cycle_b"), "cycle_b should appear in results");
        // Result should be finite and not crash
        assert!(result.len() < 100_000, "Output should be bounded even with cycles");
    }

    #[tokio::test]
    async fn test_depth_with_non_neighbors_mode_uses_hops() {
        use crate::graph::EdgeKind;

        // depth should be silently ignored for impact mode (uses hops instead)
        let node_a = make_node("caller_fn", NodeKind::Function, "src/a.rs");
        let node_b = make_node("callee_fn", NodeKind::Function, "src/b.rs");
        let edges = vec![make_edge(&node_a, &node_b, EdgeKind::Calls)];
        let gs = make_graph_state_with_edges(vec![node_a.clone(), node_b.clone()], edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // impact mode with depth=2 — depth should be ignored, hops controls behavior
        let params = SearchParams {
            node: Some(node_b.stable_id()),
            mode: Some("impact".into()),
            depth: Some(2),  // Should be ignored for impact mode
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // Should still find the impact (caller_fn) — just verifying no crash/silent error
        assert!(!result.is_empty(), "impact mode with depth param should still produce output");
        assert!(result.contains("Impact analysis"), "should be impact analysis output");
    }

    #[tokio::test]
    async fn test_depth_with_edge_type_filter_limits_each_level() {
        use crate::graph::EdgeKind;

        // node_mod -[Defines]-> fn_a -[Calls]-> fn_b
        // With edge_types=["defines"] and depth=2, fn_b should NOT appear
        // because the Calls edge at level 2 is filtered out.
        let node_mod = make_node("filtered_mod", NodeKind::Module, "src/filt.rs");
        let fn_a = make_node("filtered_fn_a", NodeKind::Function, "src/filt.rs");
        let fn_b = make_node("filtered_fn_b", NodeKind::Function, "src/filt.rs");

        let edges = vec![
            make_edge(&node_mod, &fn_a, EdgeKind::Defines),
            make_edge(&fn_a, &fn_b, EdgeKind::Calls), // not Defines
        ];
        let gs = make_graph_state_with_edges(
            vec![node_mod.clone(), fn_a.clone(), fn_b.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(node_mod.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(2),
            edge_types: Some(vec!["defines".to_string()]),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // fn_a should appear (Defines edge at level 1)
        assert!(result.contains("filtered_fn_a"), "fn_a should appear (Defines edge at level 1)");
        // fn_b should NOT appear (Calls edge at level 2 is filtered by edge_types=["defines"])
        assert!(!result.contains("filtered_fn_b"), "fn_b should NOT appear (Calls edge is filtered)");
    }

    // ── list_roots_from_slugs tests ─────────────────────────────────────────

    /// When active_slugs is empty, list_roots_from_slugs falls back to
    /// config-only discovery (same behaviour as the old list_roots).
    #[test]
    fn test_list_roots_from_slugs_empty_falls_back_to_config() {
        let repo = std::env::current_dir().unwrap();
        let result = list_roots_from_slugs(&repo, &std::collections::HashSet::new(), None, None);
        // The primary root always exists (current dir is the RNA repo).
        assert!(result.contains("## Workspace Roots"), "should produce a roots header");
        assert!(result.contains("root(s)"), "should report root count");
    }

    /// When active_slugs contains the primary slug, only that root is shown
    /// and the config-only fallback is NOT triggered.
    #[test]
    fn test_list_roots_from_slugs_filters_to_graph_slugs() {
        let repo = std::env::current_dir().unwrap();
        // Build the workspace to find the real primary slug.
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; } // can't test without at least one root

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        assert!(result.contains("## Workspace Roots"), "should produce header");
        assert!(result.contains("1 root(s)"), "should show exactly 1 root when only primary slug is active");
        assert!(result.contains(&primary_slug), "should contain primary slug");
    }

    /// Orphaned slugs (present in graph but not in config) get a placeholder line.
    #[test]
    fn test_list_roots_from_slugs_orphaned_slug_placeholder() {
        let repo = std::env::current_dir().unwrap();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert("ghost-root".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        assert!(result.contains("ghost-root"), "orphaned slug should appear");
        assert!(result.contains("path unknown"), "orphaned slug should have placeholder text");
    }

    /// An empty-string slug (stale LanceDB artifact from a pruned worktree) is excluded.
    #[test]
    fn test_list_roots_from_slugs_excludes_empty_slug() {
        let repo = std::env::current_dir().unwrap();
        let mut active_slugs = std::collections::HashSet::new();
        // Simulate a ghost entry: empty slug from a pruned worktree
        active_slugs.insert("".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        // Empty slug must never appear as a root line "- ****: (path unknown ...)"
        assert!(!result.contains("- ****: (path unknown"), "empty slug should be excluded from output");
    }

    /// Empty slug mixed with a real orphaned slug — only the real one appears.
    #[test]
    fn test_list_roots_from_slugs_empty_slug_mixed_with_real_orphan() {
        let repo = std::env::current_dir().unwrap();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert("".to_string());
        active_slugs.insert("real-orphan-zzz".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        assert!(!result.contains("- ****: (path unknown"), "empty slug should be excluded");
        assert!(result.contains("real-orphan-zzz"), "real orphan slug should still appear");
    }

    /// The "external" pseudo-slug is excluded from the output.
    #[test]
    fn test_list_roots_from_slugs_excludes_external() {
        let repo = std::env::current_dir().unwrap();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert("external".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        // "external" is filtered out; with nothing else the result reports 0 roots
        // or the fallback fires. Either way "external" must not appear as a root.
        assert!(!result.contains("**external**"), "external pseudo-slug should be excluded");
    }

    /// Adversarial: active_slugs contains a declared root but NOT the primary slug.
    /// The primary root should still not appear (graph state is authoritative),
    /// and the declared root should appear as-is.
    #[test]
    fn test_list_roots_from_slugs_primary_not_in_graph_excluded() {
        let repo = std::env::current_dir().unwrap();
        // Use a slug that is very unlikely to match the primary slug.
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert("definitely-not-primary-zzz".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        // The placeholder line for the orphaned slug should appear.
        assert!(result.contains("definitely-not-primary-zzz"), "non-primary orphan slug should appear");
        // The primary root slug (repo-native-alignment or similar) should NOT appear
        // since it's not in active_slugs.
        // We can't assert a specific slug name here, but we can assert the count is 1.
        assert!(result.contains("1 root(s)"), "should show exactly 1 root (the orphan)");
    }

    /// Adversarial: external slug mixed with legitimate slugs — only external is excluded.
    #[test]
    fn test_list_roots_from_slugs_external_mixed_with_real_slugs() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());
        active_slugs.insert("external".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        // external should be excluded, primary should appear
        assert!(!result.contains("**external**"), "external should be excluded");
        assert!(result.contains(&primary_slug), "primary slug should appear");
        assert!(result.contains("1 root(s)"), "should show exactly 1 root (external excluded)");
    }

    /// Adversarial: empty slug + external + a real orphan all in active_slugs at once.
    /// Only the real orphan should appear; empty and external must both be excluded.
    #[test]
    fn test_list_roots_from_slugs_empty_external_and_real_orphan_mixed() {
        let repo = std::env::current_dir().unwrap();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert("".to_string());
        active_slugs.insert("external".to_string());
        active_slugs.insert("only-real-zzz".to_string());

        let result = list_roots_from_slugs(&repo, &active_slugs, None, None);
        assert!(!result.contains("- ****: (path unknown"), "empty slug must be excluded");
        assert!(!result.contains("**external**"), "external must be excluded");
        assert!(result.contains("only-real-zzz"), "real orphan must appear");
        assert!(result.contains("1 root(s)"), "should show exactly 1 root");
    }

    // ── list_roots_from_slugs stats tests ────────────────────────────────────

    use crate::graph::{Edge, EdgeKind};

    fn make_node_for_root(root: &str, lang: &str) -> Node {
        Node {
            id: NodeId {
                root: root.to_string(),
                file: std::path::PathBuf::from("src/lib.rs"),
                name: "test_fn".to_string(),
                kind: NodeKind::Function,
            },
            language: lang.to_string(),
            line_start: 1,
            line_end: 5,
            signature: "fn test_fn()".to_string(),
            body: String::new(),
            metadata: std::collections::BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_edge_for_root(root: &str) -> Edge {
        Edge {
            from: NodeId {
                root: root.to_string(),
                file: std::path::PathBuf::from("src/a.rs"),
                name: "a".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: root.to_string(),
                file: std::path::PathBuf::from("src/b.rs"),
                name: "b".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::Calls,
            source: ExtractionSource::TreeSitter,
            confidence: crate::graph::Confidence::Confirmed,
        }
    }

    fn make_test_graph_state(nodes: Vec<Node>, edges: Vec<Edge>) -> crate::server::state::GraphState {
        use crate::graph::index::GraphIndex;
        let index = GraphIndex::new();
        crate::server::state::GraphState {
            nodes,
            edges,
            index,
            last_scan_completed_at: None,
        }
    }

    /// With graph_state provided, per-root symbol and edge counts appear.
    #[test]
    fn test_list_roots_from_slugs_with_symbol_counts() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let nodes = vec![
            make_node_for_root(&primary_slug, "rust"),
            make_node_for_root(&primary_slug, "rust"),
        ];
        let edges = vec![make_edge_for_root(&primary_slug)];
        let gs = make_test_graph_state(nodes, edges);

        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), None);
        assert!(result.contains("Last scan:"), "should show scan line, got: {}", result);
        assert!(result.contains("2 symbols"), "should show 2 symbols, got: {}", result);
        assert!(result.contains("1 edges"), "should show 1 edge, got: {}", result);
    }

    /// Without graph_state, no stats line appears.
    #[test]
    fn test_list_roots_from_slugs_without_stats() {
        let repo = std::env::current_dir().unwrap();
        let result = list_roots_from_slugs(&repo, &std::collections::HashSet::new(), None, None);
        assert!(!result.contains("Last scan:"), "no stats line without graph_state, got: {}", result);
        assert!(!result.contains("symbols"), "no symbol count without graph_state, got: {}", result);
    }

    /// Last scan shown as 'not yet scanned' when last_scan_completed_at is None.
    #[test]
    fn test_list_roots_from_slugs_not_yet_scanned() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let gs = make_test_graph_state(vec![], vec![]);
        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), None);
        assert!(result.contains("not yet scanned"), "should show not yet scanned, got: {}", result);
        assert!(result.contains("0 symbols"), "should show 0 symbols, got: {}", result);
    }

    /// LSP complete state shows server name and edge count.
    #[test]
    fn test_list_roots_from_slugs_lsp_complete() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let nodes = vec![make_node_for_root(&primary_slug, "rust")];
        let gs = make_test_graph_state(nodes, vec![]);

        let lsp = crate::server::state::LspEnrichmentStatus::default();
        lsp.set_server_name("rust-analyzer");
        lsp.set_complete(8410);

        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), Some(&lsp));
        assert!(result.contains("LSP: rust-analyzer"), "should show LSP server, got: {}", result);
        assert!(result.contains("8,410 edges"), "should show edge count with commas, got: {}", result);
    }

    /// Missing LSP servers are shown only for relevant languages.
    #[test]
    fn test_list_roots_from_slugs_missing_lsp_filtered_by_language() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        // Only rust nodes — pyright should NOT appear
        let nodes = vec![make_node_for_root(&primary_slug, "rust")];
        let gs = make_test_graph_state(nodes, vec![]);

        let lsp = crate::server::state::LspEnrichmentStatus::default();
        // Simulate: rust-analyzer found, pyright-langserver missing
        lsp.set_server_name("rust-analyzer");
        lsp.set_server_found();
        // Manually set missing servers via the public API (only missing servers relevant for current langs)
        // We test by confirming pyright doesn't show up for a rust-only root.
        // But we need to actually have the missing_servers populated.
        // Use a fresh status with only pyright as missing (simulate via probe_for_servers won't work in test).
        // Instead, just verify the filtering function directly:
        let rust_langs: std::collections::HashSet<String> = ["rust".to_string()].into();
        assert!(lsp_server_relevant_for_languages("rust-analyzer", &rust_langs));
        assert!(!lsp_server_relevant_for_languages("pyright-langserver", &rust_langs));
        assert!(!lsp_server_relevant_for_languages("gopls", &rust_langs));

        let py_langs: std::collections::HashSet<String> = ["python".to_string()].into();
        assert!(lsp_server_relevant_for_languages("pyright-langserver", &py_langs));
        assert!(!lsp_server_relevant_for_languages("rust-analyzer", &py_langs));

        let ts_langs: std::collections::HashSet<String> = ["typescript".to_string()].into();
        assert!(lsp_server_relevant_for_languages("typescript-language-server", &ts_langs));

        let _ = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), Some(&lsp));
    }

    /// format_count produces comma-separated thousands.
    #[test]
    fn test_format_count_commas() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234), "1,234");
        assert_eq!(format_count(8410), "8,410");
        assert_eq!(format_count(12345), "12,345");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    // ── Adversarial tests seeded from dissent ─────────────────────────────────

    /// Adversarial: graph state with no nodes for a root shows "0 symbols".
    #[test]
    fn test_list_roots_from_slugs_empty_root_shows_zero_symbols() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        // Graph state with nodes from a DIFFERENT root — primary root has 0 symbols.
        let nodes = vec![make_node_for_root("other-root-xyz", "rust")];
        let gs = make_test_graph_state(nodes, vec![]);

        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), None);
        assert!(result.contains("0 symbols"), "root with no nodes should show 0 symbols, got: {}", result);
        assert!(result.contains("0 edges"), "root with no edges should show 0 edges, got: {}", result);
    }

    /// Adversarial: LSP Complete but server_name is None — should not show an empty LSP line.
    #[test]
    fn test_list_roots_from_slugs_lsp_complete_no_server_name() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let nodes = vec![make_node_for_root(&primary_slug, "rust")];
        let gs = make_test_graph_state(nodes, vec![]);

        // Complete state but no server name set.
        let lsp = crate::server::state::LspEnrichmentStatus::default();
        lsp.set_complete(100); // Complete but no server_name set

        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), Some(&lsp));
        // Should not show "LSP:  (100 edges)" with empty server name.
        // The if let Some(ref name) guard prevents this.
        assert!(!result.contains("LSP:  ("), "should not show LSP line with empty server name, got: {}", result);
    }

    /// Adversarial: LSP Unavailable with relevant languages shows "LSP: none detected".
    #[test]
    fn test_list_roots_from_slugs_lsp_unavailable_shows_none_detected() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let nodes = vec![make_node_for_root(&primary_slug, "rust")];
        let gs = make_test_graph_state(nodes, vec![]);

        let lsp = crate::server::state::LspEnrichmentStatus::default();
        lsp.set_unavailable(); // All servers unavailable

        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), Some(&lsp));
        // When LSP unavailable and no relevant missing servers: show "LSP: none detected".
        // (missing_servers is empty since we used default(), not probe_for_servers())
        assert!(result.contains("LSP: none detected"), "should show 'LSP: none detected' when unavailable, got: {}", result);
    }

    /// Adversarial: cross-root edges counted only under 'from' root.
    #[test]
    fn test_list_roots_from_slugs_cross_root_edge_counted_under_from() {
        let repo = std::env::current_dir().unwrap();
        let workspace = crate::roots::WorkspaceConfig::load()
            .with_primary_root(repo.clone())
            .with_worktrees(&repo)
            .with_claude_memory(&repo)
            .with_agent_memories(&repo)
            .with_declared_roots(&repo);
        let resolved = workspace.resolved_roots();
        if resolved.is_empty() { return; }

        let primary_slug = resolved[0].slug.clone();
        let mut active_slugs = std::collections::HashSet::new();
        active_slugs.insert(primary_slug.clone());

        let nodes = vec![make_node_for_root(&primary_slug, "rust")];
        // Edge from primary_slug to "external" root.
        let cross_edge = Edge {
            from: NodeId {
                root: primary_slug.clone(),
                file: std::path::PathBuf::from("src/a.rs"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "external".to_string(),
                file: std::path::PathBuf::from("external/b.rs"),
                name: "callee".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::Calls,
            source: ExtractionSource::TreeSitter,
            confidence: crate::graph::Confidence::Confirmed,
        };
        let gs = make_test_graph_state(nodes, vec![cross_edge]);

        let result = list_roots_from_slugs(&repo, &active_slugs, Some(&gs), None);
        // Cross-root edge counted under primary_slug (from.root == primary_slug).
        assert!(result.contains("1 edges"), "cross-root edge should be counted under from-root, got: {}", result);
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

/// List roots using a known set of active slugs from the in-memory graph.
///
/// When the graph is available, pass its root slugs here so that the output
/// reflects what is actually loaded — including declared roots that were
/// persisted to LanceDB and loaded at startup — rather than re-discovering
/// roots from config at call time.
///
/// Roots are ordered: primary first, then others in slug-alphabetical order.
/// Falls back to config-only discovery when `active_slugs` is empty (e.g.,
/// graph not yet built).
///
/// When `graph_state` is provided, per-root symbol and edge counts are included.
/// When `lsp_status` is provided, LSP working/missing info is included.
pub fn list_roots_from_slugs(
    repo_root: &Path,
    active_slugs: &std::collections::HashSet<String>,
    graph_state: Option<&crate::server::state::GraphState>,
    lsp_status: Option<&LspEnrichmentStatus>,
) -> String {
    let workspace = crate::roots::WorkspaceConfig::load()
        .with_primary_root(repo_root.to_path_buf())
        .with_worktrees(repo_root)
        .with_claude_memory(repo_root)
        .with_agent_memories(repo_root)
        .with_declared_roots(repo_root);
    let all_resolved = workspace.resolved_roots();

    // If we have graph slugs, filter to only roots present in the graph.
    // Unknown slugs (e.g., roots that exist in config but haven't been scanned)
    // are excluded. If active_slugs is empty, fall back to all config-resolved roots.
    let resolved: Vec<_> = if active_slugs.is_empty() {
        all_resolved
    } else {
        all_resolved.into_iter().filter(|r| active_slugs.contains(&r.slug)).collect()
    };

    // Add any graph slugs not accounted for by config (edge case: a root was
    // scanned but its config entry was later removed). Emit a placeholder line.
    let config_slugs: std::collections::HashSet<_> = resolved.iter().map(|r| r.slug.clone()).collect();
    let mut orphaned: Vec<_> = active_slugs.iter().filter(|s| !s.is_empty() && !config_slugs.contains(*s) && *s != "external").cloned().collect();
    orphaned.sort();

    if resolved.is_empty() && orphaned.is_empty() {
        return "No workspace roots configured.".to_string();
    }

    // Pre-compute per-root stats in a single pass over nodes and edges.
    // This keeps list_roots_from_slugs O(nodes + edges + roots) rather than O(roots × nodes).
    let (root_stats, root_langs_map): (
        std::collections::HashMap<String, (usize, usize)>,
        std::collections::HashMap<String, std::collections::BTreeSet<String>>,
    ) = if let Some(gs) = graph_state {
        let mut node_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut edge_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut langs: std::collections::HashMap<String, std::collections::BTreeSet<String>> = std::collections::HashMap::new();
        for n in &gs.nodes {
            *node_counts.entry(n.id.root.clone()).or_insert(0) += 1;
            langs.entry(n.id.root.clone()).or_default().insert(n.language.to_lowercase());
        }
        for e in &gs.edges {
            *edge_counts.entry(e.from.root.clone()).or_insert(0) += 1;
        }
        // Merge into stats map: (node_count, edge_count) per root
        let all_slugs: std::collections::HashSet<String> = node_counts.keys().chain(edge_counts.keys()).cloned().collect();
        let stats = all_slugs.into_iter().map(|slug| {
            let nc = node_counts.get(&slug).copied().unwrap_or(0);
            let ec = edge_counts.get(&slug).copied().unwrap_or(0);
            (slug, (nc, ec))
        }).collect();
        (stats, langs)
    } else {
        (std::collections::HashMap::new(), std::collections::HashMap::new())
    };

    // Last scan age (global — same scan covers all roots).
    let last_scan_age: Option<String> = graph_state.and_then(|gs| gs.last_scan_completed_at).map(|t| {
        let secs = t.elapsed().as_secs();
        if secs < 60 { "just now".to_string() }
        else if secs < 3600 { format!("{}m ago", secs / 60) }
        else if secs < 86400 { format!("{}h ago", secs / 3600) }
        else { format!("{}d ago", secs / 86400) }
    });

    // LSP info for per-root lines.
    let lsp_server_name: Option<String> = lsp_status.and_then(|s| s.server_name());
    let lsp_complete = lsp_status
        .map(|s| s.current_state() == LspState::Complete)
        .unwrap_or(false);
    let lsp_unavailable = lsp_status
        .map(|s| s.current_state() == LspState::Unavailable)
        .unwrap_or(false);
    let lsp_edge_count: usize = lsp_status.map(|s| s.edge_count()).unwrap_or(0);
    let missing_servers: Vec<String> = lsp_status
        .map(|s| s.missing_server_names())
        .unwrap_or_default();

    // Primary root is always first (index 0 in resolved_roots() output).
    let primary_slug = resolved.first().map(|r| r.slug.as_str()).unwrap_or("");
    let mut lines: Vec<String> = resolved.iter()
        .map(|r| {
            let primary = if r.slug == primary_slug { " (primary)" } else { "" };
            let mut line = format!("- **{}**{}: `{}` (type: {}, git: {})",
                r.slug, primary, r.path.display(), r.config.root_type, r.config.git_aware);

            // Per-root stats line.
            let (node_count, edge_count) = root_stats.get(&r.slug).copied().unwrap_or((0, 0));
            if graph_state.is_some() {
                let scan_part = last_scan_age.as_deref().unwrap_or("not yet scanned");
                line.push_str(&format!("\n  Last scan: {} | {} symbols | {} edges",
                    scan_part,
                    format_count(node_count),
                    format_count(edge_count)));
            }

            // LSP working line.
            if lsp_complete {
                if let Some(ref name) = lsp_server_name {
                    // edge_count() returns all LSP-enriched edges (Calls, ReferencedBy, Implements, etc.)
                    line.push_str(&format!("\n  LSP: {} ({} edges)",
                        name, format_count(lsp_edge_count)));
                }
            }

            // Languages line — show detected languages for this root.
            let empty_langs = std::collections::BTreeSet::new();
            let root_lang_set = root_langs_map.get(&r.slug).unwrap_or(&empty_langs);
            if graph_state.is_some() && !root_lang_set.is_empty() {
                let lang_list: Vec<&str> = root_lang_set.iter().map(|s| s.as_str()).collect();
                line.push_str(&format!("\n  Languages: {} (tree-sitter)", lang_list.join(", ")));
            }

            // LSP available but not installed — use precomputed per-root languages.
            let root_langs: std::collections::HashSet<String> = root_lang_set
                .iter()
                .cloned()
                .collect();

            let relevant_missing: Vec<&str> = missing_servers.iter()
                .filter(|server| lsp_server_relevant_for_languages(server, &root_langs))
                .map(|s| s.as_str())
                .collect();

            if !relevant_missing.is_empty() {
                line.push_str(&format!("\n  LSP available but not installed: {}",
                    relevant_missing.join(", ")));
            } else if lsp_unavailable && lsp_status.is_some() {
                // No LSP found and no installable servers to suggest for this root's languages.
                line.push_str("\n  LSP: none detected");
            }

            line
        })
        .collect();

    for slug in &orphaned {
        lines.push(format!("- **{}**: (path unknown — root was scanned but not in current config)", slug));
    }

    let total = lines.len();
    format!("## Workspace Roots\n\n{} root(s)\n\n{}", total, lines.join("\n"))
}

/// Returns true if the given LSP server binary is relevant for any of the languages present in a root.
fn lsp_server_relevant_for_languages(server: &str, languages: &std::collections::HashSet<String>) -> bool {
    let relevant_langs: &[&str] = match server {
        "rust-analyzer" => &["rust"],
        "pyright-langserver" => &["python"],
        "typescript-language-server" => &["typescript", "javascript"],
        "gopls" => &["go"],
        "clangd" => &["c", "cpp", "c++"],
        "taplo" => &["toml"],
        "marksman" => &["markdown"],
        _ => &[],
    };
    relevant_langs.iter().any(|lang| languages.contains(*lang))
}

/// Format a count with comma thousands separators.
fn format_count(n: usize) -> String {
    let s = n.to_string();
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::new();
    let len = chars.len();
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
    }
    result
}

pub fn list_roots(repo_root: &Path) -> String {
    list_roots_from_slugs(repo_root, &std::collections::HashSet::new(), None, None)
}

// ── Repo map ────────────────────────────────────────────────────────

const IMPORTANCE_THRESHOLD: f64 = 0.001;

#[derive(Debug)]
pub struct RepoMapParams { pub top_n: usize, pub root_filter: Option<String>, pub non_code_slugs: HashSet<String> }
pub struct RepoMapContext<'a> { pub graph_state: &'a crate::server::state::GraphState, pub repo_root: &'a Path, pub lsp_status: Option<&'a LspEnrichmentStatus>, pub embed_status: Option<&'a EmbeddingStatus> }

pub fn repo_map(params: &RepoMapParams, ctx: &RepoMapContext<'_>) -> String {
    let graph_state = ctx.graph_state;
    let mut sections: Vec<String> = Vec::new();
    {
        let mut swi: Vec<(&Node, f64)> = graph_state.nodes.iter()
            .filter(|n| !matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field))
            .filter(|n| n.id.root != "external")
            .filter(|n| node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs))
            .filter(|n| !ranking::is_trait_impl_method(n))
            .filter_map(|n| { let imp = n.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let imp = if ranking::is_test_file(n) { imp * 0.1 } else { imp };
                if imp > IMPORTANCE_THRESHOLD { Some((n, imp)) } else { None } }).collect();
        swi.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        swi.truncate(params.top_n);
        if !swi.is_empty() {
            let single_root = params.root_filter.is_some();
            let md: String = swi.iter().map(|(n, imp)| {
                let root_tag = if single_root { String::new() } else { format!(" [{}]", n.id.root) };
                let mut line = format!("- **{}** `{}` ({}){} `{}`:{}-{} -- importance: {:.3}", n.id.kind, n.id.name, n.language, root_tag, n.id.file.display(), n.line_start, n.line_end, imp);
                if let Some(cc) = n.metadata.get("cyclomatic") { line.push_str(&format!(", complexity: {}", cc)); } line }).collect::<Vec<_>>().join("\n");
            sections.push(format!("## Top {} symbols by importance\n\n{}", swi.len(), md));
        }
    }
    // Subsystem detection via Louvain community detection on coupling edges
    {
        // Build node-id -> file-path map for cluster naming
        let node_file_map: std::collections::HashMap<String, String> = graph_state
            .nodes
            .iter()
            .filter(|n| n.id.root != "external")
            .filter(|n| node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs))
            .map(|n| {
                // Normalize to forward slashes so child_name_from_files works
                // on all platforms (Path::display uses OS-native separators).
                let path = n.id.file.to_string_lossy().replace('\\', "/");
                (n.stable_id(), path)
            })
            .collect();

        // Build pagerank scores map from node metadata
        let pagerank_scores: std::collections::HashMap<String, f64> = graph_state
            .nodes
            .iter()
            .filter_map(|n| {
                n.metadata
                    .get("importance")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|imp| (n.stable_id(), imp))
            })
            .collect();

        let mut subsystems = graph_state.index.detect_communities(&pagerank_scores, &node_file_map);
        if !subsystems.is_empty() {
            // Use the filtered node count (from node_file_map, which respects
            // root_filter) as the denominator for giant-cluster detection. This
            // avoids unrelated roots skewing the cutoff in multi-root mode.
            let filtered_node_count = node_file_map.len();
            // Filter out giant clusters that contain >50% of the filtered nodes --
            // they are not informative (everything is lumped together).
            subsystems.retain(|s| (s.symbol_count as f64) < (filtered_node_count as f64 * 0.5));

            // Deduplicate names: when multiple clusters share a name, append
            // a distinguishing suffix derived from the most-common file directory
            // component of the cluster's members.
            let mut name_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for s in &subsystems {
                *name_counts.entry(s.name.clone()).or_default() += 1;
            }
            for s in &mut subsystems {
                if name_counts.get(&s.name).copied().unwrap_or(0) > 1 {
                    // Derive disambiguating suffix from the most-common second-level
                    // directory component of member files rather than from a function
                    // name. E.g., members in src/server/graph.rs -> "graph".
                    let suffix = crate::graph::index::child_name_from_files(
                        &s.member_ids,
                        &node_file_map,
                        &s.name,
                    );
                    s.name = format!("{}/{}", s.name, suffix);
                }
            }

            // Ensure final names are globally unique after disambiguation.
            // Two clusters could still collide if they share the same dominant
            // directory component (e.g., both get "server/graph").
            {
                let mut seen: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for s in &mut subsystems {
                    let count = seen.entry(s.name.clone()).or_default();
                    *count += 1;
                    if *count > 1 {
                        s.name = format!("{}-{}", s.name, *count);
                    }
                }
            }

            // Group flat subsystems by shared module prefix into a hierarchy.
            let grouped = crate::graph::index::group_subsystems_by_prefix(subsystems);

            // Cap output to top 15 top-level subsystems by symbol count.
            let total_detected = grouped.len();
            let shown = grouped.len().min(15);
            let displayed: Vec<_> = grouped.into_iter().take(shown).collect();

            if !displayed.is_empty() {
                let format_interfaces = |s: &crate::graph::index::Subsystem| -> String {
                    if s.interfaces.is_empty() {
                        return String::new();
                    }
                    let iface_list: Vec<String> = s
                        .interfaces
                        .iter()
                        .map(|iface| {
                            let short_name = iface
                                .node_id
                                .split(':')
                                .rev()
                                .nth(1)
                                .unwrap_or(&iface.node_id);
                            if iface.node_type == "function" {
                                format!("{}()", short_name)
                            } else {
                                short_name.to_string()
                            }
                        })
                        .collect();
                    format!("\n  Interfaces: {}", iface_list.join(", "))
                };

                let md: String = displayed
                    .iter()
                    .map(|s| {
                        let sub_modules = if s.children.is_empty() {
                            String::new()
                        } else {
                            let child_names: Vec<String> = s.children.iter()
                                .map(|c| {
                                    // Strip parent prefix for cleaner display
                                    let short = c.name.strip_prefix(&format!("{}/", s.name))
                                        .unwrap_or(&c.name);
                                    format!("{} ({})", short, c.symbol_count)
                                })
                                .collect();
                            format!("\n  Sub-modules: {}", child_names.join(", "))
                        };
                        let interfaces_str = format_interfaces(s);
                        format!(
                            "- **{}** ({} symbols, cohesion: {:.2}){}{}",
                            s.name, s.symbol_count, s.cohesion, sub_modules, interfaces_str
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let suffix = if total_detected > shown {
                    format!(" (showing top {})", shown)
                } else {
                    String::new()
                };
                sections.push(format!(
                    "## Subsystems ({} detected{})\n\n{}",
                    total_detected, suffix, md
                ));
            }
        }
    }
    { let mut fc: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
        for n in &graph_state.nodes { if matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field) { continue; }
            if n.id.root == "external" { continue; }
            if !node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs) { continue; }
            *fc.entry((n.id.root.clone(), n.id.file.display().to_string())).or_default() += 1; }
        let mut sf: Vec<_> = fc.into_iter().collect(); sf.sort_by(|a, b| b.1.cmp(&a.1)); sf.truncate(10);
        let single_root = params.root_filter.is_some();
        if !sf.is_empty() { let md: String = sf.iter().map(|((root, f), count)| {
            if single_root { format!("- `{}` -- {} definitions", f, count) }
            else { format!("- [{}] `{}` -- {} definitions", root, f, count) }
        }).collect::<Vec<_>>().join("\n"); sections.push(format!("## Hotspot files\n\n{}", md)); } }
    { let outcomes = crate::oh::load_oh_artifacts(ctx.repo_root).unwrap_or_default().into_iter().filter(|a| a.kind == crate::types::OhArtifactKind::Outcome).collect::<Vec<_>>();
        if !outcomes.is_empty() { let md: String = outcomes.iter().map(|o| { let files: Vec<String> = o.frontmatter.get("files").and_then(|v| v.as_sequence()).map(|seq| seq.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            let fs = if files.is_empty() { String::new() } else { format!(" (files: {})", files.join(", ")) }; format!("- **{}**{}", o.id(), fs) }).collect::<Vec<_>>().join("\n"); sections.push(format!("## Active outcomes\n\n{}", md)); } }
    { let mut ep: Vec<&Node> = graph_state.nodes.iter().filter(|n| n.id.kind == NodeKind::Function && n.id.root != "external")
            .filter(|n| node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs))
            .filter(|n| !ranking::is_test_function(n))
            .filter(|n| { let name = n.id.name.to_lowercase(); name == "main" || name.starts_with("handle_") || name.starts_with("handler") || name.ends_with("_handler") || name.contains("endpoint") }).collect();
        ep.sort_by(|a, b| { let ia = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0); let ib = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0); ib.partial_cmp(&ia).unwrap_or(std::cmp::Ordering::Equal) });
        ep.truncate(10);
        if !ep.is_empty() {
            let single_root = params.root_filter.is_some();
            let md: String = ep.iter().map(|n| {
                if single_root { format!("- **{}** `{}`:{}-{}", n.id.name, n.id.file.display(), n.line_start, n.line_end) }
                else { format!("- **{}** [{}] `{}`:{}-{}", n.id.name, n.id.root, n.id.file.display(), n.line_start, n.line_end) }
            }).collect::<Vec<_>>().join("\n");
            sections.push(format!("## Entry points\n\n{}", md));
        } }
    let freshness = format_freshness_full(graph_state.nodes.len(), graph_state.last_scan_completed_at, ctx.lsp_status, ctx.embed_status);
    if sections.is_empty() { format!("No repository data available yet.{}", freshness) } else { format!("# Repository Map\n\n{}{}", sections.join("\n\n"), freshness) }
}
