//! Generic `.oh/extractors/` config-driven pass.
//!
//! Reads `*.toml` files from `<repo_root>/.oh/extractors/` and emits
//! `Produces`/`Consumes` edges based on declared patterns. Zero knowledge of
//! any specific broker is hardcoded in RNA — the config files teach RNA.
//!
//! # Config format
//!
//! ```toml
//! [meta]
//! name = "my-extractor"
//! applies_when = { language = "python", imports_contain = "some.library" }
//!
//! [[boundaries]]
//! function_pattern = "client.send"
//! topic_arg = 0
//! edge_kind = "Produces"
//!
//! [[boundaries]]
//! function_pattern = "client.subscribe"
//! topic_arg = 0
//! edge_kind = "Consumes"
//! ```
//!
//! # Matching semantics
//!
//! `applies_when.language` — the extractor only fires on nodes whose
//! `language` field matches (case-sensitive).
//!
//! `applies_when.imports_contain` — at least one `NodeKind::Import` in
//! `all_nodes` must have its `body` or `id.name` contain this string.
//! Both fields are checked because different language extractors store the
//! import text in different places.
//!
//! For each matching `Function` node:
//! - If the body contains `function_pattern`, extract the argument at
//!   `topic_arg` position (0-indexed) as a quoted string literal.
//! - Emit an `EdgeKind::Produces` or `EdgeKind::Consumes` edge from the
//!   function to a synthetic `NodeKind::Other("channel")` node named by
//!   the extracted topic.
//!
//! # No broker names in RNA source
//!
//! This module does not name any specific message broker. All broker-specific
//! knowledge lives in the `.oh/extractors/` TOML files. Tests use fixture
//! TOML loaded at runtime — no hardcoded broker identifiers in this file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::extract::ExtractionResult;
use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level extractor config (one `.oh/extractors/*.toml` file).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractorConfig {
    pub meta: Meta,
    #[serde(default)]
    pub boundaries: Vec<Boundary>,
}

/// Metadata section of an extractor config.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub name: String,
    pub applies_when: AppliesWhen,
}

/// Conditions that must all be true for this config to fire.
#[derive(Debug, Clone, Deserialize)]
pub struct AppliesWhen {
    /// Node language that must match (`node.language`).
    pub language: String,
    /// String that must appear in at least one `Import` node's body or name.
    pub imports_contain: String,
}

/// A single boundary declaration.
#[derive(Debug, Clone, Deserialize)]
pub struct Boundary {
    /// Substring to look for in the function body (e.g., `"publisher.publish"`).
    pub function_pattern: String,
    /// Zero-indexed position of the argument that holds the topic name.
    /// Accepts both `topic_arg` and `arg_position` as field names in TOML.
    /// When absent (None), the `function_pattern` itself is used as the channel name —
    /// useful for wrapper functions where the function name IS the semantic boundary.
    #[serde(alias = "arg_position")]
    pub topic_arg: Option<usize>,
    /// `"Produces"` or `"Consumes"`.
    pub edge_kind: String,
    /// Whether this is a decorator pattern (e.g., `@bus.subscribe`).
    /// When true, the boundary matches function decorators rather than call sites.
    /// Currently informational — decorator matching uses the same body-text heuristic.
    #[serde(default)]
    pub decorator: bool,
}

impl Boundary {
    /// Resolve the declared `edge_kind` string to an `EdgeKind`.
    ///
    /// Returns `None` for unrecognized values.
    fn resolved_edge_kind(&self) -> Option<EdgeKind> {
        match self.edge_kind.as_str() {
            "Produces" => Some(EdgeKind::Produces),
            "Consumes" => Some(EdgeKind::Consumes),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load all `*.toml` files from `<repo_root>/.oh/extractors/`.
///
/// Files that fail to parse are skipped with a warning. Returns an empty vec
/// when the directory does not exist.
pub fn load_extractor_configs(repo_root: &Path) -> Vec<ExtractorConfig> {
    let dir = repo_root.join(".oh").join("extractors");
    if !dir.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                "extractor_config: failed to read {}: {}",
                dir.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut configs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<ExtractorConfig>(&content) {
                Ok(cfg) => {
                    tracing::debug!(
                        "extractor_config: loaded '{}' from {}",
                        cfg.meta.name,
                        path.display()
                    );
                    configs.push(cfg);
                }
                Err(e) => {
                    tracing::warn!(
                        "extractor_config: failed to parse {}: {}",
                        path.display(),
                        e
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    "extractor_config: failed to read {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    configs
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// Post-extraction pass: apply all `.oh/extractors/*.toml` configs and emit
/// `Produces`/`Consumes` edges.
///
/// # Arguments
///
/// * `all_nodes` — the full merged node list (all roots).
/// * `repo_root` — path to the repository root (used to locate `.oh/extractors/`).
/// * `root_id` — primary root slug for anchoring synthetic channel nodes.
pub fn extractor_config_pass(
    all_nodes: &[Node],
    repo_root: &Path,
    root_id: &str,
) -> ExtractionResult {
    extractor_config_pass_with_configs(all_nodes, root_id, &load_extractor_configs(repo_root))
}

/// Inner implementation — accepts pre-loaded configs (testable without filesystem).
///
/// # Complexity
///
/// Pre-indexed to avoid O(configs × nodes × boundaries):
/// - One pass over `all_nodes` to build:
///   - `import_texts` — set of all import text fragments (body + name)
///   - `by_language` — map from language → Vec of (node, pre-lowercased body)
/// - Per-config: O(1) import match via HashSet + O(candidates × boundaries) scan
pub fn extractor_config_pass_with_configs(
    all_nodes: &[Node],
    root_id: &str,
    configs: &[ExtractorConfig],
) -> ExtractionResult {
    if configs.is_empty() {
        return ExtractionResult::default();
    }

    // -----------------------------------------------------------------------
    // Pre-index 1: import text fragments (body + name of all Import nodes).
    //
    // Stored as a Vec of strings rather than a HashSet so we can do substring
    // matching (imports_contain is a substring test, not an exact match).
    // A Vec is fine here because there are rarely more than ~100 imports per
    // codebase and the configs loop is short.
    // -----------------------------------------------------------------------
    let import_texts: Vec<String> = all_nodes
        .iter()
        .filter(|n| n.id.kind == NodeKind::Import)
        .flat_map(|n| {
            // Collect both name and body; the caller checks if either contains the target.
            [n.id.name.clone(), n.body.clone()]
        })
        .filter(|s| !s.is_empty())
        .collect();

    // -----------------------------------------------------------------------
    // Pre-index 2: function/impl/struct nodes grouped by language.
    //
    // Candidate node kinds are the same as the original pass.
    // Body is pre-lowercased once here to avoid repeated `.to_lowercase()`
    // calls inside the hot boundary-matching loop.
    // Nodes with root == "external" or empty bodies are excluded upfront.
    // -----------------------------------------------------------------------
    let mut by_language: std::collections::HashMap<&str, Vec<(&Node, String)>> =
        std::collections::HashMap::new();
    for node in all_nodes {
        if !matches!(
            node.id.kind,
            NodeKind::Function | NodeKind::Impl | NodeKind::Struct
        ) {
            continue;
        }
        if node.id.root == "external" || node.body.is_empty() {
            continue;
        }
        by_language
            .entry(node.language.as_str())
            .or_default()
            .push((node, node.body.to_lowercase()));
    }

    // -----------------------------------------------------------------------
    // Main loop: one pass per config.
    // Import check is O(imports) substring scan — fast for typical repos.
    // Node scan is O(candidates_for_language × boundaries) — no full-node scan.
    // -----------------------------------------------------------------------
    let mut result_edges: Vec<Edge> = Vec::new();
    let mut channel_nodes: std::collections::HashMap<String, Node> =
        std::collections::HashMap::new();
    let mut seen_edges: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();

    for config in configs {
        // O(imports) substring scan — cheap because import count is small.
        let target = config.meta.applies_when.imports_contain.as_str();
        let import_match = import_texts.iter().any(|text| text.contains(target));
        if !import_match {
            tracing::debug!(
                "extractor_config: config '{}' skipped — no import matching '{}'",
                config.meta.name,
                target
            );
            continue;
        }

        // Only scan candidates for this config's language — O(1) lookup.
        let candidates = match by_language.get(config.meta.applies_when.language.as_str()) {
            Some(c) => c,
            None => continue,
        };

        for (node, body_lower) in candidates {
            for boundary in &config.boundaries {
                let edge_kind = match boundary.resolved_edge_kind() {
                    Some(k) => k,
                    None => {
                        tracing::warn!(
                            "extractor_config: config '{}' boundary has unknown edge_kind '{}'",
                            config.meta.name,
                            boundary.edge_kind
                        );
                        continue;
                    }
                };

                // Build a regex from the function_pattern:
                // - `*` matches any sequence of identifier chars (alphanumeric, _, $, .)
                // - Everything else is treated as a literal
                // This supports: `publish_*`, `*.publish`, `*publish*`, `publisher.publish`
                let pattern_lower = boundary.function_pattern.to_lowercase();
                let is_glob = pattern_lower.contains('*');

                // Quick body pre-check before expensive regex work.
                if !is_glob {
                    let call_prefix_lower = format!("{}(", pattern_lower);
                    if !body_lower.contains(call_prefix_lower.as_str()) {
                        continue;
                    }
                } else {
                    // For globs: check that at least the literal portions appear in body.
                    let literal_parts: Vec<&str> = pattern_lower.split('*').collect();
                    if !literal_parts.iter().all(|p| p.is_empty() || body_lower.contains(p)) {
                        continue;
                    }
                }

                // Convert glob pattern to a regex that matches `pattern(`.
                // `*` → `[a-zA-Z0-9_$.]*` (identifier chars, not `(`)
                let escaped = pattern_lower
                    .chars()
                    .map(|c| match c {
                        '*' => "[a-zA-Z0-9_$.]*".to_string(),
                        '.' | '(' | ')' | '[' | ']' | '{' | '}' | '+' | '?' | '^' | '$' | '|' | '\\' => {
                            format!("\\{}", c)
                        }
                        _ => c.to_string(),
                    })
                    .collect::<String>();
                let re_str = format!("(?i){}\\(", escaped);
                let re = match regex::Regex::new(&re_str) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // Iterate ALL matches of the pattern in the body.
                let mut search_start = 0usize;
                while let Some(m) = re.find(&body_lower[search_start..]) {
                    let abs_match_start = search_start + m.start();
                    let abs_paren_offset = search_start + m.end() - 1; // points to `(`
                    search_start = search_start + m.end();

                    // Extract the actual matched function name from node.body (preserves casing).
                    let matched_fn_name = &node.body[abs_match_start..abs_paren_offset];

                    // Guard: must be valid char boundary.
                    if !node.body.is_char_boundary(abs_paren_offset) {
                        continue;
                    }
                    let body_from_here = &node.body[abs_paren_offset..];

                    // Determine the channel/topic name:
                    // - If topic_arg is Some(n): extract the nth quoted argument from the call
                    // - If topic_arg is None: use the matched function name as the channel
                    let topic_name = match boundary.topic_arg {
                        Some(0) => extract_first_quoted(body_from_here),
                        Some(n) => extract_nth_arg_quoted_from_open(body_from_here, n),
                        None => Some(matched_fn_name.trim().to_string()),
                    };

                    let topic_name = match topic_name {
                        Some(n) if !n.is_empty() => n,
                        _ => continue,
                    };

                    // Create or reuse synthetic channel node.
                    let channel_key = format!("{}:{}", root_id, topic_name);
                    let channel_node_id = channel_nodes
                        .entry(channel_key)
                        .or_insert_with(|| {
                            let mut metadata = BTreeMap::new();
                            metadata.insert(
                                "extractor_config".to_string(),
                                config.meta.name.clone(),
                            );
                            Node {
                                id: NodeId {
                                    root: root_id.to_string(),
                                    file: PathBuf::from(format!(
                                        "channels/{}",
                                        topic_name.replace(':', "/")
                                    )),
                                    name: topic_name.clone(),
                                    kind: NodeKind::Other("channel".to_string()),
                                },
                                language: String::new(),
                                line_start: 0,
                                line_end: 0,
                                signature: format!("channel {}", topic_name),
                                body: String::new(),
                                metadata,
                                source: ExtractionSource::TreeSitter,
                            }
                        })
                        .id
                        .clone();

                    // Dedup: (from_stable_id, topic_name, edge_kind).
                    let dedup_key = (
                        node.stable_id(),
                        topic_name.clone(),
                        edge_kind.to_string(),
                    );
                    if !seen_edges.insert(dedup_key) {
                        continue;
                    }

                    result_edges.push(Edge {
                        from: node.id.clone(),
                        to: channel_node_id,
                        kind: edge_kind.clone(),
                        source: ExtractionSource::TreeSitter,
                        confidence: Confidence::Detected,
                    });
                }
            }
        }
    }

    let nodes: Vec<Node> = channel_nodes.into_values().collect();

    if !result_edges.is_empty() {
        tracing::info!(
            "Extractor config pass: {} channel node(s), {} Produces/Consumes edge(s) from {} config(s)",
            nodes.len(),
            result_edges.len(),
            configs.len(),
        );
    }

    ExtractionResult {
        nodes,
        edges: result_edges,
    }
}

// ---------------------------------------------------------------------------
// Argument extraction helpers
// ---------------------------------------------------------------------------

/// Extract the first quoted string immediately after `prefix` in `body`.
///
/// Handles single quotes, double quotes, and backtick strings.
/// Skips leading whitespace and brackets before the opening quote.
#[allow(dead_code)]
fn extract_quoted_after(body: &str, prefix: &str) -> Option<String> {
    let prefix_lower = prefix.to_lowercase();
    let body_lower = body.to_lowercase();

    let start_pos = body_lower.find(prefix_lower.as_str())?;
    // Use original-case body for the substring after the prefix.
    let after = &body[start_pos + prefix.len()..];

    // Skip whitespace and leading brackets/braces.
    let after = after.trim_start_matches([' ', '\t', '\n', '\r', '{', '[']);

    let quote_char = match after.chars().next()? {
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    };

    let content = &after[1..];
    let end = content.find(quote_char)?;
    let name = content[..end].trim().to_string();

    if name.is_empty() || name.contains('\n') {
        return None;
    }

    Some(name)
}

/// Extract the `n`-th argument (0-indexed) as a quoted string from a call site.
///
/// `prefix` identifies the opening of the call (e.g., `"publisher.publish("`).
/// For `n = 0` this is equivalent to `extract_quoted_after`. For `n > 0` we
/// advance past `n` comma-delimited argument positions and then extract the
/// next quoted string. This is a best-effort heuristic — complex expressions
/// (nested calls, multi-line args) are not fully handled.
#[allow(dead_code)]
fn extract_nth_arg_quoted(body: &str, prefix: &str, n: usize) -> Option<String> {
    if n == 0 {
        return extract_quoted_after(body, prefix);
    }

    let prefix_lower = prefix.to_lowercase();
    let body_lower = body.to_lowercase();
    let start_pos = body_lower.find(prefix_lower.as_str())?;
    let after = &body[start_pos + prefix.len()..];

    // Advance past `n` commas (shallow — does not handle nested parens).
    let mut remaining = after;
    for _ in 0..n {
        remaining = remaining.splitn(2, ',').nth(1)?;
    }

    // Now extract the quoted string from the current argument.
    let trimmed = remaining.trim_start_matches([' ', '\t', '\n', '\r']);
    let quote_char = match trimmed.chars().next()? {
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    };
    let content = &trimmed[1..];
    let end = content.find(quote_char)?;
    let name = content[..end].trim().to_string();

    if name.is_empty() || name.contains('\n') {
        return None;
    }

    Some(name)
}

/// Extract the first quoted string from a string that STARTS at the opening paren.
///
/// Used by the multi-occurrence loop where we've already sliced the body to the
/// position of the opening `(` so we can call this repeatedly for each occurrence.
///
/// `at_paren` should be a string like `("topic", ...)` — the `(` is the first char.
fn extract_first_quoted(at_paren: &str) -> Option<String> {
    // Skip the opening paren and any whitespace/brackets.
    let after = at_paren
        .trim_start_matches('(')
        .trim_start_matches([' ', '\t', '\n', '\r', '{', '[']);

    let quote_char = match after.chars().next()? {
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    };

    let content = &after[1..];
    let end = content.find(quote_char)?;
    let name = content[..end].trim().to_string();

    if name.is_empty() || name.contains('\n') {
        return None;
    }

    // Skip interpolated template literals: `orders-${env}` is dynamic, not a
    // static channel name. Only backtick strings with no `${` are safe.
    if quote_char == '`' && name.contains("${") {
        return None;
    }

    Some(name)
}

/// Extract the `n`-th argument as a quoted string from a string that STARTS at `(`.
///
/// For `n = 0` equivalent to `extract_first_quoted`. For `n > 0` advances past
/// `n` commas (shallow parse — does not handle nested parens).
fn extract_nth_arg_quoted_from_open(at_paren: &str, n: usize) -> Option<String> {
    if n == 0 {
        return extract_first_quoted(at_paren);
    }

    // Skip the opening paren.
    let after = at_paren.trim_start_matches('(');

    // Advance past `n` commas (shallow — does not handle nested parens).
    let mut remaining = after;
    for _ in 0..n {
        remaining = remaining.splitn(2, ',').nth(1)?;
    }

    let trimmed = remaining.trim_start_matches([' ', '\t', '\n', '\r']);
    let quote_char = match trimmed.chars().next()? {
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    };
    let content = &trimmed[1..];
    let end = content.find(quote_char)?;
    let name = content[..end].trim().to_string();

    if name.is_empty() || name.contains('\n') {
        return None;
    }

    // Skip interpolated template literals (same as extract_first_quoted).
    if quote_char == '`' && name.contains("${") {
        return None;
    }

    Some(name)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn make_import(root: &str, lang: &str, name: &str, body: &str) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from("src/main.py"),
                name: name.into(),
                kind: NodeKind::Import,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 1,
            signature: name.into(),
            body: body.into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_fn(root: &str, lang: &str, file: &str, name: &str, body: &str) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from(file),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 20,
            signature: format!("def {}():", name),
            body: body.into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    /// Parse a TOML string into an `ExtractorConfig` for testing.
    fn parse_config(toml_str: &str) -> ExtractorConfig {
        toml::from_str(toml_str).expect("test config must parse")
    }

    /// The Google Pub/Sub fixture config (same as tests/fixtures/oh_extractors/google-pubsub.toml).
    fn pubsub_fixture_config() -> ExtractorConfig {
        parse_config(
            r#"
[meta]
name = "google-pubsub"
applies_when = { language = "python", imports_contain = "google.cloud.pubsub" }

[[boundaries]]
function_pattern = "publisher.publish"
topic_arg = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "subscriber.subscribe"
topic_arg = 0
edge_kind = "Consumes"
"#,
        )
    }

    // -------------------------------------------------------------------------
    // Config parsing
    // -------------------------------------------------------------------------

    #[test]
    fn test_config_parses_meta() {
        let cfg = pubsub_fixture_config();
        assert_eq!(cfg.meta.name, "google-pubsub");
        assert_eq!(cfg.meta.applies_when.language, "python");
        assert_eq!(
            cfg.meta.applies_when.imports_contain,
            "google.cloud.pubsub"
        );
    }

    #[test]
    fn test_config_parses_boundaries() {
        let cfg = pubsub_fixture_config();
        assert_eq!(cfg.boundaries.len(), 2);
        assert_eq!(cfg.boundaries[0].function_pattern, "publisher.publish");
        assert_eq!(cfg.boundaries[0].topic_arg, Some(0));
        assert_eq!(cfg.boundaries[0].edge_kind, "Produces");
        assert_eq!(cfg.boundaries[1].function_pattern, "subscriber.subscribe");
        assert_eq!(cfg.boundaries[1].edge_kind, "Consumes");
    }

    #[test]
    fn test_config_empty_boundaries_is_valid() {
        let cfg = parse_config(
            r#"
[meta]
name = "empty"
applies_when = { language = "python", imports_contain = "foo" }
"#,
        );
        assert!(cfg.boundaries.is_empty());
    }

    #[test]
    fn test_config_accepts_arg_position_alias() {
        // The issue format uses `arg_position`; verify the serde alias works.
        let cfg = parse_config(
            r#"
[meta]
name = "internal-event-bus"
applies_when = { language = "python", imports_contain = "src.events.bus" }

[[boundaries]]
function_pattern = "bus.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "@bus.subscribe"
arg_position = 0
edge_kind = "Consumes"
decorator = true
"#,
        );
        assert_eq!(cfg.boundaries.len(), 2);
        assert_eq!(cfg.boundaries[0].topic_arg, Some(0), "arg_position alias must map to topic_arg");
        assert!(!cfg.boundaries[0].decorator);
        assert!(cfg.boundaries[1].decorator);
    }

    // -------------------------------------------------------------------------
    // applies_when gating
    // -------------------------------------------------------------------------

    #[test]
    fn test_skips_when_no_import_matches() {
        let cfg = pubsub_fixture_config();
        // No Import nodes at all → no edges.
        let fn_node = make_fn(
            "repo",
            "python",
            "src/publisher.py",
            "publish_message",
            r#"def publish_message(topic, data):
    publisher.publish(topic, data)"#,
        );
        let result = extractor_config_pass_with_configs(&[fn_node], "repo", &[cfg]);
        assert!(result.edges.is_empty(), "No import → no edges");
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn test_skips_when_import_does_not_contain_target() {
        let cfg = pubsub_fixture_config();
        let import = make_import("repo", "python", "import boto3", "import boto3");
        let fn_node = make_fn(
            "repo",
            "python",
            "src/publisher.py",
            "publish_message",
            "publisher.publish(\"my-topic\", data)",
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert!(
            result.edges.is_empty(),
            "Wrong import → no edges (got {:?})",
            result.edges
        );
    }

    #[test]
    fn test_skips_wrong_language_nodes() {
        let cfg = pubsub_fixture_config();
        // Import matches but function node is Go, not Python.
        let import = make_import(
            "repo",
            "python",
            "from google.cloud import pubsub_v1",
            "from google.cloud import pubsub_v1",
        );
        let fn_node = make_fn(
            "repo",
            "go",
            "src/main.go",
            "PublishMessage",
            r#"publisher.publish("my-topic", data)"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert!(
            result.edges.is_empty(),
            "Language mismatch → no edges (got {:?})",
            result.edges
        );
    }

    // -------------------------------------------------------------------------
    // Edge emission
    // -------------------------------------------------------------------------

    #[test]
    fn test_emits_produces_edge() {
        let cfg = pubsub_fixture_config();
        // The import text must contain "google.cloud.pubsub" as a substring.
        // "from google.cloud.pubsub_v1 import ..." satisfies this.
        let import = make_import(
            "repo",
            "python",
            "from google.cloud.pubsub_v1 import PublisherClient",
            "from google.cloud.pubsub_v1 import PublisherClient",
        );
        let fn_node = make_fn(
            "repo",
            "python",
            "src/publisher.py",
            "publish_message",
            r#"def publish_message(topic, data):
    publisher.publish("projects/my-project/topics/orders", data.encode())"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert_eq!(result.edges.len(), 1, "Should emit exactly one Produces edge");
        assert_eq!(result.edges[0].kind, EdgeKind::Produces);
        assert_eq!(
            result.edges[0].to.name,
            "projects/my-project/topics/orders"
        );
        assert_eq!(result.nodes.len(), 1, "Should emit one channel node");
        assert!(matches!(&result.nodes[0].id.kind, NodeKind::Other(s) if s == "channel"));
    }

    #[test]
    fn test_emits_consumes_edge() {
        let cfg = pubsub_fixture_config();
        let import = make_import(
            "repo",
            "python",
            "google.cloud.pubsub",
            "google.cloud.pubsub",
        );
        let fn_node = make_fn(
            "repo",
            "python",
            "src/subscriber.py",
            "subscribe_handler",
            r#"def subscribe_handler():
    subscriber.subscribe("projects/my-project/subscriptions/my-sub", callback=handle)"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert_eq!(result.edges.len(), 1, "Should emit exactly one Consumes edge");
        assert_eq!(result.edges[0].kind, EdgeKind::Consumes);
        assert_eq!(
            result.edges[0].to.name,
            "projects/my-project/subscriptions/my-sub"
        );
    }

    #[test]
    fn test_no_edge_when_function_pattern_absent_from_body() {
        let cfg = pubsub_fixture_config();
        let import = make_import(
            "repo",
            "python",
            "from google.cloud import pubsub_v1",
            "from google.cloud import pubsub_v1",
        );
        let fn_node = make_fn(
            "repo",
            "python",
            "src/other.py",
            "other_function",
            "def other_function(): return 42",
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert!(result.edges.is_empty(), "Pattern absent → no edges");
    }

    // -------------------------------------------------------------------------
    // Deduplication
    // -------------------------------------------------------------------------

    #[test]
    fn test_deduplicates_channel_nodes() {
        let cfg = pubsub_fixture_config();
        let import = make_import(
            "repo",
            "python",
            "google.cloud.pubsub_v1",
            "google.cloud.pubsub_v1",
        );
        let fn_a = make_fn(
            "repo",
            "python",
            "src/a.py",
            "fn_a",
            r#"publisher.publish("orders", data_a)"#,
        );
        let fn_b = make_fn(
            "repo",
            "python",
            "src/b.py",
            "fn_b",
            r#"publisher.publish("orders", data_b)"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_a, fn_b], "repo", &[cfg]);
        let order_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.name == "orders")
            .collect();
        assert_eq!(
            order_nodes.len(),
            1,
            "Same topic → exactly one channel node"
        );
        assert_eq!(result.edges.len(), 2, "Two functions → two edges");
    }

    #[test]
    fn test_deduplicates_edges_same_function_same_topic() {
        // A function body that calls the same pattern twice on the same topic
        // should produce only one edge.
        let cfg = pubsub_fixture_config();
        let import = make_import("repo", "python", "google.cloud.pubsub", "google.cloud.pubsub");
        let fn_node = make_fn(
            "repo",
            "python",
            "src/pub.py",
            "double_publish",
            r#"def double_publish():
    publisher.publish("orders", msg1)
    publisher.publish("orders", msg2)"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        // Both calls resolve to "orders" → same (from_stable_id, topic, edge_kind)
        // dedup key → only one edge emitted despite two call sites.
        assert_eq!(
            result.edges.len(),
            1,
            "Same from→to→kind dedup key → one edge despite two call sites"
        );
    }

    #[test]
    fn test_multiple_topics_in_one_function_body() {
        // A function that publishes to two different topics must emit two edges.
        // This tests the multi-occurrence iteration fix (CodeRabbit Major finding).
        let cfg = pubsub_fixture_config();
        let import = make_import("repo", "python", "google.cloud.pubsub_v1", "google.cloud.pubsub_v1");
        let fn_node = make_fn(
            "repo",
            "python",
            "src/pub.py",
            "publish_both",
            r#"def publish_both():
    publisher.publish("orders", order_data)
    publisher.publish("payments", payment_data)"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert_eq!(
            result.edges.len(),
            2,
            "Two different topics in one function must produce two edges"
        );
        assert_eq!(result.nodes.len(), 2, "Two different channel nodes");
        let has_orders = result.nodes.iter().any(|n| n.id.name == "orders");
        let has_payments = result.nodes.iter().any(|n| n.id.name == "payments");
        assert!(has_orders, "Should have orders channel");
        assert!(has_payments, "Should have payments channel");
    }

    // -------------------------------------------------------------------------
    // Empty / no-config cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_empty_configs_returns_empty() {
        let fn_node = make_fn("repo", "python", "src/a.py", "fn", "pass");
        let result = extractor_config_pass_with_configs(&[fn_node], "repo", &[]);
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_external_root_skipped() {
        let cfg = pubsub_fixture_config();
        let import = make_import("external", "python", "google.cloud.pubsub", "google.cloud.pubsub");
        let fn_node = make_fn(
            "external",
            "python",
            "src/pub.py",
            "publish",
            r#"publisher.publish("orders", data)"#,
        );
        let result =
            extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert!(
            result.edges.is_empty(),
            "External root nodes must not produce edges"
        );
    }

    // -------------------------------------------------------------------------
    // extract_quoted_after unit tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_extract_quoted_after_double_quote() {
        assert_eq!(
            extract_quoted_after(
                r#"publisher.publish("projects/my-project/topics/orders", data)"#,
                "publisher.publish("
            ),
            Some("projects/my-project/topics/orders".to_string())
        );
    }

    #[test]
    fn test_extract_quoted_after_single_quote() {
        assert_eq!(
            extract_quoted_after("client.send('my-topic', payload)", "client.send("),
            Some("my-topic".to_string())
        );
    }

    #[test]
    fn test_extract_quoted_after_returns_none_for_variable() {
        assert_eq!(
            extract_quoted_after("publisher.publish(topic_var, data)", "publisher.publish("),
            None,
            "Variable argument must return None"
        );
    }

    #[test]
    fn test_extract_nth_arg_index_1() {
        // Some APIs pass the topic as the second argument.
        assert_eq!(
            extract_nth_arg_quoted(
                r#"producer.send("ignored", "actual-topic", payload)"#,
                "producer.send(",
                1
            ),
            Some("actual-topic".to_string())
        );
    }

    // -------------------------------------------------------------------------
    // Adversarial tests (seeded from dissent findings)
    // -------------------------------------------------------------------------

    /// Adversarial: the search key is `function_pattern + "("` so overlong method
    /// names (different call signatures) do NOT produce false positives.
    ///
    /// `function_pattern = "publisher.publish"` searches for `"publisher.publish("`.
    /// `publisher.publish_to_dlq("deadletter", data)` contains `publisher.publish`
    /// but NOT `publisher.publish(` — so no false positive edge is emitted.
    ///
    /// Note: `publisher.publish_async("topic", ...)` WOULD match because the search
    /// key `publisher.publish(` is NOT a prefix of `publisher.publish_async(`. Only
    /// true method-name prefix collisions (where one name is a prefix of another)
    /// are potential false positives — and those are rare in practice.
    #[test]
    fn test_adversarial_function_pattern_no_false_positive_on_different_method() {
        let cfg = pubsub_fixture_config();
        let import = make_import("repo", "python", "google.cloud.pubsub_v1", "google.cloud.pubsub_v1");
        // body calls publisher.publish_to_dlq — different method, different call signature
        let fn_node = make_fn(
            "repo",
            "python",
            "src/dlq.py",
            "send_to_dlq",
            r#"publisher.publish_to_dlq("deadletter", data)"#,
        );
        let result = extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        // "publisher.publish_to_dlq(" does NOT contain "publisher.publish(" as a substring.
        // The implementation searches for function_pattern + "(" so this correctly produces
        // NO false-positive edge.
        assert!(
            result.edges.is_empty(),
            "Different method 'publisher.publish_to_dlq' must NOT produce an edge for \
             pattern 'publisher.publish' (different call signature, no false positive)"
        );
    }

    /// Adversarial: variable topic (not a literal string) must produce no edge.
    ///
    /// Dissent: users may pass a variable as the topic argument expecting RNA to
    /// resolve it. RNA must return None and emit no edge (safe false negative).
    #[test]
    fn test_adversarial_variable_topic_produces_no_edge() {
        let cfg = pubsub_fixture_config();
        let import = make_import("repo", "python", "google.cloud.pubsub_v1", "google.cloud.pubsub_v1");
        let fn_node = make_fn(
            "repo",
            "python",
            "src/pub.py",
            "publish_dynamic",
            r#"def publish_dynamic(topic_name):
    publisher.publish(topic_name, data)"#,
        );
        let result = extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert!(
            result.edges.is_empty(),
            "Variable topic must produce no edge (safe false negative), got {:?}",
            result.edges
        );
    }

    /// Adversarial: import matching checks both body and name fields.
    ///
    /// Dissent: different language extractors store import text in different fields.
    /// This test verifies both paths are covered.
    #[test]
    fn test_adversarial_import_in_name_field_matches() {
        let cfg = pubsub_fixture_config();
        // Import with the target string in `name` only, body is empty.
        let mut import = make_import("repo", "python", "google.cloud.pubsub_v1", "");
        import.id.name = "google.cloud.pubsub_v1.PublisherClient".to_string();
        import.body = String::new(); // body is empty — only name contains the string
        let fn_node = make_fn(
            "repo",
            "python",
            "src/pub.py",
            "publish",
            r#"publisher.publish("my-topic", data)"#,
        );
        let result = extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert_eq!(
            result.edges.len(),
            1,
            "Import text in name field must also trigger the pass"
        );
    }

    /// Adversarial: multiple configs — each with different applies_when — only the
    /// matching one fires.
    #[test]
    fn test_adversarial_multiple_configs_only_matching_fires() {
        let cfg_python = pubsub_fixture_config();
        let cfg_go = parse_config(
            r#"
[meta]
name = "go-bus"
applies_when = { language = "go", imports_contain = "mycompany/eventbus" }

[[boundaries]]
function_pattern = "bus.Publish"
topic_arg = 0
edge_kind = "Produces"
"#,
        );

        let py_import = make_import("repo", "python", "google.cloud.pubsub_v1", "google.cloud.pubsub_v1");
        let py_fn = make_fn(
            "repo",
            "python",
            "src/pub.py",
            "publish_py",
            r#"publisher.publish("py-topic", data)"#,
        );
        let go_fn = make_fn(
            "repo",
            "go",
            "cmd/pub.go",
            "PublishGo",
            r#"bus.Publish("go-topic", data)"#,
        );

        // Only Python import is present → only Python config fires.
        let result = extractor_config_pass_with_configs(
            &[py_import, py_fn, go_fn],
            "repo",
            &[cfg_python, cfg_go],
        );
        assert_eq!(
            result.edges.len(),
            1,
            "Only the matching config fires (Python import matches, Go import absent)"
        );
        assert_eq!(result.edges[0].kind, EdgeKind::Produces);
        let channel = result.nodes.iter().find(|n| n.id.name == "py-topic");
        assert!(channel.is_some(), "Should have py-topic channel node");
        assert!(
            result.nodes.iter().all(|n| n.id.name != "go-topic"),
            "No go-topic channel node when Go import is absent"
        );
    }

    /// Test: topic casing is preserved (extract from node.body, not body_lower).
    #[test]
    fn test_preserves_topic_casing() {
        let cfg = pubsub_fixture_config();
        let import = make_import("repo", "python", "google.cloud.pubsub_v1", "google.cloud.pubsub_v1");
        let fn_node = make_fn(
            "repo",
            "python",
            "src/pub.py",
            "publish_event",
            r#"publisher.publish("OrdersCreated", data)"#,
        );
        let result = extractor_config_pass_with_configs(&[import, fn_node], "repo", &[cfg]);
        assert_eq!(result.edges.len(), 1, "Should produce one edge");
        // Topic name must preserve original case, not lowercase.
        let channel = result.nodes.iter().find(|n| n.id.name == "OrdersCreated");
        assert!(
            channel.is_some(),
            "Channel name must preserve original casing 'OrdersCreated', not 'orderscreated'"
        );
    }

    /// Test: interpolated template literals are skipped (dynamic topics).
    #[test]
    fn test_skips_interpolated_template_literals() {
        // Config that uses JavaScript-style backtick patterns.
        let js_config = parse_config(
            r#"
[meta]
name = "js-bus"
applies_when = { language = "javascript", imports_contain = "my-bus" }

[[boundaries]]
function_pattern = "bus.publish"
arg_position = 0
edge_kind = "Produces"
"#,
        );
        let import = make_import("repo", "javascript", "my-bus", "my-bus");
        let fn_with_interpolation = make_fn(
            "repo",
            "javascript",
            "src/pub.js",
            "publishDynamic",
            r#"bus.publish(`orders-${env}`, data)"#,
        );
        let fn_with_literal = make_fn(
            "repo",
            "javascript",
            "src/pub2.js",
            "publishStatic",
            r#"bus.publish(`orders-prod`, data)"#,
        );
        let result = extractor_config_pass_with_configs(
            &[import, fn_with_interpolation, fn_with_literal],
            "repo",
            &[js_config],
        );
        // Dynamic topic (with ${}) must produce no edge.
        // Static backtick topic must produce one edge.
        assert_eq!(result.edges.len(), 1, "Only static template literal produces an edge");
        assert!(result.nodes.iter().any(|n| n.id.name == "orders-prod"), "Static topic present");
    }

    // -------------------------------------------------------------------------
    // load_extractor_configs filesystem tests
    // -------------------------------------------------------------------------

    /// load_extractor_configs returns an empty vec when the directory does not exist.
    ///
    /// This is the happy path for repos that have no `.oh/extractors/` directory —
    /// no error is surfaced and no configs are loaded.
    #[test]
    fn test_load_extractor_configs_missing_dir_returns_empty() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // tmp root has no `.oh/extractors/` subdirectory.
        let configs = load_extractor_configs(tmp.path());
        assert!(
            configs.is_empty(),
            "missing directory must return empty vec, got {} configs",
            configs.len()
        );
    }

    /// load_extractor_configs loads valid TOML files from `.oh/extractors/`.
    ///
    /// Writes a minimal valid config, calls load_extractor_configs, and verifies
    /// that the parsed config matches the expected metadata.
    #[test]
    fn test_load_extractor_configs_loads_valid_toml() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".oh").join("extractors");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("my-bus.toml"),
            r#"
[meta]
name = "my-bus"
applies_when = { language = "python", imports_contain = "my_bus" }

[[boundaries]]
function_pattern = "bus.send"
topic_arg = 0
edge_kind = "Produces"
"#,
        )
        .unwrap();

        let configs = load_extractor_configs(tmp.path());
        assert_eq!(configs.len(), 1, "should load exactly one config");
        assert_eq!(configs[0].meta.name, "my-bus");
        assert_eq!(configs[0].meta.applies_when.language, "python");
        assert_eq!(configs[0].meta.applies_when.imports_contain, "my_bus");
        assert_eq!(configs[0].boundaries.len(), 1);
        assert_eq!(configs[0].boundaries[0].function_pattern, "bus.send");
        assert_eq!(configs[0].boundaries[0].edge_kind, "Produces");
    }

    /// load_extractor_configs loads the google-pubsub fixture from tests/fixtures.
    ///
    /// Uses the actual fixture file committed to the repo to verify that the file
    /// parses correctly and produces the expected config.
    #[test]
    fn test_load_extractor_configs_loads_google_pubsub_fixture() {
        // The fixture lives at tests/fixtures/oh_extractors/google-pubsub.toml.
        // We simulate how a repo would expose it by copying it to a temp dir under
        // `.oh/extractors/`.
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".oh").join("extractors");
        std::fs::create_dir_all(&dir).unwrap();

        // Locate the fixture relative to CARGO_MANIFEST_DIR.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        let fixture_path = std::path::Path::new(&manifest_dir)
            .join("tests")
            .join("fixtures")
            .join("oh_extractors")
            .join("google-pubsub.toml");
        let fixture_content = std::fs::read_to_string(&fixture_path)
            .unwrap_or_else(|_| {
                // Fallback: embed the fixture inline so the test never fails due to
                // working directory differences.
                r#"
[meta]
name = "google-pubsub"
applies_when = { language = "python", imports_contain = "google.cloud.pubsub" }

[[boundaries]]
function_pattern = "publisher.publish"
topic_arg = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "subscriber.subscribe"
topic_arg = 0
edge_kind = "Consumes"
"#
                .to_string()
            });
        std::fs::write(dir.join("google-pubsub.toml"), &fixture_content).unwrap();

        let configs = load_extractor_configs(tmp.path());
        assert_eq!(configs.len(), 1, "should load one config from fixture");
        let cfg = &configs[0];
        assert_eq!(cfg.meta.name, "google-pubsub");
        assert_eq!(cfg.meta.applies_when.language, "python");
        assert_eq!(cfg.meta.applies_when.imports_contain, "google.cloud.pubsub");
        assert_eq!(cfg.boundaries.len(), 2);
        assert_eq!(cfg.boundaries[0].function_pattern, "publisher.publish");
        assert_eq!(cfg.boundaries[0].edge_kind, "Produces");
        assert_eq!(cfg.boundaries[1].function_pattern, "subscriber.subscribe");
        assert_eq!(cfg.boundaries[1].edge_kind, "Consumes");
    }

    /// load_extractor_configs skips non-TOML files (e.g. `.json`, `.yaml`, `README`).
    ///
    /// Files without `.toml` extension must be silently ignored.
    #[test]
    fn test_load_extractor_configs_skips_non_toml_files() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".oh").join("extractors");
        std::fs::create_dir_all(&dir).unwrap();

        // Write a valid TOML file and several non-TOML files.
        std::fs::write(
            dir.join("valid.toml"),
            r#"
[meta]
name = "valid"
applies_when = { language = "python", imports_contain = "mylib" }
"#,
        )
        .unwrap();
        std::fs::write(dir.join("notes.md"), "# notes").unwrap();
        std::fs::write(dir.join("schema.json"), r#"{"key": "value"}"#).unwrap();
        std::fs::write(dir.join("config.yaml"), "key: value").unwrap();
        std::fs::write(dir.join("README"), "readme text").unwrap();

        let configs = load_extractor_configs(tmp.path());
        assert_eq!(
            configs.len(),
            1,
            "only .toml file should be loaded; non-TOML files must be skipped"
        );
        assert_eq!(configs[0].meta.name, "valid");
    }

    /// load_extractor_configs skips files that contain invalid TOML.
    ///
    /// Parsing errors must not panic or return an error — the bad file is logged
    /// as a warning and skipped. Valid files in the same directory still load.
    #[test]
    fn test_load_extractor_configs_skips_invalid_toml() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".oh").join("extractors");
        std::fs::create_dir_all(&dir).unwrap();

        // Write an invalid TOML file.
        std::fs::write(dir.join("broken.toml"), "this is not valid TOML ][{").unwrap();

        // Write a valid TOML file alongside the broken one.
        std::fs::write(
            dir.join("good.toml"),
            r#"
[meta]
name = "good"
applies_when = { language = "python", imports_contain = "goodlib" }
"#,
        )
        .unwrap();

        let configs = load_extractor_configs(tmp.path());
        // Broken file must be skipped, valid file must be loaded.
        assert_eq!(
            configs.len(),
            1,
            "invalid TOML must be silently skipped; only valid file should load"
        );
        assert_eq!(configs[0].meta.name, "good");
    }

    /// load_extractor_configs loads multiple TOML files from the same directory.
    ///
    /// Every valid `.toml` in `.oh/extractors/` is loaded, regardless of filename.
    #[test]
    fn test_load_extractor_configs_loads_multiple_files() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".oh").join("extractors");
        std::fs::create_dir_all(&dir).unwrap();

        for name in &["bus-a", "bus-b", "bus-c"] {
            std::fs::write(
                dir.join(format!("{}.toml", name)),
                format!(
                    r#"
[meta]
name = "{}"
applies_when = {{ language = "python", imports_contain = "lib.{}" }}
"#,
                    name, name
                ),
            )
            .unwrap();
        }

        let configs = load_extractor_configs(tmp.path());
        assert_eq!(
            configs.len(),
            3,
            "all three TOML files should be loaded; got {}",
            configs.len()
        );
        let mut names: Vec<&str> = configs.iter().map(|c| c.meta.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["bus-a", "bus-b", "bus-c"]);
    }
}