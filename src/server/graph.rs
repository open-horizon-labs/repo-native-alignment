//! Graph lifecycle: building, incremental updates, and state management.

use std::path::PathBuf;
use std::sync::Arc;

use crate::embed::EmbeddingIndex;
use crate::extract::ExtractorRegistry;
use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;
use crate::graph::store::SCHEMA_VERSION;
use crate::roots::{RootConfig, WorkspaceConfig, cache_state_path};
use crate::scanner::{ScanResult, Scanner};

use super::helpers;
use super::state::GraphState;
use super::store::{
    check_and_migrate_schema, delete_nodes_for_roots, get_stored_root_ids,
    graph_lance_path, load_graph_from_lance, persist_graph_incremental,
    persist_graph_to_lance,
};
use super::RnaHandler;

impl RnaHandler {
    /// Ensure graph is built, check for file changes since last scan.
    /// Returns a read guard to the graph.
    pub(crate) async fn get_graph(&self) -> anyhow::Result<tokio::sync::RwLockReadGuard<'_, Option<GraphState>>> {
        // Fast path: graph exists and scan cooldown hasn't expired.
        // We carry both the scan result AND the scanner forward so the caller
        // can commit scanner state only after successful graph processing.
        let pending: Option<(ScanResult, Scanner)> = {
            let guard = self.graph.read().await;
            if guard.is_some() {
                let skip_scan = {
                    let last = self.last_scan.lock().unwrap();
                    last.elapsed() < std::time::Duration::from_secs(2)
                };
                if skip_scan {
                    return Ok(guard);
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
                    // Update cooldown timestamp
                    *self.last_scan.lock().unwrap() = std::time::Instant::now();
                    return Ok(guard);
                }
                // Changes detected -- carry scan + scanner forward
                drop(guard);
                Some((scan, scanner))
            } else {
                drop(guard);
                None
            }
        };

        // Slow path: build or update graph
        {
            let mut guard = self.graph.write().await;
            if guard.is_none() {
                // First build -- full pipeline
                *guard = Some(self.build_full_graph().await?);
            } else {
                // Incremental update -- pass the already-completed scan so we
                // don't re-scan. Scanner state is committed only after success.
                let (scan, scanner) = match pending {
                    Some(p) => (Some(p.0), Some(p.1)),
                    None => (None, None),
                };
                let graph = guard.as_mut().unwrap();
                self.update_graph_with_scan(graph, scan).await?;
                // Graph update succeeded -- now persist scanner state
                if let Some(scanner) = scanner {
                    scanner.commit_state()?;
                }
            }
        }

        // Update cooldown timestamp
        *self.last_scan.lock().unwrap() = std::time::Instant::now();

        // Start background scanner (once) to keep index warm
        if !self.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
            self.spawn_background_scanner();
        }

        // Downgrade to read lock
        Ok(self.graph.read().await)
    }

    /// Build the full graph from scratch. This is the original get_graph logic.
    ///
    /// When `spawn_background` is true (default for MCP server), embedding and
    /// LSP enrichment are spawned as background tasks so the graph is queryable
    /// immediately. When false (used by `run_pipeline_foreground`), no background
    /// tasks are spawned -- the caller handles embed+LSP itself.
    pub async fn build_full_graph(&self) -> anyhow::Result<GraphState> {
        self.build_full_graph_inner(true).await
    }

    pub(crate) async fn build_full_graph_inner(&self, spawn_background: bool) -> anyhow::Result<GraphState> {
        // Initialize pattern config from .oh/config.toml (once, at first build).
        crate::extract::generic::init_pattern_config(&self.repo_root);

        // Pre-flight: ensure schema version matches before any LanceDB reads/writes.
        let db_path = graph_lance_path(&self.repo_root);
        if check_and_migrate_schema(&db_path).await? {
            tracing::info!("Schema migrated to v{} -- cache rebuilt", SCHEMA_VERSION);
        }

        // Load workspace config and merge with --repo as primary root.
        // Also auto-detect any live git worktrees and Claude Code memory
        // so all roots are indexed on the first full build.
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root);
        let resolved_roots = workspace.resolved_roots();

        // Prune stale roots: compare discovered roots against what LanceDB has stored.
        // Worktrees removed while the server was offline leave orphaned rows that cause
        // duplicate results (see #198).
        let live_slugs: std::collections::HashSet<String> = resolved_roots
            .iter()
            .map(|r| r.slug.clone())
            .collect();
        // Synthetic root IDs (e.g., "external" for LSP virtual nodes) are never
        // discovered by WorkspaceConfig but are valid -- skip them during stale pruning.
        const RESERVED_ROOT_IDS: &[&str] = &["external"];
        match get_stored_root_ids(&self.repo_root).await {
            Ok(stored) => {
                let stale: Vec<String> = stored
                    .into_iter()
                    .filter(|s| !live_slugs.contains(s))
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
            }
            Err(e) => {
                tracing::debug!("Could not query stored roots for stale pruning: {}", e);
            }
        }

        // 1. Scan all roots to detect changes (per-root tracking)
        let mut any_root_changed = false;
        let mut scanners: Vec<(String, Scanner, crate::scanner::ScanResult, PathBuf, bool)> = Vec::new();

        for resolved_root in &resolved_roots {
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
                    // Check if the cached graph already has call edges; if not,
                    // spawn background LSP enrichment (only in background mode --
                    // foreground callers handle LSP themselves).
                    if spawn_background {
                        let has_call_edges = state.edges.iter().any(|e| {
                            matches!(e.kind, crate::graph::EdgeKind::Calls)
                        });
                        if !has_call_edges {
                            tracing::info!(
                                "Cached graph has no call edges -- spawning LSP enrichment"
                            );
                            self.spawn_lsp_enrichment(&state.nodes);
                        } else {
                            tracing::info!(
                                "Cached graph already has call edges -- skipping LSP enrichment"
                            );
                            // Mark LSP as complete since we have cached edges
                            let call_count = state.edges.iter()
                                .filter(|e| matches!(e.kind, crate::graph::EdgeKind::Calls))
                                .count();
                            self.lsp_status.set_complete(call_count);
                        }
                    }

                    // No changes detected -- safe to commit scanner state
                    for (_slug, scanner, _scan, _path, _changed) in &scanners {
                        if let Err(e) = scanner.commit_state() {
                            tracing::error!("Failed to commit scanner state: {}", e);
                        }
                    }

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

        for (root_slug, scanner, _scan_result, root_path, root_changed) in &scanners {
            if !root_changed {
                // Clean root: load from LanceDB cache if available, otherwise extract.
                if let Some(ref cached) = cached_graph {
                    let cached_nodes: Vec<Node> = cached.nodes.iter()
                        .filter(|n| n.id.root == *root_slug)
                        .cloned()
                        .collect();
                    let cached_edges: Vec<Edge> = cached.edges.iter()
                        .filter(|e| e.from.root == *root_slug)
                        .cloned()
                        .collect();

                    if !cached_nodes.is_empty() {
                        tracing::info!(
                            "Clean root '{}': loaded {} nodes, {} edges from cache (preserving LSP edges)",
                            root_slug,
                            cached_nodes.len(),
                            cached_edges.len()
                        );
                        all_nodes.extend(cached_nodes);
                        all_edges.extend(cached_edges);
                        continue;
                    }
                    // Fall through to full extract if cache had no nodes for this root
                    tracing::info!(
                        "Clean root '{}': no cached nodes found, extracting fresh",
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
            let mut extraction = registry.extract_scan_result(root_path, &full_scan);

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

            tracing::info!(
                "Extracted from '{}'{}: {} nodes, {} edges",
                root_slug,
                if *root_changed { " (dirty)" } else { " (no cache)" },
                extraction.nodes.len(),
                extraction.edges.len()
            );

            all_nodes.extend(extraction.nodes);
            all_edges.extend(extraction.edges);
        }

        // Also load cached external/virtual nodes (e.g., from previous LSP enrichment)
        // that don't belong to any current root.
        // Only include nodes whose root is genuinely external/virtual -- not stale
        // worktree items that were deleted but remain in the LanceDB cache.
        if let Some(ref cached) = cached_graph {
            let current_slugs: std::collections::HashSet<&str> = scanners
                .iter()
                .map(|(slug, _, _, _, _)| slug.as_str())
                .collect();
            let non_code = self.non_code_root_slugs();
            let is_virtual_root = |root: &str| -> bool {
                root == "external" || non_code.contains(root)
            };
            let external_nodes: Vec<Node> = cached.nodes.iter()
                .filter(|n| !current_slugs.contains(n.id.root.as_str()) && is_virtual_root(&n.id.root))
                .cloned()
                .collect();
            let external_edges: Vec<Edge> = cached.edges.iter()
                .filter(|e| !current_slugs.contains(e.from.root.as_str()) && is_virtual_root(&e.from.root))
                .cloned()
                .collect();
            if !external_nodes.is_empty() {
                tracing::info!(
                    "Loaded {} external/virtual nodes, {} edges from cache",
                    external_nodes.len(),
                    external_edges.len()
                );
                all_nodes.extend(external_nodes);
                all_edges.extend(external_edges);
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

        // 7. Persist graph to LanceDB
        if let Err(e) = persist_graph_to_lance(&self.repo_root, &all_nodes, &all_edges).await {
            tracing::error!("Failed to persist graph to LanceDB: {}", e);
            return Err(e.context("LanceDB full persist failed during graph build"));
        }

        // Persist succeeded -- commit scanner state for all roots so the
        // next scan doesn't re-process the same files.
        for (_slug, scanner, _scan, _path, _changed) in &scanners {
            if let Err(e) = scanner.commit_state() {
                tracing::error!("Failed to commit scanner state: {}", e);
            }
        }

        // Graph is persisted -- return immediately so agents can query.
        // Embedding and LSP enrichment run in background via the shared graph lock.
        let symbols_ready_at = std::time::Instant::now();

        // Store embed index immediately so it's available for queries.
        // The background task below will always re-index (including .oh/
        // artifacts) via index_all_inner which drops and rebuilds the table.
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
            self.spawn_background_enrichment(&all_nodes);
        }

        Ok(GraphState {
            nodes: all_nodes,
            edges: all_edges,
            index,
            last_scan_completed_at: Some(symbols_ready_at),
        })
    }

    /// Incrementally update the graph, accepting an optional pre-computed scan.
    ///
    /// When `pending_scan` is `Some`, the caller already ran the scanner and
    /// will commit state after this method returns successfully.
    ///
    /// When `pending_scan` is `None`, this method creates its own scanner and
    /// commits state only after the graph update succeeds.
    pub(crate) async fn update_graph_with_scan(
        &self,
        graph: &mut GraphState,
        pending_scan: Option<ScanResult>,
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

        // Remove nodes/edges for deleted + changed files
        let files_to_remove: Vec<PathBuf> = scan
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
                files_to_remove
                    .iter()
                    .any(|f| e.from.file == *f || e.to.file == *f)
            })
            .map(|e| e.stable_id())
            .collect();

        graph
            .nodes
            .retain(|n| !files_to_remove.iter().any(|f| n.id.file == *f));
        graph.edges.retain(|e| {
            !files_to_remove
                .iter()
                .any(|f| e.from.file == *f || e.to.file == *f)
        });

        // Extract new + changed files
        let mut extraction = registry.extract_scan_result(&self.repo_root, &scan);

        // Set root slug on extracted nodes and edges.
        // Extractors don't set root -- the caller must assign it, matching the
        // pattern in build_full_graph and the background scanner.
        let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();
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
        let upsert_node_ids: std::collections::HashSet<String> =
            extraction.nodes.iter().map(|n| n.stable_id()).collect();
        let upsert_edges: Vec<Edge> = extraction.edges.clone();
        graph.nodes.extend(extraction.nodes);
        graph.edges.extend(extraction.edges);

        // Rebuild petgraph index
        graph.index = GraphIndex::new();
        graph.index.rebuild_from_edges(&graph.edges);
        for node in &graph.nodes {
            graph
                .index
                .ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        // Run LSP enrichers on the updated nodes (same as cold-start, but scoped to changed files).
        // Only enrichers matching the languages of changed files are invoked -- if only .rs
        // changed, only rust-analyzer runs (not pyright, marksman, etc.). This scoping is
        // handled by `enrich_all` which filters enrichers by the `languages` vec.
        // LSP enrichment runs in a background task (matching the full-build pattern) so the
        // write lock is released quickly and tool calls aren't blocked.
        let changed_files: std::collections::HashSet<_> = scan
            .changed_files
            .iter()
            .chain(scan.new_files.iter())
            .collect();
        let changed_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| changed_files.iter().any(|f| n.id.file == **f))
            .cloned()
            .collect();

        if !changed_nodes.is_empty() {
            self.spawn_incremental_lsp_enrichment(changed_nodes, graph.index.clone());
        }

        // Recompute PageRank importance scores after all graph mutations
        // (extraction + LSP enrichment) are complete.
        let pagerank_scores = graph.index.compute_pagerank(0.85, 20);
        for node in &mut graph.nodes {
            if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                node.metadata.insert("importance".to_string(), format!("{:.6}", score));
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
        // Persist failures are propagated as errors (not warnings) so the caller
        // knows not to commit scanner state -- ensuring the next scan re-detects
        // the same changes instead of silently losing them.
        match persist_graph_incremental(
            &self.repo_root,
            &upsert_nodes,
            &upsert_edges,
            &deleted_edge_ids,
            &files_to_remove,
        )
        .await
        {
            Ok(true) => {
                tracing::info!("Schema migrated during incremental update; performing full persist now");
                if let Err(e) = persist_graph_to_lance(&self.repo_root, &graph.nodes, &graph.edges).await {
                    tracing::error!("Full persist after migration failed: {}", e);
                    return Err(e.context("LanceDB full persist after migration failed"));
                }
            }
            Err(e) => {
                tracing::error!("Failed to persist updated graph: {}", e);
                return Err(e.context("LanceDB incremental persist failed"));
            }
            _ => {}
        }

        // Commit fallback scanner state only after successful persist.
        if let Some(scanner) = fallback_scanner {
            scanner.commit_state()?;
        }

        graph.last_scan_completed_at = Some(std::time::Instant::now());

        Ok(())
    }
}
