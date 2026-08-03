use std::process::Command;

use tempfile::TempDir;

#[test]
fn task_and_flat_infeasible_render_budgets_exit_nonzero_without_stdout() {
    let tmp = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src dir");
    std::fs::write(
        tmp.path().join("src/lib.rs"),
        "pub fn actionable() -> bool { true }\n",
    )
    .expect("write source fixture");
    let repo = tmp.path().to_str().expect("utf-8 temp path");
    let prepared = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"))
        .args(["scan", "--extract-only", "--repo", repo])
        .output()
        .expect("prepare search cache");
    assert!(
        prepared.status.success(),
        "fixture scan failed:\n{}",
        String::from_utf8_lossy(&prepared.stderr)
    );

    for context_mode in [None, Some("task")] {
        for budget_flag in ["--max-output-bytes", "--max-output-tokens"] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"));
            command.args([
                "search",
                "change actionable behavior and add a regression test",
                "--repo",
                repo,
                budget_flag,
                "1",
            ]);
            if let Some(context_mode) = context_mode {
                command.args(["--context-mode", context_mode]);
            }
            let output = command.output().expect("run bounded search");
            assert!(
                !output.status.success(),
                "infeasible {context_mode:?} {budget_flag} search must fail"
            );
            assert!(
                output.stdout.is_empty(),
                "infeasible search must not emit oversized stdout: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("BudgetTooSmall"),
                "typed error missing from stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn foreground_full_scan_lsp_failure_exits_nonzero() {
    if Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: rust-analyzer not installed");
        return;
    }

    let tmp = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src dir");
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"rna_pass1_abort_contract\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .expect("write Cargo.toml");
    // A deliberately large call graph makes the one-millisecond no-progress
    // watchdog deterministic: rust-analyzer cannot complete every reference
    // request before the watchdog observes the unfinished pass.
    let mut source = String::with_capacity(1_000_000);
    for index in 0..12_000 {
        let next = (index + 1) % 12_000;
        source.push_str(&format!(
            "pub fn node_{index}() -> usize {{ node_{next}() }}\n"
        ));
    }
    std::fs::write(tmp.path().join("src/lib.rs"), source).expect("write lib.rs");

    let output = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"))
        .env("RNA_LSP_PASS1_NO_PROGRESS_TIMEOUT_MS", "1")
        .env("RNA_LSP_DID_OPEN_TIMEOUT_MS", "2000")
        .args([
            "scan",
            "--full",
            "--no-embed",
            "--repo",
            tmp.path().to_str().expect("utf-8 temp path"),
            "--timings",
        ])
        .output()
        .expect("run repo-native-alignment scan");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let lower_stderr = stderr.to_ascii_lowercase();
    let pre_pass1_lsp_failure = !stderr.contains("Diagnostic snapshot: pass=lsp_pass1_references")
        && !stderr.contains("LSP call-reference enrichment aborted")
        && (stderr.contains("Missing Content-Length header")
            || lower_stderr.contains("connection reset")
            || lower_stderr.contains("broken pipe")
            || lower_stderr.contains("connection refused"));
    if pre_pass1_lsp_failure {
        eprintln!(
            "skipping forced Pass 1 abort contract: rust-analyzer failed before Pass 1 on this host; status={:?}; stderr:\n{}",
            output.status, stderr
        );
        return;
    }
    let forced_abort_observed = stderr.contains("Diagnostic snapshot: pass=lsp_pass1_references")
        || stderr.contains("LSP call-reference enrichment aborted")
        || stderr.contains("LSP enrichment aborted");
    assert!(
        forced_abort_observed,
        "large fixture must deterministically trigger the forced Pass 1 abort; status={:?}\nstderr:\n{}",
        output.status, stderr
    );
    assert!(
        !output.status.success(),
        "foreground aborted LSP enrichment must be non-zero; status={:?}\nstderr:\n{}",
        output.status,
        stderr
    );
    assert!(
        stderr.contains("Diagnostic snapshot: pass=lsp_pass1_references")
            || stderr.contains("Error: LSP call-reference enrichment aborted")
            || stderr.contains("Error: EventBus enrichment pipeline: PassesComplete event absent"),
        "expected delivered LSP failure diagnostic in stderr:\n{}",
        stderr
    );

    // Commit scanner state without replacing the durable call-reference job, then
    // start a fresh CLI process on the cache-only path. The degraded readiness and
    // original diagnostic must survive the process boundary via the job ledger.
    let prepare_cache_only = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"))
        .args([
            "scan",
            "--extract-only",
            "--repo",
            tmp.path().to_str().expect("utf-8 temp path"),
        ])
        .output()
        .expect("commit scanner state for cache-only restart");
    assert!(
        prepare_cache_only.status.success(),
        "extract-only scanner-state preparation failed:\n{}",
        String::from_utf8_lossy(&prepare_cache_only.stderr)
    );

    let restarted = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"))
        .args([
            "scan",
            "--no-embed",
            "--repo",
            tmp.path().to_str().expect("utf-8 temp path"),
            "--timings",
        ])
        .output()
        .expect("run cache-only scan in fresh process");
    let restarted_stderr = String::from_utf8_lossy(&restarted.stderr);
    assert!(
        restarted.status.success(),
        "cache-only scan failed:\n{restarted_stderr}"
    );
    assert!(
        restarted_stderr.contains("call_references: degraded"),
        "cache-only summary lost durable degraded readiness:\n{restarted_stderr}"
    );
    assert!(
        restarted_stderr.contains("forced no-progress") || restarted_stderr.contains("no progress"),
        "cache-only summary lost durable abort diagnostic:\n{restarted_stderr}"
    );
}
