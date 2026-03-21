//! Graph lifecycle: building, incremental updates, and state management.

use std::path::PathBuf;
use std::sync::Arc;

/// Metadata key for subsystem cluster assignment.
/// Shared with service.rs for filtering.
pub(crate) const SUBSYSTEM_KEY: &str = "subsystem";

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
    check_and_migrate_extraction_version, check_and_migrate_schema, delete_nodes_for_roots,
    get_stored_root_ids, graph_lance_path, load_graph_from_lance, persist_graph_incremental,
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
                self.update_graph_with_scan(graph, scan, true).await?;
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
        let live_slugs: std::collections::HashSet<String> = resolved_roots
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

        for (root_slug, scanner, _scan_result, root_path, root_changed) in &scanners {
            if !root_changed {
                // Clean root: load from pre-indexed cache if available, otherwise extract.
                if has_cached_graph {
                    let cached_nodes = cached_nodes_by_root.remove(root_slug);
                    let cached_edges = cached_edges_by_root.remove(root_slug);

                    if let Some(nodes) = cached_nodes {
                        if !nodes.is_empty() {
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

            // For dirty roots, carry forward cached LSP edges whose endpoints
            // still exist in the freshly extracted node set. Tree-sitter re-extraction
            // only produces tree-sitter edges; LSP edges (Calls, ReferencedBy from LSP)
            // are produced by the background enricher and would otherwise be lost on
            // every incremental rebuild.
            let mut lsp_carry_count = 0usize;
            if *root_changed && has_cached_graph {
                if let Some(cached_edges) = cached_edges_by_root.remove(root_slug) {
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

        // 4b. API link pass: connect URL-path string-literal Const nodes to
        //     ApiEndpoint nodes via DependsOn edges.
        //     Runs here (after all roots are merged) rather than inside
        //     extract_scan_result() so that cross-file links are created
        //     correctly during incremental scans — the caller may be in a
        //     changed file while the ApiEndpoint is in an unchanged file or
        //     vice versa.
        {
            let api_link_edges = crate::extract::api_link::api_link_pass(&all_nodes);
            if !api_link_edges.is_empty() {
                tracing::info!(
                    "API link pass: {} DependsOn edge(s) from string literals to endpoints",
                    api_link_edges.len()
                );
                all_edges.extend(api_link_edges);
            }
        }

        // 4c. Manifest pass: emit package nodes + DependsOn edges for JS/TS,
        //     Python, and Go projects by parsing package.json, pyproject.toml,
        //     requirements.txt, and go.mod files.
        {
            let root_pairs: Vec<(String, std::path::PathBuf)> = resolved_roots
                .iter()
                .map(|r| (r.slug.clone(), r.path.clone()))
                .collect();
            let manifest_result = crate::extract::manifest::manifest_pass(&root_pairs);
            if !manifest_result.nodes.is_empty() || !manifest_result.edges.is_empty() {
                tracing::info!(
                    "Manifest pass: {} package node(s), {} DependsOn edge(s)",
                    manifest_result.nodes.len(),
                    manifest_result.edges.len()
                );
                all_nodes.extend(manifest_result.nodes);
                all_edges.extend(manifest_result.edges);
            }
        }

        // 4d. TestedBy naming-convention pass: emit TestedBy edges from test
        //     functions to the production functions they exercise, using only
        //     function names (no LSP required).  Runs here so that edges are
        //     present after every tree-sitter-only scan, not just when LSP
        //     finishes initialising.
        {
            let tested_by_edges = crate::extract::naming_convention::tested_by_pass(&all_nodes);
            if !tested_by_edges.is_empty() {
                tracing::info!(
                    "TestedBy naming-convention pass: {} edge(s)",
                    tested_by_edges.len()
                );
                all_edges.extend(tested_by_edges);
            }
        }

        // 4e. Directory-module pass: emit BelongsTo edges from every symbol to
        //     a virtual NodeKind::Module node derived from its directory path.
        //     Runs unconditionally for all languages — no LSP required.
        //     The LSP enricher's Pass 4 (rust-analyzer/parentModule) provides
        //     more accurate Rust module paths and runs later as an override.
        //
        //     Deduplication note: for clean-root cache-reuse builds, the cached
        //     node set already contains module nodes from the previous run.  This
        //     pass will re-emit them with identical stable_ids — LanceDB's
        //     merge_insert upsert path handles the dedup at persist time, and
        //     GraphIndex's ensure_node deduplicates in the petgraph index.  The
        //     same approach is used by api_link_pass and tested_by_pass.
        {
            let dir_result = crate::extract::directory_module::directory_module_pass(&all_nodes);
            if !dir_result.edges.is_empty() {
                tracing::info!(
                    "Directory module pass: {} BelongsTo edge(s), {} module node(s)",
                    dir_result.edges.len(),
                    dir_result.nodes.len(),
                );
                all_nodes.extend(dir_result.nodes);
                all_edges.extend(dir_result.edges);
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
                    if name_counts.get(&s.name).copied().unwrap_or(0) > 1 {
                        if let Some(iface) = s.interfaces.first() {
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
            let _lance_guard = self.lance_write_lock.lock().await;
            if let Err(e) = persist_graph_to_lance(&self.repo_root, &all_nodes, &all_edges).await {
                tracing::error!("Failed to persist graph to LanceDB: {}", e);
                return Err(e.context("LanceDB full persist failed during graph build"));
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
        // Embedding and LSP enrichment run in background via the shared graph lock.
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
    ///
    /// When `spawn_lsp` is true (default for MCP server), LSP enrichment for
    /// changed nodes is spawned as a background task. When false (CLI `--full`
    /// incremental path), no LSP is spawned -- the caller handles it.
    pub(crate) async fn update_graph_with_scan(
        &self,
        graph: &mut GraphState,
        pending_scan: Option<ScanResult>,
        spawn_lsp: bool,
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
        //
        // NOTE: upsert_edges is declared mut so post-extraction passes
        // (api_link, tested_by) can append their edges to the persist delta.
        let mut upsert_node_ids: std::collections::HashSet<String> =
            extraction.nodes.iter().map(|n| n.stable_id()).collect();
        let mut upsert_edges: Vec<Edge> = extraction.edges.clone();
        graph.nodes.extend(extraction.nodes);
        graph.edges.extend(extraction.edges);

        // Re-run API link pass with the full (existing + new) node set so that
        // cross-file links are created when only one side of a match was updated.
        // Duplicate edges from the previous pass are deduplicated by the LanceDB
        // merge_insert upsert path and are harmless in the in-memory graph
        // (stable_id-keyed dedup happens at persist time).
        {
            let api_link_edges = crate::extract::api_link::api_link_pass(&graph.nodes);
            if !api_link_edges.is_empty() {
                tracing::info!(
                    "API link pass (incremental): {} DependsOn edge(s) from string literals to endpoints",
                    api_link_edges.len()
                );
                // Include in upsert delta so these edges are persisted to LanceDB.
                upsert_edges.extend(api_link_edges.iter().cloned());
                graph.edges.extend(api_link_edges);
            }
        }

        // Re-run manifest pass with the primary root so that changes to
        // package.json, pyproject.toml, requirements.txt, or go.mod are
        // reflected in the incremental graph. Package nodes from previous
        // passes are harmlessly overwritten (same stable_id, same data).
        {
            let root_pairs = vec![(primary_slug.clone(), self.repo_root.clone())];
            let manifest_result = crate::extract::manifest::manifest_pass(&root_pairs);
            if !manifest_result.nodes.is_empty() || !manifest_result.edges.is_empty() {
                tracing::info!(
                    "Manifest pass (incremental): {} package node(s), {} DependsOn edge(s)",
                    manifest_result.nodes.len(),
                    manifest_result.edges.len()
                );
                graph.nodes.extend(manifest_result.nodes);
                graph.edges.extend(manifest_result.edges);
            }
        }

        // Re-run TestedBy naming-convention pass over the full node set so
        // that test/production pairs where only one side changed are still
        // linked.  Include in upsert delta so edges are persisted to LanceDB —
        // previously this block ran AFTER upsert_edges was captured, meaning
        // TestedBy edges were never written to the incremental persist delta.
        {
            let tested_by_edges = crate::extract::naming_convention::tested_by_pass(&graph.nodes);
            if !tested_by_edges.is_empty() {
                tracing::info!(
                    "TestedBy naming-convention pass (incremental): {} edge(s)",
                    tested_by_edges.len()
                );
                // Include in upsert delta so these edges are persisted to LanceDB.
                upsert_edges.extend(tested_by_edges.iter().cloned());
                graph.edges.extend(tested_by_edges);
            }
        }

        // Re-run directory-module pass over the full node set so that
        // BelongsTo edges are always present without requiring LSP quiescence.
        // Include in upsert delta so edges are persisted to LanceDB.
        {
            let dir_result =
                crate::extract::directory_module::directory_module_pass(&graph.nodes);
            if !dir_result.edges.is_empty() {
                tracing::info!(
                    "Directory module pass (incremental): {} BelongsTo edge(s), {} module node(s)",
                    dir_result.edges.len(),
                    dir_result.nodes.len(),
                );
                // Also add the new module nodes to the upsert delta so they
                // are persisted to LanceDB.  Without this, upsert_nodes is
                // rebuilt only from upsert_node_ids and the module rows would
                // be missing — leaving BelongsTo edges pointing at ghost nodes.
                upsert_node_ids.extend(dir_result.nodes.iter().map(|n| n.stable_id()));
                upsert_edges.extend(dir_result.edges.iter().cloned());
                graph.edges.extend(dir_result.edges);
                graph.nodes.extend(dir_result.nodes);
            }
        }

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

        if spawn_lsp && !changed_nodes.is_empty() {
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
                    if name_counts.get(&s.name).copied().unwrap_or(0) > 1 {
                        if let Some(iface) = s.interfaces.first() {
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
            persist_graph_incremental(
                &self.repo_root,
                &upsert_nodes,
                &upsert_edges,
                &deleted_edge_ids,
                &files_to_remove,
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
        if persist_succeeded {
            if let Some(scanner) = fallback_scanner {
                scanner.commit_state()?;
            }
        }

        graph.last_scan_completed_at = Some(std::time::Instant::now());

        Ok(())
    }
}
