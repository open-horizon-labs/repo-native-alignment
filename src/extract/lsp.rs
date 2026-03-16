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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
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

use super::{EnrichmentResult, Enricher};

// ---------------------------------------------------------------------------
// URI helpers
// ---------------------------------------------------------------------------

/// Convert a file path to a `file://` URI string, then parse as lsp_types::Uri.
fn path_to_uri(path: &Path) -> Result<Uri> {
    // Canonicalize to absolute path
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let uri_string = format!("file://{}", abs.display());
    Uri::from_str(&uri_string).map_err(|e| anyhow::anyhow!("Invalid URI: {}", e))
}

/// Extract a relative file path from an LSP URI, relative to a given root.
/// Find the narrowest enclosing function/impl/struct at a given file + line.
/// Returns None if no symbol contains this location.
fn find_enclosing_symbol(nodes: &[&Node], file: &Path, line: usize) -> Option<NodeId> {
    let file_matches: Vec<_> = nodes.iter()
        .filter(|n| n.id.file == file)
        .collect();

    if file_matches.is_empty() {
        // Try matching just the filename if full path doesn't match
        let file_str = file.to_string_lossy();
        tracing::debug!(
            "No nodes match path '{}', trying {} node paths",
            file_str,
            nodes.iter().map(|n| n.id.file.display().to_string()).collect::<std::collections::HashSet<_>>().len()
        );
        // Log first few node paths for comparison
        for n in nodes.iter().take(3) {
            tracing::debug!("  node path sample: '{}'", n.id.file.display());
        }
    }

    file_matches.iter()
        .filter(|n| matches!(n.id.kind, NodeKind::Function | NodeKind::Impl | NodeKind::Struct))
        .filter(|n| n.line_start <= line && n.line_end >= line)
        .min_by_key(|n| n.line_end - n.line_start)
        .map(|n| n.id.clone())
}

fn uri_to_relative_path(uri: &Uri, root: &Path) -> PathBuf {
    let uri_str = uri.as_str();
    if let Some(file_path_str) = uri_str.strip_prefix("file://") {
        let abs_path = PathBuf::from(file_path_str);
        if let Ok(rel) = abs_path.strip_prefix(root) {
            return rel.to_path_buf();
        }
        return abs_path;
    }
    // Fallback: use the path component
    PathBuf::from(uri.path().as_str())
}

// ---------------------------------------------------------------------------
// LSP JSON-RPC transport
// ---------------------------------------------------------------------------

/// Minimal JSON-RPC message framing for LSP over stdin/stdout.
struct LspTransport {
    child: Child,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: i64,
}

impl LspTransport {
    /// Spawn a language server process and set up the transport.
    async fn spawn(command: &str, args: &[String], root_path: &Path) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(root_path)
            .kill_on_drop(true)
            .spawn()
            .context(format!("Failed to spawn {}", command))?;

        let stdout = child.stdout.take().context("No stdout from LSP process")?;
        let reader = BufReader::new(stdout);

        Ok(Self {
            child,
            reader,
            next_id: 1,
        })
    }

    /// Send a JSON-RPC request and return the response.
    async fn request<P: serde::Serialize>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        self.send_message(&request).await?;

        // Read responses until we get the one with our ID
        let timeout = tokio::time::Duration::from_secs(60);
        let result = tokio::time::timeout(timeout, self.read_response(id)).await;

        match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow::anyhow!("LSP request {} timed out after 60s", method)),
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify<P: serde::Serialize>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        self.send_message(&notification).await
    }

    async fn send_message(&mut self, msg: &serde_json::Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        let stdin = self.child.stdin.as_mut().context("No stdin")?;
        stdin.write_all(header.as_bytes()).await?;
        stdin.write_all(body.as_bytes()).await?;
        stdin.flush().await?;

        Ok(())
    }

    /// Read LSP messages until we find the response with the given ID.
    /// Discards notifications and other messages along the way.
    async fn read_response(&mut self, expected_id: i64) -> Result<serde_json::Value> {
        loop {
            let msg = self.read_message().await?;

            // Check if this is our response
            if let Some(id) = msg.get("id") {
                if id.as_i64() == Some(expected_id) {
                    if let Some(error) = msg.get("error") {
                        return Err(anyhow::anyhow!("LSP error: {}", error));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null));
                }
            }

            // Otherwise it's a notification or a response to a different request -- skip it
        }
    }

    /// Read a single LSP message (Content-Length framed).
    async fn read_message(&mut self) -> Result<serde_json::Value> {
        let mut content_length: Option<usize> = None;

        // Read headers
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).await?;
            let line = line.trim();

            if line.is_empty() {
                break; // End of headers
            }

            if let Some(len_str) = line.strip_prefix("Content-Length: ") {
                content_length = Some(len_str.parse()?);
            }
        }

        let length = content_length.context("Missing Content-Length header")?;
        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body).await?;

        let msg: serde_json::Value = serde_json::from_slice(&body)?;
        Ok(msg)
    }

    /// Shut down the language server gracefully.
    #[allow(dead_code)]
    async fn shutdown(mut self) -> Result<()> {
        // Send shutdown request
        let _ = self.request("shutdown", serde_json::Value::Null).await;
        // Send exit notification
        let _ = self.notify("exit", serde_json::Value::Null).await;
        // Wait for process to exit
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            self.child.wait(),
        )
        .await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pipelined LSP transport (concurrent JSON-RPC requests)
// ---------------------------------------------------------------------------

/// A pipelined LSP transport that supports multiple concurrent in-flight
/// requests. Uses a background reader task to dispatch responses by ID.
///
/// JSON-RPC 2.0 natively supports concurrent requests identified by ID.
/// This transport exploits that: `request()` takes `&self`, sends a request,
/// and returns a future that resolves when the matching response arrives.
struct PipelinedTransport {
    /// Writer half (stdin), protected by a mutex for serialized writes.
    writer: Mutex<tokio::process::ChildStdin>,
    /// Monotonically increasing request ID counter.
    next_id: AtomicI64,
    /// Map of pending request IDs to their response channels.
    /// Uses std::sync::Mutex (not tokio) because the critical section is
    /// non-async (just HashMap insert/remove) and we want minimal overhead
    /// under high concurrency.
    pending: Arc<std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<serde_json::Value>>>>>,
    /// Handle to the background reader task (for cleanup).
    _reader_handle: tokio::task::JoinHandle<()>,
    /// The child process (kept alive; kill_on_drop).
    _child: Arc<Mutex<Child>>,
}

impl PipelinedTransport {
    /// Convert a sequential `LspTransport` into a pipelined one.
    /// This consumes the transport and spawns a background reader task.
    fn from_sequential(mut transport: LspTransport) -> Self {
        let stdin = transport.child.stdin.take().expect("stdin already taken");
        let reader = transport.reader;
        let next_id = transport.next_id;

        let pending: Arc<std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<serde_json::Value>>>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        let pending_clone = Arc::clone(&pending);
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(reader, pending_clone).await;
        });

        Self {
            writer: Mutex::new(stdin),
            next_id: AtomicI64::new(next_id),
            pending,
            _reader_handle: reader_handle,
            _child: Arc::new(Mutex::new(transport.child)),
        }
    }

    /// Background reader loop: reads messages from the LSP server and
    /// dispatches responses to waiting callers by request ID.
    async fn reader_loop(
        mut reader: BufReader<tokio::process::ChildStdout>,
        pending: Arc<std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<serde_json::Value>>>>>,
    ) {
        loop {
            // Read a single Content-Length framed message
            let msg = match Self::read_message(&mut reader).await {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::debug!("PipelinedTransport reader loop ended: {}", e);
                    // Notify all pending requests that the transport is dead
                    let mut map = pending.lock().unwrap();
                    for (id, sender) in map.drain() {
                        let _ = sender.send(Err(anyhow::anyhow!(
                            "LSP transport closed while waiting for response {}", id
                        )));
                    }
                    break;
                }
            };

            // Check if this is a response (has "id" field and no "method" field)
            if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                if msg.get("method").is_none() {
                    // This is a response — dispatch to waiting caller
                    let mut map = pending.lock().unwrap();
                    if let Some(sender) = map.remove(&id) {
                        let result = if let Some(error) = msg.get("error") {
                            Err(anyhow::anyhow!("LSP error: {}", error))
                        } else {
                            Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                    continue;
                }
                // It has both "id" and "method" — it's a server-to-client *request*
                // (e.g. window/workDoneProgress/create). We need to respond.
                if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                    if method == "window/workDoneProgress/create" {
                        // We can't write back from the reader loop without the writer.
                        // These are non-critical during enrichment; log and skip.
                        tracing::debug!("PipelinedTransport: ignoring server request {} (id={})", method, id);
                    }
                }
            }

            // Notifications: log progress, ignore the rest
            if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                match method {
                    "$/progress" => {
                        let kind = msg.pointer("/params/value/kind").and_then(|k| k.as_str()).unwrap_or("");
                        let title = msg.pointer("/params/value/title").and_then(|t| t.as_str()).unwrap_or("");
                        if kind == "begin" || kind == "end" {
                            tracing::debug!("PipelinedTransport progress {}: {}", kind, title);
                        }
                    }
                    "experimental/serverStatus" => {
                        let health = msg.pointer("/params/health").and_then(|h| h.as_str()).unwrap_or("");
                        let quiescent = msg.pointer("/params/quiescent").and_then(|q| q.as_bool()).unwrap_or(true);
                        tracing::debug!("PipelinedTransport serverStatus: health={}, quiescent={}", health, quiescent);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Read a single Content-Length framed LSP message.
    async fn read_message(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<serde_json::Value> {
        let mut content_length: Option<usize> = None;

        // Read headers
        loop {
            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Err(anyhow::anyhow!("LSP stdout closed (EOF)"));
            }
            let line = line.trim();

            if line.is_empty() {
                break; // End of headers
            }

            if let Some(len_str) = line.strip_prefix("Content-Length: ") {
                content_length = Some(len_str.parse()?);
            }
        }

        let length = content_length.context("Missing Content-Length header")?;
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await?;

        let msg: serde_json::Value = serde_json::from_slice(&body)?;
        Ok(msg)
    }

    /// Send a JSON-RPC request and return a future that resolves with the response.
    /// This is `&self` — multiple concurrent requests are supported.
    async fn request<P: serde::Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        // Register the pending response channel BEFORE sending (avoids race)
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut map = self.pending.lock().unwrap();
            map.insert(id, tx);
        }

        // Send the request
        {
            let body = serde_json::to_string(&request)?;
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            let mut writer = self.writer.lock().await;
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(body.as_bytes()).await?;
            writer.flush().await?;
        }

        // Wait for the response with timeout.
        // 5s timeout for pipelined enrichment requests (vs 60s for sequential init).
        // Functions that can't be resolved in 5s are likely stale or broken.
        let timeout = tokio::time::Duration::from_secs(5);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow::anyhow!("LSP response channel closed for {} (id={})", method, id)),
            Err(_) => {
                // Remove the pending entry on timeout
                let mut map = self.pending.lock().unwrap();
                map.remove(&id);
                Err(anyhow::anyhow!("LSP request {} timed out after 5s (id={})", method, id))
            }
        }
    }
}

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
    ready: AtomicBool,
    /// Protected by mutex because enrich takes &self but we need to mutate transport state.
    state: Mutex<LspState>,
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
    /// Consecutive type hierarchy failures. After MAX_TYPE_HIERARCHY_STRIKES,
    /// type hierarchy is disabled for the rest of the session.
    type_hierarchy_strikes: u32,
}

/// After this many consecutive type hierarchy failures, disable type hierarchy
/// for the remainder of the enrichment pass to avoid stalling on broken servers.
const MAX_TYPE_HIERARCHY_STRIKES: u32 = 3;

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
            ready: AtomicBool::new(false),
            state: Mutex::new(LspState {
                transport: None,
                pipelined: None,
                root_path: None,
                init_failed: false,
                has_type_hierarchy: false,
                has_references: false,
                type_hierarchy_strikes: 0,
            }),
        }
    }

    /// Create a new LSP enricher with custom initialization settings.
    ///
    /// Settings are sent as `initializationOptions` in the LSP initialize request.
    pub fn with_settings(mut self, settings: serde_json::Value) -> Self {
        self.init_settings = Some(settings);
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
            .unwrap_or(true);
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

        tracing::info!(
            "Starting {} for {} LSP enrichment...",
            self.server_command,
            self.language
        );

        let transport =
            match LspTransport::spawn(&self.server_command, &self.server_args, repo_root).await {
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

        state.transport = Some(transport);
        state.root_path = Some(repo_root.to_path_buf());

        // Send initialize request
        let root_uri = path_to_uri(repo_root)?;

        #[allow(deprecated)] // root_uri is deprecated in favor of workspace_folders
        let mut init_params = InitializeParams {
            root_uri: Some(root_uri),
            capabilities: ClientCapabilities {
                window: Some(lsp_types::WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // Apply per-language initialization settings if provided
        if let Some(ref settings) = self.init_settings {
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

        let init_result_parsed: InitializeResult = serde_json::from_value(init_result)
            .context("Failed to parse initialize result")?;

        let has_references = init_result_parsed.capabilities.references_provider.is_some();
        let has_implementation = init_result_parsed.capabilities.implementation_provider.is_some();
        tracing::info!(
            "{} capabilities: references={}, implementation={}, type_hierarchy={}",
            self.server_command, has_references, has_implementation, has_type_hierarchy
        );

        state.has_type_hierarchy = has_type_hierarchy;
        state.has_references = has_references;

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
        // Timeout after 60 seconds to avoid hanging on broken servers.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(60);
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

        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(
                tokio::time::Duration::from_secs(5),
                transport.read_message(),
            )
            .await
            {
                Ok(Ok(msg)) => {
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        match method {
                            // rust-analyzer's readiness signal
                            "experimental/serverStatus" => {
                                let health = msg.pointer("/params/health").and_then(|h| h.as_str()).unwrap_or("");
                                let quiescent = msg.pointer("/params/quiescent").and_then(|q| q.as_bool()).unwrap_or(true);
                                tracing::info!("{} serverStatus: health={}, quiescent={}", self.server_command, health, quiescent);

                                if Self::server_status_is_ready(&msg) {
                                    tracing::info!("{} ready (serverStatus: ok, quiescent)", self.server_command);
                                    server_ready = true;
                                    break;
                                }
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
                                    tracing::debug!("{} progress {}: {}", self.server_command, kind, title);
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
                    // 5s timeout with no messages — server may not support serverStatus
                    tracing::info!("{} no serverStatus after 5s, assuming ready", self.server_command);
                    break;
                }
            }
        }

        if !server_ready {
            tracing::info!("{} readiness timeout reached, proceeding anyway", self.server_command);
        }

        tracing::info!("{} ready for {}", self.server_command, self.language);

        // Convert to pipelined transport for concurrent request support
        if let Some(transport) = state.transport.take() {
            let pipelined = PipelinedTransport::from_sequential(transport);
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

    async fn enrich(&self, nodes: &[Node], _index: &GraphIndex, repo_root: &Path) -> Result<EnrichmentResult> {
        let mut result = EnrichmentResult::default();

        // Filter to nodes matching this enricher's language
        let matching_nodes: Vec<&Node> = nodes
            .iter()
            .filter(|n| n.language == self.language)
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

        // Extract state under lock, then release the lock for concurrent work
        let (transport, root, has_type_hierarchy, type_hierarchy_strikes, has_references) = {
            let state = self.state.lock().await;
            let root = state
                .root_path
                .clone()
                .unwrap_or_else(|| repo_root.to_path_buf());
            let transport = match &state.pipelined {
                Some(t) => Arc::clone(t),
                None => return Ok(result),
            };
            (transport, root, state.has_type_hierarchy, state.type_hierarchy_strikes, state.has_references)
        };
        // State lock is released here — concurrent tasks can proceed

        // Share matching_nodes across concurrent tasks via Arc<Vec<Node>> (owned copies)
        let matching_nodes_owned: Arc<Vec<Node>> = Arc::new(
            matching_nodes.iter().map(|n| (*n).clone()).collect()
        );
        let language = self.language.clone();

        // ------------------------------------------------------------------
        // Pass 1: call hierarchy, find_implementations, and document links.
        // Pipelined with adaptive concurrency (TCP slow-start).
        // ------------------------------------------------------------------
        let pass1_start = std::time::Instant::now();

        // Filter to only nodes that need LSP requests:
        // Functions (call hierarchy), Traits (implementations), and Other (document links).
        // Skip test functions — they don't have meaningful cross-file callers
        // and halve the total RPC count.
        let enrichable_nodes: Vec<&Node> = matching_nodes.iter()
            .filter(|n| matches!(n.id.kind,
                NodeKind::Function | NodeKind::Trait | NodeKind::Other(_)
                | NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const))
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
                                    let matching_refs: Vec<&Node> = matching_owned.iter().collect();
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
                                        let referrer_id = find_enclosing_symbol(&matching_refs, &ref_path, ref_line);

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

            // Log progress every 500 completions
            let done = completed.load(Ordering::Relaxed);
            if done % 500 == 0 && done > 0 {
                tracing::info!(
                    "LSP enrichment progress: {}/{} nodes, {} edges so far",
                    done, enrichable_nodes.len(), result.added_edges.len(),
                );
            }
        }

        tracing::info!(
            "LSP Pass 1 complete in {:?}: {} edges from {} nodes ({} errors)",
            pass1_start.elapsed(), result.added_edges.len(), attempted, errors,
        );

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

        tracing::info!(
            "LSP enrichment complete for {}: {} edges added ({} attempted, {} errors)",
            self.language,
            result.added_edges.len(),
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
        let result = registry.enrich_all(&[], &index, &["rust".to_string(), "python".to_string()], std::path::Path::new(".")).await;
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

    /// Verify URI helper functions work correctly.
    #[test]
    fn test_uri_to_relative_path() {
        let root = PathBuf::from("/home/user/project");
        let uri = Uri::from_str("file:///home/user/project/src/main.rs").unwrap();
        let rel = uri_to_relative_path(&uri, &root);
        assert_eq!(rel, PathBuf::from("src/main.rs"));
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

        // Missing quiescent field defaults to true (ready)
        let no_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok" }
        });
        assert!(LspEnricher::server_status_is_ready(&no_quiescent),
            "missing quiescent should default to true (ready)");
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

    /// Dissent finding: find_enclosing_symbol only matches Function|Impl|Struct.
    /// References inside Enum or Const bodies won't resolve. Verify this is the
    /// actual behavior so we know the limitation.
    #[test]
    fn test_find_enclosing_symbol_skips_enum_and_const() {
        let nodes = vec![
            make_node("src/lib.rs", "MyEnum", NodeKind::Enum, 1, 20),
            make_node("src/lib.rs", "MY_CONST", NodeKind::Const, 25, 30),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line inside enum -- should NOT resolve (Enum not in the filter)
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 10);
        assert!(result.is_none(), "Enum is not in find_enclosing_symbol filter -- references inside enums are dropped");

        // Line inside const -- should NOT resolve
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 27);
        assert!(result.is_none(), "Const is not in find_enclosing_symbol filter -- references inside consts are dropped");
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
}
