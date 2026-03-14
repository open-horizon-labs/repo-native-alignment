//! MCP server: RnaHandler, graph building, background scanner, and MCP dispatch.

pub mod tools;
pub mod store;
pub mod state;
pub mod handlers;
pub mod helpers;

// Re-export public API so external `use crate::server::X` still works.
pub use tools::*;
pub use store::{load_graph_from_lance, parse_edge_kind};
pub(crate) use store::{
    check_and_migrate_schema, graph_lance_path,
    persist_graph_incremental, persist_graph_to_lance,
};
pub use state::{GraphState, LspEnrichmentStatus, LspState};
pub use helpers::format_freshness;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CallToolError, CallToolRequestParams, CallToolResult, ListToolsResult,
    PaginatedRequestParams, RpcError, TextContent,
};

use crate::embed::{EmbeddingIndex, SearchOutcome};
use crate::extract::{ExtractorRegistry, EnricherRegistry};
use crate::graph::{Edge, Node, NodeKind};
use crate::graph::index::GraphIndex;
use crate::graph::store::SCHEMA_VERSION;
use crate::roots::{RootConfig, WorkspaceConfig, cache_state_path};
use crate::scanner::{ScanResult, Scanner};
use crate::types::OhArtifactKind;
use crate::{git, markdown, oh, query, ranking};
use arc_swap::ArcSwap;
use tokio::sync::RwLock;

use helpers::{
    IMPORTANCE_THRESHOLD,
    parse_args, text_result,
};
use handlers::parse_search_mode;
use store::{
    delete_nodes_for_roots, get_stored_root_ids,
};

// ── Pipeline result ─────────────────────────────────────────────────

/// Result of a full pipeline run (used by `--full` CLI mode).
pub struct PipelineResult {
    pub node_count: usize,
    pub edge_count: usize,
    pub lsp_edge_count: usize,
    pub embed_count: usize,
    pub total_time: std::time::Duration,
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
    pub graph: Arc<RwLock<Option<GraphState>>>,
    /// Double-buffered embedding index.
    pub embed_index: Arc<ArcSwap<Option<EmbeddingIndex>>>,
    /// Whether business context has been injected into a tool response.
    pub context_injected: std::sync::atomic::AtomicBool,
    /// Cooldown: skip re-scanning if checked recently.
    pub last_scan: std::sync::Mutex<std::time::Instant>,
    /// Whether background scanner has been spawned.
    pub background_scanner_started: std::sync::atomic::AtomicBool,
    /// LSP enrichment status — shared with background enrichment tasks.
    pub lsp_status: Arc<LspEnrichmentStatus>,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            graph: Arc::new(RwLock::new(None)),
            embed_index: Arc::new(ArcSwap::from_pointee(None)),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            last_scan: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
            background_scanner_started: std::sync::atomic::AtomicBool::new(false),
            lsp_status: Arc::new(LspEnrichmentStatus::probe_for_servers()),
        }
    }
}

impl RnaHandler {
    /// Return the slug for the primary `--repo` root.
    pub(crate) fn primary_root_slug(&self) -> String {
        crate::roots::RootConfig::code_project(self.repo_root.clone()).slug()
    }

    /// Resolve the effective root filter from a user-supplied `root` parameter.
    ///
    /// - `None` (caller omitted) -> scope to primary root slug (default)
    /// - `Some("all")` -> no filter (cross-root search)
    /// - `Some(slug)` -> use as explicit root slug
    ///
    /// Returns `None` when no filtering should be applied ("all" case).
    pub(crate) fn effective_root_filter(&self, root_param: Option<&str>) -> Option<String> {
        match root_param {
            Some(v) if v.eq_ignore_ascii_case("all") => None,
            Some(slug) => Some(slug.to_string()),
            None => Some(self.primary_root_slug()),
        }
    }

    /// Return the set of root slugs that correspond to non-code roots
    /// (Notes, General, Custom). These should always be included in
    /// query results regardless of root filtering.
    pub(crate) fn non_code_root_slugs(&self) -> std::collections::HashSet<String> {
        use crate::roots::{RootType, WorkspaceConfig};
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root);
        workspace
            .resolved_roots()
            .into_iter()
            .filter(|r| r.config.root_type != RootType::CodeProject)
            .map(|r| r.slug)
            .collect()
    }

    /// Check whether a node passes the root filter.
    /// Non-code roots (Notes, General, Custom) and "external" always pass.
    pub(crate) fn node_passes_root_filter(
        &self,
        node_root: &str,
        root_filter: &Option<String>,
        non_code_slugs: &std::collections::HashSet<String>,
    ) -> bool {
        match root_filter {
            None => true, // "all" mode
            Some(slug) => {
                node_root.eq_ignore_ascii_case(slug)
                    || node_root == "external"
                    || non_code_slugs.contains(node_root)
            }
        }
    }

    /// Check whether an embedding `SearchResult` passes the root filter.
    /// Code results (`kind` starts with "code:") are filtered by root slug
    /// extracted from the stable ID prefix. Non-code results (commits, .oh/
    /// artifacts) always pass through.
    pub(crate) fn search_result_passes_root_filter(
        &self,
        result: &crate::embed::SearchResult,
        root_filter: &Option<String>,
        non_code_slugs: &std::collections::HashSet<String>,
    ) -> bool {
        if root_filter.is_none() {
            return true; // "all" mode
        }
        // Non-code results (commits, oh artifacts) always pass
        if !result.kind.starts_with("code:") {
            return true;
        }
        // Extract root slug from stable ID: "root:file:name:kind"
        let node_root = result.id.split(':').next().unwrap_or("");
        self.node_passes_root_filter(node_root, root_filter, non_code_slugs)
    }

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
                        tracing::info!("HEAD changed — triggering immediate background scan");
                    } else if fetch_head_changed {
                        tracing::info!(
                            "FETCH_HEAD changed — triggering immediate background scan"
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

                    // Slugs that disappeared → worktree was removed.
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
                                "Worktree removed — dropping in-memory nodes for root '{}'",
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

    async fn build_full_graph_inner(&self, spawn_background: bool) -> anyhow::Result<GraphState> {
        // Initialize pattern config from .oh/config.toml (once, at first build).
        crate::extract::generic::init_pattern_config(&self.repo_root);

        // Pre-flight: ensure schema version matches before any LanceDB reads/writes.
        let db_path = graph_lance_path(&self.repo_root);
        if check_and_migrate_schema(&db_path).await? {
            tracing::info!("Schema migrated to v{} — cache rebuilt", SCHEMA_VERSION);
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
        // discovered by WorkspaceConfig but are valid — skip them during stale pruning.
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
                                // Ensure FTS indexes exist — table may predate hybrid search.
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
                                "Cached graph has no call edges — spawning LSP enrichment"
                            );
                            self.spawn_lsp_enrichment(&state.nodes);
                        } else {
                            tracing::info!(
                                "Cached graph already has call edges — skipping LSP enrichment"
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
        // Only include nodes whose root is genuinely external/virtual — not stale
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
        match git::pr_merges::extract_pr_merges(&self.repo_root, Some(100)) {
            Ok((pr_nodes, pr_edges)) => {
                let modified_edges =
                    git::pr_merges::link_pr_to_symbols(&pr_nodes, &all_nodes);
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
                tracing::info!("Embedding index created — background task will re-index");
                self.embed_index.store(Arc::new(Some(idx)));
            }
            Err(e) => {
                tracing::warn!("Failed to create embed index: {}", e);
            }
        };

        if spawn_background {
            // Spawn background task for embedding + LSP enrichment.
            // The graph is queryable NOW — these improve quality progressively.
            let bg_repo_root = self.repo_root.clone();
            let bg_graph = self.graph.clone();
            let bg_embed_index = self.embed_index.clone();
            let bg_lsp_status = self.lsp_status.clone();
            let bg_nodes = all_nodes.clone();
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
                // Embed + LSP enrichment run concurrently — they use independent
                // data stores (embedding table vs graph edges).
                let embeddable_nodes: Vec<Node> = bg_nodes.iter()
                    .filter(|n| n.id.root != "external")
                    .cloned()
                    .collect();

                let embed_repo_root = bg_repo_root.clone();
                let embed_index_ref = bg_embed_index.clone();
                let embed_fut = async move {
                    // Always re-index so .oh/ artifacts added since the last
                    // full build become searchable.  index_all_inner drops and
                    // rebuilds the table -- acceptable at current repo scale.
                    match EmbeddingIndex::new(&embed_repo_root).await {
                        Ok(idx) => {
                            match idx.index_all_with_symbols(&embed_repo_root, &embeddable_nodes).await {
                                Ok(count) => {
                                    tracing::info!("[background] Embedded {} items", count);
                                    // Atomic store — no mutex needed
                                    embed_index_ref.store(Arc::new(Some(idx)));
                                }
                                Err(e) => tracing::warn!("[background] Embedding failed: {}", e),
                            }
                        }
                        Err(e) => tracing::warn!("[background] EmbeddingIndex init failed: {}", e),
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
                        if let Err(e) = persist_graph_incremental(
                            &lsp_repo_root, &upsert_nodes, &persist_edges, &[], &[],
                        ).await {
                            tracing::error!("Background LSP enrichment persist failed: {}", e);
                        }
                        bg_lsp_status.set_complete(edge_count);
                    }
                };

                tokio::join!(embed_fut, lsp_fut);
            });
        }

        Ok(GraphState {
            nodes: all_nodes,
            edges: all_edges,
            index,
            last_scan_completed_at: Some(symbols_ready_at),
        })
    }

    /// Spawn background LSP enrichment for the given nodes.
    /// Used both by the normal build path and the cache-hit early return path.
    fn spawn_lsp_enrichment(&self, nodes: &[Node]) {
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
                if let Err(e) = persist_graph_incremental(
                    &bg_repo_root, &upsert_nodes, &persist_edges, &[], &[],
                ).await {
                    tracing::error!("Background LSP enrichment persist failed: {}", e);
                }
                bg_lsp_status.set_complete(edge_count);
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

        // Phase 2: Embed + LSP enrichment (parallel — they use independent data stores)
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
                            tracing::warn!("Embed: failed — {}", e);
                            0
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Embed: init failed — {}", e);
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
                if let Err(e) = persist_graph_incremental(
                    &self.repo_root, &upsert_nodes, &persist_edges, &[], &[],
                ).await {
                    tracing::error!("Foreground LSP enrichment persist failed: {}", e);
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

    /// Incrementally update the graph, accepting an optional pre-computed scan.
    ///
    /// When `pending_scan` is `Some`, the caller already ran the scanner and
    /// will commit state after this method returns successfully.
    ///
    /// When `pending_scan` is `None`, this method creates its own scanner and
    /// commits state only after the graph update succeeds.
    async fn update_graph_with_scan(
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
        // Extractors don't set root — the caller must assign it, matching the
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

        // Track only the delta (new/changed) for LanceDB upsert — not the full graph.
        let mut upsert_nodes: Vec<Node> = extraction.nodes.clone();
        let mut upsert_edges: Vec<Edge> = extraction.edges.clone();
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

        // Run LSP enrichers on the updated nodes (same as cold-start, but scoped to changed files)
        // PageRank is deferred until after enrichment so topology changes are included.
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
            let languages: Vec<String> = changed_nodes
                .iter()
                .map(|n| n.language.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            self.lsp_status.set_running();
            let enricher_registry = EnricherRegistry::with_builtins();
            let enrichment = enricher_registry
                .enrich_all(&changed_nodes, &graph.index, &languages, &self.repo_root)
                .await;

            if !enrichment.any_enricher_ran {
                self.lsp_status.set_unavailable();
            }

            let incr_edge_count = enrichment.added_edges.len();

            // Add virtual nodes synthesized for external symbols.
            // These must NOT be re-embedded (no body).
            if !enrichment.new_nodes.is_empty() {
                tracing::info!(
                    "Incremental LSP enrichment synthesized {} virtual external nodes",
                    enrichment.new_nodes.len()
                );
                for vnode in &enrichment.new_nodes {
                    graph.index.ensure_node(&vnode.stable_id(), &vnode.id.kind.to_string());
                }
                // Include synthesized virtual nodes in the LanceDB upsert delta.
                upsert_nodes.extend(enrichment.new_nodes.iter().cloned());
                graph.nodes.extend(enrichment.new_nodes);
            }

            if !enrichment.added_edges.is_empty() {
                tracing::info!(
                    "Incremental LSP enrichment added {} edges",
                    enrichment.added_edges.len()
                );
                for edge in &enrichment.added_edges {
                    graph.index.add_edge(
                        &edge.from.to_stable_id(),
                        &edge.from.kind.to_string(),
                        &edge.to.to_stable_id(),
                        &edge.to.kind.to_string(),
                        edge.kind.clone(),
                    );
                }
                // Include LSP-synthesized edges in the LanceDB upsert delta.
                upsert_edges.extend(enrichment.added_edges.iter().cloned());
                graph.edges.extend(enrichment.added_edges);
            }

            let enriched_node_ids: std::collections::HashSet<String> =
                enrichment.updated_nodes.iter().map(|(id, _)| id.clone()).collect();

            for (node_id, patches) in &enrichment.updated_nodes {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                    for (key, value) in patches {
                        node.metadata.insert(key.clone(), value.clone());
                    }
                }
            }

            // Re-embed the enriched nodes specifically, using their post-enrichment metadata.
            // This is scoped to only changed nodes to avoid re-embedding the entire graph.
            if !enriched_node_ids.is_empty() {
                let embed_guard = self.embed_index.load();
                if let Some(ref embed_idx) = **embed_guard {
                    let enriched_nodes: Vec<_> = graph
                        .nodes
                        .iter()
                        .filter(|n| enriched_node_ids.contains(&n.stable_id()))
                        .cloned()
                        .collect();
                    // Also include LSP-enriched nodes in the LanceDB upsert delta so their
                    // updated metadata (e.g. resolved signatures) is persisted.
                    // Deduplicate against nodes already queued for upsert (extraction may overlap).
                    let already_queued: std::collections::HashSet<String> =
                        upsert_nodes.iter().map(|u| u.stable_id()).collect();
                    let new_enriched: Vec<Node> = enriched_nodes
                        .iter()
                        .filter(|n| !already_queued.contains(&n.stable_id()))
                        .cloned()
                        .collect();
                    upsert_nodes.extend(new_enriched);
                    match embed_idx.reindex_nodes(&enriched_nodes).await {
                        Ok(count) => tracing::info!(
                            "Re-embedded {} enriched nodes with LSP metadata",
                            count
                        ),
                        Err(e) => tracing::warn!("Failed to re-embed enriched nodes: {}", e),
                    }
                }
            }

            if enrichment.any_enricher_ran {
                self.lsp_status.set_complete(incr_edge_count);
            }
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

fn build_context_preamble(root: &Path) -> String {
    let artifacts = match oh::load_oh_artifacts(root) {
        Ok(a) => a,
        Err(_) => return String::new(),
    };

    if artifacts.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();

    // Active outcomes (just names + status)
    let outcomes: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Outcome).collect();
    if !outcomes.is_empty() {
        let mut section = String::from("**Active outcomes:**\n");
        for o in &outcomes {
            let status = o.frontmatter.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            section.push_str(&format!("- {} ({})\n", o.id(), status));
        }
        parts.push(section);
    }

    // Hard/soft guardrails (just statements, no full body)
    let guardrails: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Guardrail).collect();
    if !guardrails.is_empty() {
        let mut section = String::from("**Guardrails:**\n");
        for g in &guardrails {
            let severity = g.frontmatter.get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("candidate");
            let id = g.id();
            let statement = g.frontmatter.get("statement")
                .and_then(|v| v.as_str())
                .unwrap_or(&id);
            section.push_str(&format!("- [{}] {}\n", severity, statement));
        }
        parts.push(section);
    }

    // Recent metis (last 3 titles only)
    let metis: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Metis).collect();
    if !metis.is_empty() {
        let mut section = String::from("**Recent learnings:**\n");
        for m in metis.iter().rev().take(3) {
            let id = m.id();
            let title = m.frontmatter.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&id);
            section.push_str(&format!("- {}\n", title));
        }
        parts.push(section);
    }

    let mut out = format!("---\n# Business Context (auto-injected on first tool call)\n\n{}\n", parts.join("\n"));
    out.push_str("**Code exploration:** use `search` (not Grep/Read), `oh_search_context` (not search_all). `search_symbols` and `graph_query` are deprecated aliases for `search`.\n");
    out.push_str("---\n\n");
    out
}

#[async_trait]
impl rust_mcp_sdk::mcp_server::ServerHandler for RnaHandler {
    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: vec![
                OhSearchContext::tool(),
                OutcomeProgress::tool(),
                Search::tool(),
                SearchSymbols::tool(),  // deprecated alias
                GraphQuery::tool(),     // deprecated alias
                ListRoots::tool(),
                RepoMap::tool(),
            ],
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let root = &self.repo_root;

        // Build business context preamble on first tool call (deferred store).
        // We load() first to check whether injection already happened, build
        // the preamble eagerly, then only store(true) after successfully
        // inserting it into the response — avoiding a false-positive flag if
        // the tool call itself errors before we can prepend.
        let preamble = if !self.context_injected.load(std::sync::atomic::Ordering::Relaxed) {
            let ctx = build_context_preamble(root);
            if !ctx.is_empty() {
                tracing::info!("Injecting business context preamble on first tool call");
                Some(ctx)
            } else {
                // Empty preamble — mark as injected so we don't retry.
                self.context_injected.store(true, std::sync::atomic::Ordering::Relaxed);
                None
            }
        } else {
            None
        };

        let mut result = match params.name.as_str() {
            "oh_search_context" => {
                let args: OhSearchContext = parse_args(params.arguments)?;
                let query = args.query.trim();
                if query.is_empty() {
                    return Ok(text_result("Empty query. Please describe what you're looking for.".into()));
                }
                let limit = args.limit.unwrap_or(5) as usize;
                let include_code = args.include_code.unwrap_or(false);
                let include_markdown = args.include_markdown.unwrap_or(false);
                let search_mode = parse_search_mode(args.search_mode.as_deref());
                let root_filter = self.effective_root_filter(args.root.as_deref());
                let non_code_slugs = if root_filter.is_some() {
                    self.non_code_root_slugs()
                } else {
                    std::collections::HashSet::new()
                };

                // Ensure graph is built first so symbols are embedded
                // (get_graph builds the graph + embeds symbols in the pipeline)
                let (graph_node_count, graph_last_scan) = match self.get_graph().await {
                    Ok(guard) => {
                        if let Some(gs) = guard.as_ref() {
                            (gs.nodes.len(), gs.last_scan_completed_at)
                        } else {
                            (0, None)
                        }
                    }
                    Err(_) => (0, None),
                };

                let mut sections: Vec<String> = Vec::new();

                // Search .oh/ artifacts + symbols via embedding index.
                // Lock-free load from ArcSwap — no mutex contention with graph writes.
                {
                    let embed_guard = self.embed_index.load();
                    match embed_guard.as_ref() {
                        Some(index) => {
                            match index.search_with_mode(query, args.artifact_types.as_deref(), limit, search_mode).await {
                                Ok(SearchOutcome::Results(results)) => {
                                    // Filter by root: code results scoped to effective root,
                                    // non-code (commits, .oh/ artifacts) always pass through.
                                    let filtered: Vec<_> = results
                                        .into_iter()
                                        .filter(|r| self.search_result_passes_root_filter(r, &root_filter, &non_code_slugs))
                                        .collect();
                                    if !filtered.is_empty() {
                                        let md: String = filtered
                                            .iter()
                                            .map(|r| r.to_markdown())
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        sections.push(format!(
                                            "### Artifacts ({} result(s))\n\n{}",
                                            filtered.len(),
                                            md
                                        ));
                                    }
                                }
                                Ok(SearchOutcome::NotReady) => {
                                    sections.push("Embedding index: building — semantic results will appear shortly. Retry in a few seconds.".to_string());
                                }
                                Err(e) => sections.push(format!("Artifact search error: {}", e)),
                            }
                        }
                        None => sections.push("Embedding index not yet available".to_string()),
                    }
                }

                // Optionally search code symbols from the graph (with 5-tier ranking)
                if include_code {
                    if let Ok(guard) = self.get_graph().await {
                        if let Some(gs) = guard.as_ref() {
                            let query_lower = query.to_lowercase();
                            let mut matches: Vec<&Node> = gs.nodes.iter()
                                .filter(|n| n.id.kind != NodeKind::Import && n.id.root != "external")
                                .filter(|n| self.node_passes_root_filter(&n.id.root, &root_filter, &non_code_slugs))
                                .filter(|n| {
                                    n.id.name.to_lowercase().contains(&query_lower)
                                        || n.signature.to_lowercase().contains(&query_lower)
                                })
                                .collect();

                            // Rank using the shared 5-tier cascade (same logic as search_symbols)
                            ranking::sort_symbol_matches(&mut matches, &query_lower, &gs.index);
                            matches.truncate(limit);

                            if !matches.is_empty() {
                                // Output format intentionally matches search_symbols
                                // (unordered list with `- **`) for backward compatibility.
                                // Results are sorted by relevance but use the same bullet
                                // style so agents parsing the old format are unaffected.
                                let md = matches.iter()
                                    .map(|n| {
                                        let mut line = format!(
                                            "- **{} {} ({})** ({})\n  `{}`\n  ID: `{}`",
                                            n.id.kind, n.id.name, n.language,
                                            n.id.file.display(),
                                            n.signature,
                                            n.stable_id(),
                                        );
                                        if let Some(cc) = n.metadata.get("cyclomatic") {
                                            line.push_str(&format!("\n  Complexity: {}", cc));
                                        }
                                        if let Some(imp) = n.metadata.get("importance") {
                                            if let Ok(score) = imp.parse::<f64>() {
                                                if score > IMPORTANCE_THRESHOLD {
                                                    line.push_str(&format!("\n  Importance: {:.3}", score));
                                                }
                                            }
                                        }
                                        line
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n");
                                sections.push(format!(
                                    "### Code symbols ({} result(s))\n\n{}",
                                    matches.len(),
                                    md
                                ));
                            }
                        }
                    }
                }

                // Optionally search markdown (with relevance scoring)
                if include_markdown {
                    match markdown::extract_markdown_chunks(root) {
                        Ok(chunks) => {
                            // Filter chunks by effective root scope (same as code symbols above).
                            let filtered_chunks: Vec<_> = if let Some(ref slug) = root_filter {
                                let workspace = WorkspaceConfig::load()
                                    .with_primary_root(self.repo_root.clone())
                                    .with_worktrees(&self.repo_root)
                                    .with_claude_memory(&self.repo_root);
                                let root_path = workspace.resolved_roots()
                                    .into_iter()
                                    .find(|r| r.slug == *slug)
                                    .map(|r| r.path);
                                if let Some(rp) = root_path {
                                    chunks.into_iter()
                                        .filter(|c| c.file_path.starts_with(&rp))
                                        .collect()
                                } else {
                                    // Slug didn't resolve to a known root — return
                                    // nothing rather than leaking unscoped markdown.
                                    Vec::new()
                                }
                            } else {
                                chunks
                            };
                            let scored = markdown::search_chunks_ranked(&filtered_chunks, query);
                            if !scored.is_empty() {
                                // Backward-compatible format: `- ` bullets, same header
                                // as before. Score is appended as a parenthetical so
                                // agents that ignore it are unaffected.
                                let md = scored
                                    .iter()
                                    .take(limit)
                                    .map(|sc| {
                                        format!(
                                            "- (score: {:.2}) {}", sc.score, sc.chunk.to_markdown()
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n---\n\n");
                                sections.push(format!(
                                    "### Markdown ({} result(s))\n\n{}",
                                    scored.len().min(limit),
                                    md
                                ));
                            }
                        }
                        Err(e) => sections.push(format!("Markdown search error: {}", e)),
                    }
                }

                let freshness = format_freshness(graph_node_count, graph_last_scan, Some(&self.lsp_status));
                if sections.is_empty() {
                    Ok(text_result(format!(
                        "No results found matching \"{}\".{}",
                        query, freshness
                    )))
                } else {
                    Ok(text_result(format!(
                        "## Semantic search: \"{}\"\n\n{}{}",
                        query,
                        sections.join("\n\n"),
                        freshness
                    )))
                }
            }

            "outcome_progress" => {
                let args: OutcomeProgress = parse_args(params.arguments)?;
                let include_impact = args.include_impact.unwrap_or(false);
                let root_filter = self.effective_root_filter(args.root.as_deref());
                let non_code_slugs = if root_filter.is_some() {
                    self.non_code_root_slugs()
                } else {
                    std::collections::HashSet::new()
                };
                let graph_nodes = if let Ok(guard) = self.get_graph().await {
                    guard.as_ref()
                        .map(|gs| gs.nodes.iter()
                            .filter(|n| self.node_passes_root_filter(&n.id.root, &root_filter, &non_code_slugs))
                            .cloned()
                            .collect())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                match query::outcome_progress(root, &args.outcome_id, &graph_nodes) {
                    Ok(result) => {
                        let mut md = result.to_summary_markdown();

                        // Append PR merge count from the graph
                        if let Ok(guard) = self.get_graph().await {
                         if let Some(graph_state) = guard.as_ref() {
                            let file_patterns: Vec<String> = result
                                .outcomes
                                .first()
                                .and_then(|o| o.frontmatter.get("files"))
                                .and_then(|v| v.as_sequence())
                                .map(|seq| {
                                    seq.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();

                            let pr_nodes = query::find_pr_merges_for_outcome(
                                &graph_state.nodes,
                                &graph_state.edges,
                                &args.outcome_id,
                                &file_patterns,
                            );
                            if !pr_nodes.is_empty() {
                                md.push_str(&format!(
                                    "\n## PR Merges\n\n{} PR merge(s) serving this outcome\n",
                                    pr_nodes.len()
                                ));
                            }

                            // Append change impact with risk classification
                            if include_impact && !result.code_symbols.is_empty() {
                                let impacted = query::compute_impact_risk(
                                    &result.code_symbols,
                                    &graph_nodes,
                                    &graph_state.index,
                                    3, // max_hops for reverse traversal
                                );
                                md.push('\n');
                                md.push_str(&query::format_impact_markdown(&impacted));
                            } else if include_impact && result.code_symbols.is_empty() {
                                md.push_str("\n## Change Impact\n\nNo changed symbols found -- cannot compute blast radius.\n");
                            }
                         }
                        }

                        Ok(text_result(md))
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            // ── Deprecated aliases: convert to Search and fall through ──
            "search_symbols" => {
                let args: SearchSymbols = parse_args(params.arguments)?;
                let search_args = args.into_search();
                self.handle_search(search_args).await
            }

            "graph_query" => {
                let args: GraphQuery = parse_args(params.arguments)?;
                let search_args = args.into_search();
                self.handle_search(search_args).await
            }

            // ── Unified search tool ──────────────────────────────────────
            "search" => {
                let args: Search = parse_args(params.arguments)?;
                self.handle_search(args).await
            }

            "list_roots" => {
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(self.repo_root.clone())
                    .with_worktrees(&self.repo_root)
                    .with_claude_memory(&self.repo_root);
                let resolved = workspace.resolved_roots();

                if resolved.is_empty() {
                    Ok(text_result("No workspace roots configured.".to_string()))
                } else {
                    let md: String = resolved
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            let primary = if i == 0 { " (primary)" } else { "" };
                            format!(
                                "- **{}**{}: `{}` (type: {}, git: {})",
                                r.slug,
                                primary,
                                r.path.display(),
                                r.config.root_type,
                                r.config.git_aware,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(text_result(format!(
                        "## Workspace Roots\n\n{} root(s)\n\n{}",
                        resolved.len(),
                        md
                    )))
                }
            }

            "repo_map" => {
                let args: RepoMap = parse_args(params.arguments)?;
                let top_n = args.top_n.unwrap_or(15) as usize;
                let root_filter = self.effective_root_filter(args.root.as_deref());
                let non_code_slugs = if root_filter.is_some() {
                    self.non_code_root_slugs()
                } else {
                    std::collections::HashSet::new()
                };

                match self.get_graph().await {
                    Ok(guard) => {
                        let graph_state = guard.as_ref().unwrap();
                        let mut sections: Vec<String> = Vec::new();

                        // 1. Top symbols by importance (weighted PageRank)
                        {
                            let mut symbols_with_importance: Vec<(&Node, f64)> = graph_state.nodes.iter()
                                .filter(|n| !matches!(n.id.kind,
                                    NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field
                                ))
                                .filter(|n| n.id.root != "external")
                                .filter(|n| self.node_passes_root_filter(&n.id.root, &root_filter, &non_code_slugs))
                                .filter_map(|n| {
                                    let imp = n.metadata.get("importance")
                                        .and_then(|s| s.parse::<f64>().ok())
                                        .unwrap_or(0.0);
                                    // Demote test-file symbols so they don't crowd the top
                                    let imp = if ranking::is_test_file(n) { imp * 0.1 } else { imp };
                                    if imp > IMPORTANCE_THRESHOLD { Some((n, imp)) } else { None }
                                })
                                .collect();
                            symbols_with_importance.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                            symbols_with_importance.truncate(top_n);

                            if !symbols_with_importance.is_empty() {
                                let md: String = symbols_with_importance.iter()
                                    .map(|(n, imp)| {
                                        let mut line = format!(
                                            "- **{}** `{}` ({}) [{}] `{}`:{}-{} -- importance: {:.3}",
                                            n.id.kind, n.id.name, n.language,
                                            n.id.root,
                                            n.id.file.display(),
                                            n.line_start, n.line_end,
                                            imp,
                                        );
                                        if let Some(cc) = n.metadata.get("cyclomatic") {
                                            line.push_str(&format!(", complexity: {}", cc));
                                        }
                                        line
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!(
                                    "## Top {} symbols by importance\n\n{}",
                                    symbols_with_importance.len(), md
                                ));
                            }
                        }

                        // 2. Hotspot files (most definitions), qualified by workspace root
                        {
                            let mut file_counts: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
                            for n in &graph_state.nodes {
                                if matches!(n.id.kind, NodeKind::Import | NodeKind::Module | NodeKind::PrMerge | NodeKind::Field) {
                                    continue;
                                }
                                if n.id.root == "external" {
                                    continue;
                                }
                                if !self.node_passes_root_filter(&n.id.root, &root_filter, &non_code_slugs) {
                                    continue;
                                }
                                let key = (n.id.root.clone(), n.id.file.display().to_string());
                                *file_counts.entry(key).or_default() += 1;
                            }
                            let mut sorted_files: Vec<((String, String), usize)> = file_counts.into_iter().collect();
                            sorted_files.sort_by(|a, b| b.1.cmp(&a.1));
                            sorted_files.truncate(10);

                            if !sorted_files.is_empty() {
                                let md: String = sorted_files.iter()
                                    .map(|((root, f), count)| format!("- [{}] `{}` -- {} definitions", root, f, count))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!("## Hotspot files\n\n{}", md));
                            }
                        }

                        // 3. Active outcomes
                        {
                            let outcomes = oh::load_oh_artifacts(root)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|a| a.kind == OhArtifactKind::Outcome)
                                .collect::<Vec<_>>();
                            if !outcomes.is_empty() {
                                let md: String = outcomes.iter()
                                    .map(|o| {
                                        let files: Vec<String> = o.frontmatter
                                            .get("files")
                                            .and_then(|v| v.as_sequence())
                                            .map(|seq| seq.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect())
                                            .unwrap_or_default();
                                        let files_str = if files.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" (files: {})", files.join(", "))
                                        };
                                        format!("- **{}**{}", o.id(), files_str)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!("## Active outcomes\n\n{}", md));
                            }
                        }

                        // 4. Entry points (main functions, handlers), sorted by importance
                        {
                            let mut entry_points: Vec<&Node> = graph_state.nodes.iter()
                                .filter(|n| n.id.kind == NodeKind::Function && n.id.root != "external")
                                .filter(|n| self.node_passes_root_filter(&n.id.root, &root_filter, &non_code_slugs))
                                .filter(|n| {
                                    let name = n.id.name.to_lowercase();
                                    name == "main"
                                        || name.starts_with("handle_")
                                        || name.starts_with("handler")
                                        || name.ends_with("_handler")
                                        || name.contains("endpoint")
                                })
                                .collect();
                            entry_points.sort_by(|a, b| {
                                let imp_a = a.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                let imp_b = b.metadata.get("importance").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                imp_b.partial_cmp(&imp_a).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            entry_points.truncate(10);

                            if !entry_points.is_empty() {
                                let md: String = entry_points.iter()
                                    .map(|n| format!(
                                        "- **{}** [{}] `{}`:{}-{}",
                                        n.id.name,
                                        n.id.root,
                                        n.id.file.display(),
                                        n.line_start, n.line_end,
                                    ))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!("## Entry points\n\n{}", md));
                            }
                        }

                        let freshness = format_freshness(
                            graph_state.nodes.len(),
                            graph_state.last_scan_completed_at,
                            Some(&self.lsp_status),
                        );

                        if sections.is_empty() {
                            Ok(text_result(format!("No repository data available yet.{}", freshness)))
                        } else {
                            Ok(text_result(format!(
                                "# Repository Map\n\n{}{}",
                                sections.join("\n\n"),
                                freshness
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Graph error: {}", e))),
                }
            }

            _ => Err(CallToolError::unknown_tool(&params.name)),
        };

        // Prepend business context preamble to first successful tool result.
        // Only mark as injected after the insert succeeds (compare_exchange
        // guards against concurrent tool calls both injecting).
        if let (Some(preamble), Ok(tool_result)) = (preamble, &mut result) {
            if self.context_injected.compare_exchange(
                false, true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            ).is_ok() {
                tool_result.content.insert(
                    0,
                    TextContent::new(preamble, None, None).into(),
                );
            }
        }

        result
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_graph_detects_file_edits() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn original_function() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        {
            let guard = handler.get_graph().await.unwrap();
            let gs = guard.as_ref().expect("graph should be built");
            assert!(
                gs.nodes.iter().any(|n| n.id.name == "original_function"),
                "original_function should be in graph after first build. Nodes: {:?}",
                gs.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
            );
            assert!(gs.last_scan_completed_at.is_some());
        }

        std::thread::sleep(std::time::Duration::from_millis(1100));

        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn replacement_function() {}\n",
        )
        .unwrap();

        {
            let mut last = handler.last_scan.lock().unwrap();
            *last = std::time::Instant::now() - std::time::Duration::from_secs(10);
        }

        {
            let guard = handler.get_graph().await.unwrap();
            let gs = guard.as_ref().expect("graph should still exist");

            let has_replacement = gs.nodes.iter().any(|n| n.id.name == "replacement_function");
            let has_original = gs.nodes.iter().any(|n| n.id.name == "original_function");

            assert!(has_replacement);
            assert!(!has_original);

            let age = gs.last_scan_completed_at.unwrap().elapsed();
            assert!(age < std::time::Duration::from_secs(5));
        }
    }

    #[tokio::test]
    async fn test_incremental_update_preserves_root_slug() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn initial_func() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        let expected_slug = RootConfig::code_project(root.to_path_buf()).slug();
        assert!(!expected_slug.is_empty());

        {
            let guard = handler.get_graph().await.unwrap();
            let gs = guard.as_ref().expect("graph should be built");
            let node = gs.nodes.iter().find(|n| n.id.name == "initial_func")
                .expect("initial_func should be in graph");
            assert_eq!(node.id.root, expected_slug);
        }

        std::thread::sleep(std::time::Duration::from_millis(1100));

        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn updated_func() {}\n",
        )
        .unwrap();

        {
            let mut last = handler.last_scan.lock().unwrap();
            *last = std::time::Instant::now() - std::time::Duration::from_secs(10);
        }

        {
            let guard = handler.get_graph().await.unwrap();
            let gs = guard.as_ref().expect("graph should still exist");

            let node = gs.nodes.iter().find(|n| n.id.name == "updated_func")
                .expect("updated_func should be in graph after edit");
            assert_eq!(node.id.root, expected_slug);

            for edge in &gs.edges {
                if edge.from.file == std::path::PathBuf::from("src/lib.rs") {
                    assert_eq!(edge.from.root, expected_slug);
                }
            }
        }
    }

    // ── effective_root_filter / node_passes_root_filter tests ──────────

    #[test]
    fn test_effective_root_filter_none_defaults_to_primary() {
        let handler = RnaHandler::default();
        let primary = handler.primary_root_slug();
        let filter = handler.effective_root_filter(None);
        assert_eq!(filter, Some(primary));
    }

    #[test]
    fn test_effective_root_filter_all_returns_none() {
        let handler = RnaHandler::default();
        assert_eq!(handler.effective_root_filter(Some("all")), None);
        assert_eq!(handler.effective_root_filter(Some("ALL")), None);
        assert_eq!(handler.effective_root_filter(Some("All")), None);
    }

    #[test]
    fn test_effective_root_filter_explicit_slug() {
        let handler = RnaHandler::default();
        let filter = handler.effective_root_filter(Some("my-project"));
        assert_eq!(filter, Some("my-project".to_string()));
    }

    #[test]
    fn test_node_passes_root_filter_no_filter() {
        let handler = RnaHandler::default();
        let empty = std::collections::HashSet::new();
        assert!(handler.node_passes_root_filter("any-root", &None, &empty));
    }

    #[test]
    fn test_node_passes_root_filter_matching_slug() {
        let handler = RnaHandler::default();
        let empty = std::collections::HashSet::new();
        let filter = Some("my-project".to_string());
        assert!(handler.node_passes_root_filter("my-project", &filter, &empty));
        assert!(!handler.node_passes_root_filter("other-project", &filter, &empty));
    }

    #[test]
    fn test_node_passes_root_filter_case_insensitive() {
        let handler = RnaHandler::default();
        let empty = std::collections::HashSet::new();
        let filter = Some("My-Project".to_string());
        assert!(handler.node_passes_root_filter("my-project", &filter, &empty));
    }

    #[test]
    fn test_node_passes_root_filter_external_always_passes() {
        let handler = RnaHandler::default();
        let empty = std::collections::HashSet::new();
        let filter = Some("my-project".to_string());
        assert!(handler.node_passes_root_filter("external", &filter, &empty));
    }

    #[test]
    fn test_node_passes_root_filter_non_code_root_passes() {
        let handler = RnaHandler::default();
        let mut non_code = std::collections::HashSet::new();
        non_code.insert("notes-root".to_string());
        let filter = Some("my-project".to_string());
        assert!(handler.node_passes_root_filter("notes-root", &filter, &non_code));
        assert!(!handler.node_passes_root_filter("other-code", &filter, &non_code));
    }

    #[test]
    fn test_effective_root_filter_whitespace_all_is_not_special() {
        let handler = RnaHandler::default();
        let filter = handler.effective_root_filter(Some(" all "));
        assert_eq!(filter, Some(" all ".to_string()));
    }

    #[test]
    fn test_effective_root_filter_empty_string_is_not_all() {
        let handler = RnaHandler::default();
        let filter = handler.effective_root_filter(Some(""));
        assert_eq!(filter, Some("".to_string()));
    }

    #[test]
    fn test_node_passes_root_filter_external_node_with_no_filter() {
        let handler = RnaHandler::default();
        let empty = std::collections::HashSet::new();
        assert!(handler.node_passes_root_filter("external", &None, &empty));
    }

    #[test]
    fn test_node_passes_root_filter_non_code_slug_case_sensitivity() {
        let handler = RnaHandler::default();
        let mut non_code = std::collections::HashSet::new();
        non_code.insert("Notes".to_string());
        let filter = Some("my-project".to_string());
        assert!(handler.node_passes_root_filter("Notes", &filter, &non_code));
        assert!(!handler.node_passes_root_filter("notes", &filter, &non_code));
    }

    // ── search_result_passes_root_filter tests ─────────────────────────

    #[test]
    fn test_search_result_filter_code_result_matches_root() {
        let handler = RnaHandler::default();
        let filter = Some("my-project".to_string());
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "my-project:src/lib.rs:foo:function".to_string(),
            kind: "code:function".to_string(),
            title: "foo".to_string(),
            body: String::new(),
            score: 1.0,
        };
        assert!(handler.search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_code_result_wrong_root() {
        let handler = RnaHandler::default();
        let filter = Some("my-project".to_string());
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "other-project:src/lib.rs:foo:function".to_string(),
            kind: "code:function".to_string(),
            title: "foo".to_string(),
            body: String::new(),
            score: 1.0,
        };
        assert!(!handler.search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_commit_always_passes() {
        let handler = RnaHandler::default();
        let filter = Some("my-project".to_string());
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "abc123".to_string(),
            kind: "commit".to_string(),
            title: "fix: something".to_string(),
            body: String::new(),
            score: 0.8,
        };
        assert!(handler.search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_all_mode_passes_everything() {
        let handler = RnaHandler::default();
        let filter: Option<String> = None;
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "other-project:src/lib.rs:foo:function".to_string(),
            kind: "code:function".to_string(),
            title: "foo".to_string(),
            body: String::new(),
            score: 1.0,
        };
        assert!(handler.search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_non_code_root_passes() {
        let handler = RnaHandler::default();
        let filter = Some("my-project".to_string());
        let mut non_code = std::collections::HashSet::new();
        non_code.insert("claude-memory".to_string());
        let result = crate::embed::SearchResult {
            id: "claude-memory:notes/todo.md:heading:section".to_string(),
            kind: "code:section".to_string(),
            title: "todo".to_string(),
            body: String::new(),
            score: 0.7,
        };
        assert!(handler.search_result_passes_root_filter(&result, &filter, &non_code));
    }

    // ── Stale root pruning tests (#198) ────────────────────────────────

    #[tokio::test]
    async fn test_get_stored_root_ids_empty_when_no_db() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let ids = store::get_stored_root_ids(root).await.unwrap();
        assert!(ids.is_empty());
    }

    // ── parse_node_kind round-trip tests ────────────────────────────────

    #[test]
    fn test_parse_node_kind_field_round_trips() {
        let kind = store::parse_node_kind("field");
        assert!(matches!(kind, NodeKind::Field));
    }

    #[test]
    fn test_parse_node_kind_all_variants_round_trip() {
        let variants = vec![
            NodeKind::Function, NodeKind::Struct, NodeKind::Trait,
            NodeKind::Enum, NodeKind::Module, NodeKind::Import,
            NodeKind::Const, NodeKind::Impl, NodeKind::ProtoMessage,
            NodeKind::SqlTable, NodeKind::ApiEndpoint, NodeKind::Macro,
            NodeKind::Field, NodeKind::PrMerge,
        ];
        for variant in variants {
            let s = format!("{}", variant);
            let parsed = store::parse_node_kind(&s);
            assert_eq!(
                std::mem::discriminant(&variant),
                std::mem::discriminant(&parsed),
            );
        }
    }
}
