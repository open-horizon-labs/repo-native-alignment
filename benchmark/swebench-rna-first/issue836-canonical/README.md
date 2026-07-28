# Issue #836 canonical benchmark

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

The additive weaker-model sensitivity execution contributes six more
conditions per case without replacing the Sonnet/Luna evidence:

- `A_haiku`, `T_haiku`, and `T2_haiku`: the byte-identical A/T/T2 user
  prompts on Claude Haiku 4.5 through the logged-in Claude CLI; and
- `A_spark`, `T_spark`, and `T2_spark`: the byte-identical A/T/T2 user
  prompts on GPT-5.3 Codex Spark through Codex App Server.

The HumanLayer/SlopCodeBench comparison adds 24 more conditions per case by
crossing the published `anti_slop` and `plan_first` prompt strategies with
A/T/T2 on all four models. These 480 cells are additive; the original 240 cells
remain unchanged. The final package therefore reports 36 conditions and 720
cells in one analysis.

Read [METHOD.md](METHOD.md) before interpreting the unified
[REPORT.md](REPORT.md), which reports all 36 conditions and includes per-case
metrics, quality measurements, and tool-type details. `results.json`,
`t2-results.json`, `weaker-model-results.json`, and
`humanlayer-strategy-results.json` are the path-free machine-readable ledgers
used by their corresponding verifier scripts. Their evidence manifests bind
the review artifacts to retained external evidence without committing provider
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

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_weaker_results.py

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_humanlayer_strategy_results.py
```

That mode recomputes aggregates, parity, effects, matrix/tool rows, registered
order and budget, and fail-closed package digests. Where the retained evidence
root is available, the four commands also verify the bound original, T2,
weaker-model, and prompt-strategy external artifacts by content:

```bash
ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_results.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_t2_results.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_weaker_results.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_humanlayer_strategy_results.py
```
