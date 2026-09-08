//! Repository map: top symbols by importance, subsystem layout, hotspot files.

use std::collections::HashSet;
use std::path::Path;

use crate::graph::{Node, NodeKind};
use crate::ranking;
use crate::server::helpers::{collect_artifact_notes, format_freshness_full};
use crate::server::state::{EmbeddingStatus, LspEnrichmentStatus};

use super::node_passes_root_filter;

// ── Repo map ────────────────────────────────────────────────────────

const IMPORTANCE_THRESHOLD: f64 = 0.001;

/// Strongest git co-change partner for `file` in `root`, if any (#884).
/// Returns `(partner_file_display, support, confidence)`.
fn top_cochange_partner(
    graph_state: &crate::server::state::GraphState,
    root: &str,
    file: &Path,
) -> Option<(String, u32, f64)> {
    if graph_state.cochange_stats.is_empty() {
        return None;
    }
    let anchor_id = crate::git::cochange::file_anchor_node_id(root, file);
    let anchor_stable = anchor_id.to_stable_id();
    let index_map = graph_state.node_index_map();

    let mut best: Option<(String, u32, f64)> = None;
    for direction in [
        petgraph::Direction::Outgoing,
        petgraph::Direction::Incoming,
    ] {
        for neighbor_stable in graph_state.index.neighbors(
            &anchor_stable,
            Some(&[crate::graph::EdgeKind::CoChanges]),
            direction,
        ) {
            let Some(neighbor_node) = graph_state.node_by_stable_id(&neighbor_stable, index_map)
            else {
                continue;
            };
            let (from, to) = match direction {
                petgraph::Direction::Outgoing => (anchor_id.clone(), neighbor_node.id.clone()),
                petgraph::Direction::Incoming => (neighbor_node.id.clone(), anchor_id.clone()),
            };
            let edge = crate::graph::Edge {
                from,
                to,
                kind: crate::graph::EdgeKind::CoChanges,
                source: crate::graph::ExtractionSource::Git,
                confidence: crate::graph::Confidence::Detected,
                evidence: Vec::new(),
            };
            // Rank by support first, confidence second -- see the matching
            // comment in `service/search.rs`'s mode="cochange" for why (a
            // single-occurrence pair trivially has confidence=1.0).
            if let Some(stats) = graph_state.cochange_stats.get(&edge.stable_id())
                && best
                    .as_ref()
                    .is_none_or(|(_, s, c)| (stats.support, stats.confidence) > (*s, *c))
            {
                best = Some((
                    neighbor_node.id.file.display().to_string(),
                    stats.support,
                    stats.confidence,
                ));
            }
        }
    }
    best
}

#[derive(Debug)]
pub struct RepoMapParams {
    pub top_n: usize,
    pub root_filter: Option<String>,
    pub non_code_slugs: HashSet<String>,
}
pub struct RepoMapContext<'a> {
    pub graph_state: &'a crate::server::state::GraphState,
    pub repo_root: &'a Path,
    pub lsp_status: Option<&'a LspEnrichmentStatus>,
    pub embed_status: Option<&'a EmbeddingStatus>,
    pub business_context: &'a crate::business_context::BusinessContextAdmission,
}

pub fn repo_map(params: &RepoMapParams, ctx: &RepoMapContext<'_>) -> String {
    let graph_state = ctx.graph_state;
    let mut sections: Vec<String> = Vec::new();
    {
        let mut swi: Vec<(&Node, f64)> = graph_state
            .nodes
            .iter()
            .filter(|n| {
                !matches!(
                    n.id.kind,
                    NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field
                )
            })
            .filter(|n| n.id.root != "external")
            .filter(|n| {
                node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs)
            })
            .filter(|n| !ranking::is_trait_impl_method(n))
            .filter_map(|n| {
                let imp = n
                    .metadata
                    .get("importance")
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let imp = if ranking::is_test_file(n) {
                    imp * 0.1
                } else {
                    imp
                };
                if imp > IMPORTANCE_THRESHOLD {
                    Some((n, imp))
                } else {
                    None
                }
            })
            .collect();
        swi.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.id.to_stable_id().cmp(&b.0.id.to_stable_id()))
                .then_with(|| a.0.line_start.cmp(&b.0.line_start))
                .then_with(|| a.0.line_end.cmp(&b.0.line_end))
        });
        let mut seen_identities = HashSet::new();
        swi.retain(|(node, _)| seen_identities.insert(node.id.to_stable_id()));
        swi.truncate(params.top_n);
        if !swi.is_empty() {
            let single_root = params.root_filter.is_some();
            let md: String = swi
                .iter()
                .map(|(n, imp)| {
                    let root_tag = if single_root {
                        String::new()
                    } else {
                        format!(" [{}]", n.id.root)
                    };
                    let mut line = format!(
                        "- **{}** `{}` ({}){} `{}`:{}-{} -- importance: {:.3}",
                        n.id.kind,
                        n.id.name,
                        n.language,
                        root_tag,
                        n.id.file.display(),
                        n.line_start,
                        n.line_end,
                        imp
                    );
                    if let Some(cc) = n.metadata.get("cyclomatic") {
                        line.push_str(&format!(", complexity: {}", cc));
                    }
                    let notes_index_map = graph_state.node_index_map();
                    let notes = collect_artifact_notes(&n.stable_id(), &graph_state.index, |id| {
                        graph_state.node_by_stable_id(id, notes_index_map)
                    });
                    if !notes.is_empty() {
                        line.push_str(&format!(" notes:{}", notes.len()));
                    }
                    line
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!(
                "## Top {} symbols by importance\n\n{}",
                swi.len(),
                md
            ));
        }
    }
    // Subsystem detection via Louvain community detection on coupling edges
    {
        // Build node-id -> file-path map for cluster naming
        let node_file_map: std::collections::HashMap<String, String> = graph_state
            .nodes
            .iter()
            .filter(|n| n.id.root != "external")
            .filter(|n| {
                node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs)
            })
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

        let mut subsystems = graph_state
            .index
            .detect_communities(&pagerank_scores, &node_file_map);
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
            let mut name_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
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
                            let child_names: Vec<String> = s
                                .children
                                .iter()
                                .map(|c| {
                                    // Strip parent prefix for cleaner display
                                    let short = c
                                        .name
                                        .strip_prefix(&format!("{}/", s.name))
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
    {
        // Hotspot files (#889): ranked by churn x aggregate complexity, not
        // raw definition count (a size proxy previously printed under a risk
        // label -- see issue #889/#888). Churn = commits touching the file
        // within co-change mining's bounded window (`file_changes`, stamped
        // as `churn` metadata on the file's `NodeKind::Other("file")` anchor
        // node). Complexity = summed `cyclomatic` over the file's
        // non-excluded symbols (same complexity signal already rendered
        // elsewhere -- see `server/helpers.rs`).
        let mut complexity_by_file: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        for n in &graph_state.nodes {
            if matches!(
                n.id.kind,
                NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field
            ) {
                continue;
            }
            if n.id.root == "external" {
                continue;
            }
            if !node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs) {
                continue;
            }
            let cc: i64 = n
                .metadata
                .get("cyclomatic")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            // `or_default()` ensures the file appears in the ranking even when
            // every one of its non-excluded nodes has zero complexity (e.g. a
            // file of plain structs/consts) -- an explicit zero, not absence.
            *complexity_by_file
                .entry((n.id.root.clone(), n.id.file.display().to_string()))
                .or_default() += cc;
        }

        // Co-change mining (and therefore churn) only ever runs against the
        // single primary/mined root, mirroring `search.rs`'s mode="cochange"
        // and `roots.rs`'s availability line. Files on any other root, or any
        // root with no `.git`, have no churn signal -- state that explicitly
        // rather than implying churn 0 (#889).
        let mined_root = crate::roots::RootConfig::code_project(ctx.repo_root.to_path_buf()).slug();
        let git_available = git2::Repository::open(ctx.repo_root).is_ok();
        let mut churn_by_file: std::collections::HashMap<(String, String), u32> =
            std::collections::HashMap::new();
        for n in &graph_state.nodes {
            if let NodeKind::Other(kind) = &n.id.kind
                && kind == "file"
                && let Some(churn) = n.metadata.get("churn").and_then(|s| s.parse::<u32>().ok())
            {
                churn_by_file.insert((n.id.root.clone(), n.id.file.display().to_string()), churn);
            }
        }

        enum ChurnBasis {
            /// Churn known for this file's root (0 is a real, in-window count,
            /// not "unknown").
            Known(u32),
            /// No churn signal for this file -- reason stated in the output.
            Unavailable(&'static str),
        }

        let mut hotspots: Vec<(String, String, ChurnBasis, i64, f64)> = complexity_by_file
            .into_iter()
            .map(|((root, file), complexity)| {
                let basis = if root != mined_root {
                    ChurnBasis::Unavailable("root not mined for co-change/churn")
                } else if !git_available {
                    ChurnBasis::Unavailable("no .git")
                } else {
                    let churn = churn_by_file
                        .get(&(root.clone(), file.clone()))
                        .copied()
                        .unwrap_or(0);
                    ChurnBasis::Known(churn)
                };
                let score = match &basis {
                    ChurnBasis::Known(churn) => *churn as f64 * complexity as f64,
                    ChurnBasis::Unavailable(_) => 0.0,
                };
                (root, file, basis, complexity, score)
            })
            .collect();

        // Rank by score desc; ties (including all-zero-score files) break
        // deterministically by file path then root.
        hotspots.sort_by(|a, b| {
            b.4.partial_cmp(&a.4)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        hotspots.truncate(10);

        let single_root = params.root_filter.is_some();
        if !hotspots.is_empty() {
            let md: String = hotspots
                .iter()
                .map(|(root, f, basis, complexity, score)| {
                    // #884: append the strongest git co-change partner, if any,
                    // so agents see "these two files usually change together"
                    // directly in the existing Hotspot files section rather than
                    // requiring a separate search(mode="cochange") round trip.
                    let cochange_suffix =
                        top_cochange_partner(graph_state, root, std::path::Path::new(f))
                            .map(|(partner, support, confidence)| {
                                format!(
                                    " (co-changes with `{}`: support={}, confidence={:.2})",
                                    partner, support, confidence
                                )
                            })
                            .unwrap_or_default();
                    let basis_str = match basis {
                        ChurnBasis::Known(churn) => {
                            format!("churn {} x complexity {} = {:.0}", churn, complexity, score)
                        }
                        ChurnBasis::Unavailable(reason) => {
                            format!(
                                "churn: not available ({}); complexity {}",
                                reason, complexity
                            )
                        }
                    };
                    if single_root {
                        format!("- `{}` -- {}{}", f, basis_str, cochange_suffix)
                    } else {
                        format!("- [{}] `{}` -- {}{}", root, f, basis_str, cochange_suffix)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!(
                "## Hotspot files (ranked by churn x complexity: commits in window x summed cyclomatic)\n\n{}",
                md
            ));
        }
    }
    if !ctx.business_context.mode().is_disabled() {
        let outcomes = crate::oh::load_oh_artifacts(ctx.repo_root)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.kind == crate::types::OhArtifactKind::Outcome)
            .collect::<Vec<_>>();
        if !outcomes.is_empty() {
            let md: String = outcomes
                .iter()
                .map(|o| {
                    let files: Vec<String> = o
                        .frontmatter
                        .get("files")
                        .and_then(|v| v.as_sequence())
                        .map(|seq| {
                            seq.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let fs = if files.is_empty() {
                        String::new()
                    } else {
                        format!(" (files: {})", files.join(", "))
                    };
                    format!("- **{}**{}", o.id(), fs)
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("## Active outcomes\n\n{}", md));
        }
    }
    {
        let mut ep: Vec<&Node> = graph_state
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function && n.id.root != "external")
            .filter(|n| {
                node_passes_root_filter(&n.id.root, &params.root_filter, &params.non_code_slugs)
            })
            .filter(|n| !ranking::is_test_function(n))
            .filter(|n| {
                let name = n.id.name.to_lowercase();
                name == "main"
                    || name.starts_with("handle_")
                    || name.starts_with("handler")
                    || name.ends_with("_handler")
                    || name.contains("endpoint")
            })
            .collect();
        ep.sort_by(|a, b| {
            let ia = a
                .metadata
                .get("importance")
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            let ib = b
                .metadata
                .get("importance")
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            ib.partial_cmp(&ia).unwrap_or(std::cmp::Ordering::Equal)
        });
        ep.truncate(10);
        if !ep.is_empty() {
            let single_root = params.root_filter.is_some();
            let md: String = ep
                .iter()
                .map(|n| {
                    if single_root {
                        format!(
                            "- **{}** `{}`:{}-{}",
                            n.id.name,
                            n.id.file.display(),
                            n.line_start,
                            n.line_end
                        )
                    } else {
                        format!(
                            "- **{}** [{}] `{}`:{}-{}",
                            n.id.name,
                            n.id.root,
                            n.id.file.display(),
                            n.line_start,
                            n.line_end
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("## Entry points\n\n{}", md));
        }
    }
    let freshness = format_freshness_full(
        graph_state.nodes.len(),
        graph_state.last_scan_completed_at,
        ctx.lsp_status,
        ctx.embed_status,
    );
    if sections.is_empty() {
        format!("No repository data available yet.{}", freshness)
    } else {
        format!("# Repository Map\n\n{}{}", sections.join("\n\n"), freshness)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business_context::{BusinessContextAdmission, BusinessContextMode};
    use crate::graph::index::GraphIndex;
    use crate::graph::{ExtractionSource, NodeId};
    use crate::server::state::GraphState;
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

    fn make_graph_state(nodes: Vec<Node>) -> GraphState {
        let index = GraphIndex::new();
        GraphState::new(nodes, vec![], index, None, std::collections::HashSet::new())
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
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(long_slug.to_string()),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(
            !result.contains(&format!("[{}]", long_slug)),
            "Single-root mode should not show root slug prefix: {}",
            result
        );
        assert!(
            result.contains("important_fn"),
            "Should still show the symbol name"
        );
    }

    /// In multi-root mode (root_filter=None), repo_map shows root slugs.
    #[test]
    fn test_repo_map_multi_root_shows_prefix() {
        let mut node = make_node("main", NodeKind::Function, "src/main.rs");
        node.id.root = "project-a".to_string();
        node.metadata.insert("importance".into(), "0.5".into());
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: None,
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(
            result.contains("[project-a]"),
            "Multi-root mode should show root slug prefix: {}",
            result
        );
    }

    #[test]
    fn test_repo_map_deduplicates_top_symbols_by_stable_identity() {
        let mut duplicate_a = make_node("NodeKind", NodeKind::Enum, "src/graph/mod.rs");
        duplicate_a
            .metadata
            .insert("importance".into(), "0.9".into());
        duplicate_a.line_start = 10;
        duplicate_a.line_end = 20;

        let mut duplicate_b = make_node("NodeKind", NodeKind::Enum, "src/graph/mod.rs");
        duplicate_b
            .metadata
            .insert("importance".into(), "0.8".into());
        duplicate_b.line_start = 10;
        duplicate_b.line_end = 20;

        let mut distinct_same_name = make_node("NodeKind", NodeKind::Enum, "src/extract/mod.rs");
        distinct_same_name
            .metadata
            .insert("importance".into(), "0.7".into());
        distinct_same_name.line_start = 30;
        distinct_same_name.line_end = 40;

        let gs = make_graph_state(vec![duplicate_a, duplicate_b, distinct_same_name]);
        let repo_root = PathBuf::from("/tmp/test");
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some("local".to_string()),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert_eq!(
            result.matches("`src/graph/mod.rs`:10-20").count(),
            1,
            "duplicate stable identities should be shown once: {result}"
        );
        assert!(
            result.contains("`src/extract/mod.rs`:30-40"),
            "same short name in a distinct file should remain visible: {result}"
        );
    }

    #[test]
    fn disabled_business_context_skips_live_outcome_loading() {
        let graph_state = make_graph_state(Vec::new());
        let repo_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/business_context_isolation");
        let params = RepoMapParams {
            top_n: 15,
            root_filter: None,
            non_code_slugs: HashSet::new(),
        };

        let enabled = BusinessContextAdmission::default();
        let enabled_result = repo_map(
            &params,
            &RepoMapContext {
                graph_state: &graph_state,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                business_context: &enabled,
            },
        );
        assert!(enabled_result.contains("## Active outcomes"));

        let disabled = BusinessContextAdmission::new(BusinessContextMode::Disabled);
        let disabled_result = repo_map(
            &params,
            &RepoMapContext {
                graph_state: &graph_state,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                business_context: &disabled,
            },
        );
        assert!(!disabled_result.contains("## Active outcomes"));
    }

    // ── Hotspot files: churn x complexity ranking (#889) ────────────

    fn make_file_anchor(root: &str, file: &str, churn: u32) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("churn".to_string(), churn.to_string());
        Node {
            id: NodeId {
                kind: NodeKind::Other("file".to_string()),
                name: file.to_string(),
                file: PathBuf::from(file),
                root: root.to_string(),
            },
            language: String::new(),
            signature: format!("file {}", file),
            line_start: 0,
            line_end: 0,
            body: String::new(),
            metadata,
            source: ExtractionSource::Git,
        }
    }

    fn make_function(root: &str, file: &str, name: &str, cyclomatic: u32) -> Node {
        let mut node = make_node(name, NodeKind::Function, file);
        node.id.root = root.to_string();
        node.metadata
            .insert("cyclomatic".to_string(), cyclomatic.to_string());
        node
    }

    /// Ranking multiplies churn by aggregate complexity, so a file with lower
    /// churn but much higher complexity can outrank a high-churn, low-complexity
    /// file (#889 -- this is the whole point of the redefinition).
    #[test]
    fn test_hotspot_ranking_multiplies_churn_and_complexity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();
        git2::Repository::init(&repo_root).unwrap();
        let root = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

        // file_a: churn 5 x complexity 10 = 50
        // file_b: churn 1 x complexity 100 = 100 -- should rank first.
        let mut nodes = vec![
            make_function(&root, "src/a.rs", "a_fn", 10),
            make_function(&root, "src/b.rs", "b_fn", 100),
            make_file_anchor(&root, "src/a.rs", 5),
            make_file_anchor(&root, "src/b.rs", 1),
        ];
        nodes.iter_mut().for_each(|n| {
            n.metadata.insert("importance".into(), "0.01".into());
        });
        let gs = make_graph_state(nodes);
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(root.clone()),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        let hotspot_section = result
            .split("## Hotspot files")
            .nth(1)
            .expect("hotspot section present");
        let b_pos = hotspot_section.find("src/b.rs").expect("b.rs present");
        let a_pos = hotspot_section.find("src/a.rs").expect("a.rs present");
        assert!(
            b_pos < a_pos,
            "higher-scoring file (b.rs, score 100) should rank above a.rs (score 50): {result}"
        );
        assert!(
            hotspot_section.contains("churn 1 x complexity 100 = 100"),
            "expected auditable score components for b.rs: {result}"
        );
        assert!(
            hotspot_section.contains("churn 5 x complexity 10 = 50"),
            "expected auditable score components for a.rs: {result}"
        );
    }

    /// Files with zero churn (git available, file just never changed in the
    /// mined window) still appear, ranked at the bottom, with an explicit "0".
    #[test]
    fn test_hotspot_ranking_zero_churn_is_explicit_not_hidden() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();
        git2::Repository::init(&repo_root).unwrap();
        let root = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

        // No file anchor node for src/never_changed.rs -- churn_by_file has no
        // entry for it despite git being available, so it must render "0".
        let mut nodes = vec![make_function(&root, "src/never_changed.rs", "f", 42)];
        nodes[0].metadata.insert("importance".into(), "0.01".into());
        let gs = make_graph_state(nodes);
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(root),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(
            result.contains("churn 0 x complexity 42 = 0"),
            "zero churn must be stated explicitly, not omitted: {result}"
        );
    }

    /// Files with zero aggregate complexity (e.g. a file of plain structs) still
    /// appear, with complexity stated as 0 rather than the file being dropped.
    #[test]
    fn test_hotspot_ranking_zero_complexity_is_explicit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();
        git2::Repository::init(&repo_root).unwrap();
        let root = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

        // A struct node has no "cyclomatic" metadata key.
        let mut nodes = vec![
            make_node("Plain", NodeKind::Struct, "src/plain.rs"),
            make_file_anchor(&root, "src/plain.rs", 9),
        ];
        nodes[0].id.root = root.clone();
        nodes[0].metadata.insert("importance".into(), "0.01".into());
        let gs = make_graph_state(nodes);
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(root),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(
            result.contains("churn 9 x complexity 0 = 0"),
            "zero complexity must be stated explicitly: {result}"
        );
    }

    /// Ties (equal churn x complexity score) break deterministically rather
    /// than depending on HashMap iteration order.
    #[test]
    fn test_hotspot_ranking_ties_break_deterministically() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();
        git2::Repository::init(&repo_root).unwrap();
        let root = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

        let mut nodes = vec![
            make_function(&root, "src/z.rs", "z_fn", 10),
            make_function(&root, "src/y.rs", "y_fn", 10),
            make_file_anchor(&root, "src/z.rs", 2),
            make_file_anchor(&root, "src/y.rs", 2),
        ];
        nodes.iter_mut().for_each(|n| {
            n.metadata.insert("importance".into(), "0.01".into());
        });
        let gs1 = make_graph_state(nodes.clone());
        let gs2 = make_graph_state({
            nodes.reverse();
            nodes
        });
        let business_context = BusinessContextAdmission::default();
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(root.clone()),
            non_code_slugs: HashSet::new(),
        };

        let result1 = repo_map(
            &params,
            &RepoMapContext {
                graph_state: &gs1,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                business_context: &business_context,
            },
        );
        let result2 = repo_map(
            &params,
            &RepoMapContext {
                graph_state: &gs2,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                business_context: &business_context,
            },
        );
        let hotspots1 = result1.split("## Hotspot files").nth(1).unwrap();
        let hotspots2 = result2.split("## Hotspot files").nth(1).unwrap();
        assert_eq!(
            hotspots1, hotspots2,
            "tied scores must sort deterministically regardless of input order"
        );
        // y.rs sorts before z.rs alphabetically -- the tiebreak key.
        let y_pos = hotspots1.find("src/y.rs").unwrap();
        let z_pos = hotspots1.find("src/z.rs").unwrap();
        assert!(y_pos < z_pos, "alphabetical tiebreak: {hotspots1}");
    }

    /// A file on a root that co-change mining never ran against (not the
    /// primary/mined root) reports churn as unavailable with a stated reason,
    /// never a silent zero.
    #[test]
    fn test_hotspot_ranking_non_mined_root_states_churn_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_root = tmp.path().to_path_buf();
        git2::Repository::init(&repo_root).unwrap();

        // "other-root" is not `RootConfig::code_project(repo_root).slug()`.
        let mut node = make_function("other-root", "src/secondary.rs", "f", 7);
        node.metadata.insert("importance".into(), "0.01".into());
        let gs = make_graph_state(vec![node]);
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: None,
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(
            result.contains(
                "churn: not available (root not mined for co-change/churn); complexity 7"
            ),
            "non-mined root must state why churn is unavailable, not imply 0: {result}"
        );
    }

    /// A non-git repo root degrades explicitly ("no .git"), never a silent zero.
    #[test]
    fn test_hotspot_ranking_non_git_root_states_churn_unavailable() {
        let repo_root = PathBuf::from("/tmp/definitely-not-a-git-repo-889");
        let root = crate::roots::RootConfig::code_project(repo_root.clone()).slug();
        let mut node = make_function(&root, "src/f.rs", "f", 3);
        node.metadata.insert("importance".into(), "0.01".into());
        let gs = make_graph_state(vec![node]);
        let business_context = BusinessContextAdmission::default();
        let ctx = RepoMapContext {
            graph_state: &gs,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            business_context: &business_context,
        };
        let params = RepoMapParams {
            top_n: 15,
            root_filter: Some(root),
            non_code_slugs: HashSet::new(),
        };

        let result = repo_map(&params, &ctx);
        assert!(
            result.contains("churn: not available (no .git); complexity 3"),
            "non-git root must state why churn is unavailable, not imply 0: {result}"
        );
    }
}
