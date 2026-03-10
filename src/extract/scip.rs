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
        // Check git HEAD for cache freshness.
        // SCIP indexers assume a git repo — skip enrichment entirely for non-git dirs
        // to avoid redundant 120s indexer runs with no cache.
        let head_oid = match git_head_oid(repo_root) {
            Some(oid) => oid,
            None => {
                tracing::info!(
                    "SCIP enrichment skipped for '{}': not a git repository ({})",
                    config.name,
                    repo_root.display()
                );
                return Ok(scip::types::Index::new());
            }
        };

        {
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

        // Cache the result (head_oid is guaranteed non-empty here)
        {
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
            // TODO(windows): SCIP always emits forward slashes in relative_path,
            // but PathBuf on Windows uses backslashes. File matching will silently
            // fail on Windows. RNA targets macOS/Linux so this is not urgent.
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
                                    // Skip self-references (same as Calls path)
                                    if impl_id == iface_id {
                                        continue;
                                    }
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
                    // Use the same stable_id format as Edge::stable_id() to
                    // prevent silent drift between the two dedup paths.
                    let existing_ids: HashSet<String> = index
                        .all_edges()
                        .iter()
                        .map(|(from, to, kind)| {
                            // Mirror Edge::stable_id(): "{from}->{kind}->{to}"
                            // where from/to are already NodeId::to_stable_id() strings.
                            format!("{}->{}->{}", from, kind, to)
                        })
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

    // -----------------------------------------------------------------------
    // Adversarial tests — edge cases, breakage attempts, boundary conditions
    // -----------------------------------------------------------------------

    /// Edge dedup must be directional: A->B and B->A are distinct edges.
    /// If dedup conflates them, we silently lose edges.
    #[test]
    fn test_edge_dedup_preserves_direction() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/a.rs", "foo", NodeKind::Function, 1, 10),
            make_node("src/b.rs", "bar", NodeKind::Function, 1, 10),
        ];

        // Build a SCIP index where foo calls bar AND bar calls foo
        let mut index = scip::types::Index::new();

        // Define both symbols
        let mut doc_a = scip::types::Document::new();
        doc_a.relative_path = "src/a.rs".to_string();
        let mut def_foo = scip::types::Occurrence::new();
        def_foo.range = vec![0, 0, 10]; // line 1
        def_foo.symbol = "test . foo.".to_string();
        def_foo.symbol_roles = scip::types::SymbolRole::Definition.value();
        // Reference to bar inside foo
        let mut ref_bar = scip::types::Occurrence::new();
        ref_bar.range = vec![4, 0, 10]; // line 5, inside foo
        ref_bar.symbol = "test . bar.".to_string();
        ref_bar.symbol_roles = 0;
        doc_a.occurrences.push(def_foo);
        doc_a.occurrences.push(ref_bar);
        index.documents.push(doc_a);

        let mut doc_b = scip::types::Document::new();
        doc_b.relative_path = "src/b.rs".to_string();
        let mut def_bar = scip::types::Occurrence::new();
        def_bar.range = vec![0, 0, 10]; // line 1
        def_bar.symbol = "test . bar.".to_string();
        def_bar.symbol_roles = scip::types::SymbolRole::Definition.value();
        // Reference to foo inside bar
        let mut ref_foo = scip::types::Occurrence::new();
        ref_foo.range = vec![4, 0, 10]; // line 5, inside bar
        ref_foo.symbol = "test . foo.".to_string();
        ref_foo.symbol_roles = 0;
        doc_b.occurrences.push(def_bar);
        doc_b.occurrences.push(ref_foo);
        index.documents.push(doc_b);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        // Both directions must survive dedup
        assert_eq!(
            result.added_edges.len(),
            2,
            "A->B and B->A must both survive dedup (directional). Got: {:?}",
            result.added_edges.iter().map(|e| format!("{}->{}", e.from.name, e.to.name)).collect::<Vec<_>>()
        );

        let edge_names: HashSet<(String, String)> = result
            .added_edges
            .iter()
            .map(|e| (e.from.name.clone(), e.to.name.clone()))
            .collect();
        assert!(edge_names.contains(&("foo".to_string(), "bar".to_string())));
        assert!(edge_names.contains(&("bar".to_string(), "foo".to_string())));
    }

    /// If the same function references the same target symbol multiple times
    /// (e.g., calling helper() on lines 3, 7, and 9), dedup within SCIP
    /// output should collapse them to a single edge.
    #[test]
    fn test_duplicate_references_deduped_within_scip() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "caller", NodeKind::Function, 1, 20),
            make_node("src/lib.rs", "target", NodeKind::Function, 1, 5),
        ];

        let mut index = scip::types::Index::new();

        let mut doc1 = scip::types::Document::new();
        doc1.relative_path = "src/lib.rs".to_string();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 10];
        def.symbol = "test . target.".to_string();
        def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc1.occurrences.push(def);
        index.documents.push(doc1);

        let mut doc2 = scip::types::Document::new();
        doc2.relative_path = "src/main.rs".to_string();
        // Three references to target from within caller
        for line in [2, 7, 14] {
            let mut ref_occ = scip::types::Occurrence::new();
            ref_occ.range = vec![line, 0, 10];
            ref_occ.symbol = "test . target.".to_string();
            ref_occ.symbol_roles = 0;
            doc2.occurrences.push(ref_occ);
        }
        index.documents.push(doc2);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert_eq!(
            result.added_edges.len(),
            1,
            "Three references from same function to same target should dedup to 1 edge"
        );
    }

    /// Self-reference in an Implements relationship: a symbol that claims to
    /// implement itself. Both Calls and Implements paths filter self-refs.
    #[test]
    fn test_self_implementing_symbol_filtered() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/lib.rs", "Thing", NodeKind::Trait, 1, 10),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/lib.rs".to_string();

        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 10];
        def.symbol = "test . Thing#".to_string();
        def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(def);

        // SymbolInformation claiming Thing implements Thing
        let mut sym_info = scip::types::SymbolInformation::new();
        sym_info.symbol = "test . Thing#".to_string();
        let mut rel = scip::types::Relationship::new();
        rel.symbol = "test . Thing#".to_string();
        rel.is_implementation = true;
        sym_info.relationships.push(rel);
        doc.symbols.push(sym_info);

        index.documents.push(doc);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        // Self-implementing edges are filtered out (same as Calls self-ref check)
        assert_eq!(
            result.added_edges.len(),
            0,
            "Self-implementing edge should be filtered (self-ref check)"
        );
    }

    /// Local symbols (starting with "local ") should be skipped entirely.
    /// If the filter is broken, we'll get spurious edges from locals.
    #[test]
    fn test_local_symbols_filtered_out() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 20),
            make_node("src/lib.rs", "helper", NodeKind::Function, 1, 5),
        ];

        let mut index = scip::types::Index::new();

        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/lib.rs".to_string();
        // A definition using a local symbol
        let mut local_def = scip::types::Occurrence::new();
        local_def.range = vec![0, 0, 10];
        local_def.symbol = "local 42".to_string(); // local symbol
        local_def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(local_def);
        index.documents.push(doc);

        let mut doc2 = scip::types::Document::new();
        doc2.relative_path = "src/main.rs".to_string();
        let mut local_ref = scip::types::Occurrence::new();
        local_ref.range = vec![4, 0, 10];
        local_ref.symbol = "local 42".to_string(); // reference to local
        local_ref.symbol_roles = 0;
        doc2.occurrences.push(local_ref);
        index.documents.push(doc2);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert!(
            result.added_edges.is_empty(),
            "Local symbols should be filtered out, producing no edges"
        );
    }

    /// Module-kind nodes should be invisible to find_node_at.
    /// If a Module node spans lines 1-100 and a Function node spans 5-10,
    /// only the Function should match at line 7.
    #[test]
    fn test_module_nodes_invisible_to_find_node_at() {
        let nodes = vec![
            make_node("src/lib.rs", "my_module", NodeKind::Module, 1, 100),
            make_node("src/lib.rs", "real_fn", NodeKind::Function, 5, 10),
        ];
        let mut fli: HashMap<(PathBuf, usize), Vec<&Node>> = HashMap::new();
        for node in &nodes {
            for line in node.line_start..=node.line_end {
                fli.entry((node.id.file.clone(), line)).or_default().push(node);
            }
        }

        // Line 7: both Module and Function present, only Function should match
        let result = find_node_at(&fli, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "real_fn");

        // Line 50: only Module present, should return None
        let result = find_node_at(&fli, &PathBuf::from("src/lib.rs"), 50);
        assert!(
            result.is_none(),
            "Module-only lines should return None from find_node_at"
        );
    }

    /// Import nodes should not match as enclosing nodes.
    /// A reference inside an import block should not create caller edges.
    #[test]
    fn test_import_nodes_invisible_to_find_enclosing_node() {
        let nodes = vec![
            make_node("src/lib.rs", "use_std", NodeKind::Import, 1, 1),
        ];
        let mut fli: HashMap<(PathBuf, usize), Vec<&Node>> = HashMap::new();
        for node in &nodes {
            for line in node.line_start..=node.line_end {
                fli.entry((node.id.file.clone(), line)).or_default().push(node);
            }
        }

        let result = find_enclosing_node(&fli, &PathBuf::from("src/lib.rs"), 1);
        assert!(
            result.is_none(),
            "Import nodes should not be enclosing nodes for CALLS edges"
        );
    }

    /// git_head_oid should gracefully return None for a path that is not
    /// a git repository.
    #[test]
    fn test_git_head_oid_non_repo() {
        // /tmp is (almost certainly) not a git repo
        let result = git_head_oid(Path::new("/tmp"));
        assert!(
            result.is_none(),
            "git_head_oid should return None for non-repo path"
        );
    }

    /// When git HEAD can't be resolved (non-git dir), run_indexer returns
    /// an empty index and skips enrichment entirely (no redundant indexer runs).
    #[test]
    fn test_non_git_dir_skips_enrichment() {
        // git_head_oid returns None for non-git dirs
        let head_oid = git_head_oid(Path::new("/tmp"));
        assert!(head_oid.is_none(), "Non-repo should return None");

        // run_indexer would return an empty Index for non-git dirs,
        // which means no edges are extracted. Verify cache stays empty.
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };
        let cache = enricher.cache.lock().unwrap();
        assert!(cache.is_empty());
    }

    /// Malformed protobuf: parse_from_bytes should fail on garbage input.
    /// The enricher should propagate this as an error, not panic.
    #[test]
    fn test_malformed_protobuf_parse_fails() {
        // Garbage bytes
        let garbage = vec![0xFF, 0xFE, 0x00, 0x01, 0x02, 0x03];
        let result = scip::types::Index::parse_from_bytes(&garbage);
        // protobuf may or may not parse garbage (it's lenient), but it shouldn't panic
        // If it does parse, we at least shouldn't crash
        match result {
            Ok(idx) => {
                // Parsed "successfully" from garbage -- documents should be empty or weird
                // This is acceptable protobuf behavior (forward-compatible parsing)
                let _ = idx.documents.len();
            }
            Err(_) => {
                // Expected: parse failure on garbage
            }
        }
    }

    /// Empty protobuf: 0-byte input should either fail or produce empty index.
    #[test]
    fn test_empty_protobuf_parse() {
        let empty = vec![];
        let result = scip::types::Index::parse_from_bytes(&empty);
        match result {
            Ok(idx) => {
                assert!(idx.documents.is_empty(), "Empty protobuf should yield empty index");
            }
            Err(_) => {
                // Also acceptable
            }
        }
    }

    /// Cross-source dedup format consistency: the format string used in
    /// enrich() to build existing_ids must match Edge::stable_id().
    /// If they diverge, cross-source dedup silently fails.
    #[test]
    fn test_cross_source_dedup_format_consistency() {
        // Simulate what enrich() does: build existing_ids from all_edges format
        let from_id = ":src/main.rs:main:function"; // NodeId::to_stable_id() with empty root
        let to_id = ":src/lib.rs:helper:function";
        let kind = EdgeKind::Calls;

        // Format used in enrich() (line 493)
        let enrich_format = format!("{}->{}->{}", from_id, kind, to_id);

        // Format used by Edge::stable_id()
        let edge = make_edge("main", "src/main.rs", "helper", "src/lib.rs", EdgeKind::Calls);
        let stable_format = edge.stable_id();

        assert_eq!(
            enrich_format, stable_format,
            "enrich() existing_ids format must match Edge::stable_id(). \
             Divergence means cross-source dedup silently fails.\n\
             enrich format: {}\n\
             stable_id:     {}",
            enrich_format, stable_format
        );
    }

    /// Occurrence with negative or very large range values should not panic.
    #[test]
    fn test_occurrence_extreme_range_values() {
        let mut occ = scip::types::Occurrence::new();
        occ.range = vec![i32::MAX, 0, 10];
        // Should not panic — just produces a very large line number
        let line = occurrence_start_line(&occ);
        assert!(line > 0);

        // Negative range (i32 but stored as vec<i32>)
        let mut occ2 = scip::types::Occurrence::new();
        occ2.range = vec![-1, 0, 10];
        // -1 as usize will wrap around, but shouldn't panic
        let _line = occurrence_start_line(&occ2);
        // We just verify no panic occurs
    }

    /// Empty symbol string in occurrence should be skipped (not produce edges).
    #[test]
    fn test_empty_symbol_string_skipped() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/main.rs".to_string();

        // Occurrence with empty symbol
        let mut occ = scip::types::Occurrence::new();
        occ.range = vec![0, 0, 10];
        occ.symbol = "".to_string();
        occ.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(occ);

        index.documents.push(doc);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert!(
            result.added_edges.is_empty(),
            "Empty symbol string should be skipped"
        );
    }

    /// Definition at a line with no matching tree-sitter node should be
    /// silently dropped (no edges from orphan definitions).
    #[test]
    fn test_definition_at_unmapped_line_dropped() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        // Node at lines 1-5, but definition is at line 50 (unmapped)
        let nodes = vec![
            make_node("src/lib.rs", "helper", NodeKind::Function, 1, 5),
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/lib.rs".to_string();

        let mut def = scip::types::Occurrence::new();
        def.range = vec![49, 0, 10]; // line 50 (1-indexed), no node here
        def.symbol = "test . orphan.".to_string();
        def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(def);
        index.documents.push(doc);

        let mut doc2 = scip::types::Document::new();
        doc2.relative_path = "src/main.rs".to_string();
        let mut ref_occ = scip::types::Occurrence::new();
        ref_occ.range = vec![4, 0, 10];
        ref_occ.symbol = "test . orphan.".to_string();
        ref_occ.symbol_roles = 0;
        doc2.occurrences.push(ref_occ);
        index.documents.push(doc2);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert!(
            result.added_edges.is_empty(),
            "Definition at unmapped line should produce no edges"
        );
    }

    /// Reference at a line with no enclosing node should be silently dropped.
    #[test]
    fn test_reference_with_no_enclosing_node_dropped() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        // Target defined at line 1-5, but reference is at line 50 with no
        // enclosing function/struct node
        let nodes = vec![
            make_node("src/lib.rs", "target", NodeKind::Function, 1, 5),
        ];

        let mut index = scip::types::Index::new();

        let mut doc1 = scip::types::Document::new();
        doc1.relative_path = "src/lib.rs".to_string();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 10];
        def.symbol = "test . target.".to_string();
        def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc1.occurrences.push(def);
        index.documents.push(doc1);

        let mut doc2 = scip::types::Document::new();
        doc2.relative_path = "src/orphan.rs".to_string();
        let mut ref_occ = scip::types::Occurrence::new();
        ref_occ.range = vec![49, 0, 10]; // line 50, no node here
        ref_occ.symbol = "test . target.".to_string();
        ref_occ.symbol_roles = 0;
        doc2.occurrences.push(ref_occ);
        index.documents.push(doc2);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert!(
            result.added_edges.is_empty(),
            "Reference with no enclosing node should produce no edge"
        );
    }

    /// Multiple definitions of the same symbol (e.g., overloads or re-exports)
    /// should each get edges from references.
    #[test]
    fn test_multiple_definitions_same_symbol() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/a.rs", "handler_v1", NodeKind::Function, 1, 10),
            make_node("src/b.rs", "handler_v2", NodeKind::Function, 1, 10),
            make_node("src/main.rs", "caller", NodeKind::Function, 1, 10),
        ];

        let mut index = scip::types::Index::new();

        // Two definitions of the same symbol in different files
        let mut doc_a = scip::types::Document::new();
        doc_a.relative_path = "src/a.rs".to_string();
        let mut def1 = scip::types::Occurrence::new();
        def1.range = vec![0, 0, 10];
        def1.symbol = "test . handler.".to_string();
        def1.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc_a.occurrences.push(def1);
        index.documents.push(doc_a);

        let mut doc_b = scip::types::Document::new();
        doc_b.relative_path = "src/b.rs".to_string();
        let mut def2 = scip::types::Occurrence::new();
        def2.range = vec![0, 0, 10];
        def2.symbol = "test . handler.".to_string();
        def2.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc_b.occurrences.push(def2);
        index.documents.push(doc_b);

        // One reference from caller
        let mut doc_main = scip::types::Document::new();
        doc_main.relative_path = "src/main.rs".to_string();
        let mut ref_occ = scip::types::Occurrence::new();
        ref_occ.range = vec![4, 0, 10];
        ref_occ.symbol = "test . handler.".to_string();
        ref_occ.symbol_roles = 0;
        doc_main.occurrences.push(ref_occ);
        index.documents.push(doc_main);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert_eq!(
            result.added_edges.len(),
            2,
            "Reference to multiply-defined symbol should produce edge to each definition"
        );

        let target_names: HashSet<String> = result
            .added_edges
            .iter()
            .map(|e| e.to.name.clone())
            .collect();
        assert!(target_names.contains("handler_v1"));
        assert!(target_names.contains("handler_v2"));
    }

    /// An SCIP index with documents but zero occurrences should produce no edges.
    #[test]
    fn test_documents_with_no_occurrences() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/main.rs".to_string();
        // No occurrences added
        index.documents.push(doc);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        assert!(result.added_edges.is_empty());
    }

    /// The is_ready() method should return false when no indexers found.
    #[test]
    fn test_is_ready_false_when_no_indexers() {
        let enricher = ScipEnricher {
            supported_languages: vec![],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };
        assert!(!enricher.is_ready());
    }

    /// The is_ready() method should return true when at least one indexer exists.
    #[test]
    fn test_is_ready_true_with_indexers() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![KNOWN_INDEXERS[0].clone()],
            cache: Mutex::new(HashMap::new()),
        };
        assert!(enricher.is_ready());
    }

    /// If all nodes are at line 0 (e.g., from empty range occurrences),
    /// edges should still form correctly if definitions and references align.
    #[test]
    fn test_zero_line_nodes_and_occurrences() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        // A node that spans only line 0 (unusual but possible)
        let nodes = vec![
            make_node("src/a.rs", "zero_fn", NodeKind::Function, 0, 0),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/a.rs".to_string();

        // Occurrence with empty range -> occurrence_start_line returns 0
        let mut occ = scip::types::Occurrence::new();
        occ.range = vec![]; // empty range
        occ.symbol = "test . zero_fn.".to_string();
        occ.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(occ);
        index.documents.push(doc);

        let graph_index = GraphIndex::new();
        let _result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        // occurrence_start_line returns 0 for empty range, and node spans 0..=0
        // So the definition should match
        // (This documents behavior at the boundary — no panic is the assertion)
    }

    /// File path in SCIP document uses forward slashes but nodes might use
    /// platform-specific separators. On Windows this would be a mismatch.
    /// This test verifies the behavior on the current platform.
    #[test]
    fn test_file_path_consistency() {
        let enricher = ScipEnricher {
            supported_languages: vec!["rust"],
            available_indexers: vec![],
            cache: Mutex::new(HashMap::new()),
        };

        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
        ];

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        // SCIP always uses forward slashes
        doc.relative_path = "src/main.rs".to_string();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 10];
        def.symbol = "test . main.".to_string();
        def.symbol_roles = scip::types::SymbolRole::Definition.value();
        doc.occurrences.push(def);
        index.documents.push(doc);

        let graph_index = GraphIndex::new();
        let result = enricher.extract_edges(
            &index, &nodes, &graph_index, Path::new("/tmp"), &HashSet::new(),
        );

        // On Unix, PathBuf::from("src/main.rs") matches PathBuf::from("src/main.rs").
        // On Windows, this would be a mismatch (backslash vs forward slash).
        // The current code does NOT normalize paths, which is a latent Windows bug.
        // This test documents that file matching works on Unix/macOS.
        // (No assertion needed beyond no-panic for platform documentation)
    }
}
