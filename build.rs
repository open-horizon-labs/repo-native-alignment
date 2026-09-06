fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-env-changed=RNA_PRODUCER_COMMIT");
    let valid_commit = |commit: String| {
        let commit = commit.trim().to_string();
        (commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(commit)
    };
    let producer_commit = std::env::var("RNA_PRODUCER_COMMIT")
        .ok()
        .and_then(&valid_commit)
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(&valid_commit)
        })
        .expect(
            "RNA producer identity requires a 40-character Git commit from the checkout or RNA_PRODUCER_COMMIT",
        );
    println!("cargo:rustc-env=RNA_PRODUCER_COMMIT={producer_commit}");
}
