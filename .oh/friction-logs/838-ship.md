---
title: Issue #838 resident runtime ship friction
date: 2026-07-30
issue: 838
outcome: context-assembly
---

# Friction Log: #838 Resident Runtime

| Phase/Step | Tool | What happened | Workaround | Severity |
|------------|------|---------------|------------|----------|
| Remediation source patching | RNA CLI exact search / body retrieval | RNA located every affected symbol, but its rendered Rust bodies normalized multiline source formatting, so the first exact `apply_patch` context did not match. | Used bounded source views and `rg` only in the already-identified files, then applied narrow patches. | low |
| Local JavaScript syntax check | host Node shim | The repository has no selected asdf Node version, so the `node` shim refused to run. | Invoked the already-installed Node 22.21.1 binary explicitly; the script parsed successfully. | low |
| Focused Rust regression | Cargo test filter | A short test name combined with `--exact` compiled the test binary but matched zero tests. | Re-ran the fully qualified test path; the intended cache-only external-repository regression passed. | low |
| Semantic verifier tests | macOS system Python 3.9 | The verifier uses `zip(..., strict=True)`, which Python 3.9 does not support. | Re-ran the complete verifier suite with the repository host's Python 3.12 runtime; all seven tests passed. | low |
| Repository formatting gate | `cargo fmt --all --check` | The branch base contains broad formatting drift outside the issue #838 diff. | Formatted only the seven changed Rust files with `rustfmt --edition 2024 --config skip_children=true`; `git diff --check` passes. | low |
| Feature Clippy gate | `cargo clippy --features embeddings,metal,swebench-semantic-bundle -- -D warnings` | The feature build reaches four pre-existing warnings in unchanged code (`too_many_arguments` and `needless_return`). | Verified the issue #838 feature build with `cargo check`, focused feature tests, and default Clippy with warnings denied; did not expand the fix into unrelated refactoring. | low |
