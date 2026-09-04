## Ship Pipeline — PR #866
**Started:** 2026-09-04

### Pre-flight
PR #866 / issue #865, branch `865-cuda-embeddings`, head `c5462e5`. Worktree index is live via the existing debug RNA binary; installed CLI was unavailable and richer LSP/semantic lanes are degraded.

### Step 1: RNA-Grounded Review
**Verdict:** ADJUST
**Findings:** 5
- Configured embedding settings are not passed into the bulk indexing model path.
- MCP/readiness diagnostics are not visibly wired through the changed code.
- CUDA attestation/probe does not establish device execution telemetry for real indexing/search.
- `validate` contains an empty conditional and does not enforce the stated explicit-CUDA strictness invariant.
- ORT/fastembed feature/version compatibility and CUDA build/runtime behavior are unverified.
## Friction log

| Event | Severity | Detail |
|---|---|---|
| 2026-09-04 | skipped | `repo-native-alignment` CLI unavailable on PATH during mandatory RNA scan gate; GitHub/standard filesystem inspection used instead. |
| 2026-09-04 | skipped | `gh pr diff --stat` is unsupported by installed GitHub CLI; used supported diff/status queries instead. |
| 2026-09-04 | skipped | Worktree copy of `.oh/metis/computed-but-not-delivered.md` is absent; the repository instruction and PR Step 1 copy supplied the applicable guidance. |

### Step 3: Fix
Fixed configured reindex execution, added runtime diagnostics through embedding status, operation reports, search, and list_roots, corrected semantic-bundle type inference, and declared the `cuda` RustSec reachability scope. Default-feature compile and full no-default test suite pass. Embeddings-feature compile is blocked on host OpenSSL development libraries; no CUDA execution claim is made.
