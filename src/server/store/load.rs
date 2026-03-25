//! Graph loading from LanceDB tables.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use arrow_array::{Array, RecordBatch, StringArray, UInt32Array, Int32Array, Float64Array, BooleanArray};

use crate::graph::{Confidence, Edge, ExtractionSource, Node, NodeId};
use crate::graph::index::GraphIndex;

use super::{
    graph_lance_path, parse_node_kind, parse_edge_kind, parse_extraction_source, parse_confidence,
    parse_node_id_from_stable, infer_language_from_path,
};
use super::migrate::read_committed_scan_version;
use super::super::state::GraphState;

/// Load graph nodes and edges from LanceDB tables.
///
/// Reads only rows matching the currently committed `scan_version`.
/// This ensures full-rebuild appends don't expose partially-written data:
/// the new version only becomes visible after `persist_graph_to_lance` flips
/// the version pointer.
pub async fn load_graph_from_lance(repo_root: &Path) -> anyhow::Result<GraphState> {
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};

    let db_path = graph_lance_path(repo_root);
    if !db_path.exists() {
        anyhow::bail!("No persisted graph at {}", db_path.display());
    }

    // Read the committed version. If it's 0 (no version file), fall back to loading
    // all rows -- this handles legacy data written before the scan_version column existed.
    let committed_version = read_committed_scan_version(&db_path);
    let version_filter: Option<String> = if committed_version > 0 {
        Some(format!("scan_version = {}", committed_version))
    } else {
        None // Legacy data: no filter (scan_version absent or all rows at version 0)
    };

    let db = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .context("Failed to connect to LanceDB for graph loading")?;

    // -- Read symbols (nodes) --
    let nodes = {
        let table = db
            .open_table("symbols")
            .execute()
            .await
            .context("No symbols table found")?;
        let mut q = table.query();
        if let Some(ref filter) = version_filter {
            q = q.only_if(filter.as_str());
        }
        let stream = q.execute().await.context("Failed to query symbols")?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut nodes = Vec::new();
        for batch in &batches {
            let ids = batch.column_by_name("id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let root_ids = batch.column_by_name("root_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let file_paths = batch.column_by_name("file_path").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let names = batch.column_by_name("name").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let kinds = batch.column_by_name("kind").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let line_starts = batch.column_by_name("line_start").unwrap().as_any().downcast_ref::<UInt32Array>().unwrap();
            let line_ends = batch.column_by_name("line_end").unwrap().as_any().downcast_ref::<UInt32Array>().unwrap();
            let signatures = batch.column_by_name("signature").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let bodies = batch.column_by_name("body").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            // Typed metadata columns -- Arrow type safety, no JSON blobs for known fields.
            let meta_virtual_col = batch.column_by_name("meta_virtual")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let meta_package_col = batch.column_by_name("meta_package")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let meta_name_col_col = batch.column_by_name("meta_name_col")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
            // We don't store language or source in the symbols schema, so we infer from file extension
            let _ = ids; // ids column exists but we reconstruct from components

            // Read optional value and synthetic columns (present after schema migration)
            let value_col = batch.column_by_name("value")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let synthetic_col = batch.column_by_name("synthetic")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let cyclomatic_col = batch.column_by_name("cyclomatic")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
            let importance_col = batch.column_by_name("importance")
                .and_then(|c| c.as_any().downcast_ref::<Float64Array>());
            let storage_col = batch.column_by_name("storage")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let mutable_col = batch.column_by_name("mutable")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let decorators_col = batch.column_by_name("decorators")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let type_params_col = batch.column_by_name("type_params")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let pattern_hint_col = batch.column_by_name("pattern_hint")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let is_static_col = batch.column_by_name("is_static")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let is_async_col = batch.column_by_name("is_async")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let is_test_col = batch.column_by_name("is_test")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let visibility_col = batch.column_by_name("visibility")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let exported_col = batch.column_by_name("exported")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            // Diagnostic metadata columns (nullable -- only present on diagnostic nodes)
            let diag_severity_col = batch.column_by_name("diagnostic_severity")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_source_col = batch.column_by_name("diagnostic_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_message_col = batch.column_by_name("diagnostic_message")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_range_col = batch.column_by_name("diagnostic_range")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_timestamp_col = batch.column_by_name("diagnostic_timestamp")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // ApiEndpoint metadata columns (nullable -- only present on api_endpoint nodes)
            let http_method_col = batch.column_by_name("http_method")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let http_path_col = batch.column_by_name("http_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // doc_comment column -- survives LSP reindex round-trip (#416)
            let doc_comment_col = batch.column_by_name("doc_comment")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // gRPC / proto columns -- survives round-trip for GrpcClientCallsPass on incremental scans (#466)
            let parent_service_col = batch.column_by_name("parent_service")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let rpc_request_type_col = batch.column_by_name("rpc_request_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let rpc_response_type_col = batch.column_by_name("rpc_response_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let file_path = PathBuf::from(file_paths.value(i));
                let language = infer_language_from_path(&file_path);
                let mut metadata: BTreeMap<String, String> = BTreeMap::new();
                if let Some(col) = meta_virtual_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("virtual".to_string(), "true".to_string());
                    }
                if let Some(col) = meta_package_col
                    && !col.is_null(i) {
                        metadata.insert("package".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = meta_name_col_col
                    && !col.is_null(i) {
                        metadata.insert("name_col".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = value_col
                    && !col.is_null(i) {
                        metadata.insert("value".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = synthetic_col
                    && !col.is_null(i) {
                        metadata.insert("synthetic".to_string(), if col.value(i) { "true" } else { "false" }.to_string());
                    }
                if let Some(col) = cyclomatic_col
                    && !col.is_null(i) {
                        metadata.insert("cyclomatic".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = importance_col
                    && !col.is_null(i) {
                        metadata.insert("importance".to_string(), format!("{:.6}", col.value(i)));
                    }
                if let Some(col) = storage_col
                    && !col.is_null(i) {
                        metadata.insert("storage".to_string(), col.value(i).to_string());
                    }
                if let Some(col) = mutable_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("mutable".to_string(), "true".to_string());
                    }
                if let Some(col) = decorators_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("decorators".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = type_params_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("type_params".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = pattern_hint_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("pattern_hint".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = is_static_col
                    && !col.is_null(i) {
                        metadata.insert("is_static".to_string(), if col.value(i) { "true" } else { "false" }.to_string());
                    }
                if let Some(col) = is_async_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("is_async".to_string(), "true".to_string());
                    }
                if let Some(col) = is_test_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("is_test".to_string(), "true".to_string());
                    }
                if let Some(col) = visibility_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("visibility".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = exported_col
                    && !col.is_null(i) && col.value(i) {
                        metadata.insert("exported".to_string(), "true".to_string());
                    }
                if let Some(col) = diag_severity_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_severity".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_source_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_source".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_message_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_message".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_range_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_range".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = diag_timestamp_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("diagnostic_timestamp".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = http_method_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("http_method".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = http_path_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("http_path".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = doc_comment_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("doc_comment".to_string(), val.to_string());
                        }
                    }
                // gRPC / proto columns -- restore metadata for GrpcClientCallsPass (#466)
                if let Some(col) = parent_service_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("parent_service".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = rpc_request_type_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("request_type".to_string(), val.to_string());
                        }
                    }
                if let Some(col) = rpc_response_type_col
                    && !col.is_null(i) {
                        let val = col.value(i);
                        if !val.is_empty() {
                            metadata.insert("response_type".to_string(), val.to_string());
                        }
                    }
                nodes.push(Node {
                    id: NodeId {
                        root: root_ids.value(i).to_string(),
                        file: file_path,
                        name: names.value(i).to_string(),
                        kind: parse_node_kind(kinds.value(i)),
                    },
                    language,
                    line_start: line_starts.value(i) as usize,
                    line_end: line_ends.value(i) as usize,
                    signature: signatures.value(i).to_string(),
                    body: bodies.value(i).to_string(),
                    metadata,
                    source: ExtractionSource::TreeSitter, // default; not stored in schema
                });
            }
        }
        nodes
    };

    // -- Read edges --
    let edges = {
        let table = db
            .open_table("edges")
            .execute()
            .await
            .context("No edges table found")?;
        let mut q = table.query();
        if let Some(ref filter) = version_filter {
            q = q.only_if(filter.as_str());
        }
        let stream = q.execute().await.context("Failed to query edges")?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut edges = Vec::new();
        for batch in &batches {
            let source_ids = batch.column_by_name("source_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let source_types = batch.column_by_name("source_type").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let target_ids = batch.column_by_name("target_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let target_types = batch.column_by_name("target_type").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let edge_types = batch.column_by_name("edge_type").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let edge_sources = batch.column_by_name("edge_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let edge_confidences = batch.column_by_name("edge_confidence")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let root_ids = batch.column_by_name("root_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();

            for i in 0..batch.num_rows() {
                let edge_kind = match parse_edge_kind(edge_types.value(i)) {
                    Some(k) => k,
                    None => continue,
                };

                let extraction_source = edge_sources
                    .map(|a| parse_extraction_source(a.value(i)))
                    .unwrap_or(ExtractionSource::TreeSitter);
                let confidence = edge_confidences
                    .map(|a| parse_confidence(a.value(i)))
                    .unwrap_or(Confidence::Detected);

                // Parse NodeId from stable_id format: "root:file:name:kind"
                let from = parse_node_id_from_stable(source_ids.value(i), source_types.value(i), root_ids.value(i));
                let to = parse_node_id_from_stable(target_ids.value(i), target_types.value(i), root_ids.value(i));

                edges.push(Edge {
                    from,
                    to,
                    kind: edge_kind,
                    source: extraction_source,
                    confidence,
                });
            }
        }
        edges
    };

    // -- Build index --
    let mut index = GraphIndex::new();
    index.rebuild_from_edges(&edges);
    for node in &nodes {
        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
    }

    Ok(GraphState::new(nodes, edges, index, Some(std::time::Instant::now()), std::collections::HashSet::new()))
}
