//! Graph loading from LanceDB tables.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Context;
use arrow_array::{
    Array, BooleanArray, Float64Array, Int32Array, RecordBatch, StringArray, UInt32Array,
};

use crate::graph::index::GraphIndex;
use crate::graph::{Confidence, Edge, ExtractionSource, Node, NodeId};
use crate::server::store::metadata_keys as mk;

use super::super::state::GraphState;
use super::migrate::read_committed_scan_version;
use super::{
    graph_lance_path, infer_language_from_path, parse_confidence, parse_edge_kind,
    parse_extraction_source, parse_node_kind,
};

fn node_extraction_source_at(
    extraction_source_col: Option<&StringArray>,
    row: usize,
) -> ExtractionSource {
    extraction_source_col
        .filter(|col| !col.is_null(row))
        .map(|col| parse_extraction_source(col.value(row)))
        .unwrap_or(ExtractionSource::TreeSitter)
}

fn required_string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> anyhow::Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("persisted graph is missing required {name:?} column"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("persisted graph column {name:?} is not Utf8"))
}

fn ensure_persisted_node_identity(persisted_id: &str, node_id: &NodeId) -> anyhow::Result<()> {
    anyhow::ensure!(
        node_id.to_stable_id() == persisted_id,
        "persisted symbol identity mismatch: id={persisted_id:?}, fields={node_id:?}"
    );
    Ok(())
}

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
            let ids = required_string_column(batch, "id")?;
            let root_ids = batch
                .column_by_name("root_id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let file_paths = batch
                .column_by_name("file_path")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let names = batch
                .column_by_name("name")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let kinds = batch
                .column_by_name("kind")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let line_starts = batch
                .column_by_name("line_start")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap();
            let line_ends = batch
                .column_by_name("line_end")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap();
            let signatures = batch
                .column_by_name("signature")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let bodies = batch
                .column_by_name("body")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let extraction_source_col = batch
                .column_by_name("extraction_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadata_json_col = batch
                .column_by_name("metadata_json")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // Typed metadata columns -- Arrow type safety, no JSON blobs for known fields.
            let meta_virtual_col = batch
                .column_by_name("meta_virtual")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let meta_package_col = batch
                .column_by_name("meta_package")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let meta_name_col_col = batch
                .column_by_name("meta_name_col")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
            // Language remains derived from the file extension. Legacy tables may not
            // have extraction_source, so node_extraction_source_at falls back safely.

            // Read optional value and synthetic columns (present after schema migration)
            let value_col = batch
                .column_by_name("value")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let synthetic_col = batch
                .column_by_name("synthetic")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let cyclomatic_col = batch
                .column_by_name("cyclomatic")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
            let importance_col = batch
                .column_by_name("importance")
                .and_then(|c| c.as_any().downcast_ref::<Float64Array>());
            let storage_col = batch
                .column_by_name("storage")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let mutable_col = batch
                .column_by_name("mutable")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let decorators_col = batch
                .column_by_name("decorators")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let parent_scope_col = batch
                .column_by_name("parent_scope")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let parent_scope_kind_col = batch
                .column_by_name("parent_scope_kind")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let framework_hook_col = batch
                .column_by_name("framework_hook")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let type_params_col = batch
                .column_by_name("type_params")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let pattern_hint_col = batch
                .column_by_name("pattern_hint")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let is_static_col = batch
                .column_by_name("is_static")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let is_async_col = batch
                .column_by_name("is_async")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let is_test_col = batch
                .column_by_name("is_test")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            let visibility_col = batch
                .column_by_name("visibility")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let exported_col = batch
                .column_by_name("exported")
                .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
            // Diagnostic metadata columns (nullable -- only present on diagnostic nodes)
            let diag_severity_col = batch
                .column_by_name("diagnostic_severity")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_source_col = batch
                .column_by_name("diagnostic_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_message_col = batch
                .column_by_name("diagnostic_message")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_range_col = batch
                .column_by_name("diagnostic_range")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let diag_timestamp_col = batch
                .column_by_name("diagnostic_timestamp")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // ApiEndpoint metadata columns (nullable -- only present on api_endpoint nodes)
            let http_method_col = batch
                .column_by_name("http_method")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let http_path_col = batch
                .column_by_name("http_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // doc_comment column -- survives LSP reindex round-trip (#416)
            let doc_comment_col = batch
                .column_by_name("doc_comment")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // attr_refs column -- survives round-trip for import_calls_pass Step 6
            let attr_refs_col = batch
                .column_by_name("attr_refs")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // gRPC / proto columns -- survives round-trip for GrpcClientCallsPass on incremental scans (#466)
            let parent_service_col = batch
                .column_by_name("parent_service")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let rpc_request_type_col = batch
                .column_by_name("rpc_request_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let rpc_response_type_col = batch
                .column_by_name("rpc_response_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let file_path = PathBuf::from(file_paths.value(i));
                let language = infer_language_from_path(&file_path);
                let mut metadata: BTreeMap<String, String> = BTreeMap::new();
                if let Some(col) = metadata_json_col
                    && !col.is_null(i)
                {
                    let raw = col.value(i);
                    if !raw.is_empty() {
                        match serde_json::from_str::<BTreeMap<String, String>>(raw) {
                            Ok(extra) => metadata.extend(extra),
                            Err(err) => tracing::warn!(
                                "load_graph_from_lance: ignoring invalid metadata_json for {}: {}",
                                file_path.display(),
                                err
                            ),
                        }
                    }
                }
                if let Some(col) = meta_virtual_col
                    && !col.is_null(i)
                    && col.value(i)
                {
                    metadata.insert(mk::VIRTUAL.to_owned(), "true".to_string());
                }
                if let Some(col) = meta_package_col
                    && !col.is_null(i)
                {
                    metadata.insert(mk::PACKAGE.to_owned(), col.value(i).to_string());
                }
                if let Some(col) = meta_name_col_col
                    && !col.is_null(i)
                {
                    metadata.insert(mk::NAME_COL.to_owned(), col.value(i).to_string());
                }
                if let Some(col) = value_col
                    && !col.is_null(i)
                {
                    metadata.insert(mk::VALUE.to_owned(), col.value(i).to_string());
                }
                if let Some(col) = synthetic_col
                    && !col.is_null(i)
                {
                    metadata.insert(
                        mk::SYNTHETIC.to_owned(),
                        if col.value(i) { "true" } else { "false" }.to_string(),
                    );
                }
                if let Some(col) = cyclomatic_col
                    && !col.is_null(i)
                {
                    metadata.insert(mk::CYCLOMATIC.to_owned(), col.value(i).to_string());
                }
                if let Some(col) = importance_col
                    && !col.is_null(i)
                {
                    metadata.insert(mk::IMPORTANCE.to_owned(), format!("{:.6}", col.value(i)));
                }
                if let Some(col) = storage_col
                    && !col.is_null(i)
                {
                    metadata.insert(mk::STORAGE.to_owned(), col.value(i).to_string());
                }
                if let Some(col) = mutable_col
                    && !col.is_null(i)
                    && col.value(i)
                {
                    metadata.insert(mk::MUTABLE.to_owned(), "true".to_string());
                }
                if let Some(col) = decorators_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DECORATORS.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = parent_scope_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::PARENT_SCOPE.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = parent_scope_kind_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::PARENT_SCOPE_KIND.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = framework_hook_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::FRAMEWORK_HOOK.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = type_params_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::TYPE_PARAMS.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = pattern_hint_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::PATTERN_HINT.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = is_static_col
                    && !col.is_null(i)
                {
                    metadata.insert(
                        mk::IS_STATIC.to_owned(),
                        if col.value(i) { "true" } else { "false" }.to_string(),
                    );
                }
                if let Some(col) = is_async_col
                    && !col.is_null(i)
                    && col.value(i)
                {
                    metadata.insert(mk::IS_ASYNC.to_owned(), "true".to_string());
                }
                if let Some(col) = is_test_col
                    && !col.is_null(i)
                    && col.value(i)
                {
                    metadata.insert(mk::IS_TEST.to_owned(), "true".to_string());
                }
                if let Some(col) = visibility_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::VISIBILITY.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = exported_col
                    && !col.is_null(i)
                    && col.value(i)
                {
                    metadata.insert(mk::EXPORTED.to_owned(), "true".to_string());
                }
                if let Some(col) = diag_severity_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DIAG_SEVERITY.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = diag_source_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DIAG_SOURCE.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = diag_message_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DIAG_MESSAGE.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = diag_range_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DIAG_RANGE.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = diag_timestamp_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DIAG_TIMESTAMP.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = http_method_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::HTTP_METHOD.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = http_path_col
                    && !col.is_null(i)
                {
                    // An empty route path is meaningful extractor output (for
                    // example, a bare Python route decorator). Preserve the
                    // distinction between a present empty value and Arrow null.
                    metadata.insert(mk::HTTP_PATH.to_owned(), col.value(i).to_string());
                }
                if let Some(col) = doc_comment_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::DOC_COMMENT.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = attr_refs_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::ATTR_REFS.to_owned(), val.to_string());
                    }
                }
                // gRPC / proto columns -- restore metadata for GrpcClientCallsPass (#466)
                if let Some(col) = parent_service_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::PARENT_SERVICE.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = rpc_request_type_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::REQUEST_TYPE.to_owned(), val.to_string());
                    }
                }
                if let Some(col) = rpc_response_type_col
                    && !col.is_null(i)
                {
                    let val = col.value(i);
                    if !val.is_empty() {
                        metadata.insert(mk::RESPONSE_TYPE.to_owned(), val.to_string());
                    }
                }
                let id = NodeId {
                    root: root_ids.value(i).to_string(),
                    file: file_path,
                    name: names.value(i).to_string(),
                    kind: parse_node_kind(kinds.value(i)),
                };
                ensure_persisted_node_identity(ids.value(i), &id)?;
                nodes.push(Node {
                    id,
                    language,
                    line_start: line_starts.value(i) as usize,
                    line_end: line_ends.value(i) as usize,
                    signature: signatures.value(i).to_string(),
                    body: bodies.value(i).to_string(),
                    metadata,
                    source: node_extraction_source_at(extraction_source_col, i),
                });
            }
        }
        nodes
    };

    // Stable IDs are a compact lookup representation, not a reversible wire
    // encoding: both file paths and symbol names may contain `:`. Prefer exact
    // structured identities loaded from `symbols`; schema-v25 edge fields retain
    // equally exact identities for legitimate dangling endpoints. Fail closed on
    // ambiguous symbols or disagreement between the two persisted projections.
    let mut persisted_node_ids = HashMap::with_capacity(nodes.len());
    for node in &nodes {
        let stable_id = node.stable_id();
        if let Some(previous) = persisted_node_ids.insert(stable_id.clone(), node.id.clone())
            && previous != node.id
        {
            anyhow::bail!(
                "ambiguous persisted node stable ID {stable_id:?}: {previous:?} and {:?}",
                node.id
            );
        }
    }

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
            let source_ids = required_string_column(batch, "source_id")?;
            let source_files = required_string_column(batch, "source_file")?;
            let source_names = required_string_column(batch, "source_name")?;
            let source_types = required_string_column(batch, "source_type")?;
            let target_ids = required_string_column(batch, "target_id")?;
            let target_root_ids = required_string_column(batch, "target_root_id")?;
            let target_files = required_string_column(batch, "target_file")?;
            let target_names = required_string_column(batch, "target_name")?;
            let target_types = required_string_column(batch, "target_type")?;
            let edge_types = batch
                .column_by_name("edge_type")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let edge_sources = batch
                .column_by_name("edge_source")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let edge_confidences = batch
                .column_by_name("edge_confidence")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let edge_evidence = batch
                .column_by_name("edge_evidence_json")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let root_ids = batch
                .column_by_name("root_id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            for i in 0..batch.num_rows() {
                let edge_kind = match parse_edge_kind(edge_types.value(i)) {
                    Some(k) => k,
                    None => continue,
                };

                let extraction_source = edge_sources
                    .map(|a| parse_extraction_source(a.value(i)))
                    .unwrap_or(ExtractionSource::TreeSitter);
                let mut confidence = edge_confidences
                    .map(|a| parse_confidence(a.value(i)))
                    .unwrap_or(Confidence::Detected);
                let evidence = match edge_evidence.filter(|array| !array.is_null(i)) {
                    Some(array) => match serde_json::from_str(array.value(i)) {
                        Ok(evidence) => evidence,
                        Err(error) => {
                            tracing::warn!("invalid persisted edge evidence: {error}");
                            confidence = Confidence::Detected;
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                };

                let source_id = source_ids.value(i);
                let target_id = target_ids.value(i);
                let stored_from = NodeId {
                    root: root_ids.value(i).to_string(),
                    file: PathBuf::from(source_files.value(i)),
                    name: source_names.value(i).to_string(),
                    kind: parse_node_kind(source_types.value(i)),
                };
                let stored_to = NodeId {
                    root: target_root_ids.value(i).to_string(),
                    file: PathBuf::from(target_files.value(i)),
                    name: target_names.value(i).to_string(),
                    kind: parse_node_kind(target_types.value(i)),
                };
                anyhow::ensure!(
                    stored_from.to_stable_id() == source_id,
                    "persisted edge source identity mismatch: id={source_id:?}, fields={stored_from:?}"
                );
                anyhow::ensure!(
                    stored_to.to_stable_id() == target_id,
                    "persisted edge target identity mismatch: id={target_id:?}, fields={stored_to:?}"
                );
                let from = match persisted_node_ids.get(source_id) {
                    Some(node_id) => {
                        anyhow::ensure!(
                            node_id == &stored_from,
                            "persisted edge source fields disagree with symbol {source_id:?}: edge={stored_from:?}, symbol={node_id:?}"
                        );
                        node_id.clone()
                    }
                    None => stored_from,
                };
                let to = match persisted_node_ids.get(target_id) {
                    Some(node_id) => {
                        anyhow::ensure!(
                            node_id == &stored_to,
                            "persisted edge target fields disagree with symbol {target_id:?}: edge={stored_to:?}, symbol={node_id:?}"
                        );
                        node_id.clone()
                    }
                    None => stored_to,
                };

                edges.push(Edge {
                    from,
                    to,
                    kind: edge_kind,
                    source: extraction_source,
                    confidence,
                    evidence,
                });
            }
        }
        let _ = crate::graph::revalidate_edge_evidence(&mut edges, &nodes);
        edges
    };

    // -- Build index --
    let mut index = GraphIndex::new();
    index.rebuild_from_edges(&edges);
    for node in &nodes {
        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
    }

    Ok(GraphState::new(
        nodes,
        edges,
        index,
        Some(std::time::Instant::now()),
        std::collections::HashSet::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_extraction_source_supports_all_values_and_legacy_fallback() {
        let sources = StringArray::from(vec![
            Some("tree_sitter"),
            Some("lsp"),
            Some("schema"),
            Some("git"),
            Some("markdown"),
            None,
        ]);

        assert_eq!(
            node_extraction_source_at(Some(&sources), 0),
            ExtractionSource::TreeSitter
        );
        assert_eq!(
            node_extraction_source_at(Some(&sources), 1),
            ExtractionSource::Lsp
        );
        assert_eq!(
            node_extraction_source_at(Some(&sources), 2),
            ExtractionSource::Schema
        );
        assert_eq!(
            node_extraction_source_at(Some(&sources), 3),
            ExtractionSource::Git
        );
        assert_eq!(
            node_extraction_source_at(Some(&sources), 4),
            ExtractionSource::Markdown
        );
        assert_eq!(
            node_extraction_source_at(Some(&sources), 5),
            ExtractionSource::TreeSitter
        );
        assert_eq!(
            node_extraction_source_at(None, 0),
            ExtractionSource::TreeSitter,
            "legacy symbols tables without extraction_source must still load"
        );
    }

    #[test]
    fn persisted_symbol_identity_mismatch_fails_closed() {
        let id = NodeId {
            root: "fixture".to_string(),
            file: PathBuf::from("src/lib.rs"),
            name: "target".to_string(),
            kind: crate::graph::NodeKind::Function,
        };

        assert!(ensure_persisted_node_identity(&id.to_stable_id(), &id).is_ok());
        let error = ensure_persisted_node_identity("tampered", &id).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("persisted symbol identity mismatch")
        );
    }
}
