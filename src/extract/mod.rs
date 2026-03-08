//! Pluggable extractor stack: scanner -> extractors -> graph nodes + edges.
//!
//! Two-phase design:
//! - Phase 1 (sync): `Extractor` trait — tree-sitter, schema extractors run at startup.
//! - Phase 2 (async): `Enricher` trait — LSP enrichers run in background (not yet implemented).
//!
//! The `ExtractorRegistry` dispatches by file extension, then calls `can_handle()`
//! for fine-grained checks. Multiple extractors can handle the same file.

pub mod bash;
pub mod go;
pub mod hcl;
pub mod java;
pub mod javascript;
pub mod json_extractor;
pub mod lsp;
pub mod markdown;
pub mod openapi;
pub mod proto;
pub mod python;
pub mod rust;
pub mod sql;
pub mod toml_extractor;
pub mod typescript;
pub mod yaml_extractor;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;
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
        // Infrastructure / config
        registry.register(Box::new(hcl::HclExtractor::new()));
        registry.register(Box::new(json_extractor::JsonExtractor::new()));
        registry.register(Box::new(toml_extractor::TomlExtractor::new()));
        registry.register(Box::new(yaml_extractor::YamlExtractor::new()));
        // Schema / API
        registry.register(Box::new(markdown::MarkdownExtractor::new()));
        registry.register(Box::new(proto::ProtoExtractor::new()));
        registry.register(Box::new(sql::SqlExtractor::new()));
        registry.register(Box::new(openapi::OpenApiExtractor::new()));
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
            if !extractor.extensions().iter().any(|e| *e == ext) {
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
    /// For each changed/new file, reads the file content and runs matching extractors.
    /// `repo_root` is needed to construct absolute paths for reading files.
    pub fn extract_scan_result(
        &self,
        repo_root: &Path,
        scan_result: &ScanResult,
    ) -> ExtractionResult {
        let mut result = ExtractionResult::default();

        // Process changed + new files (not deleted ones)
        let files_to_process: Vec<_> = scan_result
            .changed_files
            .iter()
            .chain(scan_result.new_files.iter())
            .collect();

        for rel_path in files_to_process {
            let abs_path = repo_root.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read {}: {}", abs_path.display(), e);
                    continue;
                }
            };
            let file_result = self.extract_file(rel_path, &content);
            result.merge(file_result);
        }

        result
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
    /// Returns a merged `EnrichmentResult` from all enrichers.
    pub async fn enrich_all(
        &self,
        nodes: &[Node],
        index: &GraphIndex,
        languages: &[String],
        repo_root: &Path,
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

            match enricher.enrich(nodes, index, repo_root).await {
                Ok(enrichment) => {
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
        assert_eq!(registry.len(), 15); // rust, python, typescript, javascript, go, java, bash, hcl, json, toml, yaml, markdown, proto, sql, openapi
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
}
