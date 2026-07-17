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
  qualification/budget gate supplies closed, digest-bound receipt objects
  without changing this protocol.
- `packet-vector.json` is a byte-level B/C framing vector for independent
  implementations. It includes multiple selected candidates, an omitted
  candidate with an acquisition-only relationship, exact per-record relationship
  projection, locus-seed endpoint binding, replayed rank/top-24/65,536-byte
  admission and omission decisions, pinned `cl100k_base` token-ID vectors,
  synthetic locus identity, and a Unicode retry-prompt vector that exercises
  the upstream 6,000-character previous-response boundary.
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

Before model access, a runner must pass the bundle digest and both canonical
receipt digests copied from their externally anchored GitHub comments—not
merely trust mutable local files:

```bash
python3 scripts/validate_swebench_act_context_protocol.py \
  --expected-digest <externally-anchored-sha256> \
  --runtime-config /path/to/sealed-run-config.json \
  --expected-artifact-receipt-digest <externally-anchored-receipt-sha256> \
  --expected-budget-receipt-digest <externally-anchored-receipt-sha256>
```

The separate run config must preserve every frozen field. Setting
`paid_calls_authorized` to true is accepted only when both receipt values are
closed objects matching the schemas in `protocol.json`:

- the artifact receipt binds the external protocol digest, artifact commit and
  SHA-256, successful GitHub Actions provenance, Apple M4 platform, no-fallback
  Metal/embeddings/reranking evidence, and a complete quiescent LSP per-file
  coverage manifest with zero skipped, partial, degraded, cancelled, crashed,
  or timed-out work;
- the budget receipt binds the same protocol digest to either the #789
  qualification pair or #790 N=70 cohort, its request ceiling, fixed-decimal
  dollar ceiling, and an externally anchored approval comment/evidence digest.

Receipt schema versions, issues, request ceilings, file counts, and LSP counters
are exact JSON integers: booleans are rejected even though Python normally
compares them equal to zero or one. Receipt timestamps must parse and round-trip
as real calendar-valid UTC instants at whole-second precision. Credential
screening rejects common provider-key, GitHub, AWS, Slack, bearer-token,
private-key, and account-identifier shapes in both external receipt configs and
every locked file. AWS IAM ARNs containing a 12-digit account ID and standalone
12-digit AWS account IDs are rejected. Standalone means delimited by
non-alphanumeric characters and not immediately preceded by a decimal point, so
the frozen 12-place statistical decimals remain valid. Validation exceptions
and CLI output never echo an untrusted key or value.

Arbitrary strings are rejected. The committed template can never authorize
paid calls, and an authorized external config is rejected unless
`--expected-digest` matches the externally anchored bundle, both closed receipt
objects pass, and each object's canonical JSON SHA-256 matches its own external
CLI trust anchor.
Because validation is offline, the operator copies those values from their
external GitHub anchors; the validator neither fetches nor invents trust. Any
nonzero result stops before a credential is read. The validator performs no
network, subprocess, model, evaluator, or credential access.

The acquisition object in every packet preamble has exact nested schemas for
loci, candidates, relationships, and omissions. Non-node loci use deterministic
synthetic IDs. A new-file locus uses line bounds `0,0`, language `Unknown`, an
empty payload/digest, no RNA seed, and no traversal relationship. B and C reuse
the same canonical acquisition bytes. Every incoming relationship terminates at
a seed of its declared locus and every outgoing relationship starts at one; the
opposite endpoints plus semantic-ranked IDs close the candidate pool. The
validator re-derives graph/semantic scores and order, then replays ineligible,
top-24, and 65,536-byte admission precedence and the exact budget/omission state
instead of trusting recorded flags. Locus record headers always carry an empty
relationship array. Each selected candidate header carries exactly the ordered
acquisition relationships whose source or target is that candidate's stable ID;
relationships incident only to omitted candidates remain acquisition-only.

Retries exactly reuse the original initial prompt and append the pinned suffix
with `(raw or "")[-6000:]` from the immediately preceding response, where the
slice counts Python Unicode code points rather than UTF-8 bytes, plus exact
`failure_feedback(all_results)` text. A later retry never appends to an earlier
retry prompt.

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

The paired resolved-rate interval is fully executable, not deferred. For cells
`n00,n01,n10,n11` oriented first arm then second arm, the estimand is
`(n01-n10)/N`. The protocol uses a conservative finite-sample 95% simultaneous
Bonferroni/Clopper-Pearson interval: each discordant-cell probability receives
a 97.5% two-sided Clopper-Pearson interval (four one-sided tails of 0.0125),
then the difference bounds are `L01-U10` and `U01-L10`. The manifest freezes the
binomial-tail equations, Decimal precision, bisection count, boundary handling,
arm orientation, strict `>-0.10` gate, and four executable vectors.

H2's ActContext payload metric counts only selected-candidate payloads from the
initial packet: B uses each full payload, C its minified payload, and loci,
metadata, headers, framing, prompt text, and retry repetition are excluded. Each
record is tokenized independently with `cl100k_base.encode_ordinary`; records
are never concatenated before tokenization. The tiktoken 0.13.0 source/wheel and
mergeable-ranks hashes plus complete token-ID vectors are pinned. H2's separate
total-episode metric sums Anthropic `usage.input_tokens` across every initial and
edit-feedback request. Both metrics sum all N=70 rows before dividing by the
arm's official resolved count, and all threshold arithmetic uses unrounded
integers/counts. Token reduction is not a per-record eligibility rule: the
frozen vector deliberately contains byte-shorter minified payloads whose pinned
cl100k counts increase from 58 to 72. The fixture is not optimized to favor C;
H2 may fail and must then be published unchanged.

The official binary resolved/unresolved outcome remains the solution-quality
measure. Null and failed hypotheses are published unchanged.
