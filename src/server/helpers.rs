//! Formatting utilities, argument parsing, and display helpers.

use crate::graph;
use crate::graph::index::GraphIndex;
use petgraph::Direction;
use rust_mcp_sdk::schema::{CallToolError, CallToolResult, TextContent};
use super::state::LspEnrichmentStatus;

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

    format!(
        "\n\n*Index: {} symbols · last scan {} · schema v{}{}*",
        node_count,
        age,
        crate::graph::store::SCHEMA_VERSION,
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
    let stable_id = n.stable_id();

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
            entry.push_str(&format!(" `{}`", sig_first_line));
        }
        if let Some(tp) = n.metadata.get("type_params") {
            // Use safe inline-code formatting so angle brackets and backticks both render correctly.
            entry.push_str(&format!(" {}", format_inline_code(tp)));
        }
        if let Some(hint) = n.metadata.get("pattern_hint") {
            entry.push_str(&format!(" ~{}", hint));
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
        entry.push_str(&format!("\n  `{}`", stable_id));
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
            stable_id,
        );
        if !n.signature.is_empty() {
            entry.push_str(&format!("\n  Sig: `{}`", n.signature));
        }
        if let Some(tp) = n.metadata.get("type_params") {
            entry.push_str(&format!("\n  Type params: {}", format_inline_code(tp)));
        }
        if let Some(hint) = n.metadata.get("pattern_hint") {
            entry.push_str(&format!("\n  Pattern: {}", hint));
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
/// These are filtered by `format_neighbor_nodes` when rendering, so we must also filter
/// them from the ID list before counting to keep the reported count accurate.
pub(crate) fn is_hidden_traversal_kind(kind: &graph::NodeKind) -> bool {
    matches!(kind, graph::NodeKind::Module | graph::NodeKind::PrMerge)
}

/// Remove IDs whose node kind is hidden scaffolding (Module, PrMerge) from traversal results.
/// This ensures the count shown in headings matches the nodes actually rendered.
pub(crate) fn retain_displayable(all_ids: &mut Vec<String>, nodes: &[graph::Node]) {
    all_ids.retain(|id| {
        nodes.iter()
            .find(|n| n.stable_id() == *id)
            .map(|n| !is_hidden_traversal_kind(&n.id.kind))
            // If node not found, keep the ID (it will render as a fallback `id` line)
            .unwrap_or(true)
    });
}

pub(crate) fn format_neighbor_nodes(nodes: &[graph::Node], ids: &[String], index: &GraphIndex, compact: bool) -> String {
    ids.iter()
        .filter_map(|id| {
            if let Some(node) = nodes.iter().find(|n| n.stable_id() == *id) {
                if is_hidden_traversal_kind(&node.id.kind) {
                    return None;
                }
                Some(format_node_entry(node, index, compact))
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
        assert!(result.contains("LSP: pending..."));
    }

    #[test]
    fn test_format_freshness_with_enriched_lsp() {
        let status = LspEnrichmentStatus::default();
        status.set_complete(10);
        let result = format_freshness(50, Some(std::time::Instant::now()), Some(&status));
        assert!(result.contains("LSP: enriched +10 edges"));
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
}
