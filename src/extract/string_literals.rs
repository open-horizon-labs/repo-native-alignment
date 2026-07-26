//! Shared string-literal harvester for all tree-sitter code extractors.
//!
//! Captures string literals appearing in source code as synthetic Const nodes.
//! This surfaces cross-language literal values (e.g. `"application/json"`) so that
//! `search` can find all places a given string is used — even across language
//! boundaries.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Path;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

#[derive(Clone, Copy)]
pub struct StringLiteralConfig<'a> {
    pub language: &'a str,
    pub node_kind: &'a str,
    pub content_child: Option<&'a str>,
    pub skip_body_docstrings: bool,
}

/// Harvest single-token string literals from a tree-sitter AST as synthetic Const nodes.
///
/// Walks the entire AST looking for nodes whose kind matches `string_node_kind`.
/// The visible text value is extracted either from a named child (`content_child`)
/// or by stripping surrounding quote characters from the raw node text.
///
/// Filtering:
/// - Stripped value length must be > 3 (avoids short noise like "ok", "no").
/// - Whitespace-only values are skipped.
///
/// Deduplication: same `value` within a single file is deduplicated (at most one node per
/// unique string value per file, using the first occurrence's line number).
/// Cross-file duplicates are intentional signal.
///
/// # Arguments
/// * `root` — root node of the parsed AST
/// * `path` — file path (used for Node identity)
/// * `source` — raw source bytes
/// * `config` — language metadata, tree-sitter string node shape, and whether body
///   docstrings should be excluded from synthetic constants
/// * `nodes` — output vector to push harvested Const nodes into
pub fn harvest_string_literals(
    root: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    config: StringLiteralConfig<'_>,
    nodes: &mut Vec<Node>,
) {
    let mut seen: HashSet<String> = HashSet::new();
    harvest_rec(root, path, source, config, nodes, &mut seen);
}

fn harvest_rec(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    config: StringLiteralConfig<'_>,
    nodes: &mut Vec<Node>,
    seen: &mut HashSet<String>,
) {
    if node.kind() == config.node_kind {
        if config.skip_body_docstrings && is_body_docstring(node) {
            return;
        }
        let raw = node.utf8_text(source).unwrap_or("").trim().to_string();

        // Extract value: use named content child if specified, otherwise strip quotes
        let value = if let Some(child_kind) = config.content_child {
            let mut found = String::new();
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32)
                    && child.kind() == child_kind
                {
                    let text = child.utf8_text(source).unwrap_or("").to_string();
                    if !text.is_empty() {
                        found = text;
                        break;
                    }
                }
            }
            found
        } else {
            strip_string_quotes(&raw)
        };

        let value = value.trim().to_string();

        // Filter: len > 3 and not whitespace-only
        if value.len() > 3 && !value.trim().is_empty() {
            let line_start = node.start_position().row + 1;
            if !seen.contains(&value) {
                seen.insert(value.clone());
                let mut metadata = BTreeMap::new();
                metadata.insert("value".to_string(), value.clone());
                metadata.insert("synthetic".to_string(), "true".to_string());
                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: value.clone(),
                        kind: NodeKind::Const,
                    },
                    language: config.language.to_string(),
                    line_start,
                    line_end: node.end_position().row + 1,
                    signature: format!("\"{}\"", value),
                    body: String::new(),
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });
            }
        }

        // Do not recurse into string nodes (avoid double-counting interpolations)
        return;
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            harvest_rec(child, path, source, config, nodes, seen);
        }
    }
}

/// Return true for a string that is the first statement in a module, function, or class body.
/// The caller enables this only for language configurations whose documentation syntax uses
/// body string literals (currently Python), so generic extraction has no language-name branch.
fn is_body_docstring(node: tree_sitter::Node) -> bool {
    let Some(statement) = node.parent() else {
        return false;
    };
    if statement.kind() != "expression_statement" {
        return false;
    }
    let Some(container) = statement.parent() else {
        return false;
    };
    let Some(first_statement) = container.named_child(0) else {
        return false;
    };
    if first_statement.id() != statement.id() {
        return false;
    }
    if container.kind() == "module" {
        return true;
    }
    if container.kind() != "block" {
        return false;
    }
    container
        .parent()
        .is_some_and(|owner| matches!(owner.kind(), "function_definition" | "class_definition"))
}

/// Strip surrounding quote characters from a raw string literal text.
/// Handles: triple-quoted `"""..."""`, `'''...'''`, ` ```...``` `, `"..."`, `'...'`,
/// `` `...` ``, and Lua long-bracket `[[...]]`.
pub fn strip_string_quotes(raw: &str) -> String {
    let s = raw.trim();
    // Lua long-bracket strings: [[...]]
    if s.starts_with("[[") && s.ends_with("]]") {
        return s[2..s.len() - 2].to_string();
    }
    // Triple-quoted strings: check before single-character variants
    for triple in &[r#"""""#, "'''", "```"] {
        if s.starts_with(triple) && s.ends_with(triple) && s.len() > triple.len() * 2 {
            return s[triple.len()..s.len() - triple.len()].to_string();
        }
    }
    if s.len() >= 2 {
        let first = s.chars().next().unwrap();
        let last = s.chars().last().unwrap();
        if (first == '"' && last == '"')
            || (first == '\'' && last == '\'')
            || (first == '`' && last == '`')
        {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_string_quotes_double() {
        assert_eq!(strip_string_quotes(r#""hello world""#), "hello world");
    }

    #[test]
    fn test_strip_string_quotes_single() {
        assert_eq!(strip_string_quotes("'hello world'"), "hello world");
    }

    #[test]
    fn test_strip_string_quotes_lua_bracket() {
        assert_eq!(strip_string_quotes("[[hello world]]"), "hello world");
    }

    #[test]
    fn test_strip_string_quotes_no_quotes() {
        assert_eq!(strip_string_quotes("hello"), "hello");
    }

    #[test]
    fn test_strip_string_quotes_triple_double() {
        assert_eq!(strip_string_quotes(r#""""hello world""""#), "hello world");
    }

    #[test]
    fn test_strip_string_quotes_triple_single() {
        assert_eq!(strip_string_quotes("'''hello world'''"), "hello world");
    }

    #[test]
    fn test_strip_string_quotes_triple_backtick() {
        assert_eq!(strip_string_quotes("```hello world```"), "hello world");
    }

    #[test]
    fn test_strip_string_quotes_triple_does_not_mangle_double() {
        // A plain "x" should not be affected by triple logic
        assert_eq!(strip_string_quotes(r#""hello""#), "hello");
    }
}
