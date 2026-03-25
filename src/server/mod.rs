//! MCP server: RnaHandler, graph building, background scanner, and MCP dispatch.
// EXTRACTION_VERSION is deprecated (#526) but still used in tests and log messages.
#![allow(deprecated)]

pub mod tools;
pub mod store;
pub mod state;
pub mod handlers;
pub mod helpers;
pub mod sentinel;
mod graph;
mod enrichment;

// Re-export subsystem metadata key for service layer filtering.
pub(crate) use graph::SUBSYSTEM_KEY;

// Re-export public API so external `use crate::server::X` still works.
pub use tools::*;
pub use store::{load_graph_from_lance, parse_edge_kind};
pub(crate) use store::{
    check_and_migrate_schema, graph_lance_path,
    persist_graph_incremental, persist_graph_to_lance,
};
pub use state::{EmbeddingStatus, GraphBuildState, GraphBuildStatus, GraphState, LspEnrichmentStatus, LspState};
pub use helpers::format_freshness;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CallToolError, CallToolRequestParams, CallToolResult, ListToolsResult,
    PaginatedRequestParams, RpcError, TextContent,
};

use crate::embed::EmbeddingIndex;
#[cfg(test)]
use crate::graph::NodeKind;
use crate::types::OhArtifactKind;
use crate::oh;
use arc_swap::ArcSwap;

use helpers::parse_args;

// ── Pipeline result ─────────────────────────────────────────────────

/// Result of a full pipeline run (used by `--full` CLI mode).
pub struct PipelineResult {
    pub node_count: usize,
    pub edge_count: usize,
    pub file_count: usize,
    pub lsp_edge_count: usize,
    pub embed_count: usize,
    pub total_time: std::time::Duration,
    pub lsp_entries: Vec<crate::extract::scan_stats::LspEnrichmentEntry>,
    pub encoding_stats: crate::extract::EncodingStats,
}

impl PipelineResult {
    /// Format a structured scan summary for display.
    pub fn format_summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Scan complete: {} symbols, {} edges, {} files in {:.1}s",
            self.node_count, self.edge_count, self.file_count, self.total_time.as_secs_f64()));
        if !self.lsp_entries.is_empty() {
            lines.push(String::new());
            lines.push("LSP enrichment:".to_string());
            for entry in &self.lsp_entries { lines.push(entry.summary_line()); }
        }
        if !self.encoding_stats.is_empty() {
            lines.push(String::new());
            lines.push(format!("Encoding: {} lossy-decoded, {} binary-skipped",
                self.encoding_stats.lossy_decoded, self.encoding_stats.binary_skipped));
        }
        lines.join("\n")
    }
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
    /// Lock-free graph state via ArcSwap (#574).
    /// Readers (tool calls) load an atomic snapshot — zero blocking.
    /// Writers (pre-warm, background scanner) build a new graph and swap the pointer.
    pub graph: Arc<ArcSwap<Option<Arc<GraphState>>>>,
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
    /// Embedding build status — shared with background embedding tasks.
    pub embed_status: Arc<EmbeddingStatus>,
    /// Cached non-code root slugs (computed once, cleared on root changes).
    pub non_code_root_slugs_cache: std::sync::Mutex<Option<std::collections::HashSet<String>>>,
    /// Serializes all LanceDB writes to prevent concurrent merge_insert conflicts.
    /// Background enrichment and scanner-triggered incremental updates both write
    /// to the same LanceDB tables; concurrent writes cause "conflict" errors (#344).
    pub lance_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Subdirectory roots that are `lsp_only` — used as LSP working directories
    /// without re-extracting their files (already covered by the primary root scan).
    /// Populated from `[workspace.roots]` entries whose paths are subdirectories of `repo_root`.
    /// Each entry is `(slug, absolute_path)`.
    pub lsp_only_roots: Arc<Vec<(String, PathBuf)>>,
    /// Live scan state maintained by `ScanStatsConsumer`.
    /// Populated during active scans; empty/default until the first `RootDiscovered` event.
    /// `list_roots` reads from this for in-progress status; falls back to sentinel files
    /// on cold start when no bus events have fired for the current process lifetime.
    pub scan_stats: Arc<std::sync::RwLock<crate::extract::scan_stats::ScanStats>>,
    /// JoinHandle for the background embedding task spawned by `spawn_background_enrichment`.
    /// CLI callers can await this to ensure embeddings complete before the runtime shuts down.
    /// MCP server callers leave it as fire-and-forget (the handle is dropped on next scan).
    pub embed_handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Notifies waiters when the pre-warm task completes (success or failure).
    /// `get_graph()` listens on this to avoid starting a duplicate build while
    /// pre-warm is in progress.
    pub prewarm_notify: Arc<tokio::sync::Notify>,
    /// Whether a pre-warm task has been started.
    pub prewarm_started: std::sync::atomic::AtomicBool,
    /// Serializes graph build/update operations in `get_graph()`.
    /// With ArcSwap, reads are lock-free but concurrent builds must be serialized
    /// to avoid duplicate work (two tool calls both detecting "no graph" and both
    /// running the full extraction pipeline).
    pub graph_build_lock: Arc<tokio::sync::Mutex<()>>,
    pub graph_build_status: Arc<GraphBuildStatus>,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            graph: Arc::new(ArcSwap::from_pointee(None)),
            embed_index: Arc::new(ArcSwap::from_pointee(None)),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            last_scan: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
            background_scanner_started: std::sync::atomic::AtomicBool::new(false),
            lsp_status: Arc::new(LspEnrichmentStatus::probe_for_servers()),
            embed_status: Arc::new(EmbeddingStatus::default()),
            non_code_root_slugs_cache: std::sync::Mutex::new(None),
            lance_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            lsp_only_roots: Arc::new(Vec::new()),
            scan_stats: Arc::new(std::sync::RwLock::new(
                crate::extract::scan_stats::ScanStats::default(),
            )),
            embed_handle: tokio::sync::Mutex::new(None),
            prewarm_notify: Arc::new(tokio::sync::Notify::new()),
            prewarm_started: std::sync::atomic::AtomicBool::new(false),
            graph_build_lock: Arc::new(tokio::sync::Mutex::new(())),
            graph_build_status: Arc::new(GraphBuildStatus::default()),
        }
    }
}

impl RnaHandler {
    /// Create a clone of this handler that shares all `Arc`-wrapped state.
    ///
    /// Used by `start_prewarm()` to spawn a background graph build task that
    /// writes to the same `graph` ArcSwap as the MCP handler.  Non-Arc fields
    /// (AtomicBool, Mutex) are freshly initialized because the spawned task
    /// only needs the shared storage, not the per-request bookkeeping.
    fn clone_shared(&self) -> Self {
        Self {
            repo_root: self.repo_root.clone(),
            graph: Arc::clone(&self.graph),
            embed_index: Arc::clone(&self.embed_index),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            last_scan: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
            background_scanner_started: std::sync::atomic::AtomicBool::new(false),
            lsp_status: Arc::clone(&self.lsp_status),
            embed_status: Arc::clone(&self.embed_status),
            non_code_root_slugs_cache: std::sync::Mutex::new(None),
            lance_write_lock: Arc::clone(&self.lance_write_lock),
            lsp_only_roots: Arc::clone(&self.lsp_only_roots),
            scan_stats: Arc::clone(&self.scan_stats),
            embed_handle: tokio::sync::Mutex::new(None),
            prewarm_notify: Arc::clone(&self.prewarm_notify),
            prewarm_started: std::sync::atomic::AtomicBool::new(false),
            graph_build_lock: Arc::clone(&self.graph_build_lock),
            graph_build_status: Arc::clone(&self.graph_build_status),
        }
    }

    /// Spawn a background task that pre-warms the graph immediately after
    /// MCP initialization, so the first tool call finds the graph already
    /// built (or nearly so).  Called from `on_initialized`.
    ///
    /// If a LanceDB cache exists and no files changed, this completes in
    /// seconds.  If the cache is cold, this runs the full extraction
    /// pipeline in the background.  Either way, `get_graph()` will find
    /// the graph populated when the first tool call arrives.
    fn start_prewarm(&self) {
        self.prewarm_started.store(true, std::sync::atomic::Ordering::Relaxed);
        self.graph_build_status.set_building(0);
        let handler = self.clone_shared();
        let notify = Arc::clone(&self.prewarm_notify);
        let build_status = Arc::clone(&self.graph_build_status);
        tokio::spawn(async move {
            tracing::info!("Pre-warming graph index in background...");
            let t0 = std::time::Instant::now();
            match handler.build_full_graph().await {
                Ok(state) => {
                    let node_count = state.nodes.len();
                    let edge_count = state.edges.len();
                    // Atomic swap: store the built graph so get_graph() finds it
                    // already populated.  If another path already stored a graph,
                    // the pre-warm result wins (it's always a complete build).
                    handler.graph.store(Arc::new(Some(Arc::new(state))));
                    // Update cooldown so the first tool call skips re-scanning.
                    *handler.last_scan.lock().unwrap() = std::time::Instant::now();
                    // Start the background scanner to keep the index warm.
                    if !handler.background_scanner_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        handler.spawn_background_scanner();
                    }
                    build_status.set_ready();
                    tracing::info!(
                        "Graph pre-warm complete: {} symbols, {} edges in {:.2}s",
                        node_count,
                        edge_count,
                        t0.elapsed().as_secs_f64(),
                    );
                }
                Err(e) => {
                    build_status.set_failed(format!("{}", e));
                    tracing::warn!("Graph pre-warm failed (will retry on first tool call): {}", e);
                }
            }
            // Wake any tool calls waiting for pre-warm to finish.
            notify.notify_waiters();
        });
    }

    /// Await the background embedding task, if one was spawned.
    ///
    /// CLI callers should invoke this before returning so the Tokio runtime
    /// does not shut down while the embedding task is still in flight (which
    /// would cause a `JoinError::Cancelled` panic in LanceDB internals).
    ///
    /// MCP server callers do not need this — the runtime persists for the
    /// lifetime of the server process.
    pub async fn await_background_embed(&self) {
        let handle = self.embed_handle.lock().await.take();
        if let Some(h) = handle {
            match h.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {
                    tracing::warn!(
                        "Background embedding task was cancelled (runtime shutting down)"
                    );
                }
                Err(e) => {
                    tracing::warn!("Background embedding task failed: {}", e);
                }
            }
        }
    }

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

    pub(crate) fn cold_start_building_message(&self) -> Option<String> {
        let current = self.graph.load_full();
        if current.is_some() { return None; }
        self.graph_build_status.status_message()
    }

    /// Return the set of root slugs that correspond to non-code roots
    /// (Notes, General, Custom). These should always be included in
    /// query results regardless of root filtering.
    ///
    /// Results are cached to avoid repeated filesystem/config reads per tool call.
    pub(crate) fn non_code_root_slugs(&self) -> std::collections::HashSet<String> {
        let mut cache = self.non_code_root_slugs_cache.lock().unwrap();
        if let Some(ref cached) = *cache {
            return cached.clone();
        }
        use crate::roots::{RootType, WorkspaceConfig};
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root)
            .with_agent_memories(&self.repo_root)
            .with_declared_roots(&self.repo_root);
        let slugs: std::collections::HashSet<String> = workspace
            .resolved_roots()
            .into_iter()
            .filter(|r| r.config.root_type != RootType::CodeProject)
            .map(|r| r.slug)
            .collect();
        *cache = Some(slugs.clone());
        slugs
    }

    /// Invalidate the non-code root slugs cache (e.g., when roots change).
    pub(crate) fn invalidate_non_code_root_slugs_cache(&self) {
        *self.non_code_root_slugs_cache.lock().unwrap() = None;
    }

    /// Check whether a node passes the root filter.
    /// Delegates to the canonical implementation in `crate::service`.
    #[cfg(test)]
    pub(crate) fn node_passes_root_filter(
        &self,
        node_root: &str,
        root_filter: &Option<String>,
        non_code_slugs: &std::collections::HashSet<String>,
    ) -> bool {
        crate::service::node_passes_root_filter(node_root, root_filter, non_code_slugs)
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
    out.push_str("**Code exploration:** use `search` (not Grep/Read) for code symbols, artifacts, commits, and markdown in one call. Use `search(node: \"<id>\", mode: \"neighbors\")` for graph traversal.\n");
    out.push_str("---\n\n");
    out
}

#[async_trait]
impl rust_mcp_sdk::mcp_server::ServerHandler for RnaHandler {
    /// Called after the MCP client sends the `initialized` notification.
    /// Spawns a background task to pre-warm the graph so the first tool
    /// call doesn't block on a full extraction pipeline.
    async fn on_initialized(&self, _runtime: Arc<dyn McpServer>) {
        self.start_prewarm();
    }

    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: vec![
                OutcomeProgress::tool(),
                Search::tool(),
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

        if params.name != "list_roots" {
            if let Some(building_msg) = self.cold_start_building_message() {
                return Ok(helpers::text_result(building_msg));
            }
        }

        let mut result = match params.name.as_str() {
            "outcome_progress" => {
                let args: OutcomeProgress = parse_args(params.arguments)?;
                self.handle_outcome_progress(args).await
            }

            "search" => {
                let args: Search = parse_args(params.arguments)?;
                self.handle_search(args).await
            }

            "list_roots" => {
                let _args: ListRoots = parse_args(params.arguments)?;
                self.handle_list_roots().await
            }

            "repo_map" => {
                let args: RepoMap = parse_args(params.arguments)?;
                self.handle_repo_map(args).await
            }

            _ => Err(CallToolError::unknown_tool(&params.name)),
        };

        // Prepend business context preamble to first successful tool result.
        // Only mark as injected after the insert succeeds (compare_exchange
        // guards against concurrent tool calls both injecting).
        if let (Some(preamble), Ok(tool_result)) = (preamble, &mut result)
            && self.context_injected.compare_exchange(
                false, true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            ).is_ok() {
                tool_result.content.insert(
                    0,
                    TextContent::new(preamble, None, None).into(),
                );
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
            let gs = handler.get_graph().await.unwrap();
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
            let gs = handler.get_graph().await.unwrap();

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
        use crate::roots::RootConfig;

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
            let gs = handler.get_graph().await.unwrap();
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
            let gs = handler.get_graph().await.unwrap();

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

    /// Adversarial test: run_pipeline_foreground uses incremental path on second run.
    /// Seeded from dissent: verify changed files are detected and old symbols removed.
    #[tokio::test]
    async fn test_foreground_pipeline_incremental_on_second_run() {
        use tempfile::TempDir;
        use std::sync::Mutex;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn alpha_function() {}\npub fn beta_function() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // First run: full rebuild (no cache).
        let progress: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let p = progress.clone();
        let result = handler
            .run_pipeline_foreground(move |msg| {
                p.lock().unwrap().push(msg.to_string());
            })
            .await
            .expect("first foreground pipeline should succeed");

        assert!(result.node_count > 0, "should have extracted nodes");
        // Verify alpha and beta are in the graph.
        {
            let snap = handler.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            assert!(gs.nodes.iter().any(|n| n.id.name == "alpha_function"));
            assert!(gs.nodes.iter().any(|n| n.id.name == "beta_function"));
        }

        // Verify the first run used the full rebuild path (no "Loaded cached graph" message).
        let msgs = progress.lock().unwrap();
        assert!(
            !msgs.iter().any(|m| m.contains("Loaded cached graph")),
            "first run should NOT use cached graph"
        );
        drop(msgs);

        // Sleep so mtime changes are detected.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Modify: replace beta with gamma.
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn alpha_function() {}\npub fn gamma_function() {}\n",
        )
        .unwrap();

        // Second run: should use incremental path.
        // Need a fresh handler because the graph lock is held.
        let handler2 = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        let progress2: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let p2 = progress2.clone();
        let result2 = handler2
            .run_pipeline_foreground(move |msg| {
                p2.lock().unwrap().push(msg.to_string());
            })
            .await
            .expect("second foreground pipeline should succeed");

        assert!(result2.node_count > 0, "should have nodes after incremental");

        // Verify the second run used the incremental path.
        let msgs2 = progress2.lock().unwrap();
        assert!(
            msgs2.iter().any(|m| m.contains("Loaded cached graph")),
            "second run should use cached graph. Messages: {:?}",
            *msgs2
        );

        // Verify gamma replaced beta.
        {
            let snap = handler2.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            assert!(
                gs.nodes.iter().any(|n| n.id.name == "alpha_function"),
                "alpha_function should still be present"
            );
            assert!(
                gs.nodes.iter().any(|n| n.id.name == "gamma_function"),
                "gamma_function should be added by incremental update"
            );
            assert!(
                !gs.nodes.iter().any(|n| n.id.name == "beta_function"),
                "beta_function should be removed by incremental update"
            );
        }
    }

    /// Regression test: bumping EXTRACTION_VERSION must force full re-extraction even on
    /// the foreground incremental path (run_pipeline_foreground with a cached LanceDB graph).
    ///
    /// The bug was that run_pipeline_foreground_incremental called check_and_migrate_schema
    /// but NOT check_and_migrate_extraction_version, so a version bump went undetected and
    /// the scan reported "incremental, no changes".
    #[tokio::test]
    async fn test_extraction_version_bump_forces_full_rebuild_on_incremental_path() {
        use tempfile::TempDir;
        use std::sync::Mutex;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn stable_function() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // First run: full rebuild, writes EXTRACTION_VERSION to the version file.
        let result1 = handler
            .run_pipeline_foreground(|_| {})
            .await
            .expect("first run should succeed");
        assert!(result1.node_count > 0, "should have extracted nodes on first run");

        // Verify the extraction_version file was written.
        let version_file = root.join(".oh").join(".cache").join("lance").join("extraction_version");
        assert!(version_file.exists(), "extraction_version file should exist after first run");

        // Simulate a version bump: overwrite the stored version with a stale value.
        // This mimics what happens when a new binary with a higher EXTRACTION_VERSION
        // is run against a repo whose cache was built with an older version.
        std::fs::write(&version_file, "0").unwrap();

        // Second run: should detect the version mismatch and take the full rebuild path.
        let handler2 = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        let progress2: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let p2 = progress2.clone();
        let result2 = handler2
            .run_pipeline_foreground(move |msg| {
                p2.lock().unwrap().push(msg.to_string());
            })
            .await
            .expect("second run should succeed after extraction version migration");

        assert!(result2.node_count > 0, "should have nodes after version bump rebuild");

        let msgs = progress2.lock().unwrap();

        // The second run must enter the incremental path (LanceDB cache was written by first run).
        assert!(
            msgs.iter().any(|m| m.contains("Loaded cached graph")),
            "second run should start from cached graph (incremental entry). Messages: {:?}",
            *msgs
        );

        // Must NOT report "no changes" -- that was the bug.
        assert!(
            !msgs.iter().any(|m| m.contains("incremental, no changes")),
            "should NOT report 'incremental, no changes' after extraction version bump. Messages: {:?}",
            *msgs
        );

        // Must report the extraction version upgrade message.
        assert!(
            msgs.iter().any(|m| m.contains("Extraction version upgrade detected")),
            "should report extraction version upgrade. Messages: {:?}",
            *msgs
        );

        // Verify the version file now holds the current EXTRACTION_VERSION.
        let stored: u32 = std::fs::read_to_string(&version_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            stored,
            crate::graph::store::EXTRACTION_VERSION,
            "extraction_version file should be updated to current EXTRACTION_VERSION"
        );
    }

    /// Regression test: bumping SCHEMA_VERSION must force a full rebuild even on the
    /// foreground incremental path (run_pipeline_foreground with a cached LanceDB graph).
    ///
    /// After a schema version bump (e.g. 16→17), the next `scan` without `--full` must
    /// auto-detect the mismatch in `run_pipeline_foreground_incremental`, drop the stale
    /// LanceDB tables, and fall back to `run_pipeline_foreground_full`.  Without this,
    /// the incremental path loads the now-empty tables and returns a partial graph.
    ///
    /// Mirrors `test_extraction_version_bump_forces_full_rebuild_on_incremental_path`
    /// which covers the EXTRACTION_VERSION case (#452).
    #[tokio::test]
    async fn test_schema_version_bump_forces_full_rebuild_on_incremental_path() {
        use tempfile::TempDir;
        use std::sync::Mutex;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn stable_function() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // First run: full rebuild, writes SCHEMA_VERSION to the version file.
        let result1 = handler
            .run_pipeline_foreground(|_| {})
            .await
            .expect("first run should succeed");
        assert!(result1.node_count > 0, "should have extracted nodes on first run");

        // Verify the schema_version file was written.
        let schema_version_file = root.join(".oh").join(".cache").join("lance").join("schema_version");
        assert!(schema_version_file.exists(), "schema_version file should exist after first run");

        // Simulate a schema version bump: overwrite the stored version with a stale value.
        // This mimics what happens when a new binary with a higher SCHEMA_VERSION is run
        // against a repo whose LanceDB cache was built with an older schema.
        std::fs::write(&schema_version_file, "0").unwrap();

        // Second run: should detect the version mismatch in the incremental pre-flight,
        // drop all LanceDB tables, and fall back to run_pipeline_foreground_full.
        let handler2 = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        let progress2: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let p2 = progress2.clone();
        let result2 = handler2
            .run_pipeline_foreground(move |msg| {
                p2.lock().unwrap().push(msg.to_string());
            })
            .await
            .expect("second run should succeed after schema version migration");

        assert!(result2.node_count > 0, "should have nodes after schema version bump rebuild");

        let msgs = progress2.lock().unwrap();

        // The second run must enter the incremental path (LanceDB cache existed from first run).
        assert!(
            msgs.iter().any(|m| m.contains("Loaded cached graph")),
            "second run should start from cached graph (incremental entry). Messages: {:?}",
            *msgs
        );

        // Must report the schema migration message.
        assert!(
            msgs.iter().any(|m| m.contains("Schema migration detected")),
            "should report schema migration. Messages: {:?}",
            *msgs
        );

        // Verify the schema_version file now holds the current SCHEMA_VERSION.
        let stored: u32 = std::fs::read_to_string(&schema_version_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            stored,
            crate::graph::store::SCHEMA_VERSION,
            "schema_version file should be updated to current SCHEMA_VERSION"
        );
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
        let filter = Some("my-project".to_string());
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "my-project:src/lib.rs:foo:function".to_string(),
            kind: "code:function".to_string(),
            title: "foo".to_string(),
            body: String::new(),
            score: 1.0,
        };
        assert!(crate::service::search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_code_result_wrong_root() {
        let filter = Some("my-project".to_string());
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "other-project:src/lib.rs:foo:function".to_string(),
            kind: "code:function".to_string(),
            title: "foo".to_string(),
            body: String::new(),
            score: 1.0,
        };
        assert!(!crate::service::search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_commit_always_passes() {
        let filter = Some("my-project".to_string());
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "abc123".to_string(),
            kind: "commit".to_string(),
            title: "fix: something".to_string(),
            body: String::new(),
            score: 0.8,
        };
        assert!(crate::service::search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_all_mode_passes_everything() {
        let filter: Option<String> = None;
        let non_code = std::collections::HashSet::new();
        let result = crate::embed::SearchResult {
            id: "other-project:src/lib.rs:foo:function".to_string(),
            kind: "code:function".to_string(),
            title: "foo".to_string(),
            body: String::new(),
            score: 1.0,
        };
        assert!(crate::service::search_result_passes_root_filter(&result, &filter, &non_code));
    }

    #[test]
    fn test_search_result_filter_non_code_root_passes() {
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
        assert!(crate::service::search_result_passes_root_filter(&result, &filter, &non_code));
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
            NodeKind::Enum, NodeKind::TypeAlias, NodeKind::Module,
            NodeKind::Import, NodeKind::Const, NodeKind::Impl,
            NodeKind::ProtoMessage, NodeKind::SqlTable,
            NodeKind::ApiEndpoint, NodeKind::Macro,
            NodeKind::Field, NodeKind::PrMerge,
            NodeKind::EnumVariant, NodeKind::MarkdownSection,
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

    /// Regression test for #364: a newly-declared workspace root whose scanner
    /// state was already committed (so no file-change delta) but whose nodes had
    /// never been persisted to LanceDB must still be included in the next rebuild.
    ///
    /// The bug: `any_root_changed` was only set by file-change detection. A root
    /// with committed scanner state but absent from LanceDB reported 0 changes,
    /// keeping `any_root_changed = false` and triggering the early-return that
    /// loaded the stale cache without the new root's nodes.
    ///
    /// The fix: compare live slugs against stored LanceDB slugs at startup;
    /// if any slug is missing, pre-set `any_root_changed = true` to force a
    /// full rebuild that includes the new root.
    #[tokio::test]
    async fn test_declared_root_persisted_even_when_scanner_state_already_committed() {
        use tempfile::TempDir;

        let primary = TempDir::new().unwrap();
        let secondary = TempDir::new().unwrap();

        // Primary root: one Rust file
        std::fs::create_dir_all(primary.path().join("src")).unwrap();
        std::fs::create_dir_all(primary.path().join(".oh/.cache")).unwrap();
        std::fs::write(
            primary.path().join("src/lib.rs"),
            "pub fn primary_fn() {}\n",
        )
        .unwrap();

        // Secondary root: one Rust file (will be declared later)
        std::fs::create_dir_all(secondary.path().join("src")).unwrap();
        std::fs::write(
            secondary.path().join("src/lib.rs"),
            "pub fn secondary_fn() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: primary.path().to_path_buf(),
            ..Default::default()
        };

        // Step 1: First build without declared root. Persists primary root to LanceDB.
        let gs1 = handler.build_full_graph().await.unwrap();
        assert!(
            gs1.nodes.iter().any(|n| n.id.name == "primary_fn"),
            "primary_fn should be present after first build"
        );
        assert!(
            !gs1.nodes.iter().any(|n| n.id.name == "secondary_fn"),
            "secondary_fn must not appear before the secondary root is declared"
        );

        // Step 2: Pre-commit the scanner state for the secondary root so that a
        // subsequent scan sees 0 file changes for it — this simulates the exact
        // scenario from #364 where the state was committed on a previous run.
        let state_path = crate::roots::cache_state_path("secondary");
        if let Some(parent) = state_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        {
            // Build and commit scanner state so secondary shows 0 changes next scan.
            let mut scanner = crate::scanner::Scanner::with_excludes_and_state_path(
                secondary.path().to_path_buf(),
                vec![],
                state_path.clone(),
            )
            .unwrap();
            let _ = scanner.scan().unwrap();
            scanner.commit_state().unwrap();
        }

        // Step 3: Declare the secondary root in .oh/config.toml
        let config_dir = primary.path().join(".oh");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            format!("[workspace.roots]\nsecondary = \"{}\"\n", secondary.path().display()),
        )
        .unwrap();

        // Reset handler graph so next call triggers a fresh build_full_graph
        {
            handler.graph.store(Arc::new(None));
        }

        // Step 4: Second build. Secondary slug is in live_slugs but not in stored
        // LanceDB slugs (only primary was persisted). The fix must detect this,
        // set any_root_changed = true, and include secondary_fn in the rebuilt graph.
        let gs2 = handler.build_full_graph().await.unwrap();
        assert!(
            gs2.nodes.iter().any(|n| n.id.name == "primary_fn"),
            "primary_fn must still be present after second build"
        );
        assert!(
            gs2.nodes.iter().any(|n| n.id.name == "secondary_fn"),
            "secondary_fn must be present after second build with declared root. \
            Nodes: {:?}",
            gs2.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );

        // Step 5: Verify LanceDB was updated: load fresh from lance and check
        let persisted = crate::server::load_graph_from_lance(primary.path()).await.unwrap();
        assert!(
            persisted.nodes.iter().any(|n| n.id.name == "secondary_fn"),
            "secondary_fn must be in LanceDB after second build. \
            Stored nodes: {:?}",
            persisted.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );

        // Cleanup: remove scanner state we wrote so other tests aren't affected
        let _ = std::fs::remove_file(&state_path);
    }

    /// Regression test for #364 (round 3): newly-declared workspace root must persist
    /// even when each scan creates a fresh handler (the CLI `scan --repo .` pattern).
    ///
    /// The existing test `test_declared_root_persisted_even_when_scanner_state_already_committed`
    /// covers the same-handler case. This test covers the fresh-handler case that matches
    /// actual CLI usage: each invocation is a separate process with its own `RnaHandler`.
    ///
    /// This ensures the fix works not just within a single server session but across
    /// repeated `scan` commands — the production scenario that triggered the re-open.
    #[tokio::test]
    async fn test_declared_root_persists_across_fresh_handler_scans() {
        use tempfile::TempDir;

        let primary = TempDir::new().unwrap();
        let secondary = TempDir::new().unwrap();

        // Primary root: one Rust file
        std::fs::create_dir_all(primary.path().join("src")).unwrap();
        std::fs::create_dir_all(primary.path().join(".oh/.cache")).unwrap();
        std::fs::write(
            primary.path().join("src/lib.rs"),
            "pub fn primary_fn() {}\n",
        ).unwrap();

        // Secondary root: one Rust file
        std::fs::create_dir_all(secondary.path().join("src")).unwrap();
        std::fs::write(
            secondary.path().join("src/lib.rs"),
            "pub fn secondary_fn() {}\n",
        ).unwrap();

        // Step 1: First build with fresh handler #1 -- no secondary declared.
        {
            let handler = RnaHandler {
                repo_root: primary.path().to_path_buf(),
                ..Default::default()
            };
            let gs1 = handler.build_full_graph().await.unwrap();
            assert!(gs1.nodes.iter().any(|n| n.id.name == "primary_fn"));
            assert!(!gs1.nodes.iter().any(|n| n.id.name == "secondary_fn"));
        }
        // Handler #1 dropped here -- simulates CLI process exit.

        // Step 2: Pre-commit scanner state for secondary (simulates a previous scan
        // that committed state but didn't persist nodes -- the #364 scenario).
        let state_path = crate::roots::cache_state_path("secondary-fresh");
        // Ensure cleanup runs even if the test panics.
        struct CleanupFile(std::path::PathBuf);
        impl Drop for CleanupFile {
            fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
        }
        let _cleanup = CleanupFile(state_path.clone());
        if let Some(parent) = state_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        {
            let mut scanner = crate::scanner::Scanner::with_excludes_and_state_path(
                secondary.path().to_path_buf(),
                vec![],
                state_path.clone(),
            ).unwrap();
            let _ = scanner.scan().unwrap();
            scanner.commit_state().unwrap();
        }

        // Step 3: Declare secondary in config.
        let config_dir = primary.path().join(".oh");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            format!("[workspace.roots]\nsecondary-fresh = \"{}\"\n", secondary.path().display()),
        ).unwrap();

        // Step 4: Second build with fresh handler #2 -- should persist secondary.
        {
            let handler2 = RnaHandler {
                repo_root: primary.path().to_path_buf(),
                ..Default::default()
            };
            let gs2 = handler2.build_full_graph().await.unwrap();
            assert!(
                gs2.nodes.iter().any(|n| n.id.name == "secondary_fn"),
                "secondary_fn must be in-memory after second build with fresh handler"
            );
        }
        // Handler #2 dropped -- simulates CLI process exit.

        // Verify LanceDB was updated.
        let persisted2 = crate::server::load_graph_from_lance(primary.path()).await.unwrap();
        assert!(
            persisted2.nodes.iter().any(|n| n.id.name == "secondary_fn"),
            "secondary_fn must be in LanceDB after second build (fresh handler). \
             Stored: {:?}",
            persisted2.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );

        // Step 5: Third build with fresh handler #3 -- simulates "every scan" scenario.
        // This verifies the fix is stable: secondary is already in LanceDB, so
        // `has_new_root = false` and no spurious "forcing full rebuild" message.
        {
            let handler3 = RnaHandler {
                repo_root: primary.path().to_path_buf(),
                ..Default::default()
            };
            let gs3 = handler3.build_full_graph().await.unwrap();
            assert!(
                gs3.nodes.iter().any(|n| n.id.name == "secondary_fn"),
                "secondary_fn must still be present on third build (no regression)"
            );
        }

        // Verify LanceDB still has secondary after the third build.
        let persisted3 = crate::server::load_graph_from_lance(primary.path()).await.unwrap();
        assert!(
            persisted3.nodes.iter().any(|n| n.id.name == "secondary_fn"),
            "secondary_fn must remain in LanceDB after third build. \
             Stored: {:?}",
            persisted3.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );

        // Cleanup runs automatically when _cleanup is dropped (end of test scope).
    }

    /// Regression test for #453: ApiEndpoint nodes produced by `nextjs_routing_pass`
    /// for an lsp_only subdirectory root must survive subsequent scans.
    ///
    /// The bug: `live_slugs` excluded lsp_only roots. Stale pruning filtered stored
    /// root IDs against `live_slugs`, so the lsp_only slug (e.g. "client") was always
    /// treated as a removed worktree and deleted on every scan.
    ///
    /// The fix: stale pruning uses `all_declared_slugs` (includes lsp_only) to avoid
    /// deleting nodes produced by post-extraction passes like `nextjs_routing_pass`.
    #[tokio::test]
    async fn test_lsp_only_root_api_endpoint_nodes_survive_rescan() {
        use tempfile::TempDir;

        let primary = TempDir::new().unwrap();
        let client_dir = primary.path().join("client");

        // Primary root: one Rust file (required so the scanner has something to extract)
        std::fs::create_dir_all(primary.path().join("src")).unwrap();
        std::fs::write(
            primary.path().join("src/lib.rs"),
            "pub fn primary_fn() {}\n",
        )
        .unwrap();

        // lsp_only subdirectory root: a Next.js Pages Router API route.
        // pages/api/health.ts → ApiEndpoint node "ANY /api/health".
        // _app.tsx imports from 'next/app' so framework_detection_pass fires
        // FrameworkDetected("nextjs-app-router"), which gates nextjs_routing_pass.
        std::fs::create_dir_all(client_dir.join("pages/api")).unwrap();
        std::fs::write(
            client_dir.join("pages/api/health.ts"),
            "export default function handler(req: any, res: any) { res.json({ ok: true }); }\n",
        )
        .unwrap();
        std::fs::write(
            client_dir.join("pages/_app.tsx"),
            "import type { AppProps } from 'next/app';\nexport default function App({ Component, pageProps }: AppProps) { return <Component {...pageProps} />; }\n",
        )
        .unwrap();

        // Declare the client subdirectory as an lsp_only root in config.toml
        let config_dir = primary.path().join(".oh");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            format!(
                "[workspace.roots]\nclient = \"{}\"\n",
                client_dir.display()
            ),
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: primary.path().to_path_buf(),
            ..Default::default()
        };

        // First build: nextjs_routing_pass should produce ApiEndpoint with root="client"
        let gs1 = handler.build_full_graph().await.unwrap();
        let has_endpoint_first = gs1
            .nodes
            .iter()
            .any(|n| n.id.kind == crate::graph::NodeKind::ApiEndpoint && n.id.root == "client");
        assert!(
            has_endpoint_first,
            "First build must include ApiEndpoint from lsp_only client root. \
            Nodes: {:?}",
            gs1.nodes
                .iter()
                .filter(|n| n.id.kind == crate::graph::NodeKind::ApiEndpoint)
                .map(|n| (&n.id.root, &n.id.name))
                .collect::<Vec<_>>()
        );

        // Force a second build (simulate rescan)
        {
            handler.graph.store(Arc::new(None));
        }
        let gs2 = handler.build_full_graph().await.unwrap();
        let has_endpoint_second = gs2
            .nodes
            .iter()
            .any(|n| n.id.kind == crate::graph::NodeKind::ApiEndpoint && n.id.root == "client");
        assert!(
            has_endpoint_second,
            "Second build (rescan) must still include ApiEndpoint from lsp_only client root. \
            Stale pruning must NOT delete nodes for declared lsp_only roots. \
            Nodes: {:?}",
            gs2.nodes
                .iter()
                .filter(|n| n.id.kind == crate::graph::NodeKind::ApiEndpoint)
                .map(|n| (&n.id.root, &n.id.name))
                .collect::<Vec<_>>()
        );
    }

    /// Adversarial test for #453 fix: removing an lsp_only root from config must still
    /// cause its LanceDB nodes to be pruned on the next scan.
    ///
    /// Verifies that `all_declared_slugs` (used for stale pruning) is rebuilt from
    /// `resolved_roots` each scan, so a removed lsp_only root's slug falls out of
    /// `all_declared_slugs` and its nodes are correctly treated as stale.
    #[tokio::test]
    async fn test_removed_lsp_only_root_nodes_get_pruned() {
        use tempfile::TempDir;

        let primary = TempDir::new().unwrap();
        let client_dir = primary.path().join("client");

        // Primary root: one Rust file
        std::fs::create_dir_all(primary.path().join("src")).unwrap();
        std::fs::write(
            primary.path().join("src/lib.rs"),
            "pub fn primary_fn() {}\n",
        )
        .unwrap();

        // lsp_only subdirectory root: a Next.js Pages Router API route.
        // _app.tsx imports from 'next/app' so framework_detection_pass fires
        // FrameworkDetected("nextjs-app-router"), which gates nextjs_routing_pass.
        std::fs::create_dir_all(client_dir.join("pages/api")).unwrap();
        std::fs::write(
            client_dir.join("pages/api/health.ts"),
            "export default function handler(req: any, res: any) { res.json({ ok: true }); }\n",
        )
        .unwrap();
        std::fs::write(
            client_dir.join("pages/_app.tsx"),
            "import type { AppProps } from 'next/app';\nexport default function App({ Component, pageProps }: AppProps) { return <Component {...pageProps} />; }\n",
        )
        .unwrap();

        // Declare client as lsp_only root
        let config_dir = primary.path().join(".oh");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[workspace.roots]\nclient = \"{}\"\n",
                client_dir.display()
            ),
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: primary.path().to_path_buf(),
            ..Default::default()
        };

        // First build: ApiEndpoint nodes appear
        let gs1 = handler.build_full_graph().await.unwrap();
        assert!(
            gs1.nodes
                .iter()
                .any(|n| n.id.kind == crate::graph::NodeKind::ApiEndpoint && n.id.root == "client"),
            "First build must include ApiEndpoint from lsp_only client root"
        );

        // Remove the lsp_only root from config
        std::fs::write(&config_path, "").unwrap();

        // Force a second build (simulate rescan after config change)
        {
            handler.graph.store(Arc::new(None));
        }
        let gs2 = handler.build_full_graph().await.unwrap();

        // The client root is no longer declared — its nodes should be pruned
        let client_nodes: Vec<_> = gs2
            .nodes
            .iter()
            .filter(|n| n.id.root == "client")
            .collect();
        assert!(
            client_nodes.is_empty(),
            "After removing lsp_only root from config, its nodes must be pruned. \
            Remaining client nodes: {:?}",
            client_nodes
                .iter()
                .map(|n| &n.id.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_enricher_registry_includes_markdown() {
        // Verify that EnricherRegistry::with_builtins() registers enrichers for
        // markdown (marksman) — this confirms we must NOT blanket-skip markdown
        // from LSP enrichment. Language scoping is handled by enrich_all which
        // only invokes enrichers matching the languages of changed files.
        let registry = crate::extract::EnricherRegistry::with_builtins();
        let supported = registry.supported_languages();
        assert!(
            supported.contains("markdown"),
            "Expected markdown in registered enricher languages: {:?}",
            supported
        );
        assert!(
            supported.contains("rust"),
            "Expected rust in registered enricher languages: {:?}",
            supported
        );
    }

    #[tokio::test]
    async fn test_await_background_embed_no_handle() {
        // When no background embedding was spawned, await_background_embed
        // should return immediately without error.
        let handler = RnaHandler::default();
        handler.await_background_embed().await;
        // No panic, no error -- this is the expected no-op path.
    }

    #[tokio::test]
    async fn test_await_background_embed_completed_task() {
        // When a background task completes normally, await should succeed.
        let handler = RnaHandler::default();
        let handle = tokio::spawn(async { /* no-op task completes immediately */ });
        *handler.embed_handle.lock().await = Some(handle);
        handler.await_background_embed().await;
        // Handle should be taken (consumed).
        assert!(handler.embed_handle.lock().await.is_none());
    }

    #[tokio::test]
    async fn test_await_background_embed_cancelled_task() {
        // When a background task is aborted, await should handle gracefully
        // (log warning, not panic).
        let handler = RnaHandler::default();
        let handle = tokio::spawn(async {
            // Task that will be cancelled before it completes.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        handle.abort();
        *handler.embed_handle.lock().await = Some(handle);
        // Should not panic -- the cancelled error is caught.
        handler.await_background_embed().await;
    }

    #[tokio::test]
    async fn test_await_background_embed_panicked_task() {
        // When a background task panics, await should handle gracefully.
        let handler = RnaHandler::default();
        let handle = tokio::spawn(async {
            panic!("simulated embedding panic");
        });
        // Give the task time to panic.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        *handler.embed_handle.lock().await = Some(handle);
        // Should not panic in the caller -- the JoinError is caught.
        handler.await_background_embed().await;
    }

    #[tokio::test]
    async fn test_await_background_embed_idempotent() {
        // Calling await_background_embed twice should be safe (second call is no-op).
        let handler = RnaHandler::default();
        let handle = tokio::spawn(async {});
        *handler.embed_handle.lock().await = Some(handle);
        handler.await_background_embed().await;
        // Second call -- handle was already taken, so this is a no-op.
        handler.await_background_embed().await;
    }

    /// Verify that start_prewarm populates the graph in the background
    /// and that get_graph returns it without starting a duplicate build.
    #[tokio::test]
    async fn test_prewarm_populates_graph_before_first_tool_call() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn prewarm_test_fn() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // Start pre-warm (background task).
        handler.start_prewarm();

        // Wait for the prewarm_notify signal.
        handler.prewarm_notify.notified().await;

        // Graph should now be populated without calling get_graph.
        let snap = handler.graph.load_full();
        let gs = snap.as_ref().as_ref().expect("pre-warm should have populated the graph");
        assert!(
            gs.nodes.iter().any(|n| n.id.name == "prewarm_test_fn"),
            "prewarm_test_fn should be in graph after pre-warm. Nodes: {:?}",
            gs.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );
    }

    /// Verify that get_graph waits for a running pre-warm instead of
    /// starting a duplicate build.
    #[tokio::test]
    async fn test_get_graph_waits_for_prewarm() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn wait_test_fn() {}\n",
        )
        .unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // Start pre-warm.
        handler.start_prewarm();

        // Immediately call get_graph — it should wait for pre-warm, not build again.
        let gs = handler.get_graph().await.unwrap();
        assert!(
            gs.nodes.iter().any(|n| n.id.name == "wait_test_fn"),
            "wait_test_fn should be in graph. Nodes: {:?}",
            gs.nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );
    }

    // ── Adversarial tests (ship step 4, seeded from dissent) ────────────

    /// Dissent finding: concurrent get_graph calls could both detect "no graph"
    /// and race to build, wasting resources. The graph_build_lock should serialize
    /// them so only one build actually runs.
    #[tokio::test]
    async fn test_concurrent_get_graph_calls_serialize_via_build_lock() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn concurrent_fn() {}\n").unwrap();

        let handler = std::sync::Arc::new(RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        });

        // Spawn 5 concurrent get_graph calls. All should succeed and return
        // the same graph (same node set). Without the build lock, this would
        // potentially trigger 5 parallel builds.
        let mut handles = Vec::new();
        for _ in 0..5 {
            let h = handler.clone();
            handles.push(tokio::spawn(async move {
                h.get_graph().await
            }));
        }

        let mut node_counts = Vec::new();
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok(), "get_graph should succeed: {:?}", result.err());
            let gs = result.unwrap();
            assert!(
                gs.nodes.iter().any(|n| n.id.name == "concurrent_fn"),
                "concurrent_fn missing from graph"
            );
            node_counts.push(gs.nodes.len());
        }

        // All calls should see the same graph (same node count).
        let first = node_counts[0];
        for (i, count) in node_counts.iter().enumerate() {
            assert_eq!(*count, first, "Call {} got different node count: {} vs {}", i, count, first);
        }
    }

    /// Dissent finding: ArcSwap store() is atomic, so readers should always see
    /// a consistent snapshot -- never a partially-mutated state.
    #[tokio::test]
    async fn test_arcswap_readers_see_consistent_snapshots() {
        use arc_swap::ArcSwap;

        // Simulate the graph field type directly.
        let graph: Arc<ArcSwap<Option<Arc<GraphState>>>> = Arc::new(ArcSwap::from_pointee(None));

        // Store initial graph with 2 nodes.
        let mut initial = GraphState {
            nodes: vec![],
            edges: vec![],
            index: crate::graph::index::GraphIndex::new(),
            last_scan_completed_at: None,
            detected_frameworks: std::collections::HashSet::new(),
        };
        for name in &["alpha", "beta"] {
            initial.nodes.push(crate::graph::Node {
                id: crate::graph::NodeId {
                    root: "test".into(),
                    file: std::path::PathBuf::from("test.rs"),
                    name: name.to_string(),
                    kind: crate::graph::NodeKind::Function,
                },
                language: "rust".into(),
                line_start: 0,
                line_end: 0,
                signature: String::new(),
                body: String::new(),
                metadata: Default::default(),
                source: crate::graph::ExtractionSource::TreeSitter,
            });
        }
        graph.store(Arc::new(Some(Arc::new(initial))));

        // Spawn a reader that loads the snapshot.
        let snap = graph.load_full();
        let gs = snap.as_ref().as_ref().unwrap();
        assert_eq!(gs.nodes.len(), 2, "Should see both nodes");

        // Now store a completely new graph (3 nodes) -- the old snapshot should
        // be unaffected (this is the core RCU guarantee).
        let mut new_state = (**gs).clone();
        new_state.nodes.push(crate::graph::Node {
            id: crate::graph::NodeId {
                root: "test".into(),
                file: std::path::PathBuf::from("test.rs"),
                name: "gamma".to_string(),
                kind: crate::graph::NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 0,
            line_end: 0,
            signature: String::new(),
            body: String::new(),
            metadata: Default::default(),
            source: crate::graph::ExtractionSource::TreeSitter,
        });
        graph.store(Arc::new(Some(Arc::new(new_state))));

        // Old snapshot still sees 2 nodes (RCU: readers see the snapshot they loaded).
        assert_eq!(gs.nodes.len(), 2, "Old snapshot should still have 2 nodes");

        // New load sees 3 nodes.
        let new_snap = graph.load_full();
        let new_gs = new_snap.as_ref().as_ref().unwrap();
        assert_eq!(new_gs.nodes.len(), 3, "New snapshot should have 3 nodes");
    }

    /// Dissent finding: get_graph returns Arc<GraphState> (not a lock guard).
    /// Verify that the returned value outlives any internal state changes.
    #[tokio::test]
    async fn test_get_graph_result_outlives_subsequent_mutations() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".oh/.cache")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn outlive_fn() {}\n").unwrap();

        let handler = RnaHandler {
            repo_root: root.to_path_buf(),
            ..Default::default()
        };

        // Get the graph.
        let gs1 = handler.get_graph().await.unwrap();
        let node_count_1 = gs1.nodes.len();
        assert!(node_count_1 > 0, "Graph should have nodes");

        // Store a completely different graph (simulating a background swap).
        handler.graph.store(Arc::new(None));

        // The Arc<GraphState> we hold should still be valid and unchanged.
        assert_eq!(gs1.nodes.len(), node_count_1, "Arc should keep the snapshot alive");
        assert!(
            gs1.nodes.iter().any(|n| n.id.name == "outlive_fn"),
            "outlive_fn should still be accessible in the held Arc"
        );
    }
}
