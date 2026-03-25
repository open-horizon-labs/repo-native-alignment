//! Background scanner stage methods extracted from `spawn_background_scanner`.
//!
//! The background scanner loop calls three stages per tick:
//! 1. `scan_roots()` -- resolve workspace roots, detect file changes
//! 2. `update_graph()` -- apply changes to in-memory graph, run enrichment pipeline
//! 3. `persist_deltas()` -- write incremental changes to LanceDB, commit scanner state

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::extract::ExtractorRegistry;
use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;
use crate::roots::{RootConfig, WorkspaceConfig, cache_state_path};
use crate::scanner::Scanner;

use super::helpers;
use super::state::GraphState;
use super::store::{
    delete_nodes_for_roots, persist_graph_incremental, persist_graph_to_lance,
};

/// LanceDB persist delta: (root_slug, root_path, upsert_nodes, upsert_edges, deleted_edge_ids, files_to_remove)
pub(super) type LanceDelta = (String, PathBuf, Vec<Node>, Vec<Edge>, Vec<String>, HashSet<PathBuf>);

/// Result of scanning all workspace roots for file changes.
pub(super) struct ScanResult {
    #[allow(dead_code)]
    pub resolved_roots: Vec<crate::roots::ResolvedRoot>,
    pub per_root_scans: Vec<(String, crate::scanner::ScanResult, PathBuf, Scanner)>,
    pub removed_slugs: Vec<String>,
    pub has_changes: bool,
    pub current_root_slugs: HashSet<String>,
}

/// Scan all workspace roots for file-level changes.
///
/// Resolves live roots (primary + worktrees + memory + declared), detects removed
/// roots from the previous tick, and runs the scanner on each non-lsp_only root.
pub(super) fn scan_roots(
    repo_root: &std::path::Path,
    prev_root_slugs: &HashSet<String>,
) -> ScanResult {
    let workspace = WorkspaceConfig::load()
        .with_primary_root(repo_root.to_path_buf())
        .with_worktrees(repo_root)
        .with_claude_memory(repo_root)
        .with_agent_memories(repo_root)
        .with_declared_roots(repo_root);
    let resolved_roots = workspace.resolved_roots();
    let current_root_slugs: HashSet<String> =
        resolved_roots.iter().map(|r| r.slug.clone()).collect();

    // Slugs that disappeared -> worktree was removed.
    let removed_slugs: Vec<String> = prev_root_slugs
        .difference(&current_root_slugs)
        .cloned()
        .collect();

    // Scan every live root for file-level changes.
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

    ScanResult {
        resolved_roots,
        per_root_scans,
        removed_slugs,
        has_changes,
        current_root_slugs,
    }
}

/// Apply file-level changes to the in-memory graph and run enrichment pipeline.
///
/// Takes the current graph snapshot, applies removals and new extractions per root,
/// runs the event-bus enrichment pipeline, deduplicates, rebuilds the petgraph index,
/// and computes PageRank. Returns the updated graph state and LanceDB deltas.
#[allow(clippy::too_many_arguments)]
pub(super) async fn update_graph(
    graph_state: &mut GraphState,
    scan_result: &mut ScanResult,
    repo_root: &std::path::Path,
    scan_stats: &Arc<std::sync::RwLock<crate::extract::scan_stats::ScanStats>>,
) -> Vec<LanceDelta> {
    let mut lance_deltas: Vec<LanceDelta> = Vec::new();
    let registry = ExtractorRegistry::with_builtins();

    // Drop in-memory nodes/edges for removed worktrees.
    for slug in &scan_result.removed_slugs {
        tracing::info!(
            "Worktree removed -- dropping in-memory nodes for root '{}'",
            slug
        );
        graph_state.nodes.retain(|n| &n.id.root != slug);
        graph_state.edges.retain(|e| &e.from.root != slug);
    }

    // Apply file-level changes per root.
    for (root_slug, scan, root_path, _scanner) in &scan_result.per_root_scans {
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
        let files_to_remove: HashSet<PathBuf> = scan
            .deleted_files
            .iter()
            .chain(scan.changed_files.iter())
            .cloned()
            .collect();

        // Collect edge IDs to delete BEFORE retain.
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

        // Merge encoding stats.
        if let Ok(mut stats) = scan_stats.write() {
            stats.merge_encoding_stats(root_slug, &enc_stats);
        }

        for node in &mut extraction.nodes {
            node.id.root = root_slug.clone();
        }
        // Build file index for suffix resolution
        let file_index: HashSet<String> = graph_state.nodes
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

    // Run post-extraction passes via EventBus.
    {
        let before_node_ids: HashSet<String> =
            graph_state.nodes.iter().map(|n| n.stable_id()).collect();
        let before_edge_ids: HashSet<String> =
            graph_state.edges.iter().map(|e| e.stable_id()).collect();

        let root_pairs: Vec<(String, PathBuf)> =
            WorkspaceConfig::load()
                .with_primary_root(repo_root.to_path_buf())
                .with_worktrees(repo_root)
                .with_declared_roots(repo_root)
                .resolved_roots()
                .into_iter()
                .map(|r| (r.slug, r.path))
                .collect();
        let primary_slug =
            RootConfig::code_project(repo_root.to_path_buf()).slug();

        // Only roots with file changes should trigger LSP enrichment.
        let dirty_slugs: Option<HashSet<String>> = Some(scan_result.per_root_scans
            .iter()
            .filter(|(_, scan, _, _)| {
                !scan.changed_files.is_empty()
                    || !scan.new_files.is_empty()
                    || !scan.deleted_files.is_empty()
            })
            .map(|(slug, _, _, _)| slug.clone())
            .collect());

        match crate::extract::consumers::emit_enrichment_pipeline(
            std::mem::take(&mut graph_state.nodes),
            std::mem::take(&mut graph_state.edges),
            root_pairs,
            primary_slug.clone(),
            repo_root.to_path_buf(),
            crate::extract::consumers::BusOptions {
                scan_stats: Some(Arc::clone(scan_stats)),
                embed_idx: None,
                lance_repo_root: None,
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
                     (pipeline invariant violated) -- aborting tick, \
                     no data will be persisted: {:#}",
                    e
                );
                lance_deltas.clear();
            }
        }

        // Deduplicate nodes and edges after post-extraction passes.
        {
            let mut seen_nodes = HashSet::new();
            graph_state.nodes.reverse();
            graph_state.nodes.retain(|n| seen_nodes.insert(n.stable_id()));
            graph_state.nodes.reverse();

            let mut seen_edges = HashSet::new();
            graph_state.edges.retain(|e| seen_edges.insert(e.stable_id()));
        }

        // Collect net-new nodes/edges from the passes for LanceDB persist.
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
            lance_deltas.push((
                primary_slug,
                repo_root.to_path_buf(),
                new_nodes,
                new_edges,
                Vec::new(),
                HashSet::new(),
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

    // Recompute PageRank importance scores.
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

    lance_deltas
}

/// Persist incremental deltas to LanceDB and commit scanner state.
///
/// Acquires the write mutex to serialize with other LanceDB writers,
/// tracks which roots persisted successfully, and only commits scanner
/// state for those roots.
pub(super) async fn persist_deltas(
    lance_deltas: Vec<LanceDelta>,
    per_root_scans: &[(String, crate::scanner::ScanResult, PathBuf, Scanner)],
    removed_slugs: &[String],
    repo_root: &std::path::Path,
    graph: &arc_swap::ArcSwap<Option<Arc<GraphState>>>,
    lance_write_lock: &tokio::sync::Mutex<()>,
) {
    let mut persisted_slugs: HashSet<String> = HashSet::new();
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
                let snap = graph.load_full();
                let snapshot = snap.as_ref().as_ref()
                    .map(|gs| (gs.nodes.clone(), gs.edges.clone()));
                if let Some((nodes, edges)) = snapshot {
                    let _lance_guard = lance_write_lock.lock().await;
                    if let Err(e) = persist_graph_to_lance(repo_root, &nodes, &edges).await {
                        tracing::error!("Background scan: full persist after migration failed: {:#}", e);
                        continue;
                    }
                }
                persisted_slugs.insert(slug);
            }
            Ok(false) => {
                persisted_slugs.insert(slug);
            }
            Err(e) => {
                tracing::error!("Background scan: failed to persist graph delta for '{}': {:#}", slug, e);
            }
        }
    }

    // Commit scanner state only for roots that persisted successfully.
    for (root_slug, _scan, _root_path, scanner) in per_root_scans {
        if persisted_slugs.contains(root_slug)
            && let Err(e) = scanner.commit_state() {
                tracing::error!("Background scan: failed to commit scanner state for '{}': {}", root_slug, e);
            }
    }

    // Update freshness timestamp if any root persisted successfully.
    if !persisted_slugs.is_empty() {
        let snap = graph.load();
        if let Some(ref gs) = **snap {
            let mut updated = (**gs).clone();
            updated.last_scan_completed_at = Some(std::time::Instant::now());
            graph.store(Arc::new(Some(Arc::new(updated))));
        }
    }

    // Purge removed worktree slugs from LanceDB.
    if !removed_slugs.is_empty() {
        let _lance_guard = lance_write_lock.lock().await;
        if let Err(e) = delete_nodes_for_roots(repo_root, removed_slugs).await {
            tracing::warn!(
                "Failed to delete LanceDB rows for removed worktrees: {}",
                e
            );
        }
    }
}
