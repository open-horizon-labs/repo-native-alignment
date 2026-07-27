# Issue #836 canonical 20-case × 4-condition benchmark

This directory publishes the reviewable method and results for the completed
RNA-context benchmark associated with issue #836 and PR #837.

The final analysis contains 20 SWE-bench Verified cases and four conditions
per case:

- `A_sonnet`: standard task, Claude Sonnet 5;
- `T_sonnet`: the same task plus deterministic pre-injected RNA context;
- `A_luna`: standard task, GPT-5.6 Luna through Codex App Server; and
- `T_luna`: the same deterministic RNA treatment with Luna.

Read [METHOD.md](METHOD.md) before interpreting [REPORT.md](REPORT.md).
`results.json` is the path-free machine-readable ledger used by
`verify_results.py`. `evidence-manifest.json` binds these review artifacts to
the retained external evidence without committing provider transcripts,
checkouts, caches, or credentials.

This is the canonical repaired analysis, not a claim that every abandoned
predecessor harness attempt was valid. Attempts affected by prompt mismatch,
runtime contamination, artificial timeouts, or restrictive wrappers are
provenance only and are not included as experimental observations.

Verify the checked-in package without model, provider, evaluator, or network
calls:

```bash
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_results.py
```

That mode recomputes aggregates, parity, effects, matrix/tool rows, registered
order and budget, and fail-closed package digests. Where the retained evidence
root is available, verify all nine external artifacts by content as well:

```bash
ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_results.py
```
