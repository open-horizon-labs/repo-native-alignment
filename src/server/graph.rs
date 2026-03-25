//! Graph lifecycle: building, incremental updates, and state management.
// EXTRACTION_VERSION is deprecated (#526) but still used for backward-compat migration.
#![allow(deprecated)]

use std::path::PathBuf;
use std::sync::Arc;

/// Metadata key for subsystem cluster assignment.
/// Shared with service.rs for filtering.
pub(crate) const SUBSYSTEM_KEY: &str = "subsystem";

use crate::embed::EmbeddingIndex;
use crate::extract::ExtractorRegistry;
use crate::graph::{Edge, Node, NodeKind};
use crate::graph::index::GraphIndex;
use crate::graph::store::SCHEMA_VERSION;
use crate::roots::{RootConfig, WorkspaceConfig, cache_state_path};
use crate::scanner::{ScanResult, Scanner};

use super::helpers;
use super::state::GraphState;
use super::store::{
    check_and_migrate_extraction_version, check_and_migrate_schema, delete_nodes_for_roots,
    get_stored_root_ids, graph_lance_path, load_graph_from_lance, persist_graph_incremental,
    persist_graph_to_lance,
};
use super::RnaHandler;

impl RnaHandler {
    /// Maximum time to wait for pre-warm to finish before falling back to
    /// building the graph ourselves.
    const PREWARM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    /// Ensure graph is built, check for file changes since last scan.
    ///
    /// Returns an `Arc<GraphState>` loaded via atomic pointer swap (ArcSwap).
    /// Tool calls never block on a write lock — they always see the last
    /// complete graph snapshot (#574).
    pub(crate) async fn get_graph(&self) -> anyhow::Result<Arc<GraphState>> {
        // Fast path: graph exists and scan cooldown hasn't expired.
        // ArcSwap load is an atomic pointer read — zero blocking.
        let current = self.graph.load_full();
        if let Some(ref gs) = *current {
            let skip_scan = {
                let last = self.last_scan.lock().unwrap();
                last.elapsed() < std::time::Duration::from_secs(2)
            };
            if skip_scan {
                return Ok(Arc::clone(gs));
            }

            // Check for changes via scanner
            let mut scanner = Scanner::new(self.repo_root.clone())?;
            let scan = scanner.scan()?;
            if scan.changed_files.is_empty()
                && scan.new_files.is_empty()
                && scan.deleted_files.is_empty()
            {
                // No changes -- safe to commit state (records current mtimes)
                scanner.commit_state()?;
                *self.last_scan.lock().unwrap() = std::time::Instant::now();
                return Ok(Arc::clone(gs));
            }

            // Changes detected (#574): fast tree-sitter extraction inline,
            // then spawn full pipeline in background per ADR-001.
            //
            // 1. Run tree-sitter extraction (fast, <1s)
            // 2. Apply to cloned graph, ArcSwap immediately
            // 3. Spawn full pipeline (LSP, passes, PageRank, embed, persist) as background task
            // 4. Return immediately with tree-sitter results
            let _build_guard = self.graph_build_lock.lock().await;
            // Re-check after acquiring lock — another call may have already updated.
            let current2 = self.graph.load_full();
            if let Some(ref gs2) = *current2 {
                if !Arc::ptr_eq(gs, gs2) {
                    // Graph was updated while we waited for the lock.
                    scanner.commit_state()?;
                    *self.last_scan.lock().unwrap() = std::time::Instant::now();
                    return Ok(Arc::clone(gs2));
                }
            }

            // Fast path: tree-sitter extraction only (no LSP, no passes, no persist).
            let mut fast_state = (**gs).clone();
            let registry = ExtractorRegistry::with_builtins();
            let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();

            // Remove nodes/edges for deleted + changed files
            let files_to_remove: Vec<PathBuf> = scan
                .deleted_files
                .iter()
                .chain(scan.changed_files.iter())
                .cloned()
                .collect();
            fast_state.nodes.retain(|n| !files_to_remove.contains(&n.id.file));
            fast_state.edges.retain(|e| {
                !files_to_remove.iter().any(|f| e.from.file == *f || e.to.file == *f)
            });

            // Extract new + changed files via tree-sitter
            let (mut extraction, _enc_stats) = registry.extract_scan_result_with_stats(&self.repo_root, &scan);
            for node in &mut extraction.nodes {
                node.id.root = primary_slug.clone();
            }
            let file_index: std::collections::HashSet<String> = fast_state.nodes
                .iter()
                .chain(extraction.nodes.iter())
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect();
            for edge in &mut extraction.edges {
                edge.from.root = primary_slug.clone();
                edge.to.root = primary_slug.clone();
                helpers::resolve_edge_target_by_suffix(edge, &file_index);
            }
            fast_state.nodes.extend(extraction.nodes);
            fast_state.edges.extend(extraction.edges);

            // Rebuild petgraph index for the fast graph
            fast_state.index = crate::graph::index::GraphIndex::new();
            fast_state.index.rebuild_from_edges(&fast_state.edges);
            for node in &fast_state.nodes {
                fast_state.index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }

            // Atomic swap: tool calls see tree-sitter results immediately
            let fast_arc = Arc::new(fast_state);
            self.graph.store(Arc::new(Some(Arc::clone(&fast_arc))));
            *self.last_scan.lock().unwrap() = std::time::Instant::now();

            // Spawn full pipeline in background (LSP, passes, PageRank, subsystem,
            // embedding, LanceDB persist). When it completes, it ArcSwaps the fully
            // enriched graph in — subsequent tool calls see the enriched version.
            // Scanner state is committed only after the full pipeline succeeds.
            {
                let handler_graph = Arc::clone(&self.graph);
                let repo_root = self.repo_root.clone();
                let fast_arc_for_check = Arc::clone(&fast_arc);
                let scan_stats = Arc::clone(&self.scan_stats);
                let lance_write_lock = Arc::clone(&self.lance_write_lock);
                let embed_index = Arc::clone(&self.embed_index);
                let lsp_status = Arc::clone(&self.lsp_status);
                let graph_build_lock = Arc::clone(&self.graph_build_lock);
                // Clone the pre-fast-path graph so the full pipeline starts from
                // the same base state (with all existing LSP edges, PageRank, etc.)
                let base_state = (**gs).clone();
                // Release the build lock before spawning so the background task
                // can re-acquire it. The fast graph is already swapped in.
                drop(_build_guard);
                tokio::spawn(async move {
                    tracing::info!(
                        "Spawned background incremental pipeline: {} changed, {} new, {} deleted",
                        scan.changed_files.len(), scan.new_files.len(), scan.deleted_files.len()
                    );
                    // Serialize with other graph builds to prevent concurrent pipeline runs.
                    let _build_guard = graph_build_lock.lock().await;
                    // Re-check: if the graph was swapped since we spawned, another pipeline
                    // may have already processed these changes. Check by comparing with the
                    // fast_arc we stored — if they differ, someone else updated.
                    let current_snap = handler_graph.load_full();
                    if let Some(ref current_gs) = *current_snap {
                        if !Arc::ptr_eq(current_gs, &fast_arc_for_check) {
                            // Another update happened; skip this pipeline run.
                            tracing::info!("Background incremental pipeline: graph already updated, skipping");
                            if let Err(e) = scanner.commit_state() {
                                tracing::error!("Failed to commit scanner state: {}", e);
                            }
                            return;
                        }
                    }

                    // Run the full pipeline on the base state (which has LSP edges, etc.)
                    let mut full_state = base_state;
                    let primary_slug = RootConfig::code_project(repo_root.clone()).slug();

                    // Remove nodes/edges for deleted + changed files (same as fast path)
                    let files_to_remove: Vec<PathBuf> = scan
                        .deleted_files
                        .iter()
                        .chain(scan.changed_files.iter())
                        .cloned()
                        .collect();
                    let deleted_edge_ids: Vec<String> = full_state.edges
                        .iter()
                        .filter(|e| files_to_remove.iter().any(|f| e.from.file == *f || e.to.file == *f))
                        .map(|e| e.stable_id())
                        .collect();
                    full_state.nodes.retain(|n| !files_to_remove.contains(&n.id.file));
                    full_state.edges.retain(|e| {
                        !files_to_remove.iter().any(|f| e.from.file == *f || e.to.file == *f)
                    });

                    // Extract new + changed files
                    let registry = ExtractorRegistry::with_builtins();
                    let (mut extraction, enc_stats) = registry.extract_scan_result_with_stats(&repo_root, &scan);
                    if let Ok(mut stats) = scan_stats.write() {
                        stats.merge_encoding_stats(&primary_slug, &enc_stats);
                    }
                    for node in &mut extraction.nodes {
                        node.id.root = primary_slug.clone();
                    }
                    let file_index: std::collections::HashSet<String> = full_state.nodes
                        .iter()
                        .chain(extraction.nodes.iter())
                        .map(|n| n.id.file.to_string_lossy().to_string())
                        .collect();
                    for edge in &mut extraction.edges {
                        edge.from.root = primary_slug.clone();
                        edge.to.root = primary_slug.clone();
                        super::helpers::resolve_edge_target_by_suffix(edge, &file_index);
                    }

                    let mut upsert_node_ids: std::collections::HashSet<String> =
                        extraction.nodes.iter().map(|n| n.stable_id()).collect();
                    let mut upsert_edges: Vec<Edge> = extraction.edges.clone();
                    full_state.nodes.extend(extraction.nodes);
                    full_state.edges.extend(extraction.edges);

                    // Snapshot before passes for delta tracking
                    let node_ids_before_passes: std::collections::HashSet<String> =
                        full_state.nodes.iter().map(|n| n.stable_id()).collect();
                    let edge_ids_before_passes: std::collections::HashSet<String> =
                        full_state.edges.iter().map(|e| e.stable_id()).collect();

                    // Pre-clean: remove stale virtual nodes before passes
                    let stale_virtual_files: Vec<std::path::PathBuf> = full_state.nodes
                        .iter()
                        .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")))
                        .map(|n| n.id.file.clone())
                        .collect();
                    full_state.nodes.retain(|n| !matches!(&n.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")));
                    full_state.edges.retain(|e| {
                        !matches!(&e.to.kind, NodeKind::Other(s) if s == "subsystem")
                            && e.kind != crate::graph::EdgeKind::UsesFramework
                            && e.kind != crate::graph::EdgeKind::Produces
                            && e.kind != crate::graph::EdgeKind::Consumes
                    });
                    let mut files_to_remove_all: Vec<PathBuf> = files_to_remove;
                    files_to_remove_all.extend(stale_virtual_files);

                    // Run enrichment pipeline via EventBus (LSP, passes, framework detection)
                    let root_pairs: Vec<(String, std::path::PathBuf)> = WorkspaceConfig::load()
                        .with_primary_root(repo_root.clone())
                        .with_worktrees(&repo_root)
                        .with_declared_roots(&repo_root)
                        .resolved_roots()
                        .iter()
                        .map(|r| (r.slug.clone(), r.path.clone()))
                        .collect();
                    {
                        lsp_status.set_running();
                        let dirty_slugs: Option<std::collections::HashSet<String>> =
                            Some(std::iter::once(primary_slug.clone()).collect());
                        match crate::extract::consumers::emit_enrichment_pipeline(
                            std::mem::take(&mut full_state.nodes),
                            std::mem::take(&mut full_state.edges),
                            root_pairs,
                            primary_slug.clone(),
                            repo_root.clone(),
                            crate::extract::consumers::BusOptions {
                                scan_stats: Some(Arc::clone(&scan_stats)),
                                embed_idx: None,
                                lance_repo_root: None,
                            },
                            dirty_slugs,
                        ).await {
                            Ok((enriched_nodes, enriched_edges, detected_frameworks)) => {
                                full_state.nodes = enriched_nodes;
                                full_state.edges = enriched_edges;
                                full_state.detected_frameworks = detected_frameworks;

                                // Update LSP status
                                let lsp_edge_count = full_state.edges.iter()
                                    .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                                    .count();
                                let lsp_call_edge_count = full_state.edges.iter()
                                    .filter(|e| {
                                        e.source == crate::graph::ExtractionSource::Lsp
                                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                                    })
                                    .count();
                                if lsp_edge_count > 0 {
                                    lsp_status.set_complete(lsp_call_edge_count);
                                } else {
                                    lsp_status.set_unavailable();
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Background incremental pipeline: enrichment failed: {:#}", e
                                );
                                if let Err(e) = scanner.commit_state() {
                                    tracing::error!("Failed to commit scanner state: {}", e);
                                }
                                return;
                            }
                        }
                    }

                    // Auto-collect delta from passes
                    for n in &full_state.nodes {
                        let sid = n.stable_id();
                        if !node_ids_before_passes.contains(&sid) {
                            upsert_node_ids.insert(sid);
                        }
                    }
                    for e in &full_state.edges {
                        let sid = e.stable_id();
                        if !edge_ids_before_passes.contains(&sid) {
                            upsert_edges.push(e.clone());
                        }
                    }

                    // Dedup
                    {
                        let mut seen_nodes = std::collections::HashSet::new();
                        full_state.nodes.reverse();
                        full_state.nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                        full_state.nodes.reverse();
                        let mut seen_edges = std::collections::HashSet::new();
                        full_state.edges.retain(|e| seen_edges.insert(e.stable_id()));
                    }

                    // Rebuild index
                    full_state.index = GraphIndex::new();
                    full_state.index.rebuild_from_edges(&full_state.edges);
                    for node in &full_state.nodes {
                        full_state.index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                    }

                    // PageRank
                    let pagerank_scores = full_state.index.compute_pagerank(0.85, 20);
                    for node in &mut full_state.nodes {
                        if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                            node.metadata.insert("importance".to_string(), format!("{:.6}", score));
                        }
                    }

                    // Subsystem detection
                    {
                        let node_file_map: std::collections::HashMap<String, String> = full_state.nodes
                            .iter()
                            .filter(|n| n.id.root != "external")
                            .map(|n| (n.stable_id(), n.id.file.display().to_string()))
                            .collect();
                        let mut subsystems = full_state.index.detect_communities(&pagerank_scores, &node_file_map);
                        {
                            let mut name_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                            for s in &subsystems { *name_counts.entry(s.name.clone()).or_default() += 1; }
                            for s in &mut subsystems {
                                if name_counts.get(&s.name).copied().unwrap_or(0) > 1
                                    && let Some(iface) = s.interfaces.first() {
                                        let short = iface.node_id.split(':').rev().nth(1).unwrap_or(&iface.node_id);
                                        s.name = format!("{}/{}", s.name, short);
                                    }
                            }
                        }
                        let mut node_subsystem: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                        for subsystem in &subsystems {
                            for member_id in &subsystem.member_ids {
                                node_subsystem.insert(member_id.clone(), subsystem.name.clone());
                            }
                        }
                        for node in &mut full_state.nodes {
                            let sid = node.stable_id();
                            let old_sub = node.metadata.get(SUBSYSTEM_KEY).cloned();
                            let new_sub = node_subsystem.get(&sid).cloned();
                            if old_sub != new_sub {
                                match new_sub {
                                    Some(name) => { node.metadata.insert(SUBSYSTEM_KEY.to_owned(), name); }
                                    None => { node.metadata.remove(SUBSYSTEM_KEY); }
                                }
                                upsert_node_ids.insert(sid);
                            }
                        }

                        // Subsystem node promotion via bus
                        let node_ids_before_group2: std::collections::HashSet<String> =
                            full_state.nodes.iter().map(|n| n.stable_id()).collect();
                        let edge_ids_before_group2: std::collections::HashSet<String> =
                            full_state.edges.iter().map(|e| e.stable_id()).collect();
                        let (sub_added_nodes, sub_added_edges) =
                            crate::extract::consumers::emit_community_detection(
                                primary_slug.clone(),
                                subsystems,
                                full_state.nodes.clone(),
                            ).await.unwrap_or_else(|e| {
                                tracing::warn!("Background incremental: subsystem promotion failed (non-fatal): {}", e);
                                (vec![], vec![])
                            });
                        if !sub_added_nodes.is_empty() || !sub_added_edges.is_empty() {
                            full_state.nodes.extend(sub_added_nodes);
                            full_state.edges.extend(sub_added_edges);
                        }
                        for n in &full_state.nodes {
                            let sid = n.stable_id();
                            if !node_ids_before_group2.contains(&sid) { upsert_node_ids.insert(sid); }
                        }
                        for e in &full_state.edges {
                            let sid = e.stable_id();
                            if !edge_ids_before_group2.contains(&sid) { upsert_edges.push(e.clone()); }
                        }
                    }

                    // Update index for virtual nodes
                    for node in &full_state.nodes {
                        if matches!(&node.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")) {
                            full_state.index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                        }
                    }
                    for edge in &full_state.edges {
                        if matches!(&edge.to.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")) {
                            full_state.index.add_edge(
                                &edge.from.to_stable_id(), &edge.from.kind.to_string(),
                                &edge.to.to_stable_id(), &edge.to.kind.to_string(),
                                edge.kind.clone(),
                            );
                        }
                    }

                    // Re-embed changed-file symbols
                    let changed_files: std::collections::HashSet<_> = scan.changed_files
                        .iter().chain(scan.new_files.iter()).collect();
                    let embed_guard = embed_index.load();
                    if let Some(ref embed_idx) = **embed_guard {
                        let changed_file_nodes: Vec<_> = full_state.nodes
                            .iter()
                            .filter(|n| changed_files.iter().any(|f| n.id.file == **f))
                            .cloned()
                            .collect();
                        if let Err(e) = embed_idx.reindex_nodes(&changed_file_nodes).await {
                            tracing::warn!("Background incremental: re-embed failed: {}", e);
                            if let Err(e2) = embed_idx.index_all_with_symbols(&repo_root, &full_state.nodes).await {
                                tracing::warn!("Background incremental: full embed rebuild also failed: {}", e2);
                            }
                        }
                    }

                    // Build upsert data with post-PageRank importance scores
                    let upsert_nodes: Vec<Node> = full_state.nodes
                        .iter()
                        .filter(|n| upsert_node_ids.contains(&n.stable_id()))
                        .cloned()
                        .collect();

                    // Persist to LanceDB
                    let persist_result = {
                        let _lance_guard = lance_write_lock.lock().await;
                        persist_graph_incremental(
                            &repo_root, &upsert_nodes, &upsert_edges,
                            &deleted_edge_ids, &files_to_remove_all,
                        ).await
                    };
                    let persist_ok = match persist_result {
                        Ok(true) => {
                            let _lance_guard = lance_write_lock.lock().await;
                            match persist_graph_to_lance(&repo_root, &full_state.nodes, &full_state.edges).await {
                                Ok(()) => true,
                                Err(e) => { tracing::error!("Full persist after migration failed: {:#}", e); false }
                            }
                        }
                        Ok(false) => true,
                        Err(e) => { tracing::error!("Incremental persist failed: {:#}", e); false }
                    };

                    if persist_ok {
                        if let Err(e) = scanner.commit_state() {
                            tracing::error!("Failed to commit scanner state: {}", e);
                        }
                    }

                    full_state.last_scan_completed_at = Some(std::time::Instant::now());

                    // Atomic swap: publish the fully enriched graph
                    handler_graph.store(Arc::new(Some(Arc::new(full_state))));
                    tracing::info!("Background incremental pipeline complete -- enriched graph swapped in");
                });
            }

            // Start background scanner (once) to keep index warm
            if !self.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                self.spawn_background_scanner();
            }
            return Ok(fast_arc);
        }

        // Slow path: no graph exists yet.
        // If pre-warm is in progress, wait for it instead of building a duplicate.
        if self.prewarm_started.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("Waiting for pre-warm to finish...");
            let prewarm_deadline = tokio::time::Instant::now() + Self::PREWARM_TIMEOUT;
            loop {
                let snap = self.graph.load_full();
                if snap.is_some() {
                    break;
                }
                if tokio::time::Instant::now() >= prewarm_deadline {
                    tracing::warn!(
                        "Pre-warm did not finish within {}s — building graph ourselves",
                        Self::PREWARM_TIMEOUT.as_secs()
                    );
                    break;
                }
                tokio::select! {
                    _ = self.prewarm_notify.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
                }
            }
            // Re-check after waiting.
            let snap = self.graph.load_full();
            if let Some(ref gs) = *snap {
                *self.last_scan.lock().unwrap() = std::time::Instant::now();
                if !self.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    self.spawn_background_scanner();
                }
                return Ok(Arc::clone(gs));
            }
        }

        // Build from scratch, serialized to prevent duplicate builds.
        let _build_guard = self.graph_build_lock.lock().await;
        // Re-check: another call may have built it while we waited.
        let snap = self.graph.load_full();
        if let Some(ref gs) = *snap {
            *self.last_scan.lock().unwrap() = std::time::Instant::now();
            if !self.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                self.spawn_background_scanner();
            }
            return Ok(Arc::clone(gs));
        }

        self.graph_build_status.set_building(0);
        match self.build_full_graph().await {
            Ok(new_state) => {
                let new_arc = Arc::new(new_state);
                self.graph.store(Arc::new(Some(Arc::clone(&new_arc))));
                *self.last_scan.lock().unwrap() = std::time::Instant::now();
                self.graph_build_status.set_ready();
                if !self.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    self.spawn_background_scanner();
                }
                Ok(new_arc)
            }
            Err(e) => {
                self.graph_build_status.set_failed(format!("{}", e));
                Err(e)
            }
        }
    }

    /// Build the full graph from scratch. This is the original get_graph logic.
    ///
    /// When `spawn_background` is true (default for MCP server), embedding is
    /// spawned as a background task so the graph is queryable immediately.
    /// LSP enrichment now runs synchronously via `LspConsumer` in the event bus
    /// (`emit_enrichment_pipeline`), so it completes before this function returns.
    /// When false (used by `run_pipeline_foreground`), no background tasks are
    /// spawned -- the caller handles embed itself.
    pub async fn build_full_graph(&self) -> anyhow::Result<GraphState> {
        self.build_full_graph_inner(true).await
    }

    pub(crate) async fn build_full_graph_inner(&self, spawn_background: bool) -> anyhow::Result<GraphState> {
        // Invalidate cached root slugs since workspace/worktree config may have changed.
        self.invalidate_non_code_root_slugs_cache();

        // Initialize pattern config from .oh/config.toml (once, at first build).
        crate::extract::generic::init_pattern_config(&self.repo_root);

        // Pre-flight: ensure schema version matches before any LanceDB reads/writes.
        let db_path = graph_lance_path(&self.repo_root);
        if check_and_migrate_schema(&db_path).await? {
            tracing::info!("Schema migrated to v{} -- cache rebuilt", SCHEMA_VERSION);
        }

        // Clean up stale cache directories from previous schema versions (#298).
        // The old `.oh/.cache/embeddings/` directory is a dead copy from before
        // lance path consolidation. Remove it if the lance path exists.
        let stale_embeddings_dir = self.repo_root.join(".oh").join(".cache").join("embeddings");
        let lance_dir = self.repo_root.join(".oh").join(".cache").join("lance");
        if lance_dir.exists() && stale_embeddings_dir.exists() {
            match std::fs::remove_dir_all(&stale_embeddings_dir) {
                Ok(()) => tracing::info!(
                    "Cleaned up stale cache directory: {}",
                    stale_embeddings_dir.display()
                ),
                Err(e) => tracing::warn!(
                    "Failed to remove stale cache directory {}: {}",
                    stale_embeddings_dir.display(),
                    e
                ),
            }
        }

        // Load workspace config and merge with --repo as primary root.
        // Also auto-detect any live git worktrees, Claude Code memory,
        // and agent memory files so all roots are indexed on the first full build.
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root)
            .with_agent_memories(&self.repo_root)
            .with_declared_roots(&self.repo_root);
        let resolved_roots = workspace.resolved_roots();

        // Check extraction version: if it changed, clear scan-state files for all roots
        // to force full re-extraction with updated extraction logic (e.g., doc_comment #401).
        let secondary_slugs: Vec<String> = resolved_roots
            .iter()
            .filter(|r| r.path != self.repo_root)
            .map(|r| r.slug.clone())
            .collect();
        match check_and_migrate_extraction_version(&db_path, &self.repo_root, &secondary_slugs) {
            Ok(true) => {
                tracing::info!(
                    "Extraction version migrated to v{} — scan-state cleared, full re-extraction required",
                    crate::graph::store::EXTRACTION_VERSION
                );
                // Scan-state files are cleared; the scanner below will see all files as new.
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!("Extraction version check failed (proceeding): {}", e);
            }
        }

        // Prune stale roots: compare discovered roots against what LanceDB has stored.
        // Worktrees removed while the server was offline leave orphaned rows that cause
        // duplicate results (see #198).
        //
        // Two separate sets serve different purposes:
        //
        // `live_slugs` — non-lsp_only roots only. Used for `has_new_root` detection:
        //   lsp_only roots are excluded because they have no tree-sitter-extracted nodes
        //   and are not persisted to LanceDB directly, so including them would cause
        //   has_new_root to fire on every startup (root is "live" but has no stored nodes),
        //   triggering unnecessary cold rebuilds.
        //
        // `all_declared_slugs` — ALL declared roots including lsp_only. Used for stale
        //   pruning: lsp_only roots CAN produce nodes (e.g. nextjs_routing_pass emits
        //   ApiEndpoint nodes with root_id = lsp_only slug). Those nodes must not be
        //   treated as stale and deleted on every subsequent scan (see #453).
        let live_slugs: std::collections::HashSet<String> = resolved_roots
            .iter()
            .filter(|r| !r.config.lsp_only)
            .map(|r| r.slug.clone())
            .collect();
        let all_declared_slugs: std::collections::HashSet<String> = resolved_roots
            .iter()
            .map(|r| r.slug.clone())
            .collect();
        // Synthetic root IDs (e.g., "external" for LSP virtual nodes) are never
        // discovered by WorkspaceConfig but are valid -- skip them during stale pruning.
        const RESERVED_ROOT_IDS: &[&str] = &["external"];
        // Track whether any live root is absent from LanceDB: a newly-declared root
        // may have committed scanner state without ever being persisted to LanceDB
        // (e.g., when the root was first discovered on a run where `any_root_changed`
        // was already true for other reasons but LanceDB was not yet updated for this
        // root). In that case `any_root_changed` stays false and the early-return path
        // loads the cache without the new root's nodes. Force a rebuild if any slug is
        // missing from the stored set.
        let mut has_new_root = false;
        match get_stored_root_ids(&self.repo_root).await {
            Ok(stored) => {
                let stored_set: std::collections::HashSet<String> = stored.iter().cloned().collect();
                let stale: Vec<String> = stored
                    .into_iter()
                    .filter(|s| !all_declared_slugs.contains(s))
                    .filter(|s| !RESERVED_ROOT_IDS.contains(&s.as_str()))
                    .collect();
                if !stale.is_empty() {
                    tracing::info!(
                        "Detected {} stale root(s) in LanceDB: {}",
                        stale.len(),
                        stale.join(", ")
                    );
                    if let Err(e) = delete_nodes_for_roots(&self.repo_root, &stale).await {
                        tracing::warn!("Failed to prune stale roots at startup: {}", e);
                    }
                }
                // Detect new roots: any live slug not present in LanceDB means its
                // nodes were never persisted and must be included in a fresh build.
                let new_slugs: Vec<&str> = live_slugs
                    .iter()
                    .filter(|s| !stored_set.contains(*s) && !RESERVED_ROOT_IDS.contains(&s.as_str()))
                    .map(|s| s.as_str())
                    .collect();
                if !new_slugs.is_empty() {
                    tracing::info!(
                        "Detected {} new root(s) not yet in LanceDB: {} -- forcing full rebuild",
                        new_slugs.len(),
                        new_slugs.join(", ")
                    );
                    has_new_root = true;
                }
            }
            Err(e) => {
                tracing::debug!("Could not query stored roots for stale pruning: {}", e);
            }
        }

        // 1. Scan all roots to detect changes (per-root tracking)
        // Pre-seed with has_new_root so the early-return at step 2 is skipped when
        // any declared root is absent from LanceDB (nodes committed to scanner state
        // but never persisted).
        let mut any_root_changed = has_new_root;
        let mut scanners: Vec<(String, Scanner, crate::scanner::ScanResult, PathBuf, bool)> = Vec::new();

        for resolved_root in &resolved_roots {
            // Skip lsp_only roots: their files are already covered by the primary root
            // scan. Running a separate scanner over them would produce duplicate nodes.
            if resolved_root.config.lsp_only {
                tracing::debug!(
                    "Skipping extraction for lsp_only root '{}' at '{}' (files covered by primary root)",
                    resolved_root.slug,
                    resolved_root.path.display()
                );
                continue;
            }
            let root_slug = &resolved_root.slug;
            let root_path = &resolved_root.path;
            let excludes = resolved_root.config.effective_excludes();

            let is_primary = resolved_root.path == self.repo_root;
            let mut scanner = if is_primary {
                Scanner::with_excludes(root_path.clone(), excludes)?
            } else {
                let state_path = cache_state_path(root_slug);
                Scanner::with_excludes_and_state_path(
                    root_path.clone(),
                    excludes,
                    state_path,
                )?
            };

            let scan_result = scanner.scan()?;
            tracing::info!(
                "Scanned root '{}' ({}): {} new, {} changed, {} deleted in {:?}",
                root_slug,
                resolved_root.config.root_type,
                scan_result.new_files.len(),
                scan_result.changed_files.len(),
                scan_result.deleted_files.len(),
                scan_result.scan_duration
            );

            let root_has_changes = !scan_result.new_files.is_empty()
                || !scan_result.changed_files.is_empty()
                || !scan_result.deleted_files.is_empty();

            if root_has_changes {
                any_root_changed = true;
            }

            scanners.push((root_slug.clone(), scanner, scan_result, root_path.clone(), root_has_changes));
        }

        {
            let total_files: usize = scanners.iter()
                .map(|(_, _, scan, _, _)| scan.new_files.len() + scan.changed_files.len())
                .sum();
            self.graph_build_status.set_building(total_files);
        }

        // 2. If no changes anywhere, try loading full graph from LanceDB
        if !any_root_changed {
            match load_graph_from_lance(&self.repo_root).await {
                Ok(state) => {
                    tracing::info!(
                        "Loaded graph from LanceDB: {} nodes, {} edges",
                        state.nodes.len(),
                        state.edges.len()
                    );
                    // No changes detected -- reuse existing embedding table if present.
                    // Only rebuild if the table is missing (first run or cache cleared).
                    if let Ok(idx) = EmbeddingIndex::new(&self.repo_root).await {
                        match idx.has_table().await {
                            Ok(true) => {
                                // Ensure FTS indexes exist -- table may predate hybrid search.
                                idx.ensure_fts_index().await;
                                tracing::info!("Reusing existing embedding index (no changes detected)");
                                self.embed_index.store(Arc::new(Some(idx)));
                            }
                            Ok(false) => {
                                match idx.index_all_with_symbols(&self.repo_root, &state.nodes).await {
                                    Ok(count) => {
                                        tracing::info!("Built embedding index: {} items (table was missing)", count);
                                        self.embed_index.store(Arc::new(Some(idx)));
                                    }
                                    Err(e) => tracing::warn!("Failed to embed cached graph: {}", e),
                                }
                            }
                            Err(e) => tracing::warn!("Failed to check embedding table: {}", e),
                        }
                    }

                    // FIX(#215): The early return here previously skipped LSP
                    // enrichment entirely, leaving status stuck at SERVER_FOUND.
                    // Use the LSP completion sentinel (written after successful LSP
                    // persist) instead of the heuristic `has_call_edges` check.
                    // The heuristic fails when LSP ran but persist failed -- the
                    // edges are in memory but not durable, so the next restart
                    // would incorrectly skip re-enrichment (#477).
                    //
                    // FIX(bus path): When the sentinel is absent, route through the event bus
                    // (LspConsumer → EnrichmentComplete → AllEnrichmentsGate → AllEnrichmentsDone
                    // → EnrichmentFinalizer → PassesComplete) so ScanStatsConsumer correctly
                    // tracks LSP completion. The old spawn_lsp_enrichment bypassed all of these.
                    if spawn_background {
                        let lsp_sentinel = super::sentinel::read_lsp_sentinel(&self.repo_root);
                        if lsp_sentinel.is_none() {
                            tracing::info!(
                                "LSP sentinel absent -- spawning background LSP enrichment via bus"
                            );
                            self.spawn_lsp_enrichment_via_bus(&state.nodes, &state.edges);
                        } else {
                            tracing::info!(
                                "LSP sentinel present -- LSP enrichment already persisted, skipping"
                            );
                            // Mark LSP as complete using the actual persisted call edge count.
                            let call_count = state.edges.iter()
                                .filter(|e| matches!(e.kind, crate::graph::EdgeKind::Calls))
                                .count();
                            self.lsp_status.set_complete(call_count);
                        }
                    }

                    for (_slug, scanner, _scan, _path, _changed) in &scanners {
                        if let Err(e) = scanner.commit_state() {
                            tracing::error!("Failed to commit scanner state: {}", e);
                        }
                    }

                    self.graph_build_status.set_ready();
                    return Ok(state);
                }
                Err(e) => {
                    tracing::debug!("Could not load persisted graph: {}", e);
                }
            }
        }

        // 3. Per-root rebuild: only re-extract dirty roots, load clean roots from cache.
        // This preserves LSP edges for unchanged roots and avoids re-extracting
        // unchanged worktrees. The background scanner already does per-root updates
        // (lines 1502-1573); this brings the same pattern to the cold-start path.
        let registry = ExtractorRegistry::with_builtins();
        let mut all_nodes: Vec<Node> = Vec::new();
        let mut all_edges: Vec<Edge> = Vec::new();
        // Populated by EnrichmentFinalizer via EventBus (step 4b-4j).
        let all_detected_frameworks: std::collections::HashSet<String>;

        // Try loading cached graph for clean-root reuse.
        let cached_graph = match load_graph_from_lance(&self.repo_root).await {
            Ok(state) => {
                tracing::info!(
                    "Loaded cached graph for clean-root reuse: {} nodes, {} edges",
                    state.nodes.len(),
                    state.edges.len()
                );
                Some(state)
            }
            Err(e) => {
                tracing::debug!("No cached graph available for clean-root reuse: {}", e);
                None
            }
        };

        // Pre-index cached graph by root slug to avoid O(roots * N) scanning.
        // Consume cached_graph to move nodes/edges instead of cloning.
        let mut cached_nodes_by_root: std::collections::HashMap<String, Vec<Node>> =
            std::collections::HashMap::new();
        let mut cached_edges_by_root: std::collections::HashMap<String, Vec<Edge>> =
            std::collections::HashMap::new();
        let has_cached_graph = cached_graph.is_some();
        if let Some(cached) = cached_graph {
            for node in cached.nodes {
                let root = node.id.root.clone();
                cached_nodes_by_root.entry(root).or_default().push(node);
            }
            for edge in cached.edges {
                let root = edge.from.root.clone();
                cached_edges_by_root.entry(root).or_default().push(edge);
            }
        }

        // Track which roots are freshly extracted (dirty OR cache-miss) and need
        // LSP enrichment. Clean roots loaded from cache already have LSP edges.
        let mut freshly_extracted_slugs: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (root_slug, scanner, _scan_result, root_path, root_changed) in &scanners {
            if !root_changed {
                // Clean root: load from pre-indexed cache if available, otherwise extract.
                if has_cached_graph {
                    let cached_nodes = cached_nodes_by_root.remove(root_slug);
                    let cached_edges = cached_edges_by_root.remove(root_slug);

                    if let Some(nodes) = cached_nodes
                        && !nodes.is_empty() {
                            let edges = cached_edges.unwrap_or_default();
                            tracing::info!(
                                "Clean root '{}': loaded {} nodes, {} edges from cache (preserving LSP edges)",
                                root_slug,
                                nodes.len(),
                                edges.len()
                            );
                            all_nodes.extend(nodes);
                            all_edges.extend(edges);
                            continue;
                        }
                    // Fall through to full extract if cache had no nodes for this root.
                    // This root needs LSP enrichment since it has no cached LSP edges.
                    tracing::info!(
                        "Clean root '{}': no cached nodes found, extracting fresh (will need LSP enrichment)",
                        root_slug
                    );
                }
            }

            // Dirty root (or clean root with no cache): full extract
            let all_files = scanner.all_known_files();
            let full_scan = crate::scanner::ScanResult {
                changed_files: Vec::new(),
                new_files: all_files,
                deleted_files: Vec::new(),
                scan_duration: std::time::Duration::ZERO,
            };
            let (mut extraction, enc_stats) = registry.extract_scan_result_with_stats(root_path, &full_scan);

            // Record encoding stats for this root (full scan: replace totals).
            if let Ok(mut stats) = self.scan_stats.write() {
                stats.set_encoding_stats(root_slug, enc_stats);
            }

            for node in &mut extraction.nodes {
                node.id.root = root_slug.clone();
            }
            // Build file index for suffix matching import edges
            let file_index: std::collections::HashSet<String> = extraction.nodes
                .iter()
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect();

            for edge in &mut extraction.edges {
                edge.from.root = root_slug.clone();
                edge.to.root = root_slug.clone();
                // Resolve dangling import edges via suffix match
                helpers::resolve_edge_target_by_suffix(edge, &file_index);
            }

            // For dirty roots, carry forward cached LSP edges whose endpoints
            // still exist in the freshly extracted node set. Tree-sitter re-extraction
            // only produces tree-sitter edges; LSP edges (Calls, ReferencedBy from LSP)
            // are produced by the background enricher and would otherwise be lost on
            // every incremental rebuild.
            let mut lsp_carry_count = 0usize;
            if *root_changed && has_cached_graph
                && let Some(cached_edges) = cached_edges_by_root.remove(root_slug) {
                    let node_ids: std::collections::HashSet<String> = extraction.nodes
                        .iter()
                        .map(|n| n.stable_id())
                        .collect();
                    for edge in cached_edges {
                        if edge.source == crate::graph::ExtractionSource::Lsp {
                            let from_id = edge.from.to_stable_id();
                            let to_id = edge.to.to_stable_id();
                            // Require the source node to still exist. For the
                            // target, only require existence if it belongs to
                            // the same dirty root -- external/virtual targets
                            // may not be in our extraction node set.
                            let from_exists = node_ids.contains(&from_id);
                            let to_exists = edge.to.root != *root_slug
                                || node_ids.contains(&to_id);
                            if from_exists && to_exists {
                                extraction.edges.push(edge);
                                lsp_carry_count += 1;
                            }
                        }
                    }
                }

            tracing::info!(
                "Extracted from '{}'{}: {} nodes, {} edges{}",
                root_slug,
                if *root_changed { " (dirty)" } else { " (no cache)" },
                extraction.nodes.len(),
                extraction.edges.len(),
                if lsp_carry_count > 0 {
                    format!(" (carried forward {} LSP edges)", lsp_carry_count)
                } else {
                    String::new()
                }
            );

            // Mark this root as needing LSP enrichment (dirty or cache-miss).
            freshly_extracted_slugs.insert(root_slug.clone());

            all_nodes.extend(extraction.nodes);
            all_edges.extend(extraction.edges);
        }

        // Also load cached external/virtual nodes (e.g., from previous LSP enrichment)
        // that don't belong to any current root.
        // Only include nodes whose root is genuinely external/virtual -- not stale
        // worktree items that were deleted but remain in the LanceDB cache.
        if has_cached_graph {
            let current_slugs: std::collections::HashSet<&str> = scanners
                .iter()
                .map(|(slug, _, _, _, _)| slug.as_str())
                .collect();
            let non_code = self.non_code_root_slugs();
            let is_virtual_root = |root: &str| -> bool {
                root == "external" || non_code.contains(root)
            };
            // Use remaining entries from pre-indexed maps (roots not consumed by clean-root reuse).
            let mut ext_node_count = 0usize;
            let mut ext_edge_count = 0usize;
            for (root, nodes) in cached_nodes_by_root {
                if !current_slugs.contains(root.as_str()) && is_virtual_root(&root) {
                    ext_node_count += nodes.len();
                    all_nodes.extend(nodes);
                }
            }
            for (root, edges) in cached_edges_by_root {
                if !current_slugs.contains(root.as_str()) && is_virtual_root(&root) {
                    ext_edge_count += edges.len();
                    all_edges.extend(edges);
                }
            }
            if ext_node_count > 0 {
                tracing::info!(
                    "Loaded {} external/virtual nodes, {} edges from cache",
                    ext_node_count,
                    ext_edge_count
                );
            }
        }

        // 4. Extract PR merges from git history
        match crate::git::pr_merges::extract_pr_merges(&self.repo_root, Some(100)) {
            Ok((pr_nodes, pr_edges)) => {
                let modified_edges =
                    crate::git::pr_merges::link_pr_to_symbols(&pr_nodes, &all_nodes);
                tracing::info!(
                    "PR merges: {} nodes, {} edges, {} Modified links",
                    pr_nodes.len(),
                    pr_edges.len(),
                    modified_edges.len()
                );
                all_nodes.extend(pr_nodes);
                all_edges.extend(pr_edges);
                all_edges.extend(modified_edges);
            }
            Err(e) => {
                tracing::warn!("Failed to extract PR merges: {}", e);
            }
        }

        // 4b-4j. Post-extraction passes via EventBus (ADR Phase 3, issue #520).
        //
        // `emit_enrichment_pipeline` routes all_nodes/all_edges through the bus:
        //   - LanguageAccumulatorConsumer → emits LanguageDetected per language
        //   - LspConsumer (real, Phase 3) → runs LSP enrichment per language via
        //     block_in_place → emits EnrichmentComplete with actual edges
        //   - AllEnrichmentsGate → waits for all EnrichmentComplete → emits AllEnrichmentsDone
        //   - EnrichmentFinalizer → runs all post-extraction passes on LSP-enriched graph
        //     → emits PassesComplete
        //
        // LSP enrichment now runs synchronously inside this bus call.
        // `spawn_background_enrichment` (called below) is embedding-only.
        //   - EmbeddingIndexerConsumer — stub here; embed handled by spawn_background_enrichment
        //   - LanceDBConsumer — stub here; persist handled after PageRank + subsystem passes
        //
        // ADR Constraint 4: no direct pass function calls in src/server/.
        //
        // Both EmbeddingIndexerConsumer and LanceDBConsumer are passed None (stub mode)
        // in the full-build path: embedding runs via spawn_background_enrichment (which
        // uses the embed index created just below), and LanceDB persist runs after PageRank
        // + subsystem detection so the stored data is complete. Using real consumers here
        // would either duplicate work (embed) or persist an incomplete graph (lance).
        {
            // Mark LSP as running before the bus call so status is visible immediately.
            if spawn_background {
                self.lsp_status.set_running();
            }
            let root_pairs: Vec<(String, std::path::PathBuf)> = workspace
                .resolved_roots()
                .iter()
                .map(|r| (r.slug.clone(), r.path.clone()))
                .collect();
            let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();

            // dirty_slugs = roots that were freshly extracted and need LSP enrichment.
            // This includes both scanner-dirty roots AND clean roots whose cache was
            // empty (cache-miss). Clean roots loaded from cache already have LSP edges
            // and should not trigger a new rust-analyzer / LSP server spawn (#555).
            tracing::info!(
                "dirty_slugs for enrichment pipeline: {:?} ({} of {} roots)",
                freshly_extracted_slugs,
                freshly_extracted_slugs.len(),
                scanners.len(),
            );
            let dirty_slugs = Some(freshly_extracted_slugs);

            let (enriched_nodes, enriched_edges, detected_frameworks) =
                crate::extract::consumers::emit_enrichment_pipeline(
                    all_nodes,
                    all_edges,
                    root_pairs,
                    primary_slug,
                    self.repo_root.clone(),
                    crate::extract::consumers::BusOptions {
                        scan_stats: Some(Arc::clone(&self.scan_stats)),
                        embed_idx: None, // embed handled by spawn_background_enrichment after graph is ready
                        lance_repo_root: None, // LanceDB persist handled directly after PageRank/subsystem passes
                    },
                    dirty_slugs,
                ).await?;
            all_nodes = enriched_nodes;
            all_edges = enriched_edges;
            all_detected_frameworks = detected_frameworks;

            // Count LSP edges added by LspConsumer during bus execution.
            // These are ExtractionSource::Lsp edges in all_edges.
            // Use only LSP-sourced edges to drive the status: non-LSP Calls edges
            // (e.g. from GrpcClientCallsPass) must not falsely mark LSP as complete,
            // and a metadata-only enrichment (updated_nodes, no new edges) must not
            // report as unavailable.
            if spawn_background {
                let lsp_edge_count = all_edges.iter()
                    .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                    .count();
                let lsp_call_edge_count = all_edges.iter()
                    .filter(|e| {
                        e.source == crate::graph::ExtractionSource::Lsp
                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                    })
                    .count();
                if lsp_edge_count > 0 {
                    self.lsp_status.set_complete(lsp_call_edge_count);
                    tracing::info!(
                        "LSP enrichment complete (via bus): {} LSP call edges, {} total LSP edges",
                        lsp_call_edge_count, lsp_edge_count,
                    );
                } else {
                    // No LSP edges — either no server available or no enrichable nodes.
                    // Mark as unavailable so the status shows "no server" not "running".
                    self.lsp_status.set_unavailable();
                }
            }
        }

        // Dedup immediately after post-extraction passes so the graph index,
        // PageRank, and subsystem detection run on clean topology.
        // On a mixed dirty/clean rebuild, `all_nodes`/`all_edges` may already
        // contain cached pass output for clean roots; the passes re-emit the
        // same entries, producing duplicates. Dedup here avoids skewed PageRank
        // weights and inflated community sizes.
        {
            let mut seen_nodes = std::collections::HashSet::new();
            all_nodes.reverse();
            all_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
            all_nodes.reverse();

            let before_edges = all_edges.len();
            let mut seen_edges = std::collections::HashSet::new();
            all_edges.retain(|e| seen_edges.insert(e.stable_id()));
            if before_edges != all_edges.len() {
                tracing::debug!(
                    "Post-pass dedup: {} → {} edges ({} duplicates removed)",
                    before_edges, all_edges.len(), before_edges - all_edges.len()
                );
            }
        }

        // 5. Build petgraph index
        let mut index = GraphIndex::new();
        index.rebuild_from_edges(&all_edges);
        for node in &all_nodes {
            index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        tracing::info!(
            "Graph built: {} nodes, {} edges across {} root(s)",
            all_nodes.len(),
            all_edges.len(),
            resolved_roots.len()
        );

        // 6. Compute PageRank importance scores
        let pagerank_scores = index.compute_pagerank(0.85, 20);
        for node in &mut all_nodes {
            if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                node.metadata.insert("importance".to_string(), format!("{:.6}", score));
            }
        }
        tracing::info!("Computed PageRank importance for {} nodes", pagerank_scores.len());

        // 6b. Detect subsystems and write cluster_id to node metadata.
        // This runs after PageRank (which detect_communities needs) and before
        // LanceDB persist so the metadata survives reload.
        {
            let node_file_map: std::collections::HashMap<String, String> = all_nodes
                .iter()
                .filter(|n| n.id.root != "external")
                .map(|n| (n.stable_id(), n.id.file.display().to_string()))
                .collect();

            let mut subsystems = index.detect_communities(&pagerank_scores, &node_file_map);
            // Deduplicate subsystem names (matching repo_map rendering):
            // when multiple clusters share a name, append /<interface> suffix.
            {
                let mut name_counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for s in &subsystems {
                    *name_counts.entry(s.name.clone()).or_default() += 1;
                }
                for s in &mut subsystems {
                    if name_counts.get(&s.name).copied().unwrap_or(0) > 1
                        && let Some(iface) = s.interfaces.first() {
                            let short = iface
                                .node_id
                                .split(':')
                                .rev()
                                .nth(1)
                                .unwrap_or(&iface.node_id);
                            s.name = format!("{}/{}", s.name, short);
                        }
                }
            }
            // Build node_id -> subsystem_name lookup
            let mut node_subsystem: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for subsystem in &subsystems {
                for member_id in &subsystem.member_ids {
                    node_subsystem.insert(member_id.clone(), subsystem.name.clone());
                }
            }
            let mut tagged = 0usize;
            for node in &mut all_nodes {
                if let Some(subsystem_name) = node_subsystem.get(&node.stable_id()) {
                    node.metadata
                        .insert(SUBSYSTEM_KEY.to_owned(), subsystem_name.clone());
                    tagged += 1;
                } else {
                    // Remove stale subsystem metadata from cached nodes that are
                    // no longer in any cluster.
                    node.metadata.remove(SUBSYSTEM_KEY);
                }
            }
            if tagged > 0 {
                tracing::info!(
                    "Tagged {} nodes with subsystem metadata ({} subsystems detected)",
                    tagged,
                    subsystems.len()
                );
            }

            // 6c-6d. Emit first-class subsystem nodes + BelongsTo edges, then
            // subsystem → framework aggregation (UsesFramework edges).
            // Routed via EventBus: CommunityDetectionComplete → SubsystemConsumer
            // → SubsystemNodesComplete. This decouples graph.rs from the pass
            // functions and satisfies ADR Constraint 7.
            let primary_slug = crate::roots::RootConfig::code_project(self.repo_root.clone()).slug();
            let (sub_added_nodes, sub_added_edges) =
                crate::extract::consumers::emit_community_detection(
                    primary_slug.clone(),
                    subsystems.clone(),
                    all_nodes.clone(),
                ).await.unwrap_or_else(|e| {
                    tracing::warn!("Subsystem promotion via bus failed (non-fatal): {}", e);
                    (vec![], vec![])
                });
            if !sub_added_nodes.is_empty() || !sub_added_edges.is_empty() {
                all_nodes.extend(sub_added_nodes);
                all_edges.extend(sub_added_edges);
            }

            // 6e. Extend petgraph index to include newly emitted virtual nodes
            // (subsystem, framework, channel, event) and their edges. Step 5 built
            // the index BEFORE these nodes existed, so agents traversing the graph
            // would miss them. Iterates all_nodes/all_edges to find virtual nodes,
            // then adds only those to the index. This is O(total nodes + total edges)
            // for the scan but O(virtual nodes) for the index operations.
            for node in &all_nodes {
                if matches!(&node.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")) {
                    index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }
            }
            for edge in &all_edges {
                if matches!(&edge.to.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")) {
                    index.add_edge(
                        &edge.from.to_stable_id(),
                        &edge.from.kind.to_string(),
                        &edge.to.to_stable_id(),
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
            }
        }

        // 6z. Deduplicate all_edges before persistence.
        //
        // Post-extraction passes (api_link, tested_by, import_calls, directory_module)
        // run over the full merged all_nodes after each root's edges are loaded from
        // cache.  On a mixed dirty/clean rebuild, those passes re-emit edges that
        // are already present in all_edges from the cached roots.  Deduplicate here
        // so the LanceDB full-persist (DROP+CREATE) doesn't write duplicate rows.
        //
        // The incremental path (update_graph_with_scan) has its own dedup block;
        // keep them in sync.
        {
            let before = all_edges.len();
            let mut seen_edges = std::collections::HashSet::new();
            all_edges.retain(|e| seen_edges.insert(e.stable_id()));
            let after = all_edges.len();
            if before != after {
                tracing::debug!(
                    "Full-build edge dedup: {} → {} edges ({} duplicates removed)",
                    before, after, before - after
                );
            }
        }

        // 7. Persist graph to LanceDB
        //
        // When `spawn_background=true` (MCP server path), persist NOW so the
        // graph is queryable immediately while background LSP+embed run.
        // The background enrichment task re-persists after adding LSP edges.
        //
        // When `spawn_background=false` (CLI `--full` path via run_pipeline_foreground),
        // skip the early persist -- the caller runs LSP synchronously and then
        // does a full persist with the complete graph (tree-sitter + LSP edges).
        // Persisting here would write only tree-sitter edges, and a subsequent
        // `repo-map` loading from LanceDB cache would miss LSP edges (#311).
        if spawn_background {
            // Phase 3 (issue #520): LSP enrichment now runs synchronously inside
            // `emit_enrichment_pipeline` (via `LspConsumer` + `AllEnrichmentsGate`).
            // The graph already contains LSP edges at this point, so we can write
            // both sentinels in one persist rather than clearing+rewriting.
            // We no longer need to clear the LSP sentinel here since LSP ran before persist.

            let _lance_guard = self.lance_write_lock.lock().await;
            if let Err(e) = persist_graph_to_lance(&self.repo_root, &all_nodes, &all_edges).await {
                tracing::error!("Failed to persist graph to LanceDB: {}", e);
                return Err(e.context("LanceDB full persist failed during graph build"));
            }

            // Write both sentinels inside the lock: LSP enrichment already completed
            // above (synchronously via bus), so the persisted graph includes LSP edges.
            // Writing the LSP sentinel here avoids a subsequent startup re-running LSP
            // on an already-enriched graph (#477).
            super::sentinel::write_extract_sentinel(&self.repo_root, all_nodes.len(), all_edges.len());
            // Write or clear the LSP sentinel depending on whether LSP produced edges.
            // If LSP ran but produced no edges (no server, no supported language, empty result),
            // we must clear any stale sentinel left from a previous run. A stale sentinel would
            // cause the next startup to skip LSP enrichment even though the persisted graph is
            // tree-sitter-only, which would hide the "no server" situation.
            let has_lsp_edges = all_edges.iter()
                .any(|e| e.source == crate::graph::ExtractionSource::Lsp);
            if has_lsp_edges {
                super::sentinel::write_lsp_sentinel(&self.repo_root, all_nodes.len(), all_edges.len());
            } else {
                // No LSP data in this build — clear the old sentinel so the next startup
                // knows to re-run LSP enrichment rather than trusting stale data.
                super::sentinel::clear_lsp_sentinel(&self.repo_root);
            }
            drop(_lance_guard);

            // Post-persist sanity check: if any new roots were detected, verify they
            // actually made it into LanceDB. A mismatch here indicates a partial write
            // or concurrent overwrite -- log an error so the next scan can recover.
            if has_new_root {
                match get_stored_root_ids(&self.repo_root).await {
                    Ok(stored_after) => {
                        let stored_after_set: std::collections::HashSet<String> =
                            stored_after.into_iter().collect();
                        let missing: Vec<String> = live_slugs
                            .iter()
                            .filter(|s| !stored_after_set.contains(*s) && !RESERVED_ROOT_IDS.contains(&s.as_str()))
                            .cloned()
                            .collect();
                        if !missing.is_empty() {
                            tracing::error!(
                                "Post-persist check FAILED: {} root(s) still missing from LanceDB after full rebuild: {}. \
                                 The persist completed without error but the data is not visible. \
                                 This may indicate a concurrent overwrite by another process. \
                                 Next scan will retry.",
                                missing.len(),
                                missing.join(", ")
                            );
                        } else {
                            tracing::info!(
                                "Post-persist check: all {} live root(s) confirmed in LanceDB",
                                live_slugs.len()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Post-persist root check failed (non-fatal): {}", e);
                    }
                }
            }
        }

        // Persist succeeded (or deferred) -- commit scanner state for all roots
        // so the next scan doesn't re-process the same files.
        for (_slug, scanner, _scan, _path, _changed) in &scanners {
            if let Err(e) = scanner.commit_state() {
                tracing::error!("Failed to commit scanner state: {}", e);
            }
        }

        // Graph is ready -- return immediately so agents can query.
        // LSP enrichment already ran synchronously via LspConsumer in the bus above (#520).
        // Embedding runs in the background task below.
        let symbols_ready_at = std::time::Instant::now();

        // Store embed index immediately so it's available for queries.
        // The background task below will re-index (including .oh/
        // artifacts) via index_all_inner which uses merge_insert to upsert
        // changed rows and skip unchanged ones (BLAKE3 hash check).
        match EmbeddingIndex::new(&self.repo_root).await {
            Ok(idx) => {
                tracing::info!("Embedding index created -- background task will re-index");
                self.embed_index.store(Arc::new(Some(idx)));
            }
            Err(e) => {
                tracing::warn!("Failed to create embed index: {}", e);
            }
        };

        if spawn_background {
            let handle = self.spawn_background_enrichment(&all_nodes);
            // Store the handle so CLI callers can await it before the runtime
            // shuts down, preventing JoinError::Cancelled panics (#560).
            *self.embed_handle.lock().await = Some(handle);
        }

        Ok(GraphState::new(
            all_nodes,
            all_edges,
            index,
            Some(symbols_ready_at),
            all_detected_frameworks,
        ))
    }

    /// Incrementally update the graph, accepting an optional pre-computed scan.
    ///
    /// When `pending_scan` is `Some`, the caller already ran the scanner and
    /// will commit state after this method returns successfully.
    ///
    /// When `pending_scan` is `None`, this method creates its own scanner and
    /// commits state only after the graph update succeeds.
    ///
    /// LSP enrichment runs synchronously inside `emit_enrichment_pipeline` via `LspConsumer`.
    /// The `_spawn_lsp` parameter is kept for API compatibility but is no longer acted on.
    pub(crate) async fn update_graph_with_scan(
        &self,
        graph: &mut GraphState,
        pending_scan: Option<ScanResult>,
        _spawn_lsp: bool,
    ) -> anyhow::Result<()> {
        // If no pre-computed scan, create a fresh scanner. We hold it so we
        // can commit state after successful processing.
        let mut fallback_scanner: Option<Scanner> = None;
        let scan = match pending_scan {
            Some(s) => s,
            None => {
                // Fallback: scan fresh (used by background scanner path)
                let mut scanner = Scanner::new(self.repo_root.clone())?;
                let result = scanner.scan()?;
                fallback_scanner = Some(scanner);
                result
            }
        };

        if scan.changed_files.is_empty()
            && scan.new_files.is_empty()
            && scan.deleted_files.is_empty()
        {
            return Ok(());
        }

        tracing::info!(
            "Incremental update: {} changed, {} new, {} deleted",
            scan.changed_files.len(),
            scan.new_files.len(),
            scan.deleted_files.len()
        );

        let registry = ExtractorRegistry::with_builtins();

        // Remove nodes/edges for deleted + changed files.
        // HashSet for O(1) lookup instead of O(F) Vec scan per edge (#586).
        let mut files_to_remove: std::collections::HashSet<PathBuf> = scan
            .deleted_files
            .iter()
            .chain(scan.changed_files.iter())
            .cloned()
            .collect();

        // Collect edge stable IDs for removed/changed files BEFORE retain, so we can
        // delete them from LanceDB. (After retain they're gone from memory.)
        let deleted_edge_ids: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| {
                files_to_remove.contains(&e.from.file)
                    || files_to_remove.contains(&e.to.file)
            })
            .map(|e| e.stable_id())
            .collect();

        graph
            .nodes
            .retain(|n| !files_to_remove.contains(&n.id.file));
        graph.edges.retain(|e| {
            !files_to_remove.contains(&e.from.file)
                && !files_to_remove.contains(&e.to.file)
        });

        // Extract new + changed files
        let (mut extraction, enc_stats) = registry.extract_scan_result_with_stats(&self.repo_root, &scan);

        // Set root slug on extracted nodes and edges.
        // Extractors don't set root -- the caller must assign it, matching the
        // pattern in build_full_graph and the background scanner.
        let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();

        // Merge encoding stats (incremental scan: add to existing totals).
        if let Ok(mut stats) = self.scan_stats.write() {
            stats.merge_encoding_stats(&primary_slug, &enc_stats);
        }
        for node in &mut extraction.nodes {
            node.id.root = primary_slug.clone();
        }
        // Build file index from existing graph + new extraction for suffix matching
        let file_index: std::collections::HashSet<String> = graph.nodes
            .iter()
            .chain(extraction.nodes.iter())
            .map(|n| n.id.file.to_string_lossy().to_string())
            .collect();
        for edge in &mut extraction.edges {
            edge.from.root = primary_slug.clone();
            edge.to.root = primary_slug.clone();
            // Resolve dangling import edges via suffix match (same as build_full_graph)
            helpers::resolve_edge_target_by_suffix(edge, &file_index);
        }

        // Track which node/edge IDs are in the delta for LanceDB upsert.
        // We snapshot IDs now but rebuild the actual upsert data AFTER PageRank
        // so persisted nodes include updated importance scores.
        //
        // Post-extraction passes (api_link, manifest, tested_by, etc.) append new
        // nodes/edges to the graph vecs. Rather than requiring each pass to manually
        // call upsert_node_ids.extend()/upsert_edges.extend() — a pattern that has
        // caused multiple regressions when forgotten — we snapshot the existing stable
        // IDs before passes run and auto-collect the delta after all passes complete.
        //
        // Two snapshot points are needed because the dedup step between Group 1 passes
        // (api_link … nextjs_routing) and Group 2 passes (subsystem, fw, pubsub, ws)
        // compacts the vecs, making index-based slicing incorrect across that boundary.
        // Instead we snapshot existing stable IDs as a HashSet; the auto-collect step
        // at the end of each group extends the delta with anything not in the snapshot.
        //
        // Exception: subsystem metadata is updated IN-PLACE on existing nodes (their
        // stable IDs don't change), so those mutations are still tracked explicitly via
        // upsert_node_ids.insert(sid) inside the subsystem loop below.
        let mut upsert_node_ids: std::collections::HashSet<String> =
            extraction.nodes.iter().map(|n| n.stable_id()).collect();
        let mut upsert_edges: Vec<Edge> = extraction.edges.clone();
        graph.nodes.extend(extraction.nodes);
        graph.edges.extend(extraction.edges);

        // Snapshot existing IDs before Group 1 post-extraction passes so we can
        // auto-collect everything they add into the upsert delta.
        let node_ids_before_passes: std::collections::HashSet<String> =
            graph.nodes.iter().map(|n| n.stable_id()).collect();
        let edge_ids_before_passes: std::collections::HashSet<String> =
            graph.edges.iter().map(|e| e.stable_id()).collect();

        // Pre-clean: remove stale virtual nodes (subsystem, framework, channel, event) and
        // their associated edges BEFORE post-extraction passes run. This ensures
        // detect_communities() only sees real code symbols, and framework_detection_pass
        // re-emits fresh framework nodes with correct state.
        // Collect virtual file paths for LanceDB deletion — these will be added to
        // files_to_remove so the old rows are removed from LanceDB on persist.
        let stale_virtual_files: Vec<std::path::PathBuf> = graph
            .nodes
            .iter()
            .filter(|n| {
                matches!(&n.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event"))
            })
            .map(|n| n.id.file.clone())
            .collect();
        graph.nodes.retain(|n| !matches!(&n.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")));
        graph.edges.retain(|e| {
            !matches!(&e.to.kind, NodeKind::Other(s) if s == "subsystem")
                && e.kind != crate::graph::EdgeKind::UsesFramework
                && e.kind != crate::graph::EdgeKind::Produces
                && e.kind != crate::graph::EdgeKind::Consumes
        });
        // Schedule stale virtual file paths for LanceDB deletion so they won't
        // reappear on reload. The fresh nodes (re-emitted later) will be upserted.
        files_to_remove.extend(stale_virtual_files);

        // Re-run all post-extraction passes via EventBus (ADR Phase 2b, issue #502).
        // Include all roots for passes that need filesystem access (manifest, nextjs_routing).
        // lsp_only roots are intentionally included: nextjs_routing_pass emits ApiEndpoint
        // nodes for lsp_only subdirectory roots so agents can see what routes exist even
        // when tree-sitter extraction was skipped for those roots.
        let root_pairs_incremental: Vec<(String, std::path::PathBuf)> = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_declared_roots(&self.repo_root)
            .resolved_roots()
            .iter()
            .map(|r| (r.slug.clone(), r.path.clone()))
            .collect();
        {
            // Incremental scan: the primary root is always dirty (we only get here
            // when there are changed/new/deleted files in the primary root).
            let dirty_slugs: Option<std::collections::HashSet<String>> =
                Some(std::iter::once(primary_slug.clone()).collect());

            let (enriched_nodes, enriched_edges, detected_frameworks) =
                crate::extract::consumers::emit_enrichment_pipeline(
                    std::mem::take(&mut graph.nodes),
                    std::mem::take(&mut graph.edges),
                    root_pairs_incremental,
                    primary_slug.clone(),
                    self.repo_root.clone(),
                    crate::extract::consumers::BusOptions {
                        scan_stats: Some(Arc::clone(&self.scan_stats)),
                        embed_idx: None, // embed handled below via targeted reindex_nodes after PageRank
                        lance_repo_root: None, // LanceDB persist handled below via persist_graph_incremental
                    },
                    dirty_slugs,
                ).await.map_err(|e| {
                    // Pipeline invariant violated — abort the incremental update so the
                    // partial graph is not persisted. Scanner state is not committed on
                    // Err return, so the next scan will retry the full pass sequence.
                    e.context("incremental update aborted: post-extraction passes did not complete")
                })?;
            graph.nodes = enriched_nodes;
            graph.edges = enriched_edges;
            graph.detected_frameworks = detected_frameworks;
        }

        // Auto-collect delta: everything added by post-extraction passes since
        // the snapshot above. No pass needs to manually report its own output.
        for n in &graph.nodes {
            let sid = n.stable_id();
            if !node_ids_before_passes.contains(&sid) {
                upsert_node_ids.insert(sid);
            }
        }
        for e in &graph.edges {
            let sid = e.stable_id();
            if !edge_ids_before_passes.contains(&sid) {
                upsert_edges.push(e.clone());
            }
        }

        // Deduplicate graph.nodes and graph.edges in-place before rebuilding the
        // petgraph index.  Post-extraction passes (api_link, manifest, tested_by,
        // directory_module, nextjs_routing) re-run over the full node/edge set on
        // every incremental scan, which causes the same entries to be appended
        // repeatedly.  After N scans each entry appears N times, causing:
        //   - graph.nodes: duplicate package nodes from manifest_pass (memory growth,
        //     stale data exposed to search/query tools)
        //   - graph.edges: N-multiplied edges → inflated PageRank weights
        //
        // Node dedup: keep the LAST occurrence (newest data). Reverse, retain first
        // seen (which is now the newest), then reverse back to restore order.
        // Edge dedup: keep the FIRST occurrence (stable_id is topology-only; all
        // duplicates are structurally identical).
        {
            let mut seen_nodes = std::collections::HashSet::new();
            graph.nodes.reverse();
            graph.nodes.retain(|n| seen_nodes.insert(n.stable_id()));
            graph.nodes.reverse();

            let mut seen_edges = std::collections::HashSet::new();
            graph.edges.retain(|e| seen_edges.insert(e.stable_id()));
        }

        // Rebuild petgraph index
        graph.index = GraphIndex::new();
        graph.index.rebuild_from_edges(&graph.edges);
        for node in &graph.nodes {
            graph
                .index
                .ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        // LSP enrichment is handled synchronously by `LspConsumer` inside
        // `emit_enrichment_pipeline` (called above). `spawn_incremental_lsp_enrichment`
        // is NOT called here: the bus already ran every LspConsumer for all supported
        // languages before this point. `_spawn_lsp` is retained for API compatibility.

        // `changed_files` is still needed below for targeted re-embedding.
        let changed_files: std::collections::HashSet<_> = scan
            .changed_files
            .iter()
            .chain(scan.new_files.iter())
            .collect();

        // Recompute PageRank importance scores after all graph mutations
        // (extraction + LSP enrichment) are complete.
        let pagerank_scores = graph.index.compute_pagerank(0.85, 20);
        for node in &mut graph.nodes {
            if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                node.metadata.insert("importance".to_string(), format!("{:.6}", score));
            }
        }

        // Recompute subsystem metadata after incremental graph update.
        {
            let node_file_map: std::collections::HashMap<String, String> = graph
                .nodes
                .iter()
                .filter(|n| n.id.root != "external")
                .map(|n| (n.stable_id(), n.id.file.display().to_string()))
                .collect();

            let mut subsystems = graph.index.detect_communities(&pagerank_scores, &node_file_map);
            // Deduplicate subsystem names (matching repo_map rendering)
            {
                let mut name_counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for s in &subsystems {
                    *name_counts.entry(s.name.clone()).or_default() += 1;
                }
                for s in &mut subsystems {
                    if name_counts.get(&s.name).copied().unwrap_or(0) > 1
                        && let Some(iface) = s.interfaces.first() {
                            let short = iface
                                .node_id
                                .split(':')
                                .rev()
                                .nth(1)
                                .unwrap_or(&iface.node_id);
                            s.name = format!("{}/{}", s.name, short);
                        }
                }
            }
            let mut node_subsystem: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for subsystem in &subsystems {
                for member_id in &subsystem.member_ids {
                    node_subsystem.insert(member_id.clone(), subsystem.name.clone());
                }
            }
            // Track nodes whose subsystem changed so they get persisted to LanceDB
            for node in &mut graph.nodes {
                let sid = node.stable_id();
                let old_sub = node.metadata.get(SUBSYSTEM_KEY).cloned();
                let new_sub = node_subsystem.get(&sid).cloned();
                if old_sub != new_sub {
                    match new_sub {
                        Some(name) => { node.metadata.insert(SUBSYSTEM_KEY.to_owned(), name); }
                        None => { node.metadata.remove(SUBSYSTEM_KEY); }
                    }
                    // Include in upsert set so LanceDB gets the updated metadata
                    upsert_node_ids.insert(sid);
                }
            }

            // Snapshot before post-community passes (subsystem_node, fw aggregation).
            // The dedup step above compacts the vecs, so we need a fresh snapshot here
            // rather than relying on the pre-passes snapshot indices.
            let node_ids_before_group2: std::collections::HashSet<String> =
                graph.nodes.iter().map(|n| n.stable_id()).collect();
            let edge_ids_before_group2: std::collections::HashSet<String> =
                graph.edges.iter().map(|e| e.stable_id()).collect();

            // Emit first-class subsystem nodes + BelongsTo edges, then subsystem →
            // framework aggregation. Routed via EventBus:
            //   CommunityDetectionComplete → SubsystemConsumer → SubsystemNodesComplete
            // Stale nodes were already removed in the pre-clean step above.
            let (sub_added_nodes, sub_added_edges) =
                crate::extract::consumers::emit_community_detection(
                    primary_slug.clone(),
                    subsystems.clone(),
                    graph.nodes.clone(),
                ).await.unwrap_or_else(|e| {
                    tracing::warn!("Incremental subsystem promotion via bus failed (non-fatal): {}", e);
                    (vec![], vec![])
                });
            if !sub_added_nodes.is_empty() || !sub_added_edges.is_empty() {
                graph.nodes.extend(sub_added_nodes);
                graph.edges.extend(sub_added_edges);
            }

            // Auto-collect post-community delta: everything added by subsystem_node and
            // fw aggregation since the snapshot above.
            for n in &graph.nodes {
                let sid = n.stable_id();
                if !node_ids_before_group2.contains(&sid) {
                    upsert_node_ids.insert(sid);
                }
            }
            for e in &graph.edges {
                let sid = e.stable_id();
                if !edge_ids_before_group2.contains(&sid) {
                    upsert_edges.push(e.clone());
                }
            }
        }

        // Update petgraph index for virtual nodes added after the initial rebuild
        // (subsystem, framework, channel, event nodes from steps 6b-6c and pub/sub/ws passes).
        // The index was rebuilt at line ~1275 (before these passes ran), so virtual nodes
        // are missing. Add only virtual nodes here — O(virtual nodes + their edges).
        for node in &graph.nodes {
            if matches!(&node.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")) {
                graph.index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }
        }
        for edge in &graph.edges {
            if matches!(&edge.to.kind, NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")) {
                graph.index.add_edge(
                    &edge.from.to_stable_id(),
                    &edge.from.kind.to_string(),
                    &edge.to.to_stable_id(),
                    &edge.to.kind.to_string(),
                    edge.kind.clone(),
                );
            }
        }

        // Re-embed changed-file symbols. Uses the updated graph nodes so enriched
        // metadata is included in the embedding text.
        let embed_guard2 = self.embed_index.load();
        if let Some(ref embed_idx) = **embed_guard2 {
            let changed_file_nodes: Vec<_> = graph
                .nodes
                .iter()
                .filter(|n| changed_files.iter().any(|f| n.id.file == **f))
                .cloned()
                .collect();
            match embed_idx.reindex_nodes(&changed_file_nodes).await {
                Ok(count) => {
                    tracing::info!("Re-embedded {} changed-file nodes after incremental update", count)
                }
                Err(e) => {
                    // reindex_nodes falls back to no-op if the table doesn't exist;
                    // do a full rebuild instead.
                    tracing::warn!("Targeted re-embed failed ({}), falling back to full rebuild", e);
                    if let Err(e2) = embed_idx
                        .index_all_with_symbols(&self.repo_root, &graph.nodes)
                        .await
                    {
                        tracing::warn!("Full embed rebuild also failed: {}", e2);
                    }
                }
            }
        }

        // .oh/ artifacts are now graph nodes (markdown_section with oh_kind metadata).
        // They're re-embedded through the same reindex_nodes path as code symbols
        // when their files change -- no separate reindex_artifacts call needed.

        // Rebuild upsert_nodes from graph.nodes so they include post-PageRank importance.
        let upsert_nodes: Vec<Node> = graph
            .nodes
            .iter()
            .filter(|n| upsert_node_ids.contains(&n.stable_id()))
            .cloned()
            .collect();

        // Persist updated graph incrementally -- only the delta (changed/added nodes and edges).
        // Untouched rows remain in LanceDB as-is. Deleted files are removed by targeted delete.
        // merge_insert keeps tables alive; no empty-result query window.
        //
        // Acquire the write mutex before persisting to prevent concurrent LanceDB write
        // conflicts with background enrichment tasks (#344 round 3). The lock is released
        // after persist completes so the next persist can proceed.
        //
        // Persist failures are logged but do NOT block the MCP response — the in-memory
        // graph update succeeded and queries can proceed. Scanner state is NOT committed on
        // failure, so the next scan re-detects and retries the persist.
        let persist_result = {
            let _lance_guard = self.lance_write_lock.lock().await;
            let files_to_remove_vec: Vec<PathBuf> = files_to_remove.into_iter().collect();
            persist_graph_incremental(
                &self.repo_root,
                &upsert_nodes,
                &upsert_edges,
                &deleted_edge_ids,
                &files_to_remove_vec,
            )
            .await
        };
        let persist_succeeded = match persist_result {
            Ok(true) => {
                tracing::info!("Schema migrated during incremental update; performing full persist now");
                let _lance_guard = self.lance_write_lock.lock().await;
                if let Err(e) = persist_graph_to_lance(&self.repo_root, &graph.nodes, &graph.edges).await {
                    tracing::error!("Full persist after migration failed: {:#}", e);
                    // Don't block MCP response — log and treat as persist failure.
                    // Scanner state won't be committed so next scan retries.
                    false
                } else {
                    true
                }
            }
            Err(e) => {
                tracing::error!("Incremental persist failed (LanceDB error): {:#}", e);
                // Don't return error — the in-memory graph update succeeded.
                // Queries will use the correct in-memory state.
                // Scanner state is NOT committed when false so next scan retries persist.
                false
            }
            Ok(false) => true,
        };

        // Commit fallback scanner state only after successful persist.
        // If persist failed, scanner state is left uncommitted so the next scan
        // re-detects the same changes and retries the LanceDB write.
        if persist_succeeded
            && let Some(scanner) = fallback_scanner {
                scanner.commit_state()?;
            }

        graph.last_scan_completed_at = Some(std::time::Instant::now());

        Ok(())
    }
}

