---
date: "2026-07-15"
pipeline_issue: "/ship PR #744"
pr: 744
phase: ship
---

# PR #744 ship friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA manifest search | minor | Manifest nodes exposed dependency names but did not deliver the exact selected constraint values needed for the coherent Lance/Arrow migration. | Used one targeted `rg` against `Cargo.toml` to identify the four direct constraints. | Deliver manifest dependency constraints and resolved versions through RNA search. |
| 2026-07-15 | Clean dependency test build | moderate | The first Lance 7 test build consumed the remaining volume while compiling a fully refreshed DataFusion/Lance stack. | Preserved unrelated targets, completed the 19 storage tests, then removed only this worktree's 12 GiB disposable test cache before the MSRV check. | Size dependency-refresh targets before starting and disable debug/incremental data for compatibility-only checks. |
| 2026-07-15 | Rust 1.97 `cargo fmt --all -- --check` | minor | The repository has broad pre-existing formatter drift in untouched Rust files. | Verified the two changed Rust files directly with Rust 1.97 `rustfmt --edition 2024 --check` and left unrelated files unchanged. | Normalize repository-wide formatting in a dedicated change if it becomes a required gate. |
