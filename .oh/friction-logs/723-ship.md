---
date: "2026-07-14"
pipeline_issue: "/ship PR #723"
pr: 723
phase: ship
---

# PR #723 ship friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-14 | RNA `search("dtolnay/rust-toolchain")` | skipped | Exact workflow-action references in YAML returned no results. | Used a narrow `git grep` audit to prove all seven workflow pins agree and no `@stable` reference remains. | Improve exact-value indexing for workflow YAML. |
| 2026-07-14 | RNA worktree search and impact | minor | Repo-scoped results included duplicate symbols from sibling worktrees. | Stable worktree-qualified node IDs still allowed targeted impact analysis, but result ranking was noisy. | Keep worktree symbols isolated when a `repo` is supplied. |
| 2026-07-14 | `scripts/prep-worktree.sh` RNA prewarm | moderate | The inherited prewarm took several minutes before the worktree index became queryable. | Delayed pre-flight but did not require a fallback source scan. | Make scan progress and completion identity visible. |
| 2026-07-14 | Cargo test with hard-linked target and sccache | moderate | The first full test attempt lost a DataFusion dep-info file from the warmed cache and failed before tests executed. | Retrying in the same worktree with `RUSTC_WRAPPER=` rebuilt the affected artifacts and passed all tests. | Treat shared hard-linked compiler caches as expendable and retry without sccache on cache-integrity failures. |
| 2026-07-14 | Rust 1.97 `cargo fmt --all -- --check` | minor | The new formatter reports broad pre-existing drift across untouched files on `main`. | Avoided a repo-wide formatting rewrite in the prerequisite dependency PR; changed Rust lines are already formatted and Clippy passes. | Normalize formatting separately if the project wants Rust 1.97 rustfmt as a gate. |
| 2026-07-14 | Full dependency test rebuild | moderate | The mixed warm target had grown to 31.8 GiB and exhausted the volume while linking refreshed Lance/Tantivy test artifacts. | Cleaned only this worktree's expendable target and reran with incremental compilation disabled and four jobs; the clean target used about 10 GiB and all tests passed. | Keep dependency-refresh verification on a clean, bounded target rather than layering it onto a hard-linked cache. |
| 2026-07-14 | GitHub Actions test job | moderate | The full compatible refresh made the cold CI build exceed the job's 15-minute timeout; Actions canceled it at 15m15s while compiling `git2`, before tests ran. Later retries were still compiling at 28 and 34 minutes, showing that timeout headroom alone left an unnecessarily slow gate. | Kept a 45-minute safety budget and disabled test-profile debug symbols in CI via `CARGO_PROFILE_TEST_DEBUG=0`; the full test command and assertions are unchanged. | Keep the cold dependency-refresh path inside the CI budget; do not rely on a warm-cache rerun as proof. |
