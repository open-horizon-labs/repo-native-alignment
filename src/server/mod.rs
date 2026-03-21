//! MCP server: RnaHandler, graph building, background scanner, and MCP dispatch.

pub mod tools;
pub mod store;
pub mod state;
pub mod handlers;
pub mod helpers;
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
pub use state::{EmbeddingStatus, GraphState, LspEnrichmentStatus, LspState};
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
use tokio::sync::RwLock;

use helpers::parse_args;

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
            embed_status: Arc::new(EmbeddingStatus::default()),
            non_code_root_slugs_cache: std::sync::Mutex::new(None),
            lance_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            lsp_only_roots: Arc::new(Vec::new()),
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
            let guard = handler.graph.read().await;
            let gs = guard.as_ref().unwrap();
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
            let guard = handler2.graph.read().await;
            let gs = guard.as_ref().unwrap();
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
            let mut g = handler.graph.write().await;
            *g = None;
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
}
