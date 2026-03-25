//! Arrow RecordBatch builders for symbols (nodes) and edges tables.

use arrow_array::{RecordBatch, StringArray, UInt32Array, UInt64Array, Int32Array, Int64Array, Float64Array, BooleanArray};
use arrow_array::builder::BooleanBuilder;

use crate::graph::{Node, Edge};
use crate::graph::store::{symbols_schema, edges_schema};

/// Build a symbols `RecordBatch` for `nodes` tagged with `scan_version`.
pub(super) fn build_symbols_batch(nodes: &[Node], scan_version: u64) -> anyhow::Result<RecordBatch> {
    use std::sync::Arc;
    let schema = Arc::new(symbols_schema());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let ids: Vec<String> = nodes.iter().map(|n| n.stable_id()).collect();
    let root_ids: Vec<String> = nodes.iter().map(|n| n.id.root.clone()).collect();
    let file_paths: Vec<String> = nodes.iter().map(|n| n.id.file.display().to_string()).collect();
    let names: Vec<String> = nodes.iter().map(|n| n.id.name.clone()).collect();
    let kinds: Vec<String> = nodes.iter().map(|n| n.id.kind.to_string()).collect();
    let line_starts: Vec<u32> = nodes.iter().map(|n| n.line_start as u32).collect();
    let line_ends: Vec<u32> = nodes.iter().map(|n| n.line_end as u32).collect();
    let signatures: Vec<String> = nodes.iter().map(|n| n.signature.clone()).collect();
    let bodies: Vec<String> = nodes.iter().map(|n| n.body.clone()).collect();
    let meta_virtuals: Vec<Option<bool>> = nodes.iter()
        .map(|n| if n.metadata.get("virtual").map(|v| v.as_str()) == Some("true") { Some(true) } else { None })
        .collect();
    let meta_packages: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("package").cloned())
        .collect();
    let meta_name_cols: Vec<Option<i32>> = nodes.iter()
        .map(|n| n.metadata.get("name_col").and_then(|s| s.parse::<i32>().ok()))
        .collect();
    let values: Vec<Option<String>> = nodes.iter().map(|n| n.metadata.get("value").cloned()).collect();
    let mut synthetic_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("synthetic") {
            Some(v) => synthetic_builder.append_value(v == "true"),
            None => synthetic_builder.append_null(),
        }
    }
    let cyclomatics: Vec<Option<i32>> = nodes.iter()
        .map(|n| n.metadata.get("cyclomatic").and_then(|s| s.parse::<i32>().ok()))
        .collect();
    let importances: Vec<Option<f64>> = nodes.iter()
        .map(|n| n.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()))
        .collect();
    let storages: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("storage").cloned())
        .collect();
    let mut mutable_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("mutable") {
            Some(v) => mutable_builder.append_value(v == "true"),
            None => mutable_builder.append_null(),
        }
    }
    let decorators_col: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("decorators").cloned())
        .collect();
    let type_params_col: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("type_params").cloned())
        .collect();
    let pattern_hints: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("pattern_hint").cloned())
        .collect();
    let mut is_static_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("is_static") {
            Some(v) => is_static_builder.append_value(v == "true"),
            None => is_static_builder.append_null(),
        }
    }
    let mut is_async_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("is_async") {
            Some(v) => is_async_builder.append_value(v == "true"),
            None => is_async_builder.append_null(),
        }
    }
    let mut is_test_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("is_test") {
            Some(v) => is_test_builder.append_value(v == "true"),
            None => is_test_builder.append_null(),
        }
    }
    let visibilities: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("visibility").cloned())
        .collect();
    let mut exported_builder = BooleanBuilder::new();
    for n in nodes.iter() {
        match n.metadata.get("exported") {
            Some(v) => exported_builder.append_value(v == "true"),
            None => exported_builder.append_null(),
        }
    }
    // Diagnostic metadata columns
    let diag_severities: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_severity").cloned())
        .collect();
    let diag_sources: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_source").cloned())
        .collect();
    let diag_messages: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_message").cloned())
        .collect();
    let diag_ranges: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_range").cloned())
        .collect();
    let diag_timestamps: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("diagnostic_timestamp").cloned())
        .collect();
    // ApiEndpoint metadata columns
    let http_methods: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("http_method").cloned())
        .collect();
    let http_paths: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("http_path").cloned())
        .collect();
    // doc_comment column -- persisted for LSP reindex round-trip (#416)
    let doc_comments: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("doc_comment").cloned())
        .collect();
    // gRPC / proto columns -- populated for proto RPC Function nodes (#466)
    let parent_services: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("parent_service").cloned())
        .collect();
    let rpc_request_types: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("request_type").cloned())
        .collect();
    let rpc_response_types: Vec<Option<String>> = nodes.iter()
        .map(|n| n.metadata.get("response_type").cloned())
        .collect();
    let updated_ats: Vec<i64> = vec![now; nodes.len()];
    let scan_versions: Vec<u64> = vec![scan_version; nodes.len()];

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(root_ids)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(kinds)),
            Arc::new(UInt32Array::from(line_starts)),
            Arc::new(UInt32Array::from(line_ends)),
            Arc::new(StringArray::from(signatures)),
            Arc::new(StringArray::from(bodies)),
            Arc::new(BooleanArray::from(meta_virtuals)),
            Arc::new(StringArray::from(meta_packages)),
            Arc::new(Int32Array::from(meta_name_cols)),
            Arc::new(StringArray::from(values)),
            Arc::new(synthetic_builder.finish()),
            Arc::new(Int32Array::from(cyclomatics)),
            Arc::new(Float64Array::from(importances)),
            Arc::new(StringArray::from(storages)),
            Arc::new(mutable_builder.finish()),
            Arc::new(StringArray::from(decorators_col)),
            Arc::new(StringArray::from(type_params_col)),
            Arc::new(StringArray::from(pattern_hints)),
            Arc::new(is_static_builder.finish()),
            Arc::new(is_async_builder.finish()),
            Arc::new(is_test_builder.finish()),
            Arc::new(StringArray::from(visibilities)),
            Arc::new(exported_builder.finish()),
            Arc::new(StringArray::from(diag_severities)),
            Arc::new(StringArray::from(diag_sources)),
            Arc::new(StringArray::from(diag_messages)),
            Arc::new(StringArray::from(diag_ranges)),
            Arc::new(StringArray::from(diag_timestamps)),
            Arc::new(StringArray::from(http_methods)),
            Arc::new(StringArray::from(http_paths)),
            Arc::new(StringArray::from(doc_comments)),
            Arc::new(StringArray::from(parent_services)),
            Arc::new(StringArray::from(rpc_request_types)),
            Arc::new(StringArray::from(rpc_response_types)),
            Arc::new(Int64Array::from(updated_ats)),
            Arc::new(UInt64Array::from(scan_versions)),
        ],
    ).map_err(anyhow::Error::from)
}

/// Build an edges `RecordBatch` for `edges` tagged with `scan_version`.
pub(super) fn build_edges_batch(edges: &[Edge], scan_version: u64) -> anyhow::Result<RecordBatch> {
    use std::sync::Arc;
    let schema = Arc::new(edges_schema());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let ids: Vec<String> = edges.iter().map(|e| e.stable_id()).collect();
    let source_ids: Vec<String> = edges.iter().map(|e| e.from.to_stable_id()).collect();
    let source_types: Vec<String> = edges.iter().map(|e| e.from.kind.to_string()).collect();
    let target_ids: Vec<String> = edges.iter().map(|e| e.to.to_stable_id()).collect();
    let target_types: Vec<String> = edges.iter().map(|e| e.to.kind.to_string()).collect();
    let edge_types: Vec<String> = edges.iter().map(|e| e.kind.to_string()).collect();
    let edge_sources: Vec<String> = edges.iter().map(|e| e.source.to_string()).collect();
    let edge_confidences: Vec<String> = edges.iter().map(|e| e.confidence.to_string()).collect();
    let root_ids: Vec<String> = edges.iter().map(|e| e.from.root.clone()).collect();
    let updated_ats: Vec<i64> = vec![now; edges.len()];
    let scan_versions: Vec<u64> = vec![scan_version; edges.len()];

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(source_ids)),
            Arc::new(StringArray::from(source_types)),
            Arc::new(StringArray::from(target_ids)),
            Arc::new(StringArray::from(target_types)),
            Arc::new(StringArray::from(edge_types)),
            Arc::new(StringArray::from(edge_sources)),
            Arc::new(StringArray::from(edge_confidences)),
            Arc::new(StringArray::from(root_ids)),
            Arc::new(Int64Array::from(updated_ats)),
            Arc::new(UInt64Array::from(scan_versions)),
        ],
    ).map_err(anyhow::Error::from)
}
