# Ship Pipeline — PR #723

## Pre-flight

- PR: #723 `Adopt Rust 1.97 and patch vulnerable dependencies`
- Branch: `codex/issue-722-rust-197`
- Issue: #722
- Follow-up debt: #724
- Delivery path: merge this prerequisite before refreshing PR #710.
- Required project context: injected `AGENTS.md`, `/execute`, `/ship`, `.claude/agents/ship.md`, and the `computed-but-not-delivered` guardrail.
- RNA scan gate: live worktree index with 112,633 symbols; targeted source impact queries completed.
- Delivery verification classification: N/A because the PR changes compiler/dependency selection and no MCP-visible data.

## Execute verification

- Rust 1.97 `cargo check --lib --no-default-features`: pass.
- Rust 1.97 `cargo clippy --no-default-features -- -D warnings`: pass.
- Rust 1.97 `cargo test --no-default-features`: pass; 1,906 tests passed, 2 ignored.
- Rust 1.91 `cargo check --no-default-features`: pass; declared MSRV retained.
- `cargo audit`: zero vulnerabilities; warning-only upstream debt tracked in #724.
- Workflow YAML: parses successfully; seven Rust action references pinned to 1.97.0; no moving `stable` reference remains.
- Regression oracle: `.github/scripts/check-rust-toolchain-pins.sh` makes workflow/toolchain drift fail in CI and passes locally across all seven references.
- Formatting: changed Rust lines pass Clippy; repo-wide rustfmt 1.97 check is blocked by unrelated baseline drift logged in the friction table.

## Review evidence

- RNA review concerns: duplicate workflow pins, the direct `git2` breaking-version update, broad lockfile movement, and residual advisory warnings.
- Resolutions: a CI pin-consistency oracle; no RNA evidence of git2 remote/SSH operations; Rust 1.97 full tests plus Rust 1.91 MSRV check; remaining upstream warning debt isolated in #724.
- Independent reviewer verdict: ready, with no actionable findings. The reviewer confirmed both source migrations preserve behavior and used RNA impact/caller evidence without running a parallel Cargo build.
- Merit: the change restores the blocked build, makes compiler selection reproducible, removes five known vulnerabilities, and prevents silent workflow/toolchain drift. A lockfile-only ethnum bump would not meet that outcome.
- TODO audit: no unresolved TODO appears in the change; #724 and the post-merge refresh of #710 are explicit follow-ups.
- README: no change required. User installs consume CI/release artifacts, while source developers already enter through the checked-in toolchain file and the README links rustup.

## Ship status

- [x] Step 1: RNA review
- [x] Step 2: independent review
- [x] Step 3: fixes
- [ ] Step 3b: ready for review
- [x] Step 4: regression oracle
- [x] Step 5: merit assessment
- [x] Step 6: TODO resolution
- [x] Step 7a: manual verification
- [x] Step 7b: delivery verification (N/A: no agent-visible data)
- [x] Step 8: README (no change required)
- [x] Step 9: full tests
- [ ] Step 10: CI green
- [ ] Step 10b: final comments
- [ ] Step 11: merge
