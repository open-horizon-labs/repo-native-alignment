# One-instance SWE-bench RNA harness

This harness runs one SWE-bench Verified task through an isolated checkout,
RNA's real stdio MCP server, an explicitly configured executor, and the
official Docker evaluator. It produces an auditable run bundle for later
paired experiments. A single run is a case study, **not a benchmark score**.

## Prerequisites

- Python 3.11 or newer.
- Git.
- A released or CI-artifact RNA binary on `PATH`. Record the exact binary used;
  do not represent a local source build as a shipped RNA release.
- Docker with enough disk for the official evaluator. SWE-bench documents
  roughly 100–120 GB for the usual `env` cache level.
- The official evaluator and dataset clients:

  ```bash
  python -m pip install swebench datasets huggingface_hub
  ```

- A separately authenticated executor/model. Model tokens and provider cost
  are external to this repository. Set an explicit provider budget.

The executor runs inside the isolated checkout and receives these environment
variables:

- `SWEBENCH_TASK_PROMPT`: benchmark problem and harness instructions.
- `SWEBENCH_MCP_CONFIG`: MCP configuration containing only `rna-server`.
- `SWEBENCH_EXECUTOR_REPORT`: optional structured telemetry report path.
- `SWEBENCH_STAGE_LEDGER`: the complete ledger path.
- `SWEBENCH_MCP_TRACE`: proxy-recorded MCP traffic.
- `SWEBENCH_RUN_DIR`, `SWEBENCH_CHECKOUT`, and `SWEBENCH_INSTANCE_ID`.

The generated MCP configuration points to a logging proxy which launches the
selected RNA binary directly with `--repo <isolated-checkout>`. The wrapper
fails a live run if the executor produces no observable MCP initialization,
tool-list, or tool-call traffic.

## One command

This Claude Code example keeps the model and cost ceiling explicit:

```bash
python3 scripts/swebench_rna_one.py django__django-13279 \
  --executor-command 'claude -p --verbose --output-format stream-json \
    --strict-mcp-config --mcp-config "$SWEBENCH_MCP_CONFIG" \
    --permission-mode bypassPermissions --model sonnet --max-budget-usd 8 \
    "$(cat "$SWEBENCH_TASK_PROMPT")"' \
  --model-name claude-sonnet \
  --output-dir /tmp/swebench-rna-django-13279
```

An executor configuration file is preferable for repeatable experiments:

```json
{
  "command": [
    "my-agent",
    "--mcp-config-env",
    "SWEBENCH_MCP_CONFIG",
    "--task-env",
    "SWEBENCH_TASK_PROMPT"
  ],
  "model": {
    "provider": "example",
    "name": "model-version",
    "temperature": 0,
    "budget_usd": 8
  }
}
```

Pass it with `--executor-config executor.json`. The full selected configuration
is copied into `manifest.json`.

## Isolation and enrichment

The harness fetches only the benchmark base commit into a temporary directory,
exports its tree, deletes the fetch directory, initializes a fresh repository
in the run bundle, and makes exactly one local base-snapshot commit. Before the
executor starts, it proves:

- `git rev-list --count HEAD` is `1`;
- `git remote -v` is empty;
- the checkout is clean.

The default `--enrichment-condition full` runs the post-#765–#769 bounded LSP
pipeline and explicit embedding enrichment. `call-references` omits embeddings;
`structural` is recorded as a selected degraded experimental condition. Raw
scan, enrichment, and readiness logs are retained. A degraded LSP exit is not
silently converted to success: the wrapper accepts it only when the subsequent
readiness probe proves that the extracted graph is queryable, and records the
degraded evidence in the manifest.

## Run bundle

The output directory contains:

- `manifest.json`: dataset revision, instance/base commit, RNA revision,
  executor/model configuration, timings, isolation proof, selected and observed
  enrichment state, MCP-use summary, evaluator outcome, and errors.
- `checkout/`: isolated one-commit agent workspace.
- `mcp-config.json`, `mcp-trace.jsonl`, and `rna-mcp.stderr.log`.
- `executor.stdout.log`, `executor.stderr.log`, and
  `executor-timed-trace.jsonl`.
- `fallback-events.jsonl`: raw-read/shell fallback events when exposed by the
  executor or its report.
- `stage-ledger.json`: every required stage and accounting category. Missing
  provider telemetry remains `status: "unknown"`; MCP byte sizes are never
  converted into inferred token counts.
- `prediction.jsonl`: official prediction format with `instance_id`,
  `model_name_or_path`, and `model_patch`.
- `dataset-instance.json`: the exact row loaded from the resolved Verified
  dataset revision. Because that row may contain the gold and test patches, it
  is not written until after the executor exits. The official evaluator then
  receives this supported local JSON dataset path so evaluation cannot silently
  drift to a newer dataset row.
- `evaluation/`: exact evaluator command, stdout/stderr, reports, and official
  harness logs.

The harness independently polls Git's working-tree state after executor start
and records time to the first meaningful tracked or untracked edit (excluding
RNA/cache/build directories). Executors with richer telemetry may write
JSON to `SWEBENCH_EXECUTOR_REPORT`:

```json
{
  "stages": {
    "frontier_before_first_edit": {
      "input_tokens": 1200,
      "output_tokens": 300,
      "reasoning_tokens": 80,
      "cost_usd": 0.04
    }
  },
  "fallback_events": [
    {"tool": "Read", "path": "example.py", "reason": "implementation body"}
  ]
}
```

Unreported stages remain explicit unknowns.

## Token-free dry run

The repository fixture validates checkout isolation, manifest creation,
prediction construction, evaluator command construction, and the full stage
ledger without starting RNA, an executor, Docker, or the evaluator:

```bash
python3 scripts/swebench_rna_one.py fixture__repo-1 \
  --executor-command 'exit 99' \
  --output-dir /tmp/swebench-rna-dry-run \
  --instance-json scripts/tests/fixtures/swebench_instance.json \
  --fixture-source scripts/tests/fixtures/repo \
  --dry-run

python3 -m unittest scripts/tests/test_swebench_rna_one.py
```

## Manual Verified run

The committed implementation was manually exercised on
`django__django-13279`. Record the final run-bundle path, executor/model and
budget, observed enrichment condition, patch status, and official evaluator
outcome here before marking the issue complete. If model authorization, Docker,
or evaluator setup prevents completion, retain the failed bundle and state the
exact external blocker rather than presenting the dry run as an end-to-end
result.
