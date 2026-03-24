//! Background enrichment: LSP enrichment, embedding pipeline, and background scanner.
// EXTRACTION_VERSION is deprecated (#526) but still used for backward-compat migration.
#![allow(deprecated)]

use std::path::PathBuf;

/// LanceDB persist delta: (root_slug, root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove)
type LanceDelta = (String, PathBuf, Vec<Node>, Vec<Edge>, Vec<String>, std::collections::HashSet<PathBuf>);
use std::sync::Arc;

use crate::embed::EmbeddingIndex;
use crate::extract::{ExtractorRegistry, EnricherRegistry};
use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;
use crate::roots::{RootConfig, WorkspaceConfig, cache_state_path};
use crate::scanner::Scanner;

use super::helpers;
use super::state::GraphState;
use super::store::{
    check_and_migrate_extraction_version, delete_nodes_for_roots, persist_graph_incremental,
    persist_graph_to_lance,
};
use super::{PipelineResult, RnaHandler};

impl RnaHandler {
    /// Spawn the background scanner task (event-driven + 15min heartbeat, worktree-aware).
    pub(crate) fn spawn_background_scanner(&self) {
        let graph = Arc::clone(&self.graph);
        let repo_root = self.repo_root.clone();
        let lance_write_lock = Arc::clone(&self.lance_write_lock);
        let scan_stats = Arc::clone(&self.scan_stats);
        tokio::spawn(async move {
            // Track root slugs from the previous tick to detect removed worktrees.
            // Seed from the current resolved roots so the first tick doesn't
            // misidentify every root as "new".
            let mut prev_root_slugs: std::collections::HashSet<String> = WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_claude_memory(&repo_root)
                .with_agent_memories(&repo_root)
                .with_declared_roots(&repo_root)
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
                                let changed = last_head_oid.is_some_and(|prev| prev != oid);
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
                                last_fetch_head_mtime.is_some_and(|prev| prev != mtime);
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

                // Resolve current roots (primary + any live worktrees + claude memory + agent memories + declared roots).
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(repo_root.clone())
                    .with_worktrees(&repo_root)
                    .with_claude_memory(&repo_root)
                    .with_agent_memories(&repo_root)
                    .with_declared_roots(&repo_root);
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
                    // Skip lsp_only roots: their files are already covered by the primary root
                    // scan. Running a scanner over them would produce duplicate extraction.
                    if resolved_root.config.lsp_only {
                        continue;
                    }
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
                let mut lance_deltas: Vec<LanceDelta> = Vec::new();

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
                        // Use a HashSet for O(1) membership checks during retain/filter
                        // instead of O(n) Vec::contains / iter().any(). With a Vec the
                        // retain loops over nodes/edges become O(nodes × files_to_remove).
                        let files_to_remove: std::collections::HashSet<PathBuf> = scan
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
                                    && (files_to_remove.contains(&e.from.file)
                                        || files_to_remove.contains(&e.to.file))
                            })
                            .map(|e| e.stable_id())
                            .collect();

                        graph_state.nodes.retain(|n| {
                            n.id.root != *root_slug
                                || !files_to_remove.contains(&n.id.file)
                        });
                        graph_state.edges.retain(|e| {
                            e.from.root != *root_slug
                                || (!files_to_remove.contains(&e.from.file)
                                    && !files_to_remove.contains(&e.to.file))
                        });
                        let (mut extraction, enc_stats) = registry.extract_scan_result_with_stats(root_path, scan);

                        // Merge encoding stats (incremental scan: add to existing totals).
                        if let Ok(mut stats) = scan_stats.write() {
                            stats.merge_encoding_stats(root_slug, &enc_stats);
                        }

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

                    // Run post-extraction passes via EventBus (ADR Phase 2b, issue #502).
                    // Both foreground and background paths now use the same bus-driven
                    // consumer chain. Satisfies ADR Constraint 4 (no pass calls in src/server/).
                    {
                        // Snapshot existing stable_ids before the passes run.
                        let before_node_ids: std::collections::HashSet<String> =
                            graph_state.nodes.iter().map(|n| n.stable_id()).collect();
                        let before_edge_ids: std::collections::HashSet<String> =
                            graph_state.edges.iter().map(|e| e.stable_id()).collect();

                        let root_pairs: Vec<(String, std::path::PathBuf)> =
                            WorkspaceConfig::load()
                                .with_primary_root(repo_root.clone())
                                .with_worktrees(&repo_root)
                                .with_declared_roots(&repo_root)
                                .resolved_roots()
                                .into_iter()
                                .map(|r| (r.slug, r.path))
                                .collect();
                        let primary_slug =
                            RootConfig::code_project(repo_root.clone()).slug();

                        // Compute dirty_slugs: only roots that had file changes should
                        // trigger LSP enrichment (#555). `Some(set)` = only those roots.
                        let dirty_slugs: Option<std::collections::HashSet<String>> = Some(per_root_scans
                            .iter()
                            .filter(|(_, scan, _, _)| {
                                !scan.changed_files.is_empty()
                                    || !scan.new_files.is_empty()
                                    || !scan.deleted_files.is_empty()
                            })
                            .map(|(slug, _, _, _)| slug.clone())
                            .collect());

                        // Pipeline invariant: EnrichmentFinalizer always emits PassesComplete.
                        // If it doesn't (a logic bug), log the error and clear lance_deltas so
                        // the empty graph is not persisted. The graph remains empty until the
                        // next full rebuild (next startup or manual `scan --full`).
                        match crate::extract::consumers::emit_enrichment_pipeline(
                            std::mem::take(&mut graph_state.nodes),
                            std::mem::take(&mut graph_state.edges),
                            root_pairs,
                            primary_slug.clone(),
                            repo_root.clone(),
                            crate::extract::consumers::BusOptions {
                                scan_stats: Some(Arc::clone(&scan_stats)),
                                embed_idx: None, // embed handled by background scanner's own reindex pass
                                lance_repo_root: None, // LanceDB persist handled by background scanner's lance_deltas
                            },
                            dirty_slugs,
                        ).await {
                            Ok((enriched_nodes, enriched_edges, detected_frameworks)) => {
                                graph_state.nodes = enriched_nodes;
                                graph_state.edges = enriched_edges;
                                graph_state.detected_frameworks = detected_frameworks;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Background scanner: post-extraction passes failed \
                                     (pipeline invariant violated) — aborting tick, \
                                     no data will be persisted: {:#}",
                                    e
                                );
                                // Clear deltas so nothing bad gets written to LanceDB.
                                lance_deltas.clear();
                                // graph_state.nodes/edges are now empty (taken above).
                                // An empty graph is safe for the subsequent rebuild-index
                                // and pagerank steps (both are no-ops on an empty set).
                            }
                        }

                        // Deduplicate nodes and edges after post-extraction passes.
                        // Passes re-emit edges that may already be present from cached
                        // roots; dedup here mirrors the full-build dedup block (6z).
                        {
                            let mut seen_nodes = std::collections::HashSet::new();
                            graph_state.nodes.reverse();
                            graph_state.nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                            graph_state.nodes.reverse();

                            let mut seen_edges = std::collections::HashSet::new();
                            graph_state.edges.retain(|e| seen_edges.insert(e.stable_id()));
                        }

                        // Collect net-new nodes/edges introduced by the passes.
                        // These must be persisted to LanceDB so they survive restarts.
                        let new_nodes: Vec<Node> = graph_state
                            .nodes
                            .iter()
                            .filter(|n| !before_node_ids.contains(&n.stable_id()))
                            .cloned()
                            .collect();
                        let new_edges: Vec<Edge> = graph_state
                            .edges
                            .iter()
                            .filter(|e| !before_edge_ids.contains(&e.stable_id()))
                            .cloned()
                            .collect();

                        if !new_nodes.is_empty() || !new_edges.is_empty() {
                            tracing::info!(
                                "Post-extraction passes added {} node(s), {} edge(s) to persist delta",
                                new_nodes.len(),
                                new_edges.len()
                            );
                            // Use repo_root as the path for cross-cutting additions
                            // (api_link, manifest, etc. are global, not per-root).
                            lance_deltas.push((
                                primary_slug,
                                repo_root.clone(),
                                new_nodes,
                                new_edges,
                                Vec::new(), // no deletions — passes only add
                                std::collections::HashSet::new(), // no files removed
                            ));
                        }
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
                // Acquire the write mutex to serialize with other LanceDB writers (#344 round 3).
                // Track which roots persisted successfully so we can commit scanner state.
                let mut persisted_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
                for (slug, root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove) in lance_deltas {
                    let persist_result = {
                        let _lance_guard = lance_write_lock.lock().await;
                        let files_to_remove_vec: Vec<PathBuf> = files_to_remove.into_iter().collect();
                        persist_graph_incremental(
                            &root_path,
                            &upsert_nodes,
                            &upsert_edges,
                            &deleted_edge_ids,
                            &files_to_remove_vec,
                        )
                        .await
                    };
                    match persist_result {
                        Ok(true) => {
                            tracing::info!("Background scan: schema migrated; performing full persist now");
                            let snapshot = {
                                let g = graph.read().await;
                                g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                            };
                            if let Some((nodes, edges)) = snapshot {
                                let _lance_guard = lance_write_lock.lock().await;
                                if let Err(e) = persist_graph_to_lance(&repo_root, &nodes, &edges).await {
                                    tracing::error!("Background scan: full persist after migration failed: {:#}", e);
                                    continue; // Don't commit scanner state for this root
                                }
                            }
                            persisted_slugs.insert(slug);
                        }
                        Ok(false) => {
                            persisted_slugs.insert(slug);
                        }
                        Err(e) => {
                            tracing::error!("Background scan: failed to persist graph delta for '{}': {:#}", slug, e);
                            // Don't commit scanner state -- next scan will re-detect changes
                        }
                    }
                }

                // Commit scanner state only for roots that persisted successfully.
                for (root_slug, _scan, _root_path, scanner) in &per_root_scans {
                    if persisted_slugs.contains(root_slug)
                        && let Err(e) = scanner.commit_state() {
                            tracing::error!("Background scan: failed to commit scanner state for '{}': {}", root_slug, e);
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
                    let _lance_guard = lance_write_lock.lock().await;
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

    /// Spawn background embedding after a full graph build.
    ///
    /// **Phase 3**: LSP enrichment has been moved into `LspConsumer` within the event bus
    /// (via `emit_enrichment_pipeline`). This function now handles only the embedding pipeline.
    ///
    /// The graph is queryable NOW -- embedding improves semantic search quality progressively.
    pub(crate) fn spawn_background_enrichment(
        &self,
        all_nodes: &[Node],
    ) -> tokio::task::JoinHandle<()> {
        let bg_repo_root = self.repo_root.clone();
        let bg_embed_index = self.embed_index.clone();
        let bg_embed_status = self.embed_status.clone();
        let bg_nodes = all_nodes.to_vec();

        tokio::spawn(async move {
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
                // Use BLAKE3 incremental reindex: hash-skip unchanged items
                // instead of dropping and rebuilding the entire table.
                // Falls back to full rebuild only if the table doesn't exist yet.
                match EmbeddingIndex::new(&embed_repo_root).await {
                    Ok(idx) => {
                        let result = match idx.has_table().await {
                            Ok(true) => {
                                // Table exists -- use incremental reindex with BLAKE3 hash-skipping
                                idx.reindex_nodes(&embeddable_nodes).await
                            }
                            Ok(false) => {
                                // Table missing -- full build needed
                                idx.index_all_with_symbols(&embed_repo_root, &embeddable_nodes).await
                            }
                            Err(e) => {
                                tracing::warn!("[background] Embedding table check failed: {}", e);
                                embed_status.set_complete(0);
                                return;
                            }
                        };
                        match result {
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

            // Phase 3: LSP enrichment now runs inside the event bus via LspConsumer.
            // This function is embedding-only; no lsp_fut here.
            embed_fut.await;
        })
    }

    /// Spawn background LSP enrichment for the cache-hit path, routing through the event bus.
    ///
    /// Called from `build_full_graph_inner` when no files changed and the graph was loaded from
    /// LanceDB but the LSP sentinel is absent (LSP did not complete in the previous run).
    ///
    /// Unlike `spawn_lsp_enrichment`, this path routes through `LspConsumer` → `EnrichmentComplete`
    /// → `AllEnrichmentsGate` → `AllEnrichmentsDone` → `EnrichmentFinalizer` → `PassesComplete`,
    /// so `ScanStatsConsumer` correctly tracks LSP completion and no bus consumers are bypassed.
    ///
    /// The full enrichment pipeline is called with the cached nodes. The resulting enriched
    /// nodes/edges replace the in-memory graph state, and the LSP sentinel is written so
    /// subsequent restarts skip re-enrichment.
    pub(crate) fn spawn_lsp_enrichment_via_bus(&self, nodes: &[Node], edges: &[Edge]) {
        let bg_repo_root = self.repo_root.clone();
        let bg_graph = self.graph.clone();
        let bg_lsp_status = self.lsp_status.clone();
        let bg_lance_write_lock = Arc::clone(&self.lance_write_lock);
        let bg_scan_stats = Arc::clone(&self.scan_stats);
        let bg_nodes: Vec<Node> = nodes.to_vec();
        let bg_edges: Vec<Edge> = edges.to_vec();

        bg_lsp_status.set_running();

        tokio::spawn(async move {
            tracing::info!(
                "[cache-hit bus] LSP enrichment via bus starting with {} nodes, {} edges",
                bg_nodes.len(),
                bg_edges.len()
            );

            // Build workspace root_pairs needed by emit_enrichment_pipeline / LspConsumer.
            let workspace = WorkspaceConfig::load()
                .with_primary_root(bg_repo_root.clone())
                .with_worktrees(&bg_repo_root)
                .with_claude_memory(&bg_repo_root)
                .with_agent_memories(&bg_repo_root)
                .with_declared_roots(&bg_repo_root);
            let root_pairs: Vec<(String, std::path::PathBuf)> = workspace
                .resolved_roots()
                .iter()
                .map(|r| (r.slug.clone(), r.path.clone()))
                .collect();
            let primary_slug = RootConfig::code_project(bg_repo_root.clone()).slug();

            // Run the full enrichment pipeline (bus path): LanguageDetected → LspConsumer
            // → EnrichmentComplete → AllEnrichmentsGate → AllEnrichmentsDone → EnrichmentFinalizer
            // → PassesComplete. scan_stats is wired in so ScanStatsConsumer tracks LSP completion.
            //
            // Consume bg_nodes/bg_edges via move (no redundant clone; these are the only owners).
            // LanceDB persist is handled below after replacing the in-memory graph.
            let bus_repo_root = bg_repo_root.clone();
            // Cache-hit LSP enrichment: all roots are "dirty" because this is
            // the first time LSP runs on a cached graph (no prior LSP edges).
            // `None` = all roots dirty on first LSP run.
            let result = crate::extract::consumers::emit_enrichment_pipeline(
                bg_nodes,
                bg_edges,
                root_pairs,
                primary_slug,
                bus_repo_root,
                crate::extract::consumers::BusOptions {
                    scan_stats: Some(bg_scan_stats),
                    embed_idx: None,
                    lance_repo_root: None,
                },
                None,
            ).await;

            let (mut enriched_nodes, mut enriched_edges, _detected_frameworks) = match result {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("[cache-hit bus] emit_enrichment_pipeline failed: {:#}", e);
                    bg_lsp_status.set_unavailable();
                    return;
                }
            };

            // Dedup: PassesComplete can re-emit cached entries when the cached graph already
            // contains output from a previous pass run. Dedup avoids duplicate rows in LanceDB
            // and inflated edge weights (same logic as the full-build path in graph.rs).
            {
                let mut seen_nodes = std::collections::HashSet::new();
                enriched_nodes.reverse();
                enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                enriched_nodes.reverse();

                let mut seen_edges = std::collections::HashSet::new();
                enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
            }

            // Count LSP-sourced edges to determine enrichment status.
            let lsp_call_edge_count = enriched_edges.iter()
                .filter(|e| {
                    e.source == crate::graph::ExtractionSource::Lsp
                        && matches!(e.kind, crate::graph::EdgeKind::Calls)
                })
                .count();
            let lsp_edge_count = enriched_edges.iter()
                .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                .count();

            tracing::info!(
                "[cache-hit bus] LSP enrichment complete: {} LSP call edges, {} total LSP edges, {} total nodes",
                lsp_call_edge_count, lsp_edge_count, enriched_nodes.len()
            );

            // Build updated index from enriched edges.
            let mut new_index = crate::graph::index::GraphIndex::new();
            new_index.rebuild_from_edges(&enriched_edges);
            for node in &enriched_nodes {
                new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }

            // Replace in-memory graph state with enriched result under write lock.
            {
                let mut guard = bg_graph.write().await;
                if let Some(ref mut gs) = *guard {
                    gs.nodes = enriched_nodes.clone();
                    gs.edges = enriched_edges.clone();
                    gs.index = new_index;
                }
                // Guard drops here, releasing the write lock before LanceDB persist.
            }

            // Persist enriched graph to LanceDB and write sentinel under the write lock
            // so no concurrent writer can interleave between persist and sentinel write.
            let persist_result = {
                let _lance_guard = bg_lance_write_lock.lock().await;
                let result = persist_graph_to_lance(&bg_repo_root, &enriched_nodes, &enriched_edges).await;
                if result.is_ok() {
                    super::sentinel::write_lsp_sentinel(&bg_repo_root, enriched_nodes.len(), enriched_edges.len());
                }
                result
            };

            match persist_result {
                Ok(()) => {
                    tracing::info!(
                        "[cache-hit bus] LSP persist complete: {} nodes, {} edges",
                        enriched_nodes.len(), enriched_edges.len()
                    );
                    // Mirror other LSP paths: set_complete(0) when no edges (enricher ran but found
                    // nothing), not set_unavailable(). The sentinel is written to prevent repeated
                    // re-enrichment on repos that legitimately produce zero LSP edges.
                    bg_lsp_status.set_complete(lsp_call_edge_count);
                }
                Err(e) => {
                    tracing::error!("[cache-hit bus] LSP persist failed: {:#}", e);
                    bg_lsp_status.set_complete_persist_failed(lsp_call_edge_count);
                }
            }
        });
    }

    /// Spawn incremental LSP enrichment for changed nodes after an incremental update.
    ///
    /// Not called from the synchronous bus path (Phase 3+, issue #520) because
    /// `emit_enrichment_pipeline` already runs LspConsumers synchronously.
    /// Retained for future use (e.g., explicit CLI trigger or Phase 4 async path).
    #[allow(dead_code)]
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
        let bg_lance_write_lock = Arc::clone(&self.lance_write_lock);
        let bg_lsp_only_roots = Arc::clone(&self.lsp_only_roots);

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
                .enrich_all(&changed_nodes, &index, &languages, &bg_repo_root, &bg_lsp_only_roots)
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
                // Collect new node IDs before moving nodes into the graph.
                let new_node_ids: Vec<String> = enrichment.new_nodes.iter()
                    .map(|n| n.stable_id())
                    .collect();
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

                // Clone edges for persist (needed after lock is dropped), then move into graph.
                let persist_edges = enrichment.added_edges.clone();
                for edge in &persist_edges {
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
                        .chain(new_node_ids)
                        .collect();
                let all_upsert_nodes: Vec<Node> = gs.nodes.iter()
                    .filter(|n| all_upsert_node_ids.contains(&n.stable_id()))
                    .cloned()
                    .collect();

                let edge_count = persist_edges.len();
                drop(guard); // Release lock before slow I/O.

                // Re-embed enriched nodes.
                let embed_guard = bg_embed_index.load();
                if let Some(ref embed_idx) = **embed_guard
                    && let Err(e) = embed_idx.reindex_nodes(&all_upsert_nodes).await {
                        tracing::warn!("[incremental-bg] Failed to re-embed enriched nodes: {}", e);
                    }

                // Persist to LanceDB (slow -- outside the lock).
                // Acquire the write mutex to serialize with other LanceDB writers (#344 round 3).
                // The sentinel is written inside the same critical section as the persist so
                // another writer cannot race in between (#477 CodeRabbit critical).
                let persist_result = {
                    let _lance_guard = bg_lance_write_lock.lock().await;
                    let result = persist_graph_incremental(
                        &bg_repo_root, &all_upsert_nodes, &persist_edges, &[], &[],
                    ).await;
                    if matches!(result, Ok(false)) {
                        // Incremental persist succeeded -- write sentinel while lock is held.
                        let (total_nodes, total_edges) = {
                            let g = bg_graph.read().await;
                            g.as_ref().map(|gs| (gs.nodes.len(), gs.edges.len()))
                                .unwrap_or((0, 0))
                        };
                        super::sentinel::write_lsp_sentinel(&bg_repo_root, total_nodes, total_edges);
                    }
                    result
                };
                match persist_result {
                    Ok(true) => {
                        tracing::info!("[incremental-bg] LSP enrichment: schema migrated; performing full persist");
                        let snapshot = {
                            let g = bg_graph.read().await;
                            g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
                        };
                        if let Some((nodes, edges)) = snapshot {
                            let _lance_guard = bg_lance_write_lock.lock().await;
                            if let Err(e) = persist_graph_to_lance(&bg_repo_root, &nodes, &edges).await {
                                tracing::error!("[incremental-bg] Full persist after migration failed: {:#}", e);
                                bg_lsp_status.set_complete_persist_failed(edge_count);
                            } else {
                                super::sentinel::write_lsp_sentinel(&bg_repo_root, nodes.len(), edges.len());
                                bg_lsp_status.set_complete(edge_count);
                            }
                        } else {
                            bg_lsp_status.set_complete(edge_count);
                        }
                    }
                    Ok(false) => bg_lsp_status.set_complete(edge_count),
                    Err(e) => {
                        tracing::error!("[incremental-bg] LSP enrichment persist failed: {:#}", e);
                        bg_lsp_status.set_complete_persist_failed(edge_count);
                    }
                }
            }
        });
    }

    /// Run the full pipeline synchronously with progress reporting.
    ///
    /// This is the `--full` CLI path. When a cached graph exists in LanceDB,
    /// it uses the incremental path (only re-extract changed files, LSP on
    /// changed nodes only) for dramatically faster rescans. Falls back to
    /// full rebuild when no cache exists.
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

        // Try incremental path: load cached graph, apply delta, LSP on changed nodes.
        let lance_path = super::store::graph_lance_path(&self.repo_root);
        let cached = if lance_path.exists() {
            match super::store::load_graph_from_lance(&self.repo_root).await {
                Ok(state) => {
                    on_progress(&format!(
                        "Loaded cached graph: {} nodes, {} edges",
                        state.nodes.len(),
                        state.edges.len(),
                    ));
                    Some(state)
                }
                Err(e) => {
                    tracing::debug!("Could not load cached graph, falling back to full rebuild: {}", e);
                    None
                }
            }
        } else {
            None
        };

        if let Some(cached_state) = cached {
            return self.run_pipeline_foreground_incremental(cached_state, on_progress, pipeline_start).await;
        }

        // No cache -- full rebuild path.
        self.run_pipeline_foreground_full(on_progress, pipeline_start).await
    }

    /// Incremental foreground pipeline: load from cache, extract only changed files,
    /// LSP enrich only changed nodes, re-embed only changed symbols.
    async fn run_pipeline_foreground_incremental<F>(
        &self,
        mut cached_state: GraphState,
        on_progress: F,
        pipeline_start: std::time::Instant,
    ) -> anyhow::Result<PipelineResult>
    where
        F: Fn(&str) + Send + Sync,
    {
        // Pre-flight: ensure schema version matches. If migration happened,
        // the cache was rebuilt and our loaded graph is stale -- fall back to
        // full rebuild by returning an error that the caller can catch.
        let db_path = super::store::graph_lance_path(&self.repo_root);
        if super::store::check_and_migrate_schema(&db_path).await? {
            tracing::info!("Schema migrated during incremental pre-flight -- falling back to full rebuild");
            on_progress("Schema migration detected -- rebuilding from scratch.");
            // Clear sentinels -- they reference the old schema version and are now stale.
            super::sentinel::clear_sentinels(&self.repo_root);
            return self.run_pipeline_foreground_full(on_progress, pipeline_start).await;
        }

        // Pre-flight: check extraction version. If it changed (e.g., EXTRACTION_VERSION
        // bumped in a new binary), clear scan-state files and fall back to full rebuild so
        // all files are re-extracted with the updated extraction logic.
        // Without this check the incremental path skips build_full_graph_inner (which
        // contains the extraction-version guard) and reports "incremental, no changes".
        {
            let workspace = WorkspaceConfig::load()
                .with_primary_root(self.repo_root.clone())
                .with_worktrees(&self.repo_root)
                .with_claude_memory(&self.repo_root)
                .with_agent_memories(&self.repo_root)
                .with_declared_roots(&self.repo_root);
            let secondary_slugs: Vec<String> = workspace
                .resolved_roots()
                .iter()
                .filter(|r| r.path != self.repo_root)
                .map(|r| r.slug.clone())
                .collect();
            if check_and_migrate_extraction_version(&db_path, &self.repo_root, &secondary_slugs)? {
                tracing::info!(
                    "Extraction version migrated during incremental pre-flight -- falling back to full rebuild"
                );
                on_progress("Extraction version upgrade detected -- rebuilding from scratch.");
                // Clear sentinels -- they reference the old extraction version and are now stale.
                super::sentinel::clear_sentinels(&self.repo_root);
                return self.run_pipeline_foreground_full(on_progress, pipeline_start).await;
            }
        }

        // Phase 1: Scan to detect changes.
        let t0 = std::time::Instant::now();
        let mut scanner = Scanner::new(self.repo_root.clone())?;
        let scan = scanner.scan()?;
        let scan_time = t0.elapsed();

        let change_count = scan.changed_files.len() + scan.new_files.len() + scan.deleted_files.len();

        on_progress(&format!(
            "Scan: {} changed, {} new, {} deleted in {:.1}s",
            scan.changed_files.len(),
            scan.new_files.len(),
            scan.deleted_files.len(),
            scan_time.as_secs_f64(),
        ));

        if change_count == 0 {
            on_progress("No changes detected -- reusing cached graph.");

            // Store graph and set up embedding index.
            {
                let mut guard = self.graph.write().await;
                *guard = Some(cached_state);
            }

            // Reuse existing embedding index.
            if let Ok(idx) = EmbeddingIndex::new(&self.repo_root).await
                && let Ok(true) = idx.has_table().await {
                    idx.ensure_fts_index().await;
                    self.embed_index.store(Arc::new(Some(idx)));
                }

            // Check if LSP enrichment has been durably persisted via the completion
            // sentinel. This replaces the `has_call_edges` heuristic, which fails
            // when LSP ran but the subsequent LanceDB persist crashed: edges end up
            // in memory but the sentinel is never written, so the next restart
            // correctly re-runs LSP enrichment (#477).
            let lsp_sentinel = super::sentinel::read_lsp_sentinel(&self.repo_root);

            let lsp_edge_count = if lsp_sentinel.is_some() {
                let call_count = {
                    let guard = self.graph.read().await;
                    guard.as_ref().unwrap().edges.iter()
                        .filter(|e| matches!(e.kind, crate::graph::EdgeKind::Calls))
                        .count()
                };
                self.lsp_status.set_complete(call_count);
                on_progress(&format!("LSP: {} cached call edges (sentinel present)", call_count));
                call_count
            } else {
                // LSP sentinel absent -- enrichment has not been durably persisted.
                // Run it synchronously on all nodes.
                on_progress("LSP: no sentinel -- running full enrichment...");
                self.run_foreground_lsp_and_persist(&on_progress).await?
            };

            scanner.commit_state()?;

            // Read final counts (may have changed after LSP enrichment).
            let (total_node_count, total_edge_count) = {
                let guard = self.graph.read().await;
                let gs = guard.as_ref().unwrap();
                (gs.nodes.len(), gs.edges.len())
            };

            let total_time = pipeline_start.elapsed();
            on_progress(&format!("Graph: {} nodes, {} edges", total_node_count, total_edge_count));
            on_progress(&format!("Done in {:.1}s (incremental, no changes)", total_time.as_secs_f64()));

            return Ok(PipelineResult {
                node_count: total_node_count,
                edge_count: total_edge_count,
                lsp_edge_count,
                embed_count: 0,
                total_time,
            });
        }

        // Phase 2: Incremental extract -- only changed files.
        let t1 = std::time::Instant::now();

        // Track changed file paths for LSP scoping.
        let changed_file_set: std::collections::HashSet<PathBuf> = scan.changed_files.iter()
            .chain(scan.new_files.iter())
            .cloned()
            .collect();

        // Rebuild the index before update_graph_with_scan (it expects a valid index).
        cached_state.index = crate::graph::index::GraphIndex::new();
        cached_state.index.rebuild_from_edges(&cached_state.edges);
        for node in &cached_state.nodes {
            cached_state.index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        // Store the cached graph so update_graph_with_scan can modify it in-place.
        {
            let mut guard = self.graph.write().await;
            *guard = Some(cached_state);
        }

        // Run incremental update (extract only changed files, remove deleted, re-embed).
        // LSP spawning is disabled (spawn_lsp=false) -- we handle it synchronously below.
        {
            let mut guard = self.graph.write().await;
            let gs = guard.as_mut().unwrap();
            self.update_graph_with_scan(gs, Some(scan), false).await?;
        }

        let extract_time = t1.elapsed();

        let (node_count, file_count) = {
            let guard = self.graph.read().await;
            let gs = guard.as_ref().unwrap();
            let files: std::collections::HashSet<_> = gs.nodes.iter()
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect();
            (gs.nodes.len(), files.len())
        };

        on_progress(&format!(
            "Incremental extract: {} symbols across {} files in {:.1}s (only {} files re-extracted)",
            node_count,
            file_count,
            extract_time.as_secs_f64(),
            changed_file_set.len(),
        ));

        // Phase 3: LSP enrichment on changed nodes only (synchronous).
        let changed_nodes: Vec<Node> = {
            let guard = self.graph.read().await;
            let gs = guard.as_ref().unwrap();
            gs.nodes.iter()
                .filter(|n| changed_file_set.contains(&n.id.file))
                .cloned()
                .collect()
        };

        let languages: Vec<String> = changed_nodes.iter()
            .map(|n| n.language.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let server_name = self.lsp_status.server_name();
        if let Some(ref name) = server_name {
            on_progress(&format!("LSP: {} found on PATH", name));
        }
        self.lsp_status.set_running();

        on_progress(&format!(
            "LSP: enriching {} changed nodes ({} languages: [{}])...",
            changed_nodes.len(),
            languages.len(),
            languages.join(", "),
        ));

        let t2 = std::time::Instant::now();
        let enricher_registry = EnricherRegistry::with_builtins();
        let supported = enricher_registry.supported_languages();
        let has_supported_language = languages.iter().any(|l| supported.contains(l));
        let graph_index = {
            let guard = self.graph.read().await;
            guard.as_ref().unwrap().index.clone()
        };
        let enrichment = enricher_registry
            .enrich_all(&changed_nodes, &graph_index, &languages, &self.repo_root, &self.lsp_only_roots)
            .await;
        let lsp_time = t2.elapsed();

        let lsp_edge_count;
        if !enrichment.any_enricher_ran {
            if has_supported_language {
                on_progress(&format!("LSP: no server available ({:.1}s)", lsp_time.as_secs_f64()));
                self.lsp_status.set_unavailable();
            } else {
                on_progress(&format!("LSP: no supported enrichers for changed files ({:.1}s)", lsp_time.as_secs_f64()));
                self.lsp_status.set_complete(0);
            }
            lsp_edge_count = 0;
        } else {
            lsp_edge_count = enrichment.added_edges.len();
            on_progress(&format!(
                "LSP: enriched {} call edges for changed files in {:.1}s",
                lsp_edge_count,
                lsp_time.as_secs_f64(),
            ));

            // Apply enrichment to in-memory graph.
            let mut guard = self.graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

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

                // Build index for O(1) lookup instead of O(N) find per patch.
                let node_pos: std::collections::HashMap<String, usize> = gs.nodes
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (n.stable_id(), i))
                    .collect();
                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(&idx) = node_pos.get(node_id) {
                        let node = &mut gs.nodes[idx];
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            self.lsp_status.set_complete(lsp_edge_count);
        }

        // Phase 4: Full persist with LSP edges included.
        {
            let snapshot = {
                let g = self.graph.read().await;
                g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
            };
            if let Some((nodes, edges)) = snapshot {
                tracing::info!(
                    "Foreground incremental persist: {} nodes, {} edges (including {} LSP)",
                    nodes.len(),
                    edges.len(),
                    lsp_edge_count,
                );
                if let Err(e) = persist_graph_to_lance(&self.repo_root, &nodes, &edges).await {
                    tracing::error!("Foreground incremental persist failed: {}", e);
                    return Err(e.context("Full persist failed during incremental foreground pipeline"));
                }
                // Persist succeeded -- write both sentinels. Incremental path rewrites
                // the full graph, so extraction is also complete after this persist.
                super::sentinel::write_extract_sentinel(&self.repo_root, nodes.len(), edges.len());
                super::sentinel::write_lsp_sentinel(&self.repo_root, nodes.len(), edges.len());
            }
        }

        // Commit scanner state after successful persist.
        scanner.commit_state()?;

        // Phase 5: Summary.
        let (total_node_count, total_edge_count) = {
            let guard = self.graph.read().await;
            let gs = guard.as_ref().unwrap();
            (gs.nodes.len(), gs.edges.len())
        };

        let total_time = pipeline_start.elapsed();
        on_progress(&format!("Graph: {} nodes, {} edges", total_node_count, total_edge_count));
        on_progress(&format!("Done in {:.1}s (incremental)", total_time.as_secs_f64()));

        Ok(PipelineResult {
            node_count: total_node_count,
            edge_count: total_edge_count,
            lsp_edge_count,
            embed_count: 0, // re-embed handled by update_graph_with_scan
            total_time,
        })
    }

    /// Full rebuild foreground pipeline (no cache available).
    async fn run_pipeline_foreground_full<F>(
        &self,
        on_progress: F,
        pipeline_start: std::time::Instant,
    ) -> anyhow::Result<PipelineResult>
    where
        F: Fn(&str) + Send + Sync,
    {
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
                detected_frameworks: graph_state.detected_frameworks.clone(),
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
                .enrich_all(&graph_state.nodes, &graph_state.index, &languages, &self.repo_root, &self.lsp_only_roots)
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

            // Apply enrichment to in-memory graph
            let mut guard = self.graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

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

                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(node) = gs.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }

                drop(guard);
            }
            self.lsp_status.set_complete(lsp_edge_count);
        }

        // Phase 3: Full persist — write the complete graph (tree-sitter + LSP edges)
        // to LanceDB. build_full_graph_inner(false) deferred persistence so we can
        // include LSP edges in a single atomic write (#311).
        {
            let snapshot = {
                let g = self.graph.read().await;
                g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
            };
            if let Some((nodes, edges)) = snapshot {
                tracing::info!(
                    "Foreground full persist: {} nodes, {} edges (including {} LSP)",
                    nodes.len(),
                    edges.len(),
                    lsp_edge_count,
                );
                if let Err(e) = persist_graph_to_lance(&self.repo_root, &nodes, &edges).await {
                    tracing::error!("Foreground full persist failed: {}", e);
                    return Err(e.context("Full persist failed during foreground pipeline"));
                }
                // Full persist succeeded (tree-sitter + LSP in one write) -- write
                // both sentinels since the single persist covers both phases (#477).
                super::sentinel::write_extract_sentinel(&self.repo_root, nodes.len(), edges.len());
                super::sentinel::write_lsp_sentinel(&self.repo_root, nodes.len(), edges.len());
            }
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

    /// Run LSP enrichment on the full graph synchronously and persist.
    /// Used when the cached graph has no call edges and needs enrichment.
    /// Returns the number of LSP edges added.
    async fn run_foreground_lsp_and_persist<F>(
        &self,
        on_progress: &F,
    ) -> anyhow::Result<usize>
    where
        F: Fn(&str) + Send + Sync,
    {
        let (all_nodes, graph_index, languages) = {
            let guard = self.graph.read().await;
            let gs = guard.as_ref().unwrap();
            let langs: Vec<String> = gs.nodes.iter()
                .map(|n| n.language.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            (gs.nodes.clone(), gs.index.clone(), langs)
        };

        let server_name = self.lsp_status.server_name();
        if let Some(ref name) = server_name {
            on_progress(&format!("LSP: {} found on PATH", name));
        }
        self.lsp_status.set_running();

        let enricher_registry = EnricherRegistry::with_builtins();
        let enrichment = enricher_registry
            .enrich_all(&all_nodes, &graph_index, &languages, &self.repo_root, &self.lsp_only_roots)
            .await;

        if !enrichment.any_enricher_ran {
            on_progress("LSP: no server available");
            self.lsp_status.set_unavailable();
            return Ok(0);
        }

        let lsp_edge_count = enrichment.added_edges.len();
        on_progress(&format!("LSP: enriched {} call edges", lsp_edge_count));

        // Apply enrichment to in-memory graph.
        {
            let mut guard = self.graph.write().await;
            if let Some(ref mut gs) = *guard {
                for vnode in &enrichment.new_nodes {
                    gs.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                gs.nodes.extend(enrichment.new_nodes);

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

                // Build index for O(1) lookup instead of O(N) find per patch.
                let node_pos: std::collections::HashMap<String, usize> = gs.nodes
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (n.stable_id(), i))
                    .collect();
                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(&idx) = node_pos.get(node_id) {
                        let node = &mut gs.nodes[idx];
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }
        self.lsp_status.set_complete(lsp_edge_count);

        // Persist with LSP edges.
        let snapshot = {
            let g = self.graph.read().await;
            g.as_ref().map(|gs| (gs.nodes.clone(), gs.edges.clone()))
        };
        if let Some((nodes, edges)) = snapshot {
            if let Err(e) = persist_graph_to_lance(&self.repo_root, &nodes, &edges).await {
                tracing::error!("Foreground LSP persist failed: {}", e);
                return Err(e.context("LSP persist failed during foreground pipeline"));
            }
            // Persist succeeded -- write LSP sentinel so future startups know
            // LSP enrichment is durable and can skip re-enrichment (#477).
            super::sentinel::write_lsp_sentinel(&self.repo_root, nodes.len(), edges.len());
        }

        Ok(lsp_edge_count)
    }
}
