//! Workspace root listing with per-root scan stats and LSP status.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// Per-root stats tuple: (node_count, edge_count) and language set.
type RootStatsMap = (HashMap<String, (usize, usize)>, HashMap<String, BTreeSet<String>>);

use crate::server::state::{LspEnrichmentStatus, LspState};

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

    // Capture the primary slug from the full config list (index 0 in resolved_roots()).
    // Filtering below may exclude the primary root if it isn't in active_slugs, but
    // we still need its slug to correctly tag any other roots that do appear.
    let primary_slug_from_config = all_resolved.first().map(|r| r.slug.clone()).unwrap_or_default();

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
    let (root_stats, root_langs_map): RootStatsMap = if let Some(gs) = graph_state {
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

    // Use the primary slug from the full config list, not the filtered one.
    // If the real primary root isn't in active_slugs, we still want (primary) to be
    // tagged correctly for any displayed root that happens to be primary.
    let primary_slug = primary_slug_from_config.as_str();
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
            if lsp_complete
                && let Some(ref name) = lsp_server_name {
                    // edge_count() returns all LSP-enriched edges (Calls, ReferencedBy, Implements, etc.)
                    line.push_str(&format!("\n  LSP: {} ({} edges)",
                        name, format_count(lsp_edge_count)));
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
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(*ch);
    }
    result
}

pub fn list_roots(repo_root: &Path) -> String {
    list_roots_from_slugs(repo_root, &std::collections::HashSet::new(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, NodeKind};
    use crate::graph::ExtractionSource;
    use crate::graph::NodeId;

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
            detected_frameworks: std::collections::HashSet::new(),
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
