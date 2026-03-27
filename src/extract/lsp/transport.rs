//! LSP JSON-RPC transport layer.
//!
//! Two transports:
//! - [`LspTransport`]: minimal sequential framing used during initialization.
//! - [`PipelinedTransport`]: concurrent JSON-RPC for enrichment passes.
//!
//! Also provides URI helper functions shared by both transports and the enricher.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use lsp_types::Uri;

use crate::graph::Node;
use crate::graph::{NodeId, NodeKind};

// ---------------------------------------------------------------------------
// URI helpers
// ---------------------------------------------------------------------------

/// Convert a file path to a `file://` URI string, then parse as lsp_types::Uri.
pub(super) fn path_to_uri(path: &Path) -> Result<Uri> {
    // Canonicalize to absolute path
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let uri_string = format!("file://{}", abs.display());
    Uri::from_str(&uri_string).map_err(|e| anyhow::anyhow!("Invalid URI: {}", e))
}

/// Find the narrowest enclosing function/impl/struct at a given file + line.
/// Returns None if no symbol contains this location.
pub(super) fn find_enclosing_symbol(nodes: &[&Node], file: &Path, line: usize) -> Option<NodeId> {
    let file_matches: Vec<_> = nodes.iter().filter(|n| n.id.file == file).collect();

    if file_matches.is_empty() {
        // Try matching just the filename if full path doesn't match
        let file_str = file.to_string_lossy();
        tracing::debug!(
            "No nodes match path '{}', trying {} node paths",
            file_str,
            nodes
                .iter()
                .map(|n| n.id.file.display().to_string())
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        // Log first few node paths for comparison
        for n in nodes.iter().take(3) {
            tracing::debug!("  node path sample: '{}'", n.id.file.display());
        }
    }

    file_matches
        .iter()
        .filter(|n| {
            matches!(
                n.id.kind,
                NodeKind::Function
                    | NodeKind::Impl
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Enum
                    | NodeKind::TypeAlias
                    | NodeKind::Const
            )
        })
        .filter(|n| n.line_start <= line && n.line_end >= line)
        .min_by_key(|n| n.line_end - n.line_start)
        .map(|n| n.id.clone())
}

/// Extract a relative file path from an LSP URI, relative to a given root.
///
/// Uses `url::Url::to_file_path()` to properly percent-decode URI segments
/// (e.g. `%20` → space, `%28` → `(`), matching the behaviour of
/// `resolve_type_hierarchy_item`.  Falls back to a raw strip of the `file://`
/// prefix for non-standard URIs that the `url` crate cannot parse.
pub(super) fn uri_to_relative_path(uri: &Uri, root: &Path) -> PathBuf {
    let uri_str = uri.as_str();

    // Primary path: use url::Url for correct percent-decoding.
    if let Ok(parsed) = url::Url::parse(uri_str) {
        if let Ok(abs_path) = parsed.to_file_path() {
            let rel = abs_path.strip_prefix(root).unwrap_or(&abs_path);
            return rel.to_path_buf();
        }
        tracing::debug!(
            uri = uri_str,
            "uri_to_relative_path: url::Url parsed but to_file_path() failed; falling back to raw strip"
        );
    } else {
        tracing::debug!(
            uri = uri_str,
            "uri_to_relative_path: url::Url::parse failed; falling back to raw strip"
        );
    }

    // Fallback for non-standard URIs: strip the scheme prefix without decoding.
    if let Some(file_path_str) = uri_str.strip_prefix("file://") {
        let abs_path = PathBuf::from(file_path_str);
        if let Ok(rel) = abs_path.strip_prefix(root) {
            return rel.to_path_buf();
        }
        return abs_path;
    }

    PathBuf::from(uri.path().as_str())
}

// ---------------------------------------------------------------------------
// LSP JSON-RPC transport
// ---------------------------------------------------------------------------

/// Minimal JSON-RPC message framing for LSP over stdin/stdout.
pub(super) struct LspTransport {
    pub(super) child: Child,
    pub(super) reader: BufReader<tokio::process::ChildStdout>,
    pub(super) next_id: i64,
}

impl LspTransport {
    /// Spawn a language server process and set up the transport.
    pub(super) async fn spawn(command: &str, args: &[String], root_path: &Path) -> Result<Self> {
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
    pub(super) async fn request<P: serde::Serialize>(
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
            Err(_) => Err(anyhow::anyhow!(
                "LSP request {} timed out after 60s",
                method
            )),
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub(super) async fn notify<P: serde::Serialize>(
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

    pub(super) async fn send_message(&mut self, msg: &serde_json::Value) -> Result<()> {
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
            if let Some(id) = msg.get("id")
                && id.as_i64() == Some(expected_id)
            {
                if let Some(error) = msg.get("error") {
                    return Err(anyhow::anyhow!("LSP error: {}", error));
                }
                return Ok(msg
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }

            // Otherwise it's a notification or a response to a different request -- skip it
        }
    }

    /// Read a single LSP message (Content-Length framed).
    pub(super) async fn read_message(&mut self) -> Result<serde_json::Value> {
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
    pub(super) async fn shutdown(mut self) -> Result<()> {
        // Send shutdown request
        let _ = self.request("shutdown", serde_json::Value::Null).await;
        // Send exit notification
        let _ = self.notify("exit", serde_json::Value::Null).await;
        // Wait for process to exit
        let _ = tokio::time::timeout(tokio::time::Duration::from_secs(5), self.child.wait()).await;
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
pub(super) struct PipelinedTransport {
    /// Writer half (stdin), protected by a mutex for serialized writes.
    writer: Mutex<tokio::process::ChildStdin>,
    /// Monotonically increasing request ID counter.
    next_id: AtomicI64,
    /// Map of pending request IDs to their response channels.
    /// Uses std::sync::Mutex (not tokio) because the critical section is
    /// non-async (just HashMap insert/remove) and we want minimal overhead
    /// under high concurrency.
    pending: Arc<
        std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<serde_json::Value>>>>,
    >,
    /// Buffer for textDocument/publishDiagnostics notifications.
    /// Maps document URI to the most-recently-received diagnostics array.
    /// The reader loop fills this as notifications arrive; enrichment reads it
    /// via the shared Arc in `LspState.diagnostics_sink`.
    /// This field is kept here to ensure the sink lives as long as the transport.
    #[allow(dead_code)]
    diagnostics_sink: Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
    /// Live quiescence flag updated by the background reader loop.
    /// Set to true whenever `experimental/serverStatus { quiescent: true }` is
    /// received — even after the initialization deadline. This allows later
    /// `enrich()` calls to proceed once RA finishes indexing, instead of being
    /// permanently blocked by a one-time timeout snapshot.
    pub quiescent_flag: Arc<AtomicBool>,
    /// Handle to the background reader task (for cleanup).
    _reader_handle: tokio::task::JoinHandle<()>,
    /// The child process (kept alive; kill_on_drop).
    _child: Arc<Mutex<Child>>,
}

impl PipelinedTransport {
    /// Convert a sequential `LspTransport` into a pipelined one with a shared
    /// diagnostics sink and quiescence flag. The caller provides the sink so it
    /// can read captured `textDocument/publishDiagnostics` notifications after
    /// enrichment. The `initial_quiescent` flag is set from the initialization
    /// wait; the reader loop will continue updating it as later serverStatus
    /// notifications arrive.
    pub(super) fn from_sequential_with_diag_sink(
        mut transport: LspTransport,
        diagnostics_sink: Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
        initial_quiescent: bool,
    ) -> Self {
        let stdin = transport.child.stdin.take().expect("stdin already taken");
        let reader = transport.reader;
        let next_id = transport.next_id;

        let pending: Arc<
            std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<serde_json::Value>>>>,
        > = Arc::new(std::sync::Mutex::new(HashMap::new()));

        let quiescent_flag = Arc::new(AtomicBool::new(initial_quiescent));

        let pending_clone = Arc::clone(&pending);
        let diag_clone = Arc::clone(&diagnostics_sink);
        let quiescent_clone = Arc::clone(&quiescent_flag);
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(reader, pending_clone, diag_clone, quiescent_clone).await;
        });

        Self {
            writer: Mutex::new(stdin),
            next_id: AtomicI64::new(next_id),
            pending,
            diagnostics_sink,
            quiescent_flag,
            _reader_handle: reader_handle,
            _child: Arc::new(Mutex::new(transport.child)),
        }
    }

    /// Background reader loop: reads messages from the LSP server and
    /// dispatches responses to waiting callers by request ID.
    /// Also captures `textDocument/publishDiagnostics` notifications into
    /// `diagnostics_sink` (keyed by document URI) for later consumption.
    /// Updates `quiescent_flag` whenever `experimental/serverStatus { quiescent: true }`
    /// is received — allowing later `enrich()` calls to proceed after RA finishes indexing.
    async fn reader_loop(
        mut reader: BufReader<tokio::process::ChildStdout>,
        pending: Arc<
            std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<serde_json::Value>>>>,
        >,
        diagnostics_sink: Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
        quiescent_flag: Arc<AtomicBool>,
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
                            "LSP transport closed while waiting for response {}",
                            id
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
                            Ok(msg
                                .get("result")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                    continue;
                }
                // It has both "id" and "method" — it's a server-to-client *request*
                // (e.g. window/workDoneProgress/create). We need to respond.
                if let Some(method) = msg.get("method").and_then(|m| m.as_str())
                    && method == "window/workDoneProgress/create"
                {
                    // We can't write back from the reader loop without the writer.
                    // These are non-critical during enrichment; log and skip.
                    tracing::debug!(
                        "PipelinedTransport: ignoring server request {} (id={})",
                        method,
                        id
                    );
                }
            }

            // Notifications: log progress, capture diagnostics, ignore the rest
            if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                match method {
                    "$/progress" => {
                        let kind = msg
                            .pointer("/params/value/kind")
                            .and_then(|k| k.as_str())
                            .unwrap_or("");
                        let title = msg
                            .pointer("/params/value/title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        if kind == "begin" || kind == "end" {
                            tracing::debug!("PipelinedTransport progress {}: {}", kind, title);
                        }
                    }
                    "experimental/serverStatus" => {
                        let health = msg
                            .pointer("/params/health")
                            .and_then(|h| h.as_str())
                            .unwrap_or("");
                        let quiescent = msg
                            .pointer("/params/quiescent")
                            .and_then(|q| q.as_bool())
                            .unwrap_or(true);
                        tracing::debug!(
                            "PipelinedTransport serverStatus: health={}, quiescent={}",
                            health,
                            quiescent
                        );
                        // Live-update the quiescent flag. Once RA becomes quiescent
                        // (even after the initialization timeout), later enrich() calls
                        // can proceed. This prevents the one-time timeout from permanently
                        // blocking all subsequent enrichment for the session.
                        if quiescent {
                            quiescent_flag.store(true, Ordering::Release);
                            tracing::info!(
                                "PipelinedTransport: server became quiescent (health={}), Pass 1 now enabled",
                                health
                            );
                        }
                    }
                    "textDocument/publishDiagnostics" => {
                        // Capture diagnostics for later consumption.
                        // params.uri: document URI
                        // params.diagnostics: array of Diagnostic objects
                        if let Some(uri) = msg.pointer("/params/uri").and_then(|u| u.as_str()) {
                            let diags = msg
                                .pointer("/params/diagnostics")
                                .and_then(|d| d.as_array())
                                .cloned()
                                .unwrap_or_default();
                            let mut sink = diagnostics_sink.lock().unwrap();
                            if diags.is_empty() {
                                // Empty diagnostic list means this file is clean — remove any prior entry
                                sink.remove(uri);
                            } else {
                                sink.insert(uri.to_string(), diags);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Read a single Content-Length framed LSP message.
    async fn read_message(
        reader: &mut BufReader<tokio::process::ChildStdout>,
    ) -> Result<serde_json::Value> {
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
    pub(super) async fn request<P: serde::Serialize>(
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
        // 30s timeout for pipelined enrichment requests. The old 5s timeout
        // was too aggressive: rust-analyzer may still be finishing indexing
        // when the first batch of queries arrives, and complex call hierarchy
        // lookups on large codebases can legitimately take 10-20s.
        let timeout = tokio::time::Duration::from_secs(30);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "LSP response channel closed for {} (id={})",
                method,
                id
            )),
            Err(_) => {
                // Remove the pending entry on timeout
                let mut map = self.pending.lock().unwrap();
                map.remove(&id);
                Err(anyhow::anyhow!(
                    "LSP request {} timed out after 30s (id={})",
                    method,
                    id
                ))
            }
        }
    }
}
