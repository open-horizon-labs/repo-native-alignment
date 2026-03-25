//! LanceDB persistence: persist, load, schema migration, stale root pruning.
//!
//! ## Module structure
//!
//! - `migrate` -- schema and extraction version migration, error classification
//! - `batch` -- Arrow RecordBatch builders for symbols and edges tables
//! - `persist` -- full persist, incremental upsert, compaction, root pruning
//! - `load` -- graph loading from LanceDB tables
// EXTRACTION_VERSION is deprecated (#526) but still used for backward-compat sentinel reads.
#![allow(deprecated)]

mod batch;
pub(crate) mod migrate;
pub(crate) mod persist;
pub(crate) mod load;

use std::path::{Path, PathBuf};

use crate::graph::{Confidence, EdgeKind, ExtractionSource, NodeId, NodeKind};

// ── Re-exports for backward compatibility ────────────────────────────

// From migrate
pub(crate) use migrate::{check_and_migrate_schema, check_and_migrate_extraction_version};

// From persist
pub(crate) use persist::{
    persist_graph_to_lance, persist_graph_incremental,
    get_stored_root_ids, delete_nodes_for_roots,
};

// From load
pub use load::load_graph_from_lance;

// ── Graph persistence (LanceDB) ─────────────────────────────────────

/// LanceDB path for graph persistence.
pub(crate) fn graph_lance_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("lance")
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
pub fn parse_edge_kind(s: &str) -> Option<EdgeKind> {
    Some(match s {
        "calls" => EdgeKind::Calls,
        "implements" => EdgeKind::Implements,
        "depends_on" => EdgeKind::DependsOn,
        "connects_to" => EdgeKind::ConnectsTo,
        "defines" => EdgeKind::Defines,
        "has_field" => EdgeKind::HasField,
        "evolves" => EdgeKind::Evolves,
        "referenced_by" => EdgeKind::ReferencedBy,
        "references" => EdgeKind::References,
        "topology_boundary" => EdgeKind::TopologyBoundary,
        "modified" => EdgeKind::Modified,
        "affected" => EdgeKind::Affected,
        "serves" => EdgeKind::Serves,
        "tested_by" => EdgeKind::TestedBy,
        "belongs_to" => EdgeKind::BelongsTo,
        "re_exports" => EdgeKind::ReExports,
        "uses_framework" => EdgeKind::UsesFramework,
        "produces" => EdgeKind::Produces,
        "consumes" => EdgeKind::Consumes,
        _ => return None,
    })
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
            tracing::warn!("Unknown edge_source value: {}, defaulting to TreeSitter", s);
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
pub(crate) fn parse_node_id_from_stable(stable_id: &str, kind_hint: &str, root_hint: &str) -> NodeId {
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
        Some("c") | Some("h") | Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") | Some("hh") | Some("hxx") => "cpp".to_string(),
        Some("cs") => "csharp".to_string(),
        Some("rb") => "ruby".to_string(),
        Some("kt") | Some("kts") => "kotlin".to_string(),
        Some("swift") => "swift".to_string(),
        Some("zig") => "zig".to_string(),
        Some("lua") => "lua".to_string(),
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
