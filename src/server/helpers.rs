//! Formatting utilities, argument parsing, and display helpers.

use crate::graph;
use crate::graph::index::GraphIndex;
use petgraph::Direction;
use rust_mcp_sdk::schema::{CallToolError, CallToolResult, TextContent};
use super::state::{EmbeddingStatus, LspEnrichmentStatus};

/// Minimum importance score to display in tool output.
/// Scores at or below this threshold are suppressed as noise.
pub(crate) const IMPORTANCE_THRESHOLD: f64 = 0.001;

pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<T, CallToolError> {
    let value = match arguments {
        Some(map) => serde_json::Value::Object(map),
        None => serde_json::Value::Object(serde_json::Map::new()), // empty object, not null
    };
    serde_json::from_value(value)
        .map_err(|e| CallToolError::from_message(format!("Invalid arguments: {}", e)))
}

pub(crate) fn text_result(s: String) -> CallToolResult {
    CallToolResult::text_content(vec![TextContent::new(s, None, None)])
}

/// Format an index freshness footer for appending to tool responses.
///
/// Example output: `\n*Index: 3655 symbols · last scan 4m ago · schema v2*`
pub fn format_freshness(
    node_count: usize,
    last_scan: Option<std::time::Instant>,
    lsp_status: Option<&LspEnrichmentStatus>,
) -> String {
    format_freshness_full(node_count, last_scan, lsp_status, None)
}

/// Format an index freshness footer with optional embedding status.
///
/// When `embed_status` is provided, shows build progress during embedding
/// and the final count when complete.
pub fn format_freshness_full(
    node_count: usize,
    last_scan: Option<std::time::Instant>,
    lsp_status: Option<&LspEnrichmentStatus>,
    embed_status: Option<&EmbeddingStatus>,
) -> String {
    let age = match last_scan {
        None => "never".to_string(),
        Some(t) => {
            let secs = t.elapsed().as_secs();
            if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else {
                format!("{}h ago", secs / 3600)
            }
        }
    };

    let lsp_part = lsp_status
        .and_then(|s| s.footer_segment())
        .map(|seg| format!(" · {}", seg))
        .unwrap_or_default();

    let embed_part = embed_status
        .and_then(|s| s.footer_segment())
        .map(|seg| format!(" · {}", seg))
        .unwrap_or_default();

    format!(
        "\n\n*Index: {} symbols · last scan {} · schema v{} · extract v{}{}{}*",
        node_count,
        age,
        crate::graph::store::SCHEMA_VERSION,
        crate::graph::store::EXTRACTION_VERSION,
        embed_part,
        lsp_part,
    )
}

/// Resolve an edge's `to.file` against the set of known scanned file paths.
/// If `to.file` doesn't match any known file but a known file ends with it
/// (suffix match), update the edge to point to the matched file.
/// This handles Python absolute imports where the import path is a suffix
/// of the actual file path (e.g., `src/util/user_utils.py` matches
/// `ai_service/src/util/user_utils.py`).
pub(crate) fn resolve_edge_target_by_suffix(
    edge: &mut graph::Edge,
    file_index: &std::collections::HashSet<String>,
) {
    let target = edge.to.file.to_string_lossy().to_string();
    if file_index.contains(&target) {
        return; // exact match, nothing to resolve
    }
    // Suffix match: find a scanned file that ends with the target path
    let suffix = format!("/{}", target);
    let matches: Vec<&String> = file_index
        .iter()
        .filter(|f| f.ends_with(&suffix))
        .collect();
    if matches.len() == 1 {
        edge.to.file = std::path::PathBuf::from(matches[0]);
        edge.confidence = graph::Confidence::Confirmed;
        tracing::debug!(
            "Resolved import edge target: {} → {}",
            target,
            matches[0]
        );
    }
    // If 0 or 2+ matches, leave as-is (ambiguous or truly dangling)
}

/// Strip the root slug prefix from a stable ID when in single-root mode.
///
/// Stable IDs have the form `root:file:name:kind`. In single-root mode the
/// root prefix is noise (62+ chars). This function strips it, returning
/// `file:name:kind`. When `strip_root` is `None`, the full ID is returned.
pub(crate) fn strip_root_prefix(stable_id: &str, strip_root: Option<&str>) -> String {
    if let Some(slug) = strip_root {
        let prefix = format!("{}:", slug);
        if let Some(rest) = stable_id.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    stable_id.to_string()
}

/// Format an unresolved stable ID (no matching graph node) in a human-readable way.
///
/// Parses `root:file:name:kind` and renders as `**name** (file)` instead of
/// dumping the raw stable ID. Uses right-side splitting so it works correctly
/// both with and without root prefix stripping. Falls back to the (possibly
/// stripped) raw ID if parsing fails.
pub(crate) fn format_unresolved_id(id: &str, strip_root: Option<&str>) -> String {
    let display_id = strip_root_prefix(id, strip_root);
    // Stable ID format: root:file:name:kind. Split the original id from the
    // right so file paths with colons (e.g. Windows `C:\...`) are handled correctly.
    let rparts: Vec<&str> = id.rsplitn(3, ':').collect();
    if rparts.len() >= 3 {
        // rparts[0] = kind, rparts[1] = name, rparts[2] = root:file
        let name = rparts[1];
        let prefix = rparts[2];
        // Strip the root slug from the prefix to get the file path
        let file = strip_root
            .and_then(|slug| prefix.strip_prefix(&format!("{}:", slug)))
            .unwrap_or(prefix);
        format!("- **{}** ({})", name, file)
    } else if rparts.len() == 2 {
        // Only two parts: treat as name:kind
        let name = rparts[1];
        format!("- **{}**", name)
    } else {
        format!("- `{}`", display_id)
    }
}

/// Format a single node for search results output.
///
/// When `compact` is true, returns a one-line summary: kind, name, file:lines, signature.
/// When `compact` is false (default), returns full detail: ID, signature, value, complexity, edges.
/// Wrap a string in CommonMark inline code using a backtick fence long enough
/// to safely contain any backticks in the content itself.  Per the CommonMark
/// spec, backslash escapes do not work inside code spans — the only correct
/// approach is to use a delimiter longer than the longest contiguous backtick
/// run in the content.
fn format_inline_code(s: &str) -> String {
    let mut max_run = 0usize;
    let mut run = 0usize;
    for ch in s.chars() {
        if ch == '`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(max_run + 1);
    format!("{fence}{s}{fence}")
}

pub(crate) fn format_node_entry(n: &graph::Node, index: &GraphIndex, compact: bool) -> String {
    format_node_entry_with_root(n, index, compact, None)
}

/// Format a single node, optionally stripping the root slug prefix from
/// displayed stable IDs. When `strip_root` is `Some(slug)`, the `slug:`
/// prefix is removed from the ID display -- used in single-root mode
/// where the prefix is noise.
pub(crate) fn format_node_entry_with_root(n: &graph::Node, index: &GraphIndex, compact: bool, strip_root: Option<&str>) -> String {
    let stable_id = n.stable_id();
    let display_id = strip_root_prefix(&stable_id, strip_root);

    if compact {
        // Compact: one-line summary for broad exploration
        let mut entry = format!(
            "- **{}** `{}` `{}`:{}-{}",
            n.id.kind, n.id.name,
            n.id.file.display(),
            n.line_start, n.line_end,
        );
        if !n.signature.is_empty() {
            // Truncate signature to first line for compact
            let sig_first_line = n.signature.lines().next().unwrap_or(&n.signature);
            entry.push_str(&format!(" {}", format_inline_code(sig_first_line)));
        }
        if let Some(tp) = n.metadata.get("type_params") {
            // Use safe inline-code formatting so angle brackets and backticks both render correctly.
            entry.push_str(&format!(" {}", format_inline_code(tp)));
        }
        if let Some(hint) = n.metadata.get("pattern_hint") {
            entry.push_str(&format!(" ~{}", hint));
        }
        if n.metadata.get("is_static").map(|s| s == "true").unwrap_or(false) {
            entry.push_str(" static");
        }
        if let Some(decorators) = n.metadata.get("decorators") {
            entry.push_str(&format!(" [{}]", decorators));
        }
        if let Some(storage) = n.metadata.get("storage") {
            entry.push_str(&format!(" [{}]", storage));
            if n.metadata.get("mutable").map(|s| s == "true").unwrap_or(false) {
                entry.push_str(" mut");
            }
        }
        if let Some(cc) = n.metadata.get("cyclomatic") {
            entry.push_str(&format!(" cc:{}", cc));
        }
        if let Some(imp) = n.metadata.get("importance") {
            if let Ok(score) = imp.parse::<f64>() {
                if score > IMPORTANCE_THRESHOLD {
                    entry.push_str(&format!(" imp:{:.3}", score));
                }
            }
        }
        let edge_count = index.neighbors(&stable_id, None, Direction::Outgoing).len()
            + index.neighbors(&stable_id, None, Direction::Incoming).len();
        if edge_count > 0 {
            entry.push_str(&format!(" edges:{}", edge_count));
        }
        entry.push_str(&format!("\n  `{}`", display_id));
        entry
    } else {
        // Full detail (existing format)
        let outgoing = index.neighbors(&stable_id, None, Direction::Outgoing);
        let incoming = index.neighbors(&stable_id, None, Direction::Incoming);
        let mut entry = format!(
            "- **{}** `{}` ({}) `{}`:{}-{}\n  ID: `{}`",
            n.id.kind, n.id.name, n.language,
            n.id.file.display(),
            n.line_start, n.line_end,
            display_id,
        );
        if !n.signature.is_empty() {
            entry.push_str(&format!("\n  Sig: {}", format_inline_code(&n.signature)));
        }
        if let Some(tp) = n.metadata.get("type_params") {
            entry.push_str(&format!("\n  Type params: {}", format_inline_code(tp)));
        }
        if let Some(hint) = n.metadata.get("pattern_hint") {
            entry.push_str(&format!("\n  Pattern: {}", hint));
        }
        if let Some(is_static) = n.metadata.get("is_static") {
            entry.push_str(&format!("\n  Static: {}", if is_static == "true" { "yes" } else { "no" }));
        }
        if let Some(decorators) = n.metadata.get("decorators") {
            entry.push_str(&format!("\n  Decorators: {}", decorators));
        }
        if let Some(val) = n.metadata.get("value") {
            entry.push_str(&format!("\n  Value: `{}`", val));
        }
        if n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false) {
            entry.push_str(" *(literal)*");
        }
        if let Some(storage) = n.metadata.get("storage") {
            let mut_label = if n.metadata.get("mutable").map(|s| s == "true").unwrap_or(false) {
                " (mutable)"
            } else {
                ""
            };
            entry.push_str(&format!("\n  Storage: {}{}", storage, mut_label));
        }
        if let Some(cc) = n.metadata.get("cyclomatic") {
            entry.push_str(&format!("\n  Complexity: {}", cc));
        }
        if let Some(imp) = n.metadata.get("importance") {
            if let Ok(score) = imp.parse::<f64>() {
                if score > IMPORTANCE_THRESHOLD {
                    entry.push_str(&format!("\n  Importance: {:.3}", score));
                }
            }
        }
        // Diagnostic-specific metadata (only present on NodeKind::Other("diagnostic") nodes)
        if let Some(severity) = n.metadata.get("diagnostic_severity") {
            entry.push_str(&format!("\n  Severity: {}", severity));
        }
        if let Some(source) = n.metadata.get("diagnostic_source") {
            entry.push_str(&format!("\n  Source: {}", source));
        }
        if let Some(msg) = n.metadata.get("diagnostic_message") {
            entry.push_str(&format!("\n  Message: {}", msg));
        }
        if let Some(range) = n.metadata.get("diagnostic_range") {
            entry.push_str(&format!("\n  Range: {}", range));
        }
        if let Some(ts) = n.metadata.get("diagnostic_timestamp") {
            entry.push_str(&format!("\n  Captured: {}", ts));
        }
        if !outgoing.is_empty() {
            entry.push_str(&format!("\n  Out: {} edge(s)", outgoing.len()));
        }
        if !incoming.is_empty() {
            entry.push_str(&format!("\n  In: {} edge(s)", incoming.len()));
        }
        entry
    }
}

/// Node kinds that are structural scaffolding and should be hidden from traversal results.
/// These are filtered by `format_neighbors_grouped` when rendering, so we must also filter
/// them from the ID list before counting to keep the reported count accurate.
pub(crate) fn is_hidden_traversal_kind(kind: &graph::NodeKind) -> bool {
    matches!(kind, graph::NodeKind::Module | graph::NodeKind::PrMerge)
}

/// Format traversal results grouped by edge type.
///
/// Each edge type gets a section header (e.g., `#### Calls (3)`) followed by
/// the node entries. Edge types with zero results after hidden-kind filtering
/// are omitted.
#[allow(dead_code)]  // used by tests
pub(crate) fn format_neighbors_grouped(
    nodes: &[graph::Node],
    groups: &std::collections::BTreeMap<graph::EdgeKind, Vec<String>>,
    index: &GraphIndex,
    compact: bool,
) -> String {
    format_neighbors_grouped_with_root(nodes, groups, index, compact, None)
}

/// Format traversal results grouped by edge type, with optional root slug stripping.
///
/// When `strip_root` is `Some(slug)`, the root prefix is stripped from displayed
/// stable IDs and unresolved nodes are rendered as `**name** (file)` instead of
/// raw stable IDs.
pub(crate) fn format_neighbors_grouped_with_root(
    nodes: &[graph::Node],
    groups: &std::collections::BTreeMap<graph::EdgeKind, Vec<String>>,
    index: &GraphIndex,
    compact: bool,
    strip_root: Option<&str>,
) -> String {
    // Build O(1) lookup map: stable_id -> index into nodes vec.
    let node_map: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.stable_id(), i))
        .collect();

    let mut sections: Vec<String> = Vec::new();

    for (edge_kind, ids) in groups {
        // Filter out hidden traversal kinds (Module, PrMerge)
        let displayable_ids: Vec<&String> = ids
            .iter()
            .filter(|id| {
                node_map.get(id.as_str())
                    .map(|&i| !is_hidden_traversal_kind(&nodes[i].id.kind))
                    .unwrap_or(true)
            })
            .collect();

        if displayable_ids.is_empty() {
            continue;
        }

        let entries: Vec<String> = displayable_ids
            .iter()
            .map(|id| {
                if let Some(&i) = node_map.get(id.as_str()) {
                    format_node_entry_with_root(&nodes[i], index, compact, strip_root)
                } else {
                    format_unresolved_id(id, strip_root)
                }
            })
            .collect();

        // Capitalize the edge kind for display (e.g., "calls" -> "Calls")
        let kind_str = edge_kind.to_string();
        let kind_display = capitalize_first(&kind_str);

        sections.push(format!(
            "#### {} ({})\n\n{}",
            kind_display,
            displayable_ids.len(),
            entries.join("\n"),
        ));
    }

    sections.join("\n\n")
}

/// Capitalize the first character of a string and replace underscores with spaces.
///
/// Example: `depends_on` -> `Depends on`, `calls` -> `Calls`
fn capitalize_first(s: &str) -> String {
    let s = s.replace('_', " ");
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Format neighbor nodes as a flat list (non-grouped). Available for contexts
/// where edge-type grouping is not needed (e.g., outcome_progress display).
#[allow(dead_code)]
pub(crate) fn format_neighbor_nodes(nodes: &[graph::Node], ids: &[String], index: &GraphIndex, compact: bool) -> String {
    // Build O(1) lookup map: stable_id -> index into nodes vec.
    let node_map: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.stable_id(), i))
        .collect();

    ids.iter()
        .filter_map(|id| {
            if let Some(&i) = node_map.get(id.as_str()) {
                if is_hidden_traversal_kind(&nodes[i].id.kind) {
                    return None;
                }
                Some(format_node_entry(&nodes[i], index, compact))
            } else {
                Some(format!("- `{}`", id))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::graph::{Node, NodeId, NodeKind, ExtractionSource};

    #[test]
    fn test_format_freshness_without_lsp_status() {
        let result = format_freshness(100, None, None);
        assert!(result.contains("100 symbols"));
        assert!(result.contains("never"));
    }

    #[test]
    fn test_format_freshness_with_pending_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        let result = format_freshness(50, Some(std::time::Instant::now()), Some(&status));
        assert!(result.contains("LSP: enriching"), "expected 'LSP: enriching' in: {}", result);
    }

    #[test]
    fn test_format_freshness_with_enriched_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_complete(10);
        let result = format_freshness(50, Some(std::time::Instant::now()), Some(&status));
        assert!(result.contains("10 edges"), "expected '10 edges' in: {}", result);
    }

    #[test]
    fn test_format_freshness_with_unavailable_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        let result = format_freshness(
            50,
            Some(std::time::Instant::now()),
            Some(&status),
        );
        assert!(result.contains("LSP: no server detected"));
    }

    #[test]
    fn test_parse_args_none_arguments_returns_empty_object() {
        #[derive(serde::Deserialize)]
        struct Empty {
            #[serde(default)]
            _field: Option<String>,
        }
        let result: Result<Empty, _> = parse_args(None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_args_empty_map() {
        #[derive(serde::Deserialize)]
        struct Empty {}
        let result: Result<Empty, _> = parse_args(Some(serde_json::Map::new()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_args_extra_fields_ignored() {
        #[derive(serde::Deserialize)]
        struct Minimal {
            #[serde(default)]
            name: Option<String>,
        }
        let mut map = serde_json::Map::new();
        map.insert("name".to_string(), serde_json::Value::String("test".to_string()));
        map.insert("extra".to_string(), serde_json::Value::Number(42.into()));
        let result: Result<Minimal, _> = parse_args(Some(map));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, Some("test".to_string()));
    }

    fn make_test_node(name: &str) -> Node {
        Node {
            id: NodeId {
                root: "test".to_string(),
                file: std::path::PathBuf::from("src/test.rs"),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 10,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn test_format_node_entry_compact_vs_full() {
        let node = make_test_node("my_func");
        let index = GraphIndex::new();
        let compact = format_node_entry(&node, &index, true);
        let full = format_node_entry(&node, &index, false);

        // Compact should be shorter
        assert!(compact.len() < full.len());
        // Both should contain the function name
        assert!(compact.contains("my_func"));
        assert!(full.contains("my_func"));
        // Full should contain ID: prefix
        assert!(full.contains("ID:"));
        // Compact should not contain ID: prefix (uses different format)
        assert!(!compact.contains("ID:"));
    }

    #[test]
    fn test_format_node_entry_compact_multiline_signature() {
        let mut node = make_test_node("complex_fn");
        node.signature = "fn complex_fn(\n    a: i32,\n    b: String,\n) -> Result<()>".to_string();
        let index = GraphIndex::new();
        let compact = format_node_entry(&node, &index, true);
        // Compact should only show first line of signature
        assert!(compact.contains("fn complex_fn("));
        assert!(!compact.contains("a: i32"));
    }

    #[test]
    fn test_is_static_display_compact_and_full() {
        let index = GraphIndex::new();

        // Static method: is_static = "true"
        let mut static_node = make_test_node("create");
        static_node.metadata.insert("is_static".to_string(), "true".to_string());
        let compact = format_node_entry(&static_node, &index, true);
        let full = format_node_entry(&static_node, &index, false);
        assert!(compact.contains(" static"), "compact should show 'static' tag, got: {}", compact);
        assert!(full.contains("Static: yes"), "full should show 'Static: yes', got: {}", full);

        // Instance method: is_static = "false"
        let mut instance_node = make_test_node("serve");
        instance_node.metadata.insert("is_static".to_string(), "false".to_string());
        let compact = format_node_entry(&instance_node, &index, true);
        let full = format_node_entry(&instance_node, &index, false);
        assert!(!compact.contains(" static"), "compact should NOT show 'static' for instance method, got: {}", compact);
        assert!(full.contains("Static: no"), "full should show 'Static: no', got: {}", full);

        // Top-level function: no is_static metadata
        let top_level = make_test_node("main");
        let compact = format_node_entry(&top_level, &index, true);
        let full = format_node_entry(&top_level, &index, false);
        assert!(!compact.contains("static"), "compact should NOT show 'static' for top-level function, got: {}", compact);
        assert!(!full.contains("Static:"), "full should NOT show 'Static:' for top-level function, got: {}", full);
    }

    #[test]
    fn test_format_neighbors_grouped_basic() {
        use crate::graph::EdgeKind;

        let node_a = make_test_node("callee_a");
        let node_b = make_test_node("callee_b");
        let node_c = make_test_node("dep_c");
        let nodes = vec![node_a.clone(), node_b.clone(), node_c.clone()];

        let mut index = GraphIndex::new();
        index.ensure_node(&node_a.stable_id(), "function");
        index.ensure_node(&node_b.stable_id(), "function");
        index.ensure_node(&node_c.stable_id(), "function");

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(EdgeKind::Calls, vec![node_a.stable_id(), node_b.stable_id()]);
        groups.insert(EdgeKind::DependsOn, vec![node_c.stable_id()]);

        let result = format_neighbors_grouped(&nodes, &groups, &index, true);

        assert!(result.contains("#### Calls (2)"), "should have Calls header, got: {}", result);
        assert!(result.contains("#### Depends on (1)"), "should have DependsOn header, got: {}", result);
        assert!(result.contains("callee_a"), "should contain callee_a");
        assert!(result.contains("callee_b"), "should contain callee_b");
        assert!(result.contains("dep_c"), "should contain dep_c");
    }

    #[test]
    fn test_format_neighbors_grouped_omits_empty() {
        use crate::graph::EdgeKind;

        let node_a = make_test_node("only_node");
        let nodes = vec![node_a.clone()];

        let mut index = GraphIndex::new();
        index.ensure_node(&node_a.stable_id(), "function");

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(EdgeKind::Calls, vec![node_a.stable_id()]);
        // Empty group should not appear
        groups.insert(EdgeKind::DependsOn, vec![]);

        let result = format_neighbors_grouped(&nodes, &groups, &index, true);

        assert!(result.contains("#### Calls (1)"), "should have Calls header");
        assert!(!result.contains("Depends on"), "should not have empty DependsOn section");
    }

    #[test]
    fn test_format_neighbors_grouped_hides_module_nodes() {
        use crate::graph::EdgeKind;

        let mut module_node = make_test_node("my_module");
        module_node.id.kind = graph::NodeKind::Module;
        let func_node = make_test_node("my_func");
        let nodes = vec![module_node.clone(), func_node.clone()];

        let mut index = GraphIndex::new();
        index.ensure_node(&module_node.stable_id(), "module");
        index.ensure_node(&func_node.stable_id(), "function");

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(EdgeKind::Defines, vec![module_node.stable_id(), func_node.stable_id()]);

        let result = format_neighbors_grouped(&nodes, &groups, &index, true);

        // Module node should be hidden; only func_node counted
        assert!(result.contains("#### Defines (1)"), "should count only displayable nodes, got: {}", result);
        assert!(result.contains("my_func"), "should contain my_func");
        assert!(!result.contains("my_module"), "should not contain hidden module node");
    }

    #[test]
    fn test_format_neighbors_grouped_compact_vs_full() {
        use crate::graph::EdgeKind;

        let node = make_test_node("test_fn");
        let nodes = vec![node.clone()];

        let mut index = GraphIndex::new();
        index.ensure_node(&node.stable_id(), "function");

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(EdgeKind::Calls, vec![node.stable_id()]);

        let compact = format_neighbors_grouped(&nodes, &groups, &index, true);
        let full = format_neighbors_grouped(&nodes, &groups, &index, false);

        // Both should have the section header
        assert!(compact.contains("#### Calls (1)"));
        assert!(full.contains("#### Calls (1)"));
        // Full should be longer (more detail per node)
        assert!(full.len() > compact.len(), "full should be more detailed than compact");
    }

    // ── strip_root_prefix tests ─────────────────────────────────────

    #[test]
    fn test_strip_root_prefix_none() {
        let id = "my-root:src/lib.rs:main:function";
        assert_eq!(strip_root_prefix(id, None), id);
    }

    #[test]
    fn test_strip_root_prefix_matching() {
        let id = "my-root:src/lib.rs:main:function";
        assert_eq!(
            strip_root_prefix(id, Some("my-root")),
            "src/lib.rs:main:function"
        );
    }

    #[test]
    fn test_strip_root_prefix_non_matching() {
        let id = "other-root:src/lib.rs:main:function";
        assert_eq!(
            strip_root_prefix(id, Some("my-root")),
            "other-root:src/lib.rs:main:function"
        );
    }

    #[test]
    fn test_strip_root_prefix_long_slug() {
        let id = "users-muness1-src-open-horizon-labs-repo-native-alignment:src/scanner.rs:scan:function";
        assert_eq!(
            strip_root_prefix(id, Some("users-muness1-src-open-horizon-labs-repo-native-alignment")),
            "src/scanner.rs:scan:function"
        );
    }

    // ── format_unresolved_id tests ──────────────────────────────────

    #[test]
    fn test_format_unresolved_id_with_strip() {
        let id = "my-root:.oh/guardrails/dogfood.md:dogfood-rna-tools:markdown_section";
        let result = format_unresolved_id(id, Some("my-root"));
        assert_eq!(result, "- **dogfood-rna-tools** (.oh/guardrails/dogfood.md)");
    }

    #[test]
    fn test_format_unresolved_id_without_strip() {
        // Without stripping, the full root:file prefix is preserved for disambiguation
        let id = "my-root:.oh/guardrails/dogfood.md:dogfood-rna-tools:markdown_section";
        let result = format_unresolved_id(id, None);
        assert!(result.contains("dogfood-rna-tools"), "should contain name, got: {}", result);
        // Without strip_root, the root prefix is preserved in the file path
        assert!(result.contains("my-root:.oh/guardrails/dogfood.md"), "should contain root:file for disambiguation, got: {}", result);
    }

    #[test]
    fn test_format_unresolved_id_no_colon() {
        let id = "some-id-without-colons";
        let result = format_unresolved_id(id, None);
        assert_eq!(result, "- `some-id-without-colons`");
    }

    #[test]
    fn test_format_unresolved_id_cross_root_correct_parse() {
        // In multi-root mode (no strip), root prefix is preserved for disambiguation
        let id = "other-root:src/lib.rs:main:function";
        let result = format_unresolved_id(id, None);
        assert_eq!(result, "- **main** (other-root:src/lib.rs)");
    }

    // ── format_node_entry_with_root tests ───────────────────────────

    #[test]
    fn test_format_node_entry_with_root_strips_prefix() {
        let node = make_test_node("my_func");
        let index = GraphIndex::new();
        let full = format_node_entry_with_root(&node, &index, false, Some("test"));
        // The stable ID should NOT start with "test:" in the display
        assert!(full.contains("ID: `src/test.rs:my_func:function`"), "got: {}", full);
    }

    #[test]
    fn test_format_node_entry_with_root_compact_strips_prefix() {
        let node = make_test_node("my_func");
        let index = GraphIndex::new();
        let compact = format_node_entry_with_root(&node, &index, true, Some("test"));
        // The trailing stable ID line should not have the root prefix
        assert!(compact.contains("`src/test.rs:my_func:function`"), "got: {}", compact);
        assert!(!compact.contains("`test:src/"), "root prefix should be stripped, got: {}", compact);
    }

    // ── format_freshness_full with embedding status ─────────────────

    #[test]
    fn test_format_freshness_with_embedding_building() {
        let embed_status = super::super::state::EmbeddingStatus::default();
        embed_status.set_building(5000);
        embed_status.set_progress(1200);
        let result = format_freshness_full(100, None, None, Some(&embed_status));
        assert!(result.contains("embedding... (1200/5000)"), "got: {}", result);
    }

    #[test]
    fn test_format_freshness_with_embedding_complete() {
        let embed_status = super::super::state::EmbeddingStatus::default();
        embed_status.set_complete(4500);
        let result = format_freshness_full(100, None, None, Some(&embed_status));
        assert!(result.contains("4500 embedded"), "got: {}", result);
    }

    #[test]
    fn test_format_freshness_no_embedding_status() {
        let result = format_freshness_full(100, None, None, None);
        assert!(!result.contains("embedding"), "got: {}", result);
        assert!(!result.contains("embedded"), "got: {}", result);
    }
}
