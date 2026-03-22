//! Framework-gated post-extraction pass for pub/sub boundary edges.
//!
//! Detects message broker publish/subscribe patterns and emits:
//! - `EdgeKind::Produces` from a function node → a synthetic topic/queue node
//! - `EdgeKind::Consumes` from a function node → a synthetic topic/queue node
//!
//! # Supported frameworks
//!
//! - **kafka-python** (`confluent_kafka`, `kafka-python`): `producer.produce("topic")`,
//!   `producer.send("topic")`, `consumer.subscribe(["topic"])`
//! - **KafkaJS**: `producer.send({topic: "...", ...})`, `consumer.subscribe({topic: "..."})`
//! - **Celery**: `@app.task`, `task.delay()`, `app.send_task("task_name")`,
//!   `task.apply_async(...)`
//! - **Pika (RabbitMQ)**: `channel.basic_publish(..., routing_key="queue")`,
//!   `channel.basic_consume(queue="queue")`
//!
//! # Gating
//!
//! Call `should_run()` before running this pass — it returns `false` when none of the
//! above frameworks are detected (by `framework_detection_pass`). This prevents O(N)
//! body-text scanning on repos that don't use any of these frameworks.
//!
//! # Limitations
//!
//! Body-text matching is heuristic. Complex patterns (dynamic topic names, multi-line
//! method chains) may not be detected. False positives are mitigated by requiring
//! function context (only `Function` and `Impl` bodies are scanned).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Pattern matching
// ---------------------------------------------------------------------------

/// A single pub/sub pattern rule.
struct PubSubRule {
    /// Framework ID this rule belongs to (for logging).
    framework: &'static str,
    /// Substring to search for in node body text.
    body_pattern: &'static str,
    /// How to extract the topic/queue name from the match context.
    extraction: Extraction,
    /// Direction: produces or consumes.
    direction: Direction,
}

#[derive(Clone, Copy)]
enum Direction {
    Produces,
    Consumes,
}

enum Extraction {
    /// Extract a quoted string immediately after `after_prefix` (handles both ' and ").
    QuotedAfter(&'static str),
    /// Use a fixed synthetic node name (for patterns where topic isn't static).
    FixedName(&'static str),
}

static PUBSUB_RULES: &[PubSubRule] = &[
    // -------------------------------------------------------------------------
    // kafka-python (confluent_kafka / kafka-python)
    // -------------------------------------------------------------------------
    PubSubRule {
        framework: "kafka-python",
        body_pattern: ".produce(",
        extraction: Extraction::QuotedAfter(".produce("),
        direction: Direction::Produces,
    },
    PubSubRule {
        framework: "kafka-python",
        body_pattern: ".send(",
        extraction: Extraction::QuotedAfter(".send("),
        direction: Direction::Produces,
    },
    PubSubRule {
        framework: "kafka-python",
        body_pattern: ".subscribe([",
        extraction: Extraction::QuotedAfter(".subscribe(["),
        direction: Direction::Consumes,
    },
    // -------------------------------------------------------------------------
    // KafkaJS
    // -------------------------------------------------------------------------
    PubSubRule {
        framework: "kafkajs",
        body_pattern: "producer.send(",
        extraction: Extraction::QuotedAfter("topic:"),
        direction: Direction::Produces,
    },
    PubSubRule {
        framework: "kafkajs",
        body_pattern: "consumer.subscribe(",
        extraction: Extraction::QuotedAfter("topic:"),
        direction: Direction::Consumes,
    },
    // -------------------------------------------------------------------------
    // Celery
    // -------------------------------------------------------------------------
    PubSubRule {
        framework: "celery",
        body_pattern: "@app.task",
        extraction: Extraction::FixedName("celery:task"),
        direction: Direction::Produces,
    },
    PubSubRule {
        framework: "celery",
        body_pattern: ".delay(",
        extraction: Extraction::FixedName("celery:task"),
        direction: Direction::Consumes,
    },
    PubSubRule {
        framework: "celery",
        body_pattern: ".apply_async(",
        extraction: Extraction::FixedName("celery:task"),
        direction: Direction::Consumes,
    },
    PubSubRule {
        framework: "celery",
        body_pattern: "app.send_task(",
        extraction: Extraction::QuotedAfter("app.send_task("),
        direction: Direction::Produces,
    },
    // -------------------------------------------------------------------------
    // Pika (RabbitMQ)
    // -------------------------------------------------------------------------
    PubSubRule {
        framework: "pika",
        body_pattern: "basic_publish(",
        extraction: Extraction::QuotedAfter("routing_key="),
        direction: Direction::Produces,
    },
    PubSubRule {
        framework: "pika",
        body_pattern: "basic_consume(",
        extraction: Extraction::QuotedAfter("queue="),
        direction: Direction::Consumes,
    },
    PubSubRule {
        framework: "pika",
        body_pattern: "queue_declare(",
        extraction: Extraction::QuotedAfter("queue="),
        direction: Direction::Produces,
    },
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Whether the pub/sub pass should run given the detected framework set.
///
/// Returns `true` if any of the supported frameworks are in `detected_frameworks`.
pub fn should_run(detected_frameworks: &HashSet<String>) -> bool {
    detected_frameworks.contains("kafka-python")
        || detected_frameworks.contains("kafkajs")
        || detected_frameworks.contains("celery")
        || detected_frameworks.contains("pika")
}

/// Result of the pub/sub pass.
pub struct PubSubResult {
    /// Synthetic topic/queue/task nodes (one per unique channel name per root).
    pub nodes: Vec<Node>,
    /// Produces and Consumes edges.
    pub edges: Vec<Edge>,
}

/// Post-extraction pass: detect pub/sub patterns and emit `Produces`/`Consumes` edges.
///
/// Only call this after `framework_detection_pass` has confirmed that at least one
/// supported broker framework is present (check `should_run` first).
///
/// # Arguments
///
/// * `all_nodes` — the full node list (all roots).
/// * `detected_frameworks` — the set from `GraphState::detected_frameworks`.
/// * `root_id` — workspace primary root ID for anchoring synthetic channel nodes.
pub fn pubsub_pass(
    all_nodes: &[Node],
    detected_frameworks: &HashSet<String>,
    root_id: &str,
) -> PubSubResult {
    if !should_run(detected_frameworks) {
        return PubSubResult {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    }

    // Collect relevant rules for detected frameworks only.
    let active_rules: Vec<&PubSubRule> = PUBSUB_RULES
        .iter()
        .filter(|r| detected_frameworks.contains(r.framework))
        .collect();

    if active_rules.is_empty() {
        return PubSubResult {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    }

    let mut result_edges: Vec<Edge> = Vec::new();
    // Deduplicate channel nodes by (root, channel_name).
    let mut channel_nodes: std::collections::HashMap<String, Node> =
        std::collections::HashMap::new();
    // Deduplicate edges: (from_stable_id, channel_name, direction_str) → seen
    let mut seen_edges: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();

    for node in all_nodes {
        // Only scan function/impl bodies — skip imports, constants, etc.
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

        for rule in &active_rules {
            let pattern_lower = rule.body_pattern.to_lowercase();
            if !body_lower.contains(pattern_lower.as_str()) {
                continue;
            }

            // Extract channel name.
            let channel_name = match &rule.extraction {
                Extraction::QuotedAfter(prefix) => {
                    extract_quoted_after(&node.body, prefix)
                }
                Extraction::FixedName(name) => Some(name.to_string()),
            };

            let channel_name = match channel_name {
                Some(n) if !n.is_empty() => n,
                _ => continue,
            };

            // Create or reuse synthetic channel node.
            let channel_key = format!("{}:{}", root_id, channel_name);
            let channel_id = channel_nodes
                .entry(channel_key)
                .or_insert_with(|| {
                    let mut metadata = BTreeMap::new();
                    metadata.insert("channel_type".to_string(), rule.framework.to_string());
                    Node {
                        id: NodeId {
                            root: root_id.to_string(),
                            file: PathBuf::from(format!("channels/{}", channel_name.replace(':', "/"))),
                            name: channel_name.clone(),
                            kind: NodeKind::Other("channel".to_string()),
                        },
                        language: String::new(),
                        line_start: 0,
                        line_end: 0,
                        signature: format!("channel {}", channel_name),
                        body: String::new(),
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    }
                })
                .id
                .clone();

            let edge_kind = match rule.direction {
                Direction::Produces => EdgeKind::Produces,
                Direction::Consumes => EdgeKind::Consumes,
            };

            // Deduplicate: skip if we already emitted an edge for this
            // (from_node, channel_name, direction) combination.
            let dedup_key = (
                node.stable_id(),
                channel_name.clone(),
                edge_kind.to_string(),
            );
            if !seen_edges.insert(dedup_key) {
                continue;
            }

            result_edges.push(Edge {
                from: node.id.clone(),
                to: channel_id,
                kind: edge_kind,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
            });
        }
    }

    let nodes: Vec<Node> = channel_nodes.into_values().collect();

    if !result_edges.is_empty() {
        tracing::info!(
            "Pub/sub pass: {} channel node(s), {} Produces/Consumes edge(s)",
            nodes.len(),
            result_edges.len()
        );
    }

    PubSubResult {
        nodes,
        edges: result_edges,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract the first quoted string (single or double quotes) after `prefix`.
///
/// Given body text like `producer.send("orders", ...)` and prefix `producer.send(`,
/// returns `Some("orders")`.
fn extract_quoted_after(body: &str, prefix: &str) -> Option<String> {
    let prefix_lower = prefix.to_lowercase();
    let body_lower = body.to_lowercase();

    let start_pos = body_lower.find(prefix_lower.as_str())?;
    let after = &body[start_pos + prefix.len()..];

    // Skip whitespace, braces, and array brackets before the quoted string.
    let after = after.trim_start_matches([' ', '\t', '\n', '\r', '{', '[']);

    // Find opening quote.
    let quote_char = match after.chars().next()? {
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    };

    // Extract until closing quote.
    let content = &after[1..];
    let end = content.find(quote_char)?;
    let name = content[..end].trim().to_string();

    if name.is_empty() || name.contains('\n') {
        return None;
    }

    Some(name)
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
                file: PathBuf::from("src/main.py"),
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

    fn fw(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_should_not_run_without_frameworks() {
        assert!(!should_run(&fw(&[])));
        assert!(!should_run(&fw(&["fastapi", "flask"])));
    }

    #[test]
    fn test_should_run_with_kafka_python() {
        assert!(should_run(&fw(&["kafka-python"])));
    }

    #[test]
    fn test_should_run_with_kafkajs() {
        assert!(should_run(&fw(&["kafkajs"])));
    }

    #[test]
    fn test_should_run_with_celery() {
        assert!(should_run(&fw(&["celery"])));
    }

    #[test]
    fn test_should_run_with_pika() {
        assert!(should_run(&fw(&["pika"])));
    }

    #[test]
    fn test_kafka_python_producer_send() {
        let node = make_fn(
            "repo",
            "python",
            "send_order",
            r#"def send_order(order):
    producer.send("orders", value=order)
    producer.flush()"#,
        );
        let result = pubsub_pass(&[node], &fw(&["kafka-python"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect producer.send");
        assert_eq!(result.edges[0].kind, EdgeKind::Produces);
        let channel = result.nodes.iter().find(|n| n.id.name == "orders");
        assert!(channel.is_some(), "Should create 'orders' channel node");
    }

    #[test]
    fn test_kafka_python_consumer_subscribe() {
        let node = make_fn(
            "repo",
            "python",
            "consume_orders",
            r#"def consume_orders():
    consumer.subscribe(["orders"])
    for msg in consumer:"#,
        );
        let result = pubsub_pass(&[node], &fw(&["kafka-python"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect consumer.subscribe");
        assert_eq!(result.edges[0].kind, EdgeKind::Consumes);
    }

    #[test]
    fn test_celery_delay() {
        let node = make_fn(
            "repo",
            "python",
            "trigger_task",
            r#"def trigger_task():
    process_order.delay(order_id=123)"#,
        );
        let result = pubsub_pass(&[node], &fw(&["celery"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect .delay(");
        assert_eq!(result.edges[0].kind, EdgeKind::Consumes);
    }

    #[test]
    fn test_pika_basic_publish() {
        let node = make_fn(
            "repo",
            "python",
            "publish_msg",
            r#"def publish_msg(channel, msg):
    channel.basic_publish(
        exchange='',
        routing_key='orders',
        body=msg
    )"#,
        );
        let result = pubsub_pass(&[node], &fw(&["pika"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect basic_publish");
        assert_eq!(result.edges[0].kind, EdgeKind::Produces);
        assert!(result.nodes.iter().any(|n| n.id.name == "orders"));
    }

    #[test]
    fn test_pika_basic_consume() {
        let node = make_fn(
            "repo",
            "python",
            "setup_consumer",
            r#"def setup_consumer(channel):
    channel.basic_consume(queue='orders', on_message_callback=callback)"#,
        );
        let result = pubsub_pass(&[node], &fw(&["pika"]), "repo");
        assert!(!result.edges.is_empty(), "Should detect basic_consume");
        assert_eq!(result.edges[0].kind, EdgeKind::Consumes);
    }

    #[test]
    fn test_no_match_on_empty_body() {
        let mut node = make_fn("repo", "python", "fn_with_no_body", "");
        node.body = String::new();
        let result = pubsub_pass(&[node], &fw(&["kafka-python"]), "repo");
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_skips_import_nodes() {
        let mut import_node = make_fn("repo", "python", "import kafka", "import kafka");
        import_node.id.kind = NodeKind::Import;
        import_node.body = "import kafka".into();
        let result = pubsub_pass(&[import_node], &fw(&["kafka-python"]), "repo");
        assert!(result.edges.is_empty(), "Import nodes must not be scanned");
    }

    #[test]
    fn test_deduplication_channel_nodes() {
        // Multiple functions using the same topic → one channel node.
        let node_a = make_fn(
            "repo",
            "python",
            "fn_a",
            "def fn_a(): producer.send(\"orders\", value=x)",
        );
        let node_b = make_fn(
            "repo",
            "python",
            "fn_b",
            "def fn_b(): producer.send(\"orders\", value=y)",
        );
        let result = pubsub_pass(&[node_a, node_b], &fw(&["kafka-python"]), "repo");
        let order_nodes: Vec<_> = result.nodes.iter().filter(|n| n.id.name == "orders").collect();
        assert_eq!(order_nodes.len(), 1, "Same topic must produce one channel node");
        assert_eq!(result.edges.len(), 2, "Two functions → two edges");
    }

    #[test]
    fn test_extract_quoted_after_double_quote() {
        assert_eq!(
            extract_quoted_after("producer.send(\"orders\", value=x)", "producer.send("),
            Some("orders".to_string())
        );
    }

    #[test]
    fn test_extract_quoted_after_single_quote() {
        assert_eq!(
            extract_quoted_after("channel.basic_publish(routing_key='orders')", "routing_key="),
            Some("orders".to_string())
        );
    }

    #[test]
    fn test_extract_quoted_after_no_quote_returns_none() {
        assert_eq!(
            extract_quoted_after("producer.send(topic_var, value=x)", "producer.send("),
            None,
            "Dynamic topic names (variables) must return None"
        );
    }

    #[test]
    fn test_skips_external_root_nodes() {
        let mut node = make_fn("external", "python", "fn", "producer.send(\"topic\")");
        node.id.root = "external".into();
        let result = pubsub_pass(&[node], &fw(&["kafka-python"]), "repo");
        assert!(result.edges.is_empty(), "External root nodes must be skipped");
    }
}
