# Method

## Research question

Does deterministic repository-native context improve a software-engineering
agent's probability of producing a correct SWE-bench patch, and does it
reduce the interaction burden required to do so?

The final treatment hypothesis is **context efficacy**: the graph is injected
before the model begins, so a follow-up RNA invocation is neither required nor
counted as adherence. This differs from predecessor harnesses that attempted
to force a model-initiated first traversal. The pre-injected exposure is
reported separately from model tool calls.

## Design status and analysis population

The case population is the deterministic 20-case issue #836 cohort. Case
identity, commit, tree, and order come from the published issue836-v4
registration and selection. The canonical analysis has 80 cells: two arms on
each of two model backends for every case.

The original issue preregistration described 40 Sonnet episodes under a
mandatory first-traversal harness. Operational testing exposed material
harness defects and model/runtime incompatibilities. The final 20×4 analysis
therefore must be read as a **repaired controlled benchmark**, not as an
unchanged execution of every original harness detail. Cohort identity was
retained; prompt and runtime repairs were selected without consulting the
official exact-patch outcomes. Cross-model comparisons remain descriptive.
Within each model, A and T use the same final runtime contract.

The retained issue836-v4 registration fixes odd ranks to A→T and even ranks
to T→A (10 cases in each first-arm assignment), authorizes at most $6 for each
of 40 Sonnet episodes ($240 total), and publishes all 20 selected case
identities before model calls. The checked-in verifier binds this report to
that registration and selection. Those preserved design facts do not erase
the later prompt/runtime deviations described below.

## Conditions

Both arms receive this 166-byte base instruction (SHA-256
`da68ef814351f2953d9954f4cc309bf755605ac4e672c3d5096106cc664e3d49`):

> You are a software-engineering agent working in the current repository
> checkout. Resolve the user's SWE-bench issue using the available shell and
> file-editing tools.

Both user prompts end with two explicit requirements: implement a complete
fix and run the relevant tests.

### Control A

A receives the normalized SWE-bench problem statement and no RNA directive or
RNA results.

### Treatment T

T receives the same standard task plus this 189-byte developer directive
(SHA-256
`f91a19798b6fbee94e3e1ae17848991154d31ad2d60317f2f0436abfe327143b`):

> Use the injected RNA search and two-hop minified graph context first for
> repository orientation. Use the available rna_tool_search command for
> additional repository navigation when useful.

The T user prompt begins with a worked, verbatim RNA interaction:

1. `rna_tool_search(<deterministic query>)` and its compact search result;
2. `rna_tool_search(node=<deterministic seed>, mode="neighbors", hops=2,
   direction="both", edge_types="calls", include_body=true,
   minify_body=true, limit=<bounded limit>)` and its graph result; then
3. the byte-identical standard task used by A.

The primary query is the issue title plus at most the first 512 characters of
the problem statement. A deterministic title-only candidate is permitted
when the primary query cannot produce an admissible result. Rejected
candidates remain non-model-visible provenance. Search uses the cached strict
hybrid reranker. A preferred definition record deterministically supplies the
graph seed.

The graph is exactly two hops, call edges in both directions, with minified
bodies. The producer tries a maximum whole-record limit of 20 and reduces the
limit only as required to fit the 32 KiB user-prompt cap. Python docstrings
are removed only through AST-identified docstring handling; records and graph
projections are bounded as whole records. The final injections range from
3,076 to 30,957 bytes (mean 21,273 bytes).

Prompt bytes, base instructions, and treatment directives are byte-identical
between Sonnet and Luna within each arm. The only intended A/T differences are
the directive and the prepended RNA interaction.

## Model runtimes

- **Sonnet:** Claude Sonnet 5 through the logged-in Claude CLI.
- **Luna:** GPT-5.6 Luna through Codex App Server.

Each backend uses its provider-native tool scaffolding, so Sonnet/Luna totals
must not be treated as a randomized cross-model comparison. Within a backend,
A and T use the same checkout, runtime, network policy, and available tools.
The final runtime permits ordinary network and web access symmetrically; such
uses are retained and disclosed rather than filtered after the fact.

## Canonicalization and repairs

The following were treated as harness/runtime defects rather than model
outcomes:

- artificial RNA preprocessing and shell ceilings unrelated to SWE-bench;
- restrictive wrappers and suppressed network behavior not required by the
  standard task;
- foreign-checkout and `PYTHONPATH` contamination;
- mismatched A prompts across backends or mismatched T prompt construction;
- unbounded graph projections, extracted docstring constants, and duplicate
  prompt/task material; and
- a stale RNA launcher/binary that could hang after the model requested a
  traversal.

Affected attempts are not canonical evidence. Prompt construction was frozen
and byte-audited before replacement episodes. Luna used a predeclared
34-cell clean-runtime replacement matrix; the remaining six Luna cells were
retained only after strict runtime/transcript audit. Selection of replacements
preceded exact-patch efficacy evaluation.

The RNA product fixes discovered during this work are included on PR #837:

- recognize `.mjs` through the JavaScript/TypeScript descriptor path;
- exclude markdown results when requested by the CLI;
- avoid indexing Python docstrings as constant identities;
- deduplicate embedding candidates before reranking;
- bound lexical candidates before reranking;
- honor explicit whole-record traversal limits; and
- repair qualified-reference selection for graph context.

## Outcome evaluation

Every terminal patch was evaluated with the stock SWE-bench 4.1.0 exact-patch
evaluator. Evaluations are bound to instance ID and terminal-patch SHA-256.
Evaluator results were never provided to a model. All 80 canonical cells have
a completed `RESOLVED` or `UNRESOLVED` verdict.

## Metrics

The report includes, per cell and by condition:

- efficacy (`RESOLVED`/`UNRESOLVED`);
- elapsed wall time;
- input and output tokens;
- cached and uncached input when exposed by the provider;
- output reasoning-token subsets when exposed by the provider;
- model cost; and
- tool-call totals and types, independently recounted from transcripts.

For Sonnet, cost is the sum of provider-receipt model entries. For Luna, cost
is an API-equivalent estimate using the published standard prices captured by
the evidence manifest; it is not a claim about App Server billing.

Uncached Sonnet input is ordinary input plus cache-creation input across the
main Sonnet model and the CLI's auxiliary Haiku model. Both the inclusive and
main-model-only decompositions are reported because the receipt does not say
which internal CLI task caused the auxiliary usage. Luna reports cached and
uncached input directly. Luna reasoning output is a subset of output tokens.

## Transcript and evidence audit

All 80 transcripts passed structural parsing and independent tool recounting.
Two Luna `imageView` calls omitted from provider receipts were restored from
the transcript. One null Luna command output was retained and disclosed; no
started item lacked a completion record. Web searches and direct-solution
exposures remain in the data.

The checked-in ledger removes host paths but retains content hashes. The
external evidence root preserves prompts, receipts, transcripts, terminal
patches, runtime audits, evaluator receipts, and immutable selection
manifests. `verify_results.py` recomputes the checked-in aggregates from the
80 path-free cell records.

## Interpretation limits

There are only 20 paired cases per model. The +5 percentage-point resolution
change is one net additional resolution: two T-only wins and one A-only win
for each model. This is promising directionally but not conclusive population
evidence. Efficiency distributions are heterogeneous and contain large
positive and negative trajectory outliers. Cross-model comparisons are
descriptive, and outcome-normalized ratios are post-hoc diagnostics rather
than preregistered decision metrics.
