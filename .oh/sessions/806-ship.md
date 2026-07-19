---
session: 806-ship
artifact_type: ship-session
issue: 786
pr: 806
updated: 2026-07-19
---
# Ship Pipeline — PR #806

## Pre-flight

- Repo-local `/ship`, `.claude/agents/ship.md`, the promoted
  `computed-but-not-delivered` guardrail, issue #786, PR #806, and existing
  comments were reviewed.
- The installed RNA CLI confirmed a live schema-v23 index with 45,134 symbols.
  New #786 APIs and LSP caller/reference coverage are unavailable, so impact
  confidence is explicitly partial and exact-diff fallbacks are logged.
- Protected user files and unrelated dirty/untracked state remain untouched.

## Step 1: RNA-Grounded Review

**Reviewed commit:** `85d31bbabbb96d2a0f409a3e6f242797502bbcbc`
**Verdict:** ADJUST

The review found pre-publication atomicity, terminal job bookkeeping, ordinary
scoped-enrichment compatibility, provisioned-LSP bundle, per-case semantic
delivery/timing, workflow-trigger, lint/policy, and README gaps. The invalid
artifact run was canceled before model execution. All findings are in one
consolidated remediation batch.

## Step 2: Independent Code Review

**Reviewer:** `/root/issue786_step2_independent`
**Reviewed commit:** `85d31bbabbb96d2a0f409a3e6f242797502bbcbc`
**Verdict:** REQUEST CHANGES

The fresh reviewer independently confirmed the post-publication asset-check
atomicity defect and the nonterminal `persisting` job defect. Both are included
in the same consolidated remediation batch; this early reviewer cannot satisfy
the later exact-final-diff approval gate.

## Verification completed before remediation

- `cargo check --lib`: pass.
- `cargo check --lib --features metal`: pass.
- Focused semantic Rust tests: 17 passed.
- Combined/artifact Python tests: 12 passed.
- Full default library suite: 2,213 passed, 4 ignored.
- Exact-head Rust CI test job, LSP contract, and PR checklist passed; lint and
  RustSec feature-scope policy produced bounded remediation findings.

## Consolidated remediation

- Delayed immutable-generation pointer publication until every final runtime
  asset check succeeds; a late failure now preserves the prior pointer.
- Made every post-persistence readiness failure terminal in both live embedding
  status and the durable job ledger.
- Preserved ordinary scoped enrichment when an immutable generation is active,
  while sealed/repository qualification continues to require full graph-aware
  reconciliation.
- Reprovisioned the sealed LSP component into a private verified offline root
  instead of treating artifact cache contents as an executable installation.
- Added fresh-process strict hybrid/RRF/rerank, graph traversal, full/minified
  body, repeatability, TTFE, timing, and peak-memory evidence to each combined
  case receipt and immutable archive.
- Bound actual combined-archive time outside the immutable core, expanded the
  workflow filters to cover the verifier/tests, fixed exact CI lint/policy
  findings, and documented normal branch-switch semantic reuse in README.

## Verification after consolidated remediation

- Combined qualifier/verifier Python tests: 13 passed.
- `cargo check --lib --features metal`: pass.
- Full default library suite before adversarial remediation: 2,215 passed, 4
  ignored.
- `cargo clippy --no-default-features -- -D warnings`: pass.
- Live RustSec policy check: pass with zero vulnerabilities and the intentional
  pre-existing warning still declared.
- `git diff --check`: pass.
- No model download, embedding, reranking, LSP scan, paid API, or benchmark was
  run locally. Runtime proof remains gated on the successful exact-head CI
  artifact.

## Step 4 adversarial remediation

The adversarial gate found two fail-closed gaps before the queued CI artifact
ran. The queued jobs were canceled before model execution, and both findings
were folded into this same remediation commit:

- Case 2 now must select and publish against the exact latest verifier-clean
  cold case-1 combined archive. Null, wrong-base, or zero-reuse/cold lineage
  cannot unlock case 3.
- `current.json` rename is the explicit commit point. All reportable errors
  happen before rename; a post-commit directory-sync error cannot falsely report
  failure after changing the active pointer.

Final bounded verification after those changes:

- Combined qualifier/verifier Python tests: 13 passed, including positive
  lineage and null/wrong/cold negative coverage.
- Pointer commit-point regression: pass.
- `cargo check --lib --features metal`: pass.
- Full default library suite: 2,216 passed, 4 ignored.
- `git diff --check`: pass.
