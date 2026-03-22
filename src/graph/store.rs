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
pub const SCHEMA_VERSION: u32 = 16; // slug portability: root_id now uses directory name, not full path

/// Extraction version for source-level extraction logic.
///
/// Bump this whenever tree-sitter extraction changes produce new or different
/// metadata (e.g., new fields like `doc_comment`, changed parsing logic).
/// When mismatched against the stored version, the server clears all scan-state
/// files so every file is re-extracted from scratch on the next build.
///
/// Unlike SCHEMA_VERSION (which invalidates LanceDB tables), EXTRACTION_VERSION
/// invalidates the scanner's mtime/hash state — forcing full re-extraction without
/// dropping LanceDB tables. Bumped to 1 for doc_comment metadata extraction (#401).
/// Bumped to 3 for new C, PHP, HTML, Scala, Dart, Elixir extractors (#435).
/// Bumped to 4 for Next.js routing pass (#440) and monorepo subdirectory roots (#442).
/// Bumped to 5 for subsystem first-class nodes (#470): emits NodeKind::Other("subsystem")
/// nodes and BelongsTo edges after community detection.
pub const EXTRACTION_VERSION: u32 = 5;

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
        // Typed metadata columns — Arrow type safety, no JSON blobs for known fields.
        Field::new("meta_virtual", DataType::Boolean, true),
        Field::new("meta_package", DataType::Utf8, true),
        Field::new("meta_name_col", DataType::Int32, true),
        Field::new("value", DataType::Utf8, true),      // metadata["value"]
        Field::new("synthetic", DataType::Boolean, true), // metadata["synthetic"] == "true"
        Field::new("cyclomatic", DataType::Int32, true),   // metadata["cyclomatic"] — complexity score
        Field::new("importance", DataType::Float64, true),   // PageRank importance score (0.0-1.0)
        Field::new("storage", DataType::Utf8, true),         // metadata["storage"] — "static" (Rust) or "var" (Go)
        Field::new("mutable", DataType::Boolean, true),      // metadata["mutable"] — true for `static mut`
        Field::new("decorators", DataType::Utf8, true),        // metadata["decorators"] — comma-separated decorator/attribute text
        Field::new("type_params", DataType::Utf8, true),       // metadata["type_params"] — generic type parameters (e.g. "<T: Clone + Send>")
        Field::new("pattern_hint", DataType::Utf8, true),        // metadata["pattern_hint"] — design pattern from naming conventions (e.g. "factory", "observer")
        Field::new("is_static", DataType::Boolean, true),           // metadata["is_static"] — true for static/associated methods, false for instance methods
        Field::new("is_async", DataType::Boolean, true),             // metadata["is_async"] — true for async functions (#390)
        Field::new("is_test", DataType::Boolean, true),              // metadata["is_test"] — true for test functions (#390)
        Field::new("visibility", DataType::Utf8, true),              // metadata["visibility"] — "pub" for public re-exports (#409)
        Field::new("exported", DataType::Boolean, true),             // metadata["exported"] — true for Python __all__ exports (#409)
        // Diagnostic columns — populated for NodeKind::Other("diagnostic") nodes
        Field::new("diagnostic_severity", DataType::Utf8, true),    // "error" | "warning"
        Field::new("diagnostic_source", DataType::Utf8, true),      // LSP server name
        Field::new("diagnostic_message", DataType::Utf8, true),     // full diagnostic text
        Field::new("diagnostic_range", DataType::Utf8, true),       // "line:col-end_line:end_col"
        Field::new("diagnostic_timestamp", DataType::Utf8, true),   // unix timestamp string
        // ApiEndpoint columns — populated for NodeKind::ApiEndpoint nodes
        Field::new("http_method", DataType::Utf8, true),    // "GET" | "POST" | "PUT" | etc.
        Field::new("http_path", DataType::Utf8, true),      // "/users" | "/items/{id}" | etc.
        // Doc comment column — survives LSP reindex round-trip (#416)
        Field::new("doc_comment", DataType::Utf8, true),    // metadata["doc_comment"] — documentation comment
        // Vector column is added dynamically when embeddings are computed,
        // since the dimension depends on the model. See `symbols_schema_with_vector`.
        Field::new("updated_at", DataType::Int64, false),
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
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim,
            ),
            true, // nullable: not all symbols need embeddings immediately
        ),
        Field::new("updated_at", DataType::Int64, false),
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
        Field::new("source_type", DataType::Utf8, false),
        Field::new("target_id", DataType::Utf8, false),
        Field::new("target_type", DataType::Utf8, false),
        Field::new("edge_type", DataType::Utf8, false),
        Field::new("edge_source", DataType::Utf8, false),
        Field::new("edge_confidence", DataType::Utf8, false),
        Field::new("root_id", DataType::Utf8, false),
        Field::new("updated_at", DataType::Int64, false),
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
        Field::new("id", DataType::Utf8, false),           // root:merge_commit_sha
        Field::new("root_id", DataType::Utf8, false),
        Field::new("merge_sha", DataType::Utf8, false),     // the merge commit
        Field::new("branch_name", DataType::Utf8, true),    // from commit message
        Field::new("title", DataType::Utf8, false),         // first line of merge commit message
        Field::new("description", DataType::Utf8, true),    // rest of merge commit message
        Field::new("author", DataType::Utf8, false),
        Field::new("merged_at", DataType::Int64, false),    // unix timestamp
        Field::new("commit_count", DataType::UInt32, false), // commits in the PR
        Field::new("files_changed", DataType::Utf8, false),  // JSON array of file paths
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
    fn test_schema_version_constant() {
        // SCHEMA_VERSION must be at least 15 (bumped for doc_comment column #416)
        assert!(SCHEMA_VERSION >= 15, "SCHEMA_VERSION should be >= 15");
    }

    #[test]
    fn test_extraction_version_constant() {
        // EXTRACTION_VERSION must be at least 1 (bumped for doc_comment extraction #401)
        assert!(EXTRACTION_VERSION >= 1, "EXTRACTION_VERSION should be >= 1");
    }

    #[test]
    fn test_symbols_schema_fields() {
        let schema = symbols_schema();
        assert!(schema.field_with_name("id").is_ok());
        assert!(schema.field_with_name("root_id").is_ok());
        assert!(schema.field_with_name("file_path").is_ok());
        assert!(schema.field_with_name("name").is_ok());
        assert!(schema.field_with_name("kind").is_ok());
        assert!(schema.field_with_name("line_start").is_ok());
        assert!(schema.field_with_name("line_end").is_ok());
        assert!(schema.field_with_name("signature").is_ok());
        assert!(schema.field_with_name("body").is_ok());
        assert!(schema.field_with_name("meta_virtual").is_ok());
        assert!(schema.field_with_name("meta_package").is_ok());
        assert!(schema.field_with_name("meta_name_col").is_ok());
        assert!(schema.field_with_name("value").is_ok());
        assert!(schema.field_with_name("synthetic").is_ok());
        assert!(schema.field_with_name("importance").is_ok());
        assert!(schema.field_with_name("storage").is_ok());
        assert!(schema.field_with_name("mutable").is_ok());
        assert!(schema.field_with_name("decorators").is_ok());
        assert!(schema.field_with_name("type_params").is_ok());
        assert!(schema.field_with_name("is_static").is_ok());
        // Diagnostic columns (added for NodeKind::Other("diagnostic") nodes)
        assert!(schema.field_with_name("diagnostic_severity").is_ok());
        assert!(schema.field_with_name("diagnostic_source").is_ok());
        assert!(schema.field_with_name("diagnostic_message").is_ok());
        assert!(schema.field_with_name("diagnostic_range").is_ok());
        assert!(schema.field_with_name("diagnostic_timestamp").is_ok());
        // ApiEndpoint columns (added for NodeKind::ApiEndpoint nodes)
        assert!(schema.field_with_name("http_method").is_ok());
        assert!(schema.field_with_name("http_path").is_ok());
        // doc_comment column — survives LSP reindex round-trip (#416)
        assert!(schema.field_with_name("doc_comment").is_ok());
        assert!(schema.field_with_name("updated_at").is_ok());
        // no vector column in base schema
        assert!(schema.field_with_name("vector").is_err());
    }

    #[test]
    fn test_symbols_schema_with_vector() {
        let schema = symbols_schema_with_vector(384);
        assert!(schema.field_with_name("vector").is_ok());
        let vector_field = schema.field_with_name("vector").unwrap();
        match vector_field.data_type() {
            DataType::FixedSizeList(_, dim) => assert_eq!(*dim, 384),
            other => panic!("Expected FixedSizeList, got {:?}", other),
        }
    }

    #[test]
    fn test_is_static_column_type_and_nullability() {
        // Adversarial: verify is_static is Boolean and nullable in both schemas
        let schema = symbols_schema();
        let field = schema.field_with_name("is_static").expect("is_static missing from symbols_schema");
        assert_eq!(*field.data_type(), DataType::Boolean, "is_static should be Boolean");
        assert!(field.is_nullable(), "is_static should be nullable (top-level functions have no is_static)");

        let vec_schema = symbols_schema_with_vector(384);
        let vec_field = vec_schema.field_with_name("is_static").expect("is_static missing from symbols_schema_with_vector");
        assert_eq!(*vec_field.data_type(), DataType::Boolean, "is_static should be Boolean in vector schema");
        assert!(vec_field.is_nullable(), "is_static should be nullable in vector schema");
    }

    #[test]
    fn test_edges_schema_fields() {
        let schema = edges_schema();
        assert!(schema.field_with_name("id").is_ok());
        assert!(schema.field_with_name("source_id").is_ok());
        assert!(schema.field_with_name("source_type").is_ok());
        assert!(schema.field_with_name("target_id").is_ok());
        assert!(schema.field_with_name("target_type").is_ok());
        assert!(schema.field_with_name("edge_type").is_ok());
        assert!(schema.field_with_name("edge_source").is_ok());
        assert!(schema.field_with_name("edge_confidence").is_ok());
        assert!(schema.field_with_name("root_id").is_ok());
        assert!(schema.field_with_name("updated_at").is_ok());
    }

    #[test]
    fn test_pr_merges_schema_fields() {
        let schema = pr_merges_schema();
        assert!(schema.field_with_name("id").is_ok());
        assert!(schema.field_with_name("root_id").is_ok());
        assert!(schema.field_with_name("merge_sha").is_ok());
        assert!(schema.field_with_name("branch_name").is_ok());
        assert!(schema.field_with_name("title").is_ok());
        assert!(schema.field_with_name("description").is_ok());
        assert!(schema.field_with_name("author").is_ok());
        assert!(schema.field_with_name("merged_at").is_ok());
        assert!(schema.field_with_name("commit_count").is_ok());
        assert!(schema.field_with_name("files_changed").is_ok());
        assert!(schema.field_with_name("updated_at").is_ok());
    }

    #[test]
    fn test_file_index_schema_fields() {
        let schema = file_index_schema();
        assert!(schema.field_with_name("path").is_ok());
        assert!(schema.field_with_name("root_id").is_ok());
        assert!(schema.field_with_name("mtime").is_ok());
        assert!(schema.field_with_name("size").is_ok());
        assert!(schema.field_with_name("last_indexed").is_ok());
        assert!(schema.field_with_name("extractors_used").is_ok());
    }
}
