---
type: session
pr: 709
status: superseded
started: 2026-07-13
---

# Ship Pipeline — PR #709
**Started:** 2026-07-13

## Pre-flight

- PR: #709 `Draft: Add repo-local knowledge graph extension slice`
- Branch: `feat/local-knowledge-graph`
- Issues: closes #707; originally targeted #708.
- Delivery path: local branch -> closed draft PR review -> reopen only if ship review clears the supersession concern -> ready PR/CI/CodeRabbit -> merge to `main` -> successful CI artifact install -> MCP delivery verification -> release review.
- RNA scan gate: live local cache with 91,219 symbols.
- Initial CodeRabbit state: review skipped while draft; no inline findings before this ship run.
- Delivery-state concern: PR #709 and issue #708 were intentionally closed as superseded by the content-native plan in #711-#715. PR #710 remains open for the reusable #707 custom-edge infrastructure. Ship review must determine whether this branch still merits delivery without reviving the rejected frontmatter-first product framing or duplicating #710.
- Working-tree scope: four reviewed implementation/learning files plus this required ship session. Existing untracked `.codex/` and `.worktrees/` directories are excluded.

### Step 1: RNA-Grounded Review

**Verdict:** PAUSE

- The original #707 and #708 acceptance criteria are mechanically satisfied after the metadata-rendering and strict-boundary fixes.
- Delivery authority is unresolved: #708/#709 were deliberately superseded by #711-#715, while open PR #710 owns overlapping #707 custom-edge infrastructure.
- Frontmatter relationships remain declaration infrastructure, not content-native evidence; shipping them as the content source of truth would reverse the #711 framing.
- RNA friction: incremental persistence failed but exited 0, and repo-scoped impact was polluted by nested worktree symbols. Details are recorded in `.oh/friction-logs/709-ship.md`.
- PR comment: https://github.com/open-horizon-labs/repo-native-alignment/pull/709#issuecomment-4964812930

### Step 2: Independent Code Review

**Verdict:** REQUEST CHANGES

- Frontmatter can promote relationship declarations, including `confirmed` confidence, without body evidence; this is the mechanism superseded by #711-#715.
- Malformed or incomplete declarations can disappear or downgrade without diagnostics, creating false-green scans.
- `target.file` is not resolved or normalized, so relative paths can produce dangling graph edges.
- The reusable #707 custom-edge infrastructure should remain on one authoritative delivery path rather than being duplicated between #709 and #710.
- PR comment: https://github.com/open-horizon-labs/repo-native-alignment/pull/709#issuecomment-4964828497

### Pipeline State

**Paused before Step 3.** The ship gate requires an explicit delivery-target decision. Commit signing is also blocked because the configured 1Password SSH signing agent has no available identity; no unsigned commit was created.

## Execute — Authoritative Delivery Route

- PR #709 remains closed; it was not reopened, pushed, or reframed as the content-native solution.
- PR #710 remains the authoritative #707 custom-edge infrastructure path.
- CodeRabbit's regression concern was addressed on #710 in signed commit `a3e8f19`: the generic `References` workaround now shares the claimant source, unfiltered traversal proves the workaround is present, and a custom-edge filter must still isolate `supports`.
- Verification in the clean `issue/707` worktree: targeted custom-edge tests passed; full `cargo test --lib` passed with 1,909 tests and 2 ignored.
- The content-native worktree was not edited because it contains broad uncommitted work in the affected files. Inspection showed it already keeps boundary kinds strict and renders body-span evidence with validation status, so the old `rna.metadata.*` renderer is not the governing delivery mechanism there.
- PR #710 CI was retriggered at run `29335892881`. Both `lint` and `test` failed before compiling RNA because the moving `stable` toolchain installed Rust 1.97.0, which cannot compile locked transitive dependency `ethnum 1.5.2` (`lancedb -> lance -> jsonb -> ethnum`). The repository declares Rust 1.91 and the exact Clippy command plus the full library suite pass locally on Rust 1.92. This is a repository-wide dependency/toolchain compatibility issue, not a regression in the test-only #710 commit; resolving it requires a separate delivery decision.
- Delivery friction: normal HTTPS push hung in the macOS Keychain credential helper. The signed commit was pushed without changing global configuration by using the existing `gh` credential helper for that command only.

## Delivery Override — 2026-07-14

- The user explicitly directed that the remaining local-knowledge slice be merged after the reusable custom-edge infrastructure landed in #710.
- This overrides the earlier product-direction pause, but does not include unrelated `.codex/` or `.worktrees/` content.
- Delivery is being rebased onto current `main` through a clean follow-up PR; commits are intentionally unsigned per user instruction.
