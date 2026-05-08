use std::process::Command;

use tempfile::TempDir;

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
    std::fs::write(
        tmp.path().join("src/lib.rs"),
        "pub fn alpha() -> i32 { beta() }\npub fn beta() -> i32 { 42 }\n",
    )
    .expect("write lib.rs");

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
}
