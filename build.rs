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

    for variable in [
        "CANDLE_METAL_ENABLE_FAST_MATH",
        "PROFILE",
        "TARGET",
        "CARGO_FEATURE_EMBEDDINGS",
        "CARGO_FEATURE_METAL",
        "CARGO_FEATURE_SWEBENCH_SEMANTIC_BUNDLE",
        "RNA_SEMANTIC_BUNDLE_BUILD",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let semantic_bundle_build = std::env::var("RNA_SEMANTIC_BUNDLE_BUILD").as_deref() == Ok("1");
    if semantic_bundle_build {
        let required = [
            ("PROFILE", "release"),
            ("TARGET", "aarch64-apple-darwin"),
            ("CARGO_FEATURE_EMBEDDINGS", "1"),
            ("CARGO_FEATURE_METAL", "1"),
            ("CARGO_FEATURE_SWEBENCH_SEMANTIC_BUNDLE", "1"),
            ("CANDLE_METAL_ENABLE_FAST_MATH", "1"),
        ];
        for (name, expected) in required {
            let actual = std::env::var(name).unwrap_or_default();
            assert_eq!(
                actual, expected,
                "SWE-bench semantic bundle requires {name}={expected}"
            );
        }
    }

    // The marker is compiled into RNA and checked by strict semantic search.
    // It is true only after the exact release/target/feature contract above.
    println!(
        "cargo:rustc-env=RNA_SEMANTIC_BUNDLE_BUILD={}",
        if semantic_bundle_build { "1" } else { "0" }
    );

    // Candle compiles its embedded Metal sources at runtime. The bundle build
    // therefore seals the actual kernel optimization input (fast math) in
    // addition to Cargo's release profile and the apple-m4 target above; a
    // made-up build-script flag would not affect dependency kernel code.
    println!(
        "cargo:rustc-env=RNA_METAL_KERNEL_PROFILE={}",
        if semantic_bundle_build {
            "release-fast-math"
        } else {
            "ordinary"
        }
    );
}
