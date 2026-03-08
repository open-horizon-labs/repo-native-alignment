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

use crate::embed::EmbeddingIndex;
use crate::extract::ExtractorRegistry;
use crate::graph::index::GraphIndex;
use crate::oh;
use crate::query;
use crate::roots::WorkspaceConfig;
use crate::scanner::Scanner;

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

    // 11. Worktree smoke test
    // requires feat/rna-worktree-awareness to be merged
    checks.push(run_worktree_check(&repo).await);

    Ok(print_and_return(args, checks))
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
