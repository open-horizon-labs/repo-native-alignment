# Issue 779 paired SWE-bench Verified pilot

This directory freezes the inputs for a three-task baseline-vs-RNA pilot. It is
not a full-suite score and must not be compared numerically with Anthropic's
published 73.3% result: the populations, scaffold, prompt, trial count, and
thinking budget differ.

## Immutable inputs

- Model: `claude-haiku-4-5-20251001` through Claude Code 2.1.112.
- Dataset: `princeton-nlp/SWE-bench_Verified` at
  `c104f840cc67f8b6eec6f759ebc8b2693d585d4a`.
- Evaluator: `swebench==4.1.0`, tag commit
  `726c5461e2ef52d83cf1ea2107870a8bb3328d57`.
- RNA: successful CI run `29519554315`, commit
  `971b6f368d2153101f0f1aeab4a027a4eedd4678`, artifact ID `8384200497`,
  intentional `call-references` condition.

## Reproduce

Use Python 3.12 because the pinned dataset client is not compatible with Python
3.14:

```bash
python3.12 -m venv /tmp/swebench-779-venv
/tmp/swebench-779-venv/bin/python -m pip install \
  swebench==4.1.0 datasets==4.0.0 huggingface_hub==0.33.4

mkdir -p /tmp/rna-779-artifact
gh run download 29519554315 \
  --repo open-horizon-labs/repo-native-alignment \
  --name repo-native-alignment-darwin-arm64-fast \
  --dir /tmp/rna-779-artifact
tar -xzf /tmp/rna-779-artifact/repo-native-alignment-darwin-arm64-fast.tar.gz \
  -C /tmp/rna-779-artifact

/tmp/swebench-779-venv/bin/python scripts/swebench_rna_pair.py \
  --task-spec benchmarks/swebench-paired/779/tasks.json \
  --executor-config benchmarks/swebench-paired/779/executor.json \
  --rna-artifact benchmarks/swebench-paired/779/rna-artifact.json \
  --rna-binary /tmp/rna-779-artifact/repo-native-alignment \
  --output-dir /tmp/swebench-779-paired
```

The paired directory retains every child run, including unsuccessful runs. Its
`paired-manifest.json` records exact commands and exit codes;
`paired-report.json` contains per-instance outcomes and arm summaries. Each
child bundle retains predictions, official evaluation output, traces, fallback
events, time to first edit, token categories, and cost. Missing provider
telemetry remains `null`/unknown.

Both arms execute `scripts/swebench_claude_executor.py`. The baseline sees an
empty MCP configuration and starts Claude without extra context. The RNA arm
uses the same wrapper to make a real stdio `search` call through the traced
proxy, saves the returned context, and appends it to Claude's runtime prompt
before Claude can edit. This makes the delivered context difference a direct
consequence of RNA availability while enforcing the pre-edit MCP gate.

Before interpreting an RNA child as valid, confirm
`real_mcp_use_before_edit: true`. An MCP handshake or a successful call after
the first edit does not satisfy the experiment.
