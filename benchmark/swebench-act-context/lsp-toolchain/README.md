# Frozen SWE-bench LSP toolchain evidence

This directory binds LSP toolchain work to the exact included N=70 population.
The closed lock owns every one of the 31 inventory languages and has no
unsupported entries. Qualification always runs RNA with LSP enabled via
`scan --full --no-embed`; an extract-only invocation is only a negative
fail-closed guardrail test and is never a SWE-bench execution mode.

## Reproduce the inventory

The network-enabled acquisition phase fetches each exact base commit into a
bare cache. It does not fetch branches, run checkout code, or make model calls.

```bash
python3 scripts/swebench_lsp_toolchain.py acquire-git \
  --population benchmark/swebench-act-context/population.json \
  --git-cache /path/to/lsp-git-cache

python3 scripts/swebench_lsp_toolchain.py inventory \
  --population benchmark/swebench-act-context/population.json \
  --git-cache /path/to/lsp-git-cache \
  --output benchmark/swebench-act-context/lsp-toolchain/inventory.json \
  --file-evidence-output benchmark/swebench-act-context/lsp-toolchain/files.jsonl.gz
```

`inventory` walks the exact git trees, applies the same role and exclusion
policy as RNA's `lsp-readiness` gate, and examines the first 8 KiB of every
otherwise-included blob for NUL bytes. Every case records the exact tree,
tracked/included/excluded counts, role counts, and a digest of its sorted
per-file evidence. The aggregate records every remaining suffix, role, count,
checkout count, and sample path.

Frozen evidence for this policy version:

- Population SHA-256: `067a5589b4cdb34c5fbd81bb6ff7ff6ede4dbfc26694758fafbef3544f9e6acf`
- Inventory semantic digest: `2b52991f0dd3e6b2ebf34053dc59e77ab9e85bb022d154911dfc521cc35009a9`
- Inventory file SHA-256: `500ff11ba916a697fcdac0747d25d3094b81ab4c816f38df43540fd5018bb5d7`
- Per-file evidence SHA-256: `430efcde7e1656651a89ff697941acf5104b61ea10e0ce9aed62e7ccd0b54904`
- Exact cases: 70; repositories: 10
- Tracked observations: 269,974
- Evidenced binary/vendor/generated/data exclusions: 65,700
- Mandatory under the current gate: 204,274 files in 108 suffix buckets and 31 languages

## Lock contract

`verify-lock` accepts only a closed macOS arm64 manifest. Every runtime and
server entry must include an exact version, license, source URL, cache artifact
and digest, installed executable and digest, command, arguments, extensions,
expected capabilities, and platform. The verifier rejects inventory drift,
overlapping/missing extension ownership, unknown fields, absent cache entries,
artifact digest drift, and descriptor-extension drift.

```bash
python3 scripts/swebench_lsp_toolchain.py verify-lock \
  --lock benchmark/swebench-act-context/lsp-toolchain/toolchain-lock.json \
  --inventory benchmark/swebench-act-context/lsp-toolchain/inventory.json \
  --cache /path/to/offline-cache \
  --descriptors benchmark/swebench-act-context/lsp-toolchain/descriptor-inventory.json
```

The checked-in descriptor inventory is also exercised against RNA's built-in
descriptors by Rust tests. CI verifies its exact languages, commands,
arguments, and frozen extension ownership against the lock and inventory.
