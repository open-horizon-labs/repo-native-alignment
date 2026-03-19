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
            .with_agent_memories(&self.repo_root);
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
                self.handle_list_roots()
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
