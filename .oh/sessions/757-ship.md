---
session: 757-ship
artifact_type: ship
outcome: context-assembly
updated: 2026-07-16
---

# Ship Pipeline — PR #757

## Pre-flight

- Issue #714; branch `issue/714`; draft PR #757.
- #712 and #713 prerequisites are merged on `main` and present on the branch.
- RNA CLI index was live for discovery; MCP tools were unavailable and exact-body fallbacks are logged in `.oh/friction-logs/757-oh-task.md`.
- Two unrelated untracked `.oh` files were preserved and excluded from commits.

## Step 1: RNA-Grounded Review

- Verdict: ADJUST, then CONTINUE after fixes.
- Checked `computed-but-not-delivered`, `no-linear-scan-on-graph`, content-source contract, schema migration, persistence/load, search/traversal rendering, and issue acceptance criteria.
- Findings fixed: formatting scope drift, O(edges × nodes) evidence revalidation, lifecycle state accidentally participating in edge identity, missing single-node traversal rendering, and direction-insensitive O(E × K) evidence rendering.

## Step 2: Independent Review

- Initial verdict: REQUEST CHANGES for reciprocal-edge direction and render complexity.
- Fixed with an O(E) direction-aware index and reciprocal-edge regression coverage; final re-review pending.

## Verification so far

- `cargo check --lib`: pass.
- `cargo test --lib`: 1998 passed, 3 ignored.
- Targeted identity, stale-on-edit, persistence/load/render, frontmatter-only, and reciprocal-direction tests: pass.
- Local optimized candidate passed the real TypeScript MCP smoke client (all four tools visible; all assertions passed).

## 2026-07-16 continuation

- Merged current `origin/main` at `5e6699f6`, incorporating shipped work through #768/#774.
- Resolved the sole semantic conflict in `src/extract/lsp/passes.rs` by retaining current LSP admission/telemetry behavior and adding empty evidence payloads to the seven new LSP edge constructors.
- Full-suite compilation found three additional current-main test-only `Edge` constructors; added explicit empty evidence payloads without changing test semantics.
- Preserved the two unrelated untracked `.oh` files unchanged.
- Post-merge `cargo check --lib`: pass.
- Post-merge `cargo clippy --no-default-features -- -D warnings`: pass.
- Post-merge `cargo test`: pass (2018 library tests passed, 4 ignored; 2 binary tests, CLI exit contract, content-source contract, and doc tests all passed).
- `cargo fmt --all -- --check` was not used as a branch gate because the installed formatter proposes repository-wide changes in untouched current-main files; no unrelated formatting rewrite was applied.
- Prior final-head CI and exact-artifact evidence are treated as stale; full tests, CI, CodeRabbit, and exact-artifact MCP verification will be repeated against the updated head.
