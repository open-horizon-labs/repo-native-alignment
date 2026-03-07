//! Pluggable extractor stack: scanner -> extractors -> graph nodes + edges.
//!
//! Two-phase design:
//! - Phase 1 (sync): `Extractor` trait — tree-sitter, schema extractors run at startup.
//! - Phase 2 (async): `Enricher` trait — LSP enrichers run in background (not yet implemented).
//!
//! The `ExtractorRegistry` dispatches by file extension, then calls `can_handle()`
//! for fine-grained checks. Multiple extractors can handle the same file.

pub mod go;
pub mod python;
pub mod rust;
pub mod typescript;

use std::path::Path;

use anyhow::Result;

use crate::graph::{Edge, Node};
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

// #[async_trait::async_trait]
// pub trait Enricher: Send + Sync {
//     fn languages(&self) -> &[&str];
//     fn is_ready(&self) -> bool;
//     async fn enrich(&self, graph: &GraphIndex) -> Result<EnrichmentResult>;
// }

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
        registry.register(Box::new(rust::RustExtractor::new()));
        registry.register(Box::new(python::PythonExtractor::new()));
        registry.register(Box::new(typescript::TypeScriptExtractor::new()));
        registry.register(Box::new(go::GoExtractor::new()));
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
        assert_eq!(registry.len(), 4); // rust, python, typescript, go
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
            }],
            edges: vec![],
        };
        a.merge(b);
        assert_eq!(a.nodes.len(), 1);
    }
}
