# PR #723 ship friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-14 | RNA `search("dtolnay/rust-toolchain")` | skipped | Exact workflow-action references in YAML returned no results. | Used a narrow `git grep` audit to prove all seven workflow pins agree and no `@stable` reference remains. | Improve exact-value indexing for workflow YAML. |
| 2026-07-14 | RNA worktree search and impact | minor | Repo-scoped results included duplicate symbols from sibling worktrees. | Stable worktree-qualified node IDs still allowed targeted impact analysis, but result ranking was noisy. | Keep worktree symbols isolated when a `repo` is supplied. |
| 2026-07-14 | `scripts/prep-worktree.sh` RNA prewarm | moderate | The inherited prewarm took several minutes before the worktree index became queryable. | Delayed pre-flight but did not require a fallback source scan. | Make scan progress and completion identity visible. |
| 2026-07-14 | Cargo test with hard-linked target and sccache | moderate | The first full test attempt lost a DataFusion dep-info file from the warmed cache and failed before tests executed. | Retrying in the same worktree with `RUSTC_WRAPPER=` rebuilt the affected artifacts and passed all tests. | Treat shared hard-linked compiler caches as expendable and retry without sccache on cache-integrity failures. |
| 2026-07-14 | Rust 1.97 `cargo fmt --all -- --check` | minor | The new formatter reports broad pre-existing drift across untouched files on `main`. | Avoided a repo-wide formatting rewrite in the prerequisite dependency PR; changed Rust lines are already formatted and Clippy passes. | Normalize formatting separately if the project wants Rust 1.97 rustfmt as a gate. |
