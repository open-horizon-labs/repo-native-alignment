//! Repository map: top symbols by importance, subsystem layout, hotspot files.

use std::collections::HashSet;
use std::path::Path;

use crate::graph::{Node, NodeKind};
use crate::ranking;
use crate::server::helpers::format_freshness_full;
use crate::server::state::{EmbeddingStatus, LspEnrichmentStatus};

use super::node_passes_root_filter;

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
            // Filter out giant clusters that contain >50% of the filtered nodes (strictly)
            // they are not informative (everything is lumped together).
            subsystems.retain(|s| (s.symbol_count as f64) <= (filtered_node_count as f64 * 0.5));

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use crate::graph::{NodeId, ExtractionSource};
    use crate::server::state::GraphState;
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
        GraphState { nodes, edges: vec![], index, last_scan_completed_at: None, detected_frameworks: std::collections::HashSet::new() }
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
}
