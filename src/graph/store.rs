//! LanceDB table schemas for the graph model.
//!
//! Defines Arrow schemas for the `symbols`, `edges`, and `file_index` tables.
//! The `EmbeddingIndex` in `embed.rs` shares this same LanceDB directory,
//! storing its `artifacts` table alongside `symbols`, `edges`, etc.

use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema};

/// Schema version for all LanceDB tables.
///
/// Bump this whenever ANY schema changes (symbols, edges, pr_merges, file_index).
/// The server auto-drops and rebuilds all LanceDB tables when this mismatches
/// the stored version. No manual cache deletion needed.
/// Also surfaced in the index freshness footer on `search`.
pub const SCHEMA_VERSION: u32 = 26; // owner-qualified scoped symbol identities

/// Arrow schema for the `symbols` table.
///
/// Stores code symbols (functions, structs, traits, etc.) with embeddings
/// for semantic search and deterministic IDs for graph traversal.
///
/// Typed metadata columns (no JSON blobs for known fields):
/// - `meta_virtual`  — true for virtual external nodes produced by LSP enrichment
/// - `meta_package`  — crate/package name for virtual nodes (e.g. "lancedb", "tokio")
/// - `meta_name_col` — LSP cursor column used for go-to-definition disambiguation
/// - `value`         — constant value for Const nodes
/// - `synthetic`     — true for synthetic/inferred constants (e.g. YAML scalar key-values)
/// - `pattern_hint`  — design pattern detected from naming conventions (e.g. "factory", "observer")
/// - `diagnostic_severity`  — "error" or "warning" (for NodeKind::Other("diagnostic") nodes)
/// - `diagnostic_source`    — LSP server name (e.g. "rust-analyzer")
/// - `diagnostic_message`   — full diagnostic message text
/// - `diagnostic_range`     — "line:col-end_line:end_col" string
/// - `diagnostic_timestamp` — unix timestamp (seconds) when diagnostic was captured
/// - `http_method`  — HTTP verb for ApiEndpoint nodes ("GET", "POST", etc.)
/// - `http_path`    — HTTP path for ApiEndpoint nodes ("/users", "/items/{id}", etc.)
/// - `doc_comment`  — documentation comment extracted by tree-sitter (#416)
///
/// bump SCHEMA_VERSION in store.rs when changing this
pub fn symbols_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("root_id", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("line_start", DataType::UInt32, false),
        Field::new("line_end", DataType::UInt32, false),
        Field::new("signature", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("extraction_source", DataType::Utf8, true),
        // Typed metadata columns — Arrow type safety, no JSON blobs for known fields.
        Field::new("meta_virtual", DataType::Boolean, true),
        Field::new("meta_package", DataType::Utf8, true),
        Field::new("meta_name_col", DataType::Int32, true),
        Field::new("value", DataType::Utf8, true), // metadata["value"]
        Field::new("synthetic", DataType::Boolean, true), // metadata["synthetic"] == "true"
        Field::new("cyclomatic", DataType::Int32, true), // metadata["cyclomatic"] — complexity score
        Field::new("importance", DataType::Float64, true), // PageRank importance score (0.0-1.0)
        Field::new("storage", DataType::Utf8, true), // metadata["storage"] — "static" (Rust) or "var" (Go)
        Field::new("mutable", DataType::Boolean, true), // metadata["mutable"] — true for `static mut`
        Field::new("decorators", DataType::Utf8, true), // metadata["decorators"] — comma-separated decorator/attribute text
        Field::new("parent_scope", DataType::Utf8, true), // metadata["parent_scope"] — enclosing class/interface name
        Field::new("parent_scope_kind", DataType::Utf8, true), // metadata["parent_scope_kind"] — enclosing scope kind
        Field::new("framework_hook", DataType::Utf8, true), // metadata["framework_hook"] — extractor-config hook match
        Field::new("type_params", DataType::Utf8, true), // metadata["type_params"] — generic type parameters (e.g. "<T: Clone + Send>")
        Field::new("pattern_hint", DataType::Utf8, true), // metadata["pattern_hint"] — design pattern from naming conventions (e.g. "factory", "observer")
        Field::new("is_static", DataType::Boolean, true), // metadata["is_static"] — true for static/associated methods, false for instance methods
        Field::new("is_async", DataType::Boolean, true), // metadata["is_async"] — true for async functions (#390)
        Field::new("is_test", DataType::Boolean, true), // metadata["is_test"] — true for test functions (#390)
        Field::new("visibility", DataType::Utf8, true), // metadata["visibility"] — "pub" for public re-exports (#409)
        Field::new("exported", DataType::Boolean, true), // metadata["exported"] — true for Python __all__ exports (#409)
        // Diagnostic columns — populated for NodeKind::Other("diagnostic") nodes
        Field::new("diagnostic_severity", DataType::Utf8, true), // "error" | "warning"
        Field::new("diagnostic_source", DataType::Utf8, true),   // LSP server name
        Field::new("diagnostic_message", DataType::Utf8, true),  // full diagnostic text
        Field::new("diagnostic_range", DataType::Utf8, true),    // "line:col-end_line:end_col"
        Field::new("diagnostic_timestamp", DataType::Utf8, true), // unix timestamp string
        // ApiEndpoint columns — populated for NodeKind::ApiEndpoint nodes
        Field::new("http_method", DataType::Utf8, true), // "GET" | "POST" | "PUT" | etc.
        Field::new("http_path", DataType::Utf8, true),   // "/users" | "/items/{id}" | etc.
        // Doc comment column — survives LSP reindex round-trip (#416)
        Field::new("doc_comment", DataType::Utf8, true), // metadata["doc_comment"] — documentation comment
        // attr_refs column — survives round-trip so import_calls_pass Step 6
        // (attr-access ReferencedBy) works on incremental scans; otherwise
        // previously-extracted functions lose attr_refs after a LanceDB load.
        Field::new("attr_refs", DataType::Utf8, true), // metadata["attr_refs"] — comma-separated dot-access identifiers in function bodies
        // gRPC / proto columns — populated for proto RPC Function nodes (#466)
        // These must survive LanceDB round-trip so GrpcClientCallsPass works on incremental scans.
        Field::new("parent_service", DataType::Utf8, true), // metadata["parent_service"] — owning service name
        Field::new("rpc_request_type", DataType::Utf8, true), // metadata["request_type"] — proto request message
        Field::new("rpc_response_type", DataType::Utf8, true), // metadata["response_type"] — proto response message
        // Generic metadata JSON — persists repo-local/custom metadata keys that are
        // not represented by typed Arrow columns. Typed metadata remains in dedicated
        // columns for query/type stability; this column preserves extensibility.
        Field::new("metadata_json", DataType::Utf8, true),
        // Vector column is added dynamically when embeddings are computed,
        // since the dimension depends on the model. See `symbols_schema_with_vector`.
        Field::new("updated_at", DataType::Int64, false),
        // Append-only versioning: each full rebuild writes with an incrementing scan_version.
        // Queries filter to the latest committed version. Stale rows compacted separately.
        Field::new("scan_version", DataType::UInt64, false),
    ])
}

/// Arrow schema for `symbols` including a fixed-size vector column.
/// `dim` is the embedding dimension (e.g., 384 for BGE-small-en-v1.5).
///
/// bump SCHEMA_VERSION in store.rs when changing this
pub fn symbols_schema_with_vector(dim: i32) -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("root_id", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("line_start", DataType::UInt32, false),
        Field::new("line_end", DataType::UInt32, false),
        Field::new("signature", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("extraction_source", DataType::Utf8, true),
        Field::new("meta_virtual", DataType::Boolean, true),
        Field::new("meta_package", DataType::Utf8, true),
        Field::new("meta_name_col", DataType::Int32, true),
        Field::new("value", DataType::Utf8, true),
        Field::new("synthetic", DataType::Boolean, true),
        Field::new("cyclomatic", DataType::Int32, true),
        Field::new("importance", DataType::Float64, true),
        Field::new("storage", DataType::Utf8, true),
        Field::new("mutable", DataType::Boolean, true),
        Field::new("decorators", DataType::Utf8, true),
        Field::new("parent_scope", DataType::Utf8, true),
        Field::new("parent_scope_kind", DataType::Utf8, true),
        Field::new("framework_hook", DataType::Utf8, true),
        Field::new("type_params", DataType::Utf8, true),
        Field::new("pattern_hint", DataType::Utf8, true),
        Field::new("is_static", DataType::Boolean, true),
        Field::new("is_async", DataType::Boolean, true),
        Field::new("is_test", DataType::Boolean, true),
        Field::new("visibility", DataType::Utf8, true),
        Field::new("exported", DataType::Boolean, true),
        // Diagnostic columns — populated for NodeKind::Other("diagnostic") nodes
        Field::new("diagnostic_severity", DataType::Utf8, true),
        Field::new("diagnostic_source", DataType::Utf8, true),
        Field::new("diagnostic_message", DataType::Utf8, true),
        Field::new("diagnostic_range", DataType::Utf8, true),
        Field::new("diagnostic_timestamp", DataType::Utf8, true),
        // ApiEndpoint columns — populated for NodeKind::ApiEndpoint nodes
        Field::new("http_method", DataType::Utf8, true),
        Field::new("http_path", DataType::Utf8, true),
        // Doc comment column — survives LSP reindex round-trip (#416)
        Field::new("doc_comment", DataType::Utf8, true),
        // attr_refs column — survives round-trip (#644 follow-up)
        Field::new("attr_refs", DataType::Utf8, true),
        // gRPC / proto columns — populated for proto RPC Function nodes (#466)
        Field::new("parent_service", DataType::Utf8, true),
        Field::new("rpc_request_type", DataType::Utf8, true),
        Field::new("rpc_response_type", DataType::Utf8, true),
        // Generic metadata JSON — persists repo-local/custom metadata keys not represented
        // by typed Arrow columns.
        Field::new("metadata_json", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            true, // nullable: not all symbols need embeddings immediately
        ),
        Field::new("updated_at", DataType::Int64, false),
        // Append-only versioning: each full rebuild writes with an incrementing scan_version.
        Field::new("scan_version", DataType::UInt64, false),
    ])
}

/// Arrow schema for the `edges` table.
///
/// Stores directed relationships between nodes. Source of truth for the
/// petgraph in-memory index (which is rebuilt from this table).
///
/// bump SCHEMA_VERSION in store.rs when changing this
pub fn edges_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("source_id", DataType::Utf8, false),
        Field::new("source_file", DataType::Utf8, false),
        Field::new("source_name", DataType::Utf8, false),
        Field::new("source_type", DataType::Utf8, false),
        Field::new("target_id", DataType::Utf8, false),
        Field::new("target_root_id", DataType::Utf8, false),
        Field::new("target_file", DataType::Utf8, false),
        Field::new("target_name", DataType::Utf8, false),
        Field::new("target_type", DataType::Utf8, false),
        Field::new("edge_type", DataType::Utf8, false),
        Field::new("edge_source", DataType::Utf8, false),
        Field::new("edge_confidence", DataType::Utf8, false),
        Field::new("edge_evidence_json", DataType::Utf8, true),
        Field::new("root_id", DataType::Utf8, false),
        Field::new("updated_at", DataType::Int64, false),
        // Append-only versioning: each full rebuild writes with an incrementing scan_version.
        // Queries filter to the latest committed version. Stale rows compacted separately.
        Field::new("scan_version", DataType::UInt64, false),
    ])
}

/// Arrow schema for the `pr_merges` table.
///
/// Stores PR-level change summaries extracted from merge commits on the base
/// branch. PRs are the natural unit of meaningful change — they have semantic
/// intent (title/description), bounded scope, and map to graph edges via the
/// files they modify.
///
/// bump SCHEMA_VERSION in store.rs when changing this
pub fn pr_merges_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false), // root:merge_commit_sha
        Field::new("root_id", DataType::Utf8, false),
        Field::new("merge_sha", DataType::Utf8, false), // the merge commit
        Field::new("branch_name", DataType::Utf8, true), // from commit message
        Field::new("title", DataType::Utf8, false),     // first line of merge commit message
        Field::new("description", DataType::Utf8, true), // rest of merge commit message
        Field::new("author", DataType::Utf8, false),
        Field::new("merged_at", DataType::Int64, false), // unix timestamp
        Field::new("commit_count", DataType::UInt32, false), // commits in the PR
        Field::new("files_changed", DataType::Utf8, false), // JSON array of file paths
        Field::new("updated_at", DataType::Int64, false),
    ])
}

/// Arrow schema for the `file_index` table.
///
/// Tracks which files have been indexed and by which extractors,
/// enabling incremental re-indexing on file changes.
///
/// bump SCHEMA_VERSION in store.rs when changing this
pub fn file_index_schema() -> Schema {
    Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("root_id", DataType::Utf8, false),
        Field::new("mtime", DataType::Int64, false),
        Field::new("size", DataType::UInt64, false),
        Field::new("last_indexed", DataType::Int64, false),
        Field::new("extractors_used", DataType::Utf8, false), // comma-separated list
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbols_schema_has_expected_fields() {
        let schema = symbols_schema();
        let vector_schema = symbols_schema_with_vector(384);
        assert!(schema.field_with_name("id").is_ok());
        assert!(schema.field_with_name("signature").is_ok());
        assert!(schema.field_with_name("body").is_ok());
        assert!(schema.field_with_name("extraction_source").is_ok());
        assert!(
            vector_schema.field_with_name("extraction_source").is_ok(),
            "base and vector symbols schemas must preserve the same node provenance"
        );
        assert!(schema.field_with_name("value").is_ok());
        assert!(schema.field_with_name("synthetic").is_ok());
        assert!(schema.field_with_name("cyclomatic").is_ok());
        assert!(schema.field_with_name("importance").is_ok());
        assert!(schema.field_with_name("storage").is_ok());
        assert!(schema.field_with_name("mutable").is_ok());
        assert!(schema.field_with_name("decorators").is_ok());
        assert!(schema.field_with_name("type_params").is_ok());
        assert!(schema.field_with_name("is_static").is_ok());
        // Diagnostic columns
        assert!(schema.field_with_name("diagnostic_severity").is_ok());
        assert!(schema.field_with_name("diagnostic_source").is_ok());
        assert!(schema.field_with_name("diagnostic_message").is_ok());
        assert!(schema.field_with_name("diagnostic_range").is_ok());
        assert!(schema.field_with_name("diagnostic_timestamp").is_ok());
    }

    #[test]
    fn test_edges_schema_has_expected_fields() {
        let schema = edges_schema();
        assert!(schema.field_with_name("source_id").is_ok());
        assert!(schema.field_with_name("source_file").is_ok());
        assert!(schema.field_with_name("source_name").is_ok());
        assert!(schema.field_with_name("target_id").is_ok());
        assert!(schema.field_with_name("target_root_id").is_ok());
        assert!(schema.field_with_name("target_file").is_ok());
        assert!(schema.field_with_name("target_name").is_ok());
        assert!(schema.field_with_name("edge_type").is_ok());
    }
}
