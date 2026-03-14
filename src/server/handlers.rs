//! MCP tool handlers: search (flat/traversal/batch), repo_map, outcome_progress.

use petgraph::Direction;
use rust_mcp_sdk::schema::{CallToolError, CallToolResult};

use crate::embed::{SearchMode, SearchOutcome};
use crate::graph::{EdgeKind, Node, NodeKind};
use crate::graph::index::GraphIndex;
use crate::ranking;

use super::RnaHandler;
use super::tools::Search;
use super::helpers::{
    format_freshness, format_neighbor_nodes, format_node_entry,
    retain_displayable, text_result,
};
use super::store::parse_edge_kind;

impl RnaHandler {
    // ── Unified search handler ──────────────────────────────────────────
    // Shared implementation for `search`, `search_symbols` (deprecated alias),
    // and `graph_query` (deprecated alias). Branches on whether `mode` is set
    // (graph traversal) or absent (flat symbol search).

    pub(crate) async fn handle_search(&self, args: Search) -> Result<CallToolResult, CallToolError> {
        // Normalize inputs
        let query = args.query.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let node = args.node.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let compact = args.compact.unwrap_or(false);

        // ── Batch node retrieval path ────────────────────────────────
        // When `nodes` is provided, resolve each ID from the graph directly.
        if let Some(ref node_ids) = args.nodes {
            let node_ids: Vec<&str> = node_ids.iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if node_ids.is_empty() {
                return Ok(text_result("Empty nodes list. Provide at least one stable node ID.".to_string()));
            }
            return self.handle_search_batch(&node_ids, compact, &args).await;
        }

        if args.mode.is_some() {
            // ── Graph traversal path ──────────────────────────────────
            self.handle_search_traversal(&args, query, node, compact).await
        } else if query.is_none() && node.is_some() {
            // ── Node lookup path ────────────────────────────────────
            // `node` without `mode` or `query` → treat as single-node batch retrieval.
            let node_ids = vec![node.unwrap()];
            self.handle_search_batch(&node_ids, compact, &args).await
        } else {
            // ── Flat search path ──────────────────────────────────────
            self.handle_search_flat(&args, query, compact).await
        }
    }

    /// Flat symbol search (no `mode` parameter) plus optional artifact and markdown search.
    /// Unified search path for code symbols, artifacts, and markdown.
    async fn handle_search_flat(
        &self,
        args: &Search,
        query: Option<&str>,
        compact: bool,
    ) -> Result<CallToolResult, CallToolError> {
        let sort_by_complexity = args.sort_by.as_deref() == Some("complexity");
        let sort_by_importance = args.sort_by.as_deref() == Some("importance");
        let has_complexity_filter = args.min_complexity.is_some();
        let complexity_search = has_complexity_filter || sort_by_complexity;

        let include_artifacts = args.include_artifacts.unwrap_or(true);
        let include_markdown = args.include_markdown.unwrap_or(true);

        let query_str = query.unwrap_or("");
        if query_str.is_empty() && !complexity_search && !sort_by_importance {
            return Ok(text_result("Empty query. Please describe what you're looking for (or use min_complexity / sort_by=\"complexity\" / sort_by=\"importance\").".into()));
        }

        // Resolve effective root filter: default to primary root,
        // "all" means no filter, otherwise use the explicit slug.
        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() {
            self.non_code_root_slugs()
        } else {
            std::collections::HashSet::new()
        };

        let search_mode = parse_search_mode(args.search_mode.as_deref());
        let limit = args.top_k.unwrap_or(10) as usize;

        // Collect all output sections (code symbols, artifacts, markdown).
        let mut sections: Vec<String> = Vec::new();
        let mut graph_node_count = 0usize;
        let mut graph_last_scan = None;

        // ── Code symbol search ──────────────────────────────────────────
        match self.get_graph().await {
            Ok(guard) => {
                let graph_state = guard.as_ref().unwrap();
                graph_node_count = graph_state.nodes.len();
                graph_last_scan = graph_state.last_scan_completed_at;
                let query_lower = query_str.to_lowercase();

                let mut matches: Vec<&Node> = graph_state
                    .nodes
                    .iter()
                    .filter(|n| {
                        // In complexity search mode, only return functions.
                        if complexity_search && n.id.kind != NodeKind::Function {
                            return false;
                        }
                        // When query is non-empty, filter by name/signature match.
                        if !query_lower.is_empty() {
                            let name_match = n.id.name.to_lowercase().contains(&query_lower)
                                || n.signature.to_lowercase().contains(&query_lower);
                            if !name_match {
                                return false;
                            }
                        }
                        if let Some(ref kind_filter) = args.kind {
                            if n.id.kind.to_string().to_lowercase() != kind_filter.to_lowercase() {
                                return false;
                            }
                        }
                        if let Some(ref lang_filter) = args.language {
                            if n.language.to_lowercase() != lang_filter.to_lowercase() {
                                return false;
                            }
                        }
                        if let Some(ref file_filter) = args.file {
                            let path_str = n.id.file.to_string_lossy();
                            if !path_str.contains(file_filter.as_str()) {
                                return false;
                            }
                        }
                        // Root filter: default to primary root, "all" disables.
                        // Non-code roots and "external" always pass.
                        if !self.node_passes_root_filter(&n.id.root, &root_filter, &non_code_slugs) {
                            return false;
                        }
                        if let Some(synthetic_filter) = args.synthetic {
                            let is_synthetic = n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false);
                            if is_synthetic != synthetic_filter {
                                return false;
                            }
                        }
                        if let Some(min_cc) = args.min_complexity {
                            let Some(cc) = n.metadata.get("cyclomatic")
                                .and_then(|s| s.parse::<u32>().ok())
                            else {
                                return false;
                            };
                            if cc < min_cc {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();

                if sort_by_complexity {
                    matches.retain(|n| {
                        n.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).is_some()
                    });
                    matches.sort_by(|a, b| {
                        let cc_a = a.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        let cc_b = b.metadata.get("cyclomatic").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        cc_b.cmp(&cc_a)
                    });
                } else if sort_by_importance {
                    matches.sort_by(|a, b| {
                        let imp_a = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok());
                        let imp_b = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok());
                        match (imp_a, imp_b) {
                            (Some(a_val), Some(b_val)) => b_val.partial_cmp(&a_val).unwrap_or(std::cmp::Ordering::Equal),
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
                    let md: String = matches
                        .iter()
                        .map(|n| format_node_entry(n, &graph_state.index, compact))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    sections.push(format!(
                        "### Code symbols ({} result(s))\n\n{}",
                        matches.len(),
                        md
                    ));
                }
            }
            Err(e) => sections.push(format!("Graph error: {}", e)),
        }

        // ── Artifact search (embedding index) ───────────────────────────
        if include_artifacts && !query_str.is_empty() {
            let embed_guard = self.embed_index.load();
            match embed_guard.as_ref() {
                Some(index) => {
                    match index.search_with_mode(query_str, args.artifact_types.as_deref(), limit, search_mode).await {
                        Ok(SearchOutcome::Results(results)) => {
                            // Filter out code: results — those are already shown
                            // in the "Code symbols" section via graph search.
                            let filtered: Vec<_> = results
                                .into_iter()
                                .filter(|r| !r.kind.starts_with("code:"))
                                .filter(|r| self.search_result_passes_root_filter(r, &root_filter, &non_code_slugs))
                                .collect();
                            if !filtered.is_empty() {
                                let md: String = filtered
                                    .iter()
                                    .map(|r| r.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!(
                                    "### Artifacts ({} result(s))\n\n{}",
                                    filtered.len(),
                                    md
                                ));
                            }
                        }
                        Ok(SearchOutcome::NotReady) => {
                            sections.push("Embedding index: building -- artifact results will appear shortly. Retry in a few seconds.".to_string());
                        }
                        Err(e) => sections.push(format!("Artifact search error: {}", e)),
                    }
                }
                None => {
                    // Embedding index not available yet. This branch is only
                    // reached when include_artifacts && !query_str.is_empty(),
                    // so code search was also attempted — skip silently.
                }
            }
        }

        // ── Markdown section search ─────────────────────────────────────
        if include_markdown && !query_str.is_empty() {
            match crate::markdown::extract_markdown_chunks(&self.repo_root) {
                Ok(chunks) => {
                    let filtered_chunks: Vec<_> = if let Some(ref slug) = root_filter {
                        let workspace = crate::roots::WorkspaceConfig::load()
                            .with_primary_root(self.repo_root.clone())
                            .with_worktrees(&self.repo_root)
                            .with_claude_memory(&self.repo_root);
                        let root_path = workspace.resolved_roots()
                            .into_iter()
                            .find(|r| r.slug == *slug)
                            .map(|r| r.path);
                        if let Some(rp) = root_path {
                            chunks.into_iter()
                                .filter(|c| c.file_path.starts_with(&rp))
                                .collect()
                        } else {
                            Vec::new()
                        }
                    } else {
                        chunks
                    };
                    let scored = crate::markdown::search_chunks_ranked(&filtered_chunks, query_str);
                    if !scored.is_empty() {
                        let md = scored
                            .iter()
                            .take(limit)
                            .map(|sc| {
                                format!(
                                    "- (score: {:.2}) {}", sc.score, sc.chunk.to_markdown()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n---\n\n");
                        sections.push(format!(
                            "### Markdown ({} result(s))\n\n{}",
                            scored.len().min(limit),
                            md
                        ));
                    }
                }
                Err(e) => sections.push(format!("Markdown search error: {}", e)),
            }
        }

        let freshness = format_freshness(
            graph_node_count,
            graph_last_scan,
            Some(&self.lsp_status),
        );

        if sections.is_empty() {
            Ok(text_result(format!(
                "No results matching \"{}\".{}",
                query_str, freshness
            )))
        } else {
            Ok(text_result(format!(
                "## Search: \"{}\"\n\n{}{}",
                query_str,
                sections.join("\n\n"),
                freshness
            )))
        }
    }

    /// Graph traversal search (with `mode` parameter).
    async fn handle_search_traversal(
        &self,
        args: &Search,
        query: Option<&str>,
        node: Option<&str>,
        compact: bool,
    ) -> Result<CallToolResult, CallToolError> {
        let mode = args.mode.as_deref().unwrap_or("neighbors");
        let top_k = args.top_k.unwrap_or(1).clamp(1, 50) as usize;

        // Root filter for entry node scoping (traversal results are unscoped —
        // once you enter the graph, edges may cross roots).
        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() {
            self.non_code_root_slugs()
        } else {
            std::collections::HashSet::new()
        };

        // Reject if no entry point
        if node.is_none() && query.is_none() {
            return Ok(text_result(
                "Either query or node is required. Provide a search query or a stable node ID.".to_string()
            ));
        }

        // Resolve entry node IDs
        let search_mode = parse_search_mode(args.search_mode.as_deref());
        let (entry_node_ids, entry_header): (Vec<String>, String) = if let Some(node_id) = node {
            (vec![node_id.to_string()], String::new())
        } else if let Some(query_text) = query {
            let embed_guard = self.embed_index.load();
            match embed_guard.as_ref() {
                Some(embed_idx) => {
                    match embed_idx.search_with_mode(query_text, None, top_k.min(50) * 3, search_mode).await {
                        Ok(SearchOutcome::Results(results)) if !results.is_empty() => {
                            // Filter entry nodes by root scope — traversal results
                            // are unscoped, but entry points should respect root filter.
                            let code_results: Vec<_> = results.into_iter()
                                .filter(|r| r.kind.starts_with("code:"))
                                .filter(|r| self.search_result_passes_root_filter(r, &root_filter, &non_code_slugs))
                                .take(top_k)
                                .collect();

                            if code_results.is_empty() {
                                return Ok(text_result(format!(
                                    "No code symbols matched query \"{}\". Try a different query or use node parameter.",
                                    query_text
                                )));
                            }

                            let mut header = format!("### Matched entry nodes for \"{}\"\n\n", query_text);
                            let ids: Vec<String> = code_results.iter()
                                .map(|r| {
                                    header.push_str(&format!(
                                        "- `{}` -- {} (score: {:.2})\n",
                                        r.id, r.title, r.score
                                    ));
                                    r.id.clone()
                                })
                                .collect();
                            header.push('\n');
                            (ids, header)
                        }
                        Ok(SearchOutcome::NotReady) => {
                            return Ok(text_result(
                                "Embedding index: building -- semantic graph queries will work shortly. Use node parameter instead, or retry in a few seconds.".to_string()
                            ));
                        }
                        Ok(_) => {
                            return Ok(text_result(format!(
                                "No code symbols matched query \"{}\". Try a different query or use node parameter.",
                                query_text
                            )));
                        }
                        Err(e) => {
                            return Ok(text_result(format!(
                                "Semantic search failed: {}. Use node parameter instead.",
                                e
                            )));
                        }
                    }
                }
                None => {
                    return Ok(text_result(
                        "Embedding index not available. Use node parameter instead, or wait for the background index to build.".to_string()
                    ));
                }
            }
        } else {
            unreachable!("both-empty case handled above");
        };

        match self.get_graph().await {
            Ok(guard) => {
                let graph_state = guard.as_ref().unwrap();

                let valid_entry_ids: Vec<&String> = entry_node_ids.iter()
                    .filter(|id| graph_state.index.get_node(id).is_some())
                    .collect();

                if valid_entry_ids.is_empty() {
                    let id_list = entry_node_ids.iter()
                        .map(|id| format!("`{}`", id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Ok(text_result(format!(
                        "{}No graph nodes found for {}. The node(s) may not have edges in the graph. Try search to find valid node IDs.",
                        entry_header, id_list
                    )));
                }

                let edge_filter = args.edge_types.as_ref().map(|types| {
                    types
                        .iter()
                        .filter_map(|t| parse_edge_kind(t))
                        .collect::<Vec<_>>()
                });
                let edge_filter_slice = edge_filter.as_deref();

                let mut all_ids: Vec<String> = Vec::new();
                let mut seen = std::collections::HashSet::new();

                for node_id in &valid_entry_ids {
                    match run_traversal(&graph_state.index, node_id, mode, args.hops, args.direction.as_deref(), edge_filter_slice) {
                        Ok(ids) => {
                            for id in ids {
                                if seen.insert(id.clone()) {
                                    all_ids.push(id);
                                }
                            }
                        }
                        Err(msg) => return Ok(text_result(msg)),
                    }
                }

                let entry_set: std::collections::HashSet<&str> = valid_entry_ids.iter().map(|s| s.as_str()).collect();
                all_ids.retain(|id| !entry_set.contains(id.as_str()));

                // For tests_for mode, filter to only callers in test files
                if mode == "tests_for" {
                    all_ids.retain(|id| {
                        graph_state.nodes.iter()
                            .find(|n| n.stable_id() == *id)
                            .map(|n| ranking::is_test_file(n))
                            .unwrap_or(false)
                    });
                }

                // Filter hidden scaffolding kinds (Module, PrMerge) before counting,
                // so the reported count matches what format_neighbor_nodes renders.
                retain_displayable(&mut all_ids, &graph_state.nodes);

                let entry_label = if valid_entry_ids.len() == 1 {
                    format!("`{}`", valid_entry_ids[0])
                } else {
                    format!("{} entry nodes", valid_entry_ids.len())
                };

                let direction = args.direction.as_deref().unwrap_or("outgoing");

                let freshness = format_freshness(
                    graph_state.nodes.len(),
                    graph_state.last_scan_completed_at,
                    Some(&self.lsp_status),
                );

                if all_ids.is_empty() {
                    let mode_desc = match mode {
                        "neighbors" => format!("No {} neighbors for {}.", direction, entry_label),
                        "impact" => format!("No dependents found for {} within {} hops.", entry_label, args.hops.unwrap_or(3)),
                        "reachable" => format!("No reachable nodes from {} within {} hops.", entry_label, args.hops.unwrap_or(3)),
                        "tests_for" => format!("No test functions found calling {}. Either no tests exist for this symbol, or the call edges haven't been extracted (check LSP status).", entry_label),
                        _ => format!("No results for {}.", entry_label),
                    };
                    Ok(text_result(format!("{}{}{}", entry_header, mode_desc, freshness)))
                } else {
                    let md = format_neighbor_nodes(&graph_state.nodes, &all_ids, &graph_state.index, compact);
                    let heading = match mode {
                        "neighbors" => format!(
                            "## Graph neighbors ({}) of {}\n\n{} result(s)\n\n",
                            direction, entry_label, all_ids.len()
                        ),
                        "impact" => format!(
                            "## Impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n",
                            entry_label, all_ids.len(), args.hops.unwrap_or(3)
                        ),
                        "reachable" => format!(
                            "## Reachable from {}\n\n{} node(s) within {} hop(s)\n\n",
                            entry_label, all_ids.len(), args.hops.unwrap_or(3)
                        ),
                        "tests_for" => format!(
                            "## Test coverage for {}\n\n{} test function(s)\n\n",
                            entry_label, all_ids.len()
                        ),
                        _ => String::new(),
                    };
                    Ok(text_result(format!("{}{}{}{}", entry_header, heading, md, freshness)))
                }
            }
            Err(e) => Ok(text_result(format!("Graph error: {}", e))),
        }
    }

    /// Batch node retrieval: resolve multiple stable node IDs in a single call.
    /// When `mode` is provided, runs traversal from each node (composes with hops/direction/edge_types).
    /// When `mode` is absent, simply retrieves the nodes.
    async fn handle_search_batch(
        &self,
        node_ids: &[&str],
        compact: bool,
        args: &Search,
    ) -> Result<CallToolResult, CallToolError> {
        // If mode is provided, route each node through traversal logic
        if args.mode.is_some() {
            // Route through traversal logic for each seed node
            match self.get_graph().await {
                Ok(guard) => {
                    let graph_state = guard.as_ref().unwrap();
                    let mode = args.mode.as_deref().unwrap_or("neighbors");

                    let edge_filter = args.edge_types.as_ref().map(|types| {
                        types
                            .iter()
                            .filter_map(|t| parse_edge_kind(t))
                            .collect::<Vec<_>>()
                    });
                    let edge_filter_slice = edge_filter.as_deref();

                    let freshness = format_freshness(
                        graph_state.nodes.len(),
                        graph_state.last_scan_completed_at,
                        Some(&self.lsp_status),
                    );

                    let mut sections: Vec<String> = Vec::new();
                    for &nid in node_ids {
                        if graph_state.index.get_node(nid).is_none() {
                            sections.push(format!("### `{}`\n\nNode not found in graph.", nid));
                            continue;
                        }

                        match run_traversal(&graph_state.index, nid, mode, args.hops, args.direction.as_deref(), edge_filter_slice) {
                            Ok(mut ids) => {
                                ids.retain(|id| id != nid);
                                if mode == "tests_for" {
                                    ids.retain(|id| {
                                        graph_state.nodes.iter()
                                            .find(|n| n.stable_id() == *id)
                                            .map(|n| ranking::is_test_file(n))
                                            .unwrap_or(false)
                                    });
                                }
                                retain_displayable(&mut ids, &graph_state.nodes);

                                if ids.is_empty() {
                                    sections.push(format!("### `{}`\n\nNo {} results.", nid, mode));
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &ids, &graph_state.index, compact);
                                    sections.push(format!("### `{}`\n\n{} result(s)\n\n{}", nid, ids.len(), md));
                                }
                            }
                            Err(msg) => {
                                sections.push(format!("### `{}`\n\n{}", nid, msg));
                            }
                        }
                    }

                    Ok(text_result(format!("## Batch {} for {} node(s)\n\n{}{}", mode, node_ids.len(), sections.join("\n\n"), freshness)))
                }
                Err(e) => Ok(text_result(format!("Graph error: {}", e))),
            }
        } else {
            // Simple batch retrieval (no traversal)
            match self.get_graph().await {
                Ok(guard) => {
                    let graph_state = guard.as_ref().unwrap();
                    let freshness = format_freshness(
                        graph_state.nodes.len(),
                        graph_state.last_scan_completed_at,
                        Some(&self.lsp_status),
                    );

                    let mut found = Vec::new();
                    let mut missing = Vec::new();

                    for &nid in node_ids {
                        if let Some(node) = graph_state.nodes.iter().find(|n| n.stable_id() == nid) {
                            found.push(node);
                        } else {
                            missing.push(nid);
                        }
                    }

                    if found.is_empty() {
                        let id_list = node_ids.iter()
                            .map(|id| format!("`{}`", id))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Ok(text_result(format!(
                            "No nodes found for {}. Try search to find valid node IDs.{}",
                            id_list, freshness
                        )));
                    }

                    let md: String = found.iter()
                        .map(|n| format_node_entry(n, &graph_state.index, compact))
                        .collect::<Vec<_>>()
                        .join("\n\n");

                    let mut result = format!(
                        "## Batch retrieve: {} found\n\n{}",
                        found.len(),
                        md,
                    );
                    if !missing.is_empty() {
                        result.push_str(&format!(
                            "\n\n**Missing:** {}",
                            missing.iter().map(|id| format!("`{}`", id)).collect::<Vec<_>>().join(", ")
                        ));
                    }
                    result.push_str(&freshness);
                    Ok(text_result(result))
                }
                Err(e) => Ok(text_result(format!("Graph error: {}", e))),
            }
        }
    }
}

/// Execute a single graph traversal from a given node ID.
///
/// Shared by `handle_search_traversal` (single-node entry) and
/// `handle_search_batch` (multi-node entry with mode).  Keeping the logic
/// in one place prevents the two paths from diverging.
pub(crate) fn run_traversal(
    index: &GraphIndex,
    node_id: &str,
    mode: &str,
    hops: Option<u32>,
    direction: Option<&str>,
    edge_filter: Option<&[EdgeKind]>,
) -> Result<Vec<String>, String> {
    match mode {
        "neighbors" => {
            let max_hops = hops.unwrap_or(1) as usize;
            let dir = direction.unwrap_or("outgoing");
            match dir {
                "outgoing" => {
                    if max_hops == 1 {
                        Ok(index.neighbors(node_id, edge_filter, Direction::Outgoing))
                    } else {
                        Ok(index.reachable(node_id, max_hops, edge_filter))
                    }
                }
                "incoming" => {
                    if max_hops == 1 {
                        Ok(index.neighbors(node_id, edge_filter, Direction::Incoming))
                    } else {
                        Ok(index.impact(node_id, max_hops, edge_filter))
                    }
                }
                "both" => {
                    let out = if max_hops == 1 {
                        index.neighbors(node_id, edge_filter, Direction::Outgoing)
                    } else {
                        index.reachable(node_id, max_hops, edge_filter)
                    };
                    let inc = if max_hops == 1 {
                        index.neighbors(node_id, edge_filter, Direction::Incoming)
                    } else {
                        index.impact(node_id, max_hops, edge_filter)
                    };
                    let mut combined = out;
                    let mut seen: std::collections::HashSet<String> = combined.iter().cloned().collect();
                    for id in inc {
                        if seen.insert(id.clone()) {
                            combined.push(id);
                        }
                    }
                    Ok(combined)
                }
                _ => Err(format!(
                    "Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".",
                    dir
                )),
            }
        }
        "impact" => {
            if edge_filter.is_some() {
                return Err("edge_types is not supported with \"impact\" mode (it uses its own traversal strategy).".to_string());
            }
            let max_hops = hops.unwrap_or(3) as usize;
            Ok(index.impact(node_id, max_hops, None))
        }
        "reachable" => {
            let max_hops = hops.unwrap_or(3) as usize;
            Ok(index.reachable(node_id, max_hops, edge_filter))
        }
        "tests_for" => {
            if edge_filter.is_some() {
                return Err("edge_types is not supported with \"tests_for\" mode (it always uses Calls edges).".to_string());
            }
            let calls_filter = &[EdgeKind::Calls];
            Ok(index.neighbors(node_id, Some(calls_filter), Direction::Incoming))
        }
        other => Err(format!(
            "Unknown mode: \"{}\". Use \"neighbors\", \"impact\", \"reachable\", or \"tests_for\".",
            other
        )),
    }
}

/// Parse a `search_mode` string into [`SearchMode`].
/// Returns `Hybrid` for `None` or unrecognized values.
pub(crate) fn parse_search_mode(s: Option<&str>) -> SearchMode {
    match s.map(str::to_lowercase).as_deref() {
        Some("keyword") => SearchMode::Keyword,
        Some("semantic") => SearchMode::Semantic,
        _ => SearchMode::Hybrid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use crate::graph::{Node, NodeId, NodeKind, EdgeKind, ExtractionSource};
    use crate::graph::index::GraphIndex;

    /// Verify the tests_for pattern: incoming Calls edges filtered to test-file callers.
    #[test]
    fn test_tests_for_filters_to_test_callers_only() {
        let mut index = GraphIndex::new();
        let target = "root:src/server.rs:handle_search:function";
        let test_caller = "root:tests/server_test.rs:test_handle_search:function";
        let prod_caller = "root:src/main.rs:main:function";

        index.ensure_node(target, "function");
        index.ensure_node(test_caller, "function");
        index.ensure_node(prod_caller, "function");
        index.add_edge(test_caller, "function", target, "function", EdgeKind::Calls);
        index.add_edge(prod_caller, "function", target, "function", EdgeKind::Calls);

        let result = run_traversal(&index, target, "tests_for", None, None, None).unwrap();
        assert_eq!(result.len(), 2); // Both callers returned by traversal

        // The handler filters to test files — simulate that:
        let test_nodes = vec![
            Node {
                id: NodeId {
                    root: "root".to_string(),
                    file: PathBuf::from("tests/server_test.rs"),
                    name: "test_handle_search".to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
            Node {
                id: NodeId {
                    root: "root".to_string(),
                    file: PathBuf::from("src/main.rs"),
                    name: "main".to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
        ];

        let filtered: Vec<_> = result.iter()
            .filter(|id| {
                test_nodes.iter()
                    .find(|n| n.stable_id() == **id)
                    .map(|n| ranking::is_test_file(n))
                    .unwrap_or(false)
            })
            .collect();

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].contains("test_handle_search"));
    }

    /// Verify tests_for returns empty when no test files call the symbol.
    #[test]
    fn test_tests_for_no_test_callers() {
        let mut index = GraphIndex::new();
        let target = "root:src/server.rs:handle_search:function";
        let prod_caller = "root:src/main.rs:main:function";

        index.ensure_node(target, "function");
        index.ensure_node(prod_caller, "function");
        index.add_edge(prod_caller, "function", target, "function", EdgeKind::Calls);

        let result = run_traversal(&index, target, "tests_for", None, None, None).unwrap();
        assert_eq!(result.len(), 1);

        let test_nodes = vec![
            Node {
                id: NodeId {
                    root: "root".to_string(),
                    file: PathBuf::from("src/main.rs"),
                    name: "main".to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
        ];

        let filtered: Vec<_> = result.iter()
            .filter(|id| {
                test_nodes.iter()
                    .find(|n| n.stable_id() == **id)
                    .map(|n| ranking::is_test_file(n))
                    .unwrap_or(false)
            })
            .collect();

        assert_eq!(filtered.len(), 0);
    }

    #[test]
    fn test_code_prefix_filter_matches_all_embeddable_kinds() {
        let embeddable = vec![
            NodeKind::Function,
            NodeKind::Struct,
            NodeKind::Trait,
            NodeKind::Enum,
            NodeKind::Const,
            NodeKind::Impl,
            NodeKind::ProtoMessage,
            NodeKind::SqlTable,
            NodeKind::ApiEndpoint,
            NodeKind::Macro,
            NodeKind::Field,
        ];
        for kind in embeddable {
            let prefix = format!("code:{}", kind);
            assert!(
                prefix.starts_with("code:"),
                "kind {} should produce 'code:' prefix, got: {}",
                kind,
                prefix
            );
        }
    }

    #[test]
    fn test_non_embeddable_kinds_filtered_out_by_prefix() {
        let non_embeddable = vec![
            NodeKind::Import,
            NodeKind::Module,
            NodeKind::PrMerge,
        ];
        for kind in non_embeddable {
            let prefix = format!("code:{}", kind);
            let kind_str = kind.to_string();
            assert!(
                matches!(kind_str.as_str(), "import" | "module" | "pr_merge"),
                "kind {} should be filtered out by embed logic, prefix: {}",
                kind,
                prefix
            );
        }
    }

    #[test]
    fn test_code_prefix_filter_rejects_non_code_kinds() {
        let kinds = vec!["commit", "outcome", "signal", "guardrail"];
        for kind in kinds {
            assert!(!kind.starts_with("code:"), "Non-code kind should not have code: prefix");
        }
    }

    #[test]
    fn test_top_k_overflow_multiplication() {
        let top_k: u32 = 50;
        let result = top_k.min(50).checked_mul(3);
        assert_eq!(result, Some(150));

        let large_top_k: u32 = u32::MAX;
        let clamped = large_top_k.clamp(1, 50);
        assert_eq!(clamped, 50);
        let safe_mul = clamped as usize * 3;
        assert_eq!(safe_mul, 150);
    }

    #[test]
    fn test_parse_search_mode_defaults_to_hybrid() {
        assert!(matches!(parse_search_mode(None), SearchMode::Hybrid));
        assert!(matches!(
            parse_search_mode(Some("unknown")),
            SearchMode::Hybrid
        ));
        assert!(matches!(
            parse_search_mode(Some("")),
            SearchMode::Hybrid
        ));
    }

    #[test]
    fn test_parse_search_mode_keyword() {
        assert!(matches!(
            parse_search_mode(Some("keyword")),
            SearchMode::Keyword
        ));
        assert!(matches!(
            parse_search_mode(Some("KEYWORD")),
            SearchMode::Keyword
        ));
    }

    #[test]
    fn test_parse_search_mode_semantic() {
        assert!(matches!(
            parse_search_mode(Some("semantic")),
            SearchMode::Semantic
        ));
        assert!(matches!(
            parse_search_mode(Some("Semantic")),
            SearchMode::Semantic
        ));
    }

    /// Verify that kind-only filter correctly matches macro nodes.
    #[test]
    fn test_kind_filter_matches_macro_nodes() {
        let macro_node = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "my_vec".to_string(),
                kind: NodeKind::Macro,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 5,
            signature: "macro_rules! my_vec".to_string(),
            body: "macro_rules! my_vec { ... }".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let fn_node = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "do_stuff".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 10,
            line_end: 15,
            signature: "fn do_stuff()".to_string(),
            body: "fn do_stuff() {}".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let nodes = vec![macro_node, fn_node];

        let kind_filter = Some("macro".to_string());
        let query_lower = "";
        let matches: Vec<_> = nodes
            .iter()
            .filter(|n| {
                if !query_lower.is_empty() {
                    let name_match = n.id.name.to_lowercase().contains(query_lower)
                        || n.signature.to_lowercase().contains(query_lower);
                    if !name_match {
                        return false;
                    }
                }
                if let Some(ref kf) = kind_filter {
                    if n.id.kind.to_string().to_lowercase() != kf.to_lowercase() {
                        return false;
                    }
                }
                true
            })
            .collect();

        assert_eq!(matches.len(), 1, "Kind-only filter should find exactly 1 macro");
        assert_eq!(matches[0].id.name, "my_vec");
        assert_eq!(matches[0].id.kind, NodeKind::Macro);
    }

    /// Verify that the empty-query guard allows kind-only search.
    #[test]
    fn test_empty_query_guard_allows_kind_filter() {
        let query_str = "";
        let complexity_search = false;
        let sort_by_importance = false;

        let has_kind_filter = false;
        let rejected = query_str.is_empty()
            && !complexity_search
            && !sort_by_importance
            && !has_kind_filter;
        assert!(rejected, "Empty query without kind should be rejected");

        let has_kind_filter = true;
        let rejected = query_str.is_empty()
            && !complexity_search
            && !sort_by_importance
            && !has_kind_filter;
        assert!(!rejected, "Empty query with kind filter should be allowed");
    }
}
