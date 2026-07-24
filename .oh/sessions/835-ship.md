# Ship Pipeline — PR #835

**Started:** 2026-07-24T02:22:00Z
**Record prepared:** 2026-07-24T02:36:10Z

## Step 1: RNA-Grounded Review

**Verdict:** CONTINUE

The provider parent receives read access to only the exact regular
`~/Library/Keychains/login.keychain-db` file on macOS. It receives no Keychain
write root. `claude auth status --json` runs inside the final episode Seatbelt
before treatment acquisition or model launch, and retains only sanitized
zero-spend status.

RNA exact and broadened queries did not index the changed benchmark modules.
The exact diff, focused tests, and real Seatbelt execution were used as the
bounded authority. No graph metadata or Rust input changes.

## Step 2: Independent Code Review

**Reviewer:** `/root/pr835_auth_review`
**Reviewed commit:** `f0a08ff0352f28f8ffe03614c93a2a63e2731bde`
**Verdict:** APPROVE

The initial review requested portable historical verification, exact `--json`
argv, and a real retained read-only proof. All three were fixed and rechecked.

## Step 3: Fix

- Historical #830 source identity is verified through the immutable
  qualification-closure digest without requiring shallow-missing Git objects.
- The exact auth command includes `--json`.
- A real macOS probe reads the exact Keychain file, cannot open it for append,
  and retains no Keychain bytes or digest.

## Step 3b: Ready for Review

PR #835 was marked ready. Rust jobs are not gating because no Rust CI or
artifact-producing input changed.

## Step 4: Regression Oracle

- issue827: 145/145 passed.
- evaluator tools: 11/11 passed.
- issue830: 4/4 passed.
- Real exact-Keychain read/unwritable Seatbelt probe passed.

## Step 5: Merit Assessment

**Verdict:** MERGE

The change removes the proven cause of the failed pilot and prevents paid model
launch when the exact sandbox cannot authenticate.

## Step 6: Resolve TODOs

No TODO, FIXME, or HACK was added. Twenty-case count generalization and terminal
aggregation are owned by #836.

## Step 7a: Manual Verification

Retained proof:
`/Users/muness/swebench-evidence/issue834-auth-preflight-f0a08ff0-attempt-002/real-provider-auth-preflight.json`

SHA-256:
`a6fcb5bdafadbb8369e9d8767b4c518b31fa08eddfd906a528a5728ac362eb8a`

Result: logged in, Keychain unwritable, zero model/provider calls, zero cost,
and no credential bytes, digest, email, or organization identifier retained.
Failed setup attempt 001 remains preserved.

## Step 7b: Delivery Verification

No MCP or graph metadata changes; computed-but-not-delivered is not implicated.

## Step 8: README

No user-facing RNA capability documentation change is required. The operational
contract and next experiment are recorded in #834 and #836.

## Steps 9-10: Smoke and CI

The exact Python suites and real zero-spend sandbox check pass. Relevant
non-Rust CI is authoritative. No Rust build is dispatched or awaited by the
coordinator.

## Step 10b: Final Comment Sweep

**Verdict:** CLEAR

All independent review findings were fixed and approved. CodeRabbit was neither
triggered nor awaited.

## Step 10c: Independent Final-Diff Review

Pending on the exact commit containing this ship record. The authoritative
approval will be the fresh reviewer's PR comment; no diff change is permitted
after it.
