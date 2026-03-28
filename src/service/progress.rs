//! Outcome progress tracking and blast-radius reporting.

use std::collections::HashSet;
use std::path::Path;

use crate::graph::Edge;

use super::node_passes_root_filter;

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

pub fn outcome_progress(
    params: &OutcomeProgressParams,
    ctx: &OutcomeProgressContext<'_>,
) -> String {
    let graph_nodes: Vec<crate::graph::Node> = ctx
        .graph_state
        .nodes
        .iter()
        .filter(|n| {
            node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs)
        })
        .cloned()
        .collect();
    match crate::query::outcome_progress(ctx.repo_root, &params.outcome_id, &graph_nodes) {
        Ok(result) => {
            let mut md = result.to_summary_markdown();
            let file_patterns: Vec<String> = result
                .outcomes
                .first()
                .and_then(|o| o.frontmatter.get("files"))
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            // Use graph_nodes (already root-filtered) and matching filtered edges
            // to avoid PR merges from unrelated roots appearing in the report.
            let graph_node_ids: std::collections::HashSet<String> =
                graph_nodes.iter().map(|n| n.stable_id()).collect();
            let filtered_edges: Vec<Edge> = ctx
                .graph_state
                .edges
                .iter()
                .filter(|e| {
                    // Keep edges where either endpoint is in the root-filtered graph.
                    // PrMerge nodes reference `Serves` outcome edges whose `from` is
                    // the PrMerge node; we want those to appear even when they don't
                    // have a corresponding graph_node entry.
                    let from_id = e.from.to_stable_id();
                    let to_id = e.to.to_stable_id();
                    node_passes_root_filter(
                        &e.from.root,
                        &params.root_filter,
                        &params.non_code_slugs,
                    ) || node_passes_root_filter(
                        &e.to.root,
                        &params.root_filter,
                        &params.non_code_slugs,
                    ) || graph_node_ids.contains(&from_id)
                        || graph_node_ids.contains(&to_id)
                })
                .cloned()
                .collect();
            let pr_nodes = crate::query::find_pr_merges_for_outcome(
                &graph_nodes,
                &filtered_edges,
                &params.outcome_id,
                &file_patterns,
            );
            if !pr_nodes.is_empty() {
                md.push_str(&format!(
                    "\n## PR Merges\n\n{} PR merge(s) serving this outcome\n",
                    pr_nodes.len()
                ));
            }
            if params.include_impact && !result.code_symbols.is_empty() {
                let impacted = crate::query::compute_impact_risk(
                    &result.code_symbols,
                    &graph_nodes,
                    &ctx.graph_state.index,
                    3,
                );
                md.push('\n');
                md.push_str(&crate::query::format_impact_markdown(&impacted));
            } else if params.include_impact && result.code_symbols.is_empty() {
                md.push_str("\n## Change Impact\n\nNo changed symbols found -- cannot compute blast radius.\n");
            }
            md
        }
        Err(e) => format!("Error: {}", e),
    }
}
