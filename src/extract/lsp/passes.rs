//! LSP enrichment pass functions extracted from the monolithic `enrich()` method.
//!
//! Each pass is an `async fn` on `LspEnricher` that takes shared state and
//! appends results to the `EnrichmentResult`. The top-level `enrich()` orchestrates
//! them in sequence: Pass 0 -> Pass 1 -> Pass 2 -> Pass 4 -> Pass 5 -> Pass 3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::transport::{
    PipelinedTransport, find_enclosing_symbol, path_to_uri, uri_to_relative_path,
};
use super::{
    EnrichmentResult, LspEnricher, ZERO_EDGE_ABORT_THRESHOLD, ZERO_EDGE_MIN_WARMUP,
    ZERO_EDGE_TIMEOUT,
};
use crate::scanner::LspConfig;

impl LspEnricher {
    // ------------------------------------------------------------------
    // Pass 0: crate-level dependency graph via rust-analyzer/viewCrateGraph.
    //
    // Single request; returns the entire workspace crate graph as a DOT
    // string. Runs unconditionally (no per-node cost, no quiescence
    // requirement). Only emits nodes+edges for Rust roots.
    // ------------------------------------------------------------------
    pub(super) async fn run_pass0_crate_graph(
        &self,
        transport: &PipelinedTransport,
        matching_nodes: &[&Node],
        result: &mut EnrichmentResult,
    ) {
        if self.language != "rust" {
            return;
        }

        let pass0_start = std::time::Instant::now();
        let root_id = matching_nodes
            .first()
            .map(|n| n.id.root.clone())
            .unwrap_or_default();

        match Self::fetch_crate_graph(transport).await {
            Ok((crate_names, pairs)) if !crate_names.is_empty() => {
                let pair_count = pairs.len();
                Self::emit_crate_graph_edges(&crate_names, &pairs, &root_id, result);
                tracing::info!(
                    "LSP Pass 0 complete in {:?}: {} crate nodes, {} DependsOn edges",
                    pass0_start.elapsed(),
                    crate_names.len(),
                    pair_count
                );
            }
            Ok(_) => {
                tracing::debug!("LSP Pass 0: viewCrateGraph returned no crates");
            }
            Err(e) => {
                tracing::debug!("LSP Pass 0: viewCrateGraph failed: {}", e);
            }
        }
    }

    // ------------------------------------------------------------------
    // Pass 1: call hierarchy, find_implementations, references, and document links.
    // Pipelined with adaptive concurrency (TCP slow-start).
    //
    // Returns (attempted, errors, aborted).
    // ------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_pass1_references(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        matching_nodes_owned: &Arc<Vec<Node>>,
        refs_by_file_shared: &Arc<HashMap<PathBuf, Vec<Node>>>,
        has_references: bool,
        has_call_hierarchy: bool,
        result: &mut EnrichmentResult,
    ) -> (u32, u32, bool) {
        let pass1_start = std::time::Instant::now();
        let language = self.language.clone();

        // Filter to only nodes that need LSP requests:
        // Functions (call hierarchy), Traits (implementations), and Other (document links).
        // Skip test functions -- they don't have meaningful cross-file callers
        // and halve the total RPC count.
        // Also skip diagnostic nodes (Other("diagnostic")) to prevent them from being
        // re-enriched via the generic Other/documentLink path on subsequent passes --
        // which would generate spurious DependsOn edges from diagnostics.
        let enrichable_nodes: Vec<&Node> = matching_nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Function
                        | NodeKind::Trait
                        | NodeKind::Other(_)
                        | NodeKind::Struct
                        | NodeKind::Enum
                        | NodeKind::TypeAlias
                        | NodeKind::Const
                )
            })
            .filter(|n| !matches!(&n.id.kind, NodeKind::Other(s) if s == "diagnostic"))
            .filter(|n| {
                // Skip test functions (have #[test] or #[tokio::test] decorator)
                if n.id.kind == NodeKind::Function {
                    if let Some(decorators) = n.metadata.get("decorators")
                        && (decorators.contains("#[test]") || decorators.contains("#[tokio::test]"))
                    {
                        return false;
                    }
                    // Also skip functions in test files
                    if crate::ranking::is_test_file(n) {
                        return false;
                    }
                }
                true
            })
            .copied()
            .collect();

        let ref_eligible = enrichable_nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const
                )
            })
            .count();
        tracing::info!(
            "LSP pipeline: {} enrichable nodes out of {} total ({}f, {}t, {}r, {}o) [references={}, call_hierarchy={}]",
            enrichable_nodes.len(),
            matching_nodes.len(),
            enrichable_nodes
                .iter()
                .filter(|n| n.id.kind == NodeKind::Function)
                .count(),
            enrichable_nodes
                .iter()
                .filter(|n| n.id.kind == NodeKind::Trait)
                .count(),
            ref_eligible,
            enrichable_nodes
                .iter()
                .filter(|n| matches!(n.id.kind, NodeKind::Other(_)))
                .count(),
            has_references,
            has_call_hierarchy,
        );

        // Concurrency control: TCP slow-start from 4 to 64.
        const PIPELINE_MAX_CONCURRENCY: usize = 64;
        let concurrency_limit = Arc::new(tokio::sync::Semaphore::new(4));
        let mut join_set = tokio::task::JoinSet::new();

        let completed = Arc::new(AtomicI64::new(0));
        let error_count = Arc::new(AtomicI64::new(0));
        let ramped_up = Arc::new(AtomicBool::new(false));

        for node in &enrichable_nodes {
            let node = (*node).clone();
            let transport = Arc::clone(transport);
            let root = root.to_path_buf();
            let matching_owned = Arc::clone(matching_nodes_owned);
            let refs_by_file = Arc::clone(refs_by_file_shared);
            let language = language.clone();
            let sem = Arc::clone(&concurrency_limit);
            let completed = Arc::clone(&completed);
            let error_count = Arc::clone(&error_count);
            let ramped_up = Arc::clone(&ramped_up);

            join_set.spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                let abs_path = root.join(&node.id.file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => {
                        completed.fetch_add(1, Ordering::Relaxed);
                        return (Vec::new(), Vec::new(), false);
                    }
                };

                let (line, col) = Self::node_lsp_position(&node);
                let mut edges = Vec::new();
                let mut new_nodes = Vec::new();
                let mut had_error = false;

                match node.id.kind {
                    NodeKind::Function => {
                        Self::enrich_function_node(
                            &transport,
                            &file_uri,
                            line,
                            col,
                            &node,
                            &matching_owned,
                            &refs_by_file,
                            &root,
                            &language,
                            has_references,
                            has_call_hierarchy,
                            &mut edges,
                            &mut new_nodes,
                            &mut had_error,
                            &error_count,
                        )
                        .await;
                    }
                    NodeKind::Trait => {
                        Self::enrich_trait_node(
                            &transport,
                            &file_uri,
                            line,
                            col,
                            &node,
                            &matching_owned,
                            &root,
                            &mut edges,
                        )
                        .await;
                    }
                    NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const => {
                        if has_references {
                            Self::enrich_type_references(
                                &transport,
                                &file_uri,
                                line,
                                col,
                                &node,
                                &matching_owned,
                                &root,
                                &mut edges,
                                &mut had_error,
                                &error_count,
                            )
                            .await;
                        }
                    }
                    _ => {
                        if matches!(node.id.kind, NodeKind::Other(_)) {
                            Self::enrich_document_links(
                                &transport, &file_uri, &node, &root, &mut edges,
                            )
                            .await;
                        }
                    }
                }

                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                // Ramp up after 4 successful completions (TCP slow-start exit)
                if done >= 4 && !had_error && !ramped_up.swap(true, Ordering::Relaxed) {
                    let added = PIPELINE_MAX_CONCURRENCY - 4;
                    sem.add_permits(added);
                    tracing::info!(
                        "LSP pipeline: ramp-up to {} concurrent",
                        PIPELINE_MAX_CONCURRENCY
                    );
                }
                (edges, new_nodes, had_error)
            });
        }

        // Collect results from all concurrent tasks
        let mut attempted = 0u32;
        let mut errors = 0u32;
        let mut aborted = false;
        let mut seen_virtual_ids = std::collections::HashSet::new();
        let total_nodes = enrichable_nodes.len();
        let mut last_progress_log = std::time::Instant::now();
        let mut last_logged_count = 0u64;
        const PROGRESS_LOG_INTERVAL_SECS: u64 = 30;
        const PROGRESS_LOG_INTERVAL_NODES: u64 = 1_000;

        while let Some(task_result) = join_set.join_next().await {
            match task_result {
                Ok((edges, new_nodes, had_error)) => {
                    attempted += 1;
                    if had_error {
                        errors += 1;
                    }
                    result.added_edges.extend(edges);
                    for vnode in new_nodes {
                        if seen_virtual_ids.insert(vnode.id.clone()) {
                            result.new_nodes.push(vnode);
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    tracing::debug!("LSP enrichment task panicked: {}", e);
                }
            }

            // Log progress every 1,000 nodes or every 30 seconds (whichever comes first)
            let done = completed.load(Ordering::Relaxed) as u64;
            let elapsed_since_log = last_progress_log.elapsed().as_secs();
            let nodes_since_log = done.saturating_sub(last_logged_count);
            if done > 0
                && (nodes_since_log >= PROGRESS_LOG_INTERVAL_NODES
                    || elapsed_since_log >= PROGRESS_LOG_INTERVAL_SECS)
            {
                let elapsed_total = pass1_start.elapsed().as_secs_f64();
                let rate = done as f64 / elapsed_total;
                let remaining = if rate > 0.0 {
                    let remaining_nodes = (total_nodes as f64) - (done as f64);
                    let remaining_secs = remaining_nodes / rate;
                    if remaining_secs >= 120.0 {
                        format!("~{} min remaining", (remaining_secs / 60.0).round() as u64)
                    } else {
                        format!("~{}s remaining", remaining_secs.round() as u64)
                    }
                } else {
                    "estimating...".to_string()
                };
                tracing::info!(
                    "LSP: {} processing... {}/{} nodes ({} edges found, {})",
                    self.server_command,
                    done,
                    total_nodes,
                    result.added_edges.len(),
                    remaining,
                );
                last_progress_log = std::time::Instant::now();
                last_logged_count = done;
            }

            // Early abort: if we've processed >= 1,000 nodes AND warmed up for >= 30s,
            // OR spent >= 2 minutes with 0 edges, the language server is likely
            // misconfigured.
            if result.added_edges.is_empty()
                && ((attempted >= ZERO_EDGE_ABORT_THRESHOLD
                    && pass1_start.elapsed() >= ZERO_EDGE_MIN_WARMUP)
                    || pass1_start.elapsed() > ZERO_EDGE_TIMEOUT)
            {
                tracing::warn!(
                    "LSP: {} produced 0 edges after {}/{} nodes ({:.1}s) -- aborting (likely misconfigured)",
                    self.server_command,
                    attempted,
                    total_nodes,
                    pass1_start.elapsed().as_secs_f64(),
                );
                aborted = true;
                join_set.abort_all();
                break;
            }
        }

        tracing::info!(
            "LSP Pass 1 complete in {:?}: {} edges from {} nodes ({} errors)",
            pass1_start.elapsed(),
            result.added_edges.len(),
            attempted,
            errors,
        );

        (attempted, errors, aborted)
    }

    // ------------------------------------------------------------------
    // Pass 1 helpers: per-node-kind enrichment functions
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn enrich_function_node(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        matching_owned: &Arc<Vec<Node>>,
        refs_by_file: &Arc<HashMap<PathBuf, Vec<Node>>>,
        root: &Path,
        language: &str,
        has_references: bool,
        has_call_hierarchy: bool,
        edges: &mut Vec<Edge>,
        new_nodes: &mut Vec<Node>,
        had_error: &mut bool,
        error_count: &AtomicI64,
    ) {
        if !has_call_hierarchy && has_references {
            match Self::find_references_p(transport, file_uri, line, col).await {
                Ok(locations) => {
                    for loc in &locations {
                        let ref_path = uri_to_relative_path(&loc.uri, root);
                        let ref_line = loc.range.start.line as usize + 1;

                        if ref_path.to_string_lossy().contains(".cargo")
                            || ref_path.to_string_lossy().contains("site-packages")
                        {
                            continue;
                        }

                        if ref_path == node.id.file
                            && ref_line >= node.line_start
                            && ref_line <= node.line_end
                        {
                            continue;
                        }

                        let referrer_id =
                            refs_by_file.get(ref_path.as_path()).and_then(|candidates| {
                                let refs: Vec<&Node> = candidates.iter().collect();
                                find_enclosing_symbol(&refs, &ref_path, ref_line)
                            });

                        if let Some(referrer) = referrer_id {
                            if referrer == node.id {
                                continue;
                            }
                            edges.push(Edge {
                                from: referrer,
                                to: node.id.clone(),
                                kind: EdgeKind::Calls,
                                source: ExtractionSource::Lsp,
                                confidence: Confidence::Detected,
                            });
                        }
                    }
                }
                Err(e) => {
                    *had_error = true;
                    error_count.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("references lookup failed for {}: {}", node.id.name, e);
                }
            }
        } else if has_call_hierarchy {
            match Self::prepare_call_hierarchy_p(transport, file_uri, line, col).await {
                Ok(Some(item)) => {
                    let (incoming_result, outgoing_result) = tokio::join!(
                        Self::incoming_calls_p(transport, &item),
                        Self::outgoing_calls_p(transport, &item),
                    );

                    let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                    let mut refs_by_file_name: std::collections::HashMap<
                        (PathBuf, String),
                        Vec<NodeId>,
                    > = std::collections::HashMap::new();
                    for n in &matching_refs {
                        refs_by_file_name
                            .entry((n.id.file.clone(), n.id.name.clone()))
                            .or_default()
                            .push(n.id.clone());
                    }

                    // Process incoming calls
                    if let Ok(calls) = incoming_result {
                        for call in &calls {
                            let caller_uri = &call["from"]["uri"];
                            let caller_name = call["from"]["name"].as_str().unwrap_or("");
                            let caller_line =
                                call["from"]["range"]["start"]["line"].as_u64().unwrap_or(0)
                                    as usize
                                    + 1;

                            if let Some(uri_str) = caller_uri.as_str() {
                                let caller_path = if let Some(p) = uri_str.strip_prefix("file://") {
                                    let abs = PathBuf::from(p);
                                    abs.strip_prefix(root).unwrap_or(&abs).to_path_buf()
                                } else {
                                    continue;
                                };

                                if caller_path.to_string_lossy().contains(".cargo") {
                                    continue;
                                }

                                let key = (caller_path.clone(), caller_name.to_string());
                                let caller_id = match refs_by_file_name.get(&key) {
                                    Some(ids) if ids.len() == 1 => Some(ids[0].clone()),
                                    Some(_) => find_enclosing_symbol(
                                        &matching_refs,
                                        &caller_path,
                                        caller_line,
                                    ),
                                    None => find_enclosing_symbol(
                                        &matching_refs,
                                        &caller_path,
                                        caller_line,
                                    ),
                                };

                                if let Some(caller) = caller_id {
                                    if caller.name == node.id.name && caller.file == node.id.file {
                                        continue;
                                    }
                                    edges.push(Edge {
                                        from: caller,
                                        to: node.id.clone(),
                                        kind: EdgeKind::Calls,
                                        source: ExtractionSource::Lsp,
                                        confidence: Confidence::Confirmed,
                                    });
                                }
                            }
                        }
                    }

                    // Process outgoing calls
                    if let Ok(calls) = outgoing_result {
                        for call in &calls {
                            let callee_uri = &call["to"]["uri"];
                            let callee_name = call["to"]["name"].as_str().unwrap_or("");
                            let callee_line =
                                call["to"]["range"]["start"]["line"].as_u64().unwrap_or(0) as usize
                                    + 1;

                            if let Some(uri_str) = callee_uri.as_str() {
                                let callee_path = if let Some(p) = uri_str.strip_prefix("file://") {
                                    let abs = PathBuf::from(p);
                                    abs.strip_prefix(root).unwrap_or(&abs).to_path_buf()
                                } else {
                                    continue;
                                };

                                if callee_path.to_string_lossy().contains(".cargo") {
                                    let fqn = call["to"]["detail"]
                                        .as_str()
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or(callee_name);

                                    if fqn.is_empty() {
                                        continue;
                                    }

                                    let package = fqn.split("::").next().unwrap_or(fqn).to_string();

                                    let virtual_id = NodeId {
                                        root: "external".to_string(),
                                        file: PathBuf::new(),
                                        name: fqn.to_string(),
                                        kind: NodeKind::Function,
                                    };

                                    let mut meta = std::collections::BTreeMap::new();
                                    meta.insert("package".to_string(), package.clone());
                                    meta.insert("virtual".to_string(), "true".to_string());
                                    new_nodes.push(Node {
                                        id: virtual_id.clone(),
                                        language: language.to_string(),
                                        line_start: 0,
                                        line_end: 0,
                                        signature: fqn.to_string(),
                                        body: String::new(),
                                        metadata: meta,
                                        source: ExtractionSource::Lsp,
                                    });

                                    edges.push(Edge {
                                        from: node.id.clone(),
                                        to: virtual_id,
                                        kind: EdgeKind::Calls,
                                        source: ExtractionSource::Lsp,
                                        confidence: Confidence::Detected,
                                    });
                                    continue;
                                }

                                let key = (callee_path.clone(), callee_name.to_string());
                                let callee_id = match refs_by_file_name.get(&key) {
                                    Some(ids) if ids.len() == 1 => Some(ids[0].clone()),
                                    Some(_) => find_enclosing_symbol(
                                        &matching_refs,
                                        &callee_path,
                                        callee_line,
                                    ),
                                    None => find_enclosing_symbol(
                                        &matching_refs,
                                        &callee_path,
                                        callee_line,
                                    ),
                                };

                                if let Some(callee) = callee_id {
                                    if callee.name == node.id.name && callee.file == node.id.file {
                                        continue;
                                    }
                                    edges.push(Edge {
                                        from: node.id.clone(),
                                        to: callee,
                                        kind: EdgeKind::Calls,
                                        source: ExtractionSource::Lsp,
                                        confidence: Confidence::Confirmed,
                                    });
                                }
                            }
                        }
                    }
                }
                Ok(None) => {} // No call hierarchy item
                Err(e) => {
                    *had_error = true;
                    error_count.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("prepareCallHierarchy failed for {}: {}", node.id.name, e);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_trait_node(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        matching_owned: &Arc<Vec<Node>>,
        root: &Path,
        edges: &mut Vec<Edge>,
    ) {
        match Self::find_implementations_p(transport, file_uri, line, col).await {
            Ok(locations) => {
                let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                for loc in locations {
                    let impl_path = uri_to_relative_path(&loc.uri, root);
                    let impl_line = loc.range.start.line as usize + 1;

                    if impl_path.to_string_lossy().contains(".cargo") {
                        continue;
                    }

                    let impl_id = matching_refs
                        .iter()
                        .filter(|n| n.id.file == impl_path)
                        .filter(|n| matches!(n.id.kind, NodeKind::Impl | NodeKind::Struct))
                        .filter(|n| n.line_start <= impl_line && n.line_end >= impl_line)
                        .min_by_key(|n| n.line_end - n.line_start)
                        .map(|n| n.id.clone());

                    if let Some(implementor) = impl_id {
                        edges.push(Edge {
                            from: implementor,
                            to: node.id.clone(),
                            kind: EdgeKind::Implements,
                            source: ExtractionSource::Lsp,
                            confidence: Confidence::Confirmed,
                        });
                    }
                }
            }
            Err(e) => {
                tracing::debug!("Implementation lookup failed for {}: {}", node.id.name, e);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_type_references(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        matching_owned: &Arc<Vec<Node>>,
        root: &Path,
        edges: &mut Vec<Edge>,
        had_error: &mut bool,
        error_count: &AtomicI64,
    ) {
        match Self::find_references_p(transport, file_uri, line, col).await {
            Ok(locations) => {
                let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                let mut refs_by_file: HashMap<&Path, Vec<&Node>> = HashMap::new();
                for n in &matching_refs {
                    refs_by_file
                        .entry(n.id.file.as_path())
                        .or_default()
                        .push(*n);
                }
                for loc in &locations {
                    let ref_path = uri_to_relative_path(&loc.uri, root);
                    let ref_line = loc.range.start.line as usize + 1;

                    if ref_path.to_string_lossy().contains(".cargo") {
                        continue;
                    }

                    if ref_path == node.id.file
                        && ref_line >= node.line_start
                        && ref_line <= node.line_end
                    {
                        continue;
                    }

                    let referrer_id = refs_by_file.get(ref_path.as_path()).and_then(|candidates| {
                        find_enclosing_symbol(candidates, &ref_path, ref_line)
                    });

                    if let Some(referrer) = referrer_id {
                        if referrer == node.id {
                            continue;
                        }
                        edges.push(Edge {
                            from: referrer,
                            to: node.id.clone(),
                            kind: EdgeKind::ReferencedBy,
                            source: ExtractionSource::Lsp,
                            confidence: Confidence::Confirmed,
                        });
                    }
                }
            }
            Err(e) => {
                *had_error = true;
                error_count.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("textDocument/references failed for {}: {}", node.id.name, e);
            }
        }
    }

    async fn enrich_document_links(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        node: &Node,
        root: &Path,
        edges: &mut Vec<Edge>,
    ) {
        if let Ok(links) = Self::document_links_p(transport, file_uri).await {
            for link in &links {
                if let Some(target) = link.get("target").and_then(|t| t.as_str())
                    && let Some(target_path) = target.strip_prefix("file://")
                {
                    let rel_target = PathBuf::from(target_path);
                    let rel_target = rel_target
                        .strip_prefix(root)
                        .unwrap_or(&rel_target)
                        .to_path_buf();

                    if rel_target.to_string_lossy().starts_with("http") {
                        continue;
                    }

                    let target_id = NodeId {
                        root: node.id.root.clone(),
                        file: rel_target.clone(),
                        name: rel_target
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        kind: NodeKind::Module,
                    };

                    edges.push(Edge {
                        from: node.id.clone(),
                        to: target_id,
                        kind: EdgeKind::DependsOn,
                        source: ExtractionSource::Lsp,
                        confidence: Confidence::Confirmed,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Pass 2: type hierarchy (sequential -- strike counting needs order)
    // ------------------------------------------------------------------
    pub(super) async fn run_pass2_type_hierarchy(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        mut has_type_hierarchy: bool,
        mut type_hierarchy_strikes: u32,
        result: &mut EnrichmentResult,
    ) -> (bool, u32) {
        if !has_type_hierarchy {
            return (has_type_hierarchy, type_hierarchy_strikes);
        }

        let type_nodes: Vec<&Node> = matching_nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Trait | NodeKind::Struct | NodeKind::Enum
                )
            })
            .copied()
            .collect();

        if !type_nodes.is_empty() {
            tracing::debug!("Type hierarchy pass: {} eligible nodes", type_nodes.len());
        }

        let pass2_start = std::time::Instant::now();
        let mut pass2_done = 0u64;
        let pass2_total = type_nodes.len();
        let edges_before_pass2 = result.added_edges.len();
        let mut pass2_last_log = std::time::Instant::now();
        let mut pass2_last_count = 0u64;

        for node in &type_nodes {
            let abs_path = root.join(&node.id.file);
            let file_uri = match path_to_uri(&abs_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let (line, col) = Self::node_lsp_position(node);

            let ok = Self::enrich_type_hierarchy_p(
                transport,
                &file_uri,
                line,
                col,
                node,
                matching_nodes,
                root,
                result,
            )
            .await;

            Self::update_type_hierarchy_strikes(
                ok,
                &mut type_hierarchy_strikes,
                &mut has_type_hierarchy,
            );

            pass2_done += 1;

            // Log progress every 500 nodes or every 30 seconds
            let since_log = pass2_last_log.elapsed().as_secs();
            let nodes_since = pass2_done - pass2_last_count;
            if nodes_since >= 500 || since_log >= 30 {
                let elapsed = pass2_start.elapsed().as_secs_f64();
                let rate = pass2_done as f64 / elapsed;
                let remaining_secs = if rate > 0.0 {
                    ((pass2_total as f64) - (pass2_done as f64)) / rate
                } else {
                    0.0
                };
                let remaining = if remaining_secs >= 120.0 {
                    format!("~{} min remaining", (remaining_secs / 60.0).round() as u64)
                } else {
                    format!("~{}s remaining", remaining_secs.round() as u64)
                };
                tracing::info!(
                    "LSP: {} type hierarchy... {}/{} nodes ({} edges total, {})",
                    self.server_command,
                    pass2_done,
                    pass2_total,
                    result.added_edges.len(),
                    remaining,
                );
                pass2_last_log = std::time::Instant::now();
                pass2_last_count = pass2_done;
            }

            // Early abort: 0 new edges after 1,000 nodes + 30s warmup, OR 2 minutes
            if result.added_edges.len() == edges_before_pass2
                && ((pass2_done >= ZERO_EDGE_ABORT_THRESHOLD as u64
                    && pass2_start.elapsed() >= ZERO_EDGE_MIN_WARMUP)
                    || pass2_start.elapsed() > ZERO_EDGE_TIMEOUT)
            {
                tracing::warn!(
                    "LSP: {} type hierarchy produced 0 edges after {}/{} nodes ({:.1}s) -- aborting (likely misconfigured)",
                    self.server_command,
                    pass2_done,
                    pass2_total,
                    pass2_start.elapsed().as_secs_f64(),
                );
                break;
            }

            if !has_type_hierarchy {
                break;
            }
        }

        (has_type_hierarchy, type_hierarchy_strikes)
    }

    // ------------------------------------------------------------------
    // Pass 4: BelongsTo edges -- module hierarchy (#396).
    // ------------------------------------------------------------------
    pub(super) async fn run_pass4_belongs_to(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        result: &mut EnrichmentResult,
    ) {
        let pass4_start = std::time::Instant::now();
        let edges_before = result.added_edges.len();

        // Group matching_nodes by file
        let mut nodes_by_file: HashMap<PathBuf, Vec<&Node>> = HashMap::new();
        for n in matching_nodes {
            nodes_by_file.entry(n.id.file.clone()).or_default().push(n);
        }

        let is_rust = self.language == "rust";

        for (rel_file, file_nodes) in &nodes_by_file {
            Self::emit_belongs_to_edges(transport, file_nodes, rel_file, root, is_rust, result)
                .await;
        }

        // Remove duplicate module nodes (same stable_id emitted for multiple files in same dir)
        let mut deduplicated_new_nodes = Vec::with_capacity(result.new_nodes.len());
        let mut module_stable_ids_seen = std::collections::HashSet::new();
        for node in result.new_nodes.drain(..) {
            if matches!(node.id.kind, NodeKind::Module) {
                let sid = node.id.to_stable_id();
                if module_stable_ids_seen.insert(sid) {
                    deduplicated_new_nodes.push(node);
                }
                // else: skip duplicate
            } else {
                deduplicated_new_nodes.push(node);
            }
        }
        result.new_nodes = deduplicated_new_nodes;

        let belongs_to_count = result.added_edges.len() - edges_before;
        let module_node_count = result
            .new_nodes
            .iter()
            .filter(|n| matches!(n.id.kind, NodeKind::Module))
            .count();
        if belongs_to_count > 0 {
            tracing::info!(
                "LSP Pass 4 complete in {:?}: {} BelongsTo edges, {} module nodes",
                pass4_start.elapsed(),
                belongs_to_count,
                module_node_count
            );
        }
    }

    // ------------------------------------------------------------------
    // Pass 5: InlayHints -- inferred types in embeddings (#408).
    // ------------------------------------------------------------------
    pub(super) async fn run_pass5_inlay_hints(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        has_inlay_hints: bool,
        result: &mut EnrichmentResult,
    ) {
        if !has_inlay_hints {
            return;
        }

        let pass5_start = std::time::Instant::now();
        let mut hint_patches = 0usize;

        let mut nodes_by_file: HashMap<PathBuf, Vec<&Node>> = HashMap::new();
        for n in matching_nodes {
            nodes_by_file.entry(n.id.file.clone()).or_default().push(n);
        }

        for (rel_file, file_nodes) in &nodes_by_file {
            let abs_path = root.join(rel_file);
            let file_uri = match path_to_uri(&abs_path) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let max_line = file_nodes
                .iter()
                .map(|n| n.line_end as u32)
                .max()
                .unwrap_or(0);

            match Self::inlay_hints_for_file(transport, &file_uri, max_line + 1).await {
                Ok(hints) if !hints.is_empty() => {
                    let type_map = Self::group_inlay_hints_by_node(&hints, file_nodes);
                    for (stable_id, type_str) in type_map {
                        result.updated_nodes.push((stable_id, {
                            let mut patch = std::collections::BTreeMap::new();
                            patch.insert("inferred_types".to_string(), type_str);
                            patch
                        }));
                        hint_patches += 1;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(
                        "textDocument/inlayHint failed for {}: {}",
                        rel_file.display(),
                        e
                    );
                }
            }
        }

        if hint_patches > 0 {
            tracing::info!(
                "LSP Pass 5 complete in {:?}: {} nodes patched with inferred_types",
                pass5_start.elapsed(),
                hint_patches
            );
        }
    }

    // ------------------------------------------------------------------
    // Pass 3: diagnostics.
    //
    // Strategy: prefer pull-based diagnostics (textDocument/diagnostic,
    // LSP 3.17+) when the server advertised `diagnosticProvider`. For
    // servers that only push, fall back to the pipelined reader loop capture.
    // ------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_pass3_diagnostics(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        has_pull_diagnostics: bool,
        diag_sink: &Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
        repo_root: &Path,
        result: &mut EnrichmentResult,
    ) {
        let diag_timestamp = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string())
        };

        let unique_files: Vec<PathBuf> = {
            let mut seen = std::collections::HashSet::new();
            matching_nodes
                .iter()
                .map(|n| n.id.file.clone())
                .filter(|f| seen.insert(f.clone()))
                .collect()
        };

        let root_id = matching_nodes
            .first()
            .map(|n| n.id.root.clone())
            .unwrap_or_default();

        let lsp_config = LspConfig::load(repo_root);
        let max_severity_int = lsp_config.diagnostic_min_severity.max_severity_int();

        if has_pull_diagnostics {
            tracing::info!(
                "LSP diagnostics pass: pull-based for {} files ({})",
                unique_files.len(),
                self.server_command
            );
            let mut pull_raw_total = 0usize;
            let mut pull_files_with_diags = 0usize;
            for rel_file in &unique_files {
                let abs_path = root.join(rel_file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => continue,
                };
                match Self::pull_diagnostics_p(transport, &file_uri).await {
                    Ok(diags) => {
                        if !diags.is_empty() {
                            pull_raw_total += diags.len();
                            pull_files_with_diags += 1;
                            tracing::debug!(
                                "textDocument/diagnostic: {} raw items for {}",
                                diags.len(),
                                rel_file.display()
                            );
                        }
                        let nodes = Self::build_diagnostic_nodes(
                            file_uri.as_str(),
                            &diags,
                            root,
                            &root_id,
                            &self.server_command,
                            &self.language,
                            &diag_timestamp,
                            max_severity_int,
                        );
                        result.new_nodes.extend(nodes);
                    }
                    Err(e) => {
                        tracing::debug!(
                            "textDocument/diagnostic failed for {}: {}",
                            rel_file.display(),
                            e
                        );
                    }
                }
            }
            tracing::info!(
                "LSP diagnostics pass: pull complete -- {} raw items from {} files with diagnostics (out of {} files)",
                pull_raw_total,
                pull_files_with_diags,
                unique_files.len()
            );
        } else {
            let expected_uris: std::collections::HashSet<String> = unique_files
                .iter()
                .filter_map(|rel_file| {
                    path_to_uri(&root.join(rel_file))
                        .ok()
                        .map(|u| u.to_string())
                })
                .collect();

            let captured: HashMap<String, Vec<serde_json::Value>> = {
                let sink = diag_sink.lock().unwrap();
                sink.clone()
            };
            let relevant_count = captured
                .keys()
                .filter(|u| expected_uris.contains(*u))
                .count();
            tracing::info!(
                "LSP diagnostics pass: push-captured {}/{} relevant files with diagnostics ({})",
                relevant_count,
                captured.len(),
                self.server_command
            );
            for (uri, diags) in &captured {
                if !expected_uris.contains(uri) {
                    continue;
                }
                let nodes = Self::build_diagnostic_nodes(
                    uri,
                    diags,
                    root,
                    &root_id,
                    &self.server_command,
                    &self.language,
                    &diag_timestamp,
                    max_severity_int,
                );
                result.new_nodes.extend(nodes);
            }
        }
    }
}
