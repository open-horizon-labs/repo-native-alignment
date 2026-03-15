//! Background enrichment: LSP enrichment, embedding pipeline, and background scanner.

use std::path::PathBuf;
use std::sync::Arc;

use crate::embed::EmbeddingIndex;
use crate::extract::{ExtractorRegistry, EnricherRegistry};
use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;
use crate::roots::{WorkspaceConfig, cache_state_path};
use crate::scanner::Scanner;

use super::helpers;
use super::state::GraphState;
use super::store::{
    delete_nodes_for_roots, persist_graph_incremental, persist_graph_to_lance,
};
use super::{PipelineResult, RnaHandler};

impl RnaHandler {
    /// Spawn the background scanner task (event-driven + 15min heartbeat, worktree-aware).
    pub(crate) fn spawn_background_scanner(&self) {
        let graph = Arc::clone(&self.graph);
        let repo_root = self.repo_root.clone();
        tokio::spawn(async move {
            // Track root slugs from the previous tick to detect removed worktrees.
            // Seed from the current resolved roots so the first tick doesn't
            // misidentify every root as "new".
            let mut prev_root_slugs: std::collections::HashSet<String> = WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_claude_memory(&repo_root)
                .resolved_roots()
                .into_iter()
                .map(|r| r.slug)
                .collect();

            // HEAD-change detection state.
            let mut last_head_oid: Option<git2::Oid> = None;
            let mut last_fetch_head_mtime: Option<std::time::SystemTime> = None;

            loop {
                // Check for HEAD or FETCH_HEAD changes before waiting.
                // If a change is detected, trigger an immediate scan rather
                // than waiting for the full 15-min cadence.
                let head_changed = {
                    match git2::Repository::open(&repo_root) {
                        Ok(repo) => match repo.head().and_then(|h| h.peel_to_commit()) {
                            Ok(commit) => {
                                let oid = commit.id();
                                let changed = last_head_oid.map_or(false, |prev| prev != oid);
                                last_head_oid = Some(oid);
                                changed
                            }
                            Err(_) => false,
                        },
                        Err(_) => false,
                    }
                };

                let fetch_head_changed = {
                    let fetch_head_path = repo_root.join(".git").join("FETCH_HEAD");
                    match std::fs::metadata(&fetch_head_path).and_then(|m| m.modified()) {
                        Ok(mtime) => {
                            let changed =
                                last_fetch_head_mtime.map_or(false, |prev| prev != mtime);
                            last_fetch_head_mtime = Some(mtime);
                            changed
                        }
                        Err(_) => false,
                    }
                };

                if head_changed {
                    tracing::info!("HEAD changed -- triggering immediate background scan");
                } else if fetch_head_changed {
                    tracing::info!(
                        "FETCH_HEAD changed -- triggering immediate background scan"
                    );
                } else {
                    // No git-level change detected: wait for the 15-min heartbeat.
                    // The baselines (last_head_oid, last_fetch_head_mtime) are
                    // updated unconditionally at the top of every iteration, so any
                    // commit that arrives during the sleep will be visible on the
                    // very next wake-up without a post-sleep re-read here.
                    tokio::time::sleep(tokio::time::Duration::from_secs(900)).await;
                }

                // Resolve current roots (primary + any live worktrees + claude memory).
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(repo_root.clone())
                    .with_worktrees(&repo_root)
                    .with_claude_memory(&repo_root);
                let resolved_roots = workspace.resolved_roots();
                let current_root_slugs: std::collections::HashSet<String> =
                    resolved_roots.iter().map(|r| r.slug.clone()).collect();

                // Slugs that disappeared -> worktree was removed.
                let removed_slugs: Vec<String> = prev_root_slugs
                    .difference(&current_root_slugs)
                    .cloned()
                    .collect();

                // Scan every live root for file-level changes.
                // We carry scanners forward so we can commit state only after
                // the graph update + LanceDB persist succeeds.
                let mut has_changes = false;
                let mut per_root_scans: Vec<(String, crate::scanner::ScanResult, PathBuf, Scanner)> =
                    Vec::new();
                for resolved_root in &resolved_roots {
                    let root_slug = resolved_root.slug.clone();
                    let root_path = resolved_root.path.clone();
                    let excludes = resolved_root.config.effective_excludes();
                    let is_primary = root_path == repo_root;
                    let mut scanner = if is_primary {
                        match Scanner::with_excludes(root_path.clone(), excludes) {
                            Ok(s) => s,
                            Err(_) => continue,
                        }
                    } else {
                        let state_path = cache_state_path(&root_slug);
                        match Scanner::with_excludes_and_state_path(
                            root_path.clone(),
                            excludes,
                            state_path,
                        ) {
                            Ok(s) => s,
                            Err(_) => continue,
                        }
                    };
                    let scan = match scanner.scan() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    if !scan.changed_files.is_empty()
                        || !scan.new_files.is_empty()
                        || !scan.deleted_files.is_empty()
                    {
                        has_changes = true;
                    }
                    per_root_scans.push((root_slug, scan, root_path, scanner));
                }

                if !has_changes && removed_slugs.is_empty() {
                    prev_root_slugs = current_root_slugs;
                    continue;
                }

                // Collect LanceDB persist deltas per root (outside write guard so
                // persist_graph_incremental can run without holding the lock).
                // Structure: (root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove)
                let mut lance_deltas: Vec<(
                    String,   // root_slug
                    PathBuf,
                    Vec<Node>,
                    Vec<Edge>,
                    Vec<String>,
                    Vec<PathBuf>,
                )> = Vec::new();

                let mut guard = graph.write().await;
                if let Some(ref mut graph_state) = *guard {
                    let registry = ExtractorRegistry::with_builtins();

                    // Drop in-memory nodes/edges for removed worktrees.
                    for slug in &removed_slugs {
                        tracing::info!(
                            "Worktree removed -- dropping in-memory nodes for root '{}'",
                            slug
                        );
                        graph_state.nodes.retain(|n| &n.id.root != slug);
                        graph_state.edges.retain(|e| &e.from.root != slug);
                    }

                    // Apply file-level changes per root.
                    for (root_slug, scan, root_path, _scanner) in &per_root_scans {
                        if scan.changed_files.is_empty()
                            && scan.new_files.is_empty()
                            && scan.deleted_files.is_empty()
                        {
                            continue;
                        }
                        tracing::info!(
                            "Background scan '{}': {} changed, {} new, {} deleted",
                            root_slug,
                            scan.changed_files.len(),
                            scan.new_files.len(),
                            scan.deleted_files.len()
                        );
                        let files_to_remove: Vec<PathBuf> = scan
                            .deleted_files
                            .iter()
                            .chain(scan.changed_files.iter())
                            .cloned()
                            .collect();

                        // Collect edge IDs to delete BEFORE retain (same pattern as foreground).
                        let deleted_edge_ids: Vec<String> = graph_state
                            .edges
                            .iter()
                            .filter(|e| {
                                e.from.root == *root_slug
                                    && files_to_remove
                                        .iter()
                                        .any(|f| e.from.file == *f || e.to.file == *f)
                            })
                            .map(|e| e.stable_id())
                            .collect();

                        graph_state.nodes.retain(|n| {
                            n.id.root != *root_slug
                                || !files_to_remove.iter().any(|f| n.id.file == *f)
                        });
                        graph_state.edges.retain(|e| {
                            e.from.root != *root_slug
                                || !files_to_remove
                                    .iter()
                                    .any(|f| e.from.file == *f || e.to.file == *f)
                        });
                        let mut extraction = registry.extract_scan_result(root_path, scan);
                        for node in &mut extraction.nodes {
                            node.id.root = root_slug.clone();
                        }
                        // Build file index from existing + new nodes for suffix resolution
                        let file_index: std::collections::HashSet<String> = graph_state.nodes
                            .iter()
                            .chain(extraction.nodes.iter())
                            .map(|n| n.id.file.to_string_lossy().to_string())
                            .collect();
                        for edge in &mut extraction.edges {
                            edge.from.root = root_slug.clone();
                            edge.to.root = root_slug.clone();
                            helpers::resolve_edge_target_by_suffix(edge, &file_index);
                        }
                        let upsert_nodes = extraction.nodes.clone();
                        let upsert_edges = extraction.edges.clone();
                        graph_state.nodes.extend(extraction.nodes);
                        graph_state.edges.extend(extraction.edges);

                        lance_deltas.push((
                            root_slug.clone(),
                            root_path.clone(),
                            upsert_nodes,
                            upsert_edges,
                            deleted_edge_ids,
                            files_to_remove,
                        ));
                    }

                    // Rebuild petgraph index.
                    graph_state.index = GraphIndex::new();
                    graph_state.index.rebuild_from_edges(&graph_state.edges);
                    for node in &graph_state.nodes {
                        graph_state.index.ensure_node(
                            &node.stable_id(),
                            &node.id.kind.to_string(),
                        );
                    }

                    // Recompute PageRank importance scores after graph mutation.
                    let pagerank_scores = graph_state.index.compute_pagerank(0.85, 20);
                    for node in &mut graph_state.nodes {
                        if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                            node.metadata.insert("importance".to_string(), format!("{:.6}", score));
                        }
                    }

                    tracing::info!(
                        "Background update: {} nodes, {} edges",
                        graph_state.nodes.len(),
                        graph_state.edges.len()
                    );
                }
                drop(guard);

                // Persist incremental deltas to LanceDB for each root (lock released above).
                // Track which roots persisted successfully so we can commit scanner state.
                let mut persisted_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
                for (slug, root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove) in lance_deltas {
                    match persist_graph_incremental(
                        &root_path,
                        &upsert_nodes,
                        &upsert_edges,
                        &deleted_edge_ids,
                        &files_to_remove,
                    )
                    .await
                    {
                        Ok(true) => {
                            tracing::info!("Background scan: schema migrated; performing full persist now");
                            let snapshot = {
                                let g = graph.read().await;
                                g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                            };
                            if let Some((nodes, edges)) = snapshot {
                                if let Err(e) = persist_graph_to_lance(&repo_root, &nodes, &edges).await {
                                    tracing::error!("Background scan: full persist after migration failed: {}", e);
                                    continue; // Don't commit scanner state for this root
                                }
                            }
                            persisted_slugs.insert(slug);
                        }
                        Ok(false) => {
                            persisted_slugs.insert(slug);
                        }
                        Err(e) => {
                            tracing::error!("Background scan: failed to persist graph delta for '{}': {}", slug, e);
                            // Don't commit scanner state -- next scan will re-detect changes
                        }
                    }
                }

                // Commit scanner state only for roots that persisted successfully.
                for (root_slug, _scan, _root_path, scanner) in &per_root_scans {
                    if persisted_slugs.contains(root_slug) {
                        if let Err(e) = scanner.commit_state() {
                            tracing::error!("Background scan: failed to commit scanner state for '{}': {}", root_slug, e);
                        }
                    }
                }

                // Update freshness timestamp if any root persisted successfully.
                if !persisted_slugs.is_empty() {
                    let mut guard = graph.write().await;
                    if let Some(ref mut gs) = *guard {
                        gs.last_scan_completed_at = Some(std::time::Instant::now());
                    }
                }

                // Purge removed worktree slugs from LanceDB.
                if !removed_slugs.is_empty() {
                    if let Err(e) = delete_nodes_for_roots(&repo_root, &removed_slugs).await {
                        tracing::warn!(
                            "Failed to delete LanceDB rows for removed worktrees: {}",
                            e
                        );
                    }
                }

                prev_root_slugs = current_root_slugs;
            }
        });
        tracing::info!("Background scanner started (event-driven + 15min heartbeat, worktree-aware)");
    }

    /// Spawn background embedding + LSP enrichment after a full graph build.
    ///
    /// The graph is queryable NOW -- these improve quality progressively.
    pub(crate) fn spawn_background_enrichment(&self, all_nodes: &[Node]) {
        let bg_repo_root = self.repo_root.clone();
        let bg_graph = self.graph.clone();
        let bg_embed_index = self.embed_index.clone();
        let bg_lsp_status = self.lsp_status.clone();
        let bg_embed_status = self.embed_status.clone();
        let bg_nodes = all_nodes.to_vec();
        let bg_languages: Vec<String> = all_nodes
            .iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Mark LSP as running BEFORE spawning the background task so
        // status is visible immediately, not blocked behind embedding.
        bg_lsp_status.set_running();

        tokio::spawn(async move {
            // Embed + LSP enrichment run concurrently -- they use independent
            // data stores (embedding table vs graph edges).
            let embeddable_nodes: Vec<Node> = bg_nodes.iter()
                .filter(|n| n.id.root != "external")
                .cloned()
                .collect();

            let embed_repo_root = bg_repo_root.clone();
            let embed_index_ref = bg_embed_index.clone();
            let embed_status = bg_embed_status;
            let embeddable_count = embeddable_nodes.iter()
                .filter(|n| n.id.kind.is_embeddable())
                .count();
            embed_status.set_building(embeddable_count);
            let embed_fut = async move {
                // Always re-index so .oh/ artifacts added since the last
                // full build become searchable.  index_all_inner drops and
                // rebuilds the table -- acceptable at current repo scale.
                match EmbeddingIndex::new(&embed_repo_root).await {
                    Ok(idx) => {
                        match idx.index_all_with_symbols(&embed_repo_root, &embeddable_nodes).await {
                            Ok(count) => {
                                tracing::info!("[background] Embedded {} items", count);
                                embed_status.set_complete(count);
                                // Atomic store -- no mutex needed
                                embed_index_ref.store(Arc::new(Some(idx)));
                            }
                            Err(e) => {
                                tracing::warn!("[background] Embedding failed: {}", e);
                                embed_status.set_complete(0);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[background] EmbeddingIndex init failed: {}", e);
                        embed_status.set_complete(0);
                    }
                }
            };

            let lsp_repo_root = bg_repo_root.clone();
            let lsp_fut = async {
                // Snapshot graph data under lock, then release before the slow
                // enrich_all call so write-side graph updates aren't blocked.
                let (enrich_nodes, enrich_index) = {
                    let guard = bg_graph.read().await;
                    if let Some(ref gs) = *guard {
                        (gs.nodes.clone(), gs.index.clone())
                    } else {
                        bg_lsp_status.set_complete(0);
                        return;
                    }
                };

                let enricher_registry = EnricherRegistry::with_builtins();
                let enrichment = enricher_registry
                    .enrich_all(&enrich_nodes, &enrich_index, &bg_languages, &lsp_repo_root)
                    .await;

                if !enrichment.any_enricher_ran {
                    tracing::info!("[background] LSP enrichment: no server available");
                    bg_lsp_status.set_unavailable();
                    return;
                }

                if enrichment.new_nodes.is_empty()
                    && enrichment.added_edges.is_empty()
                    && enrichment.updated_nodes.is_empty()
                {
                    tracing::info!("[background] LSP enrichment: no changes");
                    bg_lsp_status.set_complete(0);
                    return;
                }

                tracing::info!(
                    "[background] LSP enrichment: {} virtual nodes, {} edges, {} patches",
                    enrichment.new_nodes.len(),
                    enrichment.added_edges.len(),
                    enrichment.updated_nodes.len()
                );

                // Apply enrichment to shared graph
                let mut guard = bg_graph.write().await;
                if let Some(ref mut gs) = *guard {
                    for vnode in &enrichment.new_nodes {
                        gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                    }
                    gs.nodes.extend(enrichment.new_nodes);

                    let persist_edges = enrichment.added_edges.clone();
                    for edge in &enrichment.added_edges {
                        let from_id = edge.from.to_stable_id();
                        let to_id = edge.to.to_stable_id();
                        gs.index.add_edge(
                            &from_id,
                            &edge.from.kind.to_string(),
                            &to_id,
                            &edge.to.kind.to_string(),
                            edge.kind.clone(),
                        );
                    }
                    gs.edges.extend(enrichment.added_edges);

                    let enriched_node_ids: Vec<String> = enrichment.updated_nodes.iter()
                        .map(|(id, _)| id.clone())
                        .collect();
                    for (node_id, patches) in &enrichment.updated_nodes {
                        if let Some(node) = gs.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                            for (key, value) in patches {
                                node.metadata.insert(key.clone(), value.clone());
                            }
                        }
                    }

                    // Persist enrichment incrementally
                    let upsert_nodes: Vec<Node> = gs.nodes.iter()
                        .filter(|n| enriched_node_ids.contains(&n.stable_id()))
                        .cloned()
                        .collect();
                    let edge_count = persist_edges.len();
                    drop(guard); // release lock before async persist
                    match persist_graph_incremental(
                        &lsp_repo_root, &upsert_nodes, &persist_edges, &[], &[],
                    ).await {
                        Ok(true) => {
                            tracing::info!("[background] LSP enrichment: schema migrated; performing full persist");
                            let snapshot = {
                                let g = bg_graph.read().await;
                                g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                            };
                            if let Some((nodes, edges)) = snapshot {
                                if let Err(e) = persist_graph_to_lance(&lsp_repo_root, &nodes, &edges).await {
                                    tracing::error!("[background] LSP enrichment: full persist after migration failed: {}", e);
                                    bg_lsp_status.set_complete_persist_failed(edge_count);
                                } else {
                                    bg_lsp_status.set_complete(edge_count);
                                }
                            } else {
                                bg_lsp_status.set_complete(edge_count);
                            }
                        }
                        Ok(false) => bg_lsp_status.set_complete(edge_count),
                        Err(e) => {
                            tracing::error!("Background LSP enrichment persist failed: {}", e);
                            bg_lsp_status.set_complete_persist_failed(edge_count);
                        }
                    }
                }
            };

            tokio::join!(embed_fut, lsp_fut);
        });
    }

    /// Spawn background LSP enrichment for the given nodes.
    /// Used both by the normal build path and the cache-hit early return path.
    pub(crate) fn spawn_lsp_enrichment(&self, nodes: &[Node]) {
        let bg_repo_root = self.repo_root.clone();
        let bg_graph = self.graph.clone();
        let bg_lsp_status = self.lsp_status.clone();
        let bg_nodes: Vec<Node> = nodes.to_vec();
        let bg_languages: Vec<String> = nodes
            .iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        bg_lsp_status.set_running();

        tokio::spawn(async move {
            // Snapshot graph data under lock, then release before the slow
            // enrich_all call so write-side graph updates aren't blocked.
            let (enrich_nodes, enrich_index) = {
                let guard = bg_graph.read().await;
                if let Some(ref gs) = *guard {
                    (gs.nodes.clone(), gs.index.clone())
                } else {
                    (bg_nodes, crate::graph::index::GraphIndex::new())
                }
            };

            let enricher_registry = EnricherRegistry::with_builtins();
            let enrichment = enricher_registry
                .enrich_all(&enrich_nodes, &enrich_index, &bg_languages, &bg_repo_root)
                .await;

            if !enrichment.any_enricher_ran {
                tracing::info!("[background] LSP enrichment: no server available");
                bg_lsp_status.set_unavailable();
                return;
            }

            if enrichment.new_nodes.is_empty()
                && enrichment.added_edges.is_empty()
                && enrichment.updated_nodes.is_empty()
            {
                tracing::info!("[background] LSP enrichment: no changes");
                bg_lsp_status.set_complete(0);
                return;
            }

            tracing::info!(
                "[background] LSP enrichment: {} virtual nodes, {} edges, {} patches",
                enrichment.new_nodes.len(),
                enrichment.added_edges.len(),
                enrichment.updated_nodes.len()
            );

            // Apply enrichment to shared graph
            let mut guard = bg_graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

                let persist_edges = enrichment.added_edges.clone();
                for edge in &enrichment.added_edges {
                    let from_id = edge.from.to_stable_id();
                    let to_id = edge.to.to_stable_id();
                    gs.index.add_edge(
                        &from_id,
                        &edge.from.kind.to_string(),
                        &to_id,
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
                gs.edges.extend(enrichment.added_edges);

                let enriched_node_ids: Vec<String> = enrichment.updated_nodes.iter()
                    .map(|(id, _)| id.clone())
                    .collect();
                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(node) = gs.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }

                let upsert_nodes: Vec<Node> = gs.nodes.iter()
                    .filter(|n| enriched_node_ids.contains(&n.stable_id()))
                    .cloned()
                    .collect();
                let edge_count = persist_edges.len();
                drop(guard);
                match persist_graph_incremental(
                    &bg_repo_root, &upsert_nodes, &persist_edges, &[], &[],
                ).await {
                    Ok(true) => {
                        tracing::info!("[background] LSP enrichment: schema migrated; performing full persist");
                        let snapshot = {
                            let g = bg_graph.read().await;
                            g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                        };
                        if let Some((nodes, edges)) = snapshot {
                            if let Err(e) = persist_graph_to_lance(&bg_repo_root, &nodes, &edges).await {
                                tracing::error!("[background] LSP enrichment: full persist after migration failed: {}", e);
                                bg_lsp_status.set_complete_persist_failed(edge_count);
                            } else {
                                bg_lsp_status.set_complete(edge_count);
                            }
                        } else {
                            bg_lsp_status.set_complete(edge_count);
                        }
                    }
                    Ok(false) => bg_lsp_status.set_complete(edge_count),
                    Err(e) => {
                        tracing::error!("Background LSP enrichment persist failed: {}", e);
                        bg_lsp_status.set_complete_persist_failed(edge_count);
                    }
                }
            }
        });
    }

    /// Spawn incremental LSP enrichment for changed nodes after an incremental update.
    pub(crate) fn spawn_incremental_lsp_enrichment(
        &self,
        changed_nodes: Vec<Node>,
        index: GraphIndex,
    ) {
        let languages: Vec<String> = changed_nodes
            .iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        tracing::info!(
            "Spawning background incremental LSP enrichment for {} nodes ({} languages: [{}])",
            changed_nodes.len(),
            languages.len(),
            languages.join(", ")
        );

        // Snapshot what we need for the background task, then release the write lock.
        let bg_graph = self.graph.clone();
        let bg_repo_root = self.repo_root.clone();
        let bg_lsp_status = self.lsp_status.clone();
        let bg_embed_index = self.embed_index.clone();

        self.lsp_status.set_running();

        tokio::spawn(async move {
            let enricher_registry = EnricherRegistry::with_builtins();

            // Check if any registered enricher supports the delta's languages.
            // If not, this is expected (e.g. yaml-only change with no yaml LSP) --
            // don't mark global LSP status as unavailable since other enrichers
            // (rust-analyzer, marksman) may still be healthy.
            let supported = enricher_registry.supported_languages();
            let has_supported_language = languages.iter().any(|l| supported.contains(l));

            let enrichment = enricher_registry
                .enrich_all(&changed_nodes, &index, &languages, &bg_repo_root)
                .await;

            if !enrichment.any_enricher_ran {
                if has_supported_language {
                    // Enricher was expected to run but didn't -- server likely missing.
                    bg_lsp_status.set_unavailable();
                } else {
                    // No enricher supports these languages -- not an error.
                    bg_lsp_status.set_complete(0);
                }
                return;
            }

            if enrichment.new_nodes.is_empty()
                && enrichment.added_edges.is_empty()
                && enrichment.updated_nodes.is_empty()
            {
                tracing::info!("[incremental-bg] LSP enrichment: no changes");
                bg_lsp_status.set_complete(0);
                return;
            }

            tracing::info!(
                "[incremental-bg] LSP enrichment: {} virtual nodes, {} edges, {} patches",
                enrichment.new_nodes.len(),
                enrichment.added_edges.len(),
                enrichment.updated_nodes.len()
            );

            // Acquire write lock briefly for in-memory graph mutation only.
            let mut guard = bg_graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes.clone());

                let persist_edges = enrichment.added_edges.clone();
                for edge in &enrichment.added_edges {
                    gs.index.add_edge(
                        &edge.from.to_stable_id(),
                        &edge.from.kind.to_string(),
                        &edge.to.to_stable_id(),
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
                gs.edges.extend(enrichment.added_edges);

                let enriched_node_ids: std::collections::HashSet<String> =
                    enrichment.updated_nodes.iter().map(|(id, _)| id.clone()).collect();
                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(node) = gs.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }

                // Recompute PageRank after topology changes.
                let pagerank_scores = gs.index.compute_pagerank(0.85, 20);
                for node in &mut gs.nodes {
                    if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                        node.metadata.insert("importance".to_string(), format!("{:.6}", score));
                    }
                }

                // Collect nodes to persist/re-embed AFTER PageRank so importance is current.
                let all_upsert_node_ids: std::collections::HashSet<String> =
                    enriched_node_ids.iter().cloned()
                        .chain(enrichment.new_nodes.iter().map(|n| n.stable_id()))
                        .collect();
                let all_upsert_nodes: Vec<Node> = gs.nodes.iter()
                    .filter(|n| all_upsert_node_ids.contains(&n.stable_id()))
                    .cloned()
                    .collect();

                let edge_count = persist_edges.len();
                drop(guard); // Release lock before slow I/O.

                // Re-embed enriched nodes.
                let embed_guard = bg_embed_index.load();
                if let Some(ref embed_idx) = **embed_guard {
                    if let Err(e) = embed_idx.reindex_nodes(&all_upsert_nodes).await {
                        tracing::warn!("[incremental-bg] Failed to re-embed enriched nodes: {}", e);
                    }
                }

                // Persist to LanceDB (slow -- outside the lock).
                match persist_graph_incremental(
                    &bg_repo_root, &all_upsert_nodes, &persist_edges, &[], &[],
                ).await {
                    Ok(true) => {
                        tracing::info!("[incremental-bg] LSP enrichment: schema migrated; performing full persist");
                        let snapshot = {
                            let g = bg_graph.read().await;
                            g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                        };
                        if let Some((nodes, edges)) = snapshot {
                            if let Err(e) = persist_graph_to_lance(&bg_repo_root, &nodes, &edges).await {
                                tracing::error!("[incremental-bg] Full persist after migration failed: {}", e);
                                bg_lsp_status.set_complete_persist_failed(edge_count);
                            } else {
                                bg_lsp_status.set_complete(edge_count);
                            }
                        } else {
                            bg_lsp_status.set_complete(edge_count);
                        }
                    }
                    Ok(false) => bg_lsp_status.set_complete(edge_count),
                    Err(e) => {
                        tracing::error!("[incremental-bg] LSP enrichment persist failed: {}", e);
                        bg_lsp_status.set_complete_persist_failed(edge_count);
                    }
                }
            }
        });
    }

    /// Run the full pipeline synchronously with progress reporting.
    ///
    /// This is the `--full` CLI path. It runs the same pipeline as
    /// `build_full_graph()` but does embedding and LSP enrichment in the
    /// foreground so the caller can observe progress and the process doesn't
    /// exit before background tasks complete.
    ///
    /// The `on_progress` callback receives structured status messages.
    pub async fn run_pipeline_foreground<F>(
        &self,
        on_progress: F,
    ) -> anyhow::Result<PipelineResult>
    where
        F: Fn(&str) + Send + Sync,
    {
        let pipeline_start = std::time::Instant::now();

        // Phase 1: Scan + Extract (reuses build_full_graph without background tasks)
        let t0 = std::time::Instant::now();
        let graph_state = self.build_full_graph_inner(false).await?;
        let scan_extract_time = t0.elapsed();

        let file_count = graph_state.nodes.iter()
            .map(|n| n.id.file.to_string_lossy().to_string())
            .collect::<std::collections::HashSet<_>>()
            .len();

        on_progress(&format!(
            "Scan+Extract: {} symbols across {} files in {:.1}s",
            graph_state.nodes.len(),
            file_count,
            scan_extract_time.as_secs_f64(),
        ));

        // Store graph state so it is available for queries during embed+LSP.
        {
            let mut guard = self.graph.write().await;
            *guard = Some(GraphState {
                nodes: graph_state.nodes.clone(),
                edges: graph_state.edges.clone(),
                index: {
                    let mut idx = crate::graph::index::GraphIndex::new();
                    idx.rebuild_from_edges(&graph_state.edges);
                    for node in &graph_state.nodes {
                        idx.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                    }
                    idx
                },
                last_scan_completed_at: graph_state.last_scan_completed_at,
            });
        }

        // Phase 2: Embed + LSP enrichment (parallel -- they use independent data stores)
        let embeddable_nodes: Vec<Node> = graph_state.nodes.iter()
            .filter(|n| n.id.root != "external")
            .cloned()
            .collect();

        let languages: Vec<String> = graph_state.nodes
            .iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let server_name = self.lsp_status.server_name();
        if let Some(ref name) = server_name {
            on_progress(&format!("LSP: {} found on PATH", name));
        }
        self.lsp_status.set_running();

        let embed_repo_root = self.repo_root.clone();
        let embed_index_ref = self.embed_index.clone();
        let embed_fut = async {
            let t1 = std::time::Instant::now();
            let count = match EmbeddingIndex::new(&embed_repo_root).await {
                Ok(idx) => {
                    match idx.index_all_with_symbols(&embed_repo_root, &embeddable_nodes).await {
                        Ok(count) => {
                            embed_index_ref.store(Arc::new(Some(idx)));
                            count
                        }
                        Err(e) => {
                            tracing::warn!("Embed: failed -- {}", e);
                            0
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Embed: init failed -- {}", e);
                    0
                }
            };
            let elapsed = t1.elapsed();
            (count, elapsed)
        };

        on_progress("LSP: waiting for server ready...");

        let lsp_fut = async {
            let t2 = std::time::Instant::now();
            let enricher_registry = EnricherRegistry::with_builtins();
            let enrichment = enricher_registry
                .enrich_all(&graph_state.nodes, &graph_state.index, &languages, &self.repo_root)
                .await;
            let elapsed = t2.elapsed();
            (enrichment, elapsed)
        };

        let ((embed_count, embed_time), (enrichment, lsp_time)) =
            tokio::join!(embed_fut, lsp_fut);

        on_progress(&format!(
            "Embed: {} items in {:.1}s",
            embed_count,
            embed_time.as_secs_f64(),
        ));

        let lsp_edge_count;

        if !enrichment.any_enricher_ran {
            on_progress(&format!("LSP: no server available ({:.1}s)", lsp_time.as_secs_f64()));
            self.lsp_status.set_unavailable();
            lsp_edge_count = 0;
        } else {
            lsp_edge_count = enrichment.added_edges.len();
            on_progress(&format!(
                "LSP: enriched {} call edges in {:.1}s",
                lsp_edge_count,
                lsp_time.as_secs_f64(),
            ));

            // Apply enrichment to graph
            let mut guard = self.graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

                let persist_edges = enrichment.added_edges.clone();
                for edge in &enrichment.added_edges {
                    let from_id = edge.from.to_stable_id();
                    let to_id = edge.to.to_stable_id();
                    gs.index.add_edge(
                        &from_id,
                        &edge.from.kind.to_string(),
                        &to_id,
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
                gs.edges.extend(enrichment.added_edges);

                let enriched_node_ids: Vec<String> = enrichment.updated_nodes.iter()
                    .map(|(id, _)| id.clone())
                    .collect();
                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(node) = gs.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }

                let upsert_nodes: Vec<Node> = gs.nodes.iter()
                    .filter(|n| enriched_node_ids.contains(&n.stable_id()))
                    .cloned()
                    .collect();
                drop(guard);
                match persist_graph_incremental(
                    &self.repo_root, &upsert_nodes, &persist_edges, &[], &[],
                ).await {
                    Ok(true) => {
                        tracing::info!("Foreground LSP enrichment: schema migrated; performing full persist");
                        let snapshot = {
                            let g = self.graph.read().await;
                            g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                        };
                        if let Some((nodes, edges)) = snapshot {
                            if let Err(e) = persist_graph_to_lance(&self.repo_root, &nodes, &edges).await {
                                tracing::error!("Foreground LSP enrichment: full persist after migration failed: {}", e);
                                self.lsp_status.set_complete_persist_failed(lsp_edge_count);
                                return Err(e.context("LSP enrichment full persist after migration failed"));
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::error!("Foreground LSP enrichment persist failed: {}", e);
                        self.lsp_status.set_complete_persist_failed(lsp_edge_count);
                        return Err(e.context("LSP enrichment persist failed during foreground pipeline"));
                    }
                }
            }
            self.lsp_status.set_complete(lsp_edge_count);
        }

        // Phase 4: Summary
        let total_node_count = {
            let guard = self.graph.read().await;
            guard.as_ref().map(|gs| gs.nodes.len()).unwrap_or(graph_state.nodes.len())
        };
        let total_edge_count = {
            let guard = self.graph.read().await;
            guard.as_ref().map(|gs| gs.edges.len()).unwrap_or(graph_state.edges.len())
        };

        let total_time = pipeline_start.elapsed();
        on_progress(&format!(
            "Graph: {} nodes, {} edges",
            total_node_count,
            total_edge_count,
        ));
        on_progress(&format!("Done in {:.1}s", total_time.as_secs_f64()));

        Ok(PipelineResult {
            node_count: total_node_count,
            edge_count: total_edge_count,
            lsp_edge_count,
            embed_count,
            total_time,
        })
    }
}
