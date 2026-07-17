//! `open` subcommand — launch a local HTTP visualizer for the RNA graph.
//!
//! Starts an Axum HTTP server on a random available port, serves a single-page
//! HTML/JS app at `GET /`, and proxies MCP tool calls from the browser to the
//! in-process RNA server via `POST /mcp`.  The browser opens automatically.
//!
//! Architecture:
//! ```text
//! browser  ──GET /──────────────────► Axum  ──► static HTML (embedded in binary)
//! browser  ──POST /mcp ──────────────► Axum  ──► RNA service layer (no MCP wire)
//! ```
//!
//! The `/mcp` endpoint accepts a simple JSON envelope:
//! ```json
//! { "tool": "repo_map",  "params": {} }
//! { "tool": "search",    "params": { "node": "...", "mode": "neighbors" } }
//! { "tool": "list_roots","params": {} }
//! ```
//! Responses are raw service-layer markdown/text wrapped in `{ "result": "..." }`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpListener;

use crate::business_context::{
    BusinessContextAdmission, BusinessContextMode, CacheModeDisposition,
};
use crate::embed::EmbeddingIndex;
use crate::server::state::GraphState;
use crate::server::{RnaHandler, ScanEnrichmentOptions};
use crate::service::{
    RepoMapContext, RepoMapParams, SearchContext, SearchParams, list_roots_from_slugs, repo_map,
    search,
};

// ─── Viewer HTML (inline so the binary is self-contained) ────────────────────

static VIEWER_HTML: &str = include_str!("viewer.html");

// ─── Shared server state ─────────────────────────────────────────────────────

struct ViewerState {
    graph: GraphState,
    embed_index: Option<EmbeddingIndex>,
    repo_root: PathBuf,
    /// Primary root slug for root_filter
    root_slug: String,
    business_context: BusinessContextAdmission,
}

// ─── Request / response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct McpCall {
    tool: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct McpResponse {
    result: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Tolerant extraction: accept both JSON integers and whole-number floats.
fn value_as_u64_tolerant(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64().and_then(|f| {
            if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= u64::MAX as f64 {
                Some(f as u64)
            } else {
                None
            }
        })
    })
}

// ─── Axum handlers ───────────────────────────────────────────────────────────

async fn serve_index() -> Html<&'static str> {
    Html(VIEWER_HTML)
}

async fn handle_mcp(State(state): State<Arc<ViewerState>>, Json(call): Json<McpCall>) -> Response {
    match dispatch_tool(&state, &call).await {
        Ok(text) => Json(McpResponse { result: text }).into_response(),
        Err(msg) => (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: msg })).into_response(),
    }
}

async fn dispatch_tool(state: &ViewerState, call: &McpCall) -> Result<String, String> {
    let root_filter = Some(state.root_slug.clone());
    let non_code_slugs: HashSet<String> = HashSet::new();

    match call.tool.as_str() {
        "repo_map" => {
            let top_n = call
                .params
                .get("top_n")
                .and_then(value_as_u64_tolerant)
                .unwrap_or(15) as usize;
            let params = RepoMapParams {
                top_n,
                root_filter,
                non_code_slugs,
            };
            let ctx = RepoMapContext {
                graph_state: &state.graph,
                repo_root: &state.repo_root,
                lsp_status: None,
                embed_status: None,
                business_context: &state.business_context,
            };
            Ok(repo_map(&params, &ctx))
        }

        "search" => {
            let query = call
                .params
                .get("query")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let node = call
                .params
                .get("node")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mode = call
                .params
                .get("mode")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let direction = call
                .params
                .get("direction")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let depth = call
                .params
                .get("depth")
                .and_then(value_as_u64_tolerant)
                .and_then(|v| u32::try_from(v).ok());
            let hops = call
                .params
                .get("hops")
                .and_then(value_as_u64_tolerant)
                .and_then(|v| u32::try_from(v).ok());
            let kind = call
                .params
                .get("kind")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let limit = call
                .params
                .get("limit")
                .and_then(value_as_u64_tolerant)
                .map(|v| v as usize);
            let compact = call
                .params
                .get("compact")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let subsystem = call
                .params
                .get("subsystem")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let file = call
                .params
                .get("file")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let line = call
                .params
                .get("line")
                .and_then(value_as_u64_tolerant)
                .and_then(|v| u32::try_from(v).ok());
            let end_line = call
                .params
                .get("end_line")
                .and_then(value_as_u64_tolerant)
                .and_then(|v| u32::try_from(v).ok());
            let root = call
                .params
                .get("root")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let params = SearchParams {
                query,
                node,
                mode,
                direction,
                depth,
                hops,
                kind,
                language: None,
                file,
                line,
                end_line,
                root,
                limit,
                sort_by: None,
                min_complexity: None,
                synthetic: None,
                compact,
                nodes: None,
                search_mode: None,
                rerank: false,
                include_artifacts: true,
                include_markdown: false,
                artifact_types: None,
                subsystem,
                target_subsystem: None,
                edge_types: None,
                include_body: false,
                minify_body: false,
                verbose: false,
            };
            let ctx = SearchContext {
                graph_state: &state.graph,
                embed_index: state.embed_index.as_ref(),
                repo_root: &state.repo_root,
                lsp_status: None,
                embed_status: None,
                root_filter,
                non_code_slugs,
                enrichment_jobs: Vec::new(),
                business_context: &state.business_context,
            };
            Ok(search(&params, &ctx).await)
        }

        "list_roots" => {
            let index_map = state.graph.node_index_map();
            let slugs: HashSet<String> = GraphState::root_slugs_from_index_map(index_map)
                .into_iter()
                .collect();
            Ok(list_roots_from_slugs(
                &state.repo_root,
                &slugs,
                Some(&state.graph),
                None,
                None,
            ))
        }

        other => Err(format!("Unknown tool: {other}")),
    }
}

// ─── Helper: first available TCP port ────────────────────────────────────────

async fn bind_random_port() -> anyhow::Result<TcpListener> {
    // Binding to port 0 lets the OS assign a free port.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    Ok(listener)
}

// ─── Entry point ─────────────────────────────────────────────────────────────

async fn load_viewer_graph(
    repo_root: &Path,
    business_context_mode: BusinessContextMode,
) -> anyhow::Result<(GraphState, BusinessContextAdmission)> {
    let handler = RnaHandler {
        repo_root: repo_root.to_path_buf(),
        business_context: BusinessContextAdmission::new(business_context_mode),
        ..RnaHandler::default()
    };

    let graph = match handler.prepare_business_context_cache()? {
        CacheModeDisposition::Compatible => {
            let lance_path = repo_root.join(".oh").join(".cache").join("lance");
            if !lance_path.exists() {
                anyhow::bail!(
                    "No index found at {}.\nRun `repo-native-alignment scan --repo .` first.",
                    lance_path.display()
                );
            }
            crate::server::load_graph_from_lance(repo_root)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load graph: {}", e))?
        }
        CacheModeDisposition::Initialized | CacheModeDisposition::Rebuilt { .. } => {
            let rebuilt = handler
                .build_full_graph_inner(false, ScanEnrichmentOptions::extract_only())
                .await?;
            crate::server::persist_graph_to_lance(repo_root, &rebuilt.nodes, &rebuilt.edges)
                .await?;
            rebuilt
        }
    };

    Ok((graph, handler.business_context.clone()))
}

/// Run `repo-native-alignment open --repo <path>`.
///
/// 1. Validates the selected business-context cache identity, rebuilding when needed.
/// 2. Starts Axum on a random port.
/// 3. Opens the browser.
/// 4. Blocks until Ctrl-C.
pub async fn run(repo: PathBuf, business_context_mode: BusinessContextMode) -> anyhow::Result<()> {
    let repo_root = repo
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Cannot resolve repo path {}: {}", repo.display(), e))?;

    eprintln!("Loading graph from cache...");
    let (graph, business_context) = load_viewer_graph(&repo_root, business_context_mode).await?;
    eprintln!(
        "  {} symbols, {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );

    // Load embedding index (optional — semantic search in the viewer).
    let embed_index = match EmbeddingIndex::new(&repo_root).await {
        Ok(idx) => match idx.has_table().await {
            Ok(true) => Some(idx),
            _ => None,
        },
        Err(_) => None,
    };

    // Derive the primary root slug.
    let root_slug = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

    let state = Arc::new(ViewerState {
        graph,
        embed_index,
        repo_root,
        root_slug,
        business_context,
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/mcp", post(handle_mcp))
        .with_state(state);

    let listener = bind_random_port().await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}");

    eprintln!("RNA Viewer running at {url}");
    eprintln!("Press Ctrl-C to stop.");

    // Open browser — best-effort, ignore failure.
    let url_clone = url.clone();
    tokio::spawn(async move {
        // Small delay so the server socket is ready before the browser hits it.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if let Err(e) = open_browser(&url_clone) {
            eprintln!("Could not open browser automatically: {e}");
            eprintln!("Open manually: {url_clone}");
        }
    });

    axum::serve(listener, app).await?;
    Ok(())
}

fn open_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .spawn()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        eprintln!("Automatic browser open not supported on this platform. Open: {url}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn viewer_loader_builds_before_cache_reads_for_missing_and_incompatible_modes() {
        for (case, existing_cache, persisted_mode) in [
            ("initialized", false, None),
            ("mismatch", true, Some("enabled\n")),
            ("legacy", true, None),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            std::fs::write(root.join("README.md"), "# Ordinary viewer document\n").unwrap();
            std::fs::create_dir_all(root.join(".oh/outcomes")).unwrap();
            std::fs::write(
                root.join(".oh/outcomes/leak.md"),
                "---\ntitle: Hidden\nstatus: active\n---\nhidden viewer context\n",
            )
            .unwrap();

            let cache = root.join(".oh/.cache");
            let poison = cache.join("lance/not-a-lancedb");
            if existing_cache {
                std::fs::create_dir_all(poison.parent().unwrap()).unwrap();
                std::fs::write(&poison, "must be deleted before any cache read").unwrap();
                if let Some(mode) = persisted_mode {
                    std::fs::write(cache.join("business-context-mode"), mode).unwrap();
                }
            }

            let (graph, admission) = load_viewer_graph(root, BusinessContextMode::Disabled)
                .await
                .unwrap_or_else(|error| panic!("{case} viewer load failed: {error:#}"));

            assert_eq!(admission.mode(), BusinessContextMode::Disabled);
            assert_eq!(
                std::fs::read_to_string(cache.join("business-context-mode")).unwrap(),
                "disabled\n"
            );
            if existing_cache {
                assert!(
                    !poison.exists(),
                    "{case} cache was read instead of deleted before rebuild"
                );
            }
            assert!(
                graph
                    .nodes
                    .iter()
                    .any(|node| node.id.file == PathBuf::from("README.md")),
                "{case} viewer rebuild lost ordinary repository Markdown"
            );
            assert!(
                graph
                    .nodes
                    .iter()
                    .all(|node| !node.id.file.starts_with(".oh")),
                "{case} viewer rebuild admitted disabled business artifacts"
            );

            let (reopened, reopened_admission) =
                load_viewer_graph(root, BusinessContextMode::Disabled)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{case} compatible viewer reopen failed: {error:#}")
                    });
            assert_eq!(reopened_admission.mode(), BusinessContextMode::Disabled);
            assert!(
                reopened
                    .nodes
                    .iter()
                    .any(|node| node.id.file == PathBuf::from("README.md")),
                "{case} compatible reopen lost ordinary repository Markdown"
            );
            assert!(
                reopened
                    .nodes
                    .iter()
                    .all(|node| !node.id.file.starts_with(".oh")),
                "{case} compatible reopen admitted disabled business artifacts"
            );
        }
    }

    // ── value_as_u64_tolerant ────────────────────────────────────────────

    #[test]
    fn tolerant_accepts_integer() {
        assert_eq!(value_as_u64_tolerant(&json!(15)), Some(15));
    }

    #[test]
    fn tolerant_accepts_whole_float() {
        assert_eq!(value_as_u64_tolerant(&json!(15.0)), Some(15));
    }

    #[test]
    fn tolerant_accepts_zero_float() {
        assert_eq!(value_as_u64_tolerant(&json!(0.0)), Some(0));
    }

    #[test]
    fn tolerant_accepts_zero_int() {
        assert_eq!(value_as_u64_tolerant(&json!(0)), Some(0));
    }

    #[test]
    fn tolerant_rejects_fractional() {
        assert_eq!(value_as_u64_tolerant(&json!(15.5)), None);
    }

    #[test]
    fn tolerant_rejects_negative_float() {
        assert_eq!(value_as_u64_tolerant(&json!(-1.0)), None);
    }

    #[test]
    fn tolerant_rejects_negative_int() {
        assert_eq!(value_as_u64_tolerant(&json!(-1)), None);
    }

    #[test]
    fn tolerant_rejects_string() {
        assert_eq!(value_as_u64_tolerant(&json!("15")), None);
    }

    #[test]
    fn tolerant_rejects_bool() {
        assert_eq!(value_as_u64_tolerant(&json!(true)), None);
    }

    #[test]
    fn tolerant_rejects_null() {
        assert_eq!(value_as_u64_tolerant(&json!(null)), None);
    }

    #[test]
    fn tolerant_rejects_large_float_above_u64() {
        // u64::MAX as f64 rounds up to 2^64; values at that boundary saturate
        // but the check still passes (harmless: downstream u32::try_from rejects)
        // Truly larger values are not representable in JSON.
        let huge = json!(1.8446744073709552e19_f64); // ≈ u64::MAX as f64
        // as_u64() succeeds for serde_json integer-range values; the float path
        // is only hit for explicitly fractional JSON like 30.0
        assert!(value_as_u64_tolerant(&huge).is_some());
    }

    #[test]
    fn tolerant_large_whole_float_accepted() {
        // 5 billion: valid u64, representable exactly in f64
        assert_eq!(
            value_as_u64_tolerant(&json!(5_000_000_000.0)),
            Some(5_000_000_000)
        );
    }
}
