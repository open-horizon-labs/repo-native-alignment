//! LanceDB table schemas for the graph model.
//!
//! Defines Arrow schemas for the `symbols`, `edges`, and `file_index` tables.
//! These are additive — the existing `EmbeddingIndex` in `embed.rs` continues
//! to handle `.oh/` artifact embeddings.

use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema};

/// Arrow schema for the `symbols` table.
///
/// Stores code symbols (functions, structs, traits, etc.) with embeddings
/// for semantic search and deterministic IDs for graph traversal.
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
        // Vector column is added dynamically when embeddings are computed,
        // since the dimension depends on the model. See `symbols_schema_with_vector`.
        Field::new("updated_at", DataType::Int64, false),
    ])
}

/// Arrow schema for `symbols` including a fixed-size vector column.
/// `dim` is the embedding dimension (e.g., 384 for BGE-small-en-v1.5).
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

/// Arrow schema for the `file_index` table.
///
/// Tracks which files have been indexed and by which extractors,
/// enabling incremental re-indexing on file changes.
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
