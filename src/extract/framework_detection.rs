//! Post-extraction pass that scans `Import` nodes, detects frameworks from a
//! lookup table, and emits `NodeKind::Other("framework")` nodes.
//!
//! # Why
//!
//! Without framework detection, every conditional extractor (pub/sub, gRPC, SSE)
//! must scan ALL files checking for relevant imports — O(N) per extractor.
//! With framework detection:
//! - One pass scans all imports once, building a `detected_frameworks` set.
//! - Conditional extractors call `has_framework("kafka-python")` and skip immediately
//!   if the framework is absent.
//! - Framework nodes are queryable: `search "" --kind framework`.
//!
//! # Detection algorithm
//!
//! Walk all `NodeKind::Import` nodes. For each node, check the import text (stored
//! in `id.name`) against a lookup table of `(pattern, framework_id)` pairs. Pattern
//! matching is case-insensitive substring/prefix matching on the import text.
//!
//! # Emitted nodes
//!
//! One `NodeKind::Other("framework")` node per detected framework, anchored to the
//! primary root with a virtual path `frameworks/<framework-id>`.
//!
//! # Placement
//!
//! Call after tree-sitter extraction (all Import nodes must be present) and before
//! conditional post-extraction passes (pub/sub, websocket, etc.). Run from both
//! `build_full_graph_inner` and `update_graph_with_scan`.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Framework lookup table
// ---------------------------------------------------------------------------

/// A single framework detection rule.
/// `import_pattern` is checked against the Import node's `id.name` (case-insensitive).
struct FrameworkRule {
    /// Substring to look for in the import text.
    import_pattern: &'static str,
    /// Language(s) this rule applies to (empty = all languages).
    language: &'static str,
    /// Stable framework ID (e.g., "fastapi", "nextjs-app-router").
    framework_id: &'static str,
    /// Human-readable display name.
    display_name: &'static str,
}

/// All framework detection rules, ordered by specificity (more specific first).
static FRAMEWORK_RULES: &[FrameworkRule] = &[
    // -------------------------------------------------------------------------
    // Python frameworks
    // -------------------------------------------------------------------------
    FrameworkRule { import_pattern: "fastapi", language: "python", framework_id: "fastapi", display_name: "FastAPI" },
    FrameworkRule { import_pattern: "flask", language: "python", framework_id: "flask", display_name: "Flask" },
    FrameworkRule { import_pattern: "django", language: "python", framework_id: "django", display_name: "Django" },
    FrameworkRule { import_pattern: "celery", language: "python", framework_id: "celery", display_name: "Celery" },
    // Kafka Python: confluent-kafka uses `confluent_kafka`, kafka-python uses `kafka`
    FrameworkRule { import_pattern: "confluent_kafka", language: "python", framework_id: "kafka-python", display_name: "Kafka (confluent-kafka)" },
    FrameworkRule { import_pattern: "kafka", language: "python", framework_id: "kafka-python", display_name: "Kafka (kafka-python)" },
    FrameworkRule { import_pattern: "pika", language: "python", framework_id: "pika", display_name: "Pika (RabbitMQ)" },
    FrameworkRule { import_pattern: "redis", language: "python", framework_id: "redis", display_name: "Redis" },
    FrameworkRule { import_pattern: "sqlalchemy", language: "python", framework_id: "sqlalchemy", display_name: "SQLAlchemy" },
    FrameworkRule { import_pattern: "aiohttp", language: "python", framework_id: "aiohttp", display_name: "aiohttp" },
    FrameworkRule { import_pattern: "tornado", language: "python", framework_id: "tornado", display_name: "Tornado" },
    FrameworkRule { import_pattern: "starlette", language: "python", framework_id: "starlette", display_name: "Starlette" },
    FrameworkRule { import_pattern: "socketio", language: "python", framework_id: "socketio", display_name: "Socket.IO (Python)" },
    FrameworkRule { import_pattern: "socket.io", language: "python", framework_id: "socketio", display_name: "Socket.IO (Python)" },
    FrameworkRule { import_pattern: "grpc", language: "python", framework_id: "grpc-python", display_name: "gRPC (Python)" },
    FrameworkRule { import_pattern: "boto3", language: "python", framework_id: "boto3", display_name: "AWS SDK (boto3)" },
    FrameworkRule { import_pattern: "pymongo", language: "python", framework_id: "pymongo", display_name: "PyMongo" },
    FrameworkRule { import_pattern: "httpx", language: "python", framework_id: "httpx", display_name: "HTTPX" },
    FrameworkRule { import_pattern: "pytest", language: "python", framework_id: "pytest", display_name: "pytest" },
    FrameworkRule { import_pattern: "pydantic", language: "python", framework_id: "pydantic", display_name: "Pydantic" },
    FrameworkRule { import_pattern: "langchain", language: "python", framework_id: "langchain", display_name: "LangChain" },
    FrameworkRule { import_pattern: "openai", language: "python", framework_id: "openai", display_name: "OpenAI SDK" },
    FrameworkRule { import_pattern: "anthropic", language: "python", framework_id: "anthropic", display_name: "Anthropic SDK" },

    // -------------------------------------------------------------------------
    // TypeScript / JavaScript frameworks
    // -------------------------------------------------------------------------
    // Next.js — must check 'next/server', 'next/router', etc. before generic 'react'
    FrameworkRule { import_pattern: "next/", language: "typescript", framework_id: "nextjs-app-router", display_name: "Next.js" },
    FrameworkRule { import_pattern: "next/", language: "javascript", framework_id: "nextjs-app-router", display_name: "Next.js" },
    FrameworkRule { import_pattern: "\"next\"", language: "typescript", framework_id: "nextjs-app-router", display_name: "Next.js" },
    FrameworkRule { import_pattern: "'next'", language: "javascript", framework_id: "nextjs-app-router", display_name: "Next.js" },
    // React (after Next.js to avoid clobbering)
    FrameworkRule { import_pattern: "react", language: "typescript", framework_id: "react", display_name: "React" },
    FrameworkRule { import_pattern: "react", language: "javascript", framework_id: "react", display_name: "React" },
    // Express
    FrameworkRule { import_pattern: "express", language: "typescript", framework_id: "express", display_name: "Express.js" },
    FrameworkRule { import_pattern: "express", language: "javascript", framework_id: "express", display_name: "Express.js" },
    // Kafka JS
    FrameworkRule { import_pattern: "kafkajs", language: "typescript", framework_id: "kafkajs", display_name: "KafkaJS" },
    FrameworkRule { import_pattern: "kafkajs", language: "javascript", framework_id: "kafkajs", display_name: "KafkaJS" },
    // Socket.IO
    FrameworkRule { import_pattern: "socket.io", language: "typescript", framework_id: "socketio", display_name: "Socket.IO" },
    FrameworkRule { import_pattern: "socket.io", language: "javascript", framework_id: "socketio", display_name: "Socket.IO" },
    // TanStack Query (formerly React Query)
    FrameworkRule { import_pattern: "@tanstack/", language: "typescript", framework_id: "tanstack-query", display_name: "TanStack Query" },
    FrameworkRule { import_pattern: "@tanstack/", language: "javascript", framework_id: "tanstack-query", display_name: "TanStack Query" },
    FrameworkRule { import_pattern: "react-query", language: "typescript", framework_id: "tanstack-query", display_name: "TanStack Query" },
    FrameworkRule { import_pattern: "react-query", language: "javascript", framework_id: "tanstack-query", display_name: "TanStack Query" },
    // Redis JS
    FrameworkRule { import_pattern: "ioredis", language: "typescript", framework_id: "redis", display_name: "Redis (ioredis)" },
    FrameworkRule { import_pattern: "ioredis", language: "javascript", framework_id: "redis", display_name: "Redis (ioredis)" },
    FrameworkRule { import_pattern: "redis", language: "typescript", framework_id: "redis", display_name: "Redis" },
    FrameworkRule { import_pattern: "redis", language: "javascript", framework_id: "redis", display_name: "Redis" },
    // GraphQL
    FrameworkRule { import_pattern: "graphql", language: "typescript", framework_id: "graphql", display_name: "GraphQL" },
    FrameworkRule { import_pattern: "graphql", language: "javascript", framework_id: "graphql", display_name: "GraphQL" },
    // Prisma
    FrameworkRule { import_pattern: "@prisma/", language: "typescript", framework_id: "prisma", display_name: "Prisma" },
    FrameworkRule { import_pattern: "@prisma/", language: "javascript", framework_id: "prisma", display_name: "Prisma" },
    // Typeorm
    FrameworkRule { import_pattern: "typeorm", language: "typescript", framework_id: "typeorm", display_name: "TypeORM" },
    FrameworkRule { import_pattern: "typeorm", language: "javascript", framework_id: "typeorm", display_name: "TypeORM" },
    // gRPC JS
    FrameworkRule { import_pattern: "@grpc/", language: "typescript", framework_id: "grpc-js", display_name: "gRPC (Node.js)" },
    FrameworkRule { import_pattern: "@grpc/", language: "javascript", framework_id: "grpc-js", display_name: "gRPC (Node.js)" },
    // Fastify
    FrameworkRule { import_pattern: "fastify", language: "typescript", framework_id: "fastify", display_name: "Fastify" },
    FrameworkRule { import_pattern: "fastify", language: "javascript", framework_id: "fastify", display_name: "Fastify" },
    // NestJS
    FrameworkRule { import_pattern: "@nestjs/", language: "typescript", framework_id: "nestjs", display_name: "NestJS" },
    FrameworkRule { import_pattern: "@nestjs/", language: "javascript", framework_id: "nestjs", display_name: "NestJS" },
    // Remix
    FrameworkRule { import_pattern: "@remix-run/", language: "typescript", framework_id: "remix", display_name: "Remix" },
    FrameworkRule { import_pattern: "@remix-run/", language: "javascript", framework_id: "remix", display_name: "Remix" },
    // Svelte
    FrameworkRule { import_pattern: "svelte", language: "typescript", framework_id: "svelte", display_name: "Svelte" },
    FrameworkRule { import_pattern: "svelte", language: "javascript", framework_id: "svelte", display_name: "Svelte" },
    // Vue
    FrameworkRule { import_pattern: "vue", language: "typescript", framework_id: "vue", display_name: "Vue.js" },
    FrameworkRule { import_pattern: "vue", language: "javascript", framework_id: "vue", display_name: "Vue.js" },
    // OpenAI JS
    FrameworkRule { import_pattern: "openai", language: "typescript", framework_id: "openai", display_name: "OpenAI SDK" },
    FrameworkRule { import_pattern: "openai", language: "javascript", framework_id: "openai", display_name: "OpenAI SDK" },
    FrameworkRule { import_pattern: "@anthropic-ai/", language: "typescript", framework_id: "anthropic", display_name: "Anthropic SDK" },
    FrameworkRule { import_pattern: "@anthropic-ai/", language: "javascript", framework_id: "anthropic", display_name: "Anthropic SDK" },

    // -------------------------------------------------------------------------
    // Go frameworks
    // -------------------------------------------------------------------------
    FrameworkRule { import_pattern: "gin-gonic/gin", language: "go", framework_id: "gin", display_name: "Gin (Go)" },
    FrameworkRule { import_pattern: "labstack/echo", language: "go", framework_id: "echo", display_name: "Echo (Go)" },
    FrameworkRule { import_pattern: "gorilla/mux", language: "go", framework_id: "gorilla-mux", display_name: "Gorilla Mux" },
    FrameworkRule { import_pattern: "confluent-kafka-go", language: "go", framework_id: "kafka-go", display_name: "Kafka (Go)" },
    FrameworkRule { import_pattern: "segmentio/kafka-go", language: "go", framework_id: "kafka-go", display_name: "Kafka (Go)" },
    FrameworkRule { import_pattern: "gofiber/fiber", language: "go", framework_id: "fiber", display_name: "Fiber (Go)" },
    FrameworkRule { import_pattern: "go-redis/redis", language: "go", framework_id: "redis", display_name: "Redis (Go)" },
    FrameworkRule { import_pattern: "grpc-ecosystem", language: "go", framework_id: "grpc-go", display_name: "gRPC (Go)" },
    FrameworkRule { import_pattern: "google.golang.org/grpc", language: "go", framework_id: "grpc-go", display_name: "gRPC (Go)" },
    FrameworkRule { import_pattern: "gorm.io/gorm", language: "go", framework_id: "gorm", display_name: "GORM" },

    // -------------------------------------------------------------------------
    // Rust frameworks
    // -------------------------------------------------------------------------
    FrameworkRule { import_pattern: "actix-web", language: "rust", framework_id: "actix-web", display_name: "Actix-Web" },
    FrameworkRule { import_pattern: "actix_web", language: "rust", framework_id: "actix-web", display_name: "Actix-Web" },
    FrameworkRule { import_pattern: "axum", language: "rust", framework_id: "axum", display_name: "Axum" },
    FrameworkRule { import_pattern: "warp", language: "rust", framework_id: "warp", display_name: "Warp" },
    FrameworkRule { import_pattern: "rocket", language: "rust", framework_id: "rocket", display_name: "Rocket" },
    FrameworkRule { import_pattern: "tokio", language: "rust", framework_id: "tokio", display_name: "Tokio" },
    FrameworkRule { import_pattern: "tonic", language: "rust", framework_id: "tonic", display_name: "Tonic (gRPC)" },
    FrameworkRule { import_pattern: "rdkafka", language: "rust", framework_id: "rdkafka", display_name: "rdkafka" },
    FrameworkRule { import_pattern: "lancedb", language: "rust", framework_id: "lancedb", display_name: "LanceDB" },
    FrameworkRule { import_pattern: "sqlx", language: "rust", framework_id: "sqlx", display_name: "SQLx" },
    FrameworkRule { import_pattern: "diesel", language: "rust", framework_id: "diesel", display_name: "Diesel ORM" },
];

// ---------------------------------------------------------------------------
// Public return type
// ---------------------------------------------------------------------------

/// Result of the framework detection pass.
pub struct FrameworkDetectionResult {
    /// Virtual `NodeKind::Other("framework")` nodes, one per detected framework.
    pub nodes: Vec<Node>,
    /// Set of detected framework IDs for gating conditional extractors.
    pub detected_frameworks: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Post-extraction pass: scan Import nodes, detect frameworks, emit framework nodes.
///
/// # Arguments
///
/// * `all_nodes` — the complete merged node list (all roots).
/// * `root_id` — the workspace root ID to anchor framework nodes.
///
/// # Returns
///
/// A [`FrameworkDetectionResult`] containing new virtual framework nodes and
/// the set of detected framework IDs. The caller must extend `all_nodes` with
/// `result.nodes`. The `detected_frameworks` set is stored in `GraphState` for
/// conditional extractor gating.
pub fn framework_detection_pass(all_nodes: &[Node], root_id: &str) -> FrameworkDetectionResult {
    let mut detected: HashSet<String> = HashSet::new();

    for node in all_nodes {
        if node.id.kind != NodeKind::Import {
            continue;
        }
        // Skip external imports (from LSP virtual nodes).
        if node.id.root == "external" {
            continue;
        }

        let import_text = node.id.name.to_lowercase();
        let language = node.language.as_str();

        for rule in FRAMEWORK_RULES {
            // Language filter: empty = any language, otherwise must match.
            if !rule.language.is_empty() && rule.language != language {
                continue;
            }
            let pattern = rule.import_pattern.to_lowercase();
            if import_text.contains(pattern.as_str()) {
                detected.insert(rule.framework_id.to_string());
            }
        }
    }

    if detected.is_empty() {
        return FrameworkDetectionResult {
            nodes: Vec::new(),
            detected_frameworks: HashSet::new(),
        };
    }

    // Emit one framework node per detected framework.
    let mut nodes: Vec<Node> = Vec::with_capacity(detected.len());
    for framework_id in &detected {
        // Look up display name.
        let display_name = FRAMEWORK_RULES
            .iter()
            .find(|r| r.framework_id == framework_id.as_str())
            .map(|r| r.display_name)
            .unwrap_or(framework_id.as_str());

        let mut metadata = BTreeMap::new();
        metadata.insert("display_name".to_string(), display_name.to_string());

        nodes.push(Node {
            id: NodeId {
                root: root_id.to_string(),
                file: PathBuf::from(format!("frameworks/{}", framework_id)),
                name: framework_id.clone(),
                kind: NodeKind::Other("framework".to_string()),
            },
            language: String::new(),
            line_start: 0,
            line_end: 0,
            signature: format!("framework {}", framework_id),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }

    // Sort for deterministic output.
    nodes.sort_by(|a, b| a.id.name.cmp(&b.id.name));

    tracing::info!(
        "Framework detection: {} framework(s) detected: [{}]",
        nodes.len(),
        nodes.iter().map(|n| n.id.name.as_str()).collect::<Vec<_>>().join(", ")
    );

    FrameworkDetectionResult {
        nodes,
        detected_frameworks: detected,
    }
}

// ---------------------------------------------------------------------------
// Subsystem → framework aggregation (Phase 2b)
// ---------------------------------------------------------------------------

/// Threshold for subsystem→framework edge emission.
/// If ≥ SUBSYSTEM_FRAMEWORK_THRESHOLD fraction of a subsystem's members share a
/// framework, emit a `UsesFramework` edge from the subsystem node to the framework node.
const SUBSYSTEM_FRAMEWORK_THRESHOLD: f64 = 0.70;

/// Emit `EdgeKind::UsesFramework` edges from subsystem nodes to framework nodes.
///
/// For each subsystem node, inspects the subsystem metadata field on member symbols
/// to count which frameworks are used by its members. If ≥ 70% of members share a
/// framework, emits a `UsesFramework` edge from the subsystem node to the framework node.
///
/// # Arguments
///
/// * `all_nodes` — the complete merged node list (all roots, including subsystem nodes
///   and framework nodes emitted by earlier passes).
///
/// # Returns
///
/// A `Vec<Edge>` of `UsesFramework` edges. The caller must extend `all_edges`.
pub fn subsystem_framework_aggregation_pass(all_nodes: &[Node]) -> Vec<crate::graph::Edge> {
    use std::collections::HashMap;
    use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource};

    // Build (member_stable_id → subsystem_name) from node metadata.
    let mut member_to_subsystem: HashMap<String, String> = HashMap::new();
    let mut subsystem_node_ids: HashMap<String, crate::graph::NodeId> = HashMap::new();
    let mut framework_node_ids: HashMap<String, crate::graph::NodeId> = HashMap::new();

    // Also: (member_stable_id → set of framework_ids) from Import nodes.
    // We need to count how many members of each subsystem use each framework.
    // Strategy: for each Import node, detect its frameworks (re-use detection logic),
    // then map to the import's parent_scope or file to associate with a member.

    // Simpler approach that works with available data:
    // 1. Collect (file → [framework_id]) from Import node analysis.
    // 2. Collect (member stable_id → file) from all non-subsystem nodes.
    // 3. For each subsystem, count members' files → frameworks.

    // Step 1: file → set of frameworks detected in that file.
    let mut file_frameworks: HashMap<String, HashSet<String>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind != NodeKind::Import {
            continue;
        }
        if node.id.root == "external" {
            continue;
        }
        let import_text = node.id.name.to_lowercase();
        let language = node.language.as_str();
        for rule in FRAMEWORK_RULES {
            if !rule.language.is_empty() && rule.language != language {
                continue;
            }
            let pattern = rule.import_pattern.to_lowercase();
            if import_text.contains(pattern.as_str()) {
                file_frameworks
                    .entry(node.id.file.display().to_string())
                    .or_default()
                    .insert(rule.framework_id.to_string());
            }
        }
    }

    // Step 2: collect subsystem / framework node IDs.
    for node in all_nodes {
        match &node.id.kind {
            NodeKind::Other(s) if s == "subsystem" => {
                subsystem_node_ids.insert(node.id.name.clone(), node.id.clone());
            }
            NodeKind::Other(s) if s == "framework" => {
                framework_node_ids.insert(node.id.name.clone(), node.id.clone());
            }
            _ => {}
        }
        // Track member→subsystem (from metadata).
        if let Some(sub) = node.metadata.get("subsystem") {
            member_to_subsystem.insert(node.stable_id(), sub.clone());
        }
    }

    if subsystem_node_ids.is_empty() || framework_node_ids.is_empty() {
        return Vec::new();
    }

    // Step 3: for each subsystem, count members' frameworks.
    // (subsystem_name → (framework_id → count))
    let mut subsystem_framework_counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut subsystem_member_counts: HashMap<String, usize> = HashMap::new();

    for node in all_nodes {
        // Skip virtual nodes.
        if matches!(&node.id.kind, NodeKind::Other(_)) {
            continue;
        }
        if node.id.root == "external" {
            continue;
        }
        let sid = node.stable_id();
        if let Some(sub_name) = member_to_subsystem.get(&sid) {
            *subsystem_member_counts.entry(sub_name.clone()).or_default() += 1;
            let file_key = node.id.file.display().to_string();
            if let Some(frameworks) = file_frameworks.get(&file_key) {
                let sub_entry = subsystem_framework_counts
                    .entry(sub_name.clone())
                    .or_default();
                for fw in frameworks {
                    *sub_entry.entry(fw.clone()).or_default() += 1;
                }
            }
        }
    }

    // Step 4: emit UsesFramework edges where threshold is met.
    let mut edges: Vec<Edge> = Vec::new();
    for (sub_name, fw_counts) in &subsystem_framework_counts {
        let total = *subsystem_member_counts.get(sub_name).unwrap_or(&1);
        let sub_node_id = match subsystem_node_ids.get(sub_name) {
            Some(id) => id,
            None => continue,
        };
        for (fw_id, count) in fw_counts {
            let fraction = *count as f64 / total as f64;
            if fraction >= SUBSYSTEM_FRAMEWORK_THRESHOLD {
                if let Some(fw_node_id) = framework_node_ids.get(fw_id) {
                    edges.push(Edge {
                        from: sub_node_id.clone(),
                        to: fw_node_id.clone(),
                        kind: EdgeKind::UsesFramework,
                        source: ExtractionSource::TreeSitter,
                        confidence: Confidence::Detected,
                    });
                    tracing::debug!(
                        "Subsystem '{}' uses framework '{}' ({:.0}% of {} members)",
                        sub_name,
                        fw_id,
                        fraction * 100.0,
                        total
                    );
                }
            }
        }
    }

    if !edges.is_empty() {
        tracing::info!(
            "Subsystem-framework aggregation: {} UsesFramework edge(s) emitted",
            edges.len()
        );
    }

    edges
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

    fn make_import(root: &str, lang: &str, import_text: &str) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from("src/main.py"),
                name: import_text.into(),
                kind: NodeKind::Import,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 1,
            signature: import_text.into(),
            body: import_text.into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_fn(root: &str, lang: &str, name: &str) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from("src/main.py"),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 10,
            signature: format!("def {}():", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn test_detects_fastapi() {
        let nodes = vec![make_import("repo", "python", "from fastapi import FastAPI")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("fastapi"));
        assert!(result.nodes.iter().any(|n| n.id.name == "fastapi"));
    }

    #[test]
    fn test_detects_react() {
        let nodes = vec![make_import("repo", "typescript", "import React from 'react'")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("react"));
    }

    #[test]
    fn test_detects_nextjs_from_next_slash() {
        let nodes = vec![make_import("repo", "typescript", "import { NextRequest } from 'next/server'")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("nextjs-app-router"));
    }

    #[test]
    fn test_detects_kafkajs() {
        let nodes = vec![make_import("repo", "typescript", "import { Kafka } from 'kafkajs'")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("kafkajs"));
    }

    #[test]
    fn test_detects_kafka_python() {
        let nodes = vec![make_import("repo", "python", "from kafka import KafkaProducer")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("kafka-python"));
    }

    #[test]
    fn test_detects_celery() {
        let nodes = vec![make_import("repo", "python", "from celery import Celery")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("celery"));
    }

    #[test]
    fn test_detects_socketio() {
        let nodes = vec![make_import("repo", "typescript", "import { Server } from 'socket.io'")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("socketio"));
    }

    #[test]
    fn test_detects_gin_go() {
        let nodes = vec![make_import("repo", "go", "import \"github.com/gin-gonic/gin\"")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("gin"));
    }

    #[test]
    fn test_no_false_positives_from_function_nodes() {
        // Only Import nodes should trigger detection
        let nodes = vec![make_fn("repo", "python", "fastapi_handler")];
        let result = framework_detection_pass(&nodes, "repo");
        // Function named fastapi_handler should NOT trigger framework detection
        assert!(!result.detected_frameworks.contains("fastapi"),
            "Function nodes must not trigger framework detection");
    }

    #[test]
    fn test_empty_input_returns_empty() {
        let result = framework_detection_pass(&[], "repo");
        assert!(result.nodes.is_empty());
        assert!(result.detected_frameworks.is_empty());
    }

    #[test]
    fn test_no_imports_returns_empty() {
        let nodes = vec![make_fn("repo", "python", "my_function")];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.nodes.is_empty());
        assert!(result.detected_frameworks.is_empty());
    }

    #[test]
    fn test_external_root_nodes_skipped() {
        // External nodes from LSP should not trigger detection
        let node = make_import("external", "python", "from fastapi import FastAPI");
        let result = framework_detection_pass(&[node], "repo");
        assert!(!result.detected_frameworks.contains("fastapi"),
            "External root nodes must be skipped");
    }

    #[test]
    fn test_framework_node_metadata() {
        let nodes = vec![make_import("repo", "python", "import fastapi")];
        let result = framework_detection_pass(&nodes, "repo");
        let node = result.nodes.iter().find(|n| n.id.name == "fastapi").unwrap();
        assert!(node.metadata.contains_key("display_name"));
        assert_eq!(node.metadata.get("display_name").map(|s| s.as_str()), Some("FastAPI"));
    }

    #[test]
    fn test_framework_node_kind() {
        let nodes = vec![make_import("repo", "python", "import flask")];
        let result = framework_detection_pass(&nodes, "repo");
        let node = result.nodes.iter().find(|n| n.id.name == "flask").unwrap();
        assert!(matches!(&node.id.kind, NodeKind::Other(s) if s == "framework"));
        assert_eq!(node.id.file, PathBuf::from("frameworks/flask"));
    }

    #[test]
    fn test_deduplication_across_files() {
        // Multiple files importing the same framework → one framework node
        let mut nodes = vec![];
        for i in 0..5 {
            nodes.push(Node {
                id: NodeId {
                    root: "repo".into(),
                    file: PathBuf::from(format!("src/module{}.py", i)),
                    name: "from fastapi import FastAPI".into(),
                    kind: NodeKind::Import,
                },
                language: "python".into(),
                line_start: 1,
                line_end: 1,
                signature: "from fastapi import FastAPI".into(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            });
        }
        let result = framework_detection_pass(&nodes, "repo");
        let fastapi_nodes: Vec<_> = result.nodes.iter().filter(|n| n.id.name == "fastapi").collect();
        assert_eq!(fastapi_nodes.len(), 1, "Multiple imports of same framework must produce one node");
    }

    #[test]
    fn test_language_filter_prevents_cross_language_false_positives() {
        // "kafka" in Python matches kafka-python; "kafka" in Go should NOT match
        // (Go uses specific import paths like "github.com/confluentinc/confluent-kafka-go")
        let nodes = vec![
            // Generic "kafka" in Go — should not match kafka-python rule (Python-only)
            make_import("repo", "go", "kafka"),
        ];
        let result = framework_detection_pass(&nodes, "repo");
        // kafka-python rule has language="python", so Go imports should not match it.
        assert!(!result.detected_frameworks.contains("kafka-python"),
            "kafka-python rule must not fire on Go imports");
    }

    #[test]
    fn test_multiple_frameworks_detected() {
        let nodes = vec![
            make_import("repo", "python", "from fastapi import FastAPI"),
            make_import("repo", "python", "from celery import Celery"),
            make_import("repo", "python", "import redis"),
        ];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("fastapi"));
        assert!(result.detected_frameworks.contains("celery"));
        assert!(result.detected_frameworks.contains("redis"));
        assert_eq!(result.nodes.len(), 3);
    }

    #[test]
    fn test_tanstack_query_detection() {
        let nodes = vec![
            make_import("repo", "typescript", "import { useQuery } from '@tanstack/react-query'"),
        ];
        let result = framework_detection_pass(&nodes, "repo");
        assert!(result.detected_frameworks.contains("tanstack-query"),
            "TanStack Query must be detected from @tanstack/ imports");
    }

    #[test]
    fn test_output_sorted_deterministically() {
        let nodes = vec![
            make_import("repo", "python", "import redis"),
            make_import("repo", "python", "import fastapi"),
            make_import("repo", "python", "from celery import Celery"),
        ];
        let result = framework_detection_pass(&nodes, "repo");
        // Check nodes are sorted by name
        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "Framework nodes must be sorted by name for deterministic output");
    }
}
