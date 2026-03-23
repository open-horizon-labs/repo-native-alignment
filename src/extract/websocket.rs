//! Framework-gated post-extraction pass for SSE/WebSocket event channel edges.
//!
//! Detects Socket.IO event patterns and emits:
//! - `EdgeKind::Produces` from a function node → synthetic event channel node
//!   (for `socket.emit("event", ...)` / `io.emit("event", ...)`)
//! - `EdgeKind::Consumes` from a function node → synthetic event channel node
//!   (for `socket.on("event", ...)` / `io.on("event", ...)`)
//!
//! # Supported frameworks
//!
//! - **Socket.IO (TypeScript/JavaScript)**: `socket.emit("event", ...)`,
//!   `socket.on("event", handler)`, `io.emit("event", ...)`, `io.on("event", ...)`,
//!   `socket.to("room").emit("event", ...)`, `io.to("room").emit("event", ...)`
//! - **Socket.IO (Python)**: `@socketio.on("event")`, `emit("event", ...)`,
//!   `socketio.emit("event", ...)`
//!
//! # Gating
//!
//! Call `should_run()` before running — returns `false` when `socketio` is not in
//! the detected frameworks set. This avoids O(N) body scanning on repos without
//! Socket.IO.
//!
//! # Design note
//!
//! Event names are string literals only. Dynamic event names (variables, template
//! literals) are silently skipped. This is intentional — emit static structural
//! edges only, not speculative runtime topology.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Pattern matching
// ---------------------------------------------------------------------------

struct WebSocketRule {
    /// Substring to search in node body (case-insensitive).
    body_pattern: &'static str,
    /// How to extract the event name.
    extraction: WsExtraction,
    /// Produces = emit, Consumes = on/listen.
    direction: Direction,
}

#[derive(Clone, Copy)]
enum Direction {
    Produces,
    Consumes,
}

enum WsExtraction {
    /// Extract quoted string after this prefix.
    QuotedAfter(&'static str),
}

static WEBSOCKET_RULES: &[WebSocketRule] = &[
    // TypeScript/JavaScript Socket.IO — emit patterns
    WebSocketRule {
        body_pattern: "socket.emit(",
        extraction: WsExtraction::QuotedAfter("socket.emit("),
        direction: Direction::Produces,
    },
    WebSocketRule {
        body_pattern: "io.emit(",
        extraction: WsExtraction::QuotedAfter("io.emit("),
        direction: Direction::Produces,
    },
    WebSocketRule {
        body_pattern: ".emit(",
        extraction: WsExtraction::QuotedAfter(".emit("),
        direction: Direction::Produces,
    },
    // TypeScript/JavaScript Socket.IO — on/listen patterns
    WebSocketRule {
        body_pattern: "socket.on(",
        extraction: WsExtraction::QuotedAfter("socket.on("),
        direction: Direction::Consumes,
    },
    WebSocketRule {
        body_pattern: "io.on(",
        extraction: WsExtraction::QuotedAfter("io.on("),
        direction: Direction::Consumes,
    },
    // Python Socket.IO — emit
    WebSocketRule {
        body_pattern: "socketio.emit(",
        extraction: WsExtraction::QuotedAfter("socketio.emit("),
        direction: Direction::Produces,
    },
    WebSocketRule {
        body_pattern: "emit(",
        extraction: WsExtraction::QuotedAfter("emit("),
        direction: Direction::Produces,
    },
    // Python Socket.IO — on (decorator)
    WebSocketRule {
        body_pattern: "@socketio.on(",
        extraction: WsExtraction::QuotedAfter("@socketio.on("),
        direction: Direction::Consumes,
    },
];

// ---------------------------------------------------------------------------
// Built-in event name filter
// ---------------------------------------------------------------------------

/// Well-known Socket.IO lifecycle events that are not application-level events.
/// These are filtered out to avoid noise in the graph.
static LIFECYCLE_EVENTS: &[&str] = &[
    "connect",
    "connection",
    "disconnect",
    "disconnecting",
    "error",
    "reconnect",
    "reconnect_attempt",
    "reconnecting",
    "reconnect_error",
    "reconnect_failed",
    "ping",
    "pong",
];

fn is_lifecycle_event(name: &str) -> bool {
    LIFECYCLE_EVENTS.contains(&name)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Whether the websocket pass should run given the detected framework set.
pub fn should_run(detected_frameworks: &HashSet<String>) -> bool {
    detected_frameworks.contains("socketio")
}

/// Result of the websocket pass.
pub struct WebSocketResult {
    /// Synthetic event channel nodes (one per unique event name per root).
    pub nodes: Vec<Node>,
    /// Produces and Consumes edges.
    pub edges: Vec<Edge>,
}

/// Post-extraction pass: detect Socket.IO event patterns and emit Produces/Consumes edges.
///
/// Only call this after `framework_detection_pass` has confirmed `socketio` is present
/// (check `should_run` first).
///
/// # Arguments
///
/// * `all_nodes` — the full node list (all roots).
/// * `detected_frameworks` — the set from `GraphState::detected_frameworks`.
/// * `root_id` — workspace primary root ID for anchoring synthetic event nodes.
pub fn websocket_pass(
    all_nodes: &[Node],
    detected_frameworks: &HashSet<String>,
    root_id: &str,
) -> WebSocketResult {
    if !should_run(detected_frameworks) {
        return WebSocketResult {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    }

    let mut result_edges: Vec<Edge> = Vec::new();
    let mut event_nodes: std::collections::HashMap<String, Node> =
        std::collections::HashMap::new();
    // Deduplicate edges: (from_stable_id, event_name, direction_str) → seen
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    for node in all_nodes {
        if !matches!(
            node.id.kind,
            NodeKind::Function | NodeKind::Impl | NodeKind::Struct
        ) {
            continue;
        }
        if node.id.root == "external" {
            continue;
        }
        if node.body.is_empty() {
            continue;
        }

        let body_lower = node.body.to_lowercase();

        for rule in WEBSOCKET_RULES {
            let pattern_lower = rule.body_pattern.to_lowercase();
            if !body_lower.contains(pattern_lower.as_str()) {
                continue;
            }

            // Collect ALL quoted event names for this rule — a handler body may
            // contain multiple socket.on("a", ...) / socket.emit("b", ...) calls.
            let event_names: Vec<String> = match &rule.extraction {
                WsExtraction::QuotedAfter(prefix) => extract_all_quoted_after(&node.body, prefix),
            };

            let edge_kind = match rule.direction {
                Direction::Produces => EdgeKind::Produces,
                Direction::Consumes => EdgeKind::Consumes,
            };

            for event_name in event_names {
                // Skip lifecycle events — they're not application-level topology.
                if is_lifecycle_event(&event_name) {
                    continue;
                }

                let event_key = format!("{}:{}", root_id, event_name);
                let event_id = event_nodes
                    .entry(event_key)
                    .or_insert_with(|| {
                        let mut metadata = BTreeMap::new();
                        metadata.insert("channel_type".to_string(), "socketio".to_string());
                        Node {
                            id: NodeId {
                                root: root_id.to_string(),
                                file: PathBuf::from(format!("events/{}", event_name)),
                                name: event_name.clone(),
                                kind: NodeKind::Other("event".to_string()),
                            },
                            language: String::new(),
                            line_start: 0,
                            line_end: 0,
                            signature: format!("event {}", event_name),
                            body: String::new(),
                            metadata,
                            source: ExtractionSource::TreeSitter,
                        }
                    })
                    .id
                    .clone();

                // Deduplicate: skip if we already emitted an edge for this
                // (from_node, event_name, direction) combination.
                let dedup_key = (
                    node.stable_id(),
                    event_name.clone(),
                    edge_kind.to_string(),
                );
                if !seen_edges.insert(dedup_key) {
                    continue;
                }

                result_edges.push(Edge {
                    from: node.id.clone(),
                    to: event_id,
                    kind: edge_kind.clone(),
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }

    let nodes: Vec<Node> = event_nodes.into_values().collect();

    if !result_edges.is_empty() {
        tracing::info!(
            "WebSocket pass: {} event node(s), {} Produces/Consumes edge(s)",
            nodes.len(),
            result_edges.len()
        );
    }

    WebSocketResult {
        nodes,
        edges: result_edges,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn extract_quoted_after(body: &str, prefix: &str) -> Option<String> {
    extract_all_quoted_after(body, prefix).into_iter().next()
}

/// Extract ALL quoted string arguments that immediately follow each occurrence of
/// `prefix` in `body`. A handler with multiple `socket.on("a", ...) socket.on("b", ...)`
/// calls will return `["a", "b"]` instead of just `["a"]`.
fn extract_all_quoted_after(body: &str, prefix: &str) -> Vec<String> {
    let prefix_lower = prefix.to_lowercase();
    let body_lower = body.to_lowercase();
    let prefix_len = prefix.len();

    let mut results = Vec::new();
    let mut search_start = 0usize;

    while let Some(rel_pos) = body_lower[search_start..].find(prefix_lower.as_str()) {
        let abs_pos = search_start + rel_pos;
        let after_start = abs_pos + prefix_len;
        search_start = after_start;

        // Guard: prefix_len must advance at least one byte to prevent infinite loop.
        if prefix_len == 0 {
            break;
        }

        let after = match body.get(after_start..) {
            Some(s) => s,
            None => break,
        };
        let after = after.trim_start_matches([' ', '\t', '\n', '\r']);

        let quote_char = match after.chars().next() {
            Some('"') => '"',
            Some('\'') => '\'',
            Some('`') => '`',
            _ => continue,
        };

        let content = &after[1..];
        let end = match content.find(quote_char) {
            Some(e) => e,
            None => continue,
        };
        let name = content[..end].trim().to_string();
        if name.is_empty() || name.contains('\n') {
            continue;
        }
        results.push(name);
    }

    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::path::PathBuf;

    use super::*;
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

    fn make_fn(root: &str, lang: &str, name: &str, body: &str) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from("src/server.ts"),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 20,
            signature: format!("function {}()", name),
            body: body.into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn fw(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_should_not_run_without_socketio() {
        assert!(!should_run(&fw(&[])));
        assert!(!should_run(&fw(&["kafka-python", "fastapi"])));
    }

    #[test]
    fn test_should_run_with_socketio() {
        assert!(should_run(&fw(&["socketio"])));
    }

    #[test]
    fn test_socket_emit() {
        let node = make_fn(
            "repo",
            "typescript",
            "notifyUser",
            "function notifyUser(socket) {\n  socket.emit(\"notification\", data);\n}",
        );
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect socket.emit");
        assert_eq!(result.edges[0].kind, EdgeKind::Produces);
        assert!(result.nodes.iter().any(|n| n.id.name == "notification"));
    }

    #[test]
    fn test_socket_on() {
        let node = make_fn(
            "repo",
            "typescript",
            "setupHandlers",
            "function setupHandlers(socket) {\n  socket.on(\"message\", (data) => { ... });\n}",
        );
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect socket.on");
        assert_eq!(result.edges[0].kind, EdgeKind::Consumes);
        assert!(result.nodes.iter().any(|n| n.id.name == "message"));
    }

    #[test]
    fn test_lifecycle_events_filtered() {
        let node = make_fn(
            "repo",
            "typescript",
            "setup",
            "io.on(\"connection\", (socket) => {\n  socket.on(\"disconnect\", () => {});\n});",
        );
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        // "connection" and "disconnect" are lifecycle events — must be filtered.
        assert!(
            result.nodes.iter().all(|n| !is_lifecycle_event(&n.id.name)),
            "Lifecycle events must not produce channel nodes"
        );
    }

    #[test]
    fn test_python_socketio_emit() {
        let node = make_fn(
            "repo",
            "python",
            "broadcast",
            "def broadcast(msg):\n    socketio.emit('update', {'data': msg})",
        );
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect Python socketio.emit");
        assert_eq!(result.edges[0].kind, EdgeKind::Produces);
    }

    #[test]
    fn test_python_socketio_on_decorator() {
        let node = make_fn(
            "repo",
            "python",
            "handle_message",
            "@socketio.on('chat_message')\ndef handle_message(data):\n    pass",
        );
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect @socketio.on decorator");
        assert_eq!(result.edges[0].kind, EdgeKind::Consumes);
        assert!(result.nodes.iter().any(|n| n.id.name == "chat_message"));
    }

    #[test]
    fn test_deduplication() {
        // Two handlers emitting same event → one event node, two edges.
        let node_a = make_fn(
            "repo",
            "typescript",
            "fn_a",
            "socket.emit(\"update\", data_a)",
        );
        let node_b = make_fn(
            "repo",
            "typescript",
            "fn_b",
            "socket.emit(\"update\", data_b)",
        );
        let result = websocket_pass(&[node_a, node_b], &fw(&["socketio"]), "repo");
        let update_nodes: Vec<_> = result.nodes.iter().filter(|n| n.id.name == "update").collect();
        assert_eq!(update_nodes.len(), 1, "Same event → one node");
        assert_eq!(result.edges.len(), 2, "Two functions → two edges");
    }

    #[test]
    fn test_dynamic_event_name_skipped() {
        // Variable event name — should not produce an edge.
        let node = make_fn(
            "repo",
            "typescript",
            "fn",
            "socket.emit(eventName, data)",
        );
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        assert!(result.edges.is_empty(), "Dynamic event names must not produce edges");
    }

    #[test]
    fn test_skips_external_root() {
        let mut node = make_fn("external", "typescript", "fn", "socket.emit(\"event\")");
        node.id.root = "external".into();
        let result = websocket_pass(&[node], &fw(&["socketio"]), "repo");
        assert!(result.edges.is_empty());
    }
}
