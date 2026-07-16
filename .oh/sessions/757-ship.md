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

## Independent review follow-up

Fresh independent review found two P1 lifecycle gaps:

1. third-file evidence edits were not revalidated in live incremental graph paths or scheduled for persistence;
2. selectors were ambiguous when multiple workspace roots shared the same relative path/body-node ID.

Both are fixed:

- `EvidenceSelector` now carries root identity; legacy unscoped selectors work only for a unique cross-root match and otherwise fail closed as unresolved/detected.
- Root identity participates in stable evidence identity and rendered locations.
- Foreground fast snapshots, their background full pipeline, `update_graph_with_scan`, and the multi-root background scanner all revalidate after node merge/dedup.
- Revalidation returns changed same-ID edges, which replace stale persistence upserts.
- New regressions prove root isolation, ambiguous legacy fail-closed behavior, and a third-file edit downgrading both the live edge and persistence delta.
- Independent re-review verdict: APPROVE.
- Strict Clippy: pass.
- Clean full suite: 2020 library tests passed, 4 ignored; binary, CLI contract, content-source contract, and doc tests passed.

## 2026-07-16 current-main review fixes

Fresh independent review of `e64a1b32` found two final trust gaps:

1. selectors with correct hashes/ranges could retain and render an unrelated
   producer-supplied `snippet`;
2. custom edges could remain valid/confirmed without nonblank extractor, rule,
   and pack provenance.

Both are fixed:

- successful hash/range validation now refreshes display snippets from the
  matched current body node before MCP rendering;
- `EdgeKind::Other` evidence fails closed as `Invalid`/`Detected` unless
  extractor, rule, and pack identifiers are all present and nonblank;
- generic evidence remains compatible with `pack_id: None`;
- unit regressions cover correct-hash/wrong-snippet refresh and custom versus
  generic provenance behavior;
- the LanceDB round-trip/render regression begins with an untrusted snippet and
  proves load-time validation replaces it with current source text.

Verification:

- focused evidence tests: 9 passed;
- `cargo check --lib`: pass;
- `cargo clippy --no-default-features -- -D warnings`: pass;
- full suite excluding the known checkout-history/cache-sensitive
  `test_list_roots_from_slugs_lsp_stats_per_language`: 2,043 library tests,
  5 binary tests, CLI exit contract, content-source contract, and doc tests
  passed;
- the excluded test's ambient live-operation-report failure is unchanged and
  unrelated to this diff; clean CI remains authoritative for it.

Independent re-review of `3516d7ca` found one P1 identity regression: the
display-only snippet refresh changed `Edge::stable_id()` because the full
selector was hashed. The stable identity projection now excludes `snippet`
while retaining root/file/line/byte/body-node/hash coordinates and provenance.
The correct-hash/wrong-snippet regression also asserts that validation refreshes
display text without changing durable edge identity. Focused evidence tests
remain 9/9 passing.

Final independent re-review of `7fcea80c`: **APPROVE**. The reviewer confirmed
the identity projection excludes only display text while retaining ordered
authoritative selector coordinates/hash and extractor/pack/rule provenance.
Final local verification at that head passed: focused evidence tests, library
check, strict no-default-features Clippy, 2,043 library tests (excluding the
known live-cache-sensitive roots test), 5 binary tests, CLI exit contract,
content-source contract, and doc tests.
