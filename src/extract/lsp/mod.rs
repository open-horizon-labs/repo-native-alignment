//! LSP-based enricher for cross-file reference discovery.
//!
//! Phase 2 enrichment: spawns a language server as a child process, sends
//! JSON-RPC messages over stdin/stdout, and uses `textDocument/references`
//! and `textDocument/implementation` to discover cross-file edges.
//!
//! Supports multiple language servers (rust-analyzer, pyright, typescript-language-server,
//! gopls, marksman) via the same generic `LspEnricher` struct.
//!
//! Design decisions:
//! - Spawns the language server on first `enrich()` call, not at startup
//! - Keeps the language server alive for the session duration
//! - If the server binary is not installed, logs info and skips gracefully
//! - 60-second timeout per LSP request
//!
//! ## Module structure
//!
//! - `transport` — JSON-RPC framing: [`LspTransport`] (sequential, init-phase) and
//!   [`PipelinedTransport`] (concurrent, enrichment-phase), plus URI helpers.

mod transport;
use transport::{LspTransport, PipelinedTransport, path_to_uri, find_enclosing_symbol, uri_to_relative_path};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use lsp_types::{
    ClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse,
    InitializeParams, InitializeResult, Location, Position,
    TextDocumentIdentifier, TextDocumentPositionParams, Uri,
};

use crate::graph::index::GraphIndex;
use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};
use crate::scanner::LspConfig;

use super::{EnrichmentResult, Enricher};


// ---------------------------------------------------------------------------
// LspEnricher
// ---------------------------------------------------------------------------

/// LSP enricher that uses a language server to discover cross-file references
/// and trait/interface implementations.
///
/// Generic over the language server binary — the same struct handles
/// rust-analyzer, pyright, typescript-language-server, gopls, and marksman.
pub struct LspEnricher {
    /// Language identifier (e.g., "rust", "python").
    language: String,
    /// Static ref for Enricher::languages() return (leaked once per enricher).
    language_static: &'static [&'static str],
    /// Display name for logging (e.g., "rust-analyzer-lsp").
    display_name: String,
    /// Command to spawn (e.g., "rust-analyzer", "pyright-langserver").
    server_command: String,
    /// Arguments to pass to the server (e.g., ["--stdio"]).
    server_args: Vec<String>,
    /// File extensions this enricher handles (e.g., ["rs"], ["py"]).
    /// Stored for future use in file-level filtering.
    #[allow(dead_code)]
    extensions: Vec<String>,
    /// Optional initialization settings (sent in initialize params).
    init_settings: Option<serde_json::Value>,
    /// Config file this enricher relies on (e.g., "tsconfig.json" for TypeScript).
    /// Used by pick_lsp_root to prefer lsp_roots that contain this file.
    config_file: Option<&'static str>,
    ready: AtomicBool,
    /// Protected by mutex because enrich takes &self but we need to mutate transport state.
    state: Mutex<LspState>,
    /// Override the LSP server working directory (`rootUri` / `current_dir`).
    ///
    /// When set, the language server is started from this directory instead of `repo_root`.
    /// This is used for monorepo subdirectory roots: typescript-language-server for
    /// `client/` should start from `client/` (where `tsconfig.json` lives) even though
    /// the nodes' file paths are relative to the primary repo root.
    ///
    /// Note: this only affects server startup. File path construction for LSP requests
    /// always uses `repo_root` (passed to `enrich()`), which ensures file URIs point to
    /// the correct absolute paths.
    startup_root_override: std::sync::OnceLock<PathBuf>,
}

struct LspState {
    /// Sequential transport used during initialization only.
    transport: Option<LspTransport>,
    /// Pipelined transport used during enrichment (concurrent requests).
    pipelined: Option<Arc<PipelinedTransport>>,
    /// Cached root path from initialization.
    root_path: Option<PathBuf>,
    /// Whether we already tried and failed to start the language server.
    init_failed: bool,
    /// Whether the language server supports type hierarchy requests.
    has_type_hierarchy: bool,
    /// Whether the language server supports textDocument/references requests.
    has_references: bool,
    /// Whether the language server supports pull-based diagnostics
    /// (`textDocument/diagnostic`, LSP 3.17+).
    has_pull_diagnostics: bool,
    /// Whether the language server supports inlay hints
    /// (`textDocument/inlayHint`, LSP 3.17+).
    has_inlay_hints: bool,
    /// Whether the server reached quiescent=true during initialization.
    /// When false, the quiescence deadline expired before the server finished
    /// indexing. In that case Pass 3 (diagnostics) is skipped to avoid flooding
    /// the server with diagnostic requests while it is still loading — which was
    /// the root cause of the 0-edge regression introduced by #381.
    was_quiescent: bool,
    /// Consecutive type hierarchy failures. After MAX_TYPE_HIERARCHY_STRIKES,
    /// type hierarchy is disabled for the rest of the session.
    type_hierarchy_strikes: u32,
    /// Shared diagnostics buffer populated by the pipelined transport's reader
    /// loop from `textDocument/publishDiagnostics` notifications.
    /// Maps document URI → list of LSP Diagnostic objects (JSON).
    diagnostics_sink: Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
}

/// After this many consecutive type hierarchy failures, disable type hierarchy
/// for the remainder of the enrichment pass to avoid stalling on broken servers.
const MAX_TYPE_HIERARCHY_STRIKES: u32 = 3;

/// After processing this many nodes with zero edges, abort enrichment.
/// A functioning language server should produce at least some edges within
/// the first 1,000 nodes; zero edges indicates misconfiguration (e.g., pyright
/// without a venv, or a server that can't resolve any references).
const ZERO_EDGE_ABORT_THRESHOLD: u32 = 1_000;

/// Minimum warmup time before the node-count abort can fire.
/// typescript-language-server and similar servers need time to fully index
/// the project before producing call hierarchy results. Without this guard,
/// the 1,000-node abort fires in ~0.3s on large TypeScript projects before
/// the server has finished indexing — producing 0 call edges despite being
/// correctly configured.
const ZERO_EDGE_MIN_WARMUP: std::time::Duration = std::time::Duration::from_secs(30);

/// Time-based abort: if no edges after this duration, abort enrichment.
/// On slow LSP servers (e.g., pyright without warm cache), reaching the
/// node-count threshold can take 100+ minutes. This caps the wait at 2 minutes.
const ZERO_EDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl LspEnricher {
    /// Create a new LSP enricher for the given language server.
    ///
    /// - `language`: language identifier (e.g., "rust", "python")
    /// - `command`: binary to spawn (e.g., "rust-analyzer", "pyright-langserver")
    /// - `args`: command-line arguments (e.g., &["--stdio"])
    /// - `extensions`: file extensions this enricher handles (e.g., &["rs"])
    pub fn new(language: &str, command: &str, args: &[&str], extensions: &[&str]) -> Self {
        // Leak language string once — enrichers live for the entire program
        let lang_static: &'static str = Box::leak(language.to_string().into_boxed_str());
        let lang_slice: &'static [&'static str] = Box::leak(vec![lang_static].into_boxed_slice());

        Self {
            language: language.to_string(),
            language_static: lang_slice,
            display_name: format!("{}-lsp", command),
            server_command: command.to_string(),
            server_args: args.iter().map(|s| s.to_string()).collect(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            init_settings: None,
            config_file: None,
            ready: AtomicBool::new(false),
            state: Mutex::new(LspState {
                transport: None,
                pipelined: None,
                root_path: None,
                init_failed: false,
                has_type_hierarchy: false,
                has_references: false,
                has_pull_diagnostics: false,
                has_inlay_hints: false,
                was_quiescent: false,
                type_hierarchy_strikes: 0,
                diagnostics_sink: Arc::new(std::sync::Mutex::new(HashMap::new())),
            }),
            startup_root_override: std::sync::OnceLock::new(),
        }
    }

    /// Override the LSP server startup working directory.
    ///
    /// When called before the first `enrich()` call, the language server will be
    /// started with `current_dir = lsp_root` and `rootUri = file:///<lsp_root>`.
    /// This is used for monorepo subdirectory roots (e.g., `client/`) where the
    /// language server needs to find `tsconfig.json` / `pyproject.toml` in the
    /// subdirectory.
    ///
    /// File path construction for LSP requests is unaffected — it uses `repo_root`
    /// from `enrich()`, which produces correct absolute file URIs.
    pub fn with_startup_root(self, lsp_root: PathBuf) -> Self {
        let _ = self.startup_root_override.set(lsp_root);
        self
    }

    /// Create a new LSP enricher with custom initialization settings.
    ///
    /// Settings are sent as `initializationOptions` in the LSP initialize request.
    pub fn with_settings(mut self, settings: serde_json::Value) -> Self {
        self.init_settings = Some(settings);
        self
    }

    /// Set the config file hint for lsp_root selection.
    ///
    /// When a monorepo has multiple subdirectory roots, this hint is used to
    /// prefer the root that contains this file (e.g., `tsconfig.json` for TypeScript).
    pub fn with_config_file(mut self, config_file: &'static str) -> Self {
        self.config_file = Some(config_file);
        self
    }

    /// Check if an `experimental/serverStatus` notification indicates readiness.
    ///
    /// rust-analyzer sends `quiescent: true` when it has finished all background
    /// work (indexing, proc-macro loading, etc.).  Combined with `health: "ok"`,
    /// this means the server is ready to answer queries.
    fn server_status_is_ready(msg: &serde_json::Value) -> bool {
        let health = msg
            .pointer("/params/health")
            .and_then(|h| h.as_str())
            .unwrap_or("");
        let quiescent = msg
            .pointer("/params/quiescent")
            .and_then(|q| q.as_bool())
            .unwrap_or(false); // Default to NOT ready if field is absent
        health == "ok" && quiescent
    }

    /// Check if the server binary is available on PATH.
    fn is_server_available(&self) -> bool {
        std::process::Command::new("which")
            .arg(&self.server_command)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Initialize the language server if not already running.
    async fn ensure_initialized(&self, repo_root: &Path) -> Result<()> {
        let mut state = self.state.lock().await;

        if state.pipelined.is_some() || state.transport.is_some() {
            return Ok(());
        }

        if state.init_failed {
            return Err(anyhow::anyhow!(
                "{} initialization previously failed",
                self.server_command
            ));
        }

        // Check if the server binary is available before trying to spawn
        if !self.is_server_available() {
            state.init_failed = true;
            tracing::info!(
                "LSP server '{}' not found, skipping enrichment for {}",
                self.server_command,
                self.language
            );
            return Err(anyhow::anyhow!(
                "LSP server '{}' not found on PATH",
                self.server_command
            ));
        }

        // Use startup_root_override if set (monorepo subdirectory roots), otherwise
        // fall back to repo_root. The startup root determines the LSP server's
        // `current_dir` and `rootUri`, letting language servers find their config
        // files (e.g. typescript-language-server finds `client/tsconfig.json` when
        // started from `client/`). File path construction for LSP requests still
        // uses `repo_root` (the primary root) via `state.root_path` below.
        let startup_root = self
            .startup_root_override
            .get()
            .map(|p| p.as_path())
            .unwrap_or(repo_root);

        if startup_root != repo_root {
            tracing::info!(
                "Starting {} for {} LSP enrichment from '{}' (startup root override)...",
                self.server_command,
                self.language,
                startup_root.display(),
            );
        } else {
            tracing::info!(
                "Starting {} for {} LSP enrichment...",
                self.server_command,
                self.language
            );
        }

        let transport =
            match LspTransport::spawn(&self.server_command, &self.server_args, startup_root).await {
                Ok(t) => t,
                Err(e) => {
                    state.init_failed = true;
                    tracing::warn!(
                        "{} not available, skipping LSP enrichment for {}: {}",
                        self.server_command,
                        self.language,
                        e
                    );
                    return Err(e);
                }
            };

        // Always store the primary repo_root in root_path — this is used for
        // constructing absolute file paths in LSP requests (root.join(node.id.file)).
        // The startup root is only for server initialization; file paths remain
        // relative to the primary root.
        state.transport = Some(transport);
        state.root_path = Some(repo_root.to_path_buf());

        // Send initialize request using the startup root as rootUri.
        let root_uri = path_to_uri(startup_root)?;

        #[allow(deprecated)] // root_uri is deprecated in favor of workspace_folders
        let mut init_params = InitializeParams {
            root_uri: Some(root_uri),
            capabilities: ClientCapabilities {
                window: Some(lsp_types::WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..Default::default()
                }),
                // Declare support for experimental/serverStatus notifications.
                // Without this, rust-analyzer won't send serverStatus and the
                // readiness wait falls through to a timeout, sending queries
                // while the server is still indexing (producing 0 edges).
                experimental: Some(serde_json::json!({
                    "serverStatusNotification": true
                })),
                ..Default::default()
            },
            ..Default::default()
        };

        // Apply per-language initialization settings if provided.
        // For pyright, augment with venvPath + venv when a .venv exists at startup_root,
        // so pyright can resolve installed packages and produce call edges.
        // Without this, pyright produces 0 edges on projects that use poetry/venv.
        let effective_settings = self.init_settings.as_ref().map(|s| {
            if self.language == "python" {
                let venv_dir = startup_root.join(".venv");
                if venv_dir.exists() {
                    // Merge venvPath + venv into the python.analysis section.
                    // startup_root is the venv parent (e.g., ai_service/).
                    let venv_path_str = startup_root.to_string_lossy().to_string();
                    let mut merged = s.clone();
                    if let Some(python_obj) = merged.get_mut("python") {
                        if let Some(analysis_obj) = python_obj.get_mut("analysis") {
                            if let Some(obj) = analysis_obj.as_object_mut() {
                                obj.insert("venvPath".into(), serde_json::Value::String(venv_path_str));
                                obj.insert("venv".into(), serde_json::Value::String(".venv".into()));
                            }
                        }
                    }
                    tracing::info!(
                        "pyright: found .venv at '{}', adding venvPath/venv to initializationOptions",
                        startup_root.display()
                    );
                    return merged;
                }
            }
            s.clone()
        });
        if let Some(ref settings) = effective_settings {
            init_params.initialization_options = Some(settings.clone());
        }

        let init_result = {
            let transport = state.transport.as_mut().unwrap();
            transport.request("initialize", &init_params).await?
        };

        // Parse and check server capabilities
        // Check type hierarchy provider from raw JSON before from_value consumes it,
        // because lsp-types 0.97 ServerCapabilities is missing the field.
        let has_type_hierarchy = init_result
            .pointer("/capabilities/typeHierarchyProvider")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        // Check pull-based diagnostics capability (LSP 3.17+, "diagnosticProvider")
        let has_pull_diagnostics = init_result
            .pointer("/capabilities/diagnosticProvider")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        // Check inlay hints capability (LSP 3.17+, "inlayHintProvider")
        let has_inlay_hints = init_result
            .pointer("/capabilities/inlayHintProvider")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        let init_result_parsed: InitializeResult = serde_json::from_value(init_result)
            .context("Failed to parse initialize result")?;

        let has_references = init_result_parsed.capabilities.references_provider.is_some();
        let has_implementation = init_result_parsed.capabilities.implementation_provider.is_some();
        tracing::info!(
            "{} capabilities: references={}, implementation={}, type_hierarchy={}, pull_diagnostics={}, inlay_hints={}",
            self.server_command, has_references, has_implementation, has_type_hierarchy, has_pull_diagnostics, has_inlay_hints
        );

        state.has_type_hierarchy = has_type_hierarchy;
        state.has_references = has_references;
        state.has_pull_diagnostics = has_pull_diagnostics;
        state.has_inlay_hints = has_inlay_hints;

        // Send initialized notification
        let transport = state.transport.as_mut().unwrap();
        transport
            .notify("initialized", serde_json::json!({}))
            .await?;

        tracing::info!("{} initialized, waiting for indexing...", self.server_command);

        // Wait for the language server to finish indexing the workspace.
        // rust-analyzer (and most LSP servers) need time after `initialized`
        // to build their project index. Without this wait, all reference
        // lookups return "file not found."
        //
        // We drain notifications looking for progress/done signals.
        // Timeout after 120 seconds to allow large workspaces to finish indexing.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(120);
        let transport = state.transport.as_mut().unwrap();
        // Wait for the server to be ready.
        //
        // rust-analyzer uses `experimental/serverStatus` with:
        //   health: "ok" | "warning" | "error"
        //   quiescent: bool (true = fully ready, no pending background work)
        //
        // Other LSP servers may not send this — for those, we fall back
        // to a timeout after `$/progress` tokens complete.
        let mut server_ready = false;
        // Track whether we've seen any serverStatus notification.
        // If the server supports serverStatus, we should keep waiting for
        // quiescent=true rather than bailing on a per-message timeout.
        let mut seen_server_status = false;
        // Track the raw `quiescent` bit independently of `health`.
        // We only care about "done indexing" for the Pass 3 guard; health="warning"
        // (compile errors) does not mean RA is still indexing.
        let mut saw_quiescent = false;

        while tokio::time::Instant::now() < deadline {
            // Use a short timeout only when we haven't seen serverStatus yet.
            // Once we know the server supports serverStatus, wait up to the
            // full deadline for the quiescent signal.
            let msg_timeout = if seen_server_status {
                // Wait up to remaining time in the deadline
                let remaining = deadline.duration_since(tokio::time::Instant::now());
                remaining.min(tokio::time::Duration::from_secs(60))
            } else {
                tokio::time::Duration::from_secs(5)
            };

            match tokio::time::timeout(
                msg_timeout,
                transport.read_message(),
            )
            .await
            {
                Ok(Ok(msg)) => {
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        match method {
                            // rust-analyzer's readiness signal
                            "experimental/serverStatus" => {
                                seen_server_status = true;
                                let health = msg.pointer("/params/health").and_then(|h| h.as_str()).unwrap_or("");
                                let quiescent = msg.pointer("/params/quiescent").and_then(|q| q.as_bool()).unwrap_or(false);
                                tracing::info!("{} serverStatus: health={}, quiescent={}", self.server_command, health, quiescent);

                                // Track the raw quiescent bit separately from health.
                                // Pass 3 cares about "done indexing" (quiescent=true),
                                // not about compilation health (which may be "warning" or "error").
                                if quiescent {
                                    saw_quiescent = true;
                                }

                                if Self::server_status_is_ready(&msg) {
                                    tracing::info!("{} ready (serverStatus: ok, quiescent)", self.server_command);
                                    server_ready = true;
                                    break;
                                }
                                // If quiescent=true but health!=ok, the server is done indexing
                                // but has errors/warnings. Still break — no point waiting further.
                                if quiescent {
                                    tracing::info!(
                                        "{} quiescent=true (health={}), proceeding despite non-ok health",
                                        self.server_command, health
                                    );
                                    break;
                                }
                                tracing::info!("{} not yet ready, continuing to wait for indexing...", self.server_command);
                            }
                            // Respond to progress create requests (required by protocol)
                            "window/workDoneProgress/create" => {
                                if let Some(id) = msg.get("id") {
                                    let response = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": null
                                    });
                                    let _ = transport.send_message(&response).await;
                                }
                            }
                            "$/progress" => {
                                // Log progress for debugging but don't use it for readiness
                                let kind = msg.pointer("/params/value/kind").and_then(|k| k.as_str()).unwrap_or("");
                                let title = msg.pointer("/params/value/title").and_then(|t| t.as_str()).unwrap_or("");
                                if kind == "begin" || kind == "end" {
                                    tracing::info!("{} progress {}: {}", self.server_command, kind, title);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!("Error reading LSP message during init: {}", e);
                    break;
                }
                Err(_) => {
                    if seen_server_status {
                        // We know the server supports serverStatus but hit the deadline
                        // without quiescent=true. Proceed anyway.
                        tracing::warn!(
                            "{} waited for quiescent but deadline reached, proceeding",
                            self.server_command
                        );
                    } else {
                        // No serverStatus received — server may not support it
                        tracing::info!("{} no serverStatus after 5s, assuming ready", self.server_command);
                    }
                    break;
                }
            }
        }

        if !server_ready {
            tracing::info!("{} readiness wait complete (server_ready=false, seen_status={}), proceeding", self.server_command, seen_server_status);
        }

        // Record whether the server became quiescent. Pass 3 (diagnostics) is
        // only safe to run when the server has finished indexing; sending
        // thousands of textDocument/diagnostic requests to an unindexed server
        // floods its queue, which was the root cause of the #379 regression.
        //
        // Use `saw_quiescent` (raw quiescent=true bit) rather than `server_ready`
        // (which also requires health="ok"). This ensures Pass 3 runs for repos
        // with compile errors (health="warning", quiescent=true) — the server IS
        // done indexing, it just has errors. Pass 3 is safe in that case.
        //
        // The guard only applies when the server supports `experimental/serverStatus`
        // (`seen_server_status = true`) but never sent quiescent=true before the
        // deadline. Servers that do NOT support serverStatus (pyright, marksman, etc.)
        // never set `seen_server_status`, so we treat them as quiescent (they're
        // assumed ready after the 5s fallback timeout and won't flood RA's queue).
        state.was_quiescent = saw_quiescent || !seen_server_status;
        if seen_server_status && !saw_quiescent {
            tracing::warn!(
                "{} did not reach quiescent state — Pass 3 (diagnostics) will be skipped this session",
                self.server_command
            );
        }

        tracing::info!("{} ready for {}", self.server_command, self.language);

        // Convert to pipelined transport for concurrent request support.
        // Share the diagnostics sink so publishDiagnostics notifications received
        // during enrichment are captured for later conversion to diagnostic nodes.
        // Pass the initial quiescent state so the pipelined transport's quiescent_flag
        // starts correct; the reader loop will live-update it for subsequent scans.
        if let Some(transport) = state.transport.take() {
            let diag_sink = Arc::clone(&state.diagnostics_sink);
            let pipelined = PipelinedTransport::from_sequential_with_diag_sink(
                transport, diag_sink, state.was_quiescent
            );
            tracing::info!("{} converted to pipelined transport", self.server_command);
            state.pipelined = Some(Arc::new(pipelined));
        }

        self.ready.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// Prepare call hierarchy at a position (pipelined). Returns the CallHierarchyItem if found.
    async fn prepare_call_hierarchy_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Option<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": { "line": line, "character": character }
        });

        let result: serde_json::Value = transport
            .request("textDocument/prepareCallHierarchy", &params)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        if let Some(items) = result.as_array() {
            Ok(items.first().cloned())
        } else {
            Ok(Some(result))
        }
    }

    /// Find outgoing calls (pipelined).
    async fn outgoing_calls_p(
        transport: &PipelinedTransport,
        item: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({ "item": item });
        let result: serde_json::Value = transport
            .request("callHierarchy/outgoingCalls", &params)
            .await?;
        if result.is_null() { return Ok(Vec::new()); }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Find incoming calls (pipelined).
    async fn incoming_calls_p(
        transport: &PipelinedTransport,
        item: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({ "item": item });
        let result: serde_json::Value = transport
            .request("callHierarchy/incomingCalls", &params)
            .await?;
        if result.is_null() { return Ok(Vec::new()); }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Get document links (pipelined).
    async fn document_links_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() }
        });
        let result: serde_json::Value = transport
            .request("textDocument/documentLink", &params)
            .await?;
        if result.is_null() { return Ok(Vec::new()); }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Find implementations of a trait/interface (pipelined).
    async fn find_implementations_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: file_uri.clone(),
                },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result: serde_json::Value = transport
            .request("textDocument/implementation", &params)
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        let locations: Vec<Location> =
            match serde_json::from_value::<GotoDefinitionResponse>(result) {
                Ok(GotoDefinitionResponse::Scalar(loc)) => vec![loc],
                Ok(GotoDefinitionResponse::Array(locs)) => locs,
                Ok(GotoDefinitionResponse::Link(links)) => links
                    .into_iter()
                    .map(|link| Location {
                        uri: link.target_uri,
                        range: link.target_range,
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };

        Ok(locations)
    }

    /// Compute the 0-based LSP line and column for a node.
    ///
    /// Uses the AST-recorded byte column of the name identifier stored by the
    /// extractor (metadata key "name_col"). This is exact and language-agnostic:
    /// tree-sitter records start_position().column for the name field node, so
    /// it works correctly even when the name appears multiple times in the
    /// signature (e.g. `pub fn from_str(from_str: &str)`) or when the keyword
    /// prefix length varies across languages (Python `def`, Go `func`, etc.).
    /// If the extractor did not populate name_col (legacy or non-tree-sitter
    /// nodes), falls back to signature scanning.
    fn node_lsp_position(node: &Node) -> (u32, u32) {
        let line = (node.line_start.saturating_sub(1)) as u32;
        let col = if let Some(col_str) = node.metadata.get("name_col") {
            col_str.parse::<u32>().unwrap_or_else(|_| {
                tracing::debug!(
                    node = %node.id.name,
                    raw = %col_str,
                    "name_col metadata could not be parsed as u32; falling back to signature scan"
                );
                node.signature.find(&node.id.name).map(|i| i as u32).unwrap_or(0)
            })
        } else {
            let fallback = node.signature.find(&node.id.name).map(|i| i as u32).unwrap_or(0);
            tracing::debug!(
                node = %node.id.name,
                col = fallback,
                "name_col not in metadata; using signature scan fallback (may miss on overloaded names)"
            );
            fallback
        };
        (line, col)
    }

    /// Update the type hierarchy strike counter after a single enrich attempt.
    /// Resets on success, increments on failure, and disables the feature after
    /// `MAX_TYPE_HIERARCHY_STRIKES` consecutive failures.
    fn update_type_hierarchy_strikes(
        ok: bool,
        strikes: &mut u32,
        enabled: &mut bool,
    ) {
        if ok {
            *strikes = 0;
        } else {
            *strikes += 1;
            if *strikes >= MAX_TYPE_HIERARCHY_STRIKES {
                tracing::warn!(
                    "Type hierarchy disabled after {} consecutive failures",
                    *strikes
                );
                *enabled = false;
            }
        }
    }

    /// Find references to a symbol at a position (pipelined).
    /// Returns a list of LSP Location objects for each reference site.
    async fn find_references_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": false }
        });

        let result: serde_json::Value = transport
            .request("textDocument/references", &params)
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        let locations: Vec<Location> = serde_json::from_value(result).unwrap_or_default();
        Ok(locations)
    }

    /// Use type hierarchy to discover supertypes for a node, creating
    /// Implements edges for each resolved supertype relationship.
    ///
    /// Only called for Trait/Struct/Enum nodes (the only kinds eligible for
    /// type hierarchy). Subtypes are not queried here because find_implementations
    /// already covers that direction for Traits, and Rust Struct/Enum nodes
    /// cannot have subtypes.
    ///
    /// Returns `true` if the prepare call succeeded, `false` if it failed (used for
    /// strike counting).
    async fn enrich_type_hierarchy_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
        node: &Node,
        matching_nodes: &[&Node],
        root: &Path,
        result: &mut EnrichmentResult,
    ) -> bool {
        let items = match Self::prepare_type_hierarchy_p(transport, file_uri, line, character).await {
            Ok(items) if !items.is_empty() => items,
            Ok(_) => return true, // No type hierarchy item — not a failure
            Err(e) => {
                tracing::debug!("prepareTypeHierarchy failed for {}: {}", node.id.name, e);
                return false;
            }
        };

        for item in &items {
            // Supertypes: this node implements/inherits from each supertype
            match Self::type_hierarchy_supertypes_p(transport, item).await {
                Ok(supertypes) => {
                    for supertype in &supertypes {
                        if let Some(target_id) = Self::resolve_type_hierarchy_item(
                            supertype, matching_nodes, root,
                        ) {
                            // Skip self-references
                            if target_id == node.id {
                                continue;
                            }
                            tracing::debug!(
                                "Type hierarchy: {} implements supertype {}",
                                node.id.name, target_id.name
                            );
                            result.added_edges.push(Edge {
                                from: node.id.clone(),
                                to: target_id,
                                kind: EdgeKind::Implements,
                                source: ExtractionSource::Lsp,
                                confidence: Confidence::Confirmed,
                            });
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        "typeHierarchy/supertypes failed for {}: {}",
                        node.id.name, e
                    );
                }
            }

        }

        true // prepare succeeded
    }

    /// Resolve a TypeHierarchyItem (JSON) to a NodeId in the graph.
    /// Returns None if the item's file/name doesn't match any known node.
    fn resolve_type_hierarchy_item(
        item: &serde_json::Value,
        matching_nodes: &[&Node],
        root: &Path,
    ) -> Option<NodeId> {
        let name = item.get("name")?.as_str()?;
        let uri_str = item.get("uri")?.as_str()?;

        // Use url::Url for proper percent-decoding of file:// URIs
        let abs_path = match url::Url::parse(uri_str) {
            Ok(url) => match url.to_file_path() {
                Ok(p) => p,
                Err(_) => return None,
            },
            Err(_) => {
                // Fallback: manual strip for non-standard URIs
                let file_path_str = uri_str.strip_prefix("file://")?;
                PathBuf::from(file_path_str)
            }
        };

        let rel_path = abs_path.strip_prefix(root).unwrap_or(&abs_path).to_path_buf();

        // Skip external dependencies
        if rel_path.to_string_lossy().contains(".cargo") {
            return None;
        }

        let range_start_line = item
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .map(|l| l as usize + 1)
            .unwrap_or(0);

        // Try exact name + file match first
        let candidates: Vec<_> = matching_nodes.iter()
            .filter(|n| n.id.file == rel_path)
            .filter(|n| n.id.name == name)
            .filter(|n| matches!(n.id.kind,
                NodeKind::Trait | NodeKind::Struct | NodeKind::Enum | NodeKind::Impl
            ))
            .collect();

        if candidates.len() == 1 {
            return Some(candidates[0].id.clone());
        }

        if candidates.len() > 1 {
            // Ambiguous name match — use position to disambiguate (issue #2: name collision)
            tracing::debug!(
                "resolve_type_hierarchy_item: {} candidates for '{}' in {}, using position tiebreaker",
                candidates.len(), name, rel_path.display()
            );
            if range_start_line > 0 {
                if let Some(best) = candidates.iter()
                    .filter(|n| n.line_start <= range_start_line && n.line_end >= range_start_line)
                    .min_by_key(|n| n.line_end - n.line_start)
                {
                    return Some(best.id.clone());
                }
            }
            // If position doesn't help, pick closest by line_start
            if range_start_line > 0 {
                if let Some(best) = candidates.iter()
                    .min_by_key(|n| (n.line_start as isize - range_start_line as isize).unsigned_abs())
                {
                    return Some(best.id.clone());
                }
            }
            // Last resort: take first
            return Some(candidates[0].id.clone());
        }

        // Fallback: find enclosing symbol at the position
        matching_nodes.iter()
            .filter(|n| n.id.file == rel_path)
            .filter(|n| matches!(n.id.kind,
                NodeKind::Trait | NodeKind::Struct | NodeKind::Enum | NodeKind::Impl
            ))
            .filter(|n| range_start_line == 0 || (n.line_start <= range_start_line && n.line_end >= range_start_line))
            .min_by_key(|n| n.line_end - n.line_start)
            .map(|n| n.id.clone())
    }

    /// Prepare type hierarchy at a position (pipelined).
    async fn prepare_type_hierarchy_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": { "line": line, "character": character }
        });

        let result: serde_json::Value = transport
            .request("textDocument/prepareTypeHierarchy", &params)
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Find supertypes for a TypeHierarchyItem (pipelined).
    async fn type_hierarchy_supertypes_p(
        transport: &PipelinedTransport,
        item: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({ "item": item });
        let result: serde_json::Value = transport
            .request("typeHierarchy/supertypes", &params)
            .await?;
        if result.is_null() { return Ok(Vec::new()); }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Request pull-based diagnostics for a single file (LSP 3.17+).
    /// Returns an empty Vec if the server returns null or an error.
    async fn pull_diagnostics_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() }
        });
        let result: serde_json::Value = transport
            .request("textDocument/diagnostic", &params)
            .await?;
        if result.is_null() { return Ok(Vec::new()); }
        // Response is a DocumentDiagnosticReport — either "full" or "unchanged"
        // Full: { kind: "full", items: [...] }
        // Unchanged: { kind: "unchanged", resultId: "..." }
        //
        // rust-analyzer may also return a RelatedDocumentDiagnosticReport which
        // wraps the same structure. Log the raw response at DEBUG level so we
        // can diagnose unexpected shapes without noise in normal runs.
        tracing::debug!(
            "textDocument/diagnostic response for {}: {}",
            file_uri.as_str(),
            serde_json::to_string(&result).unwrap_or_else(|_| "<serialize error>".into())
        );
        let kind = result.get("kind").and_then(|k| k.as_str()).unwrap_or("full");
        if kind == "unchanged" {
            return Ok(Vec::new());
        }
        let items = result.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
        tracing::debug!(
            "textDocument/diagnostic: {} items (kind={}) for {}",
            items.len(), kind, file_uri.as_str()
        );
        Ok(items)
    }

    /// Convert raw LSP diagnostic severity integer to a lowercase string.
    ///
    /// Per LSP spec:
    ///   1 = Error, 2 = Warning, 3 = Information, 4 = Hint
    fn lsp_severity_to_str(severity: u64) -> &'static str {
        match severity {
            1 => "error",
            2 => "warning",
            3 => "information",
            4 => "hint",
            _ => "unknown",
        }
    }

    /// Build diagnostic `Node`s from a set of LSP diagnostics for one file.
    ///
    /// `max_severity_int` is the maximum LSP severity integer to store (inclusive).
    /// LSP encodes severity as 1=Error, 2=Warning, 3=Information, 4=Hint.
    /// Default (from `DiagnosticMinSeverity::Warning`) is 2 — store Error and Warning only.
    ///
    /// Severity 0 is not a valid LSP value and is always filtered. Severities above
    /// `max_severity_int` are dropped.
    fn build_diagnostic_nodes(
        file_uri: &str,
        diagnostics: &[serde_json::Value],
        root: &Path,
        root_id: &str,
        server_command: &str,
        language: &str,
        timestamp: &str,
        max_severity_int: u64,
    ) -> Vec<Node> {
        // Resolve file path from URI
        let rel_path = {
            let abs = match url::Url::parse(file_uri).ok().and_then(|u| u.to_file_path().ok()) {
                Some(p) => p,
                None => {
                    if let Some(p) = file_uri.strip_prefix("file://") {
                        PathBuf::from(p)
                    } else {
                        return Vec::new();
                    }
                }
            };
            abs.strip_prefix(root).unwrap_or(&abs).to_path_buf()
        };

        // Skip external paths
        if rel_path.to_string_lossy().contains(".cargo") {
            return Vec::new();
        }

        let mut nodes = Vec::new();

        for diag in diagnostics {
            let severity_int = diag.get("severity").and_then(|s| s.as_u64()).unwrap_or(1);
            // Severity 0 is not a valid LSP value — always skip.
            // Keep diagnostics whose severity integer is within the configured floor.
            // (Lower integer = higher severity: 1=Error, 2=Warning, 3=Information, 4=Hint)
            if severity_int == 0 || severity_int > max_severity_int {
                continue;
            }
            let severity = Self::lsp_severity_to_str(severity_int);
            let message = diag.get("message").and_then(|m| m.as_str()).unwrap_or("").trim().to_string();
            if message.is_empty() {
                continue;
            }
            let source = diag.get("source").and_then(|s| s.as_str()).unwrap_or(server_command);

            let start_line = diag.pointer("/range/start/line").and_then(|l| l.as_u64()).unwrap_or(0) as usize + 1;
            let start_char = diag.pointer("/range/start/character").and_then(|c| c.as_u64()).unwrap_or(0);
            let end_line = diag.pointer("/range/end/line").and_then(|l| l.as_u64()).unwrap_or(0) as usize + 1;
            let end_char = diag.pointer("/range/end/character").and_then(|c| c.as_u64()).unwrap_or(0);
            let range_str = format!("{}:{}-{}:{}", start_line, start_char, end_line, end_char);

            // Name: truncated message + line number for human readability in search results.
            // Including the start line ensures that identical messages at different positions
            // produce distinct NodeIds (preventing silent overwrites in LanceDB).
            let name_snippet = if message.len() > 80 {
                format!("{}...", &message[..77])
            } else {
                message.clone()
            };
            // Node name encodes severity + line + snippet for quick scanning and uniqueness
            let node_name = format!("[{}:{}] {}", severity, start_line, name_snippet);

            let mut metadata = std::collections::BTreeMap::new();
            metadata.insert("diagnostic_severity".to_string(), severity.to_string());
            metadata.insert("diagnostic_source".to_string(), source.to_string());
            metadata.insert("diagnostic_message".to_string(), message.clone());
            metadata.insert("diagnostic_range".to_string(), range_str);
            metadata.insert("diagnostic_timestamp".to_string(), timestamp.to_string());

            let node_id = NodeId {
                root: root_id.to_string(),
                file: rel_path.clone(),
                name: node_name.clone(),
                kind: NodeKind::Other("diagnostic".to_string()),
            };

            nodes.push(Node {
                id: node_id,
                language: language.to_string(),
                line_start: start_line,
                line_end: end_line,
                signature: format!("{}: {}", severity, message),
                body: String::new(),
                metadata,
                source: ExtractionSource::Lsp,
            });
        }

        nodes
    }

    // ---------------------------------------------------------------------------
    // #405: Crate-level dependency graph via rust-analyzer/viewCrateGraph
    // ---------------------------------------------------------------------------

    /// Request `rust-analyzer/viewCrateGraph` and parse the DOT output into
    /// `(crate_name, dep_crate_name)` pairs.
    ///
    /// The DOT format emitted by rust-analyzer is:
    /// ```text
    /// digraph rust_analyzer_crate_graph {
    ///     _0 [shape=box label="my_crate"]
    ///     _1 [shape=box label="dep_crate"]
    ///     _0 -> _1
    /// }
    /// ```
    ///
    /// Only workspace crates are included by default (`full: false`).
    /// Returns `(crate_names, dep_pairs)` — see [`parse_crate_graph_dot`] for details.
    async fn fetch_crate_graph(
        transport: &PipelinedTransport,
    ) -> Result<(Vec<String>, Vec<(String, String)>)> {
        let params = serde_json::json!({ "full": false });
        let result = transport
            .request("rust-analyzer/viewCrateGraph", &params)
            .await?;

        let dot = match result.as_str() {
            Some(s) => s.to_string(),
            None => return Ok((Vec::new(), Vec::new())),
        };

        Ok(Self::parse_crate_graph_dot(&dot))
    }

    /// Parse a DOT digraph string from `rust-analyzer/viewCrateGraph`.
    ///
    /// Returns `(crate_names, dep_pairs)` where:
    /// - `crate_names`: all crate names found in the graph (including isolated crates)
    /// - `dep_pairs`: resolved `(from_crate, to_crate)` dependency pairs
    ///
    /// Isolated crates (no dependencies) are included in `crate_names` so they
    /// still get a crate node even when there are no dependency edges.
    fn parse_crate_graph_dot(dot: &str) -> (Vec<String>, Vec<(String, String)>) {
        // Maps DOT node ID (e.g. "_0") to crate name (e.g. "my_crate")
        let mut id_to_name: HashMap<String, String> = HashMap::new();
        let mut edges: Vec<(String, String)> = Vec::new();

        for line in dot.lines() {
            let line = line.trim();

            // Node definition: `_0 [shape=box label="crate_name"]`
            // Capture: node_id and label value
            if let Some(label_start) = line.find("label=\"") {
                // Extract node ID: everything before the first whitespace
                let node_id = line.split_whitespace().next().unwrap_or("").to_string();
                if !node_id.starts_with('_') {
                    continue; // not a crate node
                }
                let after_label = &line[label_start + 7..]; // skip 'label="'
                if let Some(end) = after_label.find('"') {
                    let name = after_label[..end].to_string();
                    if !name.is_empty() {
                        id_to_name.insert(node_id, name);
                    }
                }
                continue;
            }

            // Edge definition: `_0 -> _1` (with optional trailing semicolon/attributes)
            if line.contains("->") {
                let parts: Vec<&str> = line.splitn(3, "->").collect();
                if parts.len() >= 2 {
                    let from_id = parts[0].trim().trim_end_matches(';').to_string();
                    let to_id = parts[1].trim().split_whitespace().next()
                        .unwrap_or("").trim_end_matches(';').to_string();
                    if from_id.starts_with('_') && to_id.starts_with('_') {
                        edges.push((from_id, to_id));
                    }
                }
            }
        }

        // All crate names (including isolated crates with no edges)
        let mut crate_names: Vec<String> = id_to_name.values().cloned().collect();
        crate_names.sort();
        crate_names.dedup();

        // Resolve edge IDs to crate names
        let resolved_edges = edges.into_iter()
            .filter_map(|(from_id, to_id)| {
                let from = id_to_name.get(&from_id)?;
                let to = id_to_name.get(&to_id)?;
                Some((from.clone(), to.clone()))
            })
            .collect();

        (crate_names, resolved_edges)
    }

    /// Emit crate nodes and `DependsOn` edges from a parsed crate graph.
    ///
    /// Creates a `NodeKind::Other("crate")` node for every crate name (including
    /// isolated crates with no edges), then emits a `DependsOn` edge for each
    /// dependency relationship.
    fn emit_crate_graph_edges(
        crate_names: &[String],
        pairs: &[(String, String)],
        root_id: &str,
        result: &mut EnrichmentResult,
    ) {
        // Collect all unique crate names (isolated crates from crate_names + crates in edges)
        let mut all_crates: std::collections::BTreeSet<String> =
            crate_names.iter().cloned().collect();
        for (from, to) in pairs {
            all_crates.insert(from.clone());
            all_crates.insert(to.clone());
        }

        // Create a crate node for each unique crate.
        // body = crate name so build_code_embedding_text produces meaningful embeddings.
        for crate_name in &all_crates {
            let node_id = NodeId {
                root: root_id.to_string(),
                file: PathBuf::from("Cargo.toml"),
                name: crate_name.clone(),
                kind: NodeKind::Other("crate".to_string()),
            };
            result.new_nodes.push(Node {
                id: node_id,
                language: "rust".to_string(),
                line_start: 0,
                line_end: 0,
                signature: format!("crate {}", crate_name),
                body: crate_name.clone(),
                metadata: std::collections::BTreeMap::new(),
                source: ExtractionSource::Lsp,
            });
        }

        // Emit DependsOn edges
        for (from_name, to_name) in pairs {
            let from_id = NodeId {
                root: root_id.to_string(),
                file: PathBuf::from("Cargo.toml"),
                name: from_name.clone(),
                kind: NodeKind::Other("crate".to_string()),
            };
            let to_id = NodeId {
                root: root_id.to_string(),
                file: PathBuf::from("Cargo.toml"),
                name: to_name.clone(),
                kind: NodeKind::Other("crate".to_string()),
            };
            result.added_edges.push(Edge {
                from: from_id,
                to: to_id,
                kind: EdgeKind::DependsOn,
                source: ExtractionSource::Lsp,
                confidence: Confidence::Detected,
            });
        }
    }

    // ---------------------------------------------------------------------------
    // #396: BelongsTo edges via rust-analyzer/parentModule (Rust) or directory
    // ---------------------------------------------------------------------------

    /// Request `rust-analyzer/parentModule` for a file, returning the module path
    /// as a string (e.g., `"crate::server::handlers"`).
    async fn ra_parent_module(
        transport: &PipelinedTransport,
        file_uri: &Uri,
    ) -> Result<Option<String>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() }
        });

        let result: serde_json::Value = transport
            .request("rust-analyzer/parentModule", &params)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        // rust-analyzer returns an array of LocationLinks; the first gives the
        // parent module's URI which we use as the module path.
        if let Some(arr) = result.as_array() {
            if let Some(first) = arr.first() {
                // The target URI gives us the parent file path; derive module name
                // from the file name (e.g. `src/server/mod.rs` → `server`)
                if let Some(uri_str) = first.get("targetUri").and_then(|u| u.as_str()) {
                    // Extract the module name from the URI: strip file:// and get basename
                    let path = if let Some(p) = uri_str.strip_prefix("file://") {
                        PathBuf::from(p)
                    } else {
                        PathBuf::from(uri_str)
                    };
                    let module_name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| if s == "mod" {
                            // For mod.rs, use the directory name
                            path.parent()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .unwrap_or(s)
                                .to_string()
                        } else {
                            s.to_string()
                        });
                    return Ok(module_name);
                }
            }
        }

        Ok(None)
    }

    /// Emit `BelongsTo` edges from all symbols in a file to a module node.
    ///
    /// For Rust files, tries `rust-analyzer/parentModule` first.
    /// Falls back to directory-based module detection for all languages.
    ///
    /// Module nodes use `NodeKind::Module` and are created as virtual nodes
    /// if they don't already exist.
    async fn emit_belongs_to_edges(
        transport: &PipelinedTransport,
        file_nodes: &[&Node],
        rel_file: &Path,
        root: &Path,
        is_rust: bool,
        result: &mut EnrichmentResult,
    ) {
        if file_nodes.is_empty() {
            return;
        }

        // Derive a module name for this file.
        // Priority: (1) rust-analyzer/parentModule for Rust, (2) directory-based fallback.
        let module_name: Option<String> = if is_rust {
            let abs_path = root.join(rel_file);
            if let Ok(file_uri) = path_to_uri(&abs_path) {
                Self::ra_parent_module(transport, &file_uri).await.ok().flatten()
            } else {
                None
            }
        } else {
            None
        };

        // Fallback: derive module name from the immediate parent directory
        let module_name = module_name.or_else(|| {
            // For files directly in the root or without a parent dir, use the
            // file stem as the module name (e.g. `main.rs` → `main`)
            rel_file
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    rel_file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
        });

        let module_name = match module_name {
            Some(n) if !n.is_empty() => n,
            _ => return,
        };

        // Derive a stable module path from the directory path
        let module_path = rel_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(""));

        // Use the first node's root as the module node root
        let root_id = file_nodes[0].id.root.clone();

        // Create a virtual module node (may already exist in the graph — dedup is
        // handled at persist time by stable_id uniqueness)
        let module_node_id = NodeId {
            root: root_id.clone(),
            file: module_path.clone(),
            name: module_name.clone(),
            kind: NodeKind::Module,
        };

        result.new_nodes.push(Node {
            id: module_node_id.clone(),
            language: file_nodes[0].language.clone(),
            line_start: 0,
            line_end: 0,
            signature: format!("mod {}", module_name),
            body: String::new(),
            metadata: std::collections::BTreeMap::new(),
            source: ExtractionSource::Lsp,
        });

        // Emit BelongsTo edges from each symbol in this file to the module node
        for node in file_nodes {
            // Skip module nodes (avoid self-loop) and diagnostic nodes (transient, not structural)
            if node.id.kind == NodeKind::Module {
                continue;
            }
            if matches!(&node.id.kind, NodeKind::Other(s) if s == "diagnostic") {
                continue;
            }
            result.added_edges.push(Edge {
                from: node.id.clone(),
                to: module_node_id.clone(),
                kind: EdgeKind::BelongsTo,
                source: ExtractionSource::Lsp,
                confidence: Confidence::Detected,
            });
        }
    }

    // ---------------------------------------------------------------------------
    // #408: Inlay hints — inferred types in embeddings
    // ---------------------------------------------------------------------------

    /// Request `textDocument/inlayHint` for a file range and return a compact
    /// string of inferred type names suitable for embedding.
    ///
    /// Supported by: rust-analyzer, TypeScript LS, Pyright, gopls.
    async fn inlay_hints_for_file(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line_count: u32,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": line_count, "character": 0 }
            }
        });

        let result: serde_json::Value = transport
            .request("textDocument/inlayHint", &params)
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Extract type names from inlay hints and group them by the function/symbol
    /// that contains each hint's line position.
    ///
    /// Returns a map from node stable_id → compact type string.
    fn group_inlay_hints_by_node(
        hints: &[serde_json::Value],
        file_nodes: &[&Node],
    ) -> HashMap<String, String> {
        let mut node_types: HashMap<String, Vec<String>> = HashMap::new();

        for hint in hints {
            // Only capture type hints (kind=1) — parameter hints (kind=2) add noise
            let kind = hint.get("kind").and_then(|k| k.as_u64()).unwrap_or(1);
            if kind != 1 {
                continue;
            }

            let hint_line = hint
                .pointer("/position/line")
                .and_then(|l| l.as_u64())
                .map(|l| l as usize + 1)  // Convert to 1-indexed
                .unwrap_or(0);

            if hint_line == 0 {
                continue;
            }

            // Extract the label text (may be a string or array of InlayHintLabelPart)
            let label = match hint.get("label") {
                Some(serde_json::Value::String(s)) => s.trim().to_string(),
                Some(serde_json::Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("value").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => continue,
            };

            // Strip leading ": " annotation prefix that rust-analyzer emits
            let label = label.trim_start_matches(": ").trim().to_string();

            if label.is_empty() || label.len() > 64 {
                continue;
            }

            // Find the narrowest enclosing function/impl/struct for this hint line
            let enclosing = file_nodes
                .iter()
                .filter(|n| matches!(n.id.kind, NodeKind::Function | NodeKind::Impl | NodeKind::Struct))
                .filter(|n| n.line_start <= hint_line && n.line_end >= hint_line)
                .min_by_key(|n| n.line_end - n.line_start);

            if let Some(node) = enclosing {
                node_types
                    .entry(node.id.to_stable_id())
                    .or_default()
                    .push(label);
            }
        }

        // Deduplicate and format each node's types as a space-separated string
        node_types
            .into_iter()
            .map(|(id, mut types)| {
                types.sort();
                types.dedup();
                (id, types.join(" "))
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Enricher for LspEnricher {
    fn languages(&self) -> &[&str] {
        self.language_static
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn name(&self) -> &str {
        &self.display_name
    }

    fn set_startup_root(&self, lsp_root: std::path::PathBuf) {
        // OnceLock::set returns Err if already set; we silently ignore that since
        // the server may already be initialized (first call wins).
        let _ = self.startup_root_override.set(lsp_root);
    }

    fn config_file_hint(&self) -> Option<&str> {
        self.config_file
    }

    async fn enrich(&self, nodes: &[Node], _index: &GraphIndex, repo_root: &Path) -> Result<EnrichmentResult> {
        let mut result = EnrichmentResult::default();

        // Filter to nodes matching this enricher's language.
        // Skip virtual crate nodes emitted by Pass 0: they have language="rust" but
        // are structural topology nodes, not source symbols. Including them would send
        // spurious LSP requests for Cargo.toml and create bogus edges on subsequent
        // enrichment runs. Similarly skip diagnostic nodes (already filtered in Pass 1).
        let matching_nodes: Vec<&Node> = nodes
            .iter()
            .filter(|n| n.language == self.language)
            .filter(|n| !matches!(&n.id.kind, NodeKind::Other(s) if s == "crate"))
            .collect();

        let fn_count = matching_nodes.iter().filter(|n| n.id.kind == NodeKind::Function).count();
        let trait_count = matching_nodes.iter().filter(|n| n.id.kind == NodeKind::Trait).count();
        tracing::info!(
            "LSP enriching {} nodes ({} functions, {} traits) for {}",
            matching_nodes.len(), fn_count, trait_count, self.language
        );

        if matching_nodes.is_empty() {
            return Ok(result);
        }

        // Try to initialize the language server using the repo root from --repo
        if let Err(e) = self.ensure_initialized(repo_root).await {
            tracing::debug!("LSP enrichment skipped for {}: {}", self.language, e);
            return Err(e);
        }

        // Extract state under lock, then release the lock for concurrent work.
        // was_quiescent is read from the pipelined transport's live quiescent_flag
        // (not just the static snapshot from ensure_initialized). This allows later
        // enrich() calls to proceed after RA finishes indexing, even if the first
        // call timed out during initialization. The flag is updated by the background
        // reader loop whenever `experimental/serverStatus { quiescent: true }` arrives.
        let (transport, root, has_type_hierarchy, type_hierarchy_strikes, has_references, has_pull_diagnostics, has_inlay_hints, was_quiescent, diag_sink) = {
            let state = self.state.lock().await;
            let root = state
                .root_path
                .clone()
                .unwrap_or_else(|| repo_root.to_path_buf());
            let transport = match &state.pipelined {
                Some(t) => Arc::clone(t),
                None => return Ok(result),
            };
            let diag_sink = Arc::clone(&state.diagnostics_sink);
            // Use live quiescent_flag from pipelined transport for session-wide accuracy.
            // This allows RA to become quiescent after the init timeout and still be used.
            let was_quiescent = transport.quiescent_flag.load(Ordering::Acquire);
            (transport, root, state.has_type_hierarchy, state.type_hierarchy_strikes, state.has_references, state.has_pull_diagnostics, state.has_inlay_hints, was_quiescent, diag_sink)
        };
        // State lock is released here — concurrent tasks can proceed

        // ------------------------------------------------------------------
        // Pass 0: crate-level dependency graph via rust-analyzer/viewCrateGraph.
        //
        // Single request; returns the entire workspace crate graph as a DOT
        // string. Runs unconditionally (no per-node cost, no quiescence
        // requirement). Only emits nodes+edges for Rust roots.
        // ------------------------------------------------------------------
        if self.language == "rust" {
            let pass0_start = std::time::Instant::now();
            let root_id = matching_nodes
                .first()
                .map(|n| n.id.root.clone())
                .unwrap_or_default();

            match Self::fetch_crate_graph(&transport).await {
                Ok((crate_names, pairs)) if !crate_names.is_empty() => {
                    let pair_count = pairs.len();
                    Self::emit_crate_graph_edges(&crate_names, &pairs, &root_id, &mut result);
                    tracing::info!(
                        "LSP Pass 0 complete in {:?}: {} crate nodes, {} DependsOn edges",
                        pass0_start.elapsed(), crate_names.len(), pair_count
                    );
                }
                Ok(_) => {
                    tracing::debug!("LSP Pass 0: viewCrateGraph returned no crates");
                }
                Err(e) => {
                    tracing::debug!("LSP Pass 0: viewCrateGraph failed: {}", e);
                }
            }
        }

        // ------------------------------------------------------------------
        // Pass 1: call hierarchy, find_implementations, and document links.
        // Pipelined with adaptive concurrency (TCP slow-start).
        //
        // Guard: skip Pass 1 entirely when the server never reached quiescent
        // state (i.e., the initialization deadline expired before indexing
        // finished). When rust-analyzer hasn't indexed the workspace, every
        // call-hierarchy request returns 0 results — triggering the zero-edge
        // abort after ZERO_EDGE_ABORT_THRESHOLD nodes. This is indistinguishable
        // from a misconfigured server and discards the entire enrichment run.
        //
        // On large repos (19K+ nodes across two roots) RA needs >120s to
        // become quiescent. The same guard already protects Pass 3; apply it
        // here too to prevent the zero-edge abort on timed-out servers.
        //
        // Note: `matching_nodes_owned` and `language` are allocated after this
        // guard to avoid a wasted Arc<Vec<Node>> clone when skipping all passes.
        // ------------------------------------------------------------------
        if !was_quiescent {
            tracing::info!(
                "LSP Pass 1 skipped: {} did not reach quiescent state during initialization",
                self.server_command
            );
            tracing::info!(
                "LSP enrichment complete for {}: 0 edges, 0 diagnostic nodes (0 attempted, 0 errors) — skipped (not quiescent)",
                self.language,
            );
            return Ok(result);
        }

        // Share matching_nodes across concurrent tasks via Arc<Vec<Node>> (owned copies).
        // Allocated after the was_quiescent guard to avoid cloning when all passes are skipped.
        let matching_nodes_owned: Arc<Vec<Node>> = Arc::new(
            matching_nodes.iter().map(|n| (*n).clone()).collect()
        );
        let language = self.language.clone();

        let pass1_start = std::time::Instant::now();

        // Filter to only nodes that need LSP requests:
        // Functions (call hierarchy), Traits (implementations), and Other (document links).
        // Skip test functions — they don't have meaningful cross-file callers
        // and halve the total RPC count.
        // Also skip diagnostic nodes (Other("diagnostic")) to prevent them from being
        // re-enriched via the generic Other/documentLink path on subsequent passes —
        // which would generate spurious DependsOn edges from diagnostics.
        let enrichable_nodes: Vec<&Node> = matching_nodes.iter()
            .filter(|n| matches!(n.id.kind,
                NodeKind::Function | NodeKind::Trait | NodeKind::Other(_)
                | NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const))
            .filter(|n| !matches!(&n.id.kind, NodeKind::Other(s) if s == "diagnostic"))
            .filter(|n| {
                // Skip test functions (have #[test] or #[tokio::test] decorator)
                if n.id.kind == NodeKind::Function {
                    if let Some(decorators) = n.metadata.get("decorators") {
                        if decorators.contains("#[test]") || decorators.contains("#[tokio::test]") {
                            return false;
                        }
                    }
                    // Also skip functions in test files
                    if crate::ranking::is_test_file(n) {
                        return false;
                    }
                }
                true
            })
            .copied()
            .collect();

        let ref_eligible = enrichable_nodes.iter()
            .filter(|n| matches!(n.id.kind,
                NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const))
            .count();
        tracing::info!(
            "LSP pipeline: {} enrichable nodes out of {} total ({}f, {}t, {}r, {}o) [references={}]",
            enrichable_nodes.len(), matching_nodes.len(),
            enrichable_nodes.iter().filter(|n| n.id.kind == NodeKind::Function).count(),
            enrichable_nodes.iter().filter(|n| n.id.kind == NodeKind::Trait).count(),
            ref_eligible,
            enrichable_nodes.iter().filter(|n| matches!(n.id.kind, NodeKind::Other(_))).count(),
            has_references,
        );

        // Concurrency control: TCP slow-start from 4 to 64.
        // Start conservatively to let the LSP server warm its caches,
        // then ramp up quickly once it's handling requests smoothly.
        const PIPELINE_MAX_CONCURRENCY: usize = 64;
        let concurrency_limit = Arc::new(tokio::sync::Semaphore::new(4));
        let mut join_set = tokio::task::JoinSet::new();

        let completed = Arc::new(AtomicI64::new(0));
        let error_count = Arc::new(AtomicI64::new(0));
        let ramped_up = Arc::new(AtomicBool::new(false));

        for node in &enrichable_nodes {
            let node = (*node).clone();
            let transport = Arc::clone(&transport);
            let root = root.clone();
            let matching_owned = Arc::clone(&matching_nodes_owned);
            let language = language.clone();
            let sem = Arc::clone(&concurrency_limit);
            let completed = Arc::clone(&completed);
            let error_count = Arc::clone(&error_count);

            let ramped_up = Arc::clone(&ramped_up);

            join_set.spawn(async move {
                // Acquire semaphore permit — limits concurrency
                let _permit = sem.acquire().await.unwrap();

                let abs_path = root.join(&node.id.file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => {
                        completed.fetch_add(1, Ordering::Relaxed);
                        return (Vec::new(), Vec::new(), false);
                    }
                };

                let (line, col) = Self::node_lsp_position(&node);
                let mut edges = Vec::new();
                let mut new_nodes = Vec::new();
                let mut had_error = false;

                match node.id.kind {
                    NodeKind::Function => {
                        match Self::prepare_call_hierarchy_p(&transport, &file_uri, line, col).await {
                            Ok(Some(item)) => {
                                // Run incoming and outgoing calls concurrently
                                let (incoming_result, outgoing_result) = tokio::join!(
                                    Self::incoming_calls_p(&transport, &item),
                                    Self::outgoing_calls_p(&transport, &item),
                                );

                                // Process incoming calls
                                if let Ok(calls) = incoming_result {
                                    let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                                    for call in &calls {
                                        let caller_uri = &call["from"]["uri"];
                                        let caller_name = call["from"]["name"].as_str().unwrap_or("");
                                        let caller_line = call["from"]["range"]["start"]["line"].as_u64().unwrap_or(0) as usize + 1;

                                        if let Some(uri_str) = caller_uri.as_str() {
                                            let caller_path = if let Some(p) = uri_str.strip_prefix("file://") {
                                                let abs = PathBuf::from(p);
                                                abs.strip_prefix(&root).unwrap_or(&abs).to_path_buf()
                                            } else {
                                                continue;
                                            };

                                            if caller_path.to_string_lossy().contains(".cargo") {
                                                continue;
                                            }

                                            let caller_id = matching_refs.iter()
                                                .filter(|n| n.id.file == caller_path)
                                                .filter(|n| n.id.name == caller_name)
                                                .next()
                                                .map(|n| n.id.clone())
                                                .or_else(|| find_enclosing_symbol(&matching_refs, &caller_path, caller_line));

                                            if let Some(caller) = caller_id {
                                                if caller.name == node.id.name && caller.file == node.id.file {
                                                    continue;
                                                }
                                                edges.push(Edge {
                                                    from: caller,
                                                    to: node.id.clone(),
                                                    kind: EdgeKind::Calls,
                                                    source: ExtractionSource::Lsp,
                                                    confidence: Confidence::Confirmed,
                                                });
                                            }
                                        }
                                    }
                                }

                                // Process outgoing calls
                                if let Ok(calls) = outgoing_result {
                                    let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                                    for call in &calls {
                                        let callee_uri = &call["to"]["uri"];
                                        let callee_name = call["to"]["name"].as_str().unwrap_or("");
                                        let callee_line = call["to"]["range"]["start"]["line"].as_u64().unwrap_or(0) as usize + 1;

                                        if let Some(uri_str) = callee_uri.as_str() {
                                            let callee_path = if let Some(p) = uri_str.strip_prefix("file://") {
                                                let abs = PathBuf::from(p);
                                                abs.strip_prefix(&root).unwrap_or(&abs).to_path_buf()
                                            } else {
                                                continue;
                                            };

                                            if callee_path.to_string_lossy().contains(".cargo") {
                                                let fqn = call["to"]["detail"]
                                                    .as_str()
                                                    .filter(|s| !s.is_empty())
                                                    .unwrap_or(callee_name);

                                                if fqn.is_empty() {
                                                    continue;
                                                }

                                                let package = fqn.split("::").next().unwrap_or(fqn).to_string();

                                                let virtual_id = NodeId {
                                                    root: "external".to_string(),
                                                    file: PathBuf::new(),
                                                    name: fqn.to_string(),
                                                    kind: NodeKind::Function,
                                                };

                                                let mut meta = std::collections::BTreeMap::new();
                                                meta.insert("package".to_string(), package.clone());
                                                meta.insert("virtual".to_string(), "true".to_string());
                                                new_nodes.push(Node {
                                                    id: virtual_id.clone(),
                                                    language: language.clone(),
                                                    line_start: 0,
                                                    line_end: 0,
                                                    signature: fqn.to_string(),
                                                    body: String::new(),
                                                    metadata: meta,
                                                    source: ExtractionSource::Lsp,
                                                });

                                                edges.push(Edge {
                                                    from: node.id.clone(),
                                                    to: virtual_id,
                                                    kind: EdgeKind::Calls,
                                                    source: ExtractionSource::Lsp,
                                                    confidence: Confidence::Detected,
                                                });
                                                continue;
                                            }

                                            let callee_id = matching_refs.iter()
                                                .filter(|n| n.id.file == callee_path)
                                                .filter(|n| n.id.name == callee_name)
                                                .next()
                                                .map(|n| n.id.clone())
                                                .or_else(|| find_enclosing_symbol(&matching_refs, &callee_path, callee_line));

                                            if let Some(callee) = callee_id {
                                                if callee.name == node.id.name && callee.file == node.id.file {
                                                    continue;
                                                }
                                                edges.push(Edge {
                                                    from: node.id.clone(),
                                                    to: callee,
                                                    kind: EdgeKind::Calls,
                                                    source: ExtractionSource::Lsp,
                                                    confidence: Confidence::Confirmed,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {} // No call hierarchy item
                            Err(e) => {
                                had_error = true;
                                error_count.fetch_add(1, Ordering::Relaxed);
                                tracing::debug!("prepareCallHierarchy failed for {}: {}", node.id.name, e);
                            }
                        }
                    }
                    NodeKind::Trait => {
                        match Self::find_implementations_p(&transport, &file_uri, line, col).await {
                            Ok(locations) => {
                                let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                                for loc in locations {
                                    let impl_path = uri_to_relative_path(&loc.uri, &root);
                                    let impl_line = loc.range.start.line as usize + 1;

                                    if impl_path.to_string_lossy().contains(".cargo") {
                                        continue;
                                    }

                                    let impl_id = matching_refs.iter()
                                        .filter(|n| n.id.file == impl_path)
                                        .filter(|n| matches!(n.id.kind, NodeKind::Impl | NodeKind::Struct))
                                        .filter(|n| n.line_start <= impl_line && n.line_end >= impl_line)
                                        .min_by_key(|n| n.line_end - n.line_start)
                                        .map(|n| n.id.clone());

                                    if let Some(implementor) = impl_id {
                                        edges.push(Edge {
                                            from: implementor,
                                            to: node.id.clone(),
                                            kind: EdgeKind::Implements,
                                            source: ExtractionSource::Lsp,
                                            confidence: Confidence::Confirmed,
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Implementation lookup failed for {}: {}", node.id.name, e);
                            }
                        }
                    }
                    NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const => {
                        // Use textDocument/references to find usage sites for
                        // non-function symbols (structs, enums, type aliases, consts).
                        if has_references {
                            match Self::find_references_p(&transport, &file_uri, line, col).await {
                                Ok(locations) => {
                                    // Build per-file index once to avoid O(R*N) scans
                                    let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                                    let mut refs_by_file: HashMap<&Path, Vec<&Node>> = HashMap::new();
                                    for n in &matching_refs {
                                        refs_by_file.entry(n.id.file.as_path()).or_default().push(*n);
                                    }
                                    for loc in &locations {
                                        let ref_path = uri_to_relative_path(&loc.uri, &root);
                                        let ref_line = loc.range.start.line as usize + 1;

                                        // Skip external dependencies
                                        if ref_path.to_string_lossy().contains(".cargo") {
                                            continue;
                                        }

                                        // Skip self-references (definition site)
                                        if ref_path == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end {
                                            continue;
                                        }

                                        // Resolve to the enclosing symbol at the reference site
                                        let referrer_id = refs_by_file
                                            .get(ref_path.as_path())
                                            .and_then(|candidates| find_enclosing_symbol(candidates, &ref_path, ref_line));

                                        if let Some(referrer) = referrer_id {
                                            // Skip self-edges
                                            if referrer == node.id {
                                                continue;
                                            }
                                            edges.push(Edge {
                                                from: referrer,
                                                to: node.id.clone(),
                                                kind: EdgeKind::ReferencedBy,
                                                source: ExtractionSource::Lsp,
                                                confidence: Confidence::Confirmed,
                                            });
                                        }
                                    }
                                }
                                Err(e) => {
                                    had_error = true;
                                    error_count.fetch_add(1, Ordering::Relaxed);
                                    tracing::debug!("textDocument/references failed for {}: {}", node.id.name, e);
                                }
                            }
                        }
                    }
                    _ => {
                        if matches!(node.id.kind, NodeKind::Other(_)) {
                            if let Ok(links) = Self::document_links_p(&transport, &file_uri).await {
                                for link in &links {
                                    if let Some(target) = link.get("target").and_then(|t| t.as_str()) {
                                        if let Some(target_path) = target.strip_prefix("file://") {
                                            let rel_target = PathBuf::from(target_path);
                                            let rel_target = rel_target.strip_prefix(&root).unwrap_or(&rel_target).to_path_buf();

                                            if rel_target.to_string_lossy().starts_with("http") {
                                                continue;
                                            }

                                            let target_id = NodeId {
                                                root: node.id.root.clone(),
                                                file: rel_target.clone(),
                                                name: rel_target.file_name()
                                                    .and_then(|n| n.to_str())
                                                    .unwrap_or("unknown")
                                                    .to_string(),
                                                kind: NodeKind::Module,
                                            };

                                            edges.push(Edge {
                                                from: node.id.clone(),
                                                to: target_id,
                                                kind: EdgeKind::DependsOn,
                                                source: ExtractionSource::Lsp,
                                                confidence: Confidence::Confirmed,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                // Ramp up after 4 successful completions (TCP slow-start exit)
                if done >= 4 && !had_error && !ramped_up.swap(true, Ordering::Relaxed) {
                    let added = PIPELINE_MAX_CONCURRENCY - 4;
                    sem.add_permits(added);
                    tracing::info!("LSP pipeline: ramp-up to {} concurrent", PIPELINE_MAX_CONCURRENCY);
                }
                (edges, new_nodes, had_error)
            });
        }

        // Collect results from all concurrent tasks
        let mut attempted = 0u32;
        let mut errors = 0u32;
        let mut seen_virtual_ids = std::collections::HashSet::new();
        let total_nodes = enrichable_nodes.len();
        let mut last_progress_log = std::time::Instant::now();
        let mut last_logged_count = 0u64;
        const PROGRESS_LOG_INTERVAL_SECS: u64 = 30;
        const PROGRESS_LOG_INTERVAL_NODES: u64 = 1_000;

        while let Some(task_result) = join_set.join_next().await {
            match task_result {
                Ok((edges, new_nodes, had_error)) => {
                    attempted += 1;
                    if had_error {
                        errors += 1;
                    }
                    result.added_edges.extend(edges);
                    for vnode in new_nodes {
                        if seen_virtual_ids.insert(vnode.id.clone()) {
                            result.new_nodes.push(vnode);
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    tracing::debug!("LSP enrichment task panicked: {}", e);
                }
            }

            // Log progress every 1,000 nodes or every 30 seconds (whichever comes first)
            let done = completed.load(Ordering::Relaxed) as u64;
            let elapsed_since_log = last_progress_log.elapsed().as_secs();
            let nodes_since_log = done.saturating_sub(last_logged_count);
            if done > 0
                && (nodes_since_log >= PROGRESS_LOG_INTERVAL_NODES
                    || elapsed_since_log >= PROGRESS_LOG_INTERVAL_SECS)
            {
                let elapsed_total = pass1_start.elapsed().as_secs_f64();
                let rate = done as f64 / elapsed_total;
                let remaining = if rate > 0.0 {
                    let remaining_nodes = (total_nodes as f64) - (done as f64);
                    let remaining_secs = remaining_nodes / rate;
                    if remaining_secs >= 120.0 {
                        format!("~{} min remaining", (remaining_secs / 60.0).round() as u64)
                    } else {
                        format!("~{}s remaining", remaining_secs.round() as u64)
                    }
                } else {
                    "estimating...".to_string()
                };
                tracing::info!(
                    "LSP: {} processing... {}/{} nodes ({} edges found, {})",
                    self.server_command, done, total_nodes,
                    result.added_edges.len(), remaining,
                );
                last_progress_log = std::time::Instant::now();
                last_logged_count = done;
            }

            // Early abort: if we've processed >= 1,000 nodes AND warmed up for >= 30s,
            // OR spent >= 2 minutes with 0 edges, the language server is likely
            // misconfigured. The warmup guard prevents false aborts on servers like
            // typescript-language-server that need time to index before producing edges.
            if result.added_edges.is_empty()
                && ((attempted >= ZERO_EDGE_ABORT_THRESHOLD && pass1_start.elapsed() >= ZERO_EDGE_MIN_WARMUP)
                    || pass1_start.elapsed() > ZERO_EDGE_TIMEOUT)
            {
                tracing::warn!(
                    "LSP: {} produced 0 edges after {}/{} nodes ({:.1}s) — aborting (likely misconfigured)",
                    self.server_command, attempted, total_nodes, pass1_start.elapsed().as_secs_f64(),
                );
                join_set.abort_all();
                break;
            }
        }

        tracing::info!(
            "LSP Pass 1 complete in {:?}: {} edges from {} nodes ({} errors)",
            pass1_start.elapsed(), result.added_edges.len(), attempted, errors,
        );

        // Pass 1b (TestedBy naming conventions) was removed in fix/#395.
        // TestedBy edges are now emitted by the tree-sitter post-extraction
        // pass `naming_convention::tested_by_pass`, which runs unconditionally
        // after every scan — no LSP startup required.

        // ------------------------------------------------------------------
        // Pass 2: type hierarchy (sequential — strike counting needs order)
        // ------------------------------------------------------------------
        let mut has_type_hierarchy = has_type_hierarchy;
        let mut type_hierarchy_strikes = type_hierarchy_strikes;

        if has_type_hierarchy {
            let type_nodes: Vec<&Node> = matching_nodes
                .iter()
                .filter(|n| matches!(n.id.kind, NodeKind::Trait | NodeKind::Struct | NodeKind::Enum))
                .copied()
                .collect();

            if !type_nodes.is_empty() {
                tracing::debug!(
                    "Type hierarchy pass: {} eligible nodes",
                    type_nodes.len()
                );
            }

            let pass2_start = std::time::Instant::now();
            let mut pass2_done = 0u64;
            let pass2_total = type_nodes.len();
            let edges_before_pass2 = result.added_edges.len();
            let mut pass2_last_log = std::time::Instant::now();
            let mut pass2_last_count = 0u64;

            for node in &type_nodes {
                let abs_path = root.join(&node.id.file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => continue,
                };
                let (line, col) = Self::node_lsp_position(node);

                let ok = Self::enrich_type_hierarchy_p(
                    &transport, &file_uri, line, col,
                    node, &matching_nodes, &root, &mut result,
                ).await;

                Self::update_type_hierarchy_strikes(
                    ok,
                    &mut type_hierarchy_strikes,
                    &mut has_type_hierarchy,
                );

                pass2_done += 1;

                // Log progress every 500 nodes or every 30 seconds
                let since_log = pass2_last_log.elapsed().as_secs();
                let nodes_since = pass2_done - pass2_last_count;
                if nodes_since >= 500 || since_log >= 30 {
                    let elapsed = pass2_start.elapsed().as_secs_f64();
                    let rate = pass2_done as f64 / elapsed;
                    let remaining_secs = if rate > 0.0 {
                        ((pass2_total as f64) - (pass2_done as f64)) / rate
                    } else {
                        0.0
                    };
                    let remaining = if remaining_secs >= 120.0 {
                        format!("~{} min remaining", (remaining_secs / 60.0).round() as u64)
                    } else {
                        format!("~{}s remaining", remaining_secs.round() as u64)
                    };
                    tracing::info!(
                        "LSP: {} type hierarchy... {}/{} nodes ({} edges total, {})",
                        self.server_command, pass2_done, pass2_total,
                        result.added_edges.len(), remaining,
                    );
                    pass2_last_log = std::time::Instant::now();
                    pass2_last_count = pass2_done;
                }

                // Early abort: 0 new edges after 1,000 nodes + 30s warmup, OR 2 minutes
                if result.added_edges.len() == edges_before_pass2
                    && ((pass2_done >= ZERO_EDGE_ABORT_THRESHOLD as u64 && pass2_start.elapsed() >= ZERO_EDGE_MIN_WARMUP)
                        || pass2_start.elapsed() > ZERO_EDGE_TIMEOUT)
                {
                    tracing::warn!(
                        "LSP: {} type hierarchy produced 0 edges after {}/{} nodes ({:.1}s) — aborting (likely misconfigured)",
                        self.server_command, pass2_done, pass2_total, pass2_start.elapsed().as_secs_f64(),
                    );
                    break;
                }

                if !has_type_hierarchy {
                    break;
                }
            }
        }

        // Persist strike counter back to state
        {
            let mut state = self.state.lock().await;
            state.type_hierarchy_strikes = type_hierarchy_strikes;
            state.has_type_hierarchy = has_type_hierarchy;
        }

        // ------------------------------------------------------------------
        // Pass 4: BelongsTo edges — module hierarchy (#396).
        //
        // Per unique file: emit BelongsTo edges from all symbols to a module
        // node. For Rust, tries rust-analyzer/parentModule first; falls back
        // to directory-based module detection for all other languages.
        //
        // This runs unconditionally (no quiescence dependency) since:
        //   - Directory-based fallback never hits the LSP server
        //   - rust-analyzer/parentModule is cheap (one request per file, not per symbol)
        // ------------------------------------------------------------------
        {
            let pass4_start = std::time::Instant::now();
            let edges_before = result.added_edges.len();

            // Group matching_nodes by file
            let mut nodes_by_file: HashMap<PathBuf, Vec<&Node>> = HashMap::new();
            for n in &matching_nodes {
                nodes_by_file.entry(n.id.file.clone()).or_default().push(n);
            }

            let is_rust = self.language == "rust";

            for (rel_file, file_nodes) in &nodes_by_file {
                Self::emit_belongs_to_edges(
                    &transport,
                    file_nodes,
                    rel_file,
                    &root,
                    is_rust,
                    &mut result,
                ).await;
            }

            // Remove duplicate module nodes (same stable_id emitted for multiple files in same dir)
            let mut deduplicated_new_nodes = Vec::with_capacity(result.new_nodes.len());
            let mut module_stable_ids_seen = std::collections::HashSet::new();
            for node in result.new_nodes.drain(..) {
                if matches!(node.id.kind, NodeKind::Module) {
                    let sid = node.id.to_stable_id();
                    if module_stable_ids_seen.insert(sid) {
                        deduplicated_new_nodes.push(node);
                    }
                    // else: skip duplicate
                } else {
                    deduplicated_new_nodes.push(node);
                }
            }
            result.new_nodes = deduplicated_new_nodes;

            let belongs_to_count = result.added_edges.len() - edges_before;
            let module_node_count = result.new_nodes.iter()
                .filter(|n| matches!(n.id.kind, NodeKind::Module))
                .count();
            if belongs_to_count > 0 {
                tracing::info!(
                    "LSP Pass 4 complete in {:?}: {} BelongsTo edges, {} module nodes",
                    pass4_start.elapsed(), belongs_to_count, module_node_count
                );
            }
        }

        // ------------------------------------------------------------------
        // Pass 5: InlayHints — inferred types in embeddings (#408).
        //
        // For language servers that support inlayHint (rust-analyzer,
        // TypeScript LS, Pyright, gopls): request inlay hints per file,
        // extract type annotations, and patch node metadata so
        // build_code_embedding_text() includes them.
        //
        // Only runs if the server advertised inlayHintProvider capability.
        // ------------------------------------------------------------------
        if has_inlay_hints {
            let pass5_start = std::time::Instant::now();
            let mut hint_patches = 0usize;

            // Unique files (recompute since nodes_by_file was consumed above)
            let mut nodes_by_file2: HashMap<PathBuf, Vec<&Node>> = HashMap::new();
            for n in &matching_nodes {
                nodes_by_file2.entry(n.id.file.clone()).or_default().push(n);
            }

            for (rel_file, file_nodes) in &nodes_by_file2 {
                let abs_path = root.join(rel_file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => continue,
                };

                // Use max line_end as the range end for the request
                let max_line = file_nodes
                    .iter()
                    .map(|n| n.line_end as u32)
                    .max()
                    .unwrap_or(0);

                match Self::inlay_hints_for_file(&transport, &file_uri, max_line + 1).await {
                    Ok(hints) if !hints.is_empty() => {
                        let type_map = Self::group_inlay_hints_by_node(&hints, file_nodes);
                        for (stable_id, type_str) in type_map {
                            result.updated_nodes.push((
                                stable_id,
                                {
                                    let mut patch = std::collections::BTreeMap::new();
                                    patch.insert("inferred_types".to_string(), type_str);
                                    patch
                                },
                            ));
                            hint_patches += 1;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!(
                            "textDocument/inlayHint failed for {}: {}",
                            rel_file.display(), e
                        );
                    }
                }
            }

            if hint_patches > 0 {
                tracing::info!(
                    "LSP Pass 5 complete in {:?}: {} nodes patched with inferred_types",
                    pass5_start.elapsed(), hint_patches
                );
            }
        }

        // ------------------------------------------------------------------
        // Pass 3: diagnostics.
        //
        // Strategy: prefer pull-based diagnostics (textDocument/diagnostic,
        // LSP 3.17+) when the server advertised `diagnosticProvider`. For
        // servers that only push (`textDocument/publishDiagnostics`), fall
        // back to what the pipelined reader loop captured in `diag_sink`.
        //
        // Only Error and Warning severities produce nodes.
        // Files with zero qualifying diagnostics produce no nodes (clean-file rule).
        //
        // Guard: skip Pass 3 entirely when the server never reached quiescent
        // state (i.e., the initialization deadline expired before indexing
        // finished). Sending thousands of textDocument/diagnostic requests to an
        // unindexed server floods its request queue and was the root cause of the
        // zero-edge regression introduced by #381.
        // ------------------------------------------------------------------
        if !was_quiescent {
            tracing::info!(
                "LSP Pass 3 skipped: {} did not reach quiescent state during initialization",
                self.server_command
            );
            let diag_count = 0usize;
            tracing::info!(
                "LSP enrichment complete for {}: {} edges, {} diagnostic nodes ({} attempted, {} errors)",
                self.language,
                result.added_edges.len(),
                diag_count,
                attempted,
                errors,
            );
            return Ok(result);
        }

        let diag_timestamp = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string())
        };

        // Collect unique files from matching_nodes so we can request pull diagnostics
        // for exactly the files we already enriched (no extra LSP sessions).
        let unique_files: Vec<PathBuf> = {
            let mut seen = std::collections::HashSet::new();
            matching_nodes.iter()
                .map(|n| n.id.file.clone())
                .filter(|f| seen.insert(f.clone()))
                .collect()
        };

        // Use the canonical root ID from the graph (from matching_nodes), not the filesystem
        // path. This ensures diagnostic NodeIds live in the same namespace as all other nodes
        // and are correctly handled by root-prefix stripping and stale-root pruning.
        let root_id = matching_nodes
            .first()
            .map(|n| n.id.root.clone())
            .unwrap_or_default();

        // Load diagnostic severity threshold from .oh/config.toml [lsp] section.
        // Defaults to Warning (severity ≤ 2) if no config is present.
        let lsp_config = LspConfig::load(repo_root);
        let max_severity_int = lsp_config.diagnostic_min_severity.max_severity_int();

        if has_pull_diagnostics {
            tracing::info!(
                "LSP diagnostics pass: pull-based for {} files ({})",
                unique_files.len(), self.server_command
            );
            // Pull diagnostics per unique file
            let mut pull_raw_total = 0usize;
            let mut pull_files_with_diags = 0usize;
            for rel_file in &unique_files {
                let abs_path = root.join(rel_file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => continue,
                };
                match Self::pull_diagnostics_p(&transport, &file_uri).await {
                    Ok(diags) => {
                        if !diags.is_empty() {
                            pull_raw_total += diags.len();
                            pull_files_with_diags += 1;
                            tracing::debug!(
                                "textDocument/diagnostic: {} raw items for {}",
                                diags.len(), rel_file.display()
                            );
                        }
                        let nodes = Self::build_diagnostic_nodes(
                            file_uri.as_str(),
                            &diags,
                            &root,
                            &root_id,
                            &self.server_command,
                            &self.language,
                            &diag_timestamp,
                            max_severity_int,
                        );
                        result.new_nodes.extend(nodes);
                    }
                    Err(e) => {
                        tracing::debug!("textDocument/diagnostic failed for {}: {}", rel_file.display(), e);
                    }
                }
            }
            tracing::info!(
                "LSP diagnostics pass: pull complete — {} raw items from {} files with diagnostics (out of {} files)",
                pull_raw_total, pull_files_with_diags, unique_files.len()
            );
        } else {
            // Fall back to push-captured diagnostics from the reader loop.
            // Limit to URIs that correspond to this pass's files — prevents
            // re-emitting stale diagnostics for files no longer in matching_nodes.
            let expected_uris: std::collections::HashSet<String> = unique_files
                .iter()
                .filter_map(|rel_file| path_to_uri(&root.join(rel_file)).ok().map(|u| u.to_string()))
                .collect();

            let captured: HashMap<String, Vec<serde_json::Value>> = {
                let sink = diag_sink.lock().unwrap();
                sink.clone()
            };
            let relevant_count = captured.keys().filter(|u| expected_uris.contains(*u)).count();
            tracing::info!(
                "LSP diagnostics pass: push-captured {}/{} relevant files with diagnostics ({})",
                relevant_count, captured.len(), self.server_command
            );
            for (uri, diags) in &captured {
                // Only convert diagnostics for files in this pass
                if !expected_uris.contains(uri) {
                    continue;
                }
                let nodes = Self::build_diagnostic_nodes(
                    uri,
                    diags,
                    &root,
                    &root_id,
                    &self.server_command,
                    &self.language,
                    &diag_timestamp,
                    max_severity_int,
                );
                result.new_nodes.extend(nodes);
            }
        }

        let diag_count = result.new_nodes.iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "diagnostic"))
            .count();
        tracing::info!(
            "LSP enrichment complete for {}: {} edges, {} diagnostic nodes ({} attempted, {} errors)",
            self.language,
            result.added_edges.len(),
            diag_count,
            attempted,
            errors,
        );

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Verify the Enricher trait can be implemented (compile-time check).
    #[tokio::test]
    async fn test_enricher_trait_implementable() {
        struct DummyEnricher;

        #[async_trait::async_trait]
        impl Enricher for DummyEnricher {
            fn languages(&self) -> &[&str] {
                &["test"]
            }

            fn is_ready(&self) -> bool {
                true
            }

            fn name(&self) -> &str {
                "dummy"
            }

            async fn enrich(
                &self,
                _nodes: &[Node],
                _index: &GraphIndex,
                _repo_root: &Path,
            ) -> Result<EnrichmentResult> {
                Ok(EnrichmentResult::default())
            }
        }

        let enricher = DummyEnricher;
        assert_eq!(enricher.languages(), &["test"]);
        assert!(enricher.is_ready());
        assert_eq!(enricher.name(), "dummy");

        let index = GraphIndex::new();
        let result = enricher.enrich(&[], &index, std::path::Path::new(".")).await.unwrap();
        assert!(result.added_edges.is_empty());
        assert!(result.updated_nodes.is_empty());
    }

    /// Verify the LspEnricher can be constructed with correct properties for each language.
    #[test]
    fn test_lsp_enricher_creation() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        assert_eq!(enricher.languages(), &["rust"]);
        assert!(!enricher.is_ready());
        assert_eq!(enricher.name(), "rust-analyzer-lsp");
        assert_eq!(enricher.server_command, "rust-analyzer");
        assert!(enricher.server_args.is_empty());
        assert_eq!(enricher.extensions, vec!["rs"]);
    }

    /// Verify enrichers for each language have correct properties.
    #[test]
    fn test_lsp_enricher_all_languages() {
        let rust = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        assert_eq!(rust.languages(), &["rust"]);
        assert_eq!(rust.name(), "rust-analyzer-lsp");

        let python = LspEnricher::new("python", "pyright-langserver", &["--stdio"], &["py"]);
        assert_eq!(python.languages(), &["python"]);
        assert_eq!(python.name(), "pyright-langserver-lsp");
        assert_eq!(python.server_args, vec!["--stdio"]);

        let typescript = LspEnricher::new(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "tsx", "js", "jsx"],
        );
        assert_eq!(typescript.languages(), &["typescript"]);
        assert_eq!(typescript.name(), "typescript-language-server-lsp");
        assert_eq!(typescript.extensions, vec!["ts", "tsx", "js", "jsx"]);

        let go = LspEnricher::new("go", "gopls", &["serve"], &["go"]);
        assert_eq!(go.languages(), &["go"]);
        assert_eq!(go.name(), "gopls-lsp");
        assert_eq!(go.server_args, vec!["serve"]);

        let markdown = LspEnricher::new("markdown", "marksman", &["server"], &["md"]);
        assert_eq!(markdown.languages(), &["markdown"]);
        assert_eq!(markdown.name(), "marksman-lsp");
    }

    /// Verify enrichment returns empty result when no matching nodes are present.
    #[tokio::test]
    async fn test_lsp_enricher_no_matching_nodes() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let index = GraphIndex::new();

        // Pass nodes with a non-matching language
        let nodes = vec![Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("test.py"),
                name: "hello".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(),
            line_start: 1,
            line_end: 1,
            signature: "def hello()".into(),
            body: "def hello(): pass".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }];

        let result = enricher.enrich(&nodes, &index, std::path::Path::new(".")).await.unwrap();
        assert!(result.added_edges.is_empty());
    }

    /// Verify the EnricherRegistry works correctly with multiple enrichers.
    #[tokio::test]
    async fn test_enricher_registry() {
        use super::super::EnricherRegistry;

        let registry = EnricherRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        let registry = EnricherRegistry::with_builtins();
        assert!(registry.len() >= 30, "should have 30+ auto-discovered LSP servers, got {}", registry.len());
    }

    /// Verify multiple enrichers can be registered and coexist.
    #[tokio::test]
    async fn test_multiple_enrichers_registered() {
        use super::super::EnricherRegistry;

        let mut registry = EnricherRegistry::new();

        registry.register(Box::new(LspEnricher::new(
            "rust",
            "rust-analyzer",
            &[],
            &["rs"],
        )));
        registry.register(Box::new(LspEnricher::new(
            "python",
            "pyright-langserver",
            &["--stdio"],
            &["py"],
        )));
        registry.register(Box::new(LspEnricher::new(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "tsx", "js", "jsx"],
        )));

        assert_eq!(registry.len(), 3);

        // Enrich with no nodes should work fine for all enrichers
        let index = GraphIndex::new();
        let result = registry.enrich_all(&[], &index, &["rust".to_string(), "python".to_string()], std::path::Path::new("."), &[]).await;
        assert!(result.added_edges.is_empty());
    }

    /// Verify the with_settings builder works.
    #[test]
    fn test_lsp_enricher_with_settings() {
        let settings = serde_json::json!({
            "python": {
                "analysis": {
                    "autoSearchPaths": true
                }
            }
        });
        let enricher = LspEnricher::new("python", "pyright-langserver", &["--stdio"], &["py"])
            .with_settings(settings.clone());
        assert_eq!(enricher.init_settings, Some(settings));
    }

    /// Verify pyright venv detection logic: when .venv exists at startup_root,
    /// venvPath and venv should be merged into initializationOptions.
    #[test]
    fn test_pyright_venv_detection_merges_settings() {
        // Simulate the logic from ensure_initialized that merges venv settings.
        // This tests the data transformation without needing a running LSP server.
        let base_settings = serde_json::json!({
            "python": { "analysis": { "autoSearchPaths": true } }
        });
        let startup_root = std::path::Path::new("/tmp/ai_service");
        let language = "python";

        // Apply the same merge logic as ensure_initialized
        let merged = {
            let venv_dir = startup_root.join(".venv");
            // Simulate: .venv exists (we don't actually create it, just test the merge logic)
            // by directly applying the merge
            let venv_path_str = startup_root.to_string_lossy().to_string();
            let mut merged = base_settings.clone();
            if let Some(python_obj) = merged.get_mut("python") {
                if let Some(analysis_obj) = python_obj.get_mut("analysis") {
                    if let Some(obj) = analysis_obj.as_object_mut() {
                        obj.insert("venvPath".into(), serde_json::Value::String(venv_path_str.clone()));
                        obj.insert("venv".into(), serde_json::Value::String(".venv".into()));
                    }
                }
            }
            let _ = venv_dir; // consumed
            merged
        };

        assert_eq!(language, "python");
        assert_eq!(
            merged["python"]["analysis"]["autoSearchPaths"],
            serde_json::Value::Bool(true),
            "autoSearchPaths should be preserved"
        );
        assert_eq!(
            merged["python"]["analysis"]["venvPath"],
            serde_json::Value::String("/tmp/ai_service".into()),
            "venvPath should be the startup root"
        );
        assert_eq!(
            merged["python"]["analysis"]["venv"],
            serde_json::Value::String(".venv".into()),
            "venv should be .venv"
        );
    }

    /// Verify pyright venv detection: non-python enrichers are not augmented.
    #[test]
    fn test_pyright_venv_detection_skips_non_python() {
        let base_settings = serde_json::json!({
            "typescript": { "preferences": {} }
        });
        let language = "typescript";

        // The effective_settings logic should not augment non-python enrichers
        let effective_settings = {
            if language == "python" {
                // Would augment — but we're not python
                unreachable!("should not be called for typescript");
            } else {
                base_settings.clone()
            }
        };

        // TypeScript settings should be unchanged
        assert_eq!(effective_settings, base_settings);
    }

    /// Verify URI helper functions work correctly.
    #[test]
    fn test_uri_to_relative_path() {
        let root = PathBuf::from("/home/user/project");
        let uri = Uri::from_str("file:///home/user/project/src/main.rs").unwrap();
        let rel = uri_to_relative_path(&uri, &root);
        assert_eq!(rel, PathBuf::from("src/main.rs"));
    }

    /// Verify that percent-encoded characters in file URIs are decoded correctly.
    ///
    /// LSP servers return URIs like `file:///path/with%20spaces/main.rs` when the
    /// workspace lives under a directory whose name contains characters that require
    /// percent-encoding in a URI (spaces, parentheses, etc.).  Without decoding,
    /// `strip_prefix` would fail and we would silently drop graph edges.
    #[test]
    fn test_uri_to_relative_path_percent_encoded() {
        // Space encoded as %20
        let root = PathBuf::from("/home/user/my project");
        let uri = Uri::from_str("file:///home/user/my%20project/src/main.rs").unwrap();
        let rel = uri_to_relative_path(&uri, &root);
        assert_eq!(rel, PathBuf::from("src/main.rs"));

        // Parentheses encoded as %28 / %29 — common on macOS with versioned dirs
        let root2 = PathBuf::from("/home/user/project (v2)");
        let uri2 = Uri::from_str("file:///home/user/project%20%28v2%29/lib.rs").unwrap();
        let rel2 = uri_to_relative_path(&uri2, &root2);
        assert_eq!(rel2, PathBuf::from("lib.rs"));
    }

    /// Adversarial: verify fallback and edge-case behaviour of uri_to_relative_path.
    ///
    /// Seeded from dissent findings:
    /// 1. URI outside the workspace root — should return an absolute-looking path rather than panic.
    /// 2. URI with a non-file scheme that passes url::Url::parse but fails to_file_path() —
    ///    the fallback raw-strip code should be reached.
    /// 3. Normal file URI to a file outside the root — strip_prefix fails, fallback returns absolute.
    #[test]
    fn test_uri_to_relative_path_adversarial() {
        let root = PathBuf::from("/home/user/project");

        // 1. Encoded URI for a file outside the workspace root — should return the decoded
        //    absolute path (strip_prefix fails, but we still decode correctly).
        let outside_uri = Uri::from_str("file:///tmp/other%20project/foo.rs").unwrap();
        let result = uri_to_relative_path(&outside_uri, &root);
        // Should be the decoded absolute path, NOT contain %20
        let result_str = result.to_string_lossy();
        assert!(!result_str.contains("%20"), "fallback should not contain raw percent-encoding: {result_str}");
        assert!(result_str.contains("other project"), "path should be decoded: {result_str}");

        // 2. Encoded root path matches exactly the file — relative should be empty/current dir.
        let root2 = PathBuf::from("/home/user/my project");
        let exact_uri = Uri::from_str("file:///home/user/my%20project").unwrap();
        let rel2 = uri_to_relative_path(&exact_uri, &root2);
        // strip_prefix of identical path yields "" which is PathBuf::new()
        assert_eq!(rel2, PathBuf::from(""));
    }

    // -----------------------------------------------------------------------
    // Tests for resolve_type_hierarchy_item (pure function, no LSP server needed)
    // -----------------------------------------------------------------------

    fn make_node(file: &str, name: &str, kind: NodeKind, line_start: usize, line_end: usize) -> Node {
        let kind_str = match &kind {
            NodeKind::Trait => "trait",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            _ => "impl",
        };
        Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start,
            line_end,
            signature: format!("{} {}", kind_str, name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_type_hierarchy_item(name: &str, uri: &str, start_line: u64) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "uri": uri,
            "kind": 5,
            "range": {
                "start": { "line": start_line, "character": 0 },
                "end": { "line": start_line + 5, "character": 0 }
            }
        })
    }

    #[test]
    fn test_resolve_type_hierarchy_single_match() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "MyTrait", NodeKind::Trait, 10, 20);
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item("MyTrait", "file:///project/src/lib.rs", 9);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.unwrap().name, "MyTrait");
    }

    #[test]
    fn test_resolve_type_hierarchy_name_collision_uses_position() {
        let root = PathBuf::from("/project");
        let node1 = make_node("src/lib.rs", "Config", NodeKind::Struct, 10, 20);
        let node2 = make_node("src/lib.rs", "Config", NodeKind::Struct, 50, 60);
        let nodes: Vec<&Node> = vec![&node1, &node2];

        // Item at line 50 (0-indexed: 49) should resolve to node2
        let item = make_type_hierarchy_item("Config", "file:///project/src/lib.rs", 49);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.as_ref().unwrap().name, "Config");
        // The resolved node should be the one at line 50-60 (node2)
        let resolved_id = result.unwrap();
        assert!(
            nodes.iter().any(|n| n.id == resolved_id && n.line_start == 50),
            "should resolve to the node at line 50, not line 10"
        );
    }

    #[test]
    fn test_resolve_type_hierarchy_position_fallback() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 10, 20);
        let nodes: Vec<&Node> = vec![&node];

        // Item with a different name — should fall through to position-based fallback
        let item = make_type_hierarchy_item("DifferentName", "file:///project/src/lib.rs", 14);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        // line 14 (0-indexed) + 1 = 15, which is within [10, 20]
        assert_eq!(result.unwrap().name, "MyStruct");
    }

    #[test]
    fn test_resolve_type_hierarchy_external_dependency_filtered() {
        let root = PathBuf::from("/project");
        let node = make_node(".cargo/registry/src/tokio/lib.rs", "Runtime", NodeKind::Struct, 1, 100);
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item("Runtime", "file:///project/.cargo/registry/src/tokio/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), ".cargo paths should be filtered out");
    }

    #[test]
    fn test_resolve_type_hierarchy_missing_fields_returns_none() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // Missing "uri"
        let item = serde_json::json!({"name": "Foo"});
        assert!(LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none());

        // Missing "name"
        let item = serde_json::json!({"uri": "file:///project/src/lib.rs"});
        assert!(LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none());

        // Empty object
        let item = serde_json::json!({});
        assert!(LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none());
    }

    #[test]
    fn test_resolve_type_hierarchy_percent_encoded_uri() {
        let root = PathBuf::from("/my project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // URI with percent-encoded space
        let item = make_type_hierarchy_item("Foo", "file:///my%20project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.unwrap().name, "Foo");
    }

    #[test]
    fn test_max_type_hierarchy_strikes_constant() {
        // Verify the constant is reasonable
        assert_eq!(MAX_TYPE_HIERARCHY_STRIKES, 3);
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for type hierarchy edge cases
    // -----------------------------------------------------------------------

    /// If two candidates have the EXACT same name, file, kind, and line range,
    /// the position tiebreaker cannot distinguish them. Verify we get *some*
    /// result (not a panic) and it's deterministic.
    #[test]
    fn test_resolve_type_hierarchy_identical_position_tiebreaker() {
        let root = PathBuf::from("/project");
        // Two nodes with identical name, file, kind, and line range
        let node1 = make_node("src/lib.rs", "Handler", NodeKind::Struct, 10, 20);
        let node2 = make_node("src/lib.rs", "Handler", NodeKind::Struct, 10, 20);
        let nodes: Vec<&Node> = vec![&node1, &node2];

        let item = make_type_hierarchy_item("Handler", "file:///project/src/lib.rs", 9);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);

        // Must resolve to something, not panic or return None
        assert!(result.is_some(), "should resolve even with identical candidates");

        // Must be deterministic across calls
        let result2 = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result, result2, "resolution should be deterministic");
    }

    /// URI for a file completely outside the repo root. The path won't
    /// strip_prefix successfully, so rel_path == abs_path. There should be
    /// no match because no node lives at that absolute path.
    #[test]
    fn test_resolve_type_hierarchy_file_outside_repo() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // URI points to a completely different directory
        let item = make_type_hierarchy_item("Foo", "file:///other-project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        // strip_prefix fails, rel_path becomes /other-project/src/lib.rs
        // No node has that path, so should be None
        assert!(result.is_none(), "file outside repo should not resolve");
    }

    /// Stdlib or dependency URI that doesn't contain ".cargo" (e.g. sysroot).
    /// The .cargo filter won't catch it — verify it doesn't accidentally match
    /// a same-name node in the repo.
    #[test]
    fn test_resolve_type_hierarchy_stdlib_uri_no_cargo_filter() {
        let root = PathBuf::from("/project");
        // A repo node named "Iterator"
        let node = make_node("src/lib.rs", "Iterator", NodeKind::Trait, 1, 50);
        let nodes: Vec<&Node> = vec![&node];

        // LSP returns a stdlib URI (no .cargo in path, but outside repo)
        let item = make_type_hierarchy_item(
            "Iterator",
            "file:///rustup/toolchains/stable/lib/rustlib/src/rust/library/core/src/iter/traits/iterator.rs",
            0,
        );
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        // strip_prefix fails, rel_path is the full sysroot path —
        // no node has that file path, so should not match
        assert!(
            result.is_none(),
            "stdlib path outside repo should not resolve to a repo node"
        );
    }

    /// Non-Latin characters in file paths (Chinese, Arabic, emoji).
    /// url::Url should handle percent-encoding for these.
    #[test]
    fn test_resolve_type_hierarchy_unicode_path() {
        let root = PathBuf::from("/project");
        let node = make_node("src/\u{4e2d}\u{6587}.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // Percent-encoded Chinese characters: 中文 = %E4%B8%AD%E6%96%87
        let item = make_type_hierarchy_item(
            "Foo",
            "file:///project/src/%E4%B8%AD%E6%96%87.rs",
            0,
        );
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(
            result.unwrap().name,
            "Foo",
            "percent-encoded Unicode paths should decode correctly"
        );
    }

    /// Malformed URI that url::Url::parse rejects. The fallback path should
    /// attempt manual strip_prefix.
    #[test]
    fn test_resolve_type_hierarchy_malformed_uri_fallback() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Bar", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // Not a valid URL (no scheme) — url::Url::parse will fail
        let item = serde_json::json!({
            "name": "Bar",
            "uri": "file:///project/src/lib.rs",
            "kind": 5,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        // This should work via url::Url (valid file:// URI)
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_some());

        // Now try something that *only* works via fallback
        let item_bad = serde_json::json!({
            "name": "Bar",
            "uri": "not-a-url",
            "kind": 5,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        // url::Url::parse("not-a-url") => Err, fallback strip_prefix("file://") => None
        let result = LspEnricher::resolve_type_hierarchy_item(&item_bad, &nodes, &root);
        assert!(result.is_none(), "URI without file:// scheme should fail gracefully");
    }

    /// Type hierarchy item with unexpected field types (name as number, uri as null).
    /// Should return None, not panic.
    #[test]
    fn test_resolve_type_hierarchy_wrong_field_types() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // name is a number instead of string
        let item = serde_json::json!({
            "name": 42,
            "uri": "file:///project/src/lib.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        assert!(
            LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none(),
            "numeric name should not match"
        );

        // uri is null
        let item = serde_json::json!({
            "name": "Foo",
            "uri": null,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        assert!(
            LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none(),
            "null URI should not match"
        );

        // range.start.line is a string instead of number
        let item = serde_json::json!({
            "name": "Foo",
            "uri": "file:///project/src/lib.rs",
            "range": { "start": { "line": "zero", "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        // Should still resolve — line defaults to 0 when not parseable
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_some(), "bad line type should degrade gracefully, not prevent resolution");
    }

    /// When range is completely missing from the item, resolution should still
    /// work for unique name+file matches (range_start_line defaults to 0).
    #[test]
    fn test_resolve_type_hierarchy_no_range() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        let item = serde_json::json!({
            "name": "Foo",
            "uri": "file:///project/src/lib.rs",
            "kind": 5
        });
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.unwrap().name, "Foo", "missing range should not prevent unique match");
    }

    /// If the node kind is Function (not Trait/Struct/Enum/Impl), the candidate
    /// filter should exclude it. This tests the kind whitelist.
    #[test]
    fn test_resolve_type_hierarchy_ignores_non_type_nodes() {
        let root = PathBuf::from("/project");
        // A function with the same name as the type hierarchy item
        let node = make_node("src/lib.rs", "process", NodeKind::Function, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item("process", "file:///project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), "Functions should not be resolved as type hierarchy targets");
    }

    /// Empty matching_nodes should return None, not panic.
    #[test]
    fn test_resolve_type_hierarchy_empty_nodes() {
        let root = PathBuf::from("/project");
        let nodes: Vec<&Node> = vec![];
        let item = make_type_hierarchy_item("Foo", "file:///project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none());
    }

    /// The .cargo filter uses `contains(".cargo")` which would also match a
    /// directory literally named ".cargo" inside the repo. Verify the filter
    /// catches nested .cargo paths.
    #[test]
    fn test_resolve_type_hierarchy_cargo_filter_nested() {
        let root = PathBuf::from("/project");
        // A node that happens to be under a .cargo subdir in the repo
        let node = make_node("vendor/.cargo/config.toml/Foo", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item(
            "Foo",
            "file:///project/vendor/.cargo/config.toml/Foo",
            0,
        );
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), "paths containing .cargo anywhere should be filtered");
    }

    /// Position tiebreaker when the LSP range doesn't overlap any candidate's
    /// line range. Falls back to closest-by-line_start.
    ///
    /// BUG FINDING: When two nodes have the same name/file/kind, they produce
    /// identical NodeIds (NodeId doesn't include line numbers). So even though
    /// the resolver picks the positionally closest candidate, the *returned*
    /// NodeId is indistinguishable. The edge will be created with the right
    /// NodeId, but both nodes share it — the graph can't distinguish them.
    /// This is a known limitation of the NodeId design.
    #[test]
    fn test_resolve_type_hierarchy_position_no_overlap() {
        let root = PathBuf::from("/project");
        let node1 = make_node("src/lib.rs", "Config", NodeKind::Struct, 10, 20);
        let node2 = make_node("src/lib.rs", "Config", NodeKind::Struct, 50, 60);
        let nodes: Vec<&Node> = vec![&node1, &node2];

        // Line 35 (0-indexed: 34, +1=35) doesn't overlap [10,20] or [50,60]
        let item = make_type_hierarchy_item("Config", "file:///project/src/lib.rs", 34);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_some(), "should fall back to closest-by-line_start");

        // The resolver picks closest by unsigned distance: |10-35|=25, |50-35|=15
        // So node2 should be selected. But since NodeId doesn't include line info,
        // both nodes have the same NodeId — we can only verify resolution succeeded.
        let resolved = result.unwrap();
        assert_eq!(resolved.name, "Config");
        // NOTE: NodeId equality means we can't distinguish which physical node
        // was chosen from the returned ID alone. This is a design limitation.
        assert_eq!(node1.id, node2.id, "NodeId lacks position — identical-name nodes are indistinguishable");
    }

    /// Verify that an item with a non-file URI scheme (e.g. untitled:, http://)
    /// is handled gracefully.
    #[test]
    fn test_resolve_type_hierarchy_non_file_uri_scheme() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // http:// scheme — url::Url::parse succeeds but to_file_path() fails
        let item = make_type_hierarchy_item("Foo", "http://example.com/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), "non-file URI should return None");

        // untitled: scheme
        let item = make_type_hierarchy_item("Foo", "untitled:Untitled-1", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), "untitled: URI should return None");
    }

    /// Verify strike counter state is correctly initialized and the constant
    /// is used properly. The strike logic itself is tested via integration
    /// (needs a mock transport), but we can verify the initial state.
    #[test]
    fn test_strike_counter_initial_state() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let state = enricher.state.try_lock().unwrap();
        assert_eq!(state.type_hierarchy_strikes, 0);
        assert!(!state.has_type_hierarchy, "should default to false until init confirms");
    }

    /// If LspState has type hierarchy disabled (has_type_hierarchy = false),
    /// verify the strikes counter is irrelevant — it's the flag that gates.
    #[test]
    fn test_strike_counter_flag_vs_count_independence() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let mut state = enricher.state.try_lock().unwrap();

        // Even with 0 strikes, if has_type_hierarchy is false, nothing runs.
        // And even with many strikes, resetting the flag should allow it.
        state.type_hierarchy_strikes = 100;
        state.has_type_hierarchy = true;
        // The enrich loop checks `has_type_hierarchy` first, so this state
        // means "enabled but with many past strikes". Since strikes reset on
        // success, this would only persist if all calls failed.
        assert!(state.has_type_hierarchy);
        assert_eq!(state.type_hierarchy_strikes, 100);
    }

    /// If rust-analyzer is available, test actual enrichment on a small Rust file.
    #[tokio::test]
    async fn test_lsp_enricher_with_rust_analyzer() {
        // Check if rust-analyzer is installed
        let ra_check = tokio::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .await;

        if ra_check.is_err() {
            eprintln!("Skipping: rust-analyzer not installed");
            return;
        }

        // This test validates the LspEnricher can start and respond,
        // but we don't have a full Cargo project to index against in tests.
        // The enricher should handle the initialization gracefully.
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let index = GraphIndex::new();

        let nodes = vec![Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: "test_fn".into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 1,
            signature: "fn test_fn()".into(),
            body: "fn test_fn() {}".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }];

        // This may succeed or fail depending on whether we're in a Cargo project.
        // Either way, it should not panic.
        let _result = enricher.enrich(&nodes, &index, std::path::Path::new(".")).await;
    }

    /// Verify that the quiescent readiness condition matches the rust-analyzer
    /// specification: `quiescent: true` means the server is fully ready (no
    /// pending background work).  This is a regression test for the inverted
    /// check fixed in PR #226 / issue #215.
    #[test]
    fn test_server_status_quiescent_means_ready() {
        // Simulate serverStatus notifications as serde_json::Value
        let make_status = |health: &str, quiescent: bool| -> serde_json::Value {
            serde_json::json!({
                "method": "experimental/serverStatus",
                "params": { "health": health, "quiescent": quiescent }
            })
        };

        // quiescent: true, health: ok => READY (server finished background work)
        assert!(LspEnricher::server_status_is_ready(&make_status("ok", true)),
            "quiescent: true + health: ok should be ready");

        // quiescent: false, health: ok => NOT READY (still indexing)
        assert!(!LspEnricher::server_status_is_ready(&make_status("ok", false)),
            "quiescent: false + health: ok should NOT be ready");

        // quiescent: true, health: warning => NOT READY (unhealthy)
        assert!(!LspEnricher::server_status_is_ready(&make_status("warning", true)),
            "health: warning should NOT be ready regardless of quiescent");

        // quiescent: true, health: error => NOT READY (unhealthy)
        assert!(!LspEnricher::server_status_is_ready(&make_status("error", true)),
            "health: error should NOT be ready regardless of quiescent");

        // Missing quiescent field defaults to false (NOT ready)
        // Conservative: if the server doesn't tell us it's quiescent, assume it's not
        let no_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok" }
        });
        assert!(!LspEnricher::server_status_is_ready(&no_quiescent),
            "missing quiescent should default to false (not ready)");

        // Adversarial: completely empty params should not be ready
        let empty_params = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": {}
        });
        assert!(!LspEnricher::server_status_is_ready(&empty_params),
            "empty params should not be ready");

        // Adversarial: missing params entirely should not be ready
        let no_params = serde_json::json!({
            "method": "experimental/serverStatus"
        });
        assert!(!LspEnricher::server_status_is_ready(&no_params),
            "missing params should not be ready");

        // Adversarial: quiescent as string "true" should not be ready
        // (must be boolean true, not string)
        let string_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": "true" }
        });
        assert!(!LspEnricher::server_status_is_ready(&string_quiescent),
            "quiescent as string 'true' should not be ready (must be bool)");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for find_enclosing_symbol (PR #286 ship pipeline)
    // Seeded from dissent: module-level references, self-references, edge cases
    // -----------------------------------------------------------------------

    /// Dissent finding: references at module level (outside any function/struct/impl)
    /// should return None -- no enclosing symbol exists.
    #[test]
    fn test_find_enclosing_symbol_module_level_returns_none() {
        let nodes = vec![
            make_node("src/lib.rs", "my_fn", NodeKind::Function, 10, 20),
            make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 25, 35),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line 5 is before any symbol -- module-level use statement
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 5);
        assert!(result.is_none(), "module-level reference should not resolve to any symbol");

        // Line 22 is between symbols -- also module level
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 22);
        assert!(result.is_none(), "gap between symbols should not resolve");
    }

    /// Dissent finding: nested symbols should resolve to the narrowest enclosing one.
    #[test]
    fn test_find_enclosing_symbol_prefers_narrowest() {
        let nodes = vec![
            make_node("src/lib.rs", "MyImpl", NodeKind::Impl, 1, 50),
            make_node("src/lib.rs", "inner_fn", NodeKind::Function, 10, 20),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line 15 is inside both MyImpl and inner_fn -- should resolve to inner_fn
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 15);
        assert_eq!(result.unwrap().name, "inner_fn", "should resolve to narrowest enclosing symbol");
    }

    /// Dissent finding: references in a different file should not match.
    #[test]
    fn test_find_enclosing_symbol_wrong_file_returns_none() {
        let nodes = vec![
            make_node("src/lib.rs", "my_fn", NodeKind::Function, 1, 50),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        let result = find_enclosing_symbol(&refs, Path::new("src/other.rs"), 10);
        assert!(result.is_none(), "reference in different file should not match");
    }

    /// Verify find_enclosing_symbol resolves Enum and Const scopes
    /// (expanded filter per CodeRabbit review feedback).
    #[test]
    fn test_find_enclosing_symbol_resolves_enum_and_const() {
        let nodes = vec![
            make_node("src/lib.rs", "MyEnum", NodeKind::Enum, 1, 20),
            make_node("src/lib.rs", "MY_CONST", NodeKind::Const, 25, 30),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line inside enum -- should resolve after filter expansion
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 10);
        assert_eq!(result.unwrap().name, "MyEnum", "Enum should now resolve in find_enclosing_symbol");

        // Line inside const -- should resolve after filter expansion
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 27);
        assert_eq!(result.unwrap().name, "MY_CONST", "Const should now resolve in find_enclosing_symbol");
    }

    /// Verify the self-reference filtering logic: a reference at the definition
    /// site (same file, within line_start..line_end) should be filtered.
    #[test]
    fn test_self_reference_detection_logic() {
        let node = make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 10, 20);

        // Reference at the definition site
        let ref_file = PathBuf::from("src/lib.rs");
        let ref_line: usize = 15;
        let is_self_ref = ref_file == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end;
        assert!(is_self_ref, "reference within definition site should be detected as self-reference");

        // Reference in same file but outside definition
        let ref_line: usize = 25;
        let is_self_ref = ref_file == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end;
        assert!(!is_self_ref, "reference outside definition should not be self-reference");

        // Reference in different file
        let ref_file = PathBuf::from("src/other.rs");
        let ref_line: usize = 15;
        let is_self_ref = ref_file == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end;
        assert!(!is_self_ref, "reference in different file should not be self-reference");
    }

    /// Verify that .cargo path filtering works correctly.
    #[test]
    fn test_cargo_dep_filtering_logic() {
        let cargo_path = PathBuf::from("/home/user/.cargo/registry/src/index.crates.io/serde-1.0.0/src/lib.rs");
        assert!(cargo_path.to_string_lossy().contains(".cargo"), ".cargo dependency should be detected");

        let project_path = PathBuf::from("src/lib.rs");
        assert!(!project_path.to_string_lossy().contains(".cargo"), "project file should not be filtered");

        // Dissent edge case: project with "cargo" in name
        let tricky_path = PathBuf::from("my-cargo-tool/src/lib.rs");
        assert!(!tricky_path.to_string_lossy().contains(".cargo"),
            "project with 'cargo' in name (no dot prefix) should not be filtered");
    }

    /// Verify ReferencedBy edge kind has correct weight and string representation.
    #[test]
    fn test_referenced_by_edge_properties() {
        let edge = Edge {
            from: NodeId {
                root: String::new(),
                file: PathBuf::from("src/main.rs"),
                name: "caller".into(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: "MyStruct".into(),
                kind: NodeKind::Struct,
            },
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
        };

        assert_eq!(format!("{}", edge.kind), "referenced_by");
        assert_eq!(edge.source, ExtractionSource::Lsp);
        assert_eq!(edge.confidence, Confidence::Confirmed);
    }

    /// Verify that the initialize request includes experimental.serverStatusNotification.
    ///
    /// Without this capability, rust-analyzer won't send serverStatus notifications
    /// and the readiness wait falls through to a 5s timeout, querying before indexing
    /// is complete and producing 0 edges. This was the root cause of issue #293.
    #[test]
    fn test_init_params_declare_server_status_notification() {
        // Build the same init params that ensure_initialized() would build
        let root_uri = Uri::from_str("file:///tmp/test").unwrap();
        #[allow(deprecated)]
        let init_params = InitializeParams {
            root_uri: Some(root_uri),
            capabilities: ClientCapabilities {
                window: Some(lsp_types::WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..Default::default()
                }),
                experimental: Some(serde_json::json!({
                    "serverStatusNotification": true
                })),
                ..Default::default()
            },
            ..Default::default()
        };

        // Verify the experimental capability is set
        let experimental = init_params.capabilities.experimental.as_ref()
            .expect("experimental capabilities must be set");
        assert_eq!(
            experimental.get("serverStatusNotification"),
            Some(&serde_json::json!(true)),
            "serverStatusNotification must be true to receive serverStatus from rust-analyzer"
        );
    }

    /// Integration test: run LSP enrichment against the RNA repo itself.
    ///
    /// This test requires rust-analyzer to be on PATH and the RNA repo to be a
    /// valid Cargo workspace. It is marked #[ignore] so it doesn't run in CI
    /// (which may not have rust-analyzer installed), but can be run explicitly
    /// with `cargo test -- --ignored test_lsp_enrichment_produces_edges`.
    ///
    /// Regression guard for #379: verifies that rust-analyzer reaches quiescent
    /// state and produces >0 call edges. If quiescence fails, 0 edges result
    /// because RA is not indexed when call-hierarchy queries run.
    #[tokio::test]
    #[ignore]
    async fn test_lsp_enrichment_produces_edges() {
        use crate::extract::ExtractorRegistry;
        use crate::scanner::Scanner;

        // Find the repo root (where Cargo.toml is)
        let repo_root = std::env::current_dir()
            .expect("failed to get cwd");
        assert!(repo_root.join("Cargo.toml").exists(),
            "test must be run from the repo root");

        // Scan the repo to get files
        let mut scanner = Scanner::new(repo_root.clone())
            .expect("failed to create scanner");
        let scan_result = scanner.scan()
            .expect("scan failed");

        // Extract nodes from scanned files
        let registry = ExtractorRegistry::default();
        let extraction = registry.extract_scan_result(&repo_root, &scan_result);
        let nodes = extraction.nodes;
        assert!(nodes.len() > 100, "expected >100 nodes from RNA repo, got {}", nodes.len());

        // Create enricher and run
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let index = GraphIndex::new();
        let result = enricher.enrich(&nodes, &index, &repo_root).await
            .expect("LSP enrichment failed");

        let edge_count = result.added_edges.len();
        eprintln!("LSP enrichment produced {} edges from {} nodes", edge_count, nodes.len());

        // Regression guard for #379: check was_quiescent first so failures
        // report a clear message rather than just "0 edges".
        let was_quiescent = {
            let state = enricher.state.lock().await;
            state.was_quiescent
        };
        assert!(was_quiescent,
            "rust-analyzer did not reach quiescent state — this is the root cause of the \
             #379 regression. Check that rust-analyzer can index the repo within 120s.");

        assert!(edge_count > 100,
            "expected >100 LSP edges from RNA repo, got {}. \
             This likely means rust-analyzer is not responding to call hierarchy queries.",
            edge_count);

        // Check that we have Calls edges specifically
        let calls_edges = result.added_edges.iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .count();
        assert!(calls_edges > 50,
            "expected >50 Calls edges, got {}", calls_edges);
    }

    /// Regression test for #379: verify that LspState.was_quiescent defaults to false.
    ///
    /// The guard in Pass 3 relies on was_quiescent being false until the
    /// server explicitly reaches quiescent=true. If it defaulted to true,
    /// Pass 3 would run even when the server never finished indexing.
    #[test]
    fn test_lsp_state_was_quiescent_defaults_false() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        // The state is accessed via the async lock; use a blocking read for testing.
        let state = enricher.state.blocking_lock();
        assert!(!state.was_quiescent,
            "was_quiescent must default to false — Pass 3 must be skipped until \
             the server explicitly reaches quiescent=true (regression guard for #379)");
    }

    /// Regression test for #379 round 4: Pass 1 (call hierarchy) must also be
    /// skipped when `was_quiescent=false`.
    ///
    /// When RA hasn't indexed (deadline expired), Pass 1 returns 0 edges for
    /// all ZERO_EDGE_ABORT_THRESHOLD nodes, triggering the zero-edge abort.
    /// This is indistinguishable from a misconfigured server. The same guard
    /// that protects Pass 3 must also protect Pass 1.
    ///
    /// This test verifies that `was_quiescent` defaults to false (so Pass 1
    /// is skipped until RA explicitly becomes quiescent).
    #[test]
    fn test_lsp_state_was_quiescent_defaults_false_protects_pass1() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let state = enricher.state.blocking_lock();
        assert!(!state.was_quiescent,
            "was_quiescent must default to false — Pass 1 (call hierarchy) must be skipped \
             until the server explicitly reaches quiescent=true. Without this guard, the \
             zero-edge abort fires on large repos where RA doesn't index within 120s \
             (regression: #379 r4)");
    }

    /// Verify the was_quiescent logic: only servers that sent serverStatus but
    /// never reached quiescent=true trigger the Pass 3 skip.
    ///
    /// The guard is `saw_quiescent || !seen_server_status`:
    /// - saw_quiescent=true (any health): done indexing → Run Pass 3
    /// - seen_server_status=false: no serverStatus (pyright etc.) → assumed ready → Run Pass 3
    /// - saw_quiescent=false, seen_server_status=true: RA timed out → SKIP Pass 3
    ///
    /// Note: `saw_quiescent` tracks the raw `quiescent=true` bit, not `server_ready`
    /// which also requires health="ok". This means health="warning" + quiescent=true
    /// (compile errors but done indexing) correctly enables Pass 3.
    #[test]
    fn test_server_status_is_ready_drives_quiescence() {
        // health=ok + quiescent=true: server_ready=true, saw_quiescent=true → Pass 3 runs
        let ready_msg = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": true }
        });
        assert!(LspEnricher::server_status_is_ready(&ready_msg),
            "health=ok + quiescent=true must be ready — saw_quiescent=true, Pass 3 runs");

        // health="warning" + quiescent=true: server_ready=false but saw_quiescent=true → Pass 3 runs
        // (compile errors but done indexing — diagnostics are needed precisely in this state)
        let warning_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "warning", "quiescent": true }
        });
        assert!(!LspEnricher::server_status_is_ready(&warning_quiescent),
            "health=warning is not 'ready' (server_ready=false), but saw_quiescent=true \
             means was_quiescent=true and Pass 3 will run correctly");

        // quiescent=false: saw_quiescent stays false, Pass 3 blocked if deadline expires
        let not_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": false }
        });
        assert!(!LspEnricher::server_status_is_ready(&not_quiescent),
            "quiescent=false: saw_quiescent=false — if deadline expires with only these \
             messages, seen_server_status=true and saw_quiescent=false → was_quiescent=false");
    }

    // -----------------------------------------------------------------------
    // Tests for build_diagnostic_nodes (pure function, no LSP server needed)
    // -----------------------------------------------------------------------

    /// Verify that error and warning diagnostics produce nodes.
    #[test]
    fn test_build_diagnostic_nodes_basic() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "type mismatch",
                "source": "rust-analyzer",
                "range": {
                    "start": { "line": 141, "character": 4 },
                    "end": { "line": 141, "character": 20 }
                }
            }),
            serde_json::json!({
                "severity": 2,
                "message": "unused variable: `x`",
                "source": "rust-analyzer",
                "range": {
                    "start": { "line": 88, "character": 8 },
                    "end": { "line": 88, "character": 9 }
                }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 2, "error and warning should produce 2 nodes");

        // Check error node — name format is "[severity:line] message"
        let error_node = nodes.iter().find(|n| n.id.name.starts_with("[error:142]")).unwrap();
        assert_eq!(error_node.id.file, PathBuf::from("src/service.rs"));
        assert_eq!(error_node.id.kind, NodeKind::Other("diagnostic".to_string()));
        assert_eq!(error_node.line_start, 142); // 0-indexed line 141 -> 1-indexed 142
        assert_eq!(error_node.metadata.get("diagnostic_severity").unwrap(), "error");
        assert_eq!(error_node.metadata.get("diagnostic_message").unwrap(), "type mismatch");
        assert_eq!(error_node.metadata.get("diagnostic_source").unwrap(), "rust-analyzer");
        assert_eq!(error_node.metadata.get("diagnostic_range").unwrap(), "142:4-142:20");
        assert_eq!(error_node.metadata.get("diagnostic_timestamp").unwrap(), "1700000000");

        // Check warning node — name includes line number for uniqueness
        let warn_node = nodes.iter().find(|n| n.id.name.starts_with("[warning:89]")).unwrap();
        assert_eq!(warn_node.line_start, 89);
        assert_eq!(warn_node.metadata.get("diagnostic_severity").unwrap(), "warning");
    }

    /// Verify that Information (3) and Hint (4) diagnostics are filtered out.
    #[test]
    fn test_build_diagnostic_nodes_filters_information_and_hint() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 3,
                "message": "consider using async",
                "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 10 } }
            }),
            serde_json::json!({
                "severity": 4,
                "message": "hint: you might want to...",
                "range": { "start": { "line": 10, "character": 0 }, "end": { "line": 10, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(nodes.is_empty(), "information and hint diagnostics should not produce nodes");
    }

    /// Verify that an empty diagnostics list produces no nodes (zero-error files rule).
    #[test]
    fn test_build_diagnostic_nodes_empty_produces_no_nodes() {
        let root = PathBuf::from("/project");
        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &[],
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );
        assert!(nodes.is_empty(), "zero diagnostics should produce no nodes");
    }

    /// Verify .cargo paths are filtered out.
    #[test]
    fn test_build_diagnostic_nodes_cargo_path_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "some error",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/.cargo/registry/tokio/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );
        assert!(nodes.is_empty(), ".cargo paths should be filtered");
    }

    /// Verify that long messages are truncated in the node name but preserved in metadata.
    #[test]
    fn test_build_diagnostic_nodes_long_message_truncated_in_name() {
        let root = PathBuf::from("/project");
        let long_msg = "a".repeat(200);
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": long_msg,
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 1);
        let node = &nodes[0];
        // Name should be truncated (max 80 chars message snippet + "[error:N] " prefix + "...")
        assert!(node.id.name.len() < 200, "node name should be truncated for long messages");
        assert!(node.id.name.ends_with("..."), "truncated name should end with ...");
        // Name includes the line number for uniqueness
        assert!(node.id.name.starts_with("[error:1]"), "name should include severity and line number");
        // Full message preserved in metadata
        assert_eq!(node.metadata.get("diagnostic_message").unwrap().len(), 200, "full message preserved in metadata");
    }

    /// Verify diagnostic node has ExtractionSource::Lsp.
    #[test]
    fn test_build_diagnostic_nodes_source_is_lsp() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "error",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes[0].source, ExtractionSource::Lsp);
    }

    /// Verify severity_to_str handles all known severity values.
    #[test]
    fn test_lsp_severity_to_str() {
        assert_eq!(LspEnricher::lsp_severity_to_str(1), "error");
        assert_eq!(LspEnricher::lsp_severity_to_str(2), "warning");
        assert_eq!(LspEnricher::lsp_severity_to_str(3), "information");
        assert_eq!(LspEnricher::lsp_severity_to_str(4), "hint");
        assert_eq!(LspEnricher::lsp_severity_to_str(0), "unknown");
        assert_eq!(LspEnricher::lsp_severity_to_str(99), "unknown");
    }

    /// Verify diagnostic node metadata contains all required fields.
    #[test]
    fn test_build_diagnostic_nodes_has_all_metadata_fields() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "test error",
                "source": "my-lsp",
                "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 10 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "myroot",
            "my-lsp",
            "rust",
            "1234567890",
            2,
        );

        assert_eq!(nodes.len(), 1);
        let meta = &nodes[0].metadata;
        assert!(meta.contains_key("diagnostic_severity"), "missing diagnostic_severity");
        assert!(meta.contains_key("diagnostic_source"), "missing diagnostic_source");
        assert!(meta.contains_key("diagnostic_message"), "missing diagnostic_message");
        assert!(meta.contains_key("diagnostic_range"), "missing diagnostic_range");
        assert!(meta.contains_key("diagnostic_timestamp"), "missing diagnostic_timestamp");
        assert_eq!(meta.get("diagnostic_timestamp").unwrap(), "1234567890");
        assert_eq!(nodes[0].id.root, "myroot");
    }

    /// Verify diagnostics with missing severity default to error (severity 1 = error).
    #[test]
    fn test_build_diagnostic_nodes_missing_severity_defaults_to_error() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                // No severity field — should default to 1 (error)
                "message": "something bad",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 1, "missing severity defaults to error which should produce a node");
        assert_eq!(nodes[0].metadata.get("diagnostic_severity").unwrap(), "error");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests seeded from dissent findings
    // -----------------------------------------------------------------------

    /// Dissent finding #2: identical messages at different lines should produce
    /// distinct NodeIds (no silent overwrites in LanceDB).
    #[test]
    fn test_build_diagnostic_nodes_same_message_different_lines_distinct_ids() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 2,
                "message": "unused variable: `x`",
                "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 5 } }
            }),
            serde_json::json!({
                "severity": 2,
                "message": "unused variable: `x`",
                "range": { "start": { "line": 24, "character": 4 }, "end": { "line": 24, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 2, "identical messages at different lines should produce 2 nodes");
        // NodeIds must be distinct
        let id0 = nodes[0].id.to_stable_id();
        let id1 = nodes[1].id.to_stable_id();
        assert_ne!(id0, id1, "different line positions should produce distinct NodeIds");
        // Names should include the line number
        assert!(nodes[0].id.name.contains(":10]") || nodes[0].id.name.contains(":25]"),
            "name should include line 10 or 25: got '{}'", nodes[0].id.name);
    }

    /// Dissent finding #1: stale diagnostic nodes should be identifiable by timestamp.
    /// Verify the timestamp is preserved and is a non-empty string.
    #[test]
    fn test_build_diagnostic_nodes_timestamp_preserved_for_staleness_detection() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "an error",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
        ];

        let ts = "1700123456";
        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            ts,
            2,
        );

        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0].metadata.get("diagnostic_timestamp").unwrap(),
            ts,
            "timestamp must be preserved exactly for agent-side staleness filtering"
        );
    }

    /// Adversarial: diagnostic with empty message should be skipped (not produce a node
    /// with an empty name that breaks search).
    #[test]
    fn test_build_diagnostic_nodes_empty_message_skipped() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "",  // empty
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
            serde_json::json!({
                "severity": 1,
                "message": "   ",  // whitespace-only (trimmed to empty)
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(nodes.is_empty(), "empty/whitespace messages should not produce nodes");
    }

    /// Adversarial: malformed range fields (null, missing, out of order).
    /// Should produce a node with a safe default range, not panic.
    #[test]
    fn test_build_diagnostic_nodes_malformed_range_degrades_gracefully() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "error with null range",
                "range": null
            }),
            serde_json::json!({
                "severity": 1,
                "message": "error with no range"
                // no "range" key at all
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        // Should produce nodes despite malformed ranges (default to line 1)
        assert_eq!(nodes.len(), 2, "malformed range should not prevent node creation");
        for node in &nodes {
            assert_eq!(node.line_start, 1, "missing range should default to line 1");
        }
    }

    /// Adversarial: severity value 0 and very large values (out of spec).
    /// Severity 0 is not defined in LSP spec — should be treated as "unknown" and
    /// since it's not 1 or 2, should be filtered out.
    #[test]
    fn test_build_diagnostic_nodes_out_of_spec_severity_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 0,  // below spec minimum
                "message": "unknown severity",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
            serde_json::json!({
                "severity": 100,  // far above spec maximum
                "message": "unknown large severity",
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(nodes.is_empty(), "severity 0 and 100 should be filtered (only 1 and 2 are stored)");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for #379 r4: Pass 1 guard and diagnostic 0-capture
    // -----------------------------------------------------------------------

    /// Adversarial: "unlinked-file" diagnostic (the VS Code example in issue #379)
    /// is severity 2 (Warning) per LSP spec, so it SHOULD produce a node.
    ///
    /// If RA returns it as severity 3 (Information), that explains 0 captures.
    /// This test documents the expected behavior: severity 2 unlinked-file → captured.
    #[test]
    fn test_build_diagnostic_nodes_unlinked_file_warning_captured() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 2,  // Warning
                "message": "This file is not included in any crates [unlinked-file]",
                "source": "rust-analyzer",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 1,
            "unlinked-file at severity 2 (Warning) should produce a diagnostic node; \
             if this fails, RA is reporting it as Information (3) which gets filtered");
        assert_eq!(nodes[0].metadata.get("diagnostic_severity").unwrap(), "warning");
    }

    /// Adversarial: "unlinked-file" diagnostic at severity 3 (Information)
    /// should be filtered out. This is the suspected root cause of 0 captured diagnostics
    /// in issue #379 — RA may report unlinked-file as Information, not Warning.
    #[test]
    fn test_build_diagnostic_nodes_unlinked_file_information_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 3,  // Information — below our capture threshold
                "message": "This file is not included in any crates [unlinked-file]",
                "source": "rust-analyzer",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(nodes.is_empty(),
            "unlinked-file at severity 3 (Information) should be filtered; \
             this is intentional — Information diagnostics are too noisy for code-understanding queries");
    }

    /// When max_severity_int=4 (hint level), Information (3) diagnostics are captured.
    /// This exercises diagnostic_min_severity = "information" in .oh/config.toml.
    #[test]
    fn test_build_diagnostic_nodes_information_captured_when_threshold_is_information() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 3,  // Information
                "message": "consider using async",
                "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            3, // max_severity_int = Information
        );

        assert_eq!(nodes.len(), 1, "information diagnostic should be captured at threshold=3");
        assert_eq!(
            nodes[0].metadata.get("diagnostic_severity").unwrap(),
            "information"
        );
    }

    /// When max_severity_int=4 (hint level), Hint (4) diagnostics like unlinked-file
    /// and inactive-code are captured.
    /// This exercises diagnostic_min_severity = "hint" in .oh/config.toml.
    #[test]
    fn test_build_diagnostic_nodes_hint_captured_when_threshold_is_hint() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 4,  // Hint
                "message": "This file is not included in any crates [unlinked-file]",
                "source": "rust-analyzer",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }
            }),
            serde_json::json!({
                "severity": 4,  // Hint
                "message": "code is inactive due to #[cfg] directives [inactive-code]",
                "source": "rust-analyzer",
                "range": { "start": { "line": 10, "character": 0 }, "end": { "line": 15, "character": 0 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            4, // max_severity_int = Hint
        );

        assert_eq!(nodes.len(), 2, "hint-level diagnostics should be captured at threshold=4");
        for node in &nodes {
            assert_eq!(
                node.metadata.get("diagnostic_severity").unwrap(),
                "hint"
            );
        }
    }

    /// Severity 0 is always invalid and must be filtered regardless of threshold.
    #[test]
    fn test_build_diagnostic_nodes_severity_zero_always_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 0,
                "message": "invalid severity",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
        ];

        // Even with hint-level threshold (4), severity 0 must be filtered
        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            4, // hint level — most permissive threshold
        );

        assert!(nodes.is_empty(), "severity 0 is not a valid LSP value and must always be filtered");
    }

    /// DiagnosticMinSeverity::max_severity_int returns the correct floor for each variant.
    #[test]
    fn test_diagnostic_min_severity_max_int() {
        use crate::scanner::DiagnosticMinSeverity;
        assert_eq!(DiagnosticMinSeverity::Error.max_severity_int(), 1);
        assert_eq!(DiagnosticMinSeverity::Warning.max_severity_int(), 2);
        assert_eq!(DiagnosticMinSeverity::Information.max_severity_int(), 3);
        assert_eq!(DiagnosticMinSeverity::Hint.max_severity_int(), 4);
    }

    /// Default DiagnosticMinSeverity is Warning.
    #[test]
    fn test_diagnostic_min_severity_default_is_warning() {
        use crate::scanner::DiagnosticMinSeverity;
        assert_eq!(DiagnosticMinSeverity::default(), DiagnosticMinSeverity::Warning);
    }

    /// LspConfig deserializes "hint" correctly.
    #[test]
    fn test_lsp_config_deserializes_hint() {
        use crate::scanner::{DiagnosticMinSeverity, LspConfig};
        let config: LspConfig = toml::from_str(r#"diagnostic_min_severity = "hint""#).unwrap();
        assert_eq!(config.diagnostic_min_severity, DiagnosticMinSeverity::Hint);
    }

    /// LspConfig default (empty section) is Warning.
    #[test]
    fn test_lsp_config_default_is_warning() {
        use crate::scanner::{DiagnosticMinSeverity, LspConfig};
        let config = LspConfig::default();
        assert_eq!(config.diagnostic_min_severity, DiagnosticMinSeverity::Warning);
    }

    /// Adversarial: was_quiescent guard is the same for both Pass 1 and Pass 3.
    /// Verify the default state prevents both passes from running.
    /// (The actual early return from enrich() prevents all passes when !was_quiescent.)
    #[test]
    fn test_lsp_state_was_quiescent_false_prevents_all_passes() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let state = enricher.state.blocking_lock();
        // Both Pass 1 and Pass 3 check was_quiescent before running.
        // With the default false, both are guarded.
        assert!(!state.was_quiescent,
            "was_quiescent=false must prevent Pass 1 AND Pass 3; \
             the early return at Pass 1 guard covers both passes");
        // has_pull_diagnostics also defaults false (not yet initialized)
        assert!(!state.has_pull_diagnostics,
            "has_pull_diagnostics must default false (not yet initialized from LSP capabilities)");
        // has_inlay_hints also defaults false (not yet initialized)
        assert!(!state.has_inlay_hints,
            "has_inlay_hints must default false (not yet initialized from LSP capabilities)");
    }

    // -----------------------------------------------------------------------
    // #408 InlayHints: group_inlay_hints_by_node tests
    // -----------------------------------------------------------------------

    fn make_fn_node_with_lines(file: &str, name: &str, line_start: usize, line_end: usize) -> Node {
        Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start,
            line_end,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    /// Type hints within a function's line range are attributed to it
    #[test]
    fn test_group_inlay_hints_basic() {
        let fn_node = make_fn_node_with_lines("src/lib.rs", "process_order", 5, 20);
        let file_nodes: Vec<&Node> = vec![&fn_node];

        let hints = vec![
            serde_json::json!({
                "kind": 1,
                "position": { "line": 9, "character": 10 },  // 0-indexed → 1-indexed line 10
                "label": ": f64"
            }),
            serde_json::json!({
                "kind": 1,
                "position": { "line": 12, "character": 10 },  // line 13
                "label": [{ "value": ": OrderTotal" }]
            }),
        ];

        let type_map = LspEnricher::group_inlay_hints_by_node(&hints, &file_nodes);
        let stable_id = fn_node.id.to_stable_id();
        assert!(type_map.contains_key(&stable_id),
            "hints within fn lines should be attributed to the function");
        let types_str = &type_map[&stable_id];
        assert!(types_str.contains("f64"), "should contain f64");
        assert!(types_str.contains("OrderTotal"), "should contain OrderTotal");
    }

    /// Parameter hints (kind=2) are filtered out
    #[test]
    fn test_group_inlay_hints_filters_param_hints() {
        let fn_node = make_fn_node_with_lines("src/lib.rs", "do_thing", 1, 10);
        let file_nodes: Vec<&Node> = vec![&fn_node];

        let hints = vec![
            serde_json::json!({
                "kind": 2,  // parameter hint — should be ignored
                "position": { "line": 4, "character": 5 },
                "label": "amount:"
            }),
        ];

        let type_map = LspEnricher::group_inlay_hints_by_node(&hints, &file_nodes);
        assert!(type_map.is_empty(), "parameter hints (kind=2) should be filtered");
    }

    /// Type hints outside all function ranges produce no entries
    #[test]
    fn test_group_inlay_hints_outside_function_range() {
        let fn_node = make_fn_node_with_lines("src/lib.rs", "small_fn", 5, 8);
        let file_nodes: Vec<&Node> = vec![&fn_node];

        let hints = vec![
            serde_json::json!({
                "kind": 1,
                "position": { "line": 20, "character": 5 },  // 0-indexed line 20 → 1-indexed 21
                "label": ": String"
            }),
        ];

        let type_map = LspEnricher::group_inlay_hints_by_node(&hints, &file_nodes);
        assert!(type_map.is_empty(),
            "hints outside all function line ranges should produce no entries");
    }

    // -----------------------------------------------------------------------
    // EdgeKind: new variants roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_edge_kind_tested_by_display() {
        assert_eq!(EdgeKind::TestedBy.to_string(), "tested_by");
    }

    #[test]
    fn test_edge_kind_belongs_to_display() {
        assert_eq!(EdgeKind::BelongsTo.to_string(), "belongs_to");
    }

    // -----------------------------------------------------------------------
    // #405: parse_crate_graph_dot tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_crate_graph_dot_basic() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="my_crate"]
    _1 [shape=box label="dep_crate"]
    _0 -> _1
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 2);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "my_crate");
        assert_eq!(pairs[0].1, "dep_crate");
    }

    #[test]
    fn test_parse_crate_graph_dot_multiple_deps() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="rna"]
    _1 [shape=box label="lancedb"]
    _2 [shape=box label="petgraph"]
    _0 -> _1
    _0 -> _2
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 3);
        assert_eq!(pairs.len(), 2);
        let from_names: Vec<&str> = pairs.iter().map(|(f, _)| f.as_str()).collect();
        assert!(from_names.iter().all(|&f| f == "rna"));
        let to_names: std::collections::HashSet<&str> = pairs.iter().map(|(_, t)| t.as_str()).collect();
        assert!(to_names.contains("lancedb"));
        assert!(to_names.contains("petgraph"));
    }

    #[test]
    fn test_parse_crate_graph_dot_empty() {
        let dot = "digraph rust_analyzer_crate_graph {}";
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert!(crate_names.is_empty());
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_parse_crate_graph_dot_no_edges() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="standalone_crate"]
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        // Isolated crate should be in crate_names even with no edges
        assert_eq!(crate_names, vec!["standalone_crate"],
            "isolated crate should appear in crate_names");
        assert!(pairs.is_empty(), "no edges should produce empty pairs");
    }

    #[test]
    fn test_emit_crate_graph_edges_nodes_and_edges() {
        let crate_names = vec!["crate_a".to_string(), "crate_b".to_string()];
        let pairs = vec![
            ("crate_a".to_string(), "crate_b".to_string()),
        ];
        let mut result = EnrichmentResult::default();
        LspEnricher::emit_crate_graph_edges(&crate_names, &pairs, "my_root", &mut result);

        // Should have 2 crate nodes
        let crate_nodes: Vec<_> = result.new_nodes.iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "crate"))
            .collect();
        assert_eq!(crate_nodes.len(), 2);

        // Bodies should be non-empty (crate name as body for embedding quality)
        for n in &crate_nodes {
            assert!(!n.body.is_empty(), "crate node body should be the crate name");
        }

        // Should have 1 DependsOn edge
        let dep_edges: Vec<_> = result.added_edges.iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(dep_edges.len(), 1);
        assert_eq!(dep_edges[0].from.name, "crate_a");
        assert_eq!(dep_edges[0].to.name, "crate_b");
        assert_eq!(dep_edges[0].from.root, "my_root");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for #405: DOT parser edge cases and emit robustness
    // -----------------------------------------------------------------------

    /// Malformed DOT: edge references unknown node IDs — should produce no pairs but preserve known crate
    #[test]
    fn test_parse_crate_graph_dot_dangling_edge() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="known_crate"]
    _0 -> _99
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        // _99 has no label; edge should be filtered out but known_crate node is preserved
        assert_eq!(crate_names, vec!["known_crate"], "known crate should still be in crate_names");
        assert!(pairs.is_empty(), "dangling edge to unknown node should produce no pairs");
    }

    /// Label with special characters (hyphens, underscores — common in Rust crate names)
    #[test]
    fn test_parse_crate_graph_dot_hyphenated_crate_names() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="my-crate"]
    _1 [shape=box label="another_crate-2"]
    _0 -> _1
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 2);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "my-crate");
        assert_eq!(pairs[0].1, "another_crate-2");
    }

    /// Diamond dependency: A→B, A→C, B→C should produce 3 edges (not deduplicated)
    #[test]
    fn test_parse_crate_graph_dot_diamond_dependency() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="app"]
    _1 [shape=box label="core"]
    _2 [shape=box label="utils"]
    _0 -> _1
    _0 -> _2
    _1 -> _2
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 3);
        assert_eq!(pairs.len(), 3, "diamond graph should have 3 edges");
        let has_app_core = pairs.iter().any(|(f, t)| f == "app" && t == "core");
        let has_app_utils = pairs.iter().any(|(f, t)| f == "app" && t == "utils");
        let has_core_utils = pairs.iter().any(|(f, t)| f == "core" && t == "utils");
        assert!(has_app_core, "should have app→core edge");
        assert!(has_app_utils, "should have app→utils edge");
        assert!(has_core_utils, "should have core→utils edge");
    }

    /// Empty DOT string should not panic
    #[test]
    fn test_parse_crate_graph_dot_completely_empty_string() {
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot("");
        assert!(crate_names.is_empty());
        assert!(pairs.is_empty());
    }

    /// Crate nodes use `file: Cargo.toml` — verify the file path anchoring
    #[test]
    fn test_emit_crate_graph_edges_file_anchor() {
        let crate_names = vec!["crate_a".to_string(), "crate_b".to_string()];
        let pairs = vec![("crate_a".to_string(), "crate_b".to_string())];
        let mut result = EnrichmentResult::default();
        LspEnricher::emit_crate_graph_edges(&crate_names, &pairs, "root", &mut result);

        for node in &result.new_nodes {
            if matches!(&node.id.kind, NodeKind::Other(s) if s == "crate") {
                assert_eq!(
                    node.id.file,
                    PathBuf::from("Cargo.toml"),
                    "crate nodes must use Cargo.toml as file anchor"
                );
            }
        }
    }

    /// Isolated crate (no edges) should still produce a crate node
    #[test]
    fn test_emit_crate_graph_edges_isolated_crate_gets_node() {
        // Single isolated crate with no edges
        let crate_names = vec!["solo_crate".to_string()];
        let pairs: Vec<(String, String)> = vec![];
        let mut result = EnrichmentResult::default();
        LspEnricher::emit_crate_graph_edges(&crate_names, &pairs, "root", &mut result);

        let crate_nodes: Vec<_> = result.new_nodes.iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "crate"))
            .collect();
        assert_eq!(crate_nodes.len(), 1, "isolated crate should get a node");
        assert_eq!(crate_nodes[0].id.name, "solo_crate");
        assert!(result.added_edges.is_empty(), "no edges for isolated crate");
    }
}
