//! LanceDB persistence: persist, load, schema migration, stale root pruning.
//!
//! ## Module structure
//!
//! - `migrate` -- schema migration and error classification
//! - `batch` -- Arrow RecordBatch builders for symbols and edges tables
//! - `persist` -- full persist, incremental upsert, compaction, root pruning
//! - `load` -- graph loading from LanceDB tables

mod batch;
pub(crate) mod load;
pub(crate) mod metadata_keys;
pub(crate) mod migrate;
pub(crate) mod persist;

use std::path::{Path, PathBuf};

use lancedb::expr::{DfExpr, col, is_in, lit};

use crate::graph::{Confidence, EdgeKind, ExtractionSource, NodeId, NodeKind};

// ── Re-exports ────────────────────────────────────────────────────────

// From migrate
pub(crate) use migrate::check_and_migrate_schema;

// From persist
pub(crate) use persist::{
    delete_nodes_for_roots, get_stored_root_ids, persist_graph_incremental, persist_graph_to_lance,
};

// From load
pub use load::load_graph_from_lance;

// ── Graph persistence (LanceDB) ─────────────────────────────────────

/// LanceDB path for graph persistence.
pub(crate) fn graph_lance_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("lance")
}

pub(crate) const PREDICATE_BATCH_SIZE: usize = 500;

pub(crate) fn string_isin(column: &str, values: impl IntoIterator<Item = String>) -> DfExpr {
    is_in(col(column), values.into_iter().map(lit).collect())
}

// ── Parse helpers ────────────────────────────────────────────────────

/// Parse a NodeKind from its string representation.
pub(crate) fn parse_node_kind(s: &str) -> NodeKind {
    match s {
        "function" => NodeKind::Function,
        "struct" => NodeKind::Struct,
        "trait" => NodeKind::Trait,
        "enum" => NodeKind::Enum,
        "module" => NodeKind::Module,
        "import" => NodeKind::Import,
        "const" => NodeKind::Const,
        "impl" => NodeKind::Impl,
        "proto_message" => NodeKind::ProtoMessage,
        "sql_table" => NodeKind::SqlTable,
        "api_endpoint" => NodeKind::ApiEndpoint,
        "type_alias" => NodeKind::TypeAlias,
        "macro" => NodeKind::Macro,
        "field" => NodeKind::Field,
        "pr_merge" => NodeKind::PrMerge,
        "enum_variant" => NodeKind::EnumVariant,
        "markdown_section" => NodeKind::MarkdownSection,
        other => NodeKind::Other(other.to_string()),
    }
}

/// Parse an EdgeKind from its string representation.
///
/// Built-in labels map to their canonical variants. Any non-empty unknown
/// label becomes `EdgeKind::Other(label)`, preserving repo-local relationship
/// kinds loaded from LanceDB or accepted through MCP/search edge filters.
pub fn parse_edge_kind(s: &str) -> Option<EdgeKind> {
    EdgeKind::from_label(s)
}

/// Parse an ExtractionSource from its string representation.
pub(crate) fn parse_extraction_source(s: &str) -> ExtractionSource {
    match s {
        "tree_sitter" => ExtractionSource::TreeSitter,
        "lsp" => ExtractionSource::Lsp,
        "schema" => ExtractionSource::Schema,
        "git" => ExtractionSource::Git,
        "markdown" => ExtractionSource::Markdown,
        _ => {
            tracing::warn!(
                "Unknown extraction source value: {}, defaulting to TreeSitter",
                s
            );
            ExtractionSource::TreeSitter
        }
    }
}

/// Parse a Confidence from its string representation.
pub(crate) fn parse_confidence(s: &str) -> Confidence {
    match s {
        "confirmed" => Confidence::Confirmed,
        "detected" => Confidence::Detected,
        _ => {
            tracing::warn!("Unknown confidence value: {}, defaulting to Detected", s);
            Confidence::Detected
        }
    }
}

/// Parse a NodeId from its stable_id string (format: "root:file:name:kind").
/// Falls back to using the type hint and root if parsing is ambiguous.
pub(crate) fn parse_node_id_from_stable(
    stable_id: &str,
    kind_hint: &str,
    root_hint: &str,
) -> NodeId {
    // stable_id format: "root:file:name:kind"
    // We need to handle the case where file or name might contain ':'
    // Strategy: split from the end to get kind, then from the start to get root,
    // the middle is file:name which we split on the last ':'
    let parts: Vec<&str> = stable_id.splitn(2, ':').collect();
    if parts.len() < 2 {
        return NodeId {
            root: root_hint.to_string(),
            file: PathBuf::from(stable_id),
            name: String::new(),
            kind: parse_node_kind(kind_hint),
        };
    }

    let root = parts[0].to_string();
    let rest = parts[1]; // "file:name:kind"

    // Split from the end to get kind
    if let Some(last_colon) = rest.rfind(':') {
        let before_kind = &rest[..last_colon]; // "file:name"
        // Split file:name on the last colon
        if let Some(name_colon) = before_kind.rfind(':') {
            let file = &before_kind[..name_colon];
            let name = &before_kind[name_colon + 1..];
            return NodeId {
                root,
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: parse_node_kind(kind_hint),
            };
        }
        // Only one segment -- treat as file with empty name
        return NodeId {
            root,
            file: PathBuf::from(before_kind),
            name: String::new(),
            kind: parse_node_kind(kind_hint),
        };
    }

    NodeId {
        root: root_hint.to_string(),
        file: PathBuf::from(rest),
        name: String::new(),
        kind: parse_node_kind(kind_hint),
    }
}

/// Infer programming language from file extension.
pub(crate) fn infer_language_from_path(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust".to_string(),
        Some("py") => "python".to_string(),
        Some("ts") | Some("tsx") => "typescript".to_string(),
        Some("js") | Some("jsx") => "javascript".to_string(),
        Some("go") => "go".to_string(),
        Some("java") => "java".to_string(),
        Some("c") | Some("h") | Some("cpp") | Some("cc") | Some("cxx") | Some("hpp")
        | Some("hh") | Some("hxx") => "cpp".to_string(),
        Some("cs") => "csharp".to_string(),
        Some("rb") => "ruby".to_string(),
        Some("kt") | Some("kts") => "kotlin".to_string(),
        Some("swift") => "swift".to_string(),
        Some("zig") => "zig".to_string(),
        Some("lua") => "lua".to_string(),
        Some("gd") => "gdscript".to_string(),
        Some("sh") | Some("bash") => "bash".to_string(),
        Some("tf") | Some("hcl") | Some("tfvars") => "hcl".to_string(),
        Some("json") | Some("jsonc") => "json".to_string(),
        Some("proto") => "protobuf".to_string(),
        Some("sql") => "sql".to_string(),
        Some("md") => "markdown".to_string(),
        Some("toml") => "toml".to_string(),
        Some("yaml") | Some("yml") => "yaml".to_string(),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::extract::{Extractor, rust::RustExtractor};
    use crate::graph::{
        Confidence, Edge, EdgeEvidence, EdgeKind, EvidenceSelector, ExtractionSource, Node, NodeId,
        NodeKind, ValidationStatus,
    };

    use super::{load_graph_from_lance, parse_edge_kind, persist_graph_to_lance};

    fn node(kind: &str, name: &str, file: &str) -> Node {
        Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Other(kind.to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 1,
            signature: format!("{} {}", kind, name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        }
    }

    #[test]
    fn parse_edge_kind_preserves_unknown_labels_as_custom_edges() {
        assert_eq!(parse_edge_kind("calls"), Some(EdgeKind::Calls));
        assert_eq!(
            parse_edge_kind("supports"),
            Some(EdgeKind::Other("supports".to_string()))
        );
        assert_eq!(parse_edge_kind("  "), None);
    }

    #[tokio::test]
    async fn custom_edge_kind_survives_persist_load_and_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = node("quote", "quote.goodhart", ".oh/sources/goodhart.md");
        let claim = node("claim", "claim.proxy-risk", ".oh/knowledge/proxy-risk.md");
        let edge = Edge {
            from: source.id.clone(),
            to: claim.id.clone(),
            kind: EdgeKind::Other("supports".to_string()),
            source: ExtractionSource::Markdown,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };

        persist_graph_to_lance(dir.path(), &[source.clone(), claim.clone()], &[edge])
            .await
            .expect("persist graph");
        let state = load_graph_from_lance(dir.path()).await.expect("load graph");

        assert_eq!(
            state.edges.len(),
            1,
            "custom edge must not be dropped on load"
        );
        assert_eq!(state.edges[0].kind, EdgeKind::Other("supports".to_string()));
        assert_eq!(state.edges[0].confidence, Confidence::Confirmed);

        let neighbors = state.index.neighbors(
            &source.stable_id(),
            Some(&[EdgeKind::Other("supports".to_string())]),
            petgraph::Direction::Outgoing,
        );
        assert_eq!(neighbors, vec![claim.stable_id()]);
    }

    #[tokio::test]
    async fn edge_evidence_survives_persist_load_and_renders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut source = node("paragraph", "body", "chapter.md");
        source.line_start = 4;
        source.line_end = 4;
        source.body = "Retries cap cascading load.".into();
        source.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        source.metadata.insert("byte_start".into(), "20".into());
        source.metadata.insert("byte_end".into(), "47".into());
        let claim = node("claim", "graceful-recovery", "claims.md");
        let selector = EvidenceSelector {
            root_id: source.id.root.clone(),
            file_path: source.id.file.clone(),
            line_start: 4,
            line_end: 4,
            byte_start: 20,
            byte_end: 47,
            body_node_id: source.metadata["body_node_id"].clone(),
            snippet_hash: blake3::hash(source.body.as_bytes()).to_hex().to_string(),
            snippet: "stale producer-supplied display text".into(),
        };
        let edge = Edge {
            from: source.id.clone(),
            to: claim.id.clone(),
            kind: EdgeKind::Other("supports".into()),
            source: ExtractionSource::Markdown,
            confidence: Confidence::Confirmed,
            evidence: vec![EdgeEvidence {
                selectors: vec![selector],
                extractor_id: "markdown-ast@1".into(),
                pack_id: Some("reliability@1".into()),
                rule_id: "supports@1".into(),
                confidence: Confidence::Confirmed,
                validation_status: ValidationStatus::Valid,
            }],
        };

        persist_graph_to_lance(dir.path(), &[source.clone(), claim.clone()], &[edge])
            .await
            .expect("persist graph");
        let state = load_graph_from_lance(dir.path()).await.expect("load graph");
        assert_eq!(state.edges[0].evidence.len(), 1);
        assert_eq!(
            state.edges[0].evidence[0].validation_status,
            ValidationStatus::Valid
        );
        assert_eq!(
            state.edges[0].evidence[0].selectors[0].snippet, source.body,
            "load-time validation must refresh display text from the current body"
        );
        let groups = std::collections::BTreeMap::from([(
            EdgeKind::Other("supports".into()),
            vec![claim.stable_id()],
        )]);
        let rendered = crate::server::helpers::format_edge_evidence_for_groups(
            &state.edges,
            &source.stable_id(),
            &groups,
            "outgoing",
        );
        assert!(rendered.contains("chapter.md:4-4"));
        assert!(rendered.contains("Retries cap cascading load."));
        assert!(!rendered.contains("stale producer-supplied display text"));
        assert!(rendered.contains("supports@1"));
        assert!(rendered.contains("Valid"));

        let mut detected_evidence_edge = state.edges[0].clone();
        detected_evidence_edge.confidence = Confidence::Confirmed;
        detected_evidence_edge.evidence[0].confidence = Confidence::Detected;
        persist_graph_to_lance(
            dir.path(),
            &[source, claim],
            std::slice::from_ref(&detected_evidence_edge),
        )
        .await
        .expect("persist valid edge with detected evidence");
        let detected_state = load_graph_from_lance(dir.path())
            .await
            .expect("load valid edge with detected evidence");
        assert_eq!(
            detected_state.edges[0].evidence[0].validation_status,
            ValidationStatus::Valid
        );
        assert_eq!(
            detected_state.edges[0].evidence[0].confidence,
            Confidence::Detected
        );
        assert_eq!(
            detected_state.edges[0].confidence,
            Confidence::Detected,
            "valid detected evidence must cap the containing edge at detected confidence"
        );
    }

    #[tokio::test]
    async fn rust_struct_construction_survives_persist_load_and_rendering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let extractor = RustExtractor::new();
        let mut declaration = extractor
            .extract(
                std::path::Path::new("src/options.rs"),
                "pub struct BusOptions { pub enabled: bool }\n",
            )
            .expect("extract declaration");
        let mut construction = extractor
            .extract(
                std::path::Path::new("src/server.rs"),
                "fn build() { let _ = BusOptions { enabled: true }; }\n",
            )
            .expect("extract construction");
        for node in declaration.nodes.iter_mut().chain(&mut construction.nodes) {
            node.id.root = "repo".into();
        }
        let mut nodes = declaration.nodes;
        nodes.extend(construction.nodes);
        let edges = crate::extract::struct_construction::struct_construction_pass(&nodes);
        assert_eq!(edges.len(), 1);

        persist_graph_to_lance(dir.path(), &nodes, &edges)
            .await
            .expect("persist graph");
        let state = load_graph_from_lance(dir.path()).await.expect("load graph");
        let declaration = state
            .nodes
            .iter()
            .find(|node| node.id.kind == NodeKind::Struct && node.id.name == "BusOptions")
            .expect("loaded declaration");
        let site = state
            .nodes
            .iter()
            .find(|node| matches!(&node.id.kind, NodeKind::Other(kind) if kind == "struct_literal"))
            .expect("loaded construction");
        assert_eq!(
            site.metadata.get("constructed_type").map(String::as_str),
            Some("BusOptions")
        );
        assert_eq!(
            site.metadata.get("parent_scope").map(String::as_str),
            Some("build")
        );

        let edge_kind = EdgeKind::Constructs;
        let neighbors = state.index.neighbors(
            &declaration.stable_id(),
            Some(std::slice::from_ref(&edge_kind)),
            petgraph::Direction::Incoming,
        );
        assert_eq!(neighbors, vec![site.stable_id()]);

        let groups = std::collections::BTreeMap::from([(edge_kind, neighbors)]);
        let rendered = crate::server::helpers::format_neighbors_grouped(
            &state.nodes,
            &groups,
            &state.index,
            false,
        );
        assert!(rendered.contains("Constructs (1)"));
        assert!(rendered.contains("BusOptions@1:"));
    }

    #[tokio::test]
    async fn node_extraction_sources_survive_persist_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = [
            ExtractionSource::TreeSitter,
            ExtractionSource::Lsp,
            ExtractionSource::Schema,
            ExtractionSource::Git,
            ExtractionSource::Markdown,
        ];
        let nodes: Vec<Node> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let mut node = node("fixture", &format!("node-{index}"), "fixture.md");
                node.source = source.clone();
                node
            })
            .collect();

        persist_graph_to_lance(dir.path(), &nodes, &[])
            .await
            .expect("persist graph");
        let state = load_graph_from_lance(dir.path()).await.expect("load graph");
        let loaded_sources: BTreeMap<_, _> = state
            .nodes
            .iter()
            .map(|node| (node.id.name.as_str(), &node.source))
            .collect();

        for (index, expected) in sources.iter().enumerate() {
            let name = format!("node-{index}");
            assert_eq!(loaded_sources.get(name.as_str()), Some(&expected));
        }
    }
}
