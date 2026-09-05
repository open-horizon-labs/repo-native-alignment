---
ship_pr: 866
issue: 865
---

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
| 2026-09-04 | skipped | MCP smoke script could not start because local `@modelcontextprotocol/sdk` dependency is not installed; exact-head CI MCP smoke remains required. |
| 2026-09-05 | skipped | Mandatory RNA scan could not run because the `repo-native-alignment` CLI is not installed or exposed in this session; source inspection proceeded only after recording this fallback. |

### Step 3: Fix
Fixed configured reindex execution, added runtime diagnostics through embedding status, operation reports, search, and list_roots, corrected semantic-bundle type inference, and declared the `cuda` RustSec reachability scope. Default-feature compile and full no-default test suite pass. Embeddings-feature compile is blocked on host OpenSSL development libraries; no CUDA execution claim is made.

### Step 4: Regression Oracle
Added a real temporary-repository configuration test covering backend, CUDA ordinal, fallback, and batch parsing; removed the misleading empty CUDA validation branch. Existing full no-default tests pass; embedding-feature test execution remains blocked by host OpenSSL prerequisites.

### Step 10b: Final comment sweep (in progress)
CodeRabbit findings verified. Fixed stale session wording, added session frontmatter, made configuration I/O fail closed, bounded CUDA ordinal conversion, moved embedding runtime rendering into the shared roots service, and resolved the effective backend/provider/device before semantic identity and vector reuse planning. NUC execution evidence remains unavailable; no CUDA execution claim is made.

### Step 10c: Independent final-diff review
Blocked: no fresh sub-agent/repo-local review worker is callable in this task context. Required approval for exact head `1d65c9b8677c6bf4c2a793ebd435c66bcbdb2674` was not fabricated. PR remains open; NUC RTX execution evidence is also outstanding.

### Fresh ship run — current head
The current-head CodeRabbit stability finding was verified: `list_roots` collapsed generation-load errors into `not_attested` and non-embedding builds omitted runtime availability. Fixed both paths and added a regression assertion. Targeted no-default test passes; full validation and a fresh exact-head Step 10c review remain required.

### Step 7a: Manual verification
Full no-default tests and default compile passed. `nvidia-smi` sees an RTX 3060 Ti, but no CUDA/cuDNN runtime libraries or `nvcc` are available; this is hardware presence only, not execution evidence. Embedding-feature build and MCP smoke are blocked by missing OpenSSL development files and local MCP SDK package respectively.

### Resume after conflict resolution
Post-conflict review began at requested head `e869bcf66e367d5557a7e406f3892cafcc7b391a`. A concurrent regression-test commit advanced the branch to `96256327076158f41b850c4e1fb25856f840b2db`; all subsequent exact-head gates must use that SHA. Step 1 was re-posted as ADJUST with three concrete findings. Step 2 is blocked because no fresh sub-agent/repo-local review worker is callable in this session; no approval was fabricated.

### Step 10c remediation evidence

Fresh Step 10c findings are addressed on exact head `282527b`: auto with
`fallback=error` exhausts CUDA and Metal before rejecting CPU; explicit CUDA
remains strict; CUDA identity is created only after the retained production
fastembed TextEmbedding session performs a real embedding probe. The
CUDA-feature production-session test passed on the NUC RTX 3060 Ti after
installing missing disposable CUDA runtime libraries. The real MCP TypeScript
client smoke passed against the exact-head binary and disposable fixture,
including readiness/status via `list_roots`. CI lint, test, audit, Analyze,
changes, checklist, and CodeRabbit checks are green; smoke/build-release remain
skipped by workflow policy.
