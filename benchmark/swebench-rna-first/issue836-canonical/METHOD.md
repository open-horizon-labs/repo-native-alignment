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
registration and selection. The original canonical analysis has 80 cells: two
arms on each of two model backends for every case. An additive post-hoc T2
execution contributes 40 more cells, one faithful unfiltered typed-graph
treatment on each backend for every case. The original 80 cells remain intact;
T2 is reported as a distinct condition in the unified report and is not
substituted for the original T arm.

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

### Original treatment T (calls-only graph)

T receives the same standard task plus this 189-byte developer directive
(SHA-256
`f91a19798b6fbee94e3e1ae17848991154d31ad2d60317f2f0436abfe327143b`):

> Use the injected RNA search and two-hop minified graph context first for
> repository orientation. Use the available rna_tool_search command for
> additional repository navigation when useful.

The T user prompt begins with a worked, verbatim RNA interaction:

1. `rna_tool_search(title + "\n\n" + normalized_body[:512])` (or the
   title-only fallback defined below) and its compact search result, where the
   exact byte-level construction of `title` and `normalized_body` is specified
   below;
2. `rna_tool_search(node="<first admissible stable node ID under the
   deterministic title-overlap, production-first, and RNA-rerank ordering>",
   mode="neighbors", hops=2, direction="both", edge_types="calls",
   include_body=true, minify_body=true, limit=<selected whole-record
   limit>)` and its graph result; then
3. the byte-identical standard task used by A.

The query is constructed byte-deterministically. After trimming the problem
statement, the first line (trimmed at both ends) is the issue title. All
remaining lines form the body; every run of Unicode whitespace in that body is
replaced by one ASCII space, and the result is truncated to its first 512
Unicode characters. The deterministic primary query is exactly the issue
title, two ASCII line-feed bytes, and that normalized 512-character body
prefix: `title + "\n\n" + normalized_body[:512]`. No label, quoting, or other
text is added to the query. If that query
cannot produce a valid compact result, an eligible traversal root, and a
bounded valid graph projection, the producer tries exactly one fallback:
`title`. Only the selected query and result are model-visible; rejected
candidates remain non-model-visible provenance. Both searches use the cached
strict hybrid reranker.

The traversal root is selected deterministically from the selected compact
search result. Eligible records have kind `function`, `method`, `struct`,
`enum`, `trait`, or `module`. The producer case-folds the title, extracts
`[a-z0-9_]+` tokens longer than two characters, and removes this fixed
stop-word set: `bug`, `when`, `with`, `from`, `into`, `this`, `that`, `the`,
`and`, `for`, `passing`, `error`, `issue`, `fails`, and `failure`. For each
record, the case-folded concatenation of its name, one space, and its stable ID
is tokenized with the same expression. Eligible records are ordered by: (1)
descending size of the unique-token intersection between the title and record;
(2) production records before test records; and (3) their original
RNA-reranked order. A record is classified as a test when its stable-ID path
starts with `test`, contains `/test` or `tests/`, or its name starts with
`test_`.

The producer tries traversal roots in that order. For each root it tries the
whole-record limits `20`, `10`, `5`, `2`, and `1`, in that order. The selected
root and limit are the first combination that returns a valid graph projection
and keeps the complete T user prompt at or below 32 KiB (32,768 bytes). Thus
the traversal root can be a later RNA result when earlier roots cannot produce
a valid bounded prompt; it is not a random seed.

The selected graph projection is exactly two hops, call edges in both
directions, with minified bodies. Python docstrings are removed only through
AST-identified docstring handling; records and graph projections are bounded
as whole records. The final injections range from 3,076 to 30,957 bytes (mean
21,273 bytes).

Prompt bytes, base instructions, and treatment directives are byte-identical
between Sonnet and Luna within each arm. The only intended A/T differences are
the directive and the prepended RNA interaction.

### Additive treatment T2 (unfiltered typed graph)

T2 executes the faithful graph specification requested after the original T
evidence was frozen. It is an additive, post-hoc condition, not a relabeling of
T and not a retroactive preregistered arm. T2 uses the same standard task,
166-byte base instruction, and 189-byte RNA directive as T.

For each rank, T2 reuses the audited T text query and compact text-search result
byte-for-byte. The primary model-visible text-search call is exactly:

`rna_tool_search(title + "\n\n" + normalized_body[:512])`

Here, `title` is the problem statement's first line after trimming the whole
statement and then trimming that line at both ends. `normalized_body` is every
remaining line joined as the body, with each run of Unicode whitespace replaced
by one ASCII space. The slice is the first 512 Unicode characters. No label,
quoting, or other text is added. If and only if that primary query cannot yield
a valid compact result, an eligible traversal root, and a bounded valid graph
projection, the producer tries the single deterministic fallback call
`rna_tool_search(title)`. Only the selected query and result are model-visible;
both calls use the cached strict hybrid reranker.

The original calls-only graph result is not reused. Instead, the graph is
freshly projected from the warm exact-tree cache with this model-visible call:

`rna_tool_search(node="<first admissible stable node ID under the
deterministic title-overlap, production-first, and RNA-rerank ordering>",
mode="neighbors", hops=2,
direction="both", include_body=true, minify_body=true,
limit=<selected whole-record limit>)`

“Traversal root” is the selected graph node, not a random seed. Eligible
records and their ordering are exactly those defined for T above: descending
unique title-token overlap, then production before test, then original
RNA-reranked order. The producer tries candidates in that order and, for each
candidate, tries whole-record limits `20`, `10`, `5`, `2`, and `1`. It selects
the first candidate-and-limit pair that returns a valid graph projection and
keeps the complete prompt at or below 32,768 bytes. Consequently, a later
candidate is selected when every limit for an earlier candidate is invalid or
still too large.

There is deliberately no `edge_types` argument. RNA therefore traverses the
unfiltered typed graph in both directions for exactly two hops. The projected
records retain their typed edge labels (for example, `Calls`, `Defines`, or
another producer-reported type) so the model can distinguish relationships.
Deterministic whole-record projection and the limit ladder `20`, `10`, `5`,
`2`, `1` are applied only after that traversal. The selected combination is the
first admissible traversal root and limit whose complete projected records keep
the entire user prompt at or below 32,768 bytes. A later root may be selected
when an earlier root cannot yield a valid bounded projection.

All 20 selected T2 prompts passed global byte audit before their canonical
provider episodes. Ranks 1–19 passed before the original run. A later whole-record
audit found that rank 20's original projection contained an unparsed top-level
record bullet; its parser was repaired and its replacement prompt passed the
same gate before the two replacement episodes. The selected prompts range from
2,514 to 25,075 bytes (mean 18,085.8); their typed graph projections range from
496 to 11,723 bytes (mean 4,671.65). Rank 12 uses the already-audited
deterministic title-only fallback; the other 19 ranks use the primary
title-plus-normalized-512-character query. T2 prompt, base, and directive bytes
are identical between Sonnet and Luna for every rank.

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
- unbounded graph projections, extracted docstring constants, duplicate
  prompt/task material, and unparsed top-level producer record bullets; and
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
Evaluator results were never provided to a model. All 80 original canonical
cells and all 40 additive T2 cells have a completed `RESOLVED` or `UNRESOLVED`
verdict.

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

All 80 original transcripts passed structural parsing and independent tool
recounting. All 40 additive T2 transcripts passed the same checks plus exact
prompt and patch hashing, model/context validation, item reconciliation, and
foreign-rank/checkout-reference checks.
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
