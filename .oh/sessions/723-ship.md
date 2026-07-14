---
session: 723-ship
issues: [722, 724]
pr: 723
branch: codex/issue-722-rust-197
date: 2026-07-14
tags: [context-assembly, dependency-refresh, rust-toolchain, ship]
---

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

## Expanded full compatible dependency refresh

- User expanded scope after the first ready-for-review transition; PR returned to draft and the ship gate was reset.
- `cargo update` refreshed the entire lockfile under declared semver and Rust 1.91 constraints: 211 packages moved.
- Major runtime surfaces refreshed include Arrow, fastembed/ORT, Tokio, rustls, QUIC, tree-sitter grammars, OpenSSL, serde, regex, and the DataFusion support graph.
- RustSec audit after the full refresh: zero vulnerabilities and three warning-class advisories. The yanked/unmaintained `core2` entry was removed; `lru`, `paste`, and `number_prefix` remain tracked in #724.
- Rust 1.97 `cargo check --lib --no-default-features`: pass after the expanded refresh.
- Rust 1.97 `cargo clippy --no-default-features -- -D warnings`: pass after the expanded refresh.
- Rust 1.97 full test suite on a clean target: 1,906 executable tests passed, 2 ignored, 0 failed.
- Rust 1.91 `cargo check --no-default-features`: pass after the expanded refresh.
- Rust 1.97 `cargo check --features embeddings`: pass, covering refreshed fastembed 5.17.2, ORT rc.12, model formats, and their HTTP/download graph.
- Rust 1.97 `cargo check --features metal`: pass, covering the Apple Silicon Candle/metal-candle source-build path.
- Rust 1.97 ignored `test_rerank_integration` with `--features embeddings`: pass, exercising model loading, ORT session creation, inference, and ranking on the refreshed fastembed/ORT runtime.
- Direct incompatible-version inventory is explicit rather than silently migrated: Arrow/Lance, Candle, rust-mcp-sdk, TOML, SQL parser, and two tree-sitter grammars require separate API migrations beyond `cargo update`.
- Warning reachability was rechecked with feature-aware dependency trees: `number_prefix` is active only for embeddings/metal through metal-candle's older hf-hub/indicatif chain, while `lru` remains active through Tantivy/Lance. Issue #724 records both exact parent paths.

## Review evidence

- RNA review concerns: duplicate workflow pins, the direct `git2` breaking-version update, broad lockfile movement, and residual advisory warnings.
- Resolutions: a CI pin-consistency oracle; no RNA evidence of git2 remote/SSH operations; Rust 1.97 full tests plus Rust 1.91 MSRV check; remaining upstream warning debt isolated in #724.
- Fresh expanded-scope review readiness: ready, with no actionable findings. Raw diff plus compiler/audit evidence was sufficient for the lockfile and config changes; RNA mapped the two Rust compatibility hunks to their callers and tests.
- Fresh independent review found two verification blockers: the optional fastembed/ORT runtime seam and the 15-minute cold CI timeout. The reranker integration test passed; CI retains the full test command with a 45-minute safety budget and omits test debug symbols after later runs showed the link remained slow. The reviewer reported no remaining code, supply-chain, MSRV, or runtime findings.
- Merit: the change restores the blocked build, makes compiler selection reproducible, removes five known vulnerabilities, and prevents silent workflow/toolchain drift. A lockfile-only ethnum bump would not meet that outcome.
- TODO audit: no unresolved TODO appears in the change; #724 and the post-merge refresh of #710 are explicit follow-ups.
- README: no change required. User installs consume CI/release artifacts, while source developers already enter through the checked-in toolchain file and the README links rustup.

## Ship status after scope expansion

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
