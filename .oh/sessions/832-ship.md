# Ship Pipeline — PR #832

**Started:** 2026-07-24T01:52:00Z
**Final record prepared:** 2026-07-24T02:10:06Z

## Step 1: RNA-Grounded Review

**Verdict:** ADJUST, then CONTINUE after explicit deferral
**Guardrails checked:** dogfood RNA tools, CI artifact identity, independent
final review, ship-step visibility, computed-but-not-delivered
**Findings:** 5

The retained run has exactly four unique one-shot episode receipts, but no
lawful protocol selection result: three provider launches reported
`Not logged in`, one treatment arm failed readiness identity before model
launch, and the frozen verifier reports 0/4 clean and 0/4 compliant. The
general credential-liveness and terminal-aggregation defects are deferred to
#834 without permission to mutate or retry attempt 019. #817 is unauthorized
by this pilot.

RNA exact search was available but did not index the new issue827/issue830
selector modules. Broader queries confirmed the graph gap; exact diff, tests,
and immutable evidence were used as the bounded fallback.

## Step 2: Independent Code Review

**Reviewer:** `/root/ship_step2_review`
**Reviewed commit:** `de8753126b8a7f4f0f421f7908cc76a9c2f09de4`
**Verdict:** APPROVE

The reviewer approved the truthful immutable stop and #834 deferral, while
explicitly marking the four-paid-episode and valid-protocol-selection
acceptance criteria as terminally not achieved. This is not the final-diff
approval.

## Step 3: Fix

No attempt-019 evidence or registered runtime source was changed. Read-only
diagnosis established that the outer Seatbelt allowed Claude config/state paths
but denied `~/Library/Keychains`, where the already-existing host login is
stored. #834 owns the future-only Keychain scope and zero-spend in-sandbox auth
preflight.

## Step 3b: Ready for Review

PR #832 was converted from draft to ready. Non-Rust gates passed. Automatic
Rust jobs are not gating because no Rust CI or artifact-producing input changed;
they were not awaited.

## Step 4: Regression Oracle

- Narrow Seatbelt harness: 31/31 passed.
- Real macOS exact-file scope: 2/2 passed.
- Final Python suites: issue827 141/141, evaluator 11/11, issue830 3/3.
- Retained production replay attempt 020 passed with two RNA searches and zero
  model, evaluator, scan, LSP, embedding, or reranking work.
- Exact-head SWE-bench harness CI passed.

## Step 5: Merit Assessment

**Verdict:** MERGE WITH CAVEATS

The PR delivers the fail-closed selector/evaluator/isolation machinery and an
honest pilot, but it does not estimate treatment effect. The failed pilot is
retained as evidence; the expanded 20-case experiment proceeds only after #834.

## Step 6: Resolve TODOs

No added TODO, FIXME, or HACK remains. #834 owns the actionable protocol
hardening. Static unused-import/style notes are nonfunctional and remain
unchanged to preserve the executed source identity.

## Step 7a: Manual Verification

Production replay passed before launch. Attempt 019 created exactly four unique
receipts in frozen order with no retry, no qualifying patch, and zero evaluator
authorization/invocation. The verifier aggregate failed closed at 0/4 clean and
0/4 compliant. The separate stop record does not claim to be a protocol
selection result.

## Step 7b: Delivery Verification

No graph metadata was added. Successful artifact run `30058883487`, artifact
`8583861722`, digest
`sha256:b3652d2879f7103f20eb7ff8121eba572c8c1f0f0e9982251ae195a1067268a7`
passed corrected CLI and real MCP smoke. The artifact is unexpired.

## Step 8: README

README coverage exists for the issue827 harness and issue830 successor path.
The final failure and future remediation are recorded on PR #832 and issue #834.

## Steps 9-10: Smoke and CI

PR checklist, CodeQL Python analysis, and the SWE-bench RNA-first harness are
green. Since source head `a519a81023023cfec809ef255e3ce9fc723b864b`,
later changes contain no Rust CI or artifact-producing input. The successful
artifact is reused; no Rust build was dispatched by the coordinator or awaited.

## Step 10b: Final Comment Sweep

**Verdict:** CLEAR

All external findings were classified. Stale signature findings are false
positives, broad exception boundaries are intentional fail-closed evidence
containment, and remaining import/style notes are nonfunctional. No unresolved
critical or major actionable finding remains. CodeRabbit was neither triggered
nor awaited.

## Step 10c: Independent Final-Diff Review

Pending on the exact commit containing this ship record. The authoritative
approval will be the fresh reviewer's PR comment; no diff change is permitted
after that approval.
