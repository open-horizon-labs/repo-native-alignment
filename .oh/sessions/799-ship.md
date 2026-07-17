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

No model, embedding runtime, paid API, or benchmark episode was launched. The exact N=70 fleet execution remains #785.
