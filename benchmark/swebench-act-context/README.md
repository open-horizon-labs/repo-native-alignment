# Frozen SWE-bench ActContext protocol

This directory is the methodology contract for issues #779 and #782. It
freezes the Brian-compatible action-stage experiment before any additional
paid model call. It does not authorize a model call and it does not contain a
credential.

The experiment holds oracle localization and the direct Anthropic action
instrument fixed. Arm A imports Brian Sam-Bodden's `oracle_full` results. Arm B
adds a deterministic RNA CLI packet containing complete selected non-locus
bodies. Arm C reuses B's sealed loci, candidate identities, integer score
components, relationships, omissions, and order, then structurally minifies
only eligible non-locus code bodies. RNA is CLI-only; ordinary project docs
remain eligible while `.oh` and history-derived business context are disabled.

## Frozen artifacts

- `protocol.json` is the machine-readable instrument, arm, packet, statistics,
  failure, telemetry, and stop-rule contract.
- `population.json` pins all 71 upstream multi-file rows, the sole gold-invalid
  exclusion, every base commit and row/source digest, all 70 imported A
  outcomes and token counts, and the B/C execution schedule.
- `runtime-config.json` is the exact pre-access configuration a runner must
  match. Its `paid_calls_authorized` value remains false until the separate
  qualification/budget gate supplies receipts without changing this protocol.
- `packet-vector.json` is a byte-level B/C framing vector for independent
  implementations.
- `upstream/edit_patch_v2.py` is Brian's exact parser and deterministic patch
  builder at commit `fd115351d0ab742993aa5d7006f1369fb15b6e74`.
- `protocol.lock.json` and `protocol.sha256` seal every usable artifact,
  validator, and regression test.

Dataset rows are pinned to Hugging Face dataset revision
`c104f840cc67f8b6eec6f759ebc8b2693d585d4a`. The referenced Parquet LFS object
has SHA-256
`a45b1fe4e2f0c8390b2b2938ac83e92ed5979000856808f3679c07812e9e6dcd`.
Each population record also contains a canonical digest of the complete row,
plus separate problem, gold-patch, and test-patch digests. Gold content is not
included in model context; it is used only for the frozen oracle surface and
official evaluation.

## Validate without network or credentials

Run the standard-library-only validator:

```bash
python3 scripts/validate_swebench_act_context_protocol.py
python3 scripts/validate_swebench_act_context_protocol.py --json
```

Before model access, a runner must pass the digest copied from the externally
anchored #779 protocol comment or an approved qualification receipt—not merely
trust a mutable local file:

```bash
python3 scripts/validate_swebench_act_context_protocol.py \
  --expected-digest <externally-anchored-sha256> \
  --runtime-config /path/to/sealed-run-config.json
```

The separate run config must preserve every frozen field. Setting
`paid_calls_authorized` to true is accepted only when both
`qualified_artifact_receipt` and `approved_budget_receipt` are non-empty; the
committed template can never authorize paid calls, and an authorized external
config is rejected unless `--expected-digest` matches the externally anchored
bundle. Any nonzero result stops before the API key is read. The validator
performs no network, subprocess, model, evaluator, or credential access.

## A compatibility boundary

Brian's cached A evidence contains official binary outcomes, cl100k context
counts, and exact Anthropic counts for each initial request. Those quantities
are materially comparable when validation passes. Upstream A used 22 edit
feedback rounds across 11 instances but did not count those retry-request
inputs, and it did not publish comparable wall-clock timing. Accordingly:

- H1 uses the preregistered quantity `sum(initial-request input tokens over all
  frozen N=70 rows) / resolved_count` for each arm. It is not the mean among
  resolved rows.
- A retry-inclusive input tokens and wall-clock values are immutable nulls with
  reasons `upstream_not_measured` and `upstream_not_published`.
- B/C retry-inclusive totals are secondary and must never be presented as
  comparable to A.
- If a stakeholder requires retry-inclusive A/B efficiency, the run stops for
  a methodology decision; the missing A values are never reconstructed,
  inferred, or fabricated.

The official binary resolved/unresolved outcome remains the solution-quality
measure. Null and failed hypotheses are published unchanged.
