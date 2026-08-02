## Ship Pipeline — PR #859
**Started:** 2026-08-02 13:46 EDT
**Issue:** #858
**Initial reviewed head:** `5225ae58db2abb6ae4f0ea870f57d3603ced382c`
**Worktree:** `/Users/muness/src/open-horizon-labs/repo-native-alignment/.claude/worktrees/858`

### Pre-flight
- Read repo-local ship skill, full ship procedure, and repository instructions.
- Confirmed draft PR #859 closes issue #858.
- Full RNA scan completed: loaded 63,871 cached nodes and 215,073 edges; post-scan cache contains 67,872 symbols.
- Targeted artifact searches failed because duplicate exact-lexical candidates were returned; fallback recorded in `.oh/friction-logs/859-ship.md`.
- Existing review state: no inline comments; one non-actionable CodeRabbit draft-skip status comment.
- Handoff ownership audit found one unknown-provenance renderer test-fixture edit after the handoff SHA. It was preserved for independent validation as a Step 1 fix; the same-target Cargo process exited before this pipeline ran Cargo.

### Step 1: RNA-Grounded Review
**Verdict:** CONTINUE after ADJUST remediation
**Reviewed commit:** `75c0d218a51ed4eb67b79fa532413571a85bd120`
**Metis checked:** 2 entries
**Guardrails checked:** 6 entries
**Findings:** 4

1. The post-handoff metadata-compaction regression panicked on an empty relationship vector; preserved, independently validated, and fixed in `c44d54c3` (targeted test 1/1).
2. Flat-tail degradation discarded the omitted record's recovery handle; fixed in `75c0d218` by carrying a compact `rna-h2` handle through omission compaction (realistic flat-budget regression 1/1).
3. Candidate-derived required obligations can only certify seams surfaced upstream; the exact warmed-cattrs delivery packet must prove every issue obligation or return to Step 3.
4. Compact handle decode compatibility has unit coverage but requires real-client source/evidence hydration in Step 7b.

**PR evidence:** https://github.com/open-horizon-labs/repo-native-alignment/pull/859#issuecomment-5159627322

### Step 2: Independent Code Review
**Reviewer:** `/root/issue_858_ship/step2_review_b`
**Reviewed commit:** `4b00f60cbe5fe041c4e6185aad261ae554a93fbc`
**Verdict:** REQUEST CHANGES

- P1: generic test declarations with superficial multi-token affinity and graph-only decorators with one incidental query token can become Actionable.
- P1: real warmed-repository MCP delivery and matched unsteered trial remain unproven; this is resolved only by the prescribed Step 7b real-delivery gate, not by fabricating another synthetic fixture in Step 3.
- PR evidence: https://github.com/open-horizon-labs/repo-native-alignment/pull/859#issuecomment-5159652103

The first Step 2 reviewer self-disqualified before posting after `gh pr diff --exclude` leaked a forbidden session patch. The binding review came from a new distinct reviewer using pre-emission GitHub API filtering.

### Step 3: Fix
**Status:** complete

- Require actual test-function evidence before a Test-role candidate can be Actionable.
- Require two query-term matches for graph-only candidates when the query supplies at least two terms.
- Add negative regressions for a multi-term generic TypedDict fixture and superficially query-affine unrelated decorator.
- Step 2 delivery finding is assigned to mandatory Step 7b; completion evidence will be posted there before merge.

**Verification:** reviewer negative-quality regression 1/1 passed; integrated production-path cattrs packet regression 1/1 passed.

### Step 3b: Mark Ready
**Status:** complete
**Ready head:** `849b9ec50fa38d2f8456515503f543e3b7d39c73`

### Step 4: Regression Oracle
**Status:** complete
**Tests written/strengthened:** 11
**Exact linked-binary results:** renderer 15/15; task-context 18/18; hydration model 4/4; production-path issue filters 5/5.
**PR evidence:** https://github.com/open-horizon-labs/repo-native-alignment/pull/859#issuecomment-5159687696

### Step 5: Merit Assessment
**Initial verdict:** NEEDS MORE WORK

The installed pre-fix binary failed a real 20-result evidence query at 5,000 tokens with a 60,221-byte/15,017-token non-body response. The branch binary removed that failure but exposed `AccountingDidNotConverge`: flat reason compaction rewrote a long reason to another reason above its own 32-character terminal threshold. Returned to Step 3 for a terminal marker and regression before reassessment.

### Step 3 Re-entry: Merit Finding
**Status:** complete

- Shorten the flat selection-reason terminal marker below the compaction threshold.
- Add a 20-record regression with long reasons but no auxiliary verbose metadata, matching the real-query failure shape.
- Targeted terminal-marker regression passed; the only concurrent host Cargo process was verified to belong to a different repository and target directory.
- A real 20-result evidence query now returns a bounded 6,805-byte/1,701-token packet instead of either the prior 60,221-byte non-body failure or `AccountingDidNotConverge`; the terminal-state regression was tightened to prove the compact marker cannot re-enter degradation.
