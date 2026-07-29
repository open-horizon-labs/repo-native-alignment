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
registration and selection. The previously published canonical package has
240 cells: A, original T, and additive T2 on Sonnet, Luna, Haiku, and Spark for
every case. T2 is a distinct faithful unfiltered typed-graph condition; it does
not substitute for the original calls-only T arm.

The Haiku/Spark cells are a post-hoc ceiling-effect sensitivity execution. They
test the concern that Sonnet and Luna solve too many selected cases for a
context-treatment difference to be visible. They do not replace or alter the
Sonnet/Luna evidence and are not part of the preregistered selector.

The HumanLayer/SlopCodeBench comparison adds a second, orthogonal factor. Two
published prompt strategies—anti-slop and plan-first—are crossed with A, T, and
T2 on all four models and all 20 cases. Those 480 additive cells do not replace
the 240 no-strategy cells. The unified analysis therefore contains 36
conditions and 720 cells. This factorial extension was frozen before its first
provider call, but it remains a post-hoc extension of the issue836 study rather
than a retroactive preregistration.

The bounded progressive-disclosure population follow-up adds 20 Sonnet T_PD
episodes. A later prompt-channel audit adds 20 fresh native-default controls
(`A_prime`) matched to T_PD's runner contract. These 40 cells are reported in
the same reader-facing report but remain outside the frozen
36-condition/720-cell factorial. The package therefore contains 760 officially
evaluated cells without retroactively changing the canonical matrix.

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
text is added to the query. The constructed query is encoded as UTF-8 without
a byte-order mark; the two separators are the literal bytes `0x0A 0x0A`, and
no newline normalization is applied after construction. If that query
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

### Weaker-model sensitivity execution

The Haiku/Spark execution reuses the checked-in A, original T, and T2
user-prompt bytes exactly; it does not rebuild RNA context. For every case and
condition, Haiku and Spark receive matching user-prompt SHA-256 values, and
those values match the corresponding frozen Sonnet/Luna condition. All prompts
remain at or below 32,768 bytes, below Spark's 121,600-token effective context
window and Haiku's 200,000-token context window.

Conditions are serialized within each case so one checkout is never shared by
simultaneous arms. The deterministic rotating order is A→T→T2, T→T2→A, or
T2→A→T by rank; different cases may run concurrently. The analysis retains
every completed cell, including empty patches and unresolved verdicts. One
rank-6 Haiku T setup attempt failed before provider invocation because the
launcher supplied the wrong checkout path; its zero-spend setup record is
provenance only, and the first provider-invoked T episode is canonical. No
paid weaker-model cell was replaced or rerun.

### HumanLayer/SlopCodeBench prompt-strategy extension

The comparison pins the HumanLayer article at commit
`a2da7968c7d5cbc8a58e9c559f4d9eea6d460d6c` and SlopCodeBench at commit
`13de1a7a6b8b3dc5cc532a0c322a0997afa5bec7`. The exact upstream
`anti_slop` and `plan_first` Jinja templates have SHA-256 values
`e334962c38a1ca83f9de87b2120821b07aabd7159f665dfde843f04fe5ed74a5`
and
`0fcf89c6f841d9d2d02b3ec50b61ebcf4c4d7fcab812ec4e5d5e66efbec2cf8b`,
respectively.

The two sources answer related but different questions. HumanLayer's reported
Opus/Sonnet execution uses SlopCodeBench's `just-solve` baseline over
longitudinal greenfield checkpoints: model context is fresh at each checkpoint,
prior code but not prior conversation carries forward, and strict black-box
evaluation includes earlier regression tests. The article proposes review,
metric-backpressure, and frontier-to-weaker-model handoff ideas, but does not
report those proposals as executed prompt arms. The associated SlopCodeBench
study executes exactly two prompt interventions, `anti_slop` and `plan_first`.

Issue #836 is instead a single-shot existing-repository repair benchmark. It
cannot import longitudinal checkpoint carry-over without changing the task
population and estimand. The extension therefore transfers the two executed
prompt interventions as developer/system-instruction deltas while retaining
the exact frozen SWE-bench task bytes, checkouts, official evaluator,
provider-native scaffolding, and A/T/T2 RNA-context factor. Every episode still
starts in an isolated context and clean frozen-base checkout.

The semantic port removes only SlopCodeBench-specific scaffolding that is false
for this task: Jinja branching, virtual-environment and `requirements.txt`
setup, the instruction to create a new Python script, and the `{{ spec }}`
insertion. The published planning sequence and anti-slop/style rules remain.
The resulting anti-slop bytes are 808 bytes with SHA-256
`8d47fd170081caee967ae54bb0921f520d1b466c37959e9cfd59e2745c5964aa`;
the plan-first bytes are 727 bytes with SHA-256
`3d93456b81f521c34d97312e9745adfecce74333668c42aa2d90d31b4b21e430`.
This is a frozen semantic port, not a claim that a SWE-bench episode is
byte-identical to a SlopCodeBench episode.

For A, the selected strategy is the entire added developer instruction. For T
and T2, the existing 189-byte RNA directive is followed by exactly two LF bytes
and then the selected strategy bytes. The canonical A/T/T2 user prompts remain
unchanged. Strategy bytes are identical across models and ranks. For every
model and case the additive cells are:

- `A_AS`, `T_AS`, and `T2_AS`: anti-slop crossed with A/T/T2; and
- `A_PF`, `T_PF`, and `T2_PF`: plan-first crossed with A/T/T2.

Before provider spend, all 480 prompt compositions passed exact source-hash,
cross-model parity, instruction-composition, and prompt-size checks. Condition
submission order rotates deterministically; no official outcome was consulted
during prompt generation. The scale launcher uses six workers and can execute
the six strategy/context cells for one model/case concurrently. Every cell has
its own frozen-base checkout and fresh provider thread, so no code or
conversation state is shared. Elapsed-time comparisons therefore include
co-scheduled machine/provider contention; the report treats them as paired
operational measurements rather than uncontended runtime benchmarks.

The first scale audit exposed host-level Python state that was not isolated by
separate checkouts: concurrent editable installs registered sibling episode
paths globally, and a later global `.pth` startup hook printed a stale checkout
path before `sitecustomize` could filter it. Package-manager metadata also
reported an older editable checkout even though the filesystem boundary denied
the attempted read. Exact affected cells are superseded, not analyzed. Queued
and replacement cells use a per-cell virtual environment and pip target;
shared dependency directories are appended without executing host `.pth`
files; pip observes only per-cell distribution metadata; and the provider
process is denied only sibling and prior benchmark episode roots. The final
runtime additionally denies reads of host shared-site `.pth` and `.egg-link`
metadata, preventing a model that bypasses the per-cell Python wrapper from
recovering stale checkout paths through global Python startup or package
metadata. Network, ordinary host access, prompts, model settings, and time
ceilings are unchanged. Transcript admission fails on any model-visible foreign
checkout input or output, then retries only that exact frozen slot.

An efficiency-outlier review then found a distinct same-cell leak in four
otherwise completed App Server episodes. A model-issued recursive search rooted
at `..` could match the live `app-server.out.jsonl` beside its checkout, feeding
part of its own prompt and tool history back into model-visible command output.
The audit was extended to fail closed on this pattern and identified exactly
four cells: rank 5 `A_AS_spark`, rank 5 `A_PF_spark`, rank 6 `T_AS_luna`, and
rank 17 `A_PF_spark`. The runtime now makes the adjacent harness control files
unreadable to the provider while leaving the checkout, network, host tools, and
ordinary host paths available. Only those four frozen slots were rerun; the old
episodes are superseded because their efficiency measurements are inflated,
not because their model-generated patches were post-hoc classified by outcome.
The strengthened final audit passes all 480 strategy cells.

Provider accounting is explicit: 480 canonical strategy cells plus one
wrapper-timeout attempt, 14 cross-cell/host-path isolation attempts, and four
same-cell transcript-exposure attempts equal 499 paid provider episodes. Stock
SWE-bench evaluation and deterministic quality scoring add zero model calls.

#### Transfer-status audit

This table separates what HumanLayer actually executed from ideas it explicitly
lists under “what's next / things I'd do differently.” Only executed prompt
conditions are treatment arms. Operational choices and unexecuted proposals are
reported for comparison, but are not relabeled as replicated experiments.

| Design element | Source status | HumanLayer / SlopCodeBench | Issue #836 disposition |
|---|---|---|---|
| Task unit | Executed method | Three longitudinal greenfield challenges, 17 checkpoints | Twenty single-shot SWE-bench repairs; longitudinal carry-over is not transferred because it would change the task population and estimand |
| Context | Executed method | Fresh conversation per checkpoint | Fresh provider thread per cell |
| State carried | Executed method | Code, but not conversation, carries across checkpoints | No state across cells; every cell starts from its frozen base |
| Prompt parity | Executed method | Same prompt for every compared model | Exact per-rank/context user-prompt bytes are identical across strategy and model cells |
| Provider harness | Executed method | Claude Code | Provider-native Claude CLI or Codex App Server, held fixed within each model comparison |
| Baseline prompt | Executed condition | SlopCodeBench `just-solve` | Existing strategy-free A/T/T2 cells |
| Added prompt arms | Executed in linked SlopCodeBench study, not HumanLayer's reported run | SlopCodeBench `anti_slop` and `plan_first` | Frozen semantic ports crossed with A/T/T2 on all four models |
| Correctness | Executed method | Held-out black-box tests against the produced entrypoint, including prior-checkpoint regressions | Stock SWE-bench 4.1 evaluator; no prior checkpoints exist in this single-shot task |
| Quality | Executed measurement | Forty-one deterministic checkpoint metrics; treated as directional rather than a standalone maintainability oracle | Same pinned SlopCodeBench metric families on changed Python files, before and after each patch |
| Cost, time, and tokens | Executed measurement / proposed emphasis | Cost observed; article proposes greater future emphasis on time and token efficiency | Cost, elapsed time, input/cache/output tokens, and tool calls by type are reported per cell |
| Nine-way challenge parallelism | Future operational suggestion | Proposed to shorten wall-clock execution | Not a treatment; cell isolation and deterministic ordering are retained, and co-scheduling is disclosed |
| TypeScript rule port | Future tooling suggestion | Proposed because the published detectors are Python-only | Not applicable to this Python cohort and not a model treatment |
| Adversarial model-review loop | Future treatment proposal | Explicitly proposed but not executed | Not claimed as replicated; it would add a second model interaction and change the estimand |
| Deterministic quality backpressure | Future treatment proposal | Explicitly proposed but not executed | Metrics are measured after the patch, not fed back into the solver; an iterative feedback arm would be a new experiment |
| Frontier-to-weaker handoff | Future benchmark proposal | Proposed for a later longitudinal checkpoint | Not transferable to independent single-shot repairs without constructing a different longitudinal benchmark |
| Larger dataset | Future scale proposal | Proposed | Not a treatment; the frozen 20-case cohort is retained |

The completed transferable treatment set is therefore the strategy-free
baseline plus `anti_slop` and `plan_first`, crossed with A/T/T2. Review,
backpressure, and handoff remain clearly labeled candidate follow-up studies;
adding them here would not make this experiment a closer replication of the
reported HumanLayer run.

Primary sources: [HumanLayer benchmark article](https://github.com/humanlayer/advanced-context-engineering-for-coding-agents/blob/a2da7968c7d5cbc8a58e9c559f4d9eea6d460d6c/benchmarking-opus-5-on-slop-code-bench.md),
[SlopCodeBench paper](https://arxiv.org/html/2603.24755v1), and
[SlopCodeBench repository](https://github.com/SprocketLab/slop-code-bench/tree/13de1a7a6b8b3dc5cc532a0c322a0997afa5bec7).

### Cumulative-input replay decomposition

The provider token ledgers report cumulative input processed across inference
requests, not unique prompt bytes. The 480 strategy transcripts were therefore
audited request by request. For Claude, assistant events are deduplicated by
provider message ID and request context is ordinary input plus cache creation
plus cache read. For Codex App Server, each distinct cumulative
`thread/tokenUsage/updated` notification contributes its `last.inputTokens`;
byte-for-byte repeated cumulative notifications are not counted as new
requests.

For every same-case A/T or A/T2 pair, main-model input is decomposed as:

`actual delta = static treatment-prefix replay + interaction-trajectory delta`

with static replay equal to
`(T first context − A first context) × T request count`. The trajectory
residual captures changed request count and later conversation growth. The
identity is exact for all 240 Sonnet/Luna/Spark treatment cells. Haiku's
transcript-visible main sequence excludes same-model auxiliary classification
calls included in the provider total, so its four aggregate decompositions are
explicitly labeled approximate.

This accounting is descriptive and deterministic; it does not assume the
treatment is relevant. Root relevance is analyzed separately from graph
structural validity.

### Post-hoc T5 causal-working-set gate

T5 is a bounded implementation diagnostic prompted by the observation that
treatment token growth was predominantly cumulative cached-input replay, while
output and sometimes time/tool counts fell. It is outside the 720 canonical
cells. Its estimand is narrower: can a deterministic, causally relevant RNA
working set reduce an unhelpful interaction trajectory without materially
increasing unique input?

The T5 rule was frozen before its paid calls. It begins with the case's already
frozen 50-record strict hybrid/reranked title-plus-normalized-512-character
search result. Test, documentation, example, migration, and benchmark roots are
discarded; only callable and type definitions are eligible. Explicit
underscore-bearing callable/type identifiers use an exact lexical lane, with
leading-underscore identifiers in original task order preferred. Generic
prose words, constants, and CamelCase domain nouns receive no exact-match
bonus. Otherwise candidates are scored by task-token overlap in symbol name
and path, with original RNA rerank position as the tie-break.

When the task includes an explicit diagnostic identifier such as
`admin.E108`, it must occur literally in the proposed root body. The selected
production root body is injected exactly once. RNA then traverses all edge
types two hops in both directions; complete minified records are retained in
structural traversal order under an 8 KiB injection cap. T5 does not re-filter
traversed records by lexical overlap. It abstains before provider spend if the
root body is absent, fewer than three complete graph records fit, no
callable/type neighbor survives, or the complete prompt exceeds 16 KiB.

Ranks 10 and 13 were predeclared offline preflights. Rank 10 selected
`django/contrib/admin/checks.py:_check_list_display_item:function` only after
two earlier candidates abstained for lacking `admin.E108`; four graph records
fit, producing a 4,819-byte injection and 10,326-byte prompt. Rank 13 selected
`sympy/printing/latex.py:_print_Subs:function`; nine of 13 traversed records
fit, producing an 8,037-byte injection and 8,696-byte prompt. Both passed the
offline prompt audit. Only rank 10 was authorized for paid execution, on
Sonnet and Spark, and its existing anti-slop A controls were reused. Exactly
two paid T5 cells were run; no population execution was authorized.

Two earlier rank-10 setup attempts are provenance only and consumed no model
tokens. Spark's first launcher inherited an obsolete 32,768-byte manifest cap
that conflicted with the new 16 KiB contract. Sonnet's first launcher inherited
a `sandbox-exec` profile that denied the logged-in CLI's ordinary
`fstat(stderr)` call. The final runners removed those obsolete constraints,
preserved the audited prompt bytes exactly, and passed transcript audit.

The inherited 999-byte treatment-plus-anti-slop directive remained unchanged
for this gate. It says both to use the already-injected context and to use an
"available" `rna_tool_search` command for further navigation. That second
claim was false in the Spark App Server environment and is treated as a
residual treatment/runner design defect. The report therefore stops T5 after
one case and specifies a directive-only follow-up gate rather than scaling.

A later receipt/runtime audit found that T5's reused A controls were not
runtime-matched. Sonnet's control and T5 cell used different runner/isolation
policies. Spark T5 alone used an isolation revision that skipped setuptools'
`distutils` `.pth` hook; its first test failed and 35 command calls followed
that runtime failure. T5's two independent RESOLVED verdicts remain valid, but
its A/T5 efficiency deltas are designated `RUNTIME_CONFOUNDED` and are not
interpreted as treatment effects.

### Matched A6/T6 causal-replay micro-gate

T6 is the directive-only, same-runtime follow-up. It is outside the 720
canonical cells and was frozen to exactly one case (rank 10), two models
(Sonnet and Spark), and four fresh paid cells. No population run was
authorized. To balance temporal order, Sonnet ran A6 then T6 and Spark ran T6
then A6; every episode used a fresh checkout.

A6 used the 5,507-byte standard SWE-bench task and the 808-byte anti-slop
strategy. T6 used the exact 10,326-byte T5 prompt—4,819 bytes of compact RNA
interaction followed by the byte-identical task—and a 1,070-byte developer
instruction. The T6-only directive states that the RNA interaction at the
start of the prompt already executed, treats its search and two-hop result as
context, and prohibits invoking RNA or `rna_tool_search` during the episode.
The anti-slop suffix is otherwise byte-identical.

Within a model, both arms used the same runner, model/effort, checkout and base
tree, tool surface, network policy, base instructions, strategy, Python policy,
and evaluator. Spark used a per-cell virtual environment with shared
site-packages on `PYTHONPATH`; because that isolation intentionally does not
execute host `.pth` hooks, its `sitecustomize` installs setuptools'
`_distutils_hack` shim explicitly. A zero-spend preflight required
`import distutils.version` through the exact isolated interpreter before a
provider could start. The same check passed in A6 and T6. Sonnet used the same
direct logged-in CLI runner and global Python policy for both arms.

Admission required exact prompt/developer hashes, A/T case/commit/tree parity,
runtime preflight, complete provider transcript, zero follow-up RNA calls, a
terminal patch, and stock SWE-bench evaluation. All four cells passed. The
same request-level replay formula above was then applied to the matched pair;
no post-hoc cell filtering or scale execution followed.

### Twenty-case bounded progressive-disclosure treatment

The population follow-up (`T_PD`, also called condition B during design) runs
the frozen Sonnet ranks 1–20. T_PD contributes exactly 20 paid treatment
episodes: the 19
strict-qualified episodes completed in the original population attempt plus
the previously missing rank-12 episode. Every treatment patch was evaluated
with stock SWE-bench 4.1.0 using the canonical per-instance image tag.

The first analysis reused canonical Sonnet A controls. A later audit found a
material prompt-channel mismatch: those A episodes used a replacement
`--system-prompt-file`, while T_PD kept Claude Code's native tuned system
prompt and added RNA guidance through `--append-system-prompt-file`.
Consequently, the old A/T_PD efficiency contrast is retained only as
historical provenance and is not used for causal interpretation.

The repair condition, `A_prime`, contributes 20 fresh paid controls. For each
rank, A_prime uses the exact T_PD Claude CLI command, pinned CLI version,
model, effort, tools, MCP configuration, permissions, plugins, and init
surface after removing only `--append-system-prompt-file` and its RNA guidance
path. Session IDs are necessarily fresh. A_prime receives the standard
SWE-bench user task; T_PD receives that task plus the declared bounded RNA
payload. All 20 per-rank command-parity and init-surface checks passed before
admission. T_PD was not rerun, so the repaired comparison remains temporally
separated and does not eliminate provider/model sampling variance.

The control is the standard SWE-bench task with ordinary repository, edit, and
test tools. T_PD keeps that task and tool surface, and adds only:

1. a developer instruction explaining that RNA is a preferred repository
   representation and that the worked search result should guide initial
   localization; and
2. one prepended, verbatim `rna_tool_search(<deterministic query>)` call and
   result containing as many **complete** hybrid-search/rerank records as fit
   within an 8,192-byte consumer-side ceiling.

Records retain stable node IDs for optional caller-selected expansion. The
initial payload includes no automatic traversal root or graph dump; ordinary
repository tools remain available. The deterministic query is the issue title
plus its normalized first 512 characters. If that query returns no admissible
records, the predeclared fallback is the title alone. Rank 12 used that
fallback because its title-plus-512 query returned no records; the title-only
query returned 11 complete, relevant records within 8,188 visible bytes.

Nineteen inputs passed the later strict cache-qualification gate. Rank 12 uses
an earlier retained semantic-and-reranked title-only capture whose raw bytes
and projected bytes are hash-bound. The later gate rejected its cache
attachment because the reranker model overlay was symlinked and the cache
lacked a strict LSP-completeness receipt—not because retrieval failed. Rank 12
therefore constitutes a disclosed cache-attachment exception under the same
model-visible 8 KiB contract, not a post-hoc treatment change. Its completed
episode binds the corrected V2 input manifest exactly.

Admission requires prompt and developer-instruction hashes, a completed
provider transcript with no timeout, transcript-derived tool recount, terminal
patch, and patch-hash-bound official verdict. Preprocessing exposure is
reported separately and is not counted as a model tool call. The model need
not issue a follow-up RNA call: the tested hypothesis is that the already
injected modeled representation reduces subsequent repository work. All 20
T_PD transcripts made zero follow-up RNA calls.

A_prime admission additionally requires successful completion, a non-empty
terminal patch, zero provider retries in the admitted receipt, exact
command/init parity with the paired T_PD episode after treatment-append
removal, and a stock evaluator verdict. Nineteen CLI invocations exhausted
their internal HTTP 529 overload retries before any Sonnet episode output,
tool call, or patch; they are quarantined as provider-failure provenance and
are not experimental episodes. Their receipts report small Haiku helper-model
usage totaling $0.023380, which is disclosed separately from admitted-episode
cost. No completed Sonnet output or patch was discarded.

The RNA preprocessing logs are append-only operational files. Later D/E
diagnostic preflights appended calls to the rank 6, 16, and 18 files after the
T_PD prompts had already been frozen. The evidence manifest therefore binds
the original T_PD log prefix by exact byte count and SHA for those three
ranks; later lines remain provenance but are not attributed to T_PD.

Model wall time, issue-specific RNA retrieval time, and their sum are reported
separately. Full repository extraction, embedding, reranking-model setup, and
LSP enrichment are excluded from per-issue latency because they are reusable
repository preprocessing. No RNA source, public default, producer, reranker,
case order, or cohort changed during this population run. The corrected
A_prime/T_PD comparison is the primary bounded-population analysis; the
legacy reused-A contrast is explicitly superseded.

### Three-case D/E compact progressive-disclosure diagnostic

After the matched A6/T6 gate, a separate Sonnet-only diagnostic tested whether
bounded search results could serve as an index for caller-selected expansion.
Ranks 6, 16, and 18 were frozen for this mechanism check before either D or E
execution. Their existing canonical A observations were reused; no A episode
was rerun. D and E each added three paid treatment episodes. This selected
three-case pilot is explicitly outside the 720 canonical cells and cannot
estimate a population effect.

D used the unchanged warm exact-tree caches, strict hybrid retrieval with the
unchanged reranker, and a hard 4,096-byte consumer-side response ceiling. The
initial response contained as many complete metadata records as fit, including
stable IDs but no bodies. The harness injected no graph and selected no
automatic traversal root. It exposed bounded focused search and caller-selected
one-hop, bidirectional expansion through an attempt-local RNA CLI wrapper.
Ordinary repository, edit, and test tools remained available.

E reused every D user-prompt byte exactly. In both arms the prompt began with
the deterministic title-plus-normalized-first-512 query represented as
`rna_tool_search(<query>)`, immediately followed by its compact result and then
the standard SWE-bench task. E changed only the developer wording and
attempt-local wrapper path: it said to prefer RNA over Grep/Read for discovery
and to continue the demonstrated call/result pattern when more context was
needed. Live zero-spend preflight reproduced each D compact result byte-for-byte
before E model execution.

Both arms used Claude Sonnet 5 through the logged-in CLI, the ordinary
unrestricted episode runner, the same case commit/tree, a fresh episode
checkout, and the stock SWE-bench 4.1.0 evaluator with the canonical image tag.
No RNA source, producer, cache, reranker, or default changed. Preprocessing time
is reported separately and added to model time for end-to-end accounting; it
is not counted as a model tool call.

Admission required successful provider completion, exact prompt hashes, a
terminal patch, transcript/tool recount, an official patch-hash-bound verdict,
and explicit accounting of direct, cache-write, cache-read, output, cost, and
tools by type. All six D/E cells passed those gates. Neither transcript invoked
the RNA wrapper, so the observed treatment is compact pre-injected context;
interactive expansion remains untested. No D/E expansion-arm scale run or RNA
default change was authorized from this diagnostic; the separate T_PD
population run above tests bounded pre-injected context, not interactive
expansion.

## Model runtimes

- **Sonnet:** Claude Sonnet 5 through the logged-in Claude CLI.
- **Luna:** GPT-5.6 Luna through Codex App Server.
- **Haiku:** Claude Haiku 4.5 (`claude-haiku-4-5-20251001`) through the
  logged-in Claude CLI, using the same audited Claude runner as Sonnet.
- **Spark:** GPT-5.3 Codex Spark (`gpt-5.3-codex-spark`, high effort) through
  Codex App Server, with a 128,000-token nominal and 121,600-token effective
  context window.

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
  traversal; and
- Claude terminal-patch extraction relative to `HEAD`, which loses valid work
  when Haiku commits its fix. Haiku patches are instead reconstructed
  deterministically as the frozen base commit versus the final worktree, with
  untracked files included via intent-to-add; the original head-relative
  receipts and patches remain retained provenance.

Affected attempts are not canonical evidence. Prompt construction was frozen
and byte-audited before replacement episodes. Luna used a predeclared
34-cell clean-runtime replacement matrix; the remaining six Luna cells were
retained only after strict runtime/transcript audit. Selection of replacements
preceded exact-patch efficacy evaluation.

The prompt-strategy scale run inherited a separate 1,200-second Claude wrapper
ceiling even though its declared comparison window was two hours. It censored
one rank-3 `T_PF_sonnet` episode while the model's chosen regression suite was
still running. The censored receipt is provenance only. The exact same frozen
cell was rerun under the corrected 7,200-second window and passed transcript
audit and official evaluation. Every earlier completed Claude strategy cell
finished below 1,200 seconds, so the inherited ceiling did not bind them.
Codex App Server strategy cells retain no model or tool timeout.

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
verdict. The same contract applies to all 120 weaker-model cells.

The 480 prompt-strategy cells use the same evaluator module, dataset revision,
instance images, 1,800-second per-instance timeout, patch bytes, and verdict
rules. They set the stock evaluator's official `--cache_level instance` option
so concurrent rank lanes retain their own instance images. That setting changes
image cleanup/reuse only; it does not change tests or container execution. A
zero-model batch attempted while Docker Desktop was stopped and a later
two-lane batch affected by the stock harness's cross-process image-cleanup race
are retained as noncanonical derived provenance. Three later evaluations also
collided with stopped containers left by an interrupted attempt because the
stock harness deterministically reused their container names; those zero-model
errors were quarantined, the exact stopped containers were removed, and only
the three missing patch hashes were retried. A separate completed rank-18 test
run was censored while the stock harness assembled its JSON report: one process
listed containers while another process removed one of those containers, and
the resulting Docker 404 aborted report generation. Its patch verdict was not
reconstructed from logs; that exact patch hash was evaluated again in the
serial reconciliation. None of these evaluator failures produced or changed a
provider episode. Canonical efficacy uses only completed race-free evaluator
receipts bound to the exact terminal-patch SHA-256.

## Metrics

The report includes, per cell and by condition:

- efficacy (`RESOLVED`/`UNRESOLVED`);
- elapsed wall time;
- input and output tokens;
- cached and uncached input when exposed by the provider;
- output reasoning-token subsets when exposed by the provider;
- model cost; and
- tool-call totals and types, independently recounted from transcripts.

For the prompt-strategy comparison, the report also applies SlopCodeBench's
pinned deterministic verbosity and erosion metrics to changed Python files in
each terminal patch. Metrics are computed before and after the patch for
`all`, `production`, and `test` scopes; production-only effects are emphasized
so added tests do not masquerade as production-code quality. The wrapper
selects the snapshot entry file with the most Python callable definitions,
then breaks ties by non-`__init__.py`, bytes, and path, and records the selected
entry. Ast-grep must execute its pinned rules and the dependency graph must be
successfully returned. A zero-node graph is retained as a valid metric value,
as defined by the upstream implementation; it is not converted into a missing
or failed observation.

For Sonnet, cost is the sum of provider-receipt model entries. For Luna, cost
is an API-equivalent estimate using the published standard prices captured by
the evidence manifest; it is not a claim about App Server billing. Haiku cost
is the provider-receipt total and is cross-checked against its per-model usage
entry. Spark cost is reported as unavailable: App Server exposes no episode
cost for these runs and Spark has no published API rate, so the report does not
invent one.

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
All 120 weaker-model transcripts pass exact prompt hashing, execution and
terminal-patch validation, independent tool recounting, and foreign-rank
contamination checks. Spark additionally passes App Server item reconciliation
and exact context-window validation.
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

The weaker-model execution is explicitly a post-hoc sensitivity analysis. It
can reveal differential performance hidden by a ceiling, but it does not turn
the repaired study into a preregistered experiment. Haiku and Spark also differ
in context window, model capability, provider scaffolding, and native tools;
only A/T/T2 comparisons within one model are treated as paired effects.
