//! SCIP-based enricher for compiler-grade symbol graph extraction.
//!
//! Phase 2 enrichment: runs a SCIP indexer as a child process, parses the
//! resulting `index.scip` protobuf file, and extracts high-confidence edges
//! (CALLS, IMPLEMENTS) from compiler-grade symbol information.
//!
//! Supported SCIP indexers (auto-detected on PATH):
//! - `rust-analyzer scip .` (Rust)
//! - `scip-python` (Python, Pyright-based)
//! - `scip-typescript index` (TypeScript/JavaScript)
//! - `scip-go` (Go)
//!
//! Design decisions:
//! - Runs the indexer on first `enrich()` call, not at startup
//! - If the indexer binary is not installed, logs info and skips gracefully
//! - 120-second timeout per indexer invocation; child process is killed on timeout
//! - Parses `index.scip` once, extracts all edges in a single pass
//! - Caches the parsed index keyed by git HEAD — skips re-indexing if HEAD unchanged
//! - Cleans up `index.scip` after parsing

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use protobuf::Enum as ProtobufEnum;
use protobuf::Message;

use crate::graph::index::GraphIndex;
use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::{Enricher, EnrichmentResult};

// ---------------------------------------------------------------------------
// SCIP indexer configuration
// ---------------------------------------------------------------------------

/// A SCIP indexer binary and how to invoke it.
#[derive(Debug, Clone)]
struct ScipIndexerConfig {
    /// Human-readable name (e.g., "rust-analyzer").
    name: &'static str,
    /// Binary name to look up on PATH.
    binary: &'static str,
    /// Arguments to pass to the binary.
    args: &'static [&'static str],
    /// Languages this indexer handles.
    languages: &'static [&'static str],
    /// File extensions this indexer covers (for documentation/matching).
    #[allow(dead_code)]
    extensions: &'static [&'static str],
    /// Output file name (usually "index.scip").
    output_file: &'static str,
}

/// Known SCIP indexers ordered by maturity/popularity.
const KNOWN_INDEXERS: &[ScipIndexerConfig] = &[
    ScipIndexerConfig {
        name: "rust-analyzer",
        binary: "rust-analyzer",
        args: &["scip", "."],
        languages: &["rust"],
        extensions: &["rs"],
        output_file: "index.scip",
    },
    ScipIndexerConfig {
        name: "scip-python",
        binary: "scip-python",
        args: &["--project-name", "project", "."],
        languages: &["python"],
        extensions: &["py"],
        output_file: "index.scip",
    },
    ScipIndexerConfig {
        name: "scip-typescript",
        binary: "scip-typescript",
        args: &["index"],
        languages: &["typescript", "javascript"],
        extensions: &["ts", "tsx", "js", "jsx"],
        output_file: "index.scip",
    },
    ScipIndexerConfig {
        name: "scip-go",
        binary: "scip-go",
        args: &[],
        languages: &["go"],
        extensions: &["go"],
        output_file: "index.scip",
    },
];

/// Timeout for running a SCIP indexer.
const INDEXER_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// ScipEnricher
// ---------------------------------------------------------------------------

/// Cached SCIP index keyed by (indexer name, git HEAD OID).
struct CachedIndex {
    head_oid: String,
    index: scip::types::Index,
}

/// Enricher that runs SCIP indexers and parses their output to produce
/// high-confidence edges.
///
/// Auto-detects available SCIP indexers on PATH. Runs them against the repo
/// root, parses the `index.scip` protobuf, and extracts symbol definitions,
/// references, and relationships.
pub struct ScipEnricher {
    /// Which languages this enricher instance covers (only those with
    /// indexers actually found on PATH). Stored as `&'static str` refs
    /// borrowed from `KNOWN_INDEXERS` so we can return `&[&str]`.
    supported_languages: Vec<&'static str>,
    /// Indexer configs that were found on PATH.
    available_indexers: Vec<ScipIndexerConfig>,
    /// Cache: indexer name -> CachedIndex. Reused if git HEAD hasn't changed.
    cache: Mutex<HashMap<String, CachedIndex>>,
}

impl ScipEnricher {
    /// Create a new SCIP enricher, auto-detecting available indexers on PATH.
    pub fn new() -> Self {
        let mut supported_languages: Vec<&'static str> = Vec::new();
        let mut available_indexers = Vec::new();

        for config in KNOWN_INDEXERS {
            if is_binary_on_path(config.binary) {
                tracing::info!(
                    "SCIP indexer '{}' ({}) found on PATH",
                    config.name,
                    config.binary
                );
                for &lang in config.languages {
                    if !supported_languages.contains(&lang) {
                        supported_languages.push(lang);
                    }
                }
                available_indexers.push(config.clone());
            }
        }

        if available_indexers.is_empty() {
            tracing::info!(
                "No SCIP indexers found on PATH. \
                 Install one for compiler-grade edges: \
                 rust-analyzer (Rust), scip-python (Python), \
                 scip-typescript (TypeScript), scip-go (Go)"
            );
        }

        Self {
            supported_languages,
            available_indexers,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Run a single SCIP indexer and parse the output.
    /// Uses a cached result if git HEAD hasn't changed since the last run.
    async fn run_indexer(
        &self,
        config: &ScipIndexerConfig,
        repo_root: &Path,
    ) -> Result<scip::types::Index> {
        // Check git HEAD for cache freshness
        let head_oid = git_head_oid(repo_root).unwrap_or_default();

        if !head_oid.is_empty() {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(config.name) {
                if cached.head_oid == head_oid {
                    tracing::info!(
                        "SCIP cache hit for '{}' (HEAD={})",
                        config.name,
                        &head_oid[..8.min(head_oid.len())]
                    );
                    return Ok(cached.index.clone());
                }
            }
        }

        let output_path = repo_root.join(config.output_file);

        // Clean up any stale index file
        if output_path.exists() {
            std::fs::remove_file(&output_path)
                .with_context(|| format!("Failed to remove stale {}", config.output_file))?;
        }

        tracing::info!(
            "Running SCIP indexer: {} {} (in {})",
            config.binary,
            config.args.join(" "),
            repo_root.display()
        );

        // Spawn the child process so we can kill it on timeout.
        // We use `spawn()` + `wait()` instead of `.output()` so the child
        // handle stays alive and can be killed if the timeout fires.
        let mut child = tokio::process::Command::new(config.binary)
            .args(config.args)
            .current_dir(repo_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn SCIP indexer '{}'", config.name))?;

        let wait_result = tokio::time::timeout(INDEXER_TIMEOUT, child.wait()).await;

        match wait_result {
            Err(_elapsed) => {
                // Timeout — kill the child process to prevent resource leak
                tracing::warn!(
                    "SCIP indexer '{}' timed out after {}s, killing child process",
                    config.name,
                    INDEXER_TIMEOUT.as_secs()
                );
                let _ = child.kill().await;
                anyhow::bail!(
                    "SCIP indexer '{}' timed out after {}s",
                    config.name,
                    INDEXER_TIMEOUT.as_secs()
                );
            }
            Ok(Err(e)) => {
                // Process error
                anyhow::bail!("SCIP indexer '{}' failed: {}", config.name, e);
            }
            Ok(Ok(status)) => {
                if !status.success() {
                    // Try to read stderr for diagnostics
                    let stderr_msg = if let Some(mut stderr) = child.stderr.take() {
                        let mut buf = Vec::new();
                        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf).await;
                        String::from_utf8_lossy(&buf).chars().take(500).collect::<String>()
                    } else {
                        String::new()
                    };
                    anyhow::bail!(
                        "SCIP indexer '{}' exited with {}: {}",
                        config.name,
                        status,
                        stderr_msg
                    );
                }
            }
        }

        // Parse the output protobuf
        if !output_path.exists() {
            anyhow::bail!(
                "SCIP indexer '{}' succeeded but {} not found",
                config.name,
                config.output_file
            );
        }

        let bytes = std::fs::read(&output_path)
            .with_context(|| format!("Failed to read {}", output_path.display()))?;

        tracing::info!(
            "Parsing SCIP index: {} ({} bytes)",
            output_path.display(),
            bytes.len()
        );

        let index = scip::types::Index::parse_from_bytes(&bytes)
            .with_context(|| "Failed to parse SCIP protobuf")?;

        // Clean up
        let _ = std::fs::remove_file(&output_path);

        tracing::info!(
            "SCIP index parsed: {} document(s), {} external symbol(s)",
            index.documents.len(),
            index.external_symbols.len()
        );

        // Cache the result
        if !head_oid.is_empty() {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(
                config.name.to_string(),
                CachedIndex {
                    head_oid,
                    index: index.clone(),
                },
            );
        }

        Ok(index)
    }

    /// Extract edges from a parsed SCIP index.
    ///
    /// `existing_edge_ids` contains the `stable_id()` of edges already in the
    /// graph (from tree-sitter or other sources). SCIP edges that duplicate an
    /// existing edge are skipped to prevent cross-source duplication.
    fn extract_edges(
        &self,
        index: &scip::types::Index,
        nodes: &[Node],
        _graph_index: &GraphIndex,
        _repo_root: &Path,
        existing_edge_ids: &HashSet<String>,
    ) -> EnrichmentResult {
        let mut result = EnrichmentResult::default();

        // Build a lookup: (file, line) -> Node for fast matching
        let mut file_line_index: HashMap<(PathBuf, usize), Vec<&Node>> = HashMap::new();
        for node in nodes {
            for line in node.line_start..=node.line_end {
                file_line_index
                    .entry((node.id.file.clone(), line))
                    .or_default()
                    .push(node);
            }
        }

        // Build a lookup: symbol string -> list of defining NodeIds
        let mut symbol_defs: HashMap<String, Vec<NodeId>> = HashMap::new();

        // First pass: collect symbol definitions from all documents
        for doc in &index.documents {
            let rel_path = PathBuf::from(&doc.relative_path);

            for occ in &doc.occurrences {
                if occ.symbol.is_empty() || scip::symbol::is_local_symbol(&occ.symbol) {
                    continue;
                }

                let is_definition = (occ.symbol_roles & scip::types::SymbolRole::Definition.value()) != 0;

                if is_definition {
                    let line = occurrence_start_line(occ);
                    if let Some(node_id) = find_node_at(&file_line_index, &rel_path, line) {
                        symbol_defs
                            .entry(occ.symbol.clone())
                            .or_default()
                            .push(node_id);
                    }
                }
            }
        }

        // Also gather definitions from SymbolInformation.relationships
        for doc in &index.documents {
            for sym_info in &doc.symbols {
                for rel in &sym_info.relationships {
                    if rel.is_implementation {
                        // sym_info.symbol implements rel.symbol
                        if let (Some(implementor_ids), Some(interface_ids)) = (
                            symbol_defs.get(&sym_info.symbol),
                            symbol_defs.get(&rel.symbol),
                        ) {
                            for impl_id in implementor_ids {
                                for iface_id in interface_ids {
                                    let edge = Edge {
                                        from: impl_id.clone(),
                                        to: iface_id.clone(),
                                        kind: EdgeKind::Implements,
                                        source: ExtractionSource::Scip,
                                        confidence: Confidence::Confirmed,
                                    };
                                    // Skip if tree-sitter (or another source) already has this edge
                                    if !existing_edge_ids.contains(&edge.stable_id()) {
                                        result.added_edges.push(edge);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Second pass: extract reference edges (CALLS)
        for doc in &index.documents {
            let rel_path = PathBuf::from(&doc.relative_path);

            for occ in &doc.occurrences {
                if occ.symbol.is_empty() || scip::symbol::is_local_symbol(&occ.symbol) {
                    continue;
                }

                let is_definition = (occ.symbol_roles & scip::types::SymbolRole::Definition.value()) != 0;

                if !is_definition {
                    // This is a reference (call site)
                    let ref_line = occurrence_start_line(occ);

                    // Find the enclosing symbol at the reference site
                    let caller = find_enclosing_node(&file_line_index, &rel_path, ref_line);

                    // Find the target symbol definition(s)
                    let targets = symbol_defs.get(&occ.symbol);

                    if let (Some(caller_id), Some(target_ids)) = (caller, targets) {
                        for target_id in target_ids {
                            // Skip self-references
                            if caller_id == *target_id {
                                continue;
                            }
                            let edge = Edge {
                                from: caller_id.clone(),
                                to: target_id.clone(),
                                kind: EdgeKind::Calls,
                                source: ExtractionSource::Scip,
                                confidence: Confidence::Confirmed,
                            };
                            // Skip if tree-sitter (or another source) already has this edge
                            if !existing_edge_ids.contains(&edge.stable_id()) {
                                result.added_edges.push(edge);
                            }
                        }
                    }
                }
            }
        }

        // Deduplicate edges within SCIP output
        let edge_count_before = result.added_edges.len();
        result.added_edges.sort_by(|a, b| a.stable_id().cmp(&b.stable_id()));
        result.added_edges.dedup_by(|a, b| a.stable_id() == b.stable_id());

        tracing::info!(
            "SCIP enrichment: {} edges ({} before dedup, {} skipped as cross-source dupes)",
            result.added_edges.len(),
            edge_count_before,
            edge_count_before - result.added_edges.len()
        );

        result
    }
}

// ---------------------------------------------------------------------------
// Enricher trait implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl Enricher for ScipEnricher {
    fn languages(&self) -> &[&str] {
        // Returns only languages whose indexer binary was found on PATH.
        &self.supported_languages
    }

    fn is_ready(&self) -> bool {
        !self.available_indexers.is_empty()
    }

    async fn enrich(
        &self,
        nodes: &[Node],
        index: &GraphIndex,
        repo_root: &Path,
    ) -> Result<EnrichmentResult> {
        if self.available_indexers.is_empty() {
            return Ok(EnrichmentResult::default());
        }

        // Determine which languages are present in the graph
        let graph_languages: HashSet<&str> = nodes
            .iter()
            .map(|n| n.language.as_str())
            .collect();

        let mut combined = EnrichmentResult::default();

        for config in &self.available_indexers {
            // Check if any of this indexer's languages are in the graph
            let relevant = config
                .languages
                .iter()
                .any(|lang| graph_languages.contains(lang));
            if !relevant {
                continue;
            }

            match self.run_indexer(config, repo_root).await {
                Ok(scip_index) => {
                    // Build the set of existing edge stable_ids for cross-source dedup.
                    // This includes edges from tree-sitter (already in the graph)
                    // plus any edges already added by earlier SCIP indexers in this run.
                    let existing_ids: HashSet<String> = index
                        .all_edges()
                        .iter()
                        .map(|(from, to, kind)| format!("{}->{}->{}", from, kind, to))
                        .chain(combined.added_edges.iter().map(|e| e.stable_id()))
                        .collect();

                    let enrichment = self.extract_edges(
                        &scip_index, nodes, index, repo_root, &existing_ids,
                    );
                    combined.added_edges.extend(enrichment.added_edges);
                    combined.new_nodes.extend(enrichment.new_nodes);
                    combined.updated_nodes.extend(enrichment.updated_nodes);
                }
                Err(e) => {
                    tracing::warn!(
                        "SCIP indexer '{}' failed: {}",
                        config.name,
                        e
                    );
                    // Continue with other indexers
                }
            }
        }

        Ok(combined)
    }

    fn name(&self) -> &str {
        "scip"
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Check if a binary is available on PATH by attempting to run it.
///
/// Tries the binary with `--version` (or bare invocation) and checks for a
/// successful spawn. This is portable across macOS, Linux, and Windows,
/// unlike shelling out to `which`.
fn is_binary_on_path(binary: &str) -> bool {
    // Try to spawn the binary with --help. We don't care about the exit code
    // (some tools return non-zero for --help), only whether the spawn succeeds.
    // If the binary doesn't exist, spawn() returns Err (NotFound).
    std::process::Command::new(binary)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            let _ = child.kill();
            let _ = child.wait();
            true
        })
        .unwrap_or(false)
}

/// Read the current git HEAD OID for cache staleness checks.
fn git_head_oid(repo_root: &Path) -> Option<String> {
    let repo = git2::Repository::open(repo_root).ok()?;
    let head = repo.head().ok()?;
    head.target().map(|oid| oid.to_string())
}

/// Get the start line (0-indexed) from a SCIP occurrence's range field.
///
/// SCIP range encoding:
/// - 3 elements: [line, start_char, end_char] (single-line)
/// - 4 elements: [start_line, start_char, end_line, end_char] (multi-line)
fn occurrence_start_line(occ: &scip::types::Occurrence) -> usize {
    if occ.range.is_empty() {
        return 0;
    }
    // SCIP lines are 0-indexed, our nodes use 1-indexed
    occ.range[0] as usize + 1
}

/// Find the node at a specific file + line (exact match on any line in range).
fn find_node_at(
    file_line_index: &HashMap<(PathBuf, usize), Vec<&Node>>,
    file: &PathBuf,
    line: usize,
) -> Option<NodeId> {
    file_line_index
        .get(&(file.clone(), line))
        .and_then(|nodes| {
            // Prefer the narrowest node (smallest span)
            nodes
                .iter()
                .filter(|n| matches!(n.id.kind, NodeKind::Function | NodeKind::Struct | NodeKind::Trait | NodeKind::Enum | NodeKind::Const))
                .min_by_key(|n| n.line_end - n.line_start)
                .map(|n| n.id.clone())
        })
}

/// Find the enclosing function/struct/impl at a given file + line.
/// Searches for the narrowest enclosing node.
fn find_enclosing_node(
    file_line_index: &HashMap<(PathBuf, usize), Vec<&Node>>,
    file: &PathBuf,
    line: usize,
) -> Option<NodeId> {
    file_line_index
        .get(&(file.clone(), line))
        .and_then(|nodes| {
            nodes
                .iter()
                .filter(|n| matches!(n.id.kind, NodeKind::Function | NodeKind::Impl | NodeKind::Struct))
                .min_by_key(|n| n.line_end - n.line_start)
                .map(|n| n.id.clone())
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_node(file: &str, name: &str, kind: NodeKind, line_start: usize, line_end: usize) -> Node {
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
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_edge(from_name: &str, from_file: &str, to_name: &str, to_file: &str, kind: EdgeKind) -> Edge {
        Edge {
            from: NodeId {
                root: String::new(),
                file: PathBuf::from(from_file),
                name: from_name.to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: String::new(),
                file: PathBuf::from(to_file),
                name: to_name.to_string(),
                kind: NodeKind::Function,
            },
            kind,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
        }
    }

    #[test]
    fn test_occurrence_start_line_single_line() {
        let mut occ = scip::types::Occurrence::new();
        occ.range = vec![5, 0, 10]; // line 5, char 0-10
        assert_eq!(occurrence_start_line(&occ), 6); // 0-indexed -> 1-indexed
    }

    #[test]
    fn test_occurrence_start_line_multi_line() {
        let mut occ = scip::types::Occurrence::new();
        occ.range = vec![10, 0, 15, 5]; // lines 10-15
        assert_eq!(occurrence_start_line(&occ), 11); // 0-indexed -> 1-indexed
    }

    #[test]
    fn test_occurrence_start_line_empty() {
        let occ = scip::types::Occurrence::new();
        assert_eq!(occurrence_start_line(&occ), 0);
    }

    #[test]
    fn test_find_node_at() {
        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
            make_node("src/main.rs", "helper", NodeKind::Function, 12, 20),
        ];
        let mut index: HashMap<(PathBuf, usize), Vec<&Node>> = HashMap::new();
        for node in &nodes {
            for line in node.line_start..=node.line_end {
                index
                    .entry((node.id.file.clone(), line))
                    .or_default()
                    .push(node);
            }
        }

        let result = find_node_at(&index, &PathBuf::from("src/main.rs"), 5);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "main");

        let result = find_node_at(&index, &PathBuf::from("src/main.rs"), 15);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "helper");

        let result = find_node_at(&index, &PathBuf::from("src/main.rs"), 25);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_enclosing_node_prefers_narrowest() {
        // Outer function spans lines 1-20, inner function spans 5-10
        let nodes = vec![
            make_node("src/lib.rs", "outer", NodeKind::Function, 1, 20),
            make_node("src/lib.rs", "inner", NodeKind::Function, 5, 10),
        ];
        let mut index: HashMap<(PathBuf, usize), Vec<&Node>> = HashMap::new();
        for node in &nodes {
            for line in node.line_start..=node.line_end {
                index
                    .entry((node.id.file.clone(), line))
                    .or_default()
                    .push(node);
            }
        }

        // Line 7 is in both, should return inner (narrower)
        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "inner");

        // Line 15 is only in outer
        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 15);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "outer");
    }

    #[test]
    fn test_scip_enricher_no_indexers() {
        // When no indexers are available, enricher should still construct
        let enricher = ScipEnricher {
            supported_languages: vec![],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };
        assert!(!enricher.is_ready());
        assert_eq!(enricher.name(), "scip");
    }

    #[test]
    fn test_extract_edges_empty_index() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };
        let index = scip::types::Index::new();
        let nodes = vec![make_node("src/main.rs", "main", NodeKind::Function, 1, 10)];
        let graph_index = GraphIndex::new();

        let result = enricher.extract_edges(&index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new());
        assert!(result.added_edges.is_empty());
        assert!(result.new_nodes.is_empty());
    }

    #[test]
    fn test_extract_edges_with_definition_and_reference() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
            make_node("src/lib.rs", "helper", NodeKind::Function, 1, 5),
        ];

        // Build a SCIP index with a definition and reference
        let mut index = scip::types::Index::new();

        // Document 1: lib.rs with helper definition
        let mut doc1 = scip::types::Document::new();
        doc1.relative_path = "src/lib.rs".to_string();
        let mut def_occ = scip::types::Occurrence::new();
        def_occ.range = vec![0, 0, 10]; // line 0 (0-indexed) = line 1 (1-indexed)
        def_occ.symbol = "rust-analyzer cargo test . helper.".to_string();
        def_occ.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc1.occurrences.push(def_occ);
        index.documents.push(doc1);

        // Document 2: main.rs with reference to helper
        let mut doc2 = scip::types::Document::new();
        doc2.relative_path = "src/main.rs".to_string();
        let mut ref_occ = scip::types::Occurrence::new();
        ref_occ.range = vec![4, 0, 10]; // line 4 (0-indexed) = line 5 (1-indexed), inside main()
        ref_occ.symbol = "rust-analyzer cargo test . helper.".to_string();
        ref_occ.symbol_roles = 0; // reference, not definition
        doc2.occurrences.push(ref_occ);
        index.documents.push(doc2);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(&index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new());

        assert_eq!(result.added_edges.len(), 1, "Should have one CALLS edge");
        let edge = &result.added_edges[0];
        assert_eq!(edge.from.name, "main");
        assert_eq!(edge.to.name, "helper");
        assert_eq!(edge.kind, EdgeKind::Calls);
        assert_eq!(edge.source, ExtractionSource::Scip);
        assert_eq!(edge.confidence, Confidence::Confirmed);
    }

    #[test]
    fn test_extract_edges_implements_relationship() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/lib.rs", "MyTrait", NodeKind::Trait, 1, 5),
            make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 7, 15),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/lib.rs".to_string();

        // Definition of trait
        let mut trait_def = scip::types::Occurrence::new();
        trait_def.range = vec![0, 0, 10]; // line 1
        trait_def.symbol = "rust-analyzer cargo test . MyTrait#".to_string();
        trait_def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(trait_def);

        // Definition of struct
        let mut struct_def = scip::types::Occurrence::new();
        struct_def.range = vec![6, 0, 10]; // line 7
        struct_def.symbol = "rust-analyzer cargo test . MyStruct#".to_string();
        struct_def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(struct_def);

        // SymbolInformation with implements relationship
        let mut sym_info = scip::types::SymbolInformation::new();
        sym_info.symbol = "rust-analyzer cargo test . MyStruct#".to_string();
        let mut rel = scip::types::Relationship::new();
        rel.symbol = "rust-analyzer cargo test . MyTrait#".to_string();
        rel.is_implementation = true;
        sym_info.relationships.push(rel);
        doc.symbols.push(sym_info);

        index.documents.push(doc);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(&index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new());

        assert_eq!(result.added_edges.len(), 1, "Should have one IMPLEMENTS edge");
        let edge = &result.added_edges[0];
        assert_eq!(edge.from.name, "MyStruct");
        assert_eq!(edge.to.name, "MyTrait");
        assert_eq!(edge.kind, EdgeKind::Implements);
        assert_eq!(edge.source, ExtractionSource::Scip);
        assert_eq!(edge.confidence, Confidence::Confirmed);
    }

    #[test]
    fn test_cross_source_dedup_skips_existing_edges() {
        // If tree-sitter already found main->helper CALLS, SCIP should skip it
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
            make_node("src/lib.rs", "helper", NodeKind::Function, 1, 5),
        ];

        // Build a SCIP index that would produce main->helper CALLS
        let mut index = scip::types::Index::new();
        let mut doc1 = scip::types::Document::new();
        doc1.relative_path = "src/lib.rs".to_string();
        let mut def_occ = scip::types::Occurrence::new();
        def_occ.range = vec![0, 0, 10];
        def_occ.symbol = "rust-analyzer cargo test . helper.".to_string();
        def_occ.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc1.occurrences.push(def_occ);
        index.documents.push(doc1);

        let mut doc2 = scip::types::Document::new();
        doc2.relative_path = "src/main.rs".to_string();
        let mut ref_occ = scip::types::Occurrence::new();
        ref_occ.range = vec![4, 0, 10];
        ref_occ.symbol = "rust-analyzer cargo test . helper.".to_string();
        ref_occ.symbol_roles = 0;
        doc2.occurrences.push(ref_occ);
        index.documents.push(doc2);

        // Simulate that tree-sitter already found this edge
        let existing_edge = make_edge("main", "src/main.rs", "helper", "src/lib.rs", EdgeKind::Calls);
        let existing_ids: HashSet<String> = [existing_edge.stable_id()].into_iter().collect();

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(&index, &nodes, &graph_index, Path::new("/tmp"), &existing_ids);

        assert_eq!(
            result.added_edges.len(),
            0,
            "SCIP should skip edges already found by tree-sitter"
        );
    }

    #[test]
    fn test_is_binary_on_path_nonexistent() {
        // A binary that definitely doesn't exist
        assert!(!is_binary_on_path("definitely_not_a_real_binary_xyz_12345"));
    }

    #[tokio::test]
    async fn test_enrich_empty_when_no_indexers() {
        let enricher = ScipEnricher {
            supported_languages: vec![],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };
        let nodes = vec![make_node("src/main.rs", "main", NodeKind::Function, 1, 10)];
        let graph_index = GraphIndex::new();

        let result = enricher.enrich(&nodes, &graph_index, Path::new("/tmp")).await.unwrap();
        assert!(result.added_edges.is_empty());
        assert!(result.new_nodes.is_empty());
        assert!(result.updated_nodes.is_empty());
    }

    #[test]
    fn test_languages_reflects_available_indexers() {
        // With no indexers, languages() should return empty
        let enricher = ScipEnricher {
            supported_languages: vec![],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };
        assert!(
            enricher.languages().is_empty(),
            "languages() should be empty when no indexers are available"
        );

        // With rust indexer only, should only return "rust"
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![KNOWN_INDEXERS[0].clone()], // rust-analyzer
            cache: Mutex::new(HashMap::new()),
        };
        let langs = enricher.languages();
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0], "rust");
    }
}
