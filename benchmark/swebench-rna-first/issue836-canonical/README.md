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
factorial cells, plus the later 20-cell Sonnet T_PD population follow-up, in
one reader-facing analysis.

The same report also contains a clearly separated post-hoc working-set
implementation diagnostic: rank 3 only, T3 and T4 on Sonnet and Spark, reusing
the canonical anti-slop A controls. Those four paid cells are outside the 720
canonical cells and ended in a `DO_NOT_SCALE` decision for T4. Their path-free
ledger and evidence bindings are `working-set-diagnostic-results.json` and
`working-set-diagnostic-evidence-manifest.json`.

An additional one-case T5 causal-working-set diagnostic follows that failed
T4 gate without changing the 720 canonical cells. It preflighted ranks 10 and
13 offline and ran only rank 10 on Sonnet and Spark. A later audit found that
its reused A controls were runtime-mismatched, so T5 efficacy is retained but
its efficiency comparison is explicitly runtime-confounded. Its path-free
provenance ledger and evidence bindings are
`causal-working-set-diagnostic-results.json` and
`causal-working-set-diagnostic-evidence-manifest.json`.

The final bounded follow-up is the four-cell matched A6/T6 micro-gate on rank
10: fresh A and compact-treatment cells on Sonnet and Spark under the same
runtime within each model. All four are officially resolved, and T6 reduces
input, output, time, and tools on both models despite paying positive static
prefix-replay overhead. The path-free canonical replay decomposition, matched
cell details, and evidence bindings are `prompt-replay-analysis.json` and
`prompt-replay-analysis-evidence-manifest.json`. This is a mechanism check,
not a population or scale claim.

The final three-case D/E diagnostic asks whether a 4 KiB stable-ID manifest
causes Sonnet to request caller-selected expansion when RNA is first optional
and then explicitly preferred. E reuses D's primed query/result prompt
byte-for-byte. Neither condition produces a follow-up RNA call, both resolve
1/3 selected cases, and no scale or RNA-default change follows. Its path-free
ledger and evidence bindings are
`compact-progressive-disclosure-results.json` and
`compact-progressive-disclosure-evidence-manifest.json`.

The Sonnet bounded progressive-disclosure population follow-up reuses all 20
canonical A controls and adds one 8 KiB complete-record RNA treatment episode
per case. All 20 pairs have stock SWE-bench verdicts and per-arm split input,
output, time, cost, and tool-type accounting in the unified report. Its
path-free ledger and external evidence bindings are
`bounded-progressive-disclosure-results.json` and
`bounded-progressive-disclosure-evidence-manifest.json`.

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

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_working_set_diagnostic.py

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_causal_working_set_diagnostic.py

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_prompt_replay_analysis.py

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_compact_progressive_disclosure.py

PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_bounded_progressive_disclosure.py
```

That mode recomputes aggregates, parity, effects, matrix/tool rows, registered
order and budget, and fail-closed package digests. Where the retained evidence
root is available, the commands also verify their bound external artifacts by
content:

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

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_working_set_diagnostic.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_causal_working_set_diagnostic.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_prompt_replay_analysis.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_compact_progressive_disclosure.py

ISSUE836_EVIDENCE_ROOT="$PWD/../issue836-selector-20case-20260724" \
PYTHONDONTWRITEBYTECODE=1 \
  python3 benchmark/swebench-rna-first/issue836-canonical/verify_bounded_progressive_disclosure.py
```
