//! LanceDB table schemas for the graph model.
//!
//! Defines Arrow schemas for the `symbols`, `edges`, and `file_index` tables.
//! These are additive — the existing `EmbeddingIndex` in `embed.rs` continues
//! to handle `.oh/` artifact embeddings.

use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema};

/// Schema version for all LanceDB tables.
///
/// Bump this whenever ANY schema changes (symbols, edges, pr_merges, file_index).
/// The server auto-drops and rebuilds all LanceDB tables when this mismatches
/// the stored version. No manual cache deletion needed.
pub const SCHEMA_VERSION: u32 = 2;

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
        Field::new("properties_json", DataType::Utf8, true),
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

/// Arrow schema for the `_schema_meta` table.
///
/// Single-row key/value table used to persist the schema version so the
/// server can detect staleness on startup and auto-drop all tables.
pub fn schema_meta_schema() -> Schema {
    Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, false),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_version_constant() {
        // SCHEMA_VERSION must be at least 2 (bumped when _schema_meta table was introduced)
        assert!(SCHEMA_VERSION >= 2, "SCHEMA_VERSION should be >= 2");
    }

    #[test]
    fn test_schema_meta_schema_fields() {
        let schema = schema_meta_schema();
        assert!(schema.field_with_name("key").is_ok());
        assert!(schema.field_with_name("value").is_ok());
        assert_eq!(schema.fields().len(), 2);
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
    fn test_edges_schema_fields() {
        let schema = edges_schema();
        assert!(schema.field_with_name("id").is_ok());
        assert!(schema.field_with_name("source_id").is_ok());
        assert!(schema.field_with_name("source_type").is_ok());
        assert!(schema.field_with_name("target_id").is_ok());
        assert!(schema.field_with_name("target_type").is_ok());
        assert!(schema.field_with_name("edge_type").is_ok());
        assert!(schema.field_with_name("properties_json").is_ok());
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
