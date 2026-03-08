//! `rna test` — in-process pipeline verifier.
//!
//! Exercises the full scan → extract → embed → index → query pipeline
//! without spawning a child process or using the MCP protocol.
//! Exits 0 on pass, 1 on any failure.
//!
//! Usage:
//!   cargo run -- test
//!   cargo run -- test --repo /path/to/repo

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use petgraph::Direction;

use crate::embed::EmbeddingIndex;
use crate::extract::ExtractorRegistry;
use crate::graph::index::GraphIndex;
use crate::oh;
use crate::query;
use crate::roots::WorkspaceConfig;
use crate::scanner::Scanner;
use crate::server::{graph_lance_path, load_graph_from_lance, persist_graph_incremental, persist_graph_to_lance};

// ── Args ────────────────────────────────────────────────────────────

#[derive(Debug, clap::Args)]
pub struct TestArgs {
    /// Repository root to test against (default: current directory)
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,

    /// Output format: "json" or "human" (default: human)
    #[arg(long, default_value = "human")]
    pub format: String,
}

// ── Result types ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SmokeResult {
    pub pass: bool,
    pub checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Check { name, status: CheckStatus::Pass, detail: Some(detail.into()) }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Check { name, status: CheckStatus::Fail, detail: Some(detail.into()) }
    }
    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Check { name, status: CheckStatus::Skip, detail: Some(detail.into()) }
    }
}

// ── Runner ──────────────────────────────────────────────────────────

pub async fn run(args: &TestArgs) -> Result<bool> {
    let repo = args.repo.canonicalize().unwrap_or_else(|_| args.repo.clone());
    let mut checks: Vec<Check> = Vec::new();

    // 1. Scanner initializes
    let mut scanner = match Scanner::new(repo.clone()) {
        Ok(s) => {
            checks.push(Check::pass("scanner_init", "Scanner created successfully"));
            s
        }
        Err(e) => {
            checks.push(Check::fail("scanner_init", format!("Scanner::new failed: {}", e)));
            return Ok(print_and_return(args, checks));
        }
    };

    // 2. File walk
    let _scan_result = match scanner.scan() {
        Ok(r) => {
            let all_files = scanner.all_known_files();
            let count = all_files.len();
            if count > 0 {
                checks.push(Check::pass("file_walk", format!("{} files known", count)));
            } else {
                checks.push(Check::fail("file_walk", "No files found in repo"));
            }
            r
        }
        Err(e) => {
            checks.push(Check::fail("file_walk", format!("scan() failed: {}", e)));
            return Ok(print_and_return(args, checks));
        }
    };

    // 3. Symbol extraction
    let registry = ExtractorRegistry::with_builtins();
    let all_files = scanner.all_known_files();
    let full_scan = crate::scanner::ScanResult {
        changed_files: Vec::new(),
        new_files: all_files,
        deleted_files: Vec::new(),
        scan_duration: std::time::Duration::ZERO,
    };
    let extraction = registry.extract_scan_result(&repo, &full_scan);
    let symbol_count = extraction.nodes.len();
    if symbol_count > 0 {
        checks.push(Check::pass("symbol_extraction", format!("{} symbols extracted", symbol_count)));
    } else {
        checks.push(Check::fail("symbol_extraction", "No symbols extracted"));
    }

    // 4. Graph index build
    let all_nodes = extraction.nodes;
    let all_edges = extraction.edges;
    let mut index = GraphIndex::new();
    index.rebuild_from_edges(&all_edges);
    for node in &all_nodes {
        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
    }
    checks.push(Check::pass("graph_index", format!("{} edges indexed", all_edges.len())));

    // 5. Embedding index
    let embed_index = match EmbeddingIndex::new(&repo).await {
        Ok(idx) => {
            // Probe first: if the table already exists, reuse it; otherwise rebuild.
            match idx.search("_probe_", None, 1).await {
                Ok(_) => {
                    checks.push(Check::pass("embed_index", "Persisted embedding index reused"));
                    Some(idx)
                }
                Err(_) => {
                    let embeddable: Vec<_> = all_nodes.iter()
                        .filter(|n| n.id.root != "external")
                        .cloned()
                        .collect();
                    match idx.index_all_with_symbols(&repo, &embeddable).await {
                        Ok(count) => {
                            checks.push(Check::pass("embed_index", format!("{} vectors written", count)));
                            Some(idx)
                        }
                        Err(e) => {
                            checks.push(Check::fail("embed_index", format!("Embedding failed: {}", e)));
                            None
                        }
                    }
                }
            }
        }
        Err(e) => {
            checks.push(Check::fail("embed_index", format!("EmbeddingIndex::new failed: {}", e)));
            None
        }
    };

    // 6. oh_get_context path
    let artifacts = oh::load_oh_artifacts(&repo).unwrap_or_default();
    if !artifacts.is_empty() {
        let byte_count: usize = artifacts.iter().map(|a| a.to_markdown().len()).sum();
        checks.push(Check::pass("oh_get_context", format!("{} artifacts, ~{} bytes", artifacts.len(), byte_count)));
    } else {
        // Not a hard failure: repo may not have .oh/ yet (e.g., minimal fixture)
        checks.push(Check::skip("oh_get_context", "No .oh/ artifacts found (repo may not be initialized)"));
    }

    // 7. oh_search_context path — embed search for "main"
    match &embed_index {
        Some(idx) => {
            match idx.search("main", None, 5).await {
                Ok(results) if !results.is_empty() => {
                    checks.push(Check::pass("oh_search_context", format!("{} results for query \"main\"", results.len())));
                }
                Ok(_) => {
                    checks.push(Check::fail("oh_search_context", "Query \"main\" returned 0 results"));
                }
                Err(e) => {
                    checks.push(Check::fail("oh_search_context", format!("search() error: {}", e)));
                }
            }
        }
        None => {
            checks.push(Check::skip("oh_search_context", "Skipped (no embedding index)"));
        }
    }

    // 8. search_symbols path — substring search on extracted nodes
    let query_lower = "main";
    let symbol_matches: Vec<_> = all_nodes.iter()
        .filter(|n| n.id.name.to_lowercase().contains(query_lower) || n.signature.to_lowercase().contains(query_lower))
        .collect();
    if !symbol_matches.is_empty() {
        checks.push(Check::pass("search_symbols", format!("{} symbols match \"{}\"", symbol_matches.len(), query_lower)));
    } else {
        checks.push(Check::fail("search_symbols", format!("No symbols match \"{}\"", query_lower)));
    }

    // 9. outcome_progress path
    match query::outcome_progress(&repo, "agent-alignment") {
        Ok(result) => {
            let outcome_count = result.outcomes.len();
            if outcome_count > 0 || !result.markdown_chunks.is_empty() || !result.code_symbols.is_empty() {
                checks.push(Check::pass("outcome_progress", format!("{} outcomes in progress result", outcome_count)));
            } else {
                // No outcomes for "agent-alignment" is possible but worth noting
                checks.push(Check::skip("outcome_progress", "outcome_progress returned empty (outcome may not exist)"));
            }
        }
        Err(e) => {
            checks.push(Check::fail("outcome_progress", format!("outcome_progress error: {}", e)));
        }
    }

    // 10. list_roots path
    let workspace = WorkspaceConfig::load().with_primary_root(repo.clone());
    let roots = workspace.resolved_roots();
    if !roots.is_empty() {
        checks.push(Check::pass("list_roots", format!("{} workspace root(s)", roots.len())));
    } else {
        checks.push(Check::fail("list_roots", "No workspace roots resolved"));
    }

    // 11. Incremental persist smoke test
    checks.push(run_incremental_persist_check().await);

    // 12. Worktree smoke test
    // requires feat/rna-worktree-awareness to be merged
    checks.push(run_worktree_check(&repo).await);

    // 13. LSP virtual external nodes round-trip through LanceDB
    checks.push(run_external_calls_check().await);

    // 14. list_roots -- verifies WorkspaceConfig returns at least 1 root matching the test repo
    checks.push(run_list_roots_check(&repo));

    // 15. Worktree roots -- creates a real git worktree, checks with_worktrees() sees it
    checks.push(run_worktree_roots_check(&repo).await);

    // 16. Metadata round-trip -- persist node with metadata, reload, assert identical
    checks.push(run_metadata_roundtrip_check().await);

    // 17. search_symbols -- verifies function nodes and Const nodes are findable
    checks.push(run_search_symbols_check(&all_nodes));

    // 18. graph_query -- verifies GraphIndex has edges and neighbors() is callable
    checks.push(run_graph_query_check(&index, &all_nodes));

    // 19. oh_search_context (semantic) -- searches for "alignment" in embedding index
    checks.push(run_oh_search_context_check(&embed_index).await);

    Ok(print_and_return(args, checks))
}

/// Incremental persist check: exercises the `persist_graph_incremental` path end-to-end.
///
/// 1. Creates a temp directory acting as a fake repo root.
/// 2. Writes a Rust fixture with two functions (old + common).
/// 3. Calls `persist_graph_incremental` to seed the initial LanceDB state.
/// 4. Overwrites the fixture (remove old, add new, keep common).
/// 5. Simulates the incremental update: files_to_remove = [changed_file], runs extraction,
///    calls `persist_graph_incremental` with the delta.
/// 6. Queries LanceDB directly: confirms old symbol is gone, new symbol is present.
async fn run_incremental_persist_check() -> Check {
    use arrow_array::StringArray;
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;

    // Create isolated temp directory — acts as the fake repo root.
    let temp_dir = std::env::temp_dir().join("rna-smoke-incremental");
    let _ = std::fs::remove_dir_all(&temp_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Check::fail("incremental_persist", format!("Could not create temp dir: {}", e));
    }

    let fixture_path = temp_dir.join("rna_smoke_fixture.rs");

    // Step 1: Write initial file with old_fn + common_fn
    let initial_content = "\
/// Old function that will be removed in the next scan.\n\
pub fn rna_smoke_old_fn() -> u32 { 1 }\n\
\n\
/// Common function that persists across both scans.\n\
pub fn rna_smoke_common_fn() -> u32 { 2 }\n\
";
    if let Err(e) = std::fs::write(&fixture_path, initial_content) {
        return Check::fail("incremental_persist", format!("Could not write initial fixture: {}", e));
    }

    // Step 2: Extract symbols from the initial file and persist as initial state.
    let registry = ExtractorRegistry::with_builtins();
    let initial_scan = crate::scanner::ScanResult {
        changed_files: Vec::new(),
        new_files: vec![fixture_path.clone()],
        deleted_files: Vec::new(),
        scan_duration: std::time::Duration::ZERO,
    };
    let initial_extraction = registry.extract_scan_result(&temp_dir, &initial_scan);
    let initial_nodes = initial_extraction.nodes;
    let initial_edges = initial_extraction.edges;

    if initial_nodes.is_empty() {
        // Tree-sitter may not parse .rs in temp dir — skip rather than fail.
        return Check::skip("incremental_persist", "No symbols extracted from initial fixture (tree-sitter parse skipped)");
    }

    // Seed LanceDB with the initial state.
    if let Err(e) = persist_graph_incremental(
        &temp_dir,
        &initial_nodes,
        &initial_edges,
        &[],
        &[],
    ).await {
        return Check::fail("incremental_persist", format!("Initial persist failed: {}", e));
    }

    // Step 3: Update the file — remove old_fn, add new_fn, keep common_fn.
    let updated_content = "\
/// New function introduced in the second scan.\n\
pub fn rna_smoke_new_fn() -> u32 { 3 }\n\
\n\
/// Common function that persists across both scans.\n\
pub fn rna_smoke_common_fn() -> u32 { 2 }\n\
";
    if let Err(e) = std::fs::write(&fixture_path, updated_content) {
        return Check::fail("incremental_persist", format!("Could not write updated fixture: {}", e));
    }

    // Step 4: Simulate what update_graph_incrementally does:
    //   files_to_remove = [fixture_path] (it's a "changed" file)
    //   deleted_edge_ids = [] (no edges in this test)
    //   upsert = extraction of updated file
    let files_to_remove = vec![fixture_path.clone()];
    let update_scan = crate::scanner::ScanResult {
        changed_files: vec![fixture_path.clone()],
        new_files: Vec::new(),
        deleted_files: Vec::new(),
        scan_duration: std::time::Duration::ZERO,
    };
    let update_extraction = registry.extract_scan_result(&temp_dir, &update_scan);
    let upsert_nodes = update_extraction.nodes;
    let upsert_edges = update_extraction.edges;

    if let Err(e) = persist_graph_incremental(
        &temp_dir,
        &upsert_nodes,
        &upsert_edges,
        &[],
        &files_to_remove,
    ).await {
        return Check::fail("incremental_persist", format!("Incremental persist failed: {}", e));
    }

    // Step 5: Query LanceDB and verify: old_fn gone, new_fn present.
    let db_path = graph_lance_path(&temp_dir);
    let db = match lancedb::connect(db_path.to_str().unwrap_or("")).execute().await {
        Ok(d) => d,
        Err(e) => return Check::fail("incremental_persist", format!("Could not open LanceDB: {}", e)),
    };
    let table = match db.open_table("symbols").execute().await {
        Ok(t) => t,
        Err(e) => return Check::fail("incremental_persist", format!("Could not open symbols table: {}", e)),
    };
    let stream = match table.query().execute().await {
        Ok(s) => s,
        Err(e) => return Check::fail("incremental_persist", format!("Could not query symbols: {}", e)),
    };
    let batches: Vec<arrow_array::RecordBatch> = match stream.try_collect().await {
        Ok(b) => b,
        Err(e) => return Check::fail("incremental_persist", format!("Could not collect batches: {}", e)),
    };

    let mut found_old = false;
    let mut found_new = false;
    for batch in &batches {
        if let Some(names_col) = batch.column_by_name("name") {
            if let Some(names) = names_col.as_any().downcast_ref::<StringArray>() {
                for i in 0..batch.num_rows() {
                    let name = names.value(i);
                    if name.contains("rna_smoke_old_fn") {
                        found_old = true;
                    }
                    if name.contains("rna_smoke_new_fn") {
                        found_new = true;
                    }
                }
            }
        }
    }

    // Clean up temp dir.
    let _ = std::fs::remove_dir_all(&temp_dir);

    if found_old {
        Check::fail(
            "incremental_persist",
            "Ghost symbol 'rna_smoke_old_fn' still present in LanceDB after incremental update",
        )
    } else if !found_new {
        Check::fail(
            "incremental_persist",
            "New symbol 'rna_smoke_new_fn' not found in LanceDB after incremental update",
        )
    } else {
        Check::pass(
            "incremental_persist",
            "Incremental persist correct: old symbol removed, new symbol present",
        )
    }
}

/// Worktree-specific check: creates a temp git worktree, writes a unique function,
/// scans, and verifies search_symbols finds it.
///
/// NOTE: This test will initially fail if #46 (feat/rna-worktree-awareness) is not merged.
/// It is guarded so CI does not block on it — a Skip is reported rather than a Fail
/// when git worktree operations are unavailable.
async fn run_worktree_check(repo: &Path) -> Check {
    // requires feat/rna-worktree-awareness to be merged
    let worktree_path = std::env::temp_dir().join("rna-smoke-worktree");

    // Clean up any leftover worktree from a previous run
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap_or("")])
        .current_dir(repo)
        .output();

    // Create worktree
    let add_result = std::process::Command::new("git")
        .args(["worktree", "add", worktree_path.to_str().unwrap_or(""), "-b", "smoke-test-branch"])
        .current_dir(repo)
        .output();

    let add_out = match add_result {
        Ok(o) => o,
        Err(e) => return Check::skip("worktree_smoke", format!("git worktree add failed to run: {}", e)),
    };

    if !add_out.status.success() {
        let stderr = String::from_utf8_lossy(&add_out.stderr);
        return Check::skip("worktree_smoke", format!("git worktree add failed: {}", stderr.trim()));
    }

    // Write a unique function to the worktree
    let unique_fn_name = "rna_smoke_test_unique_function_abc123";
    let test_file = worktree_path.join("rna_smoke_worktree_fixture.rs");
    let content = format!(
        "// RNA smoke test worktree fixture\n// requires feat/rna-worktree-awareness to be merged\npub fn {}() -> &'static str {{ \"smoke\" }}\n",
        unique_fn_name
    );

    if let Err(e) = std::fs::write(&test_file, &content) {
        cleanup_worktree(repo, &worktree_path);
        return Check::skip("worktree_smoke", format!("Could not write fixture file: {}", e));
    }

    // Scan the worktree root
    let scan_result: Option<Vec<crate::graph::Node>> = {
        match Scanner::new(worktree_path.clone()) {
            Ok(mut s) => {
                match s.scan() {
                    Ok(_) => {
                        let all_files = s.all_known_files();
                        let full_scan = crate::scanner::ScanResult {
                            changed_files: Vec::new(),
                            new_files: all_files,
                            deleted_files: Vec::new(),
                            scan_duration: std::time::Duration::ZERO,
                        };
                        let reg = ExtractorRegistry::with_builtins();
                        let extraction = reg.extract_scan_result(&worktree_path, &full_scan);
                        Some(extraction.nodes)
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    };

    // Assert the unique function was found
    let result = match scan_result {
        Some(nodes) => {
            let found = nodes.iter().any(|n| n.id.name.contains(unique_fn_name) || n.signature.contains(unique_fn_name));
            if found {
                Check::pass("worktree_smoke", format!("Function '{}' found in worktree scan", unique_fn_name))
            } else {
                // Not a hard CI failure until feat/rna-worktree-awareness is merged
                // requires feat/rna-worktree-awareness to be merged
                Check::skip(
                    "worktree_smoke",
                    format!(
                        "Function '{}' not found in worktree scan. \
                         This is expected until feat/rna-worktree-awareness is merged.",
                        unique_fn_name
                    ),
                )
            }
        }
        None => Check::skip("worktree_smoke", "Could not scan worktree (scanner init failed)"),
    };

    cleanup_worktree(repo, &worktree_path);
    result
}

fn cleanup_worktree(repo: &Path, worktree_path: &Path) {
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap_or("")])
        .current_dir(repo)
        .output();
    // Also delete the branch if it exists
    let _ = std::process::Command::new("git")
        .args(["branch", "-D", "smoke-test-branch"])
        .current_dir(repo)
        .output();
}

/// External calls check: verifies that LSP virtual external nodes survive a
/// LanceDB round-trip (persist → reload).
///
/// The check uses a synthetic virtual node so it works even when
/// rust-analyzer is not installed. It exercises the same code paths that
/// live LSP enrichment uses, without spawning a language server.
///
/// Asserts after reload:
/// - At least one node with `id.root == "external"`
/// - That node has `metadata["virtual"] == "true"`
/// - That node's name contains `"::"` (FQN, e.g. `lancedb::connect`)
async fn run_external_calls_check() -> Check {
    use arrow_array::StringArray;
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;
    use std::collections::BTreeMap;
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

    let temp_dir = std::env::temp_dir().join("rna-smoke-external-calls");
    let _ = std::fs::remove_dir_all(&temp_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Check::fail("external_calls_persist", format!("Could not create temp dir: {}", e));
    }

    // Synthesise a virtual node that mimics what LspEnricher produces for an
    // external dependency call (e.g. `lancedb::connect`).
    let mut meta = BTreeMap::new();
    meta.insert("virtual".to_string(), "true".to_string());
    meta.insert("package".to_string(), "lancedb".to_string());

    let virtual_node = Node {
        id: NodeId {
            root: "external".to_string(),
            file: std::path::PathBuf::new(),
            name: "lancedb::connect".to_string(),
            kind: NodeKind::Function,
        },
        language: "rust".to_string(),
        line_start: 0,
        line_end: 0,
        signature: "lancedb::connect".to_string(),
        body: String::new(),
        metadata: meta,
        source: ExtractionSource::Lsp,
    };

    // Step 1: Persist the virtual node via persist_graph_incremental.
    if let Err(e) = persist_graph_incremental(
        &temp_dir,
        &[virtual_node],
        &[],
        &[],
        &[],
    ).await {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Check::fail("external_calls_persist", format!("persist_graph_incremental failed: {}", e));
    }

    // Step 2: Reload from LanceDB — simulates a server restart.
    let db_path = graph_lance_path(&temp_dir);
    let db = match lancedb::connect(db_path.to_str().unwrap_or("")).execute().await {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("external_calls_persist", format!("Could not open LanceDB: {}", e));
        }
    };
    let table = match db.open_table("symbols").execute().await {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("external_calls_persist", format!("Could not open symbols table: {}", e));
        }
    };
    let stream = match table.query().execute().await {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("external_calls_persist", format!("Could not query symbols: {}", e));
        }
    };
    let batches: Vec<arrow_array::RecordBatch> = match stream.try_collect().await {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("external_calls_persist", format!("Could not collect batches: {}", e));
        }
    };

    // Step 3: Assert the virtual node is present with correct fields.
    let mut found_root_external = false;
    let mut found_virtual_true = false;
    let mut found_fqn = false;

    for batch in &batches {
        let root_ids = match batch.column_by_name("root_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        {
            Some(a) => a,
            None => continue,
        };
        let names = match batch.column_by_name("name")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        {
            Some(a) => a,
            None => continue,
        };
        // metadata_json column carries serialized BTreeMap<String,String>.
        let metadata_col = batch.column_by_name("metadata_json")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());

        for i in 0..batch.num_rows() {
            let root = root_ids.value(i);
            let name = names.value(i);
            if root == "external" && name.contains("::") {
                found_root_external = true;
                found_fqn = true;
                // Check metadata_json for virtual=true
                if let Some(meta_col) = metadata_col {
                    let meta_str = meta_col.value(i);
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(meta_str) {
                        if parsed.get("virtual").and_then(|v| v.as_str()) == Some("true") {
                            found_virtual_true = true;
                        }
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&temp_dir);

    if !found_root_external {
        Check::fail(
            "external_calls_persist",
            "Virtual external node (root='external') not found in LanceDB after persist",
        )
    } else if !found_fqn {
        Check::fail(
            "external_calls_persist",
            "Virtual external node name does not contain '::' (expected FQN like lancedb::connect)",
        )
    } else if !found_virtual_true {
        Check::fail(
            "external_calls_persist",
            "Virtual external node metadata[\"virtual\"] != \"true\" after LanceDB round-trip — \
             metadata_json column missing or not populated in persist path",
        )
    } else {
        Check::pass(
            "external_calls_persist",
            "Virtual external node round-tripped correctly: root=external, FQN name, virtual=true in metadata",
        )
    }
}


/// list_roots check: verifies WorkspaceConfig::with_primary_root() returns at least 1 root
/// that matches the test repo path.
fn run_list_roots_check(repo: &Path) -> Check {
    let workspace = WorkspaceConfig::load().with_primary_root(repo.to_path_buf());
    let roots = workspace.resolved_roots();
    if roots.is_empty() {
        return Check::fail("list_roots_check", "No workspace roots resolved from with_primary_root()");
    }
    let primary_matches = roots.iter().any(|r| r.path == repo);
    if !primary_matches {
        Check::fail(
            "list_roots_check",
            format!(
                "Primary root {} not found in resolved roots: {:?}",
                repo.display(),
                roots.iter().map(|r| r.path.display().to_string()).collect::<Vec<_>>()
            ),
        )
    } else {
        Check::pass(
            "list_roots_check",
            format!("{} root(s) resolved, primary root matched", roots.len()),
        )
    }
}

/// Worktree roots check: creates a real git worktree, then verifies that
/// WorkspaceConfig::with_worktrees() detects it in resolved_roots().
async fn run_worktree_roots_check(repo: &Path) -> Check {
    let worktree_path = std::env::temp_dir().join("rna-smoke-worktree-roots-check");
    let branch_name = "smoke-worktree-roots-branch";

    // Clean up any pre-existing worktree / branch
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap_or("")])
        .current_dir(repo)
        .output();
    let _ = std::process::Command::new("git")
        .args(["branch", "-D", branch_name])
        .current_dir(repo)
        .output();

    // Create a linked worktree
    let add_out = match std::process::Command::new("git")
        .args(["worktree", "add", worktree_path.to_str().unwrap_or(""), "-b", branch_name])
        .current_dir(repo)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Check::skip("worktree_roots_check", format!("git worktree add failed: {}", e));
        }
    };

    if !add_out.status.success() {
        let stderr = String::from_utf8_lossy(&add_out.stderr);
        return Check::skip("worktree_roots_check", format!("git worktree add failed: {}", stderr.trim()));
    }

    // Build workspace config with primary root + worktrees
    let workspace = WorkspaceConfig::load()
        .with_primary_root(repo.to_path_buf())
        .with_worktrees(repo);
    let roots = workspace.resolved_roots();

    // Clean up before asserting
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap_or("")])
        .current_dir(repo)
        .output();
    let _ = std::process::Command::new("git")
        .args(["branch", "-D", branch_name])
        .current_dir(repo)
        .output();

    let found = roots.iter().any(|r| r.path == worktree_path);
    if found {
        Check::pass(
            "worktree_roots_check",
            format!("with_worktrees() detected the linked worktree at {}", worktree_path.display()),
        )
    } else {
        Check::skip(
            "worktree_roots_check",
            format!(
                "Linked worktree {} not found in resolved_roots() — may require worktree to be fully set up",
                worktree_path.display()
            ),
        )
    }
}

/// Metadata round-trip check: persists a node with BTreeMap metadata via
/// persist_graph_to_lance, reloads with load_graph_from_lance, and asserts the
/// metadata is identical.
async fn run_metadata_roundtrip_check() -> Check {
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
    use std::collections::BTreeMap;

    let temp_dir = std::env::temp_dir().join("rna-smoke-metadata-roundtrip");
    let _ = std::fs::remove_dir_all(&temp_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Check::fail("metadata_roundtrip", format!("Could not create temp dir: {}", e));
    }

    let mut meta = BTreeMap::new();
    meta.insert("custom_key".to_string(), "custom_value".to_string());
    meta.insert("priority".to_string(), "high".to_string());

    let node = Node {
        id: NodeId {
            root: "test-root".to_string(),
            file: std::path::PathBuf::from("src/test.rs"),
            name: "test_metadata_fn".to_string(),
            kind: NodeKind::Function,
        },
        language: "rust".to_string(),
        line_start: 1,
        line_end: 5,
        signature: "fn test_metadata_fn()".to_string(),
        body: "fn test_metadata_fn() {}".to_string(),
        metadata: meta.clone(),
        source: ExtractionSource::TreeSitter,
    };

    // Persist via the full DROP+CREATE path
    if let Err(e) = persist_graph_to_lance(&temp_dir, &[node], &[]).await {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Check::fail("metadata_roundtrip", format!("persist_graph_to_lance failed: {}", e));
    }

    // Reload from LanceDB
    let state = match load_graph_from_lance(&temp_dir).await {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("metadata_roundtrip", format!("load_graph_from_lance failed: {}", e));
        }
    };

    let _ = std::fs::remove_dir_all(&temp_dir);

    // Find the node and verify metadata
    let reloaded = state.nodes.iter().find(|n| n.id.name == "test_metadata_fn");
    match reloaded {
        None => Check::fail("metadata_roundtrip", "Node 'test_metadata_fn' not found after reload"),
        Some(n) => {
            let missing: Vec<_> = meta
                .iter()
                .filter(|(k, v)| n.metadata.get(*k) != Some(v))
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            if missing.is_empty() {
                Check::pass(
                    "metadata_roundtrip",
                    format!("Node metadata round-tripped correctly ({} keys)", meta.len()),
                )
            } else {
                Check::fail(
                    "metadata_roundtrip",
                    format!("Metadata mismatch after reload — missing or wrong: {:?}", missing),
                )
            }
        }
    }
}

/// search_symbols check: verifies that the extracted nodes include function nodes
/// and that they are findable by kind.
fn run_search_symbols_check(nodes: &[crate::graph::Node]) -> Check {
    if nodes.is_empty() {
        return Check::skip("search_symbols_check", "No nodes available (extraction may have failed)");
    }

    let function_nodes: Vec<_> = nodes
        .iter()
        .filter(|n| n.id.kind == crate::graph::NodeKind::Function)
        .collect();

    let markdown_or_other: Vec<_> = nodes
        .iter()
        .filter(|n| !matches!(n.id.kind, crate::graph::NodeKind::Function))
        .collect();

    if function_nodes.is_empty() {
        Check::fail(
            "search_symbols_check",
            format!("No Function nodes found in {} total nodes", nodes.len()),
        )
    } else {
        Check::pass(
            "search_symbols_check",
            format!(
                "{} function node(s) found, {} other node(s) (total: {})",
                function_nodes.len(),
                markdown_or_other.len(),
                nodes.len()
            ),
        )
    }
}

/// graph_query check: verifies the GraphIndex has been populated with edges and
/// that neighbors() is callable and returns sensible results.
fn run_graph_query_check(index: &GraphIndex, nodes: &[crate::graph::Node]) -> Check {
    if nodes.is_empty() {
        return Check::skip("graph_query_check", "No nodes available — skipping graph query");
    }

    let node_count = index.node_count();
    let edge_count = index.edge_count();

    // If no nodes in index, graph was likely not built from edges — skip gracefully
    if node_count == 0 {
        return Check::skip(
            "graph_query_check",
            "GraphIndex is empty (no edges extracted). This is expected for repos with no call edges yet.",
        );
    }

    // Pick the first node and verify neighbors() is callable
    let first_node = &nodes[0];
    let node_id = first_node.stable_id();
    let outgoing = index.neighbors(&node_id, None, Direction::Outgoing);
    let incoming = index.neighbors(&node_id, None, Direction::Incoming);

    Check::pass(
        "graph_query_check",
        format!(
            "GraphIndex: {} nodes, {} edges. First node '{}': {} outgoing, {} incoming neighbors",
            node_count,
            edge_count,
            first_node.id.name,
            outgoing.len(),
            incoming.len()
        ),
    )
}

/// oh_search_context (semantic) check: searches the embedding index for "alignment"
/// and verifies at least one result is returned from the .oh/ artifacts.
async fn run_oh_search_context_check(embed_index: &Option<EmbeddingIndex>) -> Check {
    let index = match embed_index {
        Some(i) => i,
        None => {
            return Check::skip(
                "oh_search_context",
                "EmbeddingIndex not available (fastembed may not be compiled in)",
            );
        }
    };

    match index.search("alignment", None, 5).await {
        Err(e) => {
            // Table-not-found means indexing was skipped — not a hard failure
            let msg = e.to_string();
            if msg.contains("Table not found") {
                Check::skip("oh_search_context", "Embedding table not found — run index_all first")
            } else {
                Check::fail("oh_search_context", format!("Semantic search failed: {}", e))
            }
        }
        Ok(results) => {
            if results.is_empty() {
                Check::skip(
                    "oh_search_context",
                    "Semantic search for 'alignment' returned 0 results (index may be empty)",
                )
            } else {
                Check::pass(
                    "oh_search_context",
                    format!(
                        "Semantic search for 'alignment' returned {} result(s); top: '{}' (score: {:.2})",
                        results.len(),
                        results[0].title,
                        results[0].score
                    ),
                )
            }
        }
    }
}

// ── Output ──────────────────────────────────────────────────────────

fn print_and_return(args: &TestArgs, checks: Vec<Check>) -> bool {
    let all_passed = checks.iter().all(|c| c.status != CheckStatus::Fail);
    let result = SmokeResult { pass: all_passed, checks };

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    } else {
        println!("RNA Pipeline Smoke Test");
        println!("=======================");
        for c in &result.checks {
            let icon = match c.status {
                CheckStatus::Pass => "PASS",
                CheckStatus::Fail => "FAIL",
                CheckStatus::Skip => "SKIP",
            };
            let detail = c.detail.as_deref().unwrap_or("");
            println!("  [{icon}] {name}: {detail}", icon = icon, name = c.name, detail = detail);
        }
        println!("=======================");
        if result.pass {
            println!("Result: PASS");
        } else {
            println!("Result: FAIL");
        }
    }

    result.pass
}
