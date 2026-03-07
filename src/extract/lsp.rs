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

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use lsp_types::{
    ClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse,
    InitializeParams, InitializeResult, Location, Position, ReferenceContext, ReferenceParams,
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
    transport: Option<LspTransport>,
    /// Cached root path from initialization.
    root_path: Option<PathBuf>,
    /// Whether we already tried and failed to start the language server.
    init_failed: bool,
}

impl LspEnricher {
    /// Create a new LSP enricher for the given language server.
    ///
    /// - `language`: language identifier (e.g., "rust", "python")
    /// - `command`: binary to spawn (e.g., "rust-analyzer", "pyright-langserver")
    /// - `args`: command-line arguments (e.g., &["--stdio"])
    /// - `extensions`: file extensions this enricher handles (e.g., &["rs"])
    pub fn new(language: &str, command: &str, args: &[&str], extensions: &[&str]) -> Self {
        Self {
            language: language.to_string(),
            server_command: command.to_string(),
            server_args: args.iter().map(|s| s.to_string()).collect(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            init_settings: None,
            ready: AtomicBool::new(false),
            state: Mutex::new(LspState {
                transport: None,
                root_path: None,
                init_failed: false,
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

        if state.transport.is_some() {
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
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };

        // Apply per-language initialization settings if provided
        if let Some(ref settings) = self.init_settings {
            init_params.initialization_options = Some(settings.clone());
        }

        let transport = state.transport.as_mut().unwrap();
        let init_result = transport.request("initialize", &init_params).await?;

        // Parse to verify it's valid
        let _result: InitializeResult = serde_json::from_value(init_result)
            .context("Failed to parse initialize result")?;

        // Send initialized notification
        transport
            .notify("initialized", serde_json::json!({}))
            .await?;

        tracing::info!("{} initialized successfully for {}", self.server_command, self.language);
        self.ready.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// Find all references to a symbol at the given position.
    async fn find_references(
        transport: &mut LspTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: file_uri.clone(),
                },
                position: Position { line, character },
            },
            context: ReferenceContext {
                include_declaration: false,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result: serde_json::Value =
            transport.request("textDocument/references", &params).await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        let locations: Vec<Location> = serde_json::from_value(result).unwrap_or_default();

        Ok(locations)
    }

    /// Find implementations of a trait/interface at the given position.
    async fn find_implementations(
        transport: &mut LspTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        // GotoImplementationParams is a type alias for GotoDefinitionParams
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

        // Implementation can return Location, Vec<Location>, or Vec<LocationLink>
        // GotoDefinitionResponse handles all variants via #[serde(untagged)]
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
}

#[async_trait::async_trait]
impl Enricher for LspEnricher {
    fn languages(&self) -> &[&str] {
        // Return a static slice for each language.
        // We leak the string to get a &'static str — this is fine because
        // enrichers live for the entire program lifetime.
        // However, to avoid repeated leaks, we match known languages to statics.
        match self.language.as_str() {
            "rust" => &["rust"],
            "python" => &["python"],
            "typescript" => &["typescript"],
            "go" => &["go"],
            "markdown" => &["markdown"],
            _ => &[],
        }
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn name(&self) -> &str {
        // Return a static name for known servers to avoid lifetime issues.
        match self.language.as_str() {
            "rust" => "rust-analyzer-lsp",
            "python" => "pyright-lsp",
            "typescript" => "typescript-lsp",
            "go" => "gopls-lsp",
            "markdown" => "marksman-lsp",
            _ => "unknown-lsp",
        }
    }

    async fn enrich(&self, nodes: &[Node], _index: &GraphIndex) -> Result<EnrichmentResult> {
        let mut result = EnrichmentResult::default();

        // Filter to nodes matching this enricher's language
        let matching_nodes: Vec<&Node> = nodes
            .iter()
            .filter(|n| n.language == self.language)
            .collect();

        if matching_nodes.is_empty() {
            return Ok(result);
        }

        // Determine repo root from the first node's file path context.
        // The nodes have relative paths; we need the repo root for absolute paths.
        let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Try to initialize the language server
        if let Err(e) = self.ensure_initialized(&repo_root).await {
            tracing::debug!("LSP enrichment skipped for {}: {}", self.language, e);
            return Ok(result);
        }

        let mut state = self.state.lock().await;
        let root = state
            .root_path
            .clone()
            .unwrap_or_else(|| repo_root.clone());
        let transport = match state.transport.as_mut() {
            Some(t) => t,
            None => return Ok(result),
        };

        // Process functions and traits/interfaces
        for node in &matching_nodes {
            let abs_path = root.join(&node.id.file);
            let file_uri = match path_to_uri(&abs_path) {
                Ok(u) => u,
                Err(_) => continue,
            };

            // line_start is 1-based, LSP positions are 0-based
            let line = (node.line_start.saturating_sub(1)) as u32;

            match node.id.kind {
                NodeKind::Function => {
                    // Find references (call sites) for functions
                    match Self::find_references(transport, &file_uri, line, 0).await {
                        Ok(locations) => {
                            for loc in locations {
                                let ref_path = uri_to_relative_path(&loc.uri, &root);

                                // Create a Calls edge from the reference location to this function
                                let caller_id = NodeId {
                                    root: String::new(),
                                    file: ref_path,
                                    name: format!("ref@L{}", loc.range.start.line + 1),
                                    kind: NodeKind::Function,
                                };

                                result.added_edges.push(Edge {
                                    from: caller_id,
                                    to: node.id.clone(),
                                    kind: EdgeKind::Calls,
                                    source: ExtractionSource::Lsp,
                                    confidence: Confidence::Confirmed,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "References lookup failed for {}: {}",
                                node.id.name,
                                e
                            );
                        }
                    }
                }
                NodeKind::Trait => {
                    // Find implementations for traits/interfaces
                    match Self::find_implementations(transport, &file_uri, line, 0).await {
                        Ok(locations) => {
                            for loc in locations {
                                let impl_path = uri_to_relative_path(&loc.uri, &root);

                                let impl_id = NodeId {
                                    root: String::new(),
                                    file: impl_path,
                                    name: format!("impl@L{}", loc.range.start.line + 1),
                                    kind: NodeKind::Impl,
                                };

                                result.added_edges.push(Edge {
                                    from: impl_id,
                                    to: node.id.clone(),
                                    kind: EdgeKind::Implements,
                                    source: ExtractionSource::Lsp,
                                    confidence: Confidence::Confirmed,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Implementation lookup failed for {}: {}",
                                node.id.name,
                                e
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        tracing::info!(
            "LSP enrichment complete for {}: {} edges added",
            self.language,
            result.added_edges.len()
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
            ) -> Result<EnrichmentResult> {
                Ok(EnrichmentResult::default())
            }
        }

        let enricher = DummyEnricher;
        assert_eq!(enricher.languages(), &["test"]);
        assert!(enricher.is_ready());
        assert_eq!(enricher.name(), "dummy");

        let index = GraphIndex::new();
        let result = enricher.enrich(&[], &index).await.unwrap();
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
        assert_eq!(python.name(), "pyright-lsp");
        assert_eq!(python.server_args, vec!["--stdio"]);

        let typescript = LspEnricher::new(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "tsx", "js", "jsx"],
        );
        assert_eq!(typescript.languages(), &["typescript"]);
        assert_eq!(typescript.name(), "typescript-lsp");
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

        let result = enricher.enrich(&nodes, &index).await.unwrap();
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
        assert_eq!(registry.len(), 5); // rust, python, typescript, go, markdown
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
        let result = registry.enrich_all(&[], &index, &["rust".to_string(), "python".to_string()]).await;
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
        let _result = enricher.enrich(&nodes, &index).await;
    }
}
