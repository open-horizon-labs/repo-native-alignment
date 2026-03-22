//! Post-extraction pass that scans `Import` nodes, detects frameworks from a
//! data-driven rule table, and emits `NodeKind::Other("framework")` nodes.
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
//! # Rule sources (merged in order)
//!
//! 1. Built-in rules from `src/extract/framework_rules.toml` (embedded at compile
//!    time via `include_str!`). No framework names are hardcoded in Rust.
//! 2. User-defined rules from `.oh/extractors/*.toml` files in the workspace root.
//!    User rules are appended after built-in rules.
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
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Rule representation (data-driven)
// ---------------------------------------------------------------------------

/// A single framework detection rule, loaded from TOML.
///
/// `import_pattern` is checked against the Import node's `id.name` (case-insensitive).
#[derive(Debug, Clone, Deserialize)]
pub struct FrameworkRule {
    /// Substring to look for in the import text (original case from TOML).
    pub import_pattern: String,
    /// Pre-lowercased form of `import_pattern`. Populated after parsing via
    /// `normalize_rules()`; skipped by serde (never in TOML).
    /// Eliminates per-comparison String allocations during matching.
    #[serde(skip)]
    pub import_pattern_lower: String,
    /// Language(s) this rule applies to (empty string = all languages).
    pub language: String,
    /// Stable framework ID (e.g., "fastapi", "nextjs-app-router").
    pub framework_id: String,
    /// Human-readable display name.
    pub display_name: String,
}

/// Populate the `import_pattern_lower` field on each rule.
/// Called once after parsing (in `builtin_rules()`) and after user rule loading.
fn normalize_rules(rules: Vec<FrameworkRule>) -> Vec<FrameworkRule> {
    rules
        .into_iter()
        .map(|mut r| {
            r.import_pattern_lower = r.import_pattern.to_lowercase();
            r
        })
        .collect()
}

/// Top-level shape of a framework rules TOML file.
#[derive(Debug, Deserialize)]
struct FrameworkRulesFile {
    #[serde(default)]
    rules: Vec<FrameworkRule>,
}

// ---------------------------------------------------------------------------
// Built-in rules — embedded at compile time, zero hardcoded names in Rust
// ---------------------------------------------------------------------------

static BUILTIN_RULES_SOURCE: &str = include_str!("framework_rules.toml");

static BUILTIN_RULES: OnceLock<Vec<FrameworkRule>> = OnceLock::new();

/// Returns the built-in rules parsed from the embedded TOML.
/// Parsed once per process via `OnceLock`.
fn builtin_rules() -> &'static [FrameworkRule] {
    BUILTIN_RULES.get_or_init(|| {
        match toml::from_str::<FrameworkRulesFile>(BUILTIN_RULES_SOURCE) {
            Ok(file) => normalize_rules(file.rules),
            Err(e) => {
                // The embedded TOML is compile-time data — a parse failure is a
                // programming error, not a runtime condition.
                panic!("Failed to parse embedded framework_rules.toml: {e}");
            }
        }
    })
}

// ---------------------------------------------------------------------------
// User-defined rules loader
// ---------------------------------------------------------------------------

/// Load user-defined framework rules from `.oh/extractors/*.toml` under `root_path`.
///
/// Files that fail to parse are logged and skipped; they never panic.
/// Returns an empty vec if the directory does not exist.
fn load_user_rules(root_path: &Path) -> Vec<FrameworkRule> {
    let extractor_dir = root_path.join(".oh").join("extractors");
    let Ok(entries) = std::fs::read_dir(&extractor_dir) else {
        return Vec::new();
    };

    let mut rules = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<FrameworkRulesFile>(&text) {
                Ok(file) => {
                    tracing::debug!(
                        "Loaded {} user framework rule(s) from {}",
                        file.rules.len(),
                        path.display()
                    );
                    rules.extend(normalize_rules(file.rules));
                }
                Err(e) => {
                    tracing::warn!(
                        "Skipping {}: failed to parse as framework rules: {e}",
                        path.display()
                    );
                }
            },
            Err(e) => {
                tracing::warn!("Skipping {}: read error: {e}", path.display());
            }
        }
    }
    rules
}

/// Returns merged rules (built-in + optional user rules from `.oh/extractors/`).
///
/// Returns `Cow::Borrowed` (zero allocation) when there are no user rules,
/// and `Cow::Owned` only when user rules are present.
fn merged_rules(root_path: Option<&Path>) -> std::borrow::Cow<'static, [FrameworkRule]> {
    if let Some(path) = root_path {
        let user_rules = load_user_rules(path);
        if user_rules.is_empty() {
            std::borrow::Cow::Borrowed(builtin_rules())
        } else {
            let mut merged = builtin_rules().to_vec();
            merged.extend(user_rules);
            std::borrow::Cow::Owned(merged)
        }
    } else {
        std::borrow::Cow::Borrowed(builtin_rules())
    }
}

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
/// Merges built-in rules (from `framework_rules.toml`) with any user-defined rules
/// found under `<root_path>/.oh/extractors/`. Pass `root_path = None` to use
/// built-in rules only (e.g., in tests).
///
/// # Arguments
///
/// * `all_nodes` — the complete merged node list (all roots).
/// * `root_id` — the workspace root ID to anchor framework nodes.
/// * `root_path` — filesystem path to the workspace root (for loading user rules).
///
/// # Returns
///
/// A [`FrameworkDetectionResult`] containing new virtual framework nodes and
/// the set of detected framework IDs. The caller must extend `all_nodes` with
/// `result.nodes`. The `detected_frameworks` set is stored in `GraphState` for
/// conditional extractor gating.
pub fn framework_detection_pass(
    all_nodes: &[Node],
    root_id: &str,
    root_path: Option<&Path>,
) -> FrameworkDetectionResult {
    let combined = merged_rules(root_path);
    let rules: &[FrameworkRule] = &combined;

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

        for rule in rules {
            // Language filter: empty = any language, otherwise must match.
            if !rule.language.is_empty() && rule.language != language {
                continue;
            }
            // Use pre-lowercased pattern to avoid per-comparison allocations.
            if import_text.contains(rule.import_pattern_lower.as_str()) {
                detected.insert(rule.framework_id.clone());
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
        // Look up display name from the first matching rule.
        let display_name = rules
            .iter()
            .find(|r| r.framework_id == framework_id.as_str())
            .map(|r| r.display_name.as_str())
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
/// * `root_path` — optional filesystem path to workspace root for loading user rules.
///
/// # Returns
///
/// A `Vec<Edge>` of `UsesFramework` edges. The caller must extend `all_edges`.
pub fn subsystem_framework_aggregation_pass(
    all_nodes: &[Node],
    root_path: Option<&Path>,
) -> Vec<crate::graph::Edge> {
    use std::collections::HashMap;
    use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource};

    let combined = merged_rules(root_path);
    let rules: &[FrameworkRule] = &combined;

    // Build (member_stable_id → subsystem_name) from node metadata.
    let mut member_to_subsystem: HashMap<String, String> = HashMap::new();
    let mut subsystem_node_ids: HashMap<String, crate::graph::NodeId> = HashMap::new();
    let mut framework_node_ids: HashMap<String, crate::graph::NodeId> = HashMap::new();

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
        for rule in rules {
            if !rule.language.is_empty() && rule.language != language {
                continue;
            }
            // Use pre-lowercased pattern to avoid per-comparison allocations.
            if import_text.contains(rule.import_pattern_lower.as_str()) {
                file_frameworks
                    .entry(node.id.file.display().to_string())
                    .or_default()
                    .insert(rule.framework_id.clone());
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
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("fastapi"));
        assert!(result.nodes.iter().any(|n| n.id.name == "fastapi"));
    }

    #[test]
    fn test_detects_react() {
        let nodes = vec![make_import("repo", "typescript", "import React from 'react'")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("react"));
    }

    #[test]
    fn test_detects_nextjs_from_next_slash() {
        let nodes = vec![make_import("repo", "typescript", "import { NextRequest } from 'next/server'")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("nextjs-app-router"));
    }

    #[test]
    fn test_detects_kafkajs() {
        let nodes = vec![make_import("repo", "typescript", "import { Kafka } from 'kafkajs'")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("kafkajs"));
    }

    #[test]
    fn test_detects_kafka_python() {
        let nodes = vec![make_import("repo", "python", "from kafka import KafkaProducer")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("kafka-python"));
    }

    #[test]
    fn test_detects_celery() {
        let nodes = vec![make_import("repo", "python", "from celery import Celery")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("celery"));
    }

    #[test]
    fn test_detects_socketio() {
        let nodes = vec![make_import("repo", "typescript", "import { Server } from 'socket.io'")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("socketio"));
    }

    #[test]
    fn test_detects_gin_go() {
        let nodes = vec![make_import("repo", "go", "import \"github.com/gin-gonic/gin\"")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.detected_frameworks.contains("gin"));
    }

    #[test]
    fn test_no_false_positives_from_function_nodes() {
        // Only Import nodes should trigger detection
        let nodes = vec![make_fn("repo", "python", "fastapi_handler")];
        let result = framework_detection_pass(&nodes, "repo", None);
        // Function named fastapi_handler should NOT trigger framework detection
        assert!(!result.detected_frameworks.contains("fastapi"),
            "Function nodes must not trigger framework detection");
    }

    #[test]
    fn test_empty_input_returns_empty() {
        let result = framework_detection_pass(&[], "repo", None);
        assert!(result.nodes.is_empty());
        assert!(result.detected_frameworks.is_empty());
    }

    #[test]
    fn test_no_imports_returns_empty() {
        let nodes = vec![make_fn("repo", "python", "my_function")];
        let result = framework_detection_pass(&nodes, "repo", None);
        assert!(result.nodes.is_empty());
        assert!(result.detected_frameworks.is_empty());
    }

    #[test]
    fn test_external_root_nodes_skipped() {
        // External nodes from LSP should not trigger detection
        let node = make_import("external", "python", "from fastapi import FastAPI");
        let result = framework_detection_pass(&[node], "repo", None);
        assert!(!result.detected_frameworks.contains("fastapi"),
            "External root nodes must be skipped");
    }

    #[test]
    fn test_framework_node_metadata() {
        let nodes = vec![make_import("repo", "python", "import fastapi")];
        let result = framework_detection_pass(&nodes, "repo", None);
        let node = result.nodes.iter().find(|n| n.id.name == "fastapi").unwrap();
        assert!(node.metadata.contains_key("display_name"));
        assert_eq!(node.metadata.get("display_name").map(|s| s.as_str()), Some("FastAPI"));
    }

    #[test]
    fn test_framework_node_kind() {
        let nodes = vec![make_import("repo", "python", "import flask")];
        let result = framework_detection_pass(&nodes, "repo", None);
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
        let result = framework_detection_pass(&nodes, "repo", None);
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
        let result = framework_detection_pass(&nodes, "repo", None);
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
        let result = framework_detection_pass(&nodes, "repo", None);
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
        let result = framework_detection_pass(&nodes, "repo", None);
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
        let result = framework_detection_pass(&nodes, "repo", None);
        // Check nodes are sorted by name
        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "Framework nodes must be sorted by name for deterministic output");
    }

    #[test]
    fn test_user_rules_extend_builtin() {
        use std::io::Write;
        // Write a temp dir with a .oh/extractors/custom.toml
        let dir = tempfile::tempdir().expect("tempdir");
        let extractors_dir = dir.path().join(".oh").join("extractors");
        std::fs::create_dir_all(&extractors_dir).unwrap();
        let rule_toml = extractors_dir.join("custom.toml");
        let mut f = std::fs::File::create(&rule_toml).unwrap();
        writeln!(f, r#"
[[rules]]
import_pattern = "my_custom_lib"
language = "python"
framework_id = "my-custom"
display_name = "My Custom Lib"
"#).unwrap();

        let nodes = vec![make_import("repo", "python", "import my_custom_lib")];
        let result = framework_detection_pass(&nodes, "repo", Some(dir.path()));
        assert!(result.detected_frameworks.contains("my-custom"),
            "User-defined rules must be detected");
    }

    #[test]
    fn test_builtin_rules_parse() {
        // Ensures the embedded TOML parses without panic.
        let rules = builtin_rules();
        assert!(!rules.is_empty(), "Embedded framework_rules.toml must not be empty");
    }
}
