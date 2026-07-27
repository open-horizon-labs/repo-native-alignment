# Issue #836 canonical benchmark and faithful T2 addendum

This directory publishes the reviewable method and results for the completed
RNA-context benchmark associated with issue #836 and PR #837.

The original canonical analysis contains 20 SWE-bench Verified cases and four
conditions per case:

- `A_sonnet`: standard task, Claude Sonnet 5;
- `T_sonnet`: the same task plus deterministic pre-injected RNA context;
- `A_luna`: standard task, GPT-5.6 Luna through Codex App Server; and
- `T_luna`: the same deterministic RNA treatment with Luna.

The additive, post-hoc T2 execution contributes two more conditions per case
without replacing any original evidence:

- `T2_sonnet`: the same treatment scaffolding with a faithful unfiltered
  `neighbors`, two-hop, bidirectional typed-graph projection; and
- `T2_luna`: the byte-identical T2 prompt with Luna.

Read [METHOD.md](METHOD.md) before interpreting the original [REPORT.md](REPORT.md)
or additive [T2_REPORT.md](T2_REPORT.md). `results.json` and `t2-results.json`
are the path-free machine-readable ledgers used by `verify_results.py` and
`verify_t2_results.py`. Their separate evidence manifests bind the review
artifacts to retained external evidence without committing provider
transcripts, checkouts, caches, or credentials.

This is the canonical repaired analysis, not a claim that every abandoned
predecessor harness attempt was valid. Attempts affected by prompt mismatch,
runtime contamination, artificial timeouts, or restrictive wrappers are
provenance only and are not included as experimental observations.

Verify the checked-in package without model, provider, evaluator, or network
calls:

```bash
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_results.py

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_t2_results.py
```

That mode recomputes aggregates, parity, effects, matrix/tool rows, registered
order and budget, and fail-closed package digests. Where the retained evidence
root is available, the two commands also verify all nine original and 26 T2
external artifacts by content:

```bash
ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_results.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_t2_results.py
```
