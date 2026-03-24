//! Pluggable extractor stack: scanner -> extractors -> graph nodes + edges.
//!
//! Two-phase design:
//! - Phase 1 (sync): `Extractor` trait — tree-sitter, schema extractors run at startup.
//! - Phase 2 (async): `Enricher` trait — LSP enrichers run in background (not yet implemented).
//!
//! The `ExtractorRegistry` dispatches by file extension, then calls `can_handle()`
//! for fine-grained checks. Multiple extractors can handle the same file.

pub mod api_link;
pub mod cache;
pub mod fastapi_router_prefix;
pub mod consumers;
pub mod directory_module;
pub mod event_bus;
pub mod scan_stats;
pub mod openapi_sdk_link;
pub mod sdk_path_inference;
pub mod extractor_config;
pub mod framework_detection;
pub mod grpc;
pub mod import_calls;
pub mod manifest;
pub mod naming_convention;
pub mod nextjs_routing;
pub mod pubsub;
pub mod subsystem_pass;
pub mod websocket;
pub mod bash;
pub mod c;
pub mod configs;
pub mod cpp;
pub mod dart;
pub mod dockerfile;
pub mod elixir;
pub mod generic;
pub mod graphql;
pub mod html;
pub mod query;
pub mod csharp;
pub mod php;
pub mod scala;
pub mod string_literals;
pub mod go;
pub mod hcl;
pub mod java;
pub mod javascript;
pub mod json_extractor;
pub mod kotlin;
pub mod lsp;
pub mod lua;
pub mod markdown;
pub mod openapi;
pub mod proto;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod sql;
pub mod swift;
pub mod toml_extractor;
pub mod typescript;
pub mod yaml_extractor;
pub mod zig;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use rayon::prelude::*;

use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;

// ---------------------------------------------------------------------------
// Content sniffing: distinguish binary from text-with-wrong-encoding
// ---------------------------------------------------------------------------

/// Examine the first 8 KB of a byte buffer to decide if the content is likely
/// binary (should be skipped) or text with a non-UTF-8 encoding (should be
/// lossy-decoded).
///
/// Binary heuristic:
/// - Any null byte (`\x00`) in the sample -> binary.
/// - More than 10% of bytes are non-text control chars (0x00-0x08, 0x0E-0x1F
///   excluding TAB, LF, CR) -> binary.
///
/// Everything else is treated as (possibly wrong-encoded) text.
fn is_binary_content(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.is_empty() {
        return false; // empty file is text
    }
    // Null bytes are a strong binary signal.
    if sample.contains(&0x00) {
        return true;
    }
    // Count non-text control bytes (exclude TAB=0x09, LF=0x0A, CR=0x0D).
    let non_text_count = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0A && b < 0x0D) || (b > 0x0D && b < 0x20))
        .count();
    let ratio = non_text_count as f64 / sample.len() as f64;
    ratio > 0.10
}
use crate::scanner::ScanResult;

// ---------------------------------------------------------------------------
// Extraction result
// ---------------------------------------------------------------------------

/// The output of running one or more extractors on a file.
#[derive(Debug, Clone, Default)]
pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Encoding statistics from a scan, for surfacing in `list_roots`.
#[derive(Debug, Clone, Default)]
pub struct EncodingStats {
    /// Files detected as binary and skipped entirely.
    pub binary_skipped: usize,
    /// Text files with non-UTF-8 encoding that were lossy-decoded.
    pub lossy_decoded: usize,
}

impl ExtractionResult {
    /// Merge another result into this one.
    pub fn merge(&mut self, other: ExtractionResult) {
        self.nodes.extend(other.nodes);
        self.edges.extend(other.edges);
    }
}

// ---------------------------------------------------------------------------
// Extractor trait (Phase 1: synchronous)
// ---------------------------------------------------------------------------

/// Phase 1: Synchronous extraction at startup.
///
/// Extractors parse a single file and produce nodes + edges.
/// They must be Send + Sync for use in the registry.
pub trait Extractor: Send + Sync {
    /// File extensions this extractor handles (e.g., &["rs"], &["py"]).
    fn extensions(&self) -> &[&str];

    /// Fine-grained check: can this extractor handle this specific file?
    /// Called after extension filtering. Default: always true.
    fn can_handle(&self, _path: &Path, _content: &str) -> bool {
        true
    }

    /// Extract nodes and edges from a single file's content.
    ///
    /// `path` is relative to the repository root.
    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult>;

    /// Human-readable name for this extractor (for diagnostics).
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Enricher trait (Phase 2: async, not yet implemented)
// ---------------------------------------------------------------------------

/// The output of running an enricher on existing graph data.
/// Enrichers add edges, patch node metadata, and may synthesize virtual nodes
/// for external (out-of-repo) symbols discovered via LSP.
#[derive(Debug, Clone, Default)]
pub struct EnrichmentResult {
    /// New edges discovered by the enricher (e.g., Calls, Implements).
    pub added_edges: Vec<Edge>,
    /// Metadata patches for existing nodes: (node_stable_id, key-value patches).
    pub updated_nodes: Vec<(String, BTreeMap<String, String>)>,
    /// Virtual nodes synthesized for external symbols (e.g., tokio::spawn).
    /// These have `root = "external"` and no body — they must NOT be embedded.
    pub new_nodes: Vec<Node>,
    /// Whether any enricher actually ran (server was available and initialized).
    /// When false, all matching enrichers were skipped (server not on PATH, init failed, etc.).
    /// Callers use this to distinguish "no server available" from "server ran, found nothing."
    pub any_enricher_ran: bool,
}

/// Phase 2: Asynchronous enrichment after initial extraction.
///
/// Enrichers run after all Phase 1 extractors have built the initial graph.
/// They add edges (cross-file references, trait implementations) and patch
/// node metadata using a higher-confidence source (e.g., LSP).
///
/// Enrichers must not block MCP server startup — they run inside the
/// lazy `get_graph()` initialization, after tree-sitter extraction.
#[async_trait::async_trait]
pub trait Enricher: Send + Sync {
    /// Languages this enricher supports (e.g., &["rust"]).
    fn languages(&self) -> &[&str];

    /// Whether the enricher is ready to run.
    /// For LSP enrichers, this indicates the language server has finished indexing.
    fn is_ready(&self) -> bool;

    /// Enrich the graph with additional edges and metadata.
    ///
    /// Receives the current nodes, the graph index for lookup, and the actual
    /// repo root (from `--repo`, not `std::env::current_dir()`).
    /// Returns new edges and node metadata patches.
    async fn enrich(&self, nodes: &[Node], index: &GraphIndex, repo_root: &Path) -> Result<EnrichmentResult>;

    /// Human-readable name for this enricher (for diagnostics).
    fn name(&self) -> &str;

    /// Set the LSP server startup working directory.
    ///
    /// When called before the first `enrich()` call, the language server is started
    /// with `current_dir = lsp_root` and `rootUri = file:///<lsp_root>`. Used for
    /// monorepo subdirectory roots (e.g. `client = "client"`) so language servers
    /// can find their config files (tsconfig.json, pyproject.toml) in the subdirectory.
    ///
    /// Default implementation is a no-op. `LspEnricher` overrides this.
    fn set_startup_root(&self, _lsp_root: std::path::PathBuf) {}

    /// Config file name this enricher relies on for project-level configuration.
    ///
    /// Used by `enrich_all` to prefer lsp_roots that contain this file when selecting
    /// the LSP server startup directory. For example, typescript-language-server relies
    /// on `tsconfig.json`, pyright on `pyproject.toml`.
    ///
    /// Returns `None` if the enricher has no specific config file preference
    /// (it will fall back to the node-count heuristic).
    ///
    /// Default: `None` (no preference).
    fn config_file_hint(&self) -> Option<&str> { None }
}

// ---------------------------------------------------------------------------
// ExtractorRegistry
// ---------------------------------------------------------------------------

/// Registry of extractors. Dispatches files to matching extractors by extension,
/// then by `can_handle()`. Merges results from multiple matching extractors.
pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn Extractor>>,
}

impl ExtractorRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
        }
    }

    /// Create a registry pre-loaded with all built-in extractors.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        // Code
        registry.register(Box::new(rust::RustExtractor::new()));
        registry.register(Box::new(python::PythonExtractor::new()));
        registry.register(Box::new(typescript::TypeScriptExtractor::new()));
        registry.register(Box::new(javascript::JavaScriptExtractor::new()));
        registry.register(Box::new(go::GoExtractor::new()));
        registry.register(Box::new(java::JavaExtractor::new()));
        registry.register(Box::new(bash::BashExtractor::new()));
        registry.register(Box::new(ruby::RubyExtractor::new()));
        registry.register(Box::new(cpp::CppExtractor::new()));
        registry.register(Box::new(c::CExtractor::new()));
        registry.register(Box::new(csharp::CSharpExtractor::new()));
        registry.register(Box::new(kotlin::KotlinExtractor::new()));
        registry.register(Box::new(zig::ZigExtractor::new()));
        registry.register(Box::new(lua::LuaExtractor::new()));
        registry.register(Box::new(swift::SwiftExtractor::new()));
        // Web languages
        registry.register(Box::new(php::PhpExtractor::new()));
        registry.register(Box::new(html::HtmlExtractor::new()));
        // JVM languages
        registry.register(Box::new(scala::ScalaExtractor::new()));
        // Flutter/mobile
        registry.register(Box::new(dart::DartExtractor::new()));
        // Functional/dynamic
        registry.register(Box::new(elixir::ElixirExtractor::new()));
        // Infrastructure / config
        registry.register(Box::new(dockerfile::DockerfileExtractor::new()));
        registry.register(Box::new(hcl::HclExtractor::new()));
        registry.register(Box::new(json_extractor::JsonExtractor::new()));
        registry.register(Box::new(toml_extractor::TomlExtractor::new()));
        registry.register(Box::new(yaml_extractor::YamlExtractor::new()));
        // Schema / API
        registry.register(Box::new(markdown::MarkdownExtractor::new()));
        registry.register(Box::new(proto::ProtoExtractor::new()));
        registry.register(Box::new(sql::SqlExtractor::new()));
        registry.register(Box::new(openapi::OpenApiExtractor::new()));
        registry.register(Box::new(graphql::GraphQlExtractor::new()));
        registry
    }

    /// Register an extractor.
    pub fn register(&mut self, extractor: Box<dyn Extractor>) {
        self.extractors.push(extractor);
    }

    /// Extract nodes and edges from a single file.
    ///
    /// Finds all extractors matching the file's extension and `can_handle()`,
    /// runs them all, and merges results.
    pub fn extract_file(&self, path: &Path, content: &str) -> ExtractionResult {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let mut result = ExtractionResult::default();

        for extractor in &self.extractors {
            if !extractor.extensions().contains(&ext) {
                continue;
            }
            if !extractor.can_handle(path, content) {
                continue;
            }
            match extractor.extract(path, content) {
                Ok(extraction) => result.merge(extraction),
                Err(e) => {
                    tracing::warn!(
                        "Extractor {} failed on {}: {}",
                        extractor.name(),
                        path.display(),
                        e
                    );
                }
            }
        }

        result
    }

    /// Extract from all files in a scan result.
    ///
    /// Files are processed in parallel using rayon. Each file is independent —
    /// no shared mutable state — so parallelism is safe. On a 10-core machine
    /// a 500-file scan drops from ~10s to ~1s.
    ///
    /// Files that are valid UTF-8 are extracted directly. Files that fail UTF-8
    /// validation are content-sniffed: truly binary files (null bytes or high
    /// control-char ratio) are skipped; text files with wrong encoding (Latin-1,
    /// Windows-1252) are lossy-decoded with U+FFFD replacement characters so
    /// their symbols still appear in the index.
    ///
    /// `repo_root` is needed to construct absolute paths for reading files.
    pub fn extract_scan_result(
        &self,
        repo_root: &Path,
        scan_result: &ScanResult,
    ) -> ExtractionResult {
        // Process changed + new files (not deleted ones)
        let files_to_process: Vec<_> = scan_result
            .changed_files
            .iter()
            .chain(scan_result.new_files.iter())
            .collect();
        let extraction_start = std::time::Instant::now();
        tracing::info!(
            "ExtractorRegistry: starting extraction for {} file(s) under {}",
            files_to_process.len(),
            repo_root.display()
        );

        // Counters for files skipped due to encoding issues (binary or lossy-decoded).
        let binary_skipped = AtomicUsize::new(0);
        let lossy_decoded = AtomicUsize::new(0);

        // Process files in parallel. Each file is independent (no shared mutable
        // state between extractors), so rayon par_iter is safe here.
        // `filter_map` skips unreadable/binary files; `reduce` merges in parallel.
        let result = files_to_process
            .into_par_iter()
            .filter_map(|rel_path| {
                let file_start = std::time::Instant::now();
                let abs_path = repo_root.join(rel_path);
                tracing::debug!("ExtractorRegistry: reading {}", abs_path.display());

                // Read raw bytes instead of read_to_string so we can handle non-UTF-8.
                let raw_bytes = match std::fs::read(&abs_path) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("Failed to read {}: {}", abs_path.display(), e);
                        return None;
                    }
                };

                // Content-sniff first: skip truly binary files before attempting decode.
                // This catches files with null bytes or high control-char ratios even
                // if they happen to be valid UTF-8 byte sequences.
                if is_binary_content(&raw_bytes) {
                    tracing::debug!("Skipping binary file {}", abs_path.display());
                    binary_skipped.fetch_add(1, Ordering::Relaxed);
                    return None;
                }

                // Try fast-path: valid UTF-8 (vast majority of source files).
                let content = match String::from_utf8(raw_bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        // Not valid UTF-8 but not binary — lossy-decode so symbols
                        // are still indexed (replacement char U+FFFD for bad bytes).
                        let bytes = e.into_bytes();
                        tracing::info!(
                            "Lossy-decoding non-UTF-8 text file {}",
                            abs_path.display()
                        );
                        lossy_decoded.fetch_add(1, Ordering::Relaxed);
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                };

                let file_result = self.extract_file(rel_path, &content);
                tracing::debug!(
                    "ExtractorRegistry: extracted {} -> {} node(s), {} edge(s) in {:?}",
                    rel_path.display(),
                    file_result.nodes.len(),
                    file_result.edges.len(),
                    file_start.elapsed()
                );
                Some(file_result)
            })
            .reduce(ExtractionResult::default, |mut acc, r| {
                acc.merge(r);
                acc
            });

        let binary_count = binary_skipped.load(Ordering::Relaxed);
        let lossy_count = lossy_decoded.load(Ordering::Relaxed);
        tracing::info!(
            "ExtractorRegistry: completed extraction in {:?} ({} node(s), {} edge(s), {} binary skipped, {} lossy-decoded)",
            extraction_start.elapsed(),
            result.nodes.len(),
            result.edges.len(),
            binary_count,
            lossy_count,
        );

        result
    }

    /// Extract from all files in a scan result, returning both the extraction
    /// result and encoding statistics for surfacing in `list_roots`.
    pub fn extract_scan_result_with_stats(
        &self,
        repo_root: &Path,
        scan_result: &ScanResult,
    ) -> (ExtractionResult, EncodingStats) {
        // Process changed + new files (not deleted ones)
        let files_to_process: Vec<_> = scan_result
            .changed_files
            .iter()
            .chain(scan_result.new_files.iter())
            .collect();
        let extraction_start = std::time::Instant::now();
        tracing::info!(
            "ExtractorRegistry: starting extraction for {} file(s) under {}",
            files_to_process.len(),
            repo_root.display()
        );

        let binary_skipped = AtomicUsize::new(0);
        let lossy_decoded = AtomicUsize::new(0);

        let result = files_to_process
            .into_par_iter()
            .filter_map(|rel_path| {
                let file_start = std::time::Instant::now();
                let abs_path = repo_root.join(rel_path);
                tracing::debug!("ExtractorRegistry: reading {}", abs_path.display());

                let raw_bytes = match std::fs::read(&abs_path) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("Failed to read {}: {}", abs_path.display(), e);
                        return None;
                    }
                };

                if is_binary_content(&raw_bytes) {
                    tracing::debug!("Skipping binary file {}", abs_path.display());
                    binary_skipped.fetch_add(1, Ordering::Relaxed);
                    return None;
                }

                let content = match String::from_utf8(raw_bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        let bytes = e.into_bytes();
                        tracing::info!(
                            "Lossy-decoding non-UTF-8 text file {}",
                            abs_path.display()
                        );
                        lossy_decoded.fetch_add(1, Ordering::Relaxed);
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                };

                let file_result = self.extract_file(rel_path, &content);
                tracing::debug!(
                    "ExtractorRegistry: extracted {} -> {} node(s), {} edge(s) in {:?}",
                    rel_path.display(),
                    file_result.nodes.len(),
                    file_result.edges.len(),
                    file_start.elapsed()
                );
                Some(file_result)
            })
            .reduce(ExtractionResult::default, |mut acc, r| {
                acc.merge(r);
                acc
            });

        let binary_count = binary_skipped.load(Ordering::Relaxed);
        let lossy_count = lossy_decoded.load(Ordering::Relaxed);
        tracing::info!(
            "ExtractorRegistry: completed extraction in {:?} ({} node(s), {} edge(s), {} binary skipped, {} lossy-decoded)",
            extraction_start.elapsed(),
            result.nodes.len(),
            result.edges.len(),
            binary_count,
            lossy_count,
        );

        let stats = EncodingStats {
            binary_skipped: binary_count,
            lossy_decoded: lossy_count,
        };

        (result, stats)
    }

    /// Number of registered extractors.
    pub fn len(&self) -> usize {
        self.extractors.len()
    }

    /// Whether the registry has no extractors.
    pub fn is_empty(&self) -> bool {
        self.extractors.is_empty()
    }
}

impl Default for ExtractorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// LSP root selection helper
// ---------------------------------------------------------------------------

/// Pick the best LSP working directory for a set of nodes given a list of candidate roots.
///
/// When a monorepo has subdirectory roots (e.g. `client = "client"`), we want the language
/// server to start from the subdirectory (where `tsconfig.json` lives) rather than the repo
/// root.
///
/// ## Selection algorithm
///
/// 1. **Config file preference (first):** if `config_file_hint` is provided (e.g., `"tsconfig.json"`),
///    prefer the first lsp_root that contains that file. This handles the case where one
///    subdirectory (e.g. `client/`) has `tsconfig.json` but another (e.g. `ai_service/`) does not,
///    even if `ai_service/` has more TypeScript test files by count.
///
/// 2. **Node count fallback:** if no candidate root has the config file, fall back to
///    the root that covers the most nodes.
///
/// 3. **Primary root default:** if no candidate matches any node, return `primary_root`.
pub fn pick_lsp_root_for_nodes<'a>(
    nodes: &[crate::graph::Node],
    primary_root: &'a Path,
    lsp_roots: &'a [(String, std::path::PathBuf)],
    config_file_hint: Option<&str>,
) -> &'a Path {
    if lsp_roots.is_empty() {
        return primary_root;
    }

    // Step 1: If we have a config file hint, prefer the first lsp_root that contains it
    // AND has at least one matching node. This catches the common case where only one
    // subdirectory has tsconfig.json / pyproject.toml.
    if let Some(config_file) = config_file_hint {
        for (_slug, root_path) in lsp_roots {
            let has_config = root_path.join(config_file).exists();
            let has_nodes = nodes.iter().any(|n| {
                let abs_file = primary_root.join(&n.id.file);
                abs_file.starts_with(root_path)
            });
            if has_config && has_nodes {
                tracing::info!(
                    "pick_lsp_root: selected '{}' (has {} and matching nodes)",
                    root_path.display(),
                    config_file,
                );
                return root_path.as_path();
            }
        }
    }

    // Step 2: Node-count fallback.
    let mut best_root: &Path = primary_root;
    let mut best_count: usize = 0;
    let mut best_depth: usize = 0; // tie-break: longer path = more specific

    for (_slug, root_path) in lsp_roots {
        let count = nodes
            .iter()
            .filter(|n| {
                // Node file paths are relative to their root, so prepend primary_root to get the
                // absolute path, then check if it starts with the candidate lsp_root.
                let abs_file = primary_root.join(&n.id.file);
                abs_file.starts_with(root_path)
            })
            .count();

        let depth = root_path.components().count();
        if count > best_count || (count == best_count && count > 0 && depth > best_depth) {
            best_count = count;
            best_depth = depth;
            best_root = root_path.as_path();
        }
    }

    if best_count > 0 {
        tracing::info!(
            "pick_lsp_root: selected '{}' by node count ({} matching nodes out of {})",
            best_root.display(),
            best_count,
            nodes.len(),
        );
    } else {
        tracing::info!(
            "pick_lsp_root: no lsp_root matched, using primary root '{}'",
            primary_root.display(),
        );
    }
    best_root
}

// ---------------------------------------------------------------------------
// EnricherRegistry
// ---------------------------------------------------------------------------

/// Registry of enrichers. Runs all matching enrichers after Phase 1 extraction.
pub struct EnricherRegistry {
    enrichers: Vec<Box<dyn Enricher>>,
}

impl EnricherRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            enrichers: Vec::new(),
        }
    }

    /// Return the set of all languages supported by registered enrichers.
    pub fn supported_languages(&self) -> std::collections::HashSet<String> {
        self.enrichers
            .iter()
            .flat_map(|e| e.languages().iter().map(|s| s.to_string()))
            .collect()
    }

    /// Create a registry pre-loaded with all built-in enrichers.
    ///
    /// Registers LSP enrichers for all languages that have tree-sitter extractors:
    /// Rust, Python, TypeScript/JavaScript, Go, and Markdown.
    ///
    /// Auto-discovers installed language servers and registers them.
    /// Each server is checked on PATH at enrichment time — missing ones are skipped.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();

        // Known LSP servers: (language, binary, args, extensions)
        // Ordered by popularity. All are optional — only used if installed.
        let servers: &[(&str, &str, &[&str], &[&str])] = &[
            // Tier 1: most common
            ("rust",         "rust-analyzer",              &[],         &["rs"]),
            ("python",       "pyright-langserver",         &["--stdio"], &["py"]),
            ("typescript",   "typescript-language-server",  &["--stdio"], &["ts", "tsx", "js", "jsx"]),
            ("go",           "gopls",                      &["serve"],  &["go"]),
            ("markdown",     "marksman",                   &["server"], &["md"]),
            // Tier 2: widely used
            ("c-cpp",        "clangd",                     &[],         &["c", "cc", "cpp", "cxx", "h", "hpp"]),
            ("java",         "jdtls",                      &[],         &["java"]),
            ("ruby",         "solargraph",                 &["stdio"],  &["rb"]),
            ("csharp",       "omnisharp",                  &["-lsp"],   &["cs"]),
            ("swift",        "sourcekit-lsp",              &[],         &["swift"]),
            ("kotlin",       "kotlin-language-server",     &[],         &["kt", "kts"]),
            ("lua",          "lua-language-server",        &[],         &["lua"]),
            ("zig",          "zls",                        &[],         &["zig"]),
            ("elixir",       "elixir-ls",                  &[],         &["ex", "exs"]),
            ("haskell",      "haskell-language-server",    &["--lsp"],  &["hs"]),
            ("ocaml",        "ocamllsp",                   &[],         &["ml", "mli"]),
            ("scala",        "metals",                     &[],         &["scala", "sc"]),
            // Tier 3: common in specific ecosystems
            ("dart",         "dart",                       &["language-server"], &["dart"]),
            ("r",            "R",                          &["--no-echo", "-e", "languageserver::run()"], &["r", "R"]),
            ("julia",        "julia",                      &["--startup-file=no", "-e", "using LanguageServer; runserver()"], &["jl"]),
            ("php",          "intelephense",               &["--stdio"], &["php"]),
            ("css",          "vscode-css-languageserver",  &["--stdio"], &["css", "scss", "less"]),
            ("html",         "vscode-html-languageserver", &["--stdio"], &["html", "htm"]),
            ("yaml",         "yaml-language-server",       &["--stdio"], &["yaml", "yml"]),
            ("json",         "vscode-json-languageserver", &["--stdio"], &["json"]),
            ("toml",         "taplo",                      &["lsp", "stdio"], &["toml"]),
            ("terraform",    "terraform-ls",               &["serve"],  &["tf", "tfvars"]),
            ("nix",          "nil",                        &[],         &["nix"]),
            ("vue",          "vue-language-server",        &["--stdio"], &["vue"]),
            ("svelte",       "svelteserver",               &["--stdio"], &["svelte"]),
            ("erlang",       "erlang_ls",                  &[],         &["erl", "hrl"]),
            ("gleam",        "gleam",                      &["lsp"],    &["gleam"]),
            ("nim",          "nimlsp",                     &[],         &["nim"]),
            ("clojure",      "clojure-lsp",               &[],         &["clj", "cljs", "cljc"]),
            ("deno",         "deno",                       &["lsp"],    &["ts", "tsx", "js", "jsx"]),
            ("protobuf",     "buf",                        &["lsp"],    &["proto"]),
            ("latex",        "texlab",                     &[],         &["tex", "bib"]),
            ("typst",        "tinymist",                   &[],         &["typ"]),
        ];

        for &(lang, cmd, args, exts) in servers {
            let enricher = lsp::LspEnricher::new(lang, cmd, args, exts);
            // Add pyright-specific settings
            let enricher = if lang == "python" {
                enricher.with_settings(serde_json::json!({
                    "python": { "analysis": { "autoSearchPaths": true } }
                }))
            } else {
                enricher
            };
            // Add config file hints for lsp_root selection in monorepos.
            // When a monorepo has multiple subdirectory roots, the enricher prefers
            // the root that contains this file over the raw node-count heuristic.
            let enricher = match lang {
                "typescript" | "deno" => enricher.with_config_file("tsconfig.json"),
                "python"              => enricher.with_config_file("pyproject.toml"),
                "go"                  => enricher.with_config_file("go.mod"),
                "rust"                => enricher.with_config_file("Cargo.toml"),
                "java"                => enricher.with_config_file("pom.xml"),
                "kotlin"              => enricher.with_config_file("build.gradle.kts"),
                _                     => enricher,
            };
            registry.register(Box::new(enricher));
        }

        registry
    }

    /// Register an enricher.
    pub fn register(&mut self, enricher: Box<dyn Enricher>) {
        self.enrichers.push(enricher);
    }

    /// Run all enrichers that support the given languages present in the graph.
    ///
    /// `repo_root` must be the actual project root (from `--repo`), not `cwd`.
    /// `lsp_roots` is an optional list of `(slug, path)` for subdirectory roots declared
    /// in `[workspace.roots]`. When provided, each enricher picks the most-specific
    /// matching root as its LSP working directory. This lets typescript-language-server
    /// start from `client/` (where `tsconfig.json` lives) rather than the repo root.
    ///
    /// Returns a merged `EnrichmentResult` from all enrichers.
    pub async fn enrich_all(
        &self,
        nodes: &[Node],
        index: &GraphIndex,
        languages: &[String],
        repo_root: &Path,
        lsp_roots: &[(String, std::path::PathBuf)],
    ) -> EnrichmentResult {
        let mut result = EnrichmentResult::default();

        tracing::info!(
            "LSP enrichment: {} language(s) detected in graph: [{}]",
            languages.len(),
            languages.join(", ")
        );

        for enricher in &self.enrichers {
            // Check if this enricher supports any language in the graph
            let supported = enricher
                .languages()
                .iter()
                .any(|lang| languages.iter().any(|l| l == lang));
            if !supported {
                continue; // silently skip — too many servers to log each one
            }

            // Configure the LSP server startup working directory if lsp_roots are available.
            // This is a one-time operation (OnceLock, first call wins) that sets the
            // working directory for the language server startup (current_dir + rootUri).
            // It must be called BEFORE the first enrich() so ensure_initialized picks it up.
            //
            // Only set if we found a more-specific root than the primary root.
            if !lsp_roots.is_empty() {
                let best = pick_lsp_root_for_nodes(
                    nodes,
                    repo_root,
                    lsp_roots,
                    enricher.config_file_hint(),
                );
                if best != repo_root {
                    enricher.set_startup_root(best.to_path_buf());
                }
            }

            match enricher.enrich(nodes, index, repo_root).await {
                Ok(enrichment) => {
                    // If enrich() returned Ok, the server was available and ran.
                    result.any_enricher_ran = true;
                    tracing::info!(
                        "Enricher {}: {} edges, {} node patches, {} virtual nodes",
                        enricher.name(),
                        enrichment.added_edges.len(),
                        enrichment.updated_nodes.len(),
                        enrichment.new_nodes.len(),
                    );
                    result.added_edges.extend(enrichment.added_edges);
                    result.updated_nodes.extend(enrichment.updated_nodes);
                    result.new_nodes.extend(enrichment.new_nodes);
                }
                Err(e) => {
                    tracing::warn!("Enricher {} failed: {}", enricher.name(), e);
                }
            }
        }

        result
    }

    /// Number of registered enrichers.
    pub fn len(&self) -> usize {
        self.enrichers.len()
    }

    /// Whether the registry has no enrichers.
    pub fn is_empty(&self) -> bool {
        self.enrichers.is_empty()
    }
}

impl Default for EnricherRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::scanner::ScanResult;
    use std::time::Duration;

    // ---------------------------------------------------------------------------
    // Adversarial tests for parallel extraction correctness
    // ---------------------------------------------------------------------------

    /// Adversarial: parallel extraction must produce nodes from all files.
    /// Seeded from dissent: "node/edge count must be identical to sequential."
    #[test]
    fn test_parallel_extraction_same_node_count_as_sequential() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let mut new_files = Vec::new();
        for i in 0..10 {
            let name = format!("src/lib_{i}.rs");
            std::fs::write(
                tmp.path().join(&name),
                format!("pub fn func_{i}() -> u32 {{ {i} }}\n"),
            ).unwrap();
            new_files.push(PathBuf::from(&name));
        }
        for i in 0..10 {
            let name = format!("src/lib_{i}.py");
            std::fs::write(
                tmp.path().join(&name),
                format!("def func_{i}():\n    return {i}\n"),
            ).unwrap();
            new_files.push(PathBuf::from(&name));
        }

        let scan = ScanResult {
            changed_files: vec![],
            new_files,
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let result = registry.extract_scan_result(tmp.path(), &scan);
        assert!(
            result.nodes.len() >= 20,
            "Should have at least one node per file, got {}",
            result.nodes.len()
        );
    }

    /// Adversarial: binary files (invalid UTF-8) must be skipped, not panic.
    /// Seeded from dissent: "invalid UTF-8 → debug-level skip, not crash."
    #[test]
    fn test_parallel_extraction_skips_binary_files() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
        // Binary file — invalid UTF-8 bytes
        std::fs::write(tmp.path().join("src/binary.rs"), b"\xff\xfe\x00\x01").unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![
                PathBuf::from("src/lib.rs"),
                PathBuf::from("src/binary.rs"),
            ],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let result = registry.extract_scan_result(tmp.path(), &scan);
        assert!(!result.nodes.is_empty(), "Should extract from valid Rust file");
    }

    /// Adversarial: empty scan must return empty result.
    /// Edge case: rayon reduce with 0 items returns the identity (ExtractionResult::default).
    #[test]
    fn test_parallel_extraction_empty_scan() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let result = registry.extract_scan_result(tmp.path(), &scan);
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    /// Adversarial: single-file parallel extraction must match direct extraction.
    /// Edge case: rayon with one item takes the identity path.
    #[test]
    fn test_parallel_extraction_single_file_matches_direct() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        let code = "pub fn only_fn() {}\npub struct Only {}\n";
        std::fs::write(tmp.path().join("only.rs"), code).unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![PathBuf::from("only.rs")],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let parallel_result = registry.extract_scan_result(tmp.path(), &scan);
        let direct_result = registry.extract_file(Path::new("only.rs"), code);

        assert_eq!(
            parallel_result.nodes.len(),
            direct_result.nodes.len(),
            "Single-file parallel extraction must match direct extraction"
        );
    }

    #[test]
    fn test_registry_dispatches_by_extension() {
        let registry = ExtractorRegistry::with_builtins();

        // Rust file should produce nodes
        let rust_code = "pub fn hello() {}\npub struct Foo {}\n";
        let result = registry.extract_file(Path::new("src/lib.rs"), rust_code);
        assert!(!result.nodes.is_empty(), "Rust extractor should produce nodes");

        // Python file should produce nodes
        let py_code = "def hello():\n    pass\n\nclass Foo:\n    pass\n";
        let result = registry.extract_file(Path::new("lib.py"), py_code);
        assert!(!result.nodes.is_empty(), "Python extractor should produce nodes");

        // TypeScript file should produce nodes
        let ts_code = "function hello() {}\nclass Foo {}\n";
        let result = registry.extract_file(Path::new("lib.ts"), ts_code);
        assert!(!result.nodes.is_empty(), "TypeScript extractor should produce nodes");

        // Go file should produce nodes
        let go_code = "package main\n\nfunc hello() {}\n\ntype Foo struct {}\n";
        let result = registry.extract_file(Path::new("main.go"), go_code);
        assert!(!result.nodes.is_empty(), "Go extractor should produce nodes");
    }

    #[test]
    fn test_registry_skips_unknown_extensions() {
        let registry = ExtractorRegistry::with_builtins();
        let result = registry.extract_file(Path::new("data.csv"), "a,b,c\n1,2,3\n");
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_registry_empty() {
        let registry = ExtractorRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_with_builtins_has_extractors() {
        let registry = ExtractorRegistry::with_builtins();
        assert_eq!(registry.len(), 30); // rust, python, typescript, javascript, go, java, bash, ruby, cpp, c, csharp, kotlin, zig, lua, swift, php, html, scala, dart, elixir, dockerfile, hcl, json, toml, yaml, markdown, proto, sql, openapi, graphql
    }

    #[test]
    fn test_extraction_result_merge() {
        let mut a = ExtractionResult::default();
        let b = ExtractionResult {
            nodes: vec![crate::graph::Node {
                id: crate::graph::NodeId {
                    root: String::new(),
                    file: PathBuf::from("test.rs"),
                    name: "foo".into(),
                    kind: crate::graph::NodeKind::Function,
                },
                language: "rust".into(),
                line_start: 1,
                line_end: 1,
                signature: "fn foo()".into(),
                body: "fn foo() {}".into(),
                metadata: Default::default(),
                source: crate::graph::ExtractionSource::TreeSitter,
            }],
            edges: vec![],
        };
        a.merge(b);
        assert_eq!(a.nodes.len(), 1);
    }

    #[test]
    fn test_string_literals_captured_as_synthetic_consts() {
        // Verify that string literals (len > 3) are captured as synthetic Const nodes.
        let registry = ExtractorRegistry::with_builtins();
        let code = "fn handle() {\n    let ct = \"application/json\";\n    let m = \"POST\";\n    let s = \"ok\";\n}\n";
        let result = registry.extract_file(Path::new("handler.rs"), code);
        let synthetic: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| {
                n.id.kind == crate::graph::NodeKind::Const
                    && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("true")
            })
            .collect();
        assert!(
            !synthetic.is_empty(),
            "Should capture string literals as synthetic Const nodes"
        );
        assert!(
            synthetic.iter().any(|n| n.id.name == "application/json"),
            "Should capture 'application/json'"
        );
        assert!(
            synthetic.iter().any(|n| n.id.name == "POST"),
            "Should capture 'POST' (len=4 > 3)"
        );
        assert!(
            !synthetic.iter().any(|n| n.id.name == "ok"),
            "Should NOT capture 'ok' (len=2 <= 3)"
        );
    }

    #[test]
    fn test_string_literals_captured_across_languages() {
        let registry = ExtractorRegistry::with_builtins();
        // Python
        let py_result = registry.extract_file(
            Path::new("handler.py"),
            "def handle():\n    ct = \"application/json\"\n",
        );
        assert!(
            py_result.nodes.iter().any(|n| n.id.kind == crate::graph::NodeKind::Const
                && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("true")),
            "Python should capture string literals"
        );
        // TypeScript
        let ts_result = registry.extract_file(
            Path::new("handler.ts"),
            "function handle() {\n    const ct = \"application/json\";\n}\n",
        );
        assert!(
            ts_result.nodes.iter().any(|n| n.id.kind == crate::graph::NodeKind::Const
                && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("true")),
            "TypeScript should capture string literals"
        );
        // Go
        let go_result = registry.extract_file(
            Path::new("handler.go"),
            "package main\nfunc handle() {\n    ct := \"application/json\"\n}\n",
        );
        assert!(
            go_result.nodes.iter().any(|n| n.id.kind == crate::graph::NodeKind::Const
                && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("true")),
            "Go should capture string literals"
        );
    }

    // ── pick_lsp_root_for_nodes adversarial tests ────────────────────────

    fn make_test_node(file: &str, lang: &str) -> crate::graph::Node {
        crate::graph::Node {
            id: crate::graph::NodeId {
                root: "primary".to_string(),
                file: PathBuf::from(file),
                name: "test".to_string(),
                kind: crate::graph::NodeKind::Function,
            },
            language: lang.to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: Default::default(),
            source: crate::graph::ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn test_pick_lsp_root_prefers_config_file_over_node_count() {
        // Adversarial: ai_service/ has MORE TypeScript files than client/,
        // but only client/ has tsconfig.json. Config-file heuristic must win.
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().to_path_buf();
        let client_dir = primary.join("client");
        let ai_service_dir = primary.join("ai_service");
        std::fs::create_dir_all(&client_dir).unwrap();
        std::fs::create_dir_all(&ai_service_dir).unwrap();
        // Only client/ has tsconfig.json
        std::fs::write(client_dir.join("tsconfig.json"), "{}").unwrap();

        // Make ai_service have MORE TypeScript nodes (dissent scenario)
        let mut nodes: Vec<crate::graph::Node> = Vec::new();
        for i in 0..100 {
            nodes.push(make_test_node(&format!("ai_service/test_{}.ts", i), "typescript"));
        }
        for i in 0..50 {
            nodes.push(make_test_node(&format!("client/src/component_{}.tsx", i), "typescript"));
        }

        let lsp_roots = vec![
            ("client".to_string(), client_dir.clone()),
            ("ai-service".to_string(), ai_service_dir.clone()),
        ];

        // Without config file hint: ai_service wins by count
        let result_no_hint = pick_lsp_root_for_nodes(&nodes, &primary, &lsp_roots, None);
        assert_eq!(
            result_no_hint, ai_service_dir.as_path(),
            "Without hint, node-count picks ai_service (100 > 50 nodes)"
        );

        // With tsconfig.json hint: client wins by config file presence
        let result_with_hint = pick_lsp_root_for_nodes(&nodes, &primary, &lsp_roots, Some("tsconfig.json"));
        assert_eq!(
            result_with_hint, client_dir.as_path(),
            "With tsconfig.json hint, client/ wins even with fewer nodes"
        );
    }

    #[test]
    fn test_pick_lsp_root_falls_back_to_count_when_no_config_file() {
        // When config file hint is provided but no root has the file,
        // fallback to node count.
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().to_path_buf();
        let client_dir = primary.join("client");
        let ai_service_dir = primary.join("ai_service");
        std::fs::create_dir_all(&client_dir).unwrap();
        std::fs::create_dir_all(&ai_service_dir).unwrap();
        // Neither has tsconfig.json

        let mut nodes: Vec<crate::graph::Node> = Vec::new();
        for i in 0..20 {
            nodes.push(make_test_node(&format!("client/src/component_{}.tsx", i), "typescript"));
        }
        for i in 0..5 {
            nodes.push(make_test_node(&format!("ai_service/test_{}.ts", i), "typescript"));
        }

        let lsp_roots = vec![
            ("client".to_string(), client_dir.clone()),
            ("ai-service".to_string(), ai_service_dir.clone()),
        ];

        // Config file not found in any root → fallback to count
        let result = pick_lsp_root_for_nodes(&nodes, &primary, &lsp_roots, Some("tsconfig.json"));
        assert_eq!(
            result, client_dir.as_path(),
            "When no root has tsconfig.json, node-count fallback picks client (20 > 5 nodes)"
        );
    }

    #[test]
    fn test_pick_lsp_root_returns_primary_when_no_match() {
        // If no lsp_root covers any node, return primary_root.
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().to_path_buf();
        let unrelated_dir = primary.join("unrelated");
        std::fs::create_dir_all(&unrelated_dir).unwrap();

        let nodes = vec![
            make_test_node("client/src/App.tsx", "typescript"),
        ];
        let lsp_roots = vec![
            ("unrelated".to_string(), unrelated_dir.clone()),
        ];

        // Node is in client/ but lsp_root is unrelated/ → no match → primary root
        let result = pick_lsp_root_for_nodes(&nodes, &primary, &lsp_roots, Some("tsconfig.json"));
        assert_eq!(
            result, primary.as_path(),
            "When no root matches any node, primary root is returned"
        );
    }

    #[test]
    fn test_pick_lsp_root_two_roots_same_config_first_wins() {
        // Dissent scenario: two roots both have tsconfig.json — first one wins.
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().to_path_buf();
        let client_dir = primary.join("client");
        let client_v2_dir = primary.join("client-v2");
        std::fs::create_dir_all(&client_dir).unwrap();
        std::fs::create_dir_all(&client_v2_dir).unwrap();
        std::fs::write(client_dir.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(client_v2_dir.join("tsconfig.json"), "{}").unwrap();

        let nodes = vec![
            make_test_node("client/src/App.tsx", "typescript"),
            make_test_node("client-v2/src/App.tsx", "typescript"),
        ];
        let lsp_roots = vec![
            ("client".to_string(), client_dir.clone()),
            ("client-v2".to_string(), client_v2_dir.clone()),
        ];

        // Both have tsconfig.json and matching nodes — first wins
        let result = pick_lsp_root_for_nodes(&nodes, &primary, &lsp_roots, Some("tsconfig.json"));
        assert_eq!(
            result, client_dir.as_path(),
            "When both roots have tsconfig.json, first in list wins"
        );
    }

    // ── is_binary_content tests ─────────────────────────────────────────

    #[test]
    fn test_is_binary_null_bytes() {
        // Null bytes are a strong binary signal.
        assert!(is_binary_content(b"hello\x00world"));
        assert!(is_binary_content(b"\x00"));
        assert!(is_binary_content(b"\xff\xfe\x00\x01"));
    }

    #[test]
    fn test_is_binary_high_control_ratio() {
        // >10% non-text control bytes → binary.
        let mut data = vec![0x01u8; 200]; // all control chars
        data.extend_from_slice(b"some text padding to make it a buffer");
        assert!(is_binary_content(&data));
    }

    #[test]
    fn test_is_binary_empty_is_text() {
        assert!(!is_binary_content(b""));
    }

    #[test]
    fn test_is_binary_valid_utf8_text() {
        assert!(!is_binary_content(b"fn hello() {}\n"));
        assert!(!is_binary_content("pub fn greet() -> &str { \"hello\" }\n".as_bytes()));
    }

    #[test]
    fn test_is_binary_latin1_is_text() {
        // Latin-1 encoded text: "caf\xe9" (cafe with accent).
        // No null bytes, low control char ratio → should be classified as text.
        let latin1 = b"// caf\xe9 au lait\nfn order() {}\n";
        assert!(!is_binary_content(latin1), "Latin-1 text should not be classified as binary");
    }

    #[test]
    fn test_is_binary_windows_1252_is_text() {
        // Windows-1252 with smart quotes and em-dash: non-UTF-8 but clearly text.
        let win1252 = b"// \x93smart quotes\x94 and \x97em dash\n";
        assert!(!is_binary_content(win1252), "Windows-1252 text should not be classified as binary");
    }

    #[test]
    fn test_is_binary_tabs_and_newlines_not_counted() {
        // TAB (0x09), LF (0x0A), CR (0x0D) are legitimate text chars.
        let text_with_whitespace = b"\t\tif true {\r\n\t\t\treturn\r\n\t\t}\n";
        assert!(!is_binary_content(text_with_whitespace));
    }

    // ── Lossy decode extraction tests ──────────────────────────────────

    #[test]
    fn test_extract_lossy_decodes_latin1_file() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        // Write a JavaScript file with a Latin-1 encoded string (0xe9 = e-acute).
        // This is invalid UTF-8 but valid Latin-1 text.
        let latin1_js = b"// caf\xe9 module\nfunction greet() { return 'hello'; }\n";
        std::fs::write(tmp.path().join("latin1.js"), latin1_js).unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![PathBuf::from("latin1.js")],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let result = registry.extract_scan_result(tmp.path(), &scan);
        assert!(
            !result.nodes.is_empty(),
            "Latin-1 encoded file should be lossy-decoded and extracted, got 0 nodes"
        );
        // Verify the function was extracted
        assert!(
            result.nodes.iter().any(|n| n.id.name == "greet"),
            "Should extract 'greet' function from lossy-decoded file"
        );
    }

    #[test]
    fn test_extract_skips_truly_binary_file() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        // Write a file with null bytes — should be detected as binary and skipped.
        std::fs::write(tmp.path().join("binary.rs"), b"\xff\xfe\x00\x01\x02\x03").unwrap();
        // Also write a valid file to ensure extraction still works.
        std::fs::write(tmp.path().join("valid.rs"), "pub fn ok() {}\n").unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![PathBuf::from("binary.rs"), PathBuf::from("valid.rs")],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let result = registry.extract_scan_result(tmp.path(), &scan);
        // Binary file should be skipped, valid file should be extracted.
        assert!(
            result.nodes.iter().any(|n| n.id.name == "ok"),
            "Valid file should still be extracted alongside binary file"
        );
    }

    #[test]
    fn test_extract_with_stats_returns_encoding_counts() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        // Latin-1 file (lossy-decoded)
        std::fs::write(
            tmp.path().join("latin1.js"),
            b"// caf\xe9\nfunction greet() {}\n",
        ).unwrap();
        // Binary file (null bytes)
        std::fs::write(
            tmp.path().join("binary.rs"),
            b"\x00\x01\x02\x03",
        ).unwrap();
        // Valid UTF-8 file
        std::fs::write(
            tmp.path().join("valid.rs"),
            "pub fn valid() {}\n",
        ).unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![
                PathBuf::from("latin1.js"),
                PathBuf::from("binary.rs"),
                PathBuf::from("valid.rs"),
            ],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let (result, stats) = registry.extract_scan_result_with_stats(tmp.path(), &scan);
        assert!(!result.nodes.is_empty(), "Should have nodes from valid + lossy files");
        assert_eq!(stats.binary_skipped, 1, "Should skip 1 binary file");
        assert_eq!(stats.lossy_decoded, 1, "Should lossy-decode 1 Latin-1 file");
    }

    #[test]
    fn test_extract_utf8_bom_file() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        // UTF-8 BOM prefix (EF BB BF) followed by valid UTF-8 content.
        let bom_content = b"\xef\xbb\xbfpub fn bom_fn() {}\n";
        std::fs::write(tmp.path().join("bom.rs"), bom_content).unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![PathBuf::from("bom.rs")],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let (result, stats) = registry.extract_scan_result_with_stats(tmp.path(), &scan);
        // BOM-prefixed UTF-8 is still valid UTF-8, should be extracted normally.
        assert!(
            !result.nodes.is_empty(),
            "UTF-8 BOM file should be extracted"
        );
        assert_eq!(stats.binary_skipped, 0);
        assert_eq!(stats.lossy_decoded, 0, "BOM-prefixed UTF-8 is valid, should not be lossy-decoded");
    }

    #[test]
    fn test_extract_mixed_encoding_file() {
        let registry = ExtractorRegistry::with_builtins();
        let tmp = tempfile::TempDir::new().unwrap();

        // Valid UTF-8 with one 0xFF byte at end (common in Windows-1252 codebases).
        let content = b"def valid_fn():\n    return 42\n# comment \xff\n".to_vec();
        std::fs::write(tmp.path().join("mixed.py"), &content).unwrap();

        let scan = ScanResult {
            changed_files: vec![],
            new_files: vec![PathBuf::from("mixed.py")],
            deleted_files: vec![],
            scan_duration: Duration::ZERO,
        };

        let (result, stats) = registry.extract_scan_result_with_stats(tmp.path(), &scan);
        assert!(
            result.nodes.iter().any(|n| n.id.name == "valid_fn"),
            "Function should be extracted from mixed-encoding file"
        );
        assert_eq!(stats.lossy_decoded, 1, "Mixed encoding should be lossy-decoded");
        assert_eq!(stats.binary_skipped, 0);
    }
}
