//! Background enrichment: LSP enrichment, embedding pipeline, and background scanner.
//!
//! ## Module structure
//!
//! The background scanner stages are extracted into `bg_scanner`:
//! - `scan_roots()` -- resolve workspace roots, detect file changes
//! - `update_graph()` -- apply changes, run enrichment pipeline
//! - `persist_deltas()` -- write to LanceDB, commit scanner state
// EXTRACTION_VERSION is deprecated (#526) but still used for backward-compat migration.
#![allow(deprecated)]

use std::path::PathBuf;
use std::sync::Arc;

use crate::embed::EmbeddingIndex;
use crate::graph::{Edge, Node};
use crate::roots::{RootConfig, WorkspaceConfig};
use crate::scanner::Scanner;

use super::state::GraphState;
use super::store::{check_and_migrate_extraction_version, persist_graph_to_lance};
use super::{PipelineResult, RnaHandler};

/// Check if a cached graph is missing enrichment passes output that should exist.
///
/// Returns `true` if the cache appears stale: it has Import nodes (so framework
/// detection should produce results) but zero `NodeKind::Other("framework")` nodes.
/// This catches caches built before the enrichment pipeline was wired via the event
/// bus (pre-v2-rc) or where a persist race dropped framework nodes.
///
/// When this returns `true`, the caller should re-run the enrichment pipeline
/// instead of serving the stale cache.
pub(crate) fn cache_needs_enrichment(nodes: &[Node]) -> bool {
    let has_imports = nodes
        .iter()
        .any(|n| n.id.kind == crate::graph::NodeKind::Import);
    if !has_imports {
        return false; // No imports => no framework detection possible, cache is fine.
    }
    let has_framework_nodes = nodes
        .iter()
        .any(|n| matches!(&n.id.kind, crate::graph::NodeKind::Other(s) if s == "framework"));
    // If there are imports but no framework nodes, AND the imports match at least one
    // framework rule, the cache is missing enrichment output.
    if has_framework_nodes {
        return false;
    }
    // Quick check: do any imports match known framework patterns?
    let result = crate::extract::framework_detection::framework_detection_pass(nodes, "check");
    !result.detected_frameworks.is_empty()
}

impl RnaHandler {
    /// Spawn the background scanner task (event-driven + 15min heartbeat, worktree-aware).
    ///
    /// The loop calls three extracted stage functions per tick:
    /// 1. `bg_scanner::scan_roots()` -- resolve workspace roots, detect file changes
    /// 2. `bg_scanner::update_graph()` -- apply changes, run enrichment pipeline
    /// 3. `bg_scanner::persist_deltas()` -- write to LanceDB, commit scanner state
    pub(crate) fn spawn_background_scanner(&self) {
        let graph = Arc::clone(&self.graph);
        let repo_root = self.repo_root.clone();
        let lance_write_lock = Arc::clone(&self.lance_write_lock);
        let scan_stats = Arc::clone(&self.scan_stats);
        tokio::spawn(async move {
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
                            let changed = last_fetch_head_mtime.is_some_and(|prev| prev != mtime);
                            last_fetch_head_mtime = Some(mtime);
                            changed
                        }
                        Err(_) => false,
                    }
                };

                if head_changed {
                    tracing::info!("HEAD changed -- triggering immediate background scan");
                } else if fetch_head_changed {
                    tracing::info!("FETCH_HEAD changed -- triggering immediate background scan");
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_secs(900)).await;
                }

                // Stage 1: scan roots for file changes.
                let mut scan_result = super::bg_scanner::scan_roots(&repo_root, &prev_root_slugs);

                if !scan_result.has_changes && scan_result.removed_slugs.is_empty() {
                    prev_root_slugs = scan_result.current_root_slugs;
                    continue;
                }

                // Stage 2: update graph (lock-free via ArcSwap).
                let current_snap = graph.load_full();
                if let Some(ref current_gs) = *current_snap {
                    let mut graph_state = (**current_gs).clone();

                    let lance_deltas = super::bg_scanner::update_graph(
                        &mut graph_state,
                        &mut scan_result,
                        &repo_root,
                        &scan_stats,
                    )
                    .await;

                    // Atomic swap: publish the new graph state.
                    graph.store(Arc::new(Some(Arc::new(graph_state))));

                    // Stage 3: persist deltas to LanceDB.
                    super::bg_scanner::persist_deltas(
                        lance_deltas,
                        &scan_result.per_root_scans,
                        &scan_result.removed_slugs,
                        &repo_root,
                        &graph,
                        &lance_write_lock,
                    )
                    .await;
                }

                prev_root_slugs = scan_result.current_root_slugs;
            }
        });
        tracing::info!(
            "Background scanner started (event-driven + 15min heartbeat, worktree-aware)"
        );
    }

    /// Spawn background LSP enrichment after the initial graph build returns (#574).
    ///
    /// The initial `build_full_graph_inner` runs with `skip_lsp=true` so it returns
    /// in seconds (tree-sitter + non-LSP passes only). This method spawns LSP enrichment
    /// in the background: when complete, it ArcSwaps the fully enriched graph and
    /// re-persists to LanceDB with LSP edges.
    ///
    /// This restores the v0.1.14 behavior where `build_full_graph_inner` returned
    /// immediately and LSP ran via `spawn_background_enrichment`.
    pub(crate) fn spawn_background_lsp_enrichment(
        &self,
        nodes: Vec<crate::graph::Node>,
        edges: Vec<crate::graph::Edge>,
        dirty_slugs: std::collections::HashSet<String>,
        detected_frameworks: std::collections::HashSet<String>,
    ) {
        let repo_root = self.repo_root.clone();
        let graph_arc = Arc::clone(&self.graph);
        let lsp_status = Arc::clone(&self.lsp_status);
        let scan_stats = Arc::clone(&self.scan_stats);
        let lance_write_lock = Arc::clone(&self.lance_write_lock);

        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            tracing::info!(
                "[background-lsp] Starting LSP enrichment: {} nodes, {} edges",
                nodes.len(),
                edges.len()
            );

            // Build root pairs for the enrichment pipeline
            let workspace = crate::roots::WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_declared_roots(&repo_root);
            let root_pairs: Vec<(String, std::path::PathBuf)> = workspace
                .resolved_roots()
                .iter()
                .map(|r| (r.slug.clone(), r.path.clone()))
                .collect();
            let primary_slug = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

            // Run enrichment pipeline WITH LSP (skip_lsp=false)
            let result = crate::extract::consumers::emit_enrichment_pipeline(
                nodes,
                edges,
                root_pairs,
                primary_slug.clone(),
                repo_root.clone(),
                crate::extract::consumers::BusOptions {
                    scan_stats: Some(Arc::clone(&scan_stats)),
                    embed_idx: None,
                    lance_repo_root: None,
                    skip_lsp: false, // this time LSP runs
                },
                Some(dirty_slugs),
            )
            .await;

            match result {
                Ok((mut enriched_nodes, mut enriched_edges, enriched_frameworks)) => {
                    // Update LSP status
                    let lsp_edge_count = enriched_edges
                        .iter()
                        .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                        .count();
                    let lsp_call_edge_count = enriched_edges
                        .iter()
                        .filter(|e| {
                            e.source == crate::graph::ExtractionSource::Lsp
                                && matches!(e.kind, crate::graph::EdgeKind::Calls)
                        })
                        .count();
                    if lsp_edge_count > 0 {
                        lsp_status.set_complete(lsp_call_edge_count);
                        tracing::info!(
                            "[background-lsp] LSP enrichment complete: {} LSP call edges, {} total LSP edges in {:.2}s",
                            lsp_call_edge_count,
                            lsp_edge_count,
                            t0.elapsed().as_secs_f64()
                        );
                    } else {
                        lsp_status.set_unavailable();
                        tracing::info!(
                            "[background-lsp] LSP enrichment produced no edges in {:.2}s",
                            t0.elapsed().as_secs_f64()
                        );
                    }

                    // Dedup
                    {
                        let mut seen_nodes = std::collections::HashSet::new();
                        enriched_nodes.reverse();
                        enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                        enriched_nodes.reverse();
                        let mut seen_edges = std::collections::HashSet::new();
                        enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                    }

                    // Rebuild petgraph index
                    let mut index = crate::graph::index::GraphIndex::new();
                    index.rebuild_from_edges(&enriched_edges);
                    for node in &enriched_nodes {
                        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                    }

                    // Recompute PageRank with LSP edges
                    let pagerank_scores = index.compute_pagerank(0.85, 20);
                    for node in &mut enriched_nodes {
                        if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                            node.metadata
                                .insert("importance".to_string(), format!("{:.6}", score));
                        }
                    }

                    // Re-run subsystem detection with updated PageRank
                    {
                        let node_file_map: std::collections::HashMap<String, String> =
                            enriched_nodes
                                .iter()
                                .filter(|n| n.id.root != "external")
                                .map(|n| (n.stable_id(), n.id.file.display().to_string()))
                                .collect();
                        let mut subsystems =
                            index.detect_communities(&pagerank_scores, &node_file_map);
                        // Dedup subsystem names
                        {
                            let mut name_counts: std::collections::HashMap<String, usize> =
                                std::collections::HashMap::new();
                            for s in &subsystems {
                                *name_counts.entry(s.name.clone()).or_default() += 1;
                            }
                            for s in &mut subsystems {
                                if name_counts.get(&s.name).copied().unwrap_or(0) > 1
                                    && let Some(iface) = s.interfaces.first()
                                {
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
                        // Remove stale virtual nodes
                        enriched_nodes.retain(|n| !matches!(&n.id.kind, crate::graph::NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")));
                        enriched_edges.retain(|e| {
                            !matches!(&e.to.kind, crate::graph::NodeKind::Other(s) if s == "subsystem")
                                && e.kind != crate::graph::EdgeKind::UsesFramework
                                && e.kind != crate::graph::EdgeKind::Produces
                                && e.kind != crate::graph::EdgeKind::Consumes
                        });
                        for node in &mut enriched_nodes {
                            if let Some(subsystem_name) = node_subsystem.get(&node.stable_id()) {
                                node.metadata.insert(
                                    super::graph::SUBSYSTEM_KEY.to_owned(),
                                    subsystem_name.clone(),
                                );
                            } else {
                                node.metadata.remove(super::graph::SUBSYSTEM_KEY);
                            }
                        }
                        // Emit subsystem virtual nodes
                        let (sub_added_nodes, sub_added_edges) =
                            crate::extract::consumers::emit_community_detection(
                                primary_slug,
                                subsystems,
                                enriched_nodes.clone(),
                            )
                            .await
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    "[background-lsp] Subsystem promotion failed: {}",
                                    e
                                );
                                (vec![], vec![])
                            });
                        enriched_nodes.extend(sub_added_nodes);
                        enriched_edges.extend(sub_added_edges);

                        // Re-add virtual nodes to index
                        for node in &enriched_nodes {
                            if matches!(&node.id.kind, crate::graph::NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event"))
                            {
                                index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                            }
                        }
                        for edge in &enriched_edges {
                            if matches!(&edge.to.kind, crate::graph::NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event"))
                            {
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

                    // Final dedup
                    {
                        let mut seen_edges = std::collections::HashSet::new();
                        enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                    }

                    // Persist to LanceDB with LSP edges
                    {
                        let _lance_guard = lance_write_lock.lock().await;
                        if let Err(e) = super::store::persist_graph_to_lance(
                            &repo_root,
                            &enriched_nodes,
                            &enriched_edges,
                        )
                        .await
                        {
                            tracing::error!("[background-lsp] LanceDB persist failed: {}", e);
                        } else {
                            super::sentinel::write_extract_sentinel(
                                &repo_root,
                                enriched_nodes.len(),
                                enriched_edges.len(),
                            );
                            let has_lsp_edges = enriched_edges
                                .iter()
                                .any(|e| e.source == crate::graph::ExtractionSource::Lsp);
                            if has_lsp_edges {
                                super::sentinel::write_lsp_sentinel(
                                    &repo_root,
                                    enriched_nodes.len(),
                                    enriched_edges.len(),
                                );
                            }
                            tracing::info!(
                                "[background-lsp] LanceDB re-persisted with LSP edges: {} nodes, {} edges",
                                enriched_nodes.len(),
                                enriched_edges.len()
                            );
                        }
                    }

                    // ArcSwap the fully enriched graph
                    let all_frameworks = detected_frameworks
                        .union(&enriched_frameworks)
                        .cloned()
                        .collect();
                    let new_state = super::state::GraphState::new(
                        enriched_nodes,
                        enriched_edges,
                        index,
                        Some(std::time::Instant::now()),
                        all_frameworks,
                    );
                    graph_arc.store(Arc::new(Some(Arc::new(new_state))));
                    tracing::info!(
                        "[background-lsp] Enriched graph swapped in after {:.2}s",
                        t0.elapsed().as_secs_f64()
                    );
                }
                Err(e) => {
                    tracing::error!("[background-lsp] LSP enrichment pipeline failed: {:#}", e);
                    lsp_status.set_unavailable();
                }
            }
        });
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
            let embeddable_nodes: Vec<Node> = bg_nodes
                .iter()
                .filter(|n| n.id.root != "external")
                .cloned()
                .collect();

            let embed_repo_root = bg_repo_root.clone();
            let embed_index_ref = bg_embed_index.clone();
            let embed_status = bg_embed_status;
            let embeddable_count = embeddable_nodes
                .iter()
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
                                idx.index_all_with_symbols(&embed_repo_root, &embeddable_nodes)
                                    .await
                            }
                            Err(e) => {
                                tracing::warn!("[background] Embedding table check failed: {}", e);
                                embed_status.set_failed(format!("{}", e));
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
                                embed_status.set_failed(format!("{}", e));
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[background] EmbeddingIndex init failed: {}", e);
                        embed_status.set_failed(format!("{}", e));
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
                    skip_lsp: false,
                },
                None,
            )
            .await;

            let (mut enriched_nodes, mut enriched_edges, _detected_frameworks) = match result {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("[cache-hit bus] emit_enrichment_pipeline failed: {:#}", e);
                    bg_lsp_status.set_failed(&format!("{}", e));
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
            let lsp_call_edge_count = enriched_edges
                .iter()
                .filter(|e| {
                    e.source == crate::graph::ExtractionSource::Lsp
                        && matches!(e.kind, crate::graph::EdgeKind::Calls)
                })
                .count();
            let lsp_edge_count = enriched_edges
                .iter()
                .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                .count();

            tracing::info!(
                "[cache-hit bus] LSP enrichment complete: {} LSP call edges, {} total LSP edges, {} total nodes",
                lsp_call_edge_count,
                lsp_edge_count,
                enriched_nodes.len()
            );

            // Build updated index from enriched edges.
            let mut new_index = crate::graph::index::GraphIndex::new();
            new_index.rebuild_from_edges(&enriched_edges);
            for node in &enriched_nodes {
                new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }

            // Atomic swap: replace the in-memory graph with the enriched version.
            // Tool calls reading the old snapshot are undisturbed; new calls see
            // the enriched version immediately.
            {
                let snap = bg_graph.load_full();
                if let Some(ref gs) = *snap {
                    let mut new_gs = (**gs).clone();
                    new_gs.nodes = enriched_nodes.clone();
                    new_gs.edges = enriched_edges.clone();
                    new_gs.index = new_index;
                    bg_graph.store(Arc::new(Some(Arc::new(new_gs))));
                }
            }

            // Persist enriched graph to LanceDB and write sentinel under the write lock
            // so no concurrent writer can interleave between persist and sentinel write.
            let persist_result = {
                let _lance_guard = bg_lance_write_lock.lock().await;
                let result =
                    persist_graph_to_lance(&bg_repo_root, &enriched_nodes, &enriched_edges).await;
                if result.is_ok() {
                    super::sentinel::write_lsp_sentinel(
                        &bg_repo_root,
                        enriched_nodes.len(),
                        enriched_edges.len(),
                    );
                }
                result
            };

            match persist_result {
                Ok(()) => {
                    tracing::info!(
                        "[cache-hit bus] LSP persist complete: {} nodes, {} edges",
                        enriched_nodes.len(),
                        enriched_edges.len()
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

    /// Build workspace `root_pairs` and `primary_slug` for `emit_enrichment_pipeline`.
    ///
    /// Factored out to avoid duplicating `WorkspaceConfig` boilerplate across
    /// every foreground path that now routes through the event bus (ADR-001, #583).
    fn build_bus_root_pairs(&self) -> (Vec<(String, PathBuf)>, String) {
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root)
            .with_agent_memories(&self.repo_root)
            .with_declared_roots(&self.repo_root);
        let root_pairs: Vec<(String, PathBuf)> = workspace
            .resolved_roots()
            .iter()
            .map(|r| (r.slug.clone(), r.path.clone()))
            .collect();
        let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();
        (root_pairs, primary_slug)
    }

    /// Run the full pipeline synchronously with progress reporting.
    ///
    /// This is the `--full` CLI path. When a cached graph exists in LanceDB,
    /// it uses the incremental path (only re-extract changed files, LSP on
    /// changed nodes only) for dramatically faster rescans. Falls back to
    /// full rebuild when no cache exists.
    ///
    /// The `on_progress` callback receives structured status messages.
    pub async fn run_pipeline_foreground<F>(&self, on_progress: F) -> anyhow::Result<PipelineResult>
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
                    tracing::debug!(
                        "Could not load cached graph, falling back to full rebuild: {}",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        if let Some(cached_state) = cached {
            return self
                .run_pipeline_foreground_incremental(cached_state, on_progress, pipeline_start)
                .await;
        }

        // No cache -- full rebuild path.
        self.run_pipeline_foreground_full(on_progress, pipeline_start)
            .await
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
            tracing::info!(
                "Schema migrated during incremental pre-flight -- falling back to full rebuild"
            );
            on_progress("Schema migration detected -- rebuilding from scratch.");
            // Clear sentinels -- they reference the old schema version and are now stale.
            super::sentinel::clear_sentinels(&self.repo_root);
            return self
                .run_pipeline_foreground_full(on_progress, pipeline_start)
                .await;
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
                return self
                    .run_pipeline_foreground_full(on_progress, pipeline_start)
                    .await;
            }
        }

        // Phase 1: Scan to detect changes.
        let t0 = std::time::Instant::now();
        let mut scanner = Scanner::new(self.repo_root.clone())?;
        let scan = scanner.scan()?;
        let scan_time = t0.elapsed();

        let change_count =
            scan.changed_files.len() + scan.new_files.len() + scan.deleted_files.len();

        on_progress(&format!(
            "Scan: {} changed, {} new, {} deleted in {:.1}s",
            scan.changed_files.len(),
            scan.new_files.len(),
            scan.deleted_files.len(),
            scan_time.as_secs_f64(),
        ));

        if change_count == 0 {
            // FIX(#601): check if the cached graph is missing enrichment output
            // (e.g., framework nodes). Caches built before the event bus was wired
            // (pre-v2-rc) or by a binary that skipped post-extraction passes may lack
            // framework nodes even though Import nodes are present. When stale, clear
            // sentinels and re-run the full enrichment pipeline.
            let stale_enrichment = cache_needs_enrichment(&cached_state.nodes);
            if stale_enrichment {
                on_progress(
                    "No file changes but cached graph missing enrichment output -- re-enriching...",
                );
                super::sentinel::clear_sentinels(&self.repo_root);
            } else {
                on_progress("No changes detected -- reusing cached graph.");
            }

            // Store graph atomically and set up embedding index.
            self.graph.store(Arc::new(Some(Arc::new(cached_state))));

            // Reuse existing embedding index.
            if let Ok(idx) = EmbeddingIndex::new(&self.repo_root).await
                && let Ok(true) = idx.has_table().await
            {
                idx.ensure_fts_index().await;
                self.embed_index.store(Arc::new(Some(idx)));
            }

            // Check if LSP enrichment has been durably persisted via the completion
            // sentinel. This replaces the `has_call_edges` heuristic, which fails
            // when LSP ran but the subsequent LanceDB persist crashed: edges end up
            // in memory but the sentinel is never written, so the next restart
            // correctly re-runs LSP enrichment (#477).
            let lsp_sentinel = super::sentinel::read_lsp_sentinel(&self.repo_root);

            let lsp_edge_count = if lsp_sentinel.is_some() && !stale_enrichment {
                let call_count = {
                    let snap = self.graph.load_full();
                    snap.as_ref()
                        .as_ref()
                        .unwrap()
                        .edges
                        .iter()
                        .filter(|e| matches!(e.kind, crate::graph::EdgeKind::Calls))
                        .count()
                };
                self.lsp_status.set_complete(call_count);
                on_progress(&format!(
                    "LSP: {} cached call edges (sentinel present)",
                    call_count
                ));
                call_count
            } else {
                // LSP sentinel absent or stale enrichment -- run full enrichment.
                on_progress("LSP: running full enrichment...");
                self.run_foreground_lsp_and_persist(&on_progress).await?
            };

            scanner.commit_state()?;

            // Read final counts (may have changed after LSP enrichment).
            let (total_node_count, total_edge_count) = {
                let snap = self.graph.load_full();
                let gs = snap.as_ref().as_ref().unwrap();
                (gs.nodes.len(), gs.edges.len())
            };

            let total_time = pipeline_start.elapsed();
            on_progress(&format!(
                "Graph: {} nodes, {} edges",
                total_node_count, total_edge_count
            ));
            on_progress(&format!(
                "Done in {:.1}s (incremental, no changes)",
                total_time.as_secs_f64()
            ));

            return Ok(PipelineResult {
                node_count: total_node_count,
                edge_count: total_edge_count,
                file_count: 0,
                lsp_edge_count,
                embed_count: 0,
                total_time,
                lsp_entries: vec![],
                encoding_stats: crate::extract::EncodingStats::default(),
            });
        }

        // Phase 2: Incremental extract -- only changed files.
        let t1 = std::time::Instant::now();

        // Track changed file paths for LSP scoping.
        let changed_file_set: std::collections::HashSet<PathBuf> = scan
            .changed_files
            .iter()
            .chain(scan.new_files.iter())
            .cloned()
            .collect();

        // Rebuild the index before update_graph_with_scan (it expects a valid index).
        cached_state.index = crate::graph::index::GraphIndex::new();
        cached_state.index.rebuild_from_edges(&cached_state.edges);
        for node in &cached_state.nodes {
            cached_state
                .index
                .ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        // Run incremental update on the local cached_state, then store atomically.
        // LSP spawning is disabled (spawn_lsp=false) -- we handle it synchronously below.
        self.update_graph_with_scan(&mut cached_state, Some(scan), false)
            .await?;
        self.graph.store(Arc::new(Some(Arc::new(cached_state))));

        let extract_time = t1.elapsed();

        let (node_count, file_count) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            let files: std::collections::HashSet<_> = gs
                .nodes
                .iter()
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

        // Phase 3: Enrichment via event bus (ADR-001, #583).
        // Route through emit_enrichment_pipeline instead of calling
        // EnricherRegistry::enrich_all() directly. Pass the full graph;
        // dirty_slugs scopes LSP to only the primary root (changed files).
        let (all_nodes, all_edges) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            (gs.nodes.clone(), gs.edges.clone())
        };

        let server_name = self.lsp_status.server_name();
        if let Some(ref name) = server_name {
            on_progress(&format!("LSP: {} found on PATH", name));
        }
        self.lsp_status.set_running();

        on_progress(&format!(
            "Enrichment: running pipeline via event bus ({} changed files)...",
            changed_file_set.len(),
        ));

        let (root_pairs, primary_slug) = self.build_bus_root_pairs();
        // Only the primary root has changes in the incremental path.
        let dirty_slugs: Option<std::collections::HashSet<String>> =
            Some(std::iter::once(primary_slug.clone()).collect());

        let t2 = std::time::Instant::now();
        let bus_result = crate::extract::consumers::emit_enrichment_pipeline(
            all_nodes,
            all_edges,
            root_pairs,
            primary_slug,
            self.repo_root.clone(),
            crate::extract::consumers::BusOptions {
                scan_stats: Some(Arc::clone(&self.scan_stats)),
                embed_idx: None,
                lance_repo_root: None,
                skip_lsp: false,
            },
            dirty_slugs,
        )
        .await;
        let bus_time = t2.elapsed();

        let lsp_edge_count;
        match bus_result {
            Ok((mut enriched_nodes, mut enriched_edges, detected_frameworks)) => {
                // Dedup: passes can re-emit cached entries.
                {
                    let mut seen_nodes = std::collections::HashSet::new();
                    enriched_nodes.reverse();
                    enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                    enriched_nodes.reverse();

                    let mut seen_edges = std::collections::HashSet::new();
                    enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                }

                lsp_edge_count = enriched_edges
                    .iter()
                    .filter(|e| {
                        e.source == crate::graph::ExtractionSource::Lsp
                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                    })
                    .count();

                on_progress(&format!(
                    "Enrichment: {} LSP call edges via bus in {:.1}s",
                    lsp_edge_count,
                    bus_time.as_secs_f64(),
                ));

                // Build updated index and apply enriched graph via atomic swap.
                let mut new_index = crate::graph::index::GraphIndex::new();
                new_index.rebuild_from_edges(&enriched_edges);
                for node in &enriched_nodes {
                    new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                {
                    let snap = self.graph.load_full();
                    if let Some(ref current_gs) = *snap {
                        let mut gs = (**current_gs).clone();
                        gs.nodes = enriched_nodes;
                        gs.edges = enriched_edges;
                        gs.index = new_index;
                        gs.detected_frameworks = detected_frameworks;
                        self.graph.store(Arc::new(Some(Arc::new(gs))));
                    }
                }
                self.lsp_status.set_complete(lsp_edge_count);
            }
            Err(e) => {
                tracing::error!(
                    "Foreground incremental pipeline: emit_enrichment_pipeline failed: {:#}",
                    e
                );
                on_progress(&format!(
                    "Enrichment: pipeline failed in {:.1}s -- graph has tree-sitter data only",
                    bus_time.as_secs_f64(),
                ));
                self.lsp_status.set_unavailable();
                lsp_edge_count = 0;
            }
        }

        // Phase 4: Full persist with LSP edges included.
        {
            let snapshot = {
                let snap = self.graph.load_full();
                snap.as_ref()
                    .as_ref()
                    .map(|gs| (gs.nodes.clone(), gs.edges.clone()))
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
                    return Err(
                        e.context("Full persist failed during incremental foreground pipeline")
                    );
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
        let (total_node_count, total_edge_count, file_count) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            let fc = gs
                .nodes
                .iter()
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect::<std::collections::HashSet<_>>()
                .len();
            (gs.nodes.len(), gs.edges.len(), fc)
        };
        let encoding_stats = {
            let ss = self.scan_stats.read().unwrap_or_else(|e| e.into_inner());
            let mut agg = crate::extract::EncodingStats::default();
            for es in ss.encoding_stats.values() {
                agg.merge(es);
            }
            agg
        };
        let total_time = pipeline_start.elapsed();
        let result = PipelineResult {
            node_count: total_node_count,
            edge_count: total_edge_count,
            file_count,
            lsp_edge_count,
            embed_count: 0,
            total_time,
            lsp_entries: vec![],
            encoding_stats,
        };
        on_progress(&result.format_summary());
        Ok(result)
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

        let file_count = graph_state
            .nodes
            .iter()
            .map(|n| n.id.file.to_string_lossy().to_string())
            .collect::<std::collections::HashSet<_>>()
            .len();

        on_progress(&format!(
            "Scan+Extract: {} symbols across {} files in {:.1}s",
            graph_state.nodes.len(),
            file_count,
            scan_extract_time.as_secs_f64(),
        ));

        // Store graph state atomically so it is available for queries during embed+LSP.
        {
            let mut idx = crate::graph::index::GraphIndex::new();
            idx.rebuild_from_edges(&graph_state.edges);
            for node in &graph_state.nodes {
                idx.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }
            self.graph.store(Arc::new(Some(Arc::new(GraphState::new(
                graph_state.nodes.clone(),
                graph_state.edges.clone(),
                idx,
                graph_state.last_scan_completed_at,
                graph_state.detected_frameworks.clone(),
            )))));
        }

        // Phase 2: Embed + LSP enrichment (parallel -- they use independent data stores)
        let embeddable_nodes: Vec<Node> = graph_state
            .nodes
            .iter()
            .filter(|n| n.id.root != "external")
            .cloned()
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
                    match idx
                        .index_all_with_symbols(&embed_repo_root, &embeddable_nodes)
                        .await
                    {
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

        on_progress("Enrichment: running pipeline via event bus...");

        // ADR-001 (#583): route through emit_enrichment_pipeline instead of
        // calling EnricherRegistry::enrich_all() directly. The bus runs all
        // post-extraction passes (LSP, subsystem detection, framework detection,
        // import calls, tested_by, etc.) and returns the fully enriched graph.
        let (root_pairs, primary_slug) = self.build_bus_root_pairs();
        let bus_fut = async {
            let t2 = std::time::Instant::now();
            let result = crate::extract::consumers::emit_enrichment_pipeline(
                graph_state.nodes.clone(),
                graph_state.edges.clone(),
                root_pairs,
                primary_slug,
                self.repo_root.clone(),
                crate::extract::consumers::BusOptions {
                    scan_stats: Some(Arc::clone(&self.scan_stats)),
                    embed_idx: None,
                    lance_repo_root: None,
                    skip_lsp: false,
                },
                None, // full rebuild: all roots dirty
            )
            .await;
            let elapsed = t2.elapsed();
            (result, elapsed)
        };

        let ((embed_count, embed_time), (bus_result, bus_time)) = tokio::join!(embed_fut, bus_fut);

        on_progress(&format!(
            "Embed: {} items in {:.1}s",
            embed_count,
            embed_time.as_secs_f64(),
        ));

        let lsp_edge_count;

        match bus_result {
            Ok((mut enriched_nodes, mut enriched_edges, detected_frameworks)) => {
                // Dedup: passes can re-emit cached entries.
                {
                    let mut seen_nodes = std::collections::HashSet::new();
                    enriched_nodes.reverse();
                    enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                    enriched_nodes.reverse();

                    let mut seen_edges = std::collections::HashSet::new();
                    enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                }

                lsp_edge_count = enriched_edges
                    .iter()
                    .filter(|e| {
                        e.source == crate::graph::ExtractionSource::Lsp
                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                    })
                    .count();

                on_progress(&format!(
                    "Enrichment: {} LSP call edges via bus in {:.1}s",
                    lsp_edge_count,
                    bus_time.as_secs_f64(),
                ));

                // Build updated index and apply enriched graph via atomic swap.
                let mut new_index = crate::graph::index::GraphIndex::new();
                new_index.rebuild_from_edges(&enriched_edges);
                for node in &enriched_nodes {
                    new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                {
                    let snap = self.graph.load_full();
                    if let Some(ref current_gs) = *snap {
                        let mut gs = (**current_gs).clone();
                        gs.nodes = enriched_nodes;
                        gs.edges = enriched_edges;
                        gs.index = new_index;
                        gs.detected_frameworks = detected_frameworks;
                        self.graph.store(Arc::new(Some(Arc::new(gs))));
                    }
                }
                self.lsp_status.set_complete(lsp_edge_count);
            }
            Err(e) => {
                tracing::error!(
                    "Foreground full pipeline: emit_enrichment_pipeline failed: {:#}",
                    e
                );
                on_progress(&format!(
                    "Enrichment: pipeline failed in {:.1}s -- graph has tree-sitter data only",
                    bus_time.as_secs_f64(),
                ));
                self.lsp_status.set_unavailable();
                lsp_edge_count = 0;
            }
        }

        // Phase 3: Full persist — write the complete graph (tree-sitter + LSP edges)
        // to LanceDB. build_full_graph_inner(false) deferred persistence so we can
        // include LSP edges in a single atomic write (#311).
        {
            let snapshot = {
                let snap = self.graph.load_full();
                snap.as_ref()
                    .as_ref()
                    .map(|gs| (gs.nodes.clone(), gs.edges.clone()))
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
        let (total_node_count, total_edge_count) = {
            let snap = self.graph.load_full();
            match snap.as_ref().as_ref() {
                Some(gs) => (gs.nodes.len(), gs.edges.len()),
                None => (graph_state.nodes.len(), graph_state.edges.len()),
            }
        };
        let encoding_stats = {
            let ss = self.scan_stats.read().unwrap_or_else(|e| e.into_inner());
            let mut agg = crate::extract::EncodingStats::default();
            for es in ss.encoding_stats.values() {
                agg.merge(es);
            }
            agg
        };
        let total_time = pipeline_start.elapsed();
        let result = PipelineResult {
            node_count: total_node_count,
            edge_count: total_edge_count,
            file_count,
            lsp_edge_count,
            embed_count,
            total_time,
            lsp_entries: vec![],
            encoding_stats,
        };
        on_progress(&result.format_summary());
        Ok(result)
    }

    /// Run enrichment pipeline on the full graph synchronously and persist.
    /// Used when the cached graph has no LSP sentinel and needs enrichment.
    /// Routes through `emit_enrichment_pipeline` per ADR-001 (#583).
    /// Returns the number of LSP call edges added.
    async fn run_foreground_lsp_and_persist<F>(&self, on_progress: &F) -> anyhow::Result<usize>
    where
        F: Fn(&str) + Send + Sync,
    {
        let (all_nodes, all_edges) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            (gs.nodes.clone(), gs.edges.clone())
        };

        let server_name = self.lsp_status.server_name();
        if let Some(ref name) = server_name {
            on_progress(&format!("LSP: {} found on PATH", name));
        }
        self.lsp_status.set_running();

        on_progress("Enrichment: running pipeline via event bus (no sentinel)...");

        let (root_pairs, primary_slug) = self.build_bus_root_pairs();
        let bus_result = crate::extract::consumers::emit_enrichment_pipeline(
            all_nodes,
            all_edges,
            root_pairs,
            primary_slug,
            self.repo_root.clone(),
            crate::extract::consumers::BusOptions {
                scan_stats: Some(Arc::clone(&self.scan_stats)),
                embed_idx: None,
                lance_repo_root: None,
                skip_lsp: false,
            },
            None, // no sentinel means all roots need enrichment
        )
        .await;

        match bus_result {
            Ok((mut enriched_nodes, mut enriched_edges, detected_frameworks)) => {
                // Dedup: passes can re-emit cached entries.
                {
                    let mut seen_nodes = std::collections::HashSet::new();
                    enriched_nodes.reverse();
                    enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                    enriched_nodes.reverse();

                    let mut seen_edges = std::collections::HashSet::new();
                    enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                }

                let lsp_edge_count = enriched_edges
                    .iter()
                    .filter(|e| {
                        e.source == crate::graph::ExtractionSource::Lsp
                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                    })
                    .count();

                on_progress(&format!(
                    "Enrichment: {} LSP call edges via bus",
                    lsp_edge_count
                ));

                // Build updated index and apply enriched graph via atomic swap.
                let mut new_index = crate::graph::index::GraphIndex::new();
                new_index.rebuild_from_edges(&enriched_edges);
                for node in &enriched_nodes {
                    new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                {
                    let snap = self.graph.load_full();
                    if let Some(ref current_gs) = *snap {
                        let mut gs = (**current_gs).clone();
                        gs.nodes = enriched_nodes.clone();
                        gs.edges = enriched_edges.clone();
                        gs.index = new_index;
                        gs.detected_frameworks = detected_frameworks;
                        self.graph.store(Arc::new(Some(Arc::new(gs))));
                    }
                }
                self.lsp_status.set_complete(lsp_edge_count);

                // Persist with enriched edges.
                if let Err(e) =
                    persist_graph_to_lance(&self.repo_root, &enriched_nodes, &enriched_edges).await
                {
                    tracing::error!("Foreground LSP persist failed: {}", e);
                    return Err(e.context("LSP persist failed during foreground pipeline"));
                }
                // Persist succeeded -- write LSP sentinel so future startups know
                // LSP enrichment is durable and can skip re-enrichment (#477).
                super::sentinel::write_lsp_sentinel(
                    &self.repo_root,
                    enriched_nodes.len(),
                    enriched_edges.len(),
                );

                Ok(lsp_edge_count)
            }
            Err(e) => {
                tracing::error!(
                    "Foreground LSP pipeline: emit_enrichment_pipeline failed: {:#}",
                    e
                );
                on_progress("Enrichment: pipeline failed -- no LSP edges available");
                self.lsp_status.set_unavailable();
                Ok(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractionSource, NodeId, NodeKind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(name: &str, kind: NodeKind) -> Node {
        Node {
            id: NodeId {
                root: "test".to_string(),
                file: PathBuf::from("src/test.rs"),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start: 0,
            line_end: 0,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn test_cache_needs_enrichment_empty_graph() {
        assert!(!cache_needs_enrichment(&[]));
    }

    #[test]
    fn test_cache_needs_enrichment_no_imports() {
        let nodes = vec![make_node("Config", NodeKind::Function)];
        assert!(!cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_cache_needs_enrichment_imports_with_frameworks_but_no_framework_nodes() {
        // Import matches a known framework pattern ("tokio") but no framework
        // nodes exist -- cache is stale.
        let mut import_node = make_node("use tokio::runtime::Runtime", NodeKind::Import);
        import_node.id.file = PathBuf::from("src/main.rs");
        let nodes = vec![import_node];
        assert!(cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_cache_needs_enrichment_imports_with_framework_nodes_present() {
        // Import matches a known framework AND a framework node exists -- cache is fine.
        let mut import_node = make_node("use tokio::runtime::Runtime", NodeKind::Import);
        import_node.id.file = PathBuf::from("src/main.rs");
        let mut fw_node = make_node("tokio", NodeKind::Other("framework".to_string()));
        fw_node.id.file = PathBuf::from("frameworks/tokio");
        let nodes = vec![import_node, fw_node];
        assert!(!cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_cache_needs_enrichment_imports_no_matching_framework() {
        // Import exists but doesn't match any known framework rule -- cache is fine.
        let import_node = make_node("use my_unknown_crate::Foo", NodeKind::Import);
        let nodes = vec![import_node];
        assert!(!cache_needs_enrichment(&nodes));
    }
}
