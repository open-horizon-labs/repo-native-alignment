# Ship Pipeline — PR #799
**Started:** 2026-07-17

## Scope

Ship only issue #784: persisted, fail-closed per-file LSP completeness reporting and pre-model readiness. Server provisioning/descriptors and the passing frozen N=70 fleet run remain #785.

### Step 1: RNA-Grounded Review
**Verdict:** ADJUST
**Index:** refreshed extraction-only to 41,239 symbols, schema v23; exact search ready, LSP caller/reference coverage unavailable.

The review found seven bounded #784 seams: current graph/server identity was not revalidated on reopen; full-scan durable job validation/lifecycle evidence was not consumed; document-symbol node output was absent from persistence proof; the aggregate accepted arbitrary reports and a caller-selected N; disabled extract-only scans were incorrectly failed; relative aggregate output paths failed; and MCP could claim covered files while compatibility was stale.

### Step 2: Independent Review
**Verdict:** REQUEST CHANGES
**Comment:** https://github.com/open-horizon-labs/repo-native-alignment/pull/799#issuecomment-5007127952

The independent findings were consolidated with CI evidence before one remediation pass. No provisioning, descriptor work, model call, embedding run, benchmark episode, or #797 cleanup entered scope.

### Step 3: Consolidated Remediation
**Status:** COMPLETE

- Reopen now validates reported LSP stable IDs against the delivered graph and compares installed server version/digest with the persisted identity.
- Report construction consumes the exact related call-reference job's durable lifecycle/readiness validation while retaining scan-time work-item evidence in its independent ledger namespace.
- LSP node output, including document-symbol evidence, is included in result counts and stable-ID persistence checks.
- Aggregate input is a frozen manifest binding each report to instance ID, repository, and base commit; readiness is fixed at 70 distinct instances with no threshold override.
- Disabled extract-only full scans persist a blocked report for later inspection without failing the generic isolation rebuild.
- Relative aggregate paths and MCP compatibility coverage counts fail closed correctly.
- The real MCP smoke client asserts that search delivery contains the per-file completeness readiness block.

### Verification
**Status:** COMPLETE

- `cargo check --lib --bin repo-native-alignment --no-default-features` — pass
- `cargo clippy --lib --bin repo-native-alignment --no-default-features -- -D warnings` — pass
- completeness/MCP-format focused tests — 24 passed
- CLI readiness parse test — 1 passed
- prior failing disabled full-scan isolation test — 1 passed
- `cargo test --no-default-features` — pass: 2,095 lib + 6 bin + 7 integration tests; 4 lib tests and 12 doctests intentionally ignored
- `node --check .github/scripts/mcp-smoke.mjs` and `git diff --check` — pass
- protected dirty-file hashes exactly match preflight; protected/unrelated paths remain unstaged

### Exact-Head CI Follow-up

The automatic PR lint used Rust 1.97 and found one style-only `question_mark` diagnostic not emitted by the local Rust 1.91 Clippy. The exact suggested rewrite was applied without changing behavior; downstream exact-head review and CI were restarted after the repush.

No model, embedding runtime, paid API, or benchmark episode was launched. The exact N=70 fleet execution remains #785.

### Step 4: Adversarial Test
**Verdict:** FAIL on `e1c7ec4d`
**Comment:** https://github.com/open-horizon-labs/repo-native-alignment/pull/799#issuecomment-5007312745

The adversarial pass found four fail-open seams: absent related-job/language validation could still become processed; expected output was inferred only after mapping; processed-zero reports were not graph-snapshot-bound; and 70 arbitrary identities could satisfy the aggregate.

### Consolidated Adversarial Remediation
**Status:** COMPLETE

- Work-item schema v3 persists raw applicable result counts before graph mapping; nonzero raw output now requires persisted graph evidence.
- Missing related call-reference jobs or per-language durable validation fail closed, and requested operations no longer self-advertise server capability.
- Completeness schema v2 binds every report, including processed-zero reports, to the full graph snapshot digest.
- Aggregate readiness verifies the checked-in `population.json` against its existing `protocol.lock.json` SHA-256, requires the exact 70 included instance/repository/base-commit tuples, and requires each report to bind the matching repository and checkout.
- Four adversarial regressions cover those exact seams. No runner, fleet, model, embedding, credential, provisioning, or #797 cleanup work was added.

### Adversarial Remediation Verification
**Status:** COMPLETE

- completeness focused suite — 27 passed, 0 failed
- exact corrected regressions — 1 passed each
- `cargo clippy --lib --bin repo-native-alignment --no-default-features -- -D warnings` — pass
- `cargo test --no-default-features` — pass: 2,099 lib + 6 bin + 7 integration tests; 4 lib tests and 12 doctests intentionally ignored
- `git diff --check` — pass
- protected dirty-file hashes exactly match preflight; protected/unrelated paths remain unstaged
