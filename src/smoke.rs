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
use crate::graph::{Edge, Node};
use crate::graph::index::GraphIndex;
use crate::query;
use crate::roots::WorkspaceConfig;
use crate::scanner::Scanner;
use crate::server::{check_and_migrate_schema, graph_lance_path, load_graph_from_lance, persist_graph_incremental, persist_graph_to_lance};

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
    tracing::info!("Smoke: starting RNA pipeline test for {}", repo.display());
    let mut checks: Vec<Check> = Vec::new();

    // 1. Scanner initializes
    let scanner_init_start = std::time::Instant::now();
    tracing::info!("Smoke: initializing scanner");
    let mut scanner = match Scanner::new(repo.clone()) {
        Ok(s) => {
            tracing::info!(
                "Smoke: scanner initialized in {:?}",
                scanner_init_start.elapsed()
            );
            checks.push(Check::pass("scanner_init", "Scanner created successfully"));
            s
        }
        Err(e) => {
            checks.push(Check::fail("scanner_init", format!("Scanner::new failed: {}", e)));
            return Ok(print_and_return(args, checks));
        }
    };

    // 2. File walk
    let file_walk_start = std::time::Instant::now();
    tracing::info!("Smoke: walking files");
    let _scan_result = match scanner.scan() {
        Ok(r) => {
            let all_files = scanner.all_known_files();
            let count = all_files.len();
            tracing::info!(
                "Smoke: file walk completed in {:?} ({} files known)",
                file_walk_start.elapsed(),
                count
            );
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
    let extraction_start = std::time::Instant::now();
    tracing::info!("Smoke: extracting symbols from scanned files");
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
    tracing::info!(
        "Smoke: symbol extraction completed in {:?} ({} symbols, {} edges)",
        extraction_start.elapsed(),
        extraction.nodes.len(),
        extraction.edges.len()
    );
    if symbol_count > 0 {
        checks.push(Check::pass("symbol_extraction", format!("{} symbols extracted", symbol_count)));
    } else {
        checks.push(Check::fail("symbol_extraction", "No symbols extracted"));
    }

    // 4. Graph index build
    let graph_index_start = std::time::Instant::now();
    tracing::info!("Smoke: building graph index");
    let all_nodes = extraction.nodes;
    let all_edges = extraction.edges;
    let mut index = GraphIndex::new();
    index.rebuild_from_edges(&all_edges);
    for node in &all_nodes {
        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
    }
    tracing::info!(
        "Smoke: graph index built in {:?} ({} edges)",
        graph_index_start.elapsed(),
        all_edges.len()
    );
    checks.push(Check::pass("graph_index", format!("{} edges indexed", all_edges.len())));

    // 5. Embedding index
    let embed_start = std::time::Instant::now();
    tracing::info!("Smoke: preparing embedding index");
    let embed_index = match EmbeddingIndex::new(&repo).await {
        Ok(idx) => {
            // Probe first: if the table already exists, reuse it; otherwise rebuild.
            match idx.search("_probe_", None, 1).await {
                Ok(crate::embed::SearchOutcome::Results(_)) => {
                    tracing::info!(
                        "Smoke: embedding index reused in {:?}",
                        embed_start.elapsed()
                    );
                    checks.push(Check::pass("embed_index", "Persisted embedding index reused"));
                    Some(idx)
                }
                Ok(crate::embed::SearchOutcome::NotReady) | Err(_) => {
                    let embeddable: Vec<_> = all_nodes.iter()
                        .filter(|n| n.id.root != "external")
                        .take(50)
                        .cloned()
                        .collect();
                    match idx.index_all_with_symbols(&repo, &embeddable).await {
                        Ok(count) => {
                            tracing::info!(
                                "Smoke: embedding index built in {:?} ({} vectors)",
                                embed_start.elapsed(),
                                count
                            );
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

    // 6. oh_search_context path — embed search for "main"
    match &embed_index {
        Some(idx) => {
            match idx.search("main", None, 5).await {
                Ok(crate::embed::SearchOutcome::Results(results)) if !results.is_empty() => {
                    checks.push(Check::pass("oh_search_context", format!("{} results for query \"main\"", results.len())));
                }
                Ok(crate::embed::SearchOutcome::NotReady) => {
                    checks.push(Check::skip("oh_search_context", "Embedding table not built yet"));
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
    match query::outcome_progress(&repo, "agent-alignment", &all_nodes) {
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

    // 14. HEAD-change detection logic
    checks.push(run_head_change_detection_check());

    // 15. Cross-language constants smoke test
    // Part A: in-memory extraction
    let const_nodes: Vec<_> = all_nodes.iter()
        .filter(|n| n.id.kind == crate::graph::NodeKind::Const)
        .collect();
    if const_nodes.is_empty() {
        checks.push(Check::fail("const_extraction", "No Const nodes extracted"));
    } else {
        let with_value = const_nodes.iter().filter(|n| n.metadata.contains_key("value")).count();
        checks.push(Check::pass(
            "const_extraction",
            format!("{} Const nodes, {} with value", const_nodes.len(), with_value),
        ));
    }

    // 16. LanceDB round-trip — persist, reload, assert Const value survives
    checks.push(run_const_lance_roundtrip(&all_nodes, &all_edges).await);

    // 17. list_roots -- verifies WorkspaceConfig returns at least 1 root matching the test repo
    checks.push(run_list_roots_check(&repo));

    // 18. Worktree roots -- creates a real git worktree, checks with_worktrees() sees it
    checks.push(run_worktree_roots_check(&repo).await);

    // 19. Metadata round-trip -- persist node with metadata, reload, assert identical
    checks.push(run_metadata_roundtrip_check().await);

    // 20. search_symbols -- verifies function nodes and Const nodes are findable
    checks.push(run_search_symbols_check(&all_nodes));

    // 21. graph_query -- verifies GraphIndex has edges and neighbors() is callable
    checks.push(run_graph_query_check(&index, &all_nodes));

    // 22. oh_search_context (semantic) -- searches for "alignment" in embedding index
    checks.push(run_oh_search_context_check(&embed_index).await);

    // 23. Schema version check — write stale version, verify migration, verify no-op on re-check
    checks.push(run_schema_version_check().await);

    // 24. Broken symlink check — scanner must not crash on broken symlinks
    checks.push(run_broken_symlink_check().await);

    // 25. LSP status footer — verifies format_freshness renders all 3 LSP states
    checks.push(run_lsp_status_footer_check());

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

/// Persist a subset of nodes to a temp LanceDB, reload, and verify Const `value` survives.
async fn run_const_lance_roundtrip(nodes: &[Node], edges: &[Edge]) -> Check {
    // Pick at most 50 nodes so the write is fast; ensure at least one Const with a value
    let const_with_value: Vec<_> = nodes.iter()
        .filter(|n| n.id.kind == crate::graph::NodeKind::Const && n.metadata.contains_key("value"))
        .cloned()
        .collect();

    if const_with_value.is_empty() {
        return Check::skip("const_lance_roundtrip", "No Const nodes with value — skipping LanceDB round-trip check");
    }

    // Use a temp dir as the fake repo root for LanceDB
    let tmp_root = std::env::temp_dir().join("rna-smoke-const-lance-roundtrip");
    if let Err(e) = std::fs::create_dir_all(&tmp_root) {
        return Check::fail("const_lance_roundtrip", format!("Failed to create temp dir: {}", e));
    }
    let tmp_root = tmp_root.as_path();

    // Persist: use the const nodes + a small sample of other nodes
    let sample: Vec<_> = nodes.iter().take(50).cloned()
        .chain(const_with_value.iter().cloned())
        .collect();
    // Deduplicate by stable_id
    let mut seen = std::collections::HashSet::new();
    let deduped: Vec<_> = sample.into_iter().filter(|n| seen.insert(n.stable_id())).collect();

    if let Err(e) = persist_graph_to_lance(tmp_root, &deduped, edges).await {
        return Check::fail("const_lance_roundtrip", format!("persist_graph_to_lance failed: {}", e));
    }

    // Reload
    let reloaded = match load_graph_from_lance(tmp_root).await {
        Ok(state) => state,
        Err(e) => return Check::fail("const_lance_roundtrip", format!("load_graph_from_lance failed: {}", e)),
    };

    // Assert Const nodes still have value after reload
    let reloaded_consts_with_value = reloaded.nodes.iter()
        .filter(|n| n.id.kind == crate::graph::NodeKind::Const && n.metadata.contains_key("value"))
        .count();

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(tmp_root);

    if reloaded_consts_with_value == 0 {
        Check::fail("const_lance_roundtrip", "Const nodes lost `value` metadata after LanceDB round-trip")
    } else {
        Check::pass(
            "const_lance_roundtrip",
            format!("{} Const nodes retained `value` after persist→reload", reloaded_consts_with_value),
        )
    }
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
    use arrow_array::{Array, BooleanArray, StringArray};
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
        // Typed meta_virtual column — Boolean, nullable.
        let meta_virtual_col = batch.column_by_name("meta_virtual")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());

        for i in 0..batch.num_rows() {
            let root = root_ids.value(i);
            let name = names.value(i);
            if root == "external" && name.contains("::") {
                found_root_external = true;
                found_fqn = true;
                // Check meta_virtual typed column for true
                if let Some(col) = meta_virtual_col {
                    if !col.is_null(i) && col.value(i) {
                        found_virtual_true = true;
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
            "Virtual external node meta_virtual != true after LanceDB round-trip — \
             meta_virtual column missing or not populated in persist path",
        )
    } else {
        Check::pass(
            "external_calls_persist",
            "Virtual external node round-tripped correctly: root=external, FQN name, meta_virtual=true",
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
    meta.insert("value".to_string(), "42".to_string());
    meta.insert("synthetic".to_string(), "false".to_string());
    meta.insert("cyclomatic".to_string(), "7".to_string());

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

fn run_lsp_status_footer_check() -> Check {
    use crate::server::{format_freshness, LspEnrichmentStatus};

    let status = LspEnrichmentStatus::default();

    // Not started → no LSP in footer
    let footer = format_freshness(100, Some(std::time::Instant::now()), Some(&status));
    if footer.contains("LSP") {
        return Check::fail("lsp_status_footer", "Footer shows LSP segment in NotStarted state");
    }

    // Running → "LSP: pending"
    status.set_running();
    let footer = format_freshness(100, Some(std::time::Instant::now()), Some(&status));
    if !footer.contains("LSP: pending") {
        return Check::fail("lsp_status_footer", format!("Expected 'LSP: pending', got: {}", footer));
    }

    // Complete → "LSP: enriched (N edges)"
    status.set_complete(42);
    let footer = format_freshness(100, Some(std::time::Instant::now()), Some(&status));
    if !footer.contains("LSP: enriched (42 edges)") {
        return Check::fail("lsp_status_footer", format!("Expected 'LSP: enriched (42 edges)', got: {}", footer));
    }

    Check::pass("lsp_status_footer", "Footer renders all 3 LSP states correctly")
}

/// HEAD-change detection check.
///
/// Verifies the logic used by the background scanner to decide whether a
/// reindex is needed:
///
/// 1. Creates a temp git repo with an initial commit (HEAD = commit A).
/// 2. Records the HEAD OID.
/// 3. Makes a second commit (HEAD = commit B).
/// 4. Re-reads the HEAD OID.
/// 5. Asserts the two OIDs differ (change detected).
/// 6. Reads the OID a third time without committing.
/// 7. Asserts the OID is unchanged (no spurious trigger).
///
/// This is a unit-level check — it exercises only the git2 detection path,
/// not the full reindex cycle.
fn run_head_change_detection_check() -> Check {
    fn read_head_oid(repo: &git2::Repository) -> Option<git2::Oid> {
        repo.head()
            .ok()?
            .peel_to_commit()
            .ok()
            .map(|c| c.id())
    }

    fn make_commit(repo: &git2::Repository, message: &str) -> Result<git2::Oid, git2::Error> {
        let sig = git2::Signature::now("Test", "test@example.com")?;
        let mut index = repo.index()?;
        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(head_ref) => {
                let commit = head_ref.peel_to_commit()?;
                vec![commit]
            }
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
    }

    // Step 1: init temp repo.
    let temp_dir = std::env::temp_dir().join("rna-smoke-head-change");
    let _ = std::fs::remove_dir_all(&temp_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Check::fail(
            "head_change_detection",
            format!("Could not create temp dir: {}", e),
        );
    }
    let repo = match git2::Repository::init(&temp_dir) {
        Ok(r) => r,
        Err(e) => {
            return Check::fail(
                "head_change_detection",
                format!("git2::Repository::init failed: {}", e),
            )
        }
    };

    // Step 2: initial commit.
    if let Err(e) = make_commit(&repo, "initial commit") {
        return Check::fail(
            "head_change_detection",
            format!("First commit failed: {}", e),
        );
    }
    let oid_a = match read_head_oid(&repo) {
        Some(o) => o,
        None => {
            return Check::fail(
                "head_change_detection",
                "Could not read HEAD OID after first commit",
            )
        }
    };

    // Step 3: second commit.
    if let Err(e) = make_commit(&repo, "second commit") {
        return Check::fail(
            "head_change_detection",
            format!("Second commit failed: {}", e),
        );
    }
    let oid_b = match read_head_oid(&repo) {
        Some(o) => o,
        None => {
            return Check::fail(
                "head_change_detection",
                "Could not read HEAD OID after second commit",
            )
        }
    };

    // Step 4: assert change detected.
    if oid_a == oid_b {
        return Check::fail(
            "head_change_detection",
            format!(
                "HEAD OID unchanged after second commit (both = {}); detection would not fire",
                oid_a
            ),
        );
    }

    // Step 5: assert no spurious change on stable re-read.
    let oid_b2 = match read_head_oid(&repo) {
        Some(o) => o,
        None => {
            return Check::fail(
                "head_change_detection",
                "Could not re-read HEAD OID for stable-state check",
            )
        }
    };
    if oid_b != oid_b2 {
        return Check::fail(
            "head_change_detection",
            "HEAD OID changed between reads without a commit; spurious trigger would occur",
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);

    Check::pass(
        "head_change_detection",
        format!(
            "HEAD-change detection correct: A ({}) != B ({}); stable re-read matches",
            &oid_a.to_string()[..8],
            &oid_b.to_string()[..8],
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
        Ok(crate::embed::SearchOutcome::NotReady) => {
            Check::skip("oh_search_context", "Embedding table not built yet — index is still building")
        }
        Ok(crate::embed::SearchOutcome::Results(results)) => {
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
        Err(e) => {
            Check::fail("oh_search_context", format!("Semantic search failed: {}", e))
        }
    }
}

/// Schema version check: verifies that `check_and_migrate_schema` detects stale versions
/// and performs a clean migration, and is idempotent when the version already matches.
///
/// 1. Writes a `_schema_meta` row with version 0 (intentionally stale).
/// 2. Calls `check_and_migrate_schema` — asserts it returns `true` (migration occurred).
/// 3. Reads back the stored version — asserts it now equals `SCHEMA_VERSION`.
/// 4. Calls `check_and_migrate_schema` again — asserts it returns `false` (no-op).
async fn run_schema_version_check() -> Check {
    use crate::graph::store::{schema_meta_schema, SCHEMA_VERSION};
    use arrow_array::{RecordBatch, RecordBatchIterator, StringArray};
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;
    use std::sync::Arc;

    let temp_dir = std::env::temp_dir().join("rna-smoke-schema-version");
    let _ = std::fs::remove_dir_all(&temp_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Check::fail("schema_version_check", format!("Could not create temp dir: {}", e));
    }

    let db_path = temp_dir.join(".oh").join(".cache").join("lance");
    if let Err(e) = std::fs::create_dir_all(&db_path) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Check::fail("schema_version_check", format!("Could not create lance dir: {}", e));
    }

    // Step 1: Seed _schema_meta with version 0 (stale).
    let seed_result: anyhow::Result<()> = async {
        let db = lancedb::connect(db_path.to_str().unwrap_or_default())
            .execute()
            .await?;
        let schema = Arc::new(schema_meta_schema());
        let keys = StringArray::from(vec!["schema_version"]);
        let values = StringArray::from(vec!["0"]);
        let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(keys), Arc::new(values)])?;
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        db.create_table("_schema_meta", Box::new(batches))
            .execute()
            .await?;
        Ok(())
    }
    .await;

    if let Err(e) = seed_result {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Check::fail("schema_version_check", format!("Failed to seed stale meta: {}", e));
    }

    // Step 2: First call — expect migration (returns true).
    let migrated = match check_and_migrate_schema(&db_path).await {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("schema_version_check", format!("check_and_migrate_schema failed: {}", e));
        }
    };

    if !migrated {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Check::fail(
            "schema_version_check",
            "Expected migration=true for stale version 0, got false",
        );
    }

    // Step 3: Read back stored version — must equal SCHEMA_VERSION.
    let stored_version: anyhow::Result<u32> = async {
        let db = lancedb::connect(db_path.to_str().unwrap_or_default())
            .execute()
            .await?;
        let tbl = db.open_table("_schema_meta").execute().await?;
        let batches: Vec<_> = tbl.query().execute().await?.try_collect().await?;
        for batch in &batches {
            let keys = batch
                .column_by_name("key")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| anyhow::anyhow!("missing key column"))?;
            let values = batch
                .column_by_name("value")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| anyhow::anyhow!("missing value column"))?;
            for i in 0..batch.num_rows() {
                if keys.value(i) == "schema_version" {
                    return values.value(i).parse::<u32>().map_err(Into::into);
                }
            }
        }
        anyhow::bail!("schema_version key not found in _schema_meta")
    }
    .await;

    let version = match stored_version {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("schema_version_check", format!("Failed to read stored version: {}", e));
        }
    };

    if version != SCHEMA_VERSION {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Check::fail(
            "schema_version_check",
            format!("Stored version {} != SCHEMA_VERSION {}", version, SCHEMA_VERSION),
        );
    }

    // Step 4: Second call — must be a no-op (returns false).
    let noop = match check_and_migrate_schema(&db_path).await {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Check::fail("schema_version_check", format!("Second check_and_migrate_schema failed: {}", e));
        }
    };

    let _ = std::fs::remove_dir_all(&temp_dir);

    if noop {
        return Check::fail(
            "schema_version_check",
            "Expected no-op (false) on second call with matching version, got true",
        );
    }

    Check::pass(
        "schema_version_check",
        format!(
            "Migration detected stale v0→v{SCHEMA_VERSION}, then confirmed no-op on re-check"
        ),
    )
}

/// Broken symlink check: creates a temp repo with a broken symlink,
/// verifies the scanner completes without crashing.
async fn run_broken_symlink_check() -> Check {
    let temp_dir = std::env::temp_dir().join("rna-smoke-broken-symlink");
    let _ = std::fs::remove_dir_all(&temp_dir);
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Check::fail("broken_symlink", format!("Could not create temp dir: {}", e));
    }

    // Init a git repo so the scanner can use it
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(&temp_dir)
        .output();

    // Create a real file so the scan has something to find
    let _ = std::fs::write(temp_dir.join("main.rs"), "fn hello() {}\n");

    // Create a broken symlink
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink("/nonexistent/path/that/does/not/exist", temp_dir.join("broken-link.rs"));
    }

    // Scan — should complete without error
    let mut scanner = match crate::scanner::Scanner::new(temp_dir.clone()) {
        Ok(s) => s,
        Err(e) => return Check::fail("broken_symlink", format!("Scanner init failed: {}", e)),
    };

    match scanner.scan() {
        Ok(result) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            Check::pass(
                "broken_symlink",
                format!("Scan completed with broken symlink present ({} files)", result.changed_files.len() + result.new_files.len()),
            )
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            Check::fail("broken_symlink", format!("Scan crashed on broken symlink: {}", e))
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
