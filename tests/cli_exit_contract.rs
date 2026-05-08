use std::process::Command;

use tempfile::TempDir;

#[test]
fn foreground_full_scan_lsp_pass1_abort_exits_nonzero() {
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
    assert!(
        !output.status.success(),
        "forced LSP Pass 1 abort must be non-zero; status={:?}\nstderr:\n{}",
        output.status,
        stderr
    );
    assert!(
        stderr.contains("Diagnostic snapshot: pass=lsp_pass1_references"),
        "expected diagnostic snapshot in stderr:\n{}",
        stderr
    );
    assert!(
        stderr.contains("Error: EventBus enrichment pipeline: PassesComplete event absent"),
        "expected hard pipeline invariant error in stderr:\n{}",
        stderr
    );
}
