# Canonical 20-case × 36-condition report

Status: **COMPLETE**. Canonical cells: **720/720**; officially evaluated: **720/720**.

This is the single reader-facing report for all 36 conditions on the same 20
frozen cases: A, T, and T2 on Claude Sonnet 5, GPT-5.6 Luna, Claude Haiku 4.5,
and GPT-5.3 Codex Spark, each without an added strategy and with the published
HumanLayer/SlopCodeBench `anti_slop` (AS) and `plan_first` (PF) strategies.

The original 80-cell A/T evidence and all 240 strategy-free cells remain
unchanged. The additive strategy factorial contributes 480 cells. Every
canonical cell is officially evaluated; the generated unified detail section
reports success, time, ordinary and cached input, cache writes, output, cost
availability, quality, and tool counts by tool type for every case and
condition.

The Sonnet/Luna T2 extension executed 42 paid provider episodes: 40 canonical
cells plus two superseded attempts affected by the documented projection-parser
harness defect. The weaker-model extension required no paid reruns. The new
strategy factorial's separate 499-attempt accounting appears in METHOD.md.

The strategy factorial is post hoc and is analyzed through paired same-case
comparisons against the matching strategy-free condition. Cross-model totals
remain descriptive because provider scaffolding and native tools differ.

Read [METHOD.md](METHOD.md) for treatment construction, canonicalization,
runtime isolation, quality scoring, and limitations.

## Executive result

Each cell below is `A / T / T2` resolved; token changes are treatment versus
the matching A on the same 20 cases. AS and PF are semantic ports of the
published HumanLayer/SlopCodeBench strategies, not an exact reproduction of
that benchmark's model, tasks, or harness.

| Backend | Strategy | Resolved A / T / T2 | Total tokens T / T2 vs A | Time T / T2 vs A | Tools T / T2 vs A |
|---|---|---:|---:|---:|---:|
| Sonnet | base | 17 / 18 / 17 | +2.9% / +19.8% | −16.2% / −7.8% | −17.4% / −5.4% |
| Sonnet | anti-slop | 15 / 15 / 16 | +58.6% / +61.4% | +2.1% / −0.0% | +2.0% / +5.5% |
| Sonnet | plan-first | 15 / 16 / 16 | +13.6% / +3.2% | −14.2% / −5.7% | −14.2% / −10.1% |
| Luna | base | 18 / 19 / 17 | +2.5% / −0.4% | +4.4% / −3.0% | −10.1% / −9.8% |
| Luna | anti-slop | 19 / 18 / 18 | +29.5% / +16.0% | +4.6% / −4.4% | −5.1% / −15.2% |
| Luna | plan-first | 18 / 17 / 17 | +10.4% / +27.5% | −8.2% / +3.1% | −9.5% / −1.4% |
| Haiku | base | 15 / 15 / 16 | +22.9% / +33.9% | −2.3% / +3.5% | +3.4% / +8.3% |
| Haiku | anti-slop | 15 / 16 / 15 | +8.6% / +5.4% | +0.6% / −1.2% | −6.5% / −4.5% |
| Haiku | plan-first | 16 / 15 / 14 | +26.2% / +15.9% | +9.3% / −2.7% | +1.5% / −3.0% |
| Spark | base | 15 / 17 / 16 | +74.8% / +2.1% | +18.5% / −16.7% | +13.8% / −12.8% |
| Spark | anti-slop | 14 / 14 / 15 | +20.2% / −0.4% | −5.1% / −8.2% | −3.9% / −11.7% |
| Spark | plan-first | 15 / 14 / 15 | +24.6% / +12.6% | −0.7% / −6.1% | −6.1% / −13.0% |

The raw 40%-token-reduction hypothesis is not supported. The HumanLayer-derived
strategies add headroom but do not produce a general RNA efficacy rescue:
same-case gains and losses remain sparse and model-dependent. The most favorable
strategy interaction is Spark anti-slop A→T2: one net resolution, 0.4% fewer
tokens, 8.2% less time, and 11.7% fewer tools. Sonnet plan-first A→T2 adds one
net resolution and reduces time/tools, but uses 3.2% more total tokens.

Most treatment token growth is cumulative cache-read/input replay across turns,
not model output. For example, Sonnet plan-first T/T2 output falls 12.9%/16.5%
while total tokens rise 13.6%/3.2%. These are descriptive single-run paired
results over 20 deliberately selected cases; no pooled population effect or
prompt-strategy causality is claimed.

## Post-hoc working-set gate: stopped after one case

After the 720 canonical cells were complete, two bounded RNA working-set
prototypes were tested on rank 3 (`sympy__sympy-13757`) under anti-slop using
the already-complete A controls. This is an implementation diagnostic outside
the canonical 720-cell analysis, not another population estimate. Only four
new paid cells were run: T3 and T4 on Sonnet and Spark. The requested T4 gate
stopped after exactly those two model cells; no scale run followed.

All six A/T3/T4 cells were transcript-audited and officially RESOLVED. T3 used
a 10,839-byte compact working-set prompt. T4 reduced that to 2,610 bytes by
querying `Poly multiplication`, selecting `test_Poly_mul`, and exposing only
the lexically matching `mul` and `mul_ground` graph records. T4 materially
improved on T3 for Sonnet, but it still used 40.8% more total tokens and 21.4%
more tools than A. Spark regressed sharply.

| Backend | Cell | Resolved | Prompt bytes | Input tokens | Output tokens | Total tokens | Time | Cost | Tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| Sonnet | A-AS | yes | 513 | 566,804 | 10,825 | 577,629 | 187.7s | $0.480654 | 28 (Bash=17, Edit=3, Read=8) |
| Sonnet | T3-AS | yes | 10,839 | 1,422,546 | 10,815 | 1,433,361 | 205.5s | $0.875044 | 42 (Bash=35, Edit=2, Read=5) |
| Sonnet | T4-AS | yes | 2,610 | 805,618 | 7,409 | 813,027 | 150.4s | $0.552445 | 34 (Bash=22, Edit=2, Grep=6, Read=4) |
| Spark | A-AS | yes | 513 | 849,590 | 15,621 | 865,211 | 81.6s | n/a | 44 (commandExecution=40, fileChange=4) |
| Spark | T3-AS | yes | 10,839 | 1,280,754 | 12,636 | 1,293,390 | 77.5s | n/a | 42 (commandExecution=40, fileChange=2) |
| Spark | T4-AS | yes | 2,610 | 3,242,977 | 21,930 | 3,264,907 | 171.6s | n/a | 79 (commandExecution=74, fileChange=2, webSearch=3) |

Input is cumulative context processed across turns. Its decomposition makes
the failure mode explicit:

| Backend | Cell | Ordinary input | Cache write | Cache read | Uncached input | Tool-result characters |
|---|---|---:|---:|---:|---:|---:|
| Sonnet | A-AS | 785 | 25,918 | 540,101 | 26,703 | 33,874 |
| Sonnet | T3-AS | 4,410 | 49,645 | 1,368,491 | 54,055 | 51,949 |
| Sonnet | T4-AS | 1,589 | 34,832 | 769,197 | 36,421 | 64,052 |
| Spark | A-AS | 36,534 | 0 | 813,056 | 36,534 | 46,825 |
| Spark | T3-AS | 46,578 | 0 | 1,234,176 | 46,578 | 78,709 |
| Spark | T4-AS | 79,841 | 0 | 3,163,136 | 79,841 | 100,182 |

Relative to the matching A, T4 changed Sonnet input/output/time/tools by
+42.1%/−31.6%/−19.9%/+21.4%; Spark changed them by
+281.7%/+40.4%/+110.4%/+79.5%. Spark's 2.1-KiB injected context was therefore
not the direct token burden. It induced 35 additional tool calls and more than
doubled tool-result output; the growing conversation was then replayed as
3.16M cached-input tokens.

The transcript review identifies a treatment-design bug. T4's lexical
projection selected a test root and discarded structurally relevant nodes
whose names did not repeat the task words. Both models ultimately fixed the
operator-dispatch layer (`_op_priority`), not the injected `mul`/`mul_ground`
implementation path. Spark spent most of its excess trajectory rediscovering
that dispatch mechanism and repairing its local Python test environment. One
late `rm -rf distutils` cleanup command was rejected by App Server policy and
replaced with a Python cleanup; that event is real runner friction, but it
occurred after the dominant exploratory expansion and cannot explain the
35-call difference by itself.

The decision is **DO NOT SCALE T4**. The next candidate must include the
selected root body, prefer a confident production protocol root, retain
structurally relevant control-flow records rather than applying lexical
overlap again after traversal, and abstain instead of injecting a low-confidence
graph. Offline rank-3 probes show why: the deterministic operator-aware query
`Poly __mul__ __rmul__` ranks the production `polytools.__rmul__` first, while
a two-hop traversal from `Expr.__mul__` exposes `call_highest_priority` and
`binary_op_wrapper`. Because those probes were derived after inspecting rank
3, they are design evidence only; any next paid gate must freeze the rule and
use different predeclared cases.

The path-free diagnostic ledger is
[`working-set-diagnostic-results.json`](working-set-diagnostic-results.json),
with retained artifacts bound by
[`working-set-diagnostic-evidence-manifest.json`](working-set-diagnostic-evidence-manifest.json).

## Follow-up causal-working-set gate: split result on one case

The replayed-input diagnosis was not disproved by T4; work on it stopped when
T4 exposed a flawed lexical projection. A corrected T5 producer was therefore
frozen and tested only through the small gate requested: two offline preflight
cases (ranks 10 and 13), followed by exactly two paid T5 cells on rank 10
(`django__django-11551`), one each on Sonnet and Spark. The existing rank-10
anti-slop A controls were reused. No full sweep was launched.

T5 starts from the frozen 50-record strict hybrid/reranked title-plus-512
result, admits only production callable/type roots, and uses exact matching
only for explicit underscore-bearing identifiers. Otherwise it scores symbol
and path overlap with the frozen rerank order as the tie-break. An explicit
diagnostic such as `admin.E108` must occur in the selected root body. T5 then
injects that root body once and complete all-edge, bidirectional, two-hop graph
records in structural order under an 8 KiB injection and 16 KiB full-prompt
cap. It never lexically re-filters the traversed graph and abstains when the
bounded projection lacks a root body, three complete records, or a
callable/type neighbor.

For rank 10 the deterministic traversal root was
`django/contrib/admin/checks.py:_check_list_display_item:function`; the two
earlier candidates abstained because their bodies did not contain
`admin.E108`. Four complete graph records fit. The 4,819-byte injection
produced a 10,326-byte prompt, byte-identical across Sonnet and Spark. Rank 13
also passed offline preflight with `_print_Subs`, an 8,037-byte injection, and a
8,696-byte prompt, but received no provider call.

Both paid T5 patches passed transcript audit and the stock SWE-bench evaluator.

| Backend | Cell | Resolved | Prompt bytes | Input tokens | Uncached input | Cache read | Output tokens | Total tokens | Time | Cost | Tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| Sonnet | A-AS | yes | 5,507 | 334,857 | 21,758 | 313,099 | 6,799 | 341,656 | 117.1s | $0.315745 | 17 (Bash=7, Edit=3, Grep=3, Read=4) |
| Sonnet | T5-AS | yes | 10,326 | 102,892 | 20,784 | 82,108 | 1,685 | 104,577 | 21.5s | $0.156994 | 6 (Bash=3, Edit=1, Glob=1, Read=1) |
| Spark | A-AS | yes | 5,507 | 967,970 | 42,914 | 925,056 | 10,797 | 978,767 | 96.6s | n/a | 44 (commandExecution=40, fileChange=4) |
| Spark | T5-AS | yes | 10,326 | 1,815,758 | 46,286 | 1,769,472 | 15,964 | 1,831,722 | 137.1s | n/a | 60 (commandExecution=56, fileChange=4) |

Relative to the matching A, Sonnet T5 reduced input/output/total tokens by
69.3%/75.2%/69.4%, time by 81.6%, tools by 64.7%, cache-read input by 73.8%,
and reported cost by 50.3%. Spark T5 instead increased those token fields by
87.6%/47.9%/87.1%, time by 41.9%, tools by 36.4%, and cache-read input by
91.3%. Uncached input moved only −4.5% for Sonnet and +7.9% for Spark. That is
the key causal diagnostic: the treatment prefix itself is not large enough to
explain either result; the model's ensuing trajectory determines how many
times the conversation is replayed.

The transcripts explain the split. Sonnet used the injected
`_check_list_display_item` context immediately, completed in six tool calls,
and passed 54 relevant tests. Spark also began at the exact injected location,
so it did not ignore the context. It then spent three calls probing a missing
`rna_tool_search` executable because the inherited directive inaccurately says
that command is available, and most of its remaining 56 command calls fought
the old Django checkout's Python 3.12 `distutils` incompatibility. It still
produced a resolved patch. These are model/runner interaction effects, not
evidence that 4.8 KiB of RNA context directly cost 853,111 extra tokens.

The decision is **DO NOT SCALE T5 YET**. This one case shows that the
treatment-design bug was real and that the corrected working set eliminated
the replay explosion on Sonnet's observed trajectory; it does not establish a
population effect or
cross-model robustness. The next minimal gate should change only the directive
to state that the injected RNA interaction has already executed and no
follow-up RNA command is required, then test one different preflight case on
Sonnet and Spark under an aligned deterministic test runtime. A scale decision
comes only after that transcript audit.

The path-free T5 ledger is
[`causal-working-set-diagnostic-results.json`](causal-working-set-diagnostic-results.json),
with retained artifacts bound by
[`causal-working-set-diagnostic-evidence-manifest.json`](causal-working-set-diagnostic-evidence-manifest.json).

For continuity with the preregistered two-model A/T analysis, its original
executive rows are retained verbatim:

| Backend | A resolved | T resolved | Total tokens A→T | Time A→T | Cost A→T | Tools A→T |
|---|---:|---:|---:|---:|---:|---:|
| Sonnet | 17/20 | **18/20** | +2.9% | **−16.2%** | +7.7% | **−17.4%** |
| Luna | 18/20 | **19/20** | +2.5% | +4.4% | **−1.5%** | **−10.1%** |

## Weaker-model sensitivity: Haiku and Spark

Drazen's ceiling-effect concern motivated this additive sensitivity run: the same frozen 20 cases and byte-identical A, T, and T2 user prompts were run on Claude Haiku 4.5 and Codex Spark. Conditions were serialized within case in a deterministic rotating A/T/T2 order while cases ran concurrently. No original Sonnet/Luna cell was rerun.

The two change columns below are **A→T / A→T2**. These are within-model paired comparisons; cross-model totals remain descriptive because the Claude CLI and Codex App Server expose different native scaffolding and tools.

| Backend | A resolved | T resolved | T2 resolved | Total-token change | Time change | Tool-count change | Cost change |
|---|---:|---:|---:|---:|---:|---:|---:|
| Claude Haiku 4.5 | 15/20 | 15/20 | 16/20 | +22.9% / +33.9% | -2.3% / +3.5% | +3.4% / +8.3% | +16.7% / +22.9% |
| Codex Spark | 15/20 | 17/20 | 16/20 | +74.8% / +2.1% | +18.5% / -16.7% | +13.8% / -12.8% | n/a |

Haiku resolves 15/20 under A, 15/20 under T, and 16/20 under T2. Spark resolves 15/20, 17/20, and 16/20 respectively. This weaker-model sensitivity exposes efficacy differences that the higher Sonnet/Luna baseline partly obscured; it does not imply that one context projection dominates for every model.

Aggregated descriptively across the two weaker backends, A resolves 30/40 cases, original T resolves 32/40, and T2 resolves 32/40. This is not a pooled causal estimate: Spark gains two resolutions from original T, whereas Haiku gains one from T2. T2 is close to A in Spark tokens and reduces Spark time/tools, but it is the most token- and cost-intensive Haiku arm.

The original T prompts total 457,051 bytes across 20 cases, versus 361,716 for T2. Because that prefix is processed again across turns, the smaller T2 projection and any reduction in interaction can matter far more than the one-time prompt-byte difference—especially for Spark's smaller context window.

Spark cost is reported as unavailable rather than estimated: the App Server does not expose episode cost and this non-API model has no published API rate. Haiku cost is the provider-reported Claude CLI cost and is cross-checked against each receipt's per-model cost total.

### Weaker-model paired efficacy changes

| Comparison | Baseline resolved | Treatment resolved | Baseline-only | Treatment-only | Same verdict |
|---|---:|---:|---:|---:|---:|
| Haiku A→T | 15/20 | 15/20 | 1 | 1 | 18 |
| Haiku A→T2 | 15/20 | 16/20 | 1 | 2 | 17 |
| Spark A→T | 15/20 | 17/20 | 0 | 2 | 18 |
| Spark A→T2 | 15/20 | 16/20 | 0 | 1 | 19 |

### Weaker-model aggregate totals

| Condition | Resolved | Input tokens | Output tokens | Total tokens | Wall time | Cost | Tools |
|---|---:|---:|---:|---:|---:|---:|---:|
| A_haiku | 15/20 | 34,725,218 | 315,924 | 35,041,142 | 3962.5s | $6.943730 | 990 |
| T_haiku | 15/20 | 42,753,244 | 324,795 | 43,078,039 | 3870.3s | $8.103202 | 1,024 |
| T2_haiku | 16/20 | 46,593,924 | 320,063 | 46,913,987 | 4100.2s | $8.536925 | 1,072 |
| A_spark | 15/20 | 18,205,305 | 222,867 | 18,428,172 | 1580.5s | n/a | 749 |
| T_spark | 17/20 | 31,940,798 | 279,222 | 32,220,020 | 1872.1s | n/a | 852 |
| T2_spark | 16/20 | 18,613,051 | 210,738 | 18,823,789 | 1316.7s | n/a | 653 |

### Weaker-model input/output decomposition

| Condition | Uncached input | Cached input | Output | Reasoning-output subset |
|---|---:|---:|---:|---:|
| A_haiku | 1,010,471 | 33,714,747 | 315,924 | n/a |
| T_haiku | 1,253,421 | 41,499,823 | 324,795 | n/a |
| T2_haiku | 1,275,674 | 45,318,250 | 320,063 | n/a |
| A_spark | 819,065 | 17,386,240 | 222,867 | 162,991 |
| T_spark | 1,137,982 | 30,802,816 | 279,222 | 205,944 |
| T2_spark | 819,387 | 17,793,664 | 210,738 | 158,945 |

### Weaker-model per-case efficacy and efficiency

Each cell reports official SWE-bench efficacy, elapsed wall time, cumulative input tokens split into uncached and cached input, output tokens, and cost. Input is cumulative context processed over turns, not unique prompt bytes.

#### Claude Haiku 4.5 metrics

| Rank | Case | A-Haiku | T-Haiku | T2-Haiku |
|---:|---|---|---|---|
| 1 | `sympy__sympy-23534` | success yes · 181.4s · in 1,064,110 (uncached 39,633, cached 1,024,477) · out 12,877 · $0.245034 | success yes · 185.0s · in 1,516,587 (uncached 68,400, cached 1,448,187) · out 11,513 · $0.331103 | success yes · 184.8s · in 1,748,521 (uncached 64,004, cached 1,684,517) · out 12,776 · $0.353791 |
| 2 | `django__django-11179` | success yes · 128.5s · in 1,090,962 (uncached 44,053, cached 1,046,909) · out 9,262 · $0.238198 | success yes · 141.1s · in 1,367,484 (uncached 61,528, cached 1,305,956) · out 11,539 · $0.302155 | success yes · 151.7s · in 1,761,518 (uncached 56,586, cached 1,704,932) · out 10,749 · $0.327936 |
| 3 | `sympy__sympy-13757` | success yes · 237.4s · in 2,122,727 (uncached 53,486, cached 2,069,241) · out 19,849 · $0.411869 | success yes · 395.1s · in 5,575,752 (uncached 91,341, cached 5,484,411) · out 29,756 · $0.868122 | success yes · 279.5s · in 3,046,989 (uncached 74,193, cached 2,972,796) · out 22,463 · $0.548954 |
| 4 | `django__django-13033` | success no · 462.5s · in 4,737,283 (uncached 88,482, cached 4,648,801) · out 43,144 · $0.855024 | success yes · 311.7s · in 3,471,018 (uncached 88,053, cached 3,382,965) · out 30,801 · $0.657278 | success yes · 354.1s · in 6,594,771 (uncached 109,241, cached 6,485,530) · out 29,266 · $1.003386 |
| 5 | `pydata__xarray-4075` | success yes · 140.2s · in 1,248,402 (uncached 59,097, cached 1,189,305) · out 10,774 · $0.289354 | success yes · 184.5s · in 2,206,386 (uncached 73,630, cached 2,132,756) · out 14,487 · $0.424076 | success yes · 275.8s · in 3,196,780 (uncached 77,137, cached 3,119,643) · out 20,893 · $0.562329 |
| 6 | `django__django-13794` | success no · 206.1s · in 2,056,009 (uncached 60,658, cached 1,995,351) · out 17,742 · $0.408506 | success no · 275.4s · in 2,968,731 (uncached 63,183, cached 2,905,548) · out 23,139 · $0.522601 | success no · 166.3s · in 1,882,600 (uncached 61,130, cached 1,821,470) · out 12,719 · $0.360713 |
| 7 | `matplotlib__matplotlib-24026` | success yes · 278.8s · in 2,237,833 (uncached 58,193, cached 2,179,640) · out 26,489 · $0.464988 | success yes · 342.9s · in 3,654,347 (uncached 75,959, cached 3,578,388) · out 30,650 · $0.655189 | success yes · 385.8s · in 2,992,068 (uncached 74,431, cached 2,917,637) · out 26,014 · $0.563294 |
| 8 | `django__django-11163` | success yes · 182.1s · in 1,929,543 (uncached 54,075, cached 1,875,468) · out 17,122 · $0.380256 | success yes · 127.2s · in 1,090,202 (uncached 42,832, cached 1,047,370) · out 9,764 · $0.230650 | success yes · 141.7s · in 1,574,552 (uncached 49,896, cached 1,524,656) · out 13,400 · $0.312255 |
| 9 | `django__django-16612` | success yes · 166.2s · in 1,720,959 (uncached 45,290, cached 1,675,669) · out 11,685 · $0.315370 | success yes · 191.3s · in 2,681,622 (uncached 80,772, cached 2,600,850) · out 13,539 · $0.479853 | success yes · 251.3s · in 3,009,385 (uncached 71,652, cached 2,937,733) · out 20,285 · $0.528777 |
| 10 | `django__django-11551` | success yes · 202.9s · in 2,178,313 (uncached 55,642, cached 2,122,671) · out 16,686 · $0.404450 | success yes · 201.3s · in 2,603,131 (uncached 67,598, cached 2,535,533) · out 16,743 · $0.464186 | success yes · 157.8s · in 2,660,554 (uncached 86,986, cached 2,573,568) · out 12,695 · $0.487150 |
| 11 | `django__django-13658` | success yes · 242.6s · in 1,142,765 (uncached 41,375, cached 1,101,390) · out 11,980 · $0.251557 | success yes · 165.4s · in 1,047,009 (uncached 56,786, cached 990,223) · out 9,098 · $0.246779 | success no · 217.6s · in 3,188,940 (uncached 73,717, cached 3,115,223) · out 17,986 · $0.540672 |
| 12 | `psf__requests-1724` | success yes · 361.3s · in 3,022,943 (uncached 66,386, cached 2,956,557) · out 16,380 · $0.508361 | success yes · 185.3s · in 2,280,203 (uncached 56,768, cached 2,223,435) · out 18,918 · $0.420187 | success yes · 198.7s · in 3,190,373 (uncached 74,569, cached 3,115,804) · out 15,259 · $0.528220 |
| 13 | `sympy__sympy-18763` | success no · 195.3s · in 1,297,717 (uncached 42,291, cached 1,255,426) · out 14,291 · $0.280362 | success no · 158.2s · in 1,919,445 (uncached 73,284, cached 1,846,161) · out 12,064 · $0.383291 | success no · 187.4s · in 1,314,420 (uncached 51,136, cached 1,263,284) · out 15,370 · $0.299830 |
| 14 | `pytest-dev__pytest-7982` | success yes · 138.7s · in 1,041,988 (uncached 36,831, cached 1,005,157) · out 12,927 · $0.237803 | success yes · 110.4s · in 860,152 (uncached 34,399, cached 825,753) · out 9,824 · $0.198491 | success yes · 131.5s · in 1,081,268 (uncached 37,223, cached 1,044,045) · out 10,306 · $0.228703 |
| 15 | `pytest-dev__pytest-7432` | success yes · 159.0s · in 1,669,529 (uncached 51,777, cached 1,617,752) · out 15,754 · $0.342979 | success yes · 182.5s · in 1,940,332 (uncached 58,759, cached 1,881,573) · out 18,293 · $0.387809 | success yes · 137.8s · in 1,317,997 (uncached 51,548, cached 1,266,449) · out 13,544 · $0.290316 |
| 16 | `django__django-12193` | success yes · 210.1s · in 2,346,805 (uncached 51,296, cached 2,295,509) · out 18,901 · $0.425391 | success no · 208.5s · in 2,520,022 (uncached 72,353, cached 2,447,669) · out 20,014 · $0.478418 | success yes · 236.9s · in 3,131,298 (uncached 66,515, cached 3,064,783) · out 21,923 · $0.542858 |
| 17 | `django__django-16485` | success yes · 134.3s · in 962,503 (uncached 47,389, cached 915,114) · out 12,862 · $0.249720 | success yes · 126.6s · in 993,797 (uncached 38,249, cached 955,548) · out 10,687 · $0.219888 | success yes · 138.7s · in 934,593 (uncached 39,138, cached 895,455) · out 13,253 · $0.231340 |
| 18 | `django__django-16877` | success no · 138.1s · in 1,353,387 (uncached 41,946, cached 1,311,441) · out 10,569 · $0.266856 | success no · 130.1s · in 1,233,593 (uncached 42,625, cached 1,190,968) · out 11,937 · $0.255874 | success no · 131.5s · in 1,234,604 (uncached 49,029, cached 1,185,575) · out 9,296 · $0.255903 |
| 19 | `django__django-11451` | success yes · 78.1s · in 707,001 (uncached 38,141, cached 668,860) · out 6,835 · $0.176064 | success yes · 132.7s · in 1,714,243 (uncached 58,710, cached 1,655,533) · out 11,337 · $0.329819 | success yes · 253.9s · in 1,455,707 (uncached 55,015, cached 1,400,692) · out 12,120 · $0.302904 |
| 20 | `pytest-dev__pytest-7205` | success no · 118.7s · in 794,439 (uncached 34,430, cached 760,009) · out 9,795 · $0.191588 | success no · 114.9s · in 1,109,188 (uncached 48,192, cached 1,060,996) · out 10,692 · $0.247435 | success yes · 117.2s · in 1,276,986 (uncached 52,528, cached 1,224,458) · out 9,746 · $0.267593 |

#### Codex Spark metrics

| Rank | Case | A-Spark | T-Spark | T2-Spark |
|---:|---|---|---|---|
| 1 | `sympy__sympy-23534` | success yes · 37.3s · in 252,108 (uncached 17,356, cached 234,752) · out 4,616 · n/a | success yes · 67.1s · in 576,033 (uncached 27,809, cached 548,224) · out 6,814 · n/a | success yes · 53.9s · in 512,675 (uncached 23,203, cached 489,472) · out 5,371 · n/a |
| 2 | `django__django-11179` | success yes · 61.7s · in 397,212 (uncached 20,636, cached 376,576) · out 5,048 · n/a | success yes · 74.3s · in 691,214 (uncached 32,142, cached 659,072) · out 5,786 · n/a | success yes · 41.3s · in 459,466 (uncached 23,498, cached 435,968) · out 5,960 · n/a |
| 3 | `sympy__sympy-13757` | success yes · 144.4s · in 1,310,761 (uncached 63,657, cached 1,247,104) · out 15,696 · n/a | success yes · 246.9s · in 4,725,480 (uncached 141,672, cached 4,583,808) · out 24,252 · n/a | success yes · 69.1s · in 1,143,689 (uncached 38,025, cached 1,105,664) · out 8,617 · n/a |
| 4 | `django__django-13033` | success yes · 84.3s · in 1,536,505 (uncached 68,857, cached 1,467,648) · out 19,119 · n/a | success yes · 258.7s · in 6,102,718 (uncached 163,390, cached 5,939,328) · out 47,368 · n/a | success yes · 66.3s · in 1,169,273 (uncached 47,865, cached 1,121,408) · out 15,683 · n/a |
| 5 | `pydata__xarray-4075` | success yes · 75.8s · in 885,504 (uncached 38,656, cached 846,848) · out 12,456 · n/a | success yes · 76.5s · in 1,407,578 (uncached 76,762, cached 1,330,816) · out 18,678 · n/a | success yes · 88.8s · in 1,361,214 (uncached 65,086, cached 1,296,128) · out 17,868 · n/a |
| 6 | `django__django-13794` | success no · 60.6s · in 751,225 (uncached 45,305, cached 705,920) · out 9,822 · n/a | success no · 87.5s · in 1,208,299 (uncached 41,323, cached 1,166,976) · out 14,716 · n/a | success no · 48.7s · in 732,905 (uncached 45,033, cached 687,872) · out 10,351 · n/a |
| 7 | `matplotlib__matplotlib-24026` | success yes · 170.3s · in 2,902,601 (uncached 74,953, cached 2,827,648) · out 25,021 · n/a | success yes · 186.7s · in 4,051,743 (uncached 96,543, cached 3,955,200) · out 25,168 · n/a | success yes · 132.9s · in 2,321,615 (uncached 86,479, cached 2,235,136) · out 18,866 · n/a |
| 8 | `django__django-11163` | success yes · 24.6s · in 145,579 (uncached 10,155, cached 135,424) · out 1,837 · n/a | success yes · 29.5s · in 296,370 (uncached 16,946, cached 279,424) · out 2,361 · n/a | success yes · 32.1s · in 266,807 (uncached 36,791, cached 230,016) · out 2,782 · n/a |
| 9 | `django__django-16612` | success yes · 45.6s · in 422,429 (uncached 21,405, cached 401,024) · out 5,746 · n/a | success yes · 71.7s · in 1,045,119 (uncached 36,991, cached 1,008,128) · out 9,126 · n/a | success yes · 39.9s · in 606,751 (uncached 29,727, cached 577,024) · out 6,029 · n/a |
| 10 | `django__django-11551` | success yes · 70.6s · in 1,052,491 (uncached 45,643, cached 1,006,848) · out 11,933 · n/a | success yes · 77.7s · in 1,103,176 (uncached 61,000, cached 1,042,176) · out 13,035 · n/a | success yes · 102.9s · in 1,432,380 (uncached 45,884, cached 1,386,496) · out 16,475 · n/a |
| 11 | `django__django-13658` | success yes · 94.2s · in 1,037,430 (uncached 54,902, cached 982,528) · out 12,888 · n/a | success yes · 67.2s · in 855,646 (uncached 38,238, cached 817,408) · out 9,884 · n/a | success yes · 85.5s · in 1,625,122 (uncached 51,490, cached 1,573,632) · out 15,070 · n/a |
| 12 | `psf__requests-1724` | success yes · 88.6s · in 1,031,212 (uncached 60,076, cached 971,136) · out 13,983 · n/a | success yes · 93.4s · in 1,415,248 (uncached 49,872, cached 1,365,376) · out 13,621 · n/a | success yes · 66.6s · in 1,194,696 (uncached 44,232, cached 1,150,464) · out 14,895 · n/a |
| 13 | `sympy__sympy-18763` | success no · 69.9s · in 832,732 (uncached 39,644, cached 793,088) · out 13,863 · n/a | success no · 59.1s · in 626,857 (uncached 43,945, cached 582,912) · out 6,083 · n/a | success no · 49.2s · in 549,605 (uncached 28,517, cached 521,088) · out 4,818 · n/a |
| 14 | `pytest-dev__pytest-7982` | success yes · 60.7s · in 515,334 (uncached 39,046, cached 476,288) · out 7,676 · n/a | success yes · 53.8s · in 782,569 (uncached 44,777, cached 737,792) · out 6,583 · n/a | success yes · 34.8s · in 320,708 (uncached 19,268, cached 301,440) · out 4,987 · n/a |
| 15 | `pytest-dev__pytest-7432` | success yes · 49.6s · in 383,283 (uncached 36,659, cached 346,624) · out 8,346 · n/a | success yes · 105.7s · in 2,735,820 (uncached 74,572, cached 2,661,248) · out 27,017 · n/a | success yes · 43.8s · in 499,193 (uncached 24,057, cached 475,136) · out 8,394 · n/a |
| 16 | `django__django-12193` | success no · 113.5s · in 978,096 (uncached 32,816, cached 945,280) · out 11,609 · n/a | success yes · 59.4s · in 970,609 (uncached 38,385, cached 932,224) · out 8,302 · n/a | success yes · 71.3s · in 998,881 (uncached 37,601, cached 961,280) · out 11,194 · n/a |
| 17 | `django__django-16485` | success yes · 39.1s · in 337,963 (uncached 16,299, cached 321,664) · out 5,470 · n/a | success yes · 30.7s · in 232,595 (uncached 16,787, cached 215,808) · out 4,241 · n/a | success yes · 25.5s · in 198,676 (uncached 15,892, cached 182,784) · out 6,681 · n/a |
| 18 | `django__django-16877` | success no · 131.0s · in 1,373,336 (uncached 47,896, cached 1,325,440) · out 13,576 · n/a | success yes · 73.5s · in 1,165,364 (uncached 40,116, cached 1,125,248) · out 12,003 · n/a | success no · 106.0s · in 1,090,795 (uncached 35,947, cached 1,054,848) · out 14,404 · n/a |
| 19 | `django__django-11451` | success yes · 76.1s · in 550,933 (uncached 43,925, cached 507,008) · out 8,616 · n/a | success yes · 61.1s · in 866,316 (uncached 56,332, cached 809,984) · out 12,612 · n/a | success yes · 49.9s · in 750,844 (uncached 56,572, cached 694,272) · out 10,455 · n/a |
| 20 | `pytest-dev__pytest-7205` | success no · 82.7s · in 1,508,571 (uncached 41,179, cached 1,467,392) · out 15,546 · n/a | success no · 91.7s · in 1,082,044 (uncached 40,380, cached 1,041,664) · out 11,572 · n/a | success no · 108.1s · in 1,377,756 (uncached 64,220, cached 1,313,536) · out 11,838 · n/a |

### Weaker-model tool use by case and condition

#### Claude Haiku 4.5 tool counts

| Rank | A-Haiku | T-Haiku | T2-Haiku |
|---:|---|---|---|
| 1 | 38 (Bash=28, Edit=1, Read=5, Write=4) | 33 (Bash=27, Edit=2, Read=4) | 39 (Bash=32, Edit=1, Read=6) |
| 2 | 36 (Bash=28, Edit=1, Read=7) | 33 (Bash=24, Edit=2, Read=7) | 44 (Bash=31, Edit=4, Read=9) |
| 3 | 67 (Bash=46, Edit=7, Read=14) | 106 (Bash=74, Edit=10, Read=22) | 75 (Bash=51, Edit=7, Grep=1, Read=16) |
| 4 | 90 (Bash=62, Edit=10, Read=14, Write=3, bash=1) | 69 (Bash=46, Edit=8, Read=15) | 102 (Bash=63, Edit=18, Read=19, Write=2) |
| 5 | 35 (Bash=27, Edit=2, Read=6) | 47 (Bash=38, Edit=3, Read=5, bash=1) | 64 (Bash=53, Edit=4, Glob=1, Read=6) |
| 6 | 55 (Bash=37, Edit=7, Read=11) | 78 (Bash=56, Edit=9, Read=13) | 47 (Bash=30, Edit=6, Glob=1, Grep=2, Read=8) |
| 7 | 67 (Bash=48, Edit=6, Read=13) | 89 (Bash=65, Edit=6, Read=18) | 70 (Bash=45, Edit=6, Read=17, Write=2) |
| 8 | 49 (Bash=32, Edit=3, Grep=1, Read=8, Write=5) | 38 (Bash=26, Edit=3, Read=9) | 49 (Bash=34, Edit=3, Read=12) |
| 9 | 51 (Bash=37, Edit=2, Glob=1, Grep=1, Read=9, Write=1) | 51 (Bash=36, Edit=3, Read=12) | 72 (Bash=47, Edit=3, Read=22) |
| 10 | 55 (Bash=41, Edit=4, Read=10) | 57 (Bash=32, Edit=8, Read=17) | 45 (Bash=27, Edit=4, Grep=2, Read=12) |
| 11 | 39 (Bash=31, Edit=1, Grep=1, Read=6) | 29 (Bash=23, Edit=1, Read=5) | 63 (Bash=52, Edit=1, Read=10) |
| 12 | 73 (Bash=40, Edit=11, Read=21, Write=1) | 63 (Bash=37, Edit=11, Read=10, Write=5) | 64 (Bash=36, Edit=12, Read=16) |
| 13 | 53 (Bash=43, Edit=2, Read=8) | 37 (Bash=28, Edit=2, Read=7) | 47 (Bash=33, Edit=3, Grep=1, Read=10) |
| 14 | 41 (Bash=35, Edit=1, Grep=1, Read=4) | 32 (Bash=28, Edit=1, Read=3) | 42 (Bash=31, Edit=1, Glob=2, Grep=2, Read=6) |
| 15 | 42 (Bash=32, Edit=4, Read=6) | 47 (Bash=36, Edit=2, Read=9) | 38 (Bash=30, Edit=3, Read=5) |
| 16 | 65 (Bash=49, Edit=2, Read=14) | 57 (Bash=40, Edit=5, Glob=1, Grep=1, Read=10) | 69 (Bash=49, Edit=3, Grep=1, Read=16) |
| 17 | 31 (Bash=22, Edit=2, Read=7) | 36 (Bash=28, Edit=2, Read=6) | 31 (Bash=22, Edit=2, Read=7) |
| 18 | 46 (Bash=33, Edit=2, Read=10, Write=1) | 46 (Bash=28, Edit=2, Read=15, Write=1) | 36 (Bash=25, Edit=1, Grep=1, Read=8, Write=1) |
| 19 | 25 (Bash=17, Edit=1, Read=6, Write=1) | 43 (Bash=28, Edit=5, Read=10) | 37 (Bash=28, Edit=2, Grep=1, Read=6) |
| 20 | 32 (Bash=22, Edit=4, Read=6) | 33 (Bash=21, Edit=5, Read=7) | 38 (Bash=22, Edit=9, Read=7) |

#### Codex Spark tool counts

| Rank | A-Spark | T-Spark | T2-Spark |
|---:|---|---|---|
| 1 | 18 (commandExecution=15, fileChange=3) | 25 (commandExecution=22, fileChange=2, webSearch=1) | 25 (commandExecution=22, fileChange=3) |
| 2 | 27 (commandExecution=24, fileChange=3) | 27 (commandExecution=25, fileChange=2) | 22 (commandExecution=20, fileChange=2) |
| 3 | 53 (commandExecution=49, fileChange=3, webSearch=1) | 99 (commandExecution=94, fileChange=5) | 44 (commandExecution=41, fileChange=3) |
| 4 | 47 (commandExecution=43, fileChange=4) | 100 (commandExecution=90, fileChange=10) | 30 (commandExecution=23, fileChange=7) |
| 5 | 35 (commandExecution=31, fileChange=4) | 39 (commandExecution=35, fileChange=3, webSearch=1) | 43 (commandExecution=39, fileChange=3, webSearch=1) |
| 6 | 38 (commandExecution=35, fileChange=3) | 46 (commandExecution=40, fileChange=5, webSearch=1) | 25 (commandExecution=23, fileChange=2) |
| 7 | 66 (commandExecution=60, fileChange=5, webSearch=1) | 81 (commandExecution=73, fileChange=8) | 60 (commandExecution=55, fileChange=5) |
| 8 | 14 (commandExecution=12, fileChange=2) | 17 (commandExecution=15, fileChange=2) | 16 (commandExecution=14, fileChange=2) |
| 9 | 26 (commandExecution=24, fileChange=2) | 40 (commandExecution=38, fileChange=2) | 25 (commandExecution=23, fileChange=2) |
| 10 | 35 (commandExecution=32, fileChange=3) | 41 (commandExecution=36, fileChange=3, webSearch=2) | 46 (commandExecution=40, fileChange=6) |
| 11 | 40 (commandExecution=38, fileChange=2) | 29 (commandExecution=27, fileChange=2) | 41 (commandExecution=35, fileChange=6) |
| 12 | 43 (commandExecution=38, fileChange=5) | 42 (commandExecution=39, fileChange=3) | 38 (commandExecution=33, fileChange=5) |
| 13 | 34 (commandExecution=32, fileChange=2) | 30 (commandExecution=28, fileChange=2) | 28 (commandExecution=26, fileChange=2) |
| 14 | 32 (commandExecution=28, fileChange=4) | 30 (commandExecution=26, fileChange=4) | 22 (commandExecution=20, fileChange=2) |
| 15 | 24 (commandExecution=21, fileChange=3) | 57 (commandExecution=49, fileChange=8) | 24 (commandExecution=22, fileChange=2) |
| 16 | 49 (commandExecution=45, fileChange=4) | 33 (commandExecution=30, fileChange=3) | 41 (commandExecution=37, fileChange=3, webSearch=1) |
| 17 | 22 (commandExecution=20, fileChange=2) | 13 (commandExecution=11, fileChange=2) | 13 (commandExecution=11, fileChange=2) |
| 18 | 63 (commandExecution=54, fileChange=9) | 35 (commandExecution=28, fileChange=7) | 40 (commandExecution=36, fileChange=4) |
| 19 | 27 (commandExecution=25, fileChange=2) | 29 (commandExecution=27, fileChange=2) | 26 (commandExecution=22, fileChange=4) |
| 20 | 56 (commandExecution=46, fileChange=10) | 39 (commandExecution=35, fileChange=4) | 44 (commandExecution=38, fileChange=6) |

### Weaker-model transcript and accounting disclosures

- All 120 weaker-model cells passed exact prompt hashes, execution completion, terminal-patch binding, independent transcript parsing and tool recount, and foreign-rank contamination checks.
- All 120 terminal patches received stock SWE-bench 4.1.0 exact-patch verdicts. Empty patches remain valid unresolved observations; they are not filtered or retried.
- The inherited Claude runner initially extracted `git diff HEAD`, which hid committed Haiku fixes. The canonical Haiku patches are deterministic frozen-base-to-final-worktree diffs from the original checkouts; original head-relative receipts and patches are retained as harness-bug provenance, and no provider episode was rerun.
- One rank-15 A-Spark evaluator attempt completed its tests successfully but the stock evaluator exited during a parallel Docker image-cleanup race. The exact patch was evaluated serially; the failed derived evaluator attempt is retained as provenance and no model episode was rerun.
- A/T/T2 user-prompt bytes are identical across Haiku and Spark for every case and match the corresponding frozen Sonnet/Luna condition. Provider-native runtime scaffolding necessarily differs by surface.
- The injected RNA exposure in T and T2 is treatment context, not a model-initiated tool call. Follow-up RNA calls are reported separately and are not required by the pre-injected-context hypothesis.
- The 80 treatment cells contain 80 deterministic injected RNA exposures. Models made 0 follow-up RNA calls; those calls remain separate from the ordinary tool totals above.

## HumanLayer/SlopCodeBench prompt-strategy factorial

This additive post-hoc execution crosses the frozen A/T/T2 context arms with semantic ports of SlopCodeBench's published `anti_slop` (AS) and `plan_first` (PF) strategies on all four models. The original 240 cells remain the strategy-free baseline; no cell is relabeled or replaced.

Scope boundary: HumanLayer's reported run used SlopCodeBench's `just-solve`
condition. `anti_slop` and `plan_first` are the two executed prompt variants in
the linked SlopCodeBench study and are the transferable additions evaluated
here. The article's adversarial-review, deterministic-backpressure, and
frontier-to-weaker-handoff ideas are future proposals, not reported treatment
arms; they are not silently counted among these 720 cells. METHOD.md contains
the complete executed-versus-proposed transfer audit.

All 720 final transcripts pass their applicable independent audit. T/T2 contribute 480 deterministic pre-injected RNA exposures. The 480-cell strategy extension records 25 ordinary follow-up RNA calls; the older ledgers did not expose one uniform follow-up-count field. The injected exposure is context, not a model tool call, and is excluded from tool counts. Preconditioning was necessary because earlier harness/model behavior did not reliably act on RNA system guidance alone; follow-up traversal is not required by the context-efficacy hypothesis. Four superseded strategy attempts that ingested their own live harness transcript were replaced before analysis, as documented in METHOD.md.

### Complete 36-condition summary

Input columns are mutually explicit: ordinary input excludes cache writes and reads; uncached total is ordinary plus cache-write input; cache-read input is billed from cache.

| Condition | Success | Time | Ordinary in | Uncached total | Cache read | Cache write | Output | Cost | Tools by type |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A_AS_haiku | 15/20 | 4253.7s | 29,019 | 998,561 | 37,955,214 | 969,542 | 335,544 | $7.441344 | Bash=764, Edit=84, Glob=1, Grep=8, Read=188, Write=34 |
| A_AS_luna | 19/20 | 2541.1s | 1,063,623 | 1,063,623 | 8,952,064 | 0 | 88,558 | $2.830177 | commandExecution=268, fileChange=34, webSearch=34 |
| A_AS_sonnet | 15/20 | 4076.3s | 21,257 | 436,120 | 10,114,040 | 414,863 | 145,026 | $7.718577 | Bash=321, Edit=80, Glob=3, Grep=25, Read=73, Write=8 |
| A_AS_spark | 14/20 | 2042.8s | 987,868 | 987,868 | 21,488,000 | 0 | 235,299 | n/a | commandExecution=752, fileChange=69, webSearch=15 |
| A_PF_haiku | 16/20 | 4292.0s | 28,803 | 970,673 | 35,675,105 | 941,870 | 319,522 | $7.077663 | Bash=749, Edit=74, Glob=1, Grep=7, Read=196, Write=24, bash=1 |
| A_PF_luna | 18/20 | 3204.7s | 1,240,331 | 1,240,331 | 11,874,048 | 0 | 118,766 | $3.430332 | commandExecution=301, fileChange=39, webSearch=29 |
| A_PF_sonnet | 15/20 | 5150.0s | 21,511 | 612,033 | 16,622,580 | 590,522 | 202,620 | $11.589695 | Bash=403, Edit=85, Glob=1, Grag=1, Grep=50, Read=87, Write=5 |
| A_PF_spark | 15/20 | 1816.4s | 916,618 | 916,618 | 19,041,536 | 0 | 234,442 | n/a | commandExecution=701, fileChange=67, mcpToolCall=4, webSearch=15 |
| A_haiku | 15/20 | 3962.5s | 28,307 | 1,010,471 | 33,714,747 | 982,164 | 315,924 | $6.943730 | Bash=710, Edit=73, Glob=1, Grep=4, Read=185, Write=16, bash=1 |
| A_luna | 18/20 | 2463.7s | 1,036,130 | 1,036,130 | 11,890,176 | 0 | 91,065 | $3.151538 | commandExecution=265, fileChange=34, webSearch=38 |
| A_sonnet | 17/20 | 4025.2s | 21,325 | 463,028 | 12,273,988 | 441,703 | 146,071 | $8.543340 | Bash=333, Edit=81, Grep=47, Read=71, Write=7 |
| A_spark | 15/20 | 1580.5s | 819,065 | 819,065 | 17,386,240 | 0 | 222,867 | n/a | commandExecution=672, fileChange=75, webSearch=2 |
| T2_AS_haiku | 15/20 | 4204.0s | 146,219 | 1,214,142 | 39,842,256 | 1,067,923 | 339,044 | $7.961511 | Bash=678, Edit=85, Glob=1, Grep=3, Read=224, Write=38, bash=1 |
| T2_AS_luna | 18/20 | 2428.4s | 1,076,351 | 1,076,351 | 10,561,792 | 0 | 82,916 | $2.990026 | commandExecution=218, fileChange=31, webSearch=36 |
| T2_AS_sonnet | 16/20 | 4075.4s | 138,955 | 750,650 | 16,359,214 | 611,695 | 147,025 | $10.920786 | Bash=368, Edit=66, Glob=1, Grep=23, Read=74, Write=6 |
| T2_AS_spark | 15/20 | 1874.4s | 946,825 | 946,825 | 21,444,608 | 0 | 234,175 | n/a | commandExecution=669, fileChange=61, mcpToolCall=2, webSearch=6 |
| T2_PF_haiku | 14/20 | 4174.1s | 146,163 | 1,241,280 | 41,254,513 | 1,095,117 | 343,680 | $8.180248 | Bash=681, Edit=99, Glob=2, Grep=9, Read=215, Write=13, bash=1 |
| T2_PF_luna | 17/20 | 3305.4s | 1,277,827 | 1,277,827 | 15,478,528 | 0 | 116,443 | $3.984338 | commandExecution=279, fileChange=39, webSearch=46 |
| T2_PF_sonnet | 16/20 | 4857.2s | 139,013 | 786,884 | 17,043,061 | 647,871 | 169,272 | $11.677005 | Bash=385, Edit=79, Grep=8, Read=91, Write=5 |
| T2_PF_spark | 15/20 | 1704.7s | 969,703 | 969,703 | 21,525,760 | 0 | 235,396 | n/a | commandExecution=618, fileChange=65, mcpToolCall=1, webSearch=1 |
| T2_haiku | 16/20 | 4100.2s | 146,563 | 1,275,674 | 45,318,250 | 1,129,111 | 320,063 | $8.536925 | Bash=741, Edit=93, Glob=4, Grep=11, Read=218, Write=5 |
| T2_luna | 17/20 | 2388.9s | 1,146,736 | 1,146,736 | 11,731,712 | 0 | 90,955 | $3.205637 | commandExecution=231, fileChange=39, webSearch=34 |
| T2_sonnet | 17/20 | 3713.0s | 138,893 | 722,549 | 14,577,901 | 583,656 | 137,374 | $10.073257 | Bash=337, Edit=68, Glob=1, Grep=21, Read=78, Write=5 |
| T2_spark | 16/20 | 1316.7s | 819,387 | 819,387 | 17,793,664 | 0 | 210,738 | n/a | commandExecution=580, fileChange=71, webSearch=2 |
| T_AS_haiku | 16/20 | 4279.9s | 177,517 | 1,273,786 | 41,067,238 | 1,096,269 | 341,102 | $8.182289 | Bash=706, Edit=73, Glob=3, Grep=3, Read=205, Write=19 |
| T_AS_luna | 18/20 | 2659.2s | 1,170,725 | 1,170,725 | 11,825,920 | 0 | 92,953 | $3.211035 | commandExecution=250, fileChange=39, webSearch=30 |
| T_AS_sonnet | 15/20 | 4160.4s | 170,345 | 827,480 | 15,983,355 | 657,135 | 149,335 | $11.146686 | Bash=327, Edit=73, Grep=28, Read=87, Write=5 |
| T_AS_spark | 14/20 | 1939.5s | 988,897 | 988,897 | 26,044,160 | 0 | 264,933 | n/a | commandExecution=731, fileChange=68, mcpToolCall=2, webSearch=2 |
| T_PF_haiku | 15/20 | 4692.8s | 177,991 | 1,290,884 | 44,952,621 | 1,112,893 | 394,806 | $8.873069 | Bash=725, Edit=91, Glob=2, Grep=5, Read=234, Write=10, bash=1 |
| T_PF_luna | 17/20 | 2941.9s | 1,227,747 | 1,227,747 | 13,277,696 | 0 | 108,131 | $3.424303 | commandExecution=282, fileChange=30, webSearch=22 |
| T_PF_sonnet | 16/20 | 4421.3s | 170,389 | 877,723 | 18,751,683 | 707,334 | 176,481 | $12.685611 | Bash=349, Edit=77, Grep=22, Read=88, Write=6 |
| T_PF_spark | 14/20 | 1803.5s | 986,292 | 986,292 | 23,934,080 | 0 | 244,528 | n/a | commandExecution=675, fileChange=60, webSearch=4 |
| T_haiku | 15/20 | 3870.3s | 177,597 | 1,253,421 | 41,499,823 | 1,075,824 | 324,795 | $8.103202 | Bash=721, Edit=90, Glob=1, Grep=1, Read=204, Write=6, bash=1 |
| T_luna | 19/20 | 2571.5s | 1,037,119 | 1,037,119 | 12,202,496 | 0 | 97,953 | $3.105087 | commandExecution=238, fileChange=37, imageView=2, webSearch=26 |
| T_sonnet | 18/20 | 3374.4s | 170,189 | 727,235 | 12,391,901 | 557,046 | 131,649 | $9.202908 | Bash=281, Edit=74, Glob=1, Grep=14, Read=71, Write=4 |
| T_spark | 17/20 | 1872.1s | 1,137,982 | 1,137,982 | 30,802,816 | 0 | 279,222 | n/a | commandExecution=768, fileChange=79, webSearch=5 |

### Ceiling sensitivity across stronger and weaker models

`Headroom` is the number of the 20 frozen cases not solved by the matching A condition. Rescues/regressions are same-case flips, so this table distinguishes a ceiling effect from offsetting treatment wins and losses.

| Model | Strategy | A solved | Headroom | T solved | T rescues/regressions | T2 solved | T2 rescues/regressions |
|---|---|---:|---:|---:|---:|---:|---:|
| sonnet | base | 17/20 | 3 | 18/20 | 2/1 | 17/20 | 2/2 |
| sonnet | AS | 15/20 | 5 | 15/20 | 0/0 | 16/20 | 1/0 |
| sonnet | PF | 15/20 | 5 | 16/20 | 1/0 | 16/20 | 1/0 |
| luna | base | 18/20 | 2 | 19/20 | 2/1 | 17/20 | 0/1 |
| luna | AS | 19/20 | 1 | 18/20 | 0/1 | 18/20 | 0/1 |
| luna | PF | 18/20 | 2 | 17/20 | 1/2 | 17/20 | 1/2 |
| haiku | base | 15/20 | 5 | 15/20 | 1/1 | 16/20 | 2/1 |
| haiku | AS | 15/20 | 5 | 16/20 | 1/0 | 15/20 | 1/1 |
| haiku | PF | 16/20 | 4 | 15/20 | 0/1 | 14/20 | 0/2 |
| spark | base | 15/20 | 5 | 17/20 | 2/0 | 16/20 | 1/0 |
| spark | AS | 14/20 | 6 | 14/20 | 0/0 | 15/20 | 1/0 |
| spark | PF | 15/20 | 5 | 14/20 | 0/1 | 15/20 | 0/0 |

### Strategy effects versus the matching no-strategy baseline

Each row is paired on the same 20 frozen cases. Positive efficacy is favorable; negative time/token/tool percentages are favorable. Exact McNemar p-values are descriptive because these arms were added post hoc.

| Model | Context | Strategy | Success before→after | Wins/losses | p | Time | Uncached in | Cache read | Cache write | Total tokens | Output | Tools | Tool mix before→after | Cost | Production verbosity ΔΔ | Production erosion ΔΔ |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|
| sonnet | A | AS | 17→15 (-10.0pp) | 0/2 | 0.500 | +1.3% | -5.8% | -17.6% | -6.1% | -17.0% | -0.7% | -5.4% | Bash=333→321, Edit=81→80, Glob=0→3, Grep=47→25, Read=71→73, Write=7→8 | -9.7% | +0.0007 | -0.0003 |
| sonnet | A | PF | 17→15 (-10.0pp) | 0/2 | 0.500 | +27.9% | +32.2% | +35.4% | +33.7% | +35.3% | +38.7% | +17.3% | Bash=333→403, Edit=81→85, Glob=0→1, Grag=0→1, Grep=47→50, Read=71→87, Write=7→5 | +35.7% | +0.0009 | +0.0025 |
| sonnet | T | AS | 18→15 (-15.0pp) | 0/3 | 0.250 | +23.3% | +13.8% | +29.0% | +18.0% | +28.0% | +13.4% | +16.9% | Bash=281→327, Edit=74→73, Glob=1→0, Grep=14→28, Read=71→87, Write=4→5 | +21.1% | +0.0001 | -0.0002 |
| sonnet | T | PF | 18→16 (-10.0pp) | 1/3 | 0.625 | +31.0% | +20.7% | +51.3% | +27.0% | +49.5% | +34.1% | +21.8% | Bash=281→349, Edit=74→77, Glob=1→0, Grep=14→22, Read=71→88, Write=4→6 | +37.8% | -0.0000 | +0.0001 |
| sonnet | T2 | AS | 17→16 (-5.0pp) | 1/2 | 1.000 | +9.8% | +3.9% | +12.2% | +4.8% | +11.8% | +7.0% | +5.5% | Bash=337→368, Edit=68→66, Glob=1→1, Grep=21→23, Read=78→74, Write=5→6 | +8.4% | -0.0002 | +0.0003 |
| sonnet | T2 | PF | 17→16 (-5.0pp) | 1/2 | 1.000 | +30.8% | +8.9% | +16.9% | +11.0% | +16.6% | +23.2% | +11.4% | Bash=337→385, Edit=68→79, Glob=1→0, Grep=21→8, Read=78→91, Write=5→5 | +15.9% | +0.0004 | +0.0006 |
| luna | A | AS | 18→19 (+5.0pp) | 1/0 | 1.000 | +3.1% | +2.7% | -24.7% | n/a | -22.4% | -2.8% | -0.3% | commandExecution=265→268, fileChange=34→34, webSearch=38→34 | -10.2% | -0.0006 | +0.0006 |
| luna | A | PF | 18→18 (+0.0pp) | 1/1 | 1.000 | +30.1% | +19.7% | -0.1% | n/a | +1.7% | +30.4% | +9.5% | commandExecution=265→301, fileChange=34→39, webSearch=38→29 | +8.8% | -0.0004 | -0.0003 |
| luna | T | AS | 19→18 (-5.0pp) | 1/2 | 1.000 | +3.4% | +12.9% | -3.1% | n/a | -1.9% | -5.1% | +5.3% | commandExecution=238→250, fileChange=37→39, imageView=2→0, webSearch=26→30 | +3.4% | -0.0002 | +0.0010 |
| luna | T | PF | 19→17 (-10.0pp) | 1/3 | 0.625 | +14.4% | +18.4% | +8.8% | n/a | +9.6% | +10.4% | +10.2% | commandExecution=238→282, fileChange=37→30, imageView=2→0, webSearch=26→22 | +10.3% | -0.0006 | +0.0002 |
| luna | T2 | AS | 17→18 (+5.0pp) | 1/0 | 1.000 | +1.7% | -6.1% | -10.0% | n/a | -9.6% | -8.8% | -6.2% | commandExecution=231→218, fileChange=39→31, webSearch=34→36 | -6.7% | +0.0001 | +0.0000 |
| luna | T2 | PF | 17→17 (+0.0pp) | 1/1 | 1.000 | +38.4% | +11.4% | +31.9% | n/a | +30.1% | +28.0% | +19.7% | commandExecution=231→279, fileChange=39→39, webSearch=34→46 | +24.3% | +0.0003 | -0.0009 |
| haiku | A | AS | 15→15 (+0.0pp) | 1/1 | 1.000 | +7.3% | -1.2% | +12.6% | -1.3% | +12.1% | +6.2% | +9.0% | Bash=710→764, Edit=73→84, Glob=1→1, Grep=4→8, Read=185→188, Write=16→34, bash=1→0 | +7.2% | +0.0007 | -0.0004 |
| haiku | A | PF | 15→16 (+5.0pp) | 2/1 | 1.000 | +8.3% | -3.9% | +5.8% | -4.1% | +5.5% | +1.1% | +6.3% | Bash=710→749, Edit=73→74, Glob=1→1, Grep=4→7, Read=185→196, Write=16→24, bash=1→1 | +1.9% | +0.0007 | -0.0003 |
| haiku | T | AS | 15→16 (+5.0pp) | 2/1 | 1.000 | +10.6% | +1.6% | -1.0% | +1.9% | -0.9% | +5.0% | -1.5% | Bash=721→706, Edit=90→73, Glob=1→3, Grep=1→3, Read=204→205, Write=6→19, bash=1→0 | +1.0% | -0.0002 | +0.0005 |
| haiku | T | PF | 15→15 (+0.0pp) | 1/1 | 1.000 | +21.3% | +3.0% | +8.3% | +3.4% | +8.3% | +21.6% | +4.3% | Bash=721→725, Edit=90→91, Glob=1→2, Grep=1→5, Read=204→234, Write=6→10, bash=1→1 | +9.5% | -0.0009 | +0.0000 |
| haiku | T2 | AS | 16→15 (-5.0pp) | 1/2 | 1.000 | +2.5% | -4.8% | -12.1% | -5.4% | -11.8% | +5.9% | -3.9% | Bash=741→678, Edit=93→85, Glob=4→1, Grep=11→3, Read=218→224, Write=5→38, bash=0→1 | -6.7% | -0.0191 | +0.0021 |
| haiku | T2 | PF | 16→14 (-10.0pp) | 1/3 | 0.625 | +1.8% | -2.7% | -9.0% | -3.0% | -8.7% | +7.4% | -4.9% | Bash=741→681, Edit=93→99, Glob=4→2, Grep=11→9, Read=218→215, Write=5→13, bash=0→1 | -4.2% | -0.0002 | -0.0001 |
| spark | A | AS | 15→14 (-5.0pp) | 0/1 | 1.000 | +29.3% | +20.6% | +23.6% | n/a | +23.2% | +5.6% | +11.6% | commandExecution=672→752, fileChange=75→69, webSearch=2→15 | n/a | -0.0005 | +0.0001 |
| spark | A | PF | 15→15 (+0.0pp) | 1/1 | 1.000 | +14.9% | +11.9% | +9.5% | n/a | +9.6% | +5.2% | +5.1% | commandExecution=672→701, fileChange=75→67, mcpToolCall=0→4, webSearch=2→15 | n/a | -0.0004 | +0.0000 |
| spark | T | AS | 17→14 (-15.0pp) | 0/3 | 0.250 | +3.6% | -13.1% | -15.4% | n/a | -15.3% | -5.1% | -5.8% | commandExecution=768→731, fileChange=79→68, mcpToolCall=0→2, webSearch=5→2 | n/a | -0.0010 | -0.0005 |
| spark | T | PF | 17→14 (-15.0pp) | 0/3 | 0.250 | -3.7% | -13.3% | -22.3% | n/a | -21.9% | -12.4% | -13.3% | commandExecution=768→675, fileChange=79→60, webSearch=5→4 | n/a | -0.0003 | +0.0008 |
| spark | T2 | AS | 16→15 (-5.0pp) | 1/2 | 1.000 | +42.4% | +15.6% | +20.5% | n/a | +20.2% | +11.1% | +13.0% | commandExecution=580→669, fileChange=71→61, mcpToolCall=0→2, webSearch=2→6 | n/a | -0.0001 | -0.0027 |
| spark | T2 | PF | 16→15 (-5.0pp) | 1/2 | 1.000 | +29.5% | +18.3% | +21.0% | n/a | +20.8% | +11.7% | +4.9% | commandExecution=580→618, fileChange=71→65, mcpToolCall=0→1, webSearch=2→1 | n/a | -0.0011 | -0.0024 |

### RNA context effects by strategy (including the strategy-free base)

| Model | Strategy | Context comparison | Success A→treatment | Wins/losses | Time | Uncached in | Cache read | Cache write | Total tokens | Output | Tools | Tool mix A→treatment | Cost |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| sonnet | base | A→T | 17→18 (+5.0pp) | 2/1 | -16.2% | +57.1% | +1.0% | +26.1% | +2.9% | -9.9% | -17.4% | Bash=333→281, Edit=81→74, Glob=0→1, Grep=47→14, Read=71→71, Write=7→4 | +7.7% |
| sonnet | base | A→T2 | 17→17 (+0.0pp) | 2/2 | -7.8% | +56.0% | +18.8% | +32.1% | +19.8% | -6.0% | -5.4% | Bash=333→337, Edit=81→68, Glob=0→1, Grep=47→21, Read=71→78, Write=7→5 | +17.9% |
| sonnet | AS | A→T | 15→15 (+0.0pp) | 0/0 | +2.1% | +89.7% | +58.0% | +58.4% | +58.6% | +3.0% | +2.0% | Bash=321→327, Edit=80→73, Glob=3→0, Grep=25→28, Read=73→87, Write=8→5 | +44.4% |
| sonnet | AS | A→T2 | 15→16 (+5.0pp) | 1/0 | -0.0% | +72.1% | +61.7% | +47.4% | +61.4% | +1.4% | +5.5% | Bash=321→368, Edit=80→66, Glob=3→1, Grep=25→23, Read=73→74, Write=8→6 | +41.5% |
| sonnet | PF | A→T | 15→16 (+5.0pp) | 1/0 | -14.2% | +43.4% | +12.8% | +19.8% | +13.6% | -12.9% | -14.2% | Bash=403→349, Edit=85→77, Glob=1→0, Grag=1→0, Grep=50→22, Read=87→88, Write=5→6 | +9.5% |
| sonnet | PF | A→T2 | 15→16 (+5.0pp) | 1/0 | -5.7% | +28.6% | +2.5% | +9.7% | +3.2% | -16.5% | -10.1% | Bash=403→385, Edit=85→79, Glob=1→0, Grag=1→0, Grep=50→8, Read=87→91, Write=5→5 | +0.8% |
| luna | base | A→T | 18→19 (+5.0pp) | 2/1 | +4.4% | +0.1% | +2.6% | n/a | +2.5% | +7.6% | -10.1% | commandExecution=265→238, fileChange=34→37, imageView=0→2, webSearch=38→26 | -1.5% |
| luna | base | A→T2 | 18→17 (-5.0pp) | 0/1 | -3.0% | +10.7% | -1.3% | n/a | -0.4% | -0.1% | -9.8% | commandExecution=265→231, fileChange=34→39, webSearch=38→34 | +1.7% |
| luna | AS | A→T | 19→18 (-5.0pp) | 0/1 | +4.6% | +10.1% | +32.1% | n/a | +29.5% | +5.0% | -5.1% | commandExecution=268→250, fileChange=34→39, webSearch=34→30 | +13.5% |
| luna | AS | A→T2 | 19→18 (-5.0pp) | 0/1 | -4.4% | +1.2% | +18.0% | n/a | +16.0% | -6.4% | -15.2% | commandExecution=268→218, fileChange=34→31, webSearch=34→36 | +5.6% |
| luna | PF | A→T | 18→17 (-5.0pp) | 1/2 | -8.2% | -1.0% | +11.8% | n/a | +10.4% | -9.0% | -9.5% | commandExecution=301→282, fileChange=39→30, webSearch=29→22 | -0.2% |
| luna | PF | A→T2 | 18→17 (-5.0pp) | 1/2 | +3.1% | +3.0% | +30.4% | n/a | +27.5% | -2.0% | -1.4% | commandExecution=301→279, fileChange=39→39, webSearch=29→46 | +16.2% |
| haiku | base | A→T | 15→15 (+0.0pp) | 1/1 | -2.3% | +24.0% | +23.1% | +9.5% | +22.9% | +2.8% | +3.4% | Bash=710→721, Edit=73→90, Glob=1→1, Grep=4→1, Read=185→204, Write=16→6, bash=1→1 | +16.7% |
| haiku | base | A→T2 | 15→16 (+5.0pp) | 2/1 | +3.5% | +26.2% | +34.4% | +15.0% | +33.9% | +1.3% | +8.3% | Bash=710→741, Edit=73→93, Glob=1→4, Grep=4→11, Read=185→218, Write=16→5, bash=1→0 | +22.9% |
| haiku | AS | A→T | 15→16 (+5.0pp) | 1/0 | +0.6% | +27.6% | +8.2% | +13.1% | +8.6% | +1.7% | -6.5% | Bash=764→706, Edit=84→73, Glob=1→3, Grep=8→3, Read=188→205, Write=34→19 | +10.0% |
| haiku | AS | A→T2 | 15→15 (+0.0pp) | 1/1 | -1.2% | +21.6% | +5.0% | +10.1% | +5.4% | +1.0% | -4.5% | Bash=764→678, Edit=84→85, Glob=1→1, Grep=8→3, Read=188→224, Write=34→38, bash=0→1 | +7.0% |
| haiku | PF | A→T | 16→15 (-5.0pp) | 0/1 | +9.3% | +33.0% | +26.0% | +18.2% | +26.2% | +23.6% | +1.5% | Bash=749→725, Edit=74→91, Glob=1→2, Grep=7→5, Read=196→234, Write=24→10, bash=1→1 | +25.4% |
| haiku | PF | A→T2 | 16→14 (-10.0pp) | 0/2 | -2.7% | +27.9% | +15.6% | +16.3% | +15.9% | +7.6% | -3.0% | Bash=749→681, Edit=74→99, Glob=1→2, Grep=7→9, Read=196→215, Write=24→13, bash=1→1 | +15.6% |
| spark | base | A→T | 15→17 (+10.0pp) | 2/0 | +18.5% | +38.9% | +77.2% | n/a | +74.8% | +25.3% | +13.8% | commandExecution=672→768, fileChange=75→79, webSearch=2→5 | n/a |
| spark | base | A→T2 | 15→16 (+5.0pp) | 1/0 | -16.7% | +0.0% | +2.3% | n/a | +2.1% | -5.4% | -12.8% | commandExecution=672→580, fileChange=75→71, webSearch=2→2 | n/a |
| spark | AS | A→T | 14→14 (+0.0pp) | 0/0 | -5.1% | +0.1% | +21.2% | n/a | +20.2% | +12.6% | -3.9% | commandExecution=752→731, fileChange=69→68, mcpToolCall=0→2, webSearch=15→2 | n/a |
| spark | AS | A→T2 | 14→15 (+5.0pp) | 1/0 | -8.2% | -4.2% | -0.2% | n/a | -0.4% | -0.5% | -11.7% | commandExecution=752→669, fileChange=69→61, mcpToolCall=0→2, webSearch=15→6 | n/a |
| spark | PF | A→T | 15→14 (-5.0pp) | 0/1 | -0.7% | +7.6% | +25.7% | n/a | +24.6% | +4.3% | -6.1% | commandExecution=701→675, fileChange=67→60, mcpToolCall=4→0, webSearch=15→4 | n/a |
| spark | PF | A→T2 | 15→15 (+0.0pp) | 0/0 | -6.1% | +5.8% | +13.0% | n/a | +12.6% | +0.4% | -13.0% | commandExecution=701→618, fileChange=67→65, mcpToolCall=4→1, webSearch=15→1 | n/a |

### All 720 cells: per-case efficacy, efficiency, quality, and tool type

`Prod verb Δ` and `Prod erosion Δ` are final-minus-base on production Python files changed by that cell. `n/a` means no comparable production-Python before/after scope.

| Rank | Case | Model | Context | Strategy | Success | Time | Ordinary in | Uncached total | Cache read | Cache write | Output | Cost | Tools by type | Prod verb Δ | Prod erosion Δ |
|---:|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|
| 1 | sympy__sympy-23534 | haiku | A | AS | yes | 128.1s | 993 | 39,231 | 810,849 | 38,238 | 9,484 | $0.205974 | Bash=20, Edit=2, Read=7 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | A | PF | yes | 146.1s | 1,033 | 43,750 | 1,019,202 | 42,717 | 11,712 | $0.246947 | Bash=22, Edit=3, Read=9 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | A | base | yes | 181.4s | 1,065 | 39,633 | 1,024,477 | 38,568 | 12,877 | $0.245034 | Bash=28, Edit=1, Read=5, Write=4 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | T | AS | yes | 154.5s | 8,049 | 55,901 | 1,081,701 | 47,852 | 11,515 | $0.269498 | Bash=22, Edit=2, Read=5 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | T | PF | yes | 218.3s | 8,121 | 69,654 | 1,674,932 | 61,533 | 18,918 | $0.393270 | Bash=25, Edit=4, Read=9 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | T | base | yes | 185.0s | 8,081 | 68,400 | 1,448,187 | 60,319 | 11,513 | $0.331103 | Bash=27, Edit=2, Read=4 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | T2 | AS | yes | 127.3s | 6,437 | 58,584 | 850,650 | 52,147 | 9,579 | $0.243691 | Bash=19, Edit=1, Read=5 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | T2 | PF | yes | 126.0s | 6,469 | 42,429 | 810,821 | 35,960 | 10,077 | $0.209856 | Bash=20, Edit=3, Grep=1, Read=5 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | haiku | T2 | base | yes | 184.8s | 6,549 | 64,004 | 1,684,517 | 57,455 | 12,776 | $0.353791 | Bash=32, Edit=1, Read=6 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | A | AS | yes | 63.8s | 41,426 | 41,426 | 139,520 | 0 | 2,055 | $0.067708 | commandExecution=7, fileChange=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | A | PF | yes | 126.8s | 63,646 | 63,646 | 347,392 | 0 | 4,483 | $0.125283 | commandExecution=12, fileChange=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | A | base | yes | 74.5s | 51,291 | 51,291 | 150,016 | 0 | 2,769 | $0.082907 | commandExecution=9 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | T | AS | yes | 51.0s | 27,264 | 27,264 | 195,072 | 0 | 1,874 | $0.058015 | commandExecution=6, fileChange=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | T | PF | yes | 72.7s | 39,504 | 39,504 | 334,336 | 0 | 2,492 | $0.087890 | commandExecution=7, fileChange=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | T | base | yes | 53.5s | 32,256 | 32,256 | 173,056 | 0 | 2,087 | $0.062084 | commandExecution=6, fileChange=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | T2 | AS | yes | 47.0s | 30,912 | 30,912 | 201,472 | 0 | 1,511 | $0.060125 | commandExecution=6, fileChange=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | T2 | PF | yes | 94.6s | 36,010 | 36,010 | 298,496 | 0 | 2,591 | $0.081406 | commandExecution=9, fileChange=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | luna | T2 | base | yes | 89.8s | 46,070 | 46,070 | 395,008 | 0 | 2,761 | $0.102137 | commandExecution=6, fileChange=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | A | AS | yes | 95.8s | 790 | 14,869 | 276,640 | 14,079 | 3,735 | $0.224177 | Bash=11, Edit=3, Grep=2, Read=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | A | PF | yes | 240.5s | 800 | 21,671 | 423,813 | 20,871 | 7,014 | $0.358286 | Bash=11, Edit=4, Grep=1, Read=7 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | A | base | yes | 199.6s | 780 | 18,383 | 161,128 | 17,603 | 2,705 | $0.195187 | Bash=8, Edit=1, Grep=2, Read=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | T | AS | yes | 162.7s | 7,846 | 32,052 | 456,064 | 24,206 | 4,287 | $0.354112 | Bash=12, Edit=3, Read=3 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | T | PF | yes | 120.9s | 7,834 | 30,661 | 296,395 | 22,827 | 3,126 | $0.280477 | Bash=9, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | T | base | yes | 61.4s | 7,828 | 35,040 | 199,721 | 27,212 | 1,938 | $0.259946 | Bash=6, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | T2 | AS | yes | 101.1s | 6,254 | 22,819 | 250,570 | 16,565 | 2,449 | $0.217432 | Bash=9, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | T2 | PF | yes | 163.7s | 6,272 | 33,735 | 564,738 | 27,463 | 6,378 | $0.436049 | Bash=14, Edit=4, Read=3 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | sonnet | T2 | base | yes | 105.2s | 6,256 | 31,743 | 269,782 | 25,487 | 2,895 | $0.283434 | Bash=10, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | A | AS | yes | 79.6s | 26,147 | 26,147 | 551,680 | 0 | 7,631 | n/a | commandExecution=29, fileChange=3 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | A | PF | yes | 33.8s | 34,439 | 34,439 | 250,112 | 0 | 4,168 | n/a | commandExecution=13, fileChange=3, mcpToolCall=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | A | base | yes | 37.3s | 17,356 | 17,356 | 234,752 | 0 | 4,616 | n/a | commandExecution=15, fileChange=3 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | T | AS | yes | 63.6s | 27,734 | 27,734 | 536,960 | 0 | 7,808 | n/a | commandExecution=23, fileChange=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | T | PF | yes | 69.6s | 27,109 | 27,109 | 550,016 | 0 | 5,462 | n/a | commandExecution=20, fileChange=4 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | T | base | yes | 67.1s | 27,809 | 27,809 | 548,224 | 0 | 6,814 | n/a | commandExecution=22, fileChange=2, webSearch=1 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | T2 | AS | yes | 66.9s | 45,259 | 45,259 | 582,528 | 0 | 6,460 | n/a | commandExecution=21, fileChange=2 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | T2 | PF | yes | 65.0s | 30,950 | 30,950 | 630,016 | 0 | 6,093 | n/a | commandExecution=22, fileChange=4 | +0.0000 | +0.0000 |
| 1 | sympy__sympy-23534 | spark | T2 | base | yes | 53.9s | 23,203 | 23,203 | 489,472 | 0 | 5,371 | n/a | commandExecution=22, fileChange=3 | +0.0000 | +0.0000 |
| 2 | django__django-11179 | haiku | A | AS | yes | 177.5s | 997 | 41,284 | 1,456,002 | 40,287 | 12,949 | $0.291916 | Bash=35, Edit=4, Read=8 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | A | PF | yes | 200.7s | 1,053 | 42,688 | 1,595,151 | 41,635 | 13,510 | $0.311388 | Bash=33, Edit=4, Read=17 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | A | base | yes | 128.5s | 909 | 44,053 | 1,046,909 | 43,144 | 9,262 | $0.238198 | Bash=28, Edit=1, Read=7 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | T | AS | yes | 191.1s | 9,416 | 60,937 | 2,404,233 | 51,521 | 14,434 | $0.425051 | Bash=46, Edit=3, Read=12 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | T | PF | yes | 254.2s | 9,424 | 88,231 | 3,603,085 | 78,807 | 19,638 | $0.625536 | Bash=41, Edit=9, Read=12 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | T | base | yes | 141.1s | 9,192 | 61,528 | 1,305,956 | 52,336 | 11,539 | $0.302155 | Bash=24, Edit=2, Read=7 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | T2 | AS | yes | 145.4s | 9,378 | 58,750 | 1,157,039 | 49,372 | 13,933 | $0.293491 | Bash=19, Edit=1, Read=7, Write=5 | -0.0360 | +0.0420 |
| 2 | django__django-11179 | haiku | T2 | PF | yes | 113.9s | 9,370 | 54,949 | 1,136,228 | 45,579 | 8,159 | $0.254946 | Bash=20, Edit=3, Read=8 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | haiku | T2 | base | yes | 151.7s | 9,474 | 56,586 | 1,704,932 | 47,112 | 10,749 | $0.327936 | Bash=31, Edit=4, Read=9 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | A | AS | yes | 67.7s | 49,185 | 49,185 | 114,688 | 0 | 2,651 | $0.076560 | commandExecution=8, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | A | PF | yes | 59.9s | 31,563 | 31,563 | 172,544 | 0 | 1,893 | $0.060175 | commandExecution=7, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | A | base | yes | 66.0s | 23,884 | 23,884 | 179,200 | 0 | 2,250 | $0.055304 | commandExecution=10, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | T | AS | yes | 84.7s | 24,891 | 24,891 | 193,024 | 0 | 2,012 | $0.056265 | commandExecution=7, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | T | PF | yes | 81.6s | 27,842 | 27,842 | 217,344 | 0 | 3,068 | $0.067984 | commandExecution=6, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | T | base | yes | 70.5s | 32,970 | 32,970 | 259,072 | 0 | 2,221 | $0.072203 | commandExecution=10, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | T2 | AS | yes | 51.7s | 40,681 | 40,681 | 124,416 | 0 | 1,659 | $0.063077 | commandExecution=6, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | T2 | PF | yes | 74.1s | 27,286 | 27,286 | 214,272 | 0 | 2,744 | $0.065177 | commandExecution=6, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | luna | T2 | base | yes | 50.6s | 53,062 | 53,062 | 158,720 | 0 | 1,845 | $0.080004 | commandExecution=5, fileChange=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | A | AS | yes | 74.4s | 640 | 10,376 | 168,214 | 9,736 | 3,134 | $0.156406 | Bash=7, Edit=2, Read=4 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | A | PF | yes | 207.6s | 652 | 14,677 | 282,104 | 14,025 | 5,019 | $0.244638 | Bash=10, Edit=3, Read=6 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | A | base | yes | 233.9s | 672 | 20,338 | 507,449 | 19,666 | 7,214 | $0.379073 | Bash=17, Edit=6, Read=6 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | T | AS | yes | 74.5s | 8,949 | 29,156 | 331,688 | 20,207 | 3,284 | $0.278837 | Bash=8, Edit=2, Read=4 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | T | PF | yes | 222.1s | 8,955 | 31,368 | 422,834 | 22,413 | 3,684 | $0.325435 | Bash=12, Edit=2, Read=3 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | T | base | yes | 215.4s | 8,969 | 34,425 | 628,122 | 25,456 | 4,879 | $0.423247 | Bash=16, Edit=3, Grep=1, Read=4 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | T2 | AS | yes | 92.9s | 9,155 | 39,393 | 570,583 | 30,238 | 5,645 | $0.446337 | Bash=14, Edit=2, Read=4 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | T2 | PF | yes | 79.6s | 9,141 | 31,624 | 313,556 | 22,483 | 3,862 | $0.295902 | Bash=7, Edit=2, Read=4 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | sonnet | T2 | base | yes | 119.3s | 9,137 | 34,021 | 271,776 | 24,884 | 2,564 | $0.278292 | Bash=8, Edit=1, Read=2 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | A | AS | yes | 56.3s | 40,838 | 40,838 | 523,008 | 0 | 5,734 | n/a | commandExecution=27, fileChange=3 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | A | PF | yes | 45.0s | 38,472 | 38,472 | 343,040 | 0 | 5,892 | n/a | commandExecution=18, fileChange=3 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | A | base | yes | 61.7s | 20,636 | 20,636 | 376,576 | 0 | 5,048 | n/a | commandExecution=24, fileChange=3 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | T | AS | yes | 52.3s | 27,398 | 27,398 | 559,872 | 0 | 6,213 | n/a | commandExecution=23, fileChange=2 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | T | PF | yes | 88.5s | 49,439 | 49,439 | 712,960 | 0 | 7,563 | n/a | commandExecution=23, fileChange=3 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | T | base | yes | 74.3s | 32,142 | 32,142 | 659,072 | 0 | 5,786 | n/a | commandExecution=25, fileChange=2 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | T2 | AS | yes | 66.2s | 48,574 | 48,574 | 706,560 | 0 | 8,392 | n/a | commandExecution=27, fileChange=2, mcpToolCall=1 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | T2 | PF | yes | 66.7s | 26,129 | 26,129 | 507,904 | 0 | 6,236 | n/a | commandExecution=21, fileChange=2 | +0.0003 | +0.0009 |
| 2 | django__django-11179 | spark | T2 | base | yes | 41.3s | 23,498 | 23,498 | 435,968 | 0 | 5,960 | n/a | commandExecution=20, fileChange=2 | +0.0003 | +0.0009 |
| 3 | sympy__sympy-13757 | haiku | A | AS | yes | 257.3s | 1,280 | 52,306 | 2,111,227 | 51,026 | 19,234 | $0.410625 | Bash=44, Edit=7, Read=17 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | A | PF | yes | 307.2s | 1,416 | 53,172 | 2,881,955 | 51,756 | 22,498 | $0.505613 | Bash=60, Edit=9, Read=16 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | A | base | yes | 237.4s | 1,272 | 53,486 | 2,069,241 | 52,214 | 19,849 | $0.411869 | Bash=46, Edit=7, Read=14 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | T | AS | yes | 312.2s | 11,549 | 77,707 | 3,544,124 | 66,158 | 19,731 | $0.596932 | Bash=52, Edit=8, Read=16 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | T | PF | yes | 369.9s | 11,581 | 89,934 | 4,116,127 | 78,353 | 32,622 | $0.743010 | Bash=50, Edit=10, Glob=1, Read=20 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | T | base | yes | 395.1s | 11,781 | 91,341 | 5,484,411 | 79,560 | 29,756 | $0.868122 | Bash=74, Edit=10, Read=22 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | T2 | AS | yes | 416.2s | 9,275 | 90,689 | 5,443,642 | 81,414 | 38,183 | $0.907382 | Bash=57, Edit=18, Read=19, Write=12 | -0.0011 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | T2 | PF | yes | 471.9s | 9,195 | 95,459 | 5,151,518 | 86,264 | 40,482 | $0.899285 | Bash=71, Edit=11, Read=14 | -0.0024 | +0.0000 |
| 3 | sympy__sympy-13757 | haiku | T2 | base | yes | 279.5s | 9,027 | 74,193 | 2,972,796 | 65,166 | 22,463 | $0.548954 | Bash=51, Edit=7, Grep=1, Read=16 | -0.0026 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | A | AS | yes | 140.3s | 60,655 | 60,655 | 722,688 | 0 | 5,266 | $0.194520 | commandExecution=19, fileChange=1, webSearch=3 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | A | PF | yes | 137.6s | 61,803 | 61,803 | 572,416 | 0 | 4,853 | $0.168163 | commandExecution=15, fileChange=2, webSearch=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | A | base | yes | 205.7s | 74,028 | 74,028 | 1,285,632 | 0 | 7,706 | $0.248827 | commandExecution=28, fileChange=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | T | AS | yes | 158.1s | 72,803 | 72,803 | 961,024 | 0 | 5,914 | $0.234389 | commandExecution=15, fileChange=3, webSearch=3 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | T | PF | yes | 122.4s | 45,207 | 45,207 | 582,656 | 0 | 5,079 | $0.133947 | commandExecution=16, fileChange=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | T | base | yes | 133.7s | 62,953 | 62,953 | 591,872 | 0 | 5,419 | $0.154654 | commandExecution=14, fileChange=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | T2 | AS | yes | 141.4s | 54,236 | 54,236 | 619,264 | 0 | 5,298 | $0.147950 | commandExecution=17, fileChange=1 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | T2 | PF | yes | 180.7s | 67,261 | 67,261 | 761,600 | 0 | 7,396 | $0.207797 | commandExecution=19, fileChange=1, webSearch=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | luna | T2 | base | yes | 137.7s | 51,227 | 51,227 | 708,864 | 0 | 5,689 | $0.156247 | commandExecution=16, fileChange=3 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | A | AS | yes | 187.7s | 785 | 26,703 | 540,101 | 25,918 | 10,825 | $0.480654 | Bash=17, Edit=3, Read=8 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | A | PF | yes | 175.5s | 793 | 22,436 | 620,687 | 21,643 | 8,885 | $0.450104 | Bash=23, Edit=1, Glob=1, Grep=5, Read=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | A | base | yes | 256.0s | 769 | 18,348 | 345,247 | 17,579 | 5,589 | $0.293556 | Bash=15, Edit=1, Grep=2, Read=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | T | AS | yes | 247.0s | 10,976 | 42,753 | 753,767 | 31,777 | 5,257 | $0.506557 | Bash=17, Edit=1, Grep=3, Read=4 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | T | PF | yes | 427.9s | 11,000 | 50,415 | 1,442,559 | 39,415 | 14,699 | $0.900715 | Bash=27, Edit=3, Read=7 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | T | base | yes | 318.0s | 11,004 | 56,552 | 1,432,356 | 45,548 | 12,258 | $0.897869 | Bash=29, Edit=4, Grep=1, Read=5 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | T2 | AS | yes | 522.4s | 8,522 | 54,526 | 1,789,263 | 46,004 | 13,484 | $1.023653 | Bash=44, Edit=2, Read=5 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | T2 | PF | yes | 180.1s | 8,486 | 55,076 | 1,280,557 | 46,590 | 10,413 | $0.828354 | Bash=26, Edit=2, Read=5 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | sonnet | T2 | base | yes | 149.6s | 8,470 | 39,176 | 688,668 | 30,706 | 5,759 | $0.485625 | Bash=16, Edit=1, Grep=3, Read=5 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | A | AS | yes | 81.6s | 36,534 | 36,534 | 813,056 | 0 | 15,621 | n/a | commandExecution=40, fileChange=4 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | A | PF | yes | 92.5s | 65,083 | 65,083 | 1,272,320 | 0 | 14,096 | n/a | commandExecution=45, fileChange=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | A | base | yes | 144.4s | 63,657 | 63,657 | 1,247,104 | 0 | 15,696 | n/a | commandExecution=49, fileChange=3, webSearch=1 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | T | AS | yes | 261.0s | 110,162 | 110,162 | 4,379,136 | 0 | 38,784 | n/a | commandExecution=89, fileChange=5, mcpToolCall=1 | +0.0045 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | T | PF | yes | 130.4s | 74,966 | 74,966 | 2,225,920 | 0 | 16,117 | n/a | commandExecution=54, fileChange=2, webSearch=4 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | T | base | yes | 246.9s | 141,672 | 141,672 | 4,583,808 | 0 | 24,252 | n/a | commandExecution=94, fileChange=5 | -0.0017 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | T2 | AS | yes | 89.5s | 61,835 | 61,835 | 1,078,016 | 0 | 12,920 | n/a | commandExecution=39, fileChange=2, webSearch=2 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | T2 | PF | yes | 89.0s | 65,140 | 65,140 | 779,904 | 0 | 11,883 | n/a | commandExecution=32, fileChange=2, webSearch=1 | +0.0000 | +0.0000 |
| 3 | sympy__sympy-13757 | spark | T2 | base | yes | 69.1s | 38,025 | 38,025 | 1,105,664 | 0 | 8,617 | n/a | commandExecution=41, fileChange=3 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | haiku | A | AS | yes | 310.9s | 2,540 | 80,262 | 4,303,426 | 77,722 | 26,958 | $0.723117 | Bash=68, Edit=4, Read=18 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | haiku | A | PF | yes | 358.9s | 2,420 | 78,124 | 3,472,642 | 75,704 | 35,164 | $0.676912 | Bash=51, Edit=8, Read=16 | +0.0001 | +0.0007 |
| 4 | django__django-13033 | haiku | A | base | no | 462.5s | 2,540 | 88,482 | 4,648,801 | 85,942 | 43,144 | $0.855024 | Bash=62, Edit=10, Read=14, Write=3, bash=1 | +0.0006 | +0.0082 |
| 4 | django__django-13033 | haiku | T | AS | yes | 381.6s | 11,266 | 104,873 | 5,421,845 | 93,607 | 32,332 | $0.902325 | Bash=52, Edit=9, Glob=2, Read=19, Write=4 | +0.0000 | +0.0009 |
| 4 | django__django-13033 | haiku | T | PF | yes | 364.5s | 11,122 | 82,188 | 3,322,115 | 71,066 | 27,796 | $0.624445 | Bash=44, Edit=9, Grep=1, Read=14 | +0.0002 | +0.0023 |
| 4 | django__django-13033 | haiku | T | base | yes | 311.7s | 11,130 | 88,053 | 3,382,965 | 76,923 | 30,801 | $0.657278 | Bash=46, Edit=8, Read=15 | +0.0000 | +0.0005 |
| 4 | django__django-13033 | haiku | T2 | AS | yes | 253.5s | 9,715 | 76,341 | 3,030,153 | 66,626 | 21,053 | $0.551247 | Bash=47, Edit=6, Read=15, Write=1 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | haiku | T2 | PF | yes | 283.8s | 9,659 | 140,298 | 4,662,927 | 130,639 | 26,410 | $0.869280 | Bash=35, Edit=2, Grep=5, Read=12, Write=8 | -0.0085 | +0.0000 |
| 4 | django__django-13033 | haiku | T2 | base | yes | 354.1s | 9,979 | 109,241 | 6,485,530 | 99,262 | 29,266 | $1.003386 | Bash=63, Edit=18, Read=19, Write=2 | +0.0000 | +0.0009 |
| 4 | django__django-13033 | luna | A | AS | yes | 70.9s | 74,215 | 74,215 | 469,248 | 0 | 2,599 | $0.166734 | commandExecution=9, fileChange=1, webSearch=3 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | luna | A | PF | yes | 127.0s | 77,520 | 77,520 | 734,464 | 0 | 5,095 | $0.241536 | commandExecution=8, fileChange=1, webSearch=6 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | luna | A | base | yes | 79.0s | 49,711 | 49,711 | 512,512 | 0 | 2,537 | $0.146184 | commandExecution=10, fileChange=1, webSearch=3 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | luna | T | AS | yes | 82.9s | 64,605 | 64,605 | 473,856 | 0 | 3,256 | $0.171527 | commandExecution=8, fileChange=1, webSearch=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | luna | T | PF | yes | 139.8s | 64,718 | 64,718 | 872,448 | 0 | 5,418 | $0.214471 | commandExecution=18, fileChange=1, webSearch=3 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | luna | T | base | yes | 198.5s | 50,919 | 50,919 | 598,528 | 0 | 8,693 | $0.162930 | commandExecution=15, fileChange=4 | +0.0001 | +0.0002 |
| 4 | django__django-13033 | luna | T2 | AS | yes | 119.1s | 61,754 | 61,754 | 541,440 | 0 | 3,822 | $0.178830 | commandExecution=15, fileChange=1, webSearch=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | luna | T2 | PF | yes | 110.0s | 58,394 | 58,394 | 741,888 | 0 | 4,227 | $0.197945 | commandExecution=11, fileChange=1, webSearch=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | luna | T2 | base | yes | 107.8s | 63,520 | 63,520 | 724,224 | 0 | 4,249 | $0.211436 | commandExecution=12, fileChange=1, webSearch=5 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | A | AS | yes | 159.3s | 1,865 | 23,094 | 497,223 | 21,229 | 8,172 | $0.400934 | Bash=19, Edit=4, Read=3 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | sonnet | A | PF | yes | 928.7s | 1,945 | 65,065 | 2,579,582 | 63,120 | 22,907 | $1.498243 | Bash=52, Edit=8, Read=6 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | A | base | yes | 154.2s | 1,881 | 32,863 | 817,530 | 30,982 | 10,768 | $0.594492 | Bash=14, Edit=7, Grep=4, Read=9 | +0.0000 | +0.0000 |
| 4 | django__django-13033 | sonnet | T | AS | yes | 126.6s | 10,611 | 42,104 | 588,322 | 31,493 | 7,002 | $0.481010 | Bash=11, Edit=5, Read=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | T | PF | yes | 260.8s | 10,623 | 44,385 | 798,786 | 33,762 | 9,291 | $0.592104 | Bash=14, Edit=6, Grep=2, Read=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | T | base | yes | 111.4s | 10,615 | 39,642 | 617,418 | 29,027 | 7,989 | $0.489729 | Bash=16, Edit=6 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | T2 | AS | yes | 109.1s | 9,188 | 35,472 | 423,049 | 26,284 | 6,522 | $0.391535 | Bash=6, Edit=5, Grep=1, Read=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | T2 | PF | yes | 223.6s | 9,226 | 60,342 | 1,339,464 | 51,116 | 16,805 | $0.969820 | Bash=21, Edit=7, Read=7 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | sonnet | T2 | base | yes | 195.1s | 9,238 | 55,293 | 1,515,947 | 46,055 | 11,394 | $0.911260 | Bash=26, Edit=4, Grep=2, Read=9 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | spark | A | AS | yes | 147.3s | 102,748 | 102,748 | 2,242,816 | 0 | 13,917 | n/a | commandExecution=51, fileChange=4, webSearch=4 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | spark | A | PF | yes | 92.5s | 55,156 | 55,156 | 1,052,416 | 0 | 15,292 | n/a | commandExecution=38, fileChange=5, webSearch=1 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | spark | A | base | yes | 84.3s | 68,857 | 68,857 | 1,467,648 | 0 | 19,119 | n/a | commandExecution=43, fileChange=4 | +0.0001 | +0.0003 |
| 4 | django__django-13033 | spark | T | AS | yes | 98.6s | 65,183 | 65,183 | 1,915,776 | 0 | 15,789 | n/a | commandExecution=45, fileChange=6 | +0.0001 | +0.0003 |
| 4 | django__django-13033 | spark | T | PF | yes | 91.8s | 75,256 | 75,256 | 2,221,440 | 0 | 19,408 | n/a | commandExecution=48, fileChange=7 | +0.0001 | +0.0003 |
| 4 | django__django-13033 | spark | T | base | yes | 258.7s | 163,390 | 163,390 | 5,939,328 | 0 | 47,368 | n/a | commandExecution=90, fileChange=10 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | spark | T2 | AS | yes | 107.5s | 55,458 | 55,458 | 1,869,184 | 0 | 18,101 | n/a | commandExecution=41, fileChange=7 | +0.0001 | +0.0004 |
| 4 | django__django-13033 | spark | T2 | PF | yes | 76.5s | 49,750 | 49,750 | 1,101,312 | 0 | 17,018 | n/a | commandExecution=28, fileChange=4 | +0.0001 | +0.0005 |
| 4 | django__django-13033 | spark | T2 | base | yes | 66.3s | 47,865 | 47,865 | 1,121,408 | 0 | 15,683 | n/a | commandExecution=23, fileChange=7 | +0.0001 | +0.0003 |
| 5 | pydata__xarray-4075 | haiku | A | AS | yes | 227.6s | 1,808 | 56,874 | 2,073,910 | 55,066 | 15,982 | $0.399241 | Bash=44, Edit=3, Read=9 | +0.0033 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | A | PF | yes | 160.5s | 1,648 | 59,610 | 1,191,143 | 57,962 | 12,997 | $0.301671 | Bash=27, Edit=3, Read=6 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | A | base | yes | 140.2s | 1,640 | 59,097 | 1,189,305 | 57,457 | 10,774 | $0.289354 | Bash=27, Edit=2, Read=6 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | T | AS | yes | 132.6s | 8,735 | 70,743 | 1,016,908 | 62,008 | 12,202 | $0.295452 | Bash=20, Edit=2, Read=5 | +0.0033 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | T | PF | yes | 141.0s | 8,743 | 64,263 | 1,106,274 | 55,520 | 12,928 | $0.295050 | Bash=20, Edit=2, Read=6 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | T | base | yes | 184.5s | 8,895 | 73,630 | 2,132,756 | 64,735 | 14,487 | $0.424076 | Bash=38, Edit=3, Read=5, bash=1 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | T2 | AS | yes | 129.1s | 8,118 | 65,202 | 1,257,668 | 57,084 | 10,086 | $0.298483 | Bash=25, Edit=2, Read=4, bash=1 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | T2 | PF | yes | 182.2s | 8,182 | 64,803 | 1,590,777 | 56,621 | 15,727 | $0.359137 | Bash=30, Edit=3, Glob=1, Read=5, bash=1 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | haiku | T2 | base | yes | 275.8s | 8,374 | 77,137 | 3,119,643 | 68,763 | 20,893 | $0.562329 | Bash=53, Edit=4, Glob=1, Read=6 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | A | AS | yes | 142.0s | 48,434 | 48,434 | 453,376 | 0 | 6,509 | $0.132826 | commandExecution=17, fileChange=1 | +0.0000 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | A | PF | yes | 204.9s | 81,977 | 81,977 | 989,440 | 0 | 7,863 | $0.278099 | commandExecution=19, fileChange=2, webSearch=5 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | A | base | yes | 175.5s | 78,822 | 78,822 | 465,152 | 0 | 7,692 | $0.201489 | commandExecution=12, fileChange=2, webSearch=3 | +0.0130 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | T | AS | yes | 117.7s | 45,833 | 45,833 | 441,088 | 0 | 4,850 | $0.119042 | commandExecution=14, fileChange=1 | +0.0000 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | T | PF | yes | 152.7s | 54,256 | 54,256 | 562,688 | 0 | 6,602 | $0.150137 | commandExecution=15, fileChange=1 | +0.0000 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | T | base | yes | 188.3s | 96,040 | 96,040 | 1,288,192 | 0 | 7,627 | $0.330621 | commandExecution=16, fileChange=3, webSearch=6 | +0.0161 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | T2 | AS | yes | 138.4s | 68,001 | 68,001 | 729,344 | 0 | 5,097 | $0.231517 | commandExecution=8, fileChange=1, webSearch=6 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | T2 | PF | yes | 238.0s | 88,564 | 88,564 | 1,362,176 | 0 | 10,340 | $0.326822 | commandExecution=18, fileChange=4, webSearch=4 | +0.0161 | +0.0000 |
| 5 | pydata__xarray-4075 | luna | T2 | base | yes | 180.5s | 83,559 | 83,559 | 583,168 | 0 | 7,996 | $0.189852 | commandExecution=16, fileChange=2 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | A | AS | yes | 314.5s | 1,419 | 28,186 | 738,278 | 26,767 | 10,077 | $0.534645 | Bash=19, Edit=10, Glob=1, Read=3 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | A | PF | yes | 227.4s | 1,399 | 22,302 | 463,549 | 20,903 | 6,666 | $0.365828 | Bash=11, Edit=6, Grep=2, Read=4 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | A | base | yes | 275.1s | 1,421 | 32,647 | 808,276 | 31,226 | 11,149 | $0.598495 | Bash=14, Edit=14, Grep=1, Read=5 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | T | AS | yes | 253.4s | 8,558 | 40,339 | 649,929 | 31,781 | 5,698 | $0.479629 | Bash=15, Edit=4, Read=4 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | T | PF | yes | 283.9s | 8,566 | 45,535 | 840,172 | 36,969 | 11,468 | $0.654424 | Bash=16, Edit=7, Read=4 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | T | base | yes | 279.7s | 8,578 | 48,543 | 1,106,410 | 39,965 | 9,480 | $0.722487 | Bash=20, Edit=8, Read=5 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | T2 | AS | yes | 96.9s | 7,885 | 31,735 | 383,719 | 23,850 | 5,264 | $0.344965 | Bash=8, Edit=3, Glob=1, Read=3 | +0.0000 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | T2 | PF | yes | 254.8s | 7,907 | 39,861 | 756,397 | 31,954 | 7,207 | $0.534613 | Bash=18, Edit=4, Read=4 | +0.0130 | +0.0000 |
| 5 | pydata__xarray-4075 | sonnet | T2 | base | yes | 264.3s | 7,929 | 58,123 | 1,355,721 | 50,194 | 10,683 | $0.876066 | Bash=20, Edit=10, Read=7 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | A | AS | yes | 109.0s | 58,270 | 58,270 | 1,094,016 | 0 | 12,241 | n/a | commandExecution=40, fileChange=2 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | A | PF | yes | 148.4s | 44,466 | 44,466 | 1,132,416 | 0 | 15,398 | n/a | commandExecution=42, fileChange=2, mcpToolCall=1, webSearch=1 | +0.0000 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | A | base | yes | 75.8s | 38,656 | 38,656 | 846,848 | 0 | 12,456 | n/a | commandExecution=31, fileChange=4 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | T | AS | yes | 112.9s | 66,793 | 66,793 | 2,013,568 | 0 | 17,291 | n/a | commandExecution=49, fileChange=3, webSearch=1 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | T | PF | yes | 108.1s | 74,983 | 74,983 | 1,514,496 | 0 | 21,287 | n/a | commandExecution=36, fileChange=4 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | T | base | yes | 76.5s | 76,762 | 76,762 | 1,330,816 | 0 | 18,678 | n/a | commandExecution=35, fileChange=3, webSearch=1 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | T2 | AS | yes | 104.7s | 51,605 | 51,605 | 1,147,648 | 0 | 15,343 | n/a | commandExecution=33, fileChange=3 | +0.0098 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | T2 | PF | yes | 98.7s | 58,944 | 58,944 | 1,382,784 | 0 | 15,007 | n/a | commandExecution=33, fileChange=5 | +0.0066 | +0.0000 |
| 5 | pydata__xarray-4075 | spark | T2 | base | yes | 88.8s | 65,086 | 65,086 | 1,296,128 | 0 | 17,868 | n/a | commandExecution=39, fileChange=3, webSearch=1 | +0.0066 | +0.0000 |
| 6 | django__django-13794 | haiku | A | AS | no | 245.9s | 1,183 | 65,797 | 3,139,529 | 64,614 | 18,533 | $0.537029 | Bash=49, Edit=7, Read=15 | +0.0006 | -0.0036 |
| 6 | django__django-13794 | haiku | A | PF | no | 163.2s | 975 | 45,285 | 1,373,488 | 44,310 | 13,094 | $0.292414 | Bash=30, Edit=6, Read=9 | +0.0006 | -0.0036 |
| 6 | django__django-13794 | haiku | A | base | no | 206.1s | 1,055 | 60,658 | 1,995,351 | 59,603 | 17,742 | $0.408506 | Bash=37, Edit=7, Read=11 | +0.0015 | -0.0051 |
| 6 | django__django-13794 | haiku | T | AS | no | 246.7s | 9,799 | 78,152 | 2,239,558 | 68,353 | 22,643 | $0.483676 | Bash=35, Edit=5, Glob=1, Grep=1, Read=9 | +0.0022 | -0.0036 |
| 6 | django__django-13794 | haiku | T | PF | no | 337.4s | 9,793 | 57,149 | 1,739,683 | 47,356 | 49,611 | $0.526528 | Bash=34, Edit=5, Grep=1, Read=9 | +0.0019 | -0.0053 |
| 6 | django__django-13794 | haiku | T | base | no | 275.4s | 10,015 | 63,183 | 2,905,548 | 53,168 | 23,139 | $0.522601 | Bash=56, Edit=9, Read=13 | +0.0015 | -0.0051 |
| 6 | django__django-13794 | haiku | T2 | AS | no | 183.5s | 7,257 | 71,648 | 1,770,530 | 64,391 | 17,524 | $0.400712 | Bash=26, Edit=6, Read=11 | +0.0006 | -0.0036 |
| 6 | django__django-13794 | haiku | T2 | PF | no | 207.4s | 7,337 | 55,008 | 1,775,375 | 47,671 | 18,177 | $0.371102 | Bash=38, Edit=6, Glob=1, Grep=1, Read=7 | +0.0030 | -0.0040 |
| 6 | django__django-13794 | haiku | T2 | base | no | 166.3s | 7,289 | 61,130 | 1,821,470 | 53,841 | 12,719 | $0.360713 | Bash=30, Edit=6, Glob=1, Grep=2, Read=8 | +0.0002 | -0.0024 |
| 6 | django__django-13794 | luna | A | AS | yes | 82.2s | 35,331 | 35,331 | 306,176 | 0 | 2,924 | $0.123493 | commandExecution=7, fileChange=1, webSearch=4 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | luna | A | PF | no | 367.3s | 80,882 | 80,882 | 1,281,792 | 0 | 15,268 | $0.300669 | commandExecution=30, fileChange=4 | +0.0015 | -0.0040 |
| 6 | django__django-13794 | luna | A | base | yes | 112.7s | 63,737 | 63,737 | 767,744 | 0 | 3,749 | $0.213005 | commandExecution=10, fileChange=1, webSearch=5 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | luna | T | AS | yes | 122.8s | 110,505 | 110,505 | 617,728 | 0 | 3,925 | $0.225828 | commandExecution=12, fileChange=1, webSearch=3 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | luna | T | PF | yes | 127.0s | 72,657 | 72,657 | 479,488 | 0 | 4,602 | $0.178218 | commandExecution=13, fileChange=1, webSearch=3 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | luna | T | base | no | 163.9s | 50,365 | 50,365 | 507,136 | 0 | 7,150 | $0.173979 | commandExecution=13, fileChange=2, webSearch=3 | -0.0007 | -0.0012 |
| 6 | django__django-13794 | luna | T2 | AS | yes | 75.2s | 39,599 | 39,599 | 317,952 | 0 | 2,668 | $0.117402 | commandExecution=9, fileChange=1, webSearch=3 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | luna | T2 | PF | yes | 153.2s | 69,526 | 69,526 | 588,288 | 0 | 4,371 | $0.214581 | commandExecution=11, fileChange=1, webSearch=6 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | luna | T2 | base | yes | 98.5s | 59,021 | 59,021 | 507,904 | 0 | 3,770 | $0.162431 | commandExecution=11, fileChange=2, webSearch=3 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | sonnet | A | AS | no | 129.4s | 642 | 13,542 | 245,279 | 12,900 | 6,972 | $0.256108 | Bash=8, Edit=6, Read=3 | +0.0002 | -0.0024 |
| 6 | django__django-13794 | sonnet | A | PF | no | 261.4s | 678 | 43,370 | 975,173 | 42,692 | 16,944 | $0.803536 | Bash=22, Edit=6, Grep=1, Read=6 | +0.0047 | +0.0520 |
| 6 | django__django-13794 | sonnet | A | base | yes | 236.7s | 694 | 37,702 | 1,078,609 | 37,008 | 15,847 | $0.784036 | Bash=24, Edit=8, Grep=5, Read=6 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | sonnet | T | AS | no | 106.0s | 9,422 | 42,700 | 491,821 | 33,278 | 7,228 | $0.464986 | Bash=7, Edit=6, Grep=2, Read=4 | +0.0002 | -0.0024 |
| 6 | django__django-13794 | sonnet | T | PF | yes | 182.6s | 9,440 | 49,095 | 859,060 | 39,655 | 13,073 | $0.701129 | Bash=14, Edit=8, Grep=2, Read=4 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | sonnet | T | base | no | 417.9s | 9,460 | 48,785 | 1,201,012 | 39,325 | 16,085 | $0.846995 | Bash=26, Edit=6, Grep=1, Read=5 | +0.0015 | -0.0040 |
| 6 | django__django-13794 | sonnet | T2 | AS | yes | 143.5s | 6,968 | 43,423 | 772,192 | 36,455 | 6,176 | $0.549974 | Bash=22, Edit=4, Grep=2, Read=3 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | sonnet | T2 | PF | yes | 174.0s | 6,958 | 51,943 | 745,680 | 44,985 | 8,818 | $0.632800 | Bash=17, Edit=5, Grep=1, Read=3 | +0.0031 | +0.0022 |
| 6 | django__django-13794 | sonnet | T2 | base | no | 291.0s | 6,966 | 36,889 | 779,644 | 29,923 | 10,796 | $0.582291 | Bash=16, Edit=5, Glob=1, Grep=4, Read=4 | +0.0006 | -0.0036 |
| 6 | django__django-13794 | spark | A | AS | no | 98.4s | 37,560 | 37,560 | 840,320 | 0 | 10,006 | n/a | commandExecution=36, fileChange=3 | +0.0002 | -0.0024 |
| 6 | django__django-13794 | spark | A | PF | no | 86.1s | 36,156 | 36,156 | 920,832 | 0 | 10,447 | n/a | commandExecution=38, fileChange=5 | -0.0007 | -0.0021 |
| 6 | django__django-13794 | spark | A | base | no | 60.6s | 45,305 | 45,305 | 705,920 | 0 | 9,822 | n/a | commandExecution=35, fileChange=3 | +0.0015 | -0.0040 |
| 6 | django__django-13794 | spark | T | AS | no | 63.5s | 39,783 | 39,783 | 867,072 | 0 | 8,060 | n/a | commandExecution=24, fileChange=4, mcpToolCall=1 | -0.0007 | -0.0021 |
| 6 | django__django-13794 | spark | T | PF | no | 48.2s | 33,396 | 33,396 | 606,720 | 0 | 9,050 | n/a | commandExecution=24, fileChange=2 | -0.0007 | -0.0021 |
| 6 | django__django-13794 | spark | T | base | no | 87.5s | 41,323 | 41,323 | 1,166,976 | 0 | 14,716 | n/a | commandExecution=40, fileChange=5, webSearch=1 | +0.0015 | -0.0040 |
| 6 | django__django-13794 | spark | T2 | AS | no | 63.7s | 46,034 | 46,034 | 629,760 | 0 | 8,347 | n/a | commandExecution=30, fileChange=2, mcpToolCall=1, webSearch=1 | +0.0002 | -0.0024 |
| 6 | django__django-13794 | spark | T2 | PF | no | 50.1s | 43,527 | 43,527 | 559,872 | 0 | 9,997 | n/a | commandExecution=25, fileChange=2 | -0.0007 | -0.0012 |
| 6 | django__django-13794 | spark | T2 | base | no | 48.7s | 45,033 | 45,033 | 687,872 | 0 | 10,351 | n/a | commandExecution=23, fileChange=2 | +0.0002 | -0.0034 |
| 7 | matplotlib__matplotlib-24026 | haiku | A | AS | yes | 347.1s | 1,975 | 61,796 | 3,324,368 | 59,821 | 26,356 | $0.585834 | Bash=70, Edit=5, Read=7, Write=6 | -0.0077 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | A | PF | yes | 508.9s | 1,855 | 64,858 | 2,955,849 | 63,003 | 27,856 | $0.562726 | Bash=53, Edit=3, Read=11, Write=6 | -0.0128 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | A | base | yes | 278.8s | 1,807 | 58,193 | 2,179,640 | 56,386 | 26,489 | $0.464988 | Bash=48, Edit=6, Read=13 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | T | AS | yes | 370.4s | 7,706 | 70,064 | 3,000,856 | 62,358 | 29,304 | $0.579028 | Bash=59, Edit=3, Read=12, Write=1 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | T | PF | yes | 320.4s | 7,658 | 72,090 | 2,917,735 | 64,432 | 28,085 | $0.568720 | Bash=47, Edit=4, Read=12, Write=6 | -0.0066 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | T | base | yes | 342.9s | 7,818 | 75,959 | 3,578,388 | 68,141 | 30,650 | $0.655189 | Bash=65, Edit=6, Read=18 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | T2 | AS | no | 378.2s | 7,514 | 96,904 | 4,104,083 | 89,390 | 28,562 | $0.739512 | Bash=58, Edit=7, Glob=1, Read=11, Write=7 | -0.3598 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | T2 | PF | no | 237.5s | 7,282 | 64,085 | 2,096,020 | 56,803 | 20,381 | $0.432395 | Bash=28, Edit=8, Read=19 | -0.0123 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | haiku | T2 | base | yes | 385.8s | 7,402 | 74,431 | 2,917,637 | 67,029 | 26,014 | $0.563294 | Bash=45, Edit=6, Read=17, Write=2 | -0.0003 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | A | AS | yes | 218.4s | 46,187 | 46,187 | 562,944 | 0 | 8,728 | $0.194849 | commandExecution=20, fileChange=2, webSearch=4 | -0.0128 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | A | PF | yes | 299.2s | 105,932 | 105,932 | 1,324,288 | 0 | 9,976 | $0.368217 | commandExecution=23, fileChange=5, webSearch=7 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | A | base | yes | 283.5s | 104,111 | 104,111 | 2,111,232 | 0 | 9,401 | $0.401640 | commandExecution=27, fileChange=3, webSearch=3 | -0.0142 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | T | AS | yes | 256.4s | 64,681 | 64,681 | 970,240 | 0 | 8,642 | $0.213557 | commandExecution=20, fileChange=5 | -0.0128 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | T | PF | yes | 311.3s | 104,314 | 104,314 | 2,604,032 | 0 | 9,672 | $0.492749 | commandExecution=23, fileChange=3, webSearch=7 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | T | base | yes | 258.5s | 125,400 | 125,400 | 2,936,576 | 0 | 9,132 | $0.513850 | commandExecution=26, fileChange=2, imageView=2, webSearch=4 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | T2 | AS | yes | 322.4s | 117,557 | 117,557 | 2,533,888 | 0 | 10,712 | $0.495218 | commandExecution=22, fileChange=3, webSearch=6 | -0.0142 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | T2 | PF | yes | 257.3s | 113,881 | 113,881 | 2,350,592 | 0 | 7,112 | $0.421612 | commandExecution=19, fileChange=1, webSearch=3 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | luna | T2 | base | yes | 286.1s | 127,607 | 127,607 | 2,753,536 | 0 | 9,621 | $0.590687 | commandExecution=21, fileChange=4, webSearch=13 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | A | AS | yes | 648.0s | 1,378 | 59,480 | 1,962,174 | 58,102 | 16,117 | $1.180479 | Bash=42, Edit=5, Glob=1, Read=8, Write=1 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | A | PF | yes | 621.7s | 1,388 | 92,282 | 3,197,642 | 90,894 | 19,219 | $1.794432 | Bash=51, Edit=5, Read=6 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | A | base | yes | 747.5s | 1,412 | 85,639 | 3,505,674 | 84,227 | 24,186 | $1.921416 | Bash=62, Edit=6, Grep=1, Read=5 | -0.0331 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | T | AS | yes | 543.1s | 7,209 | 91,607 | 2,610,206 | 84,398 | 19,377 | $1.587388 | Bash=42, Edit=7, Read=6 | -0.0188 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | T | PF | yes | 389.5s | 7,209 | 118,762 | 3,730,708 | 111,553 | 19,501 | $2.088328 | Bash=44, Edit=5, Read=5, Write=1 | -0.0188 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | T | base | yes | 308.6s | 7,159 | 47,410 | 903,652 | 40,251 | 10,986 | $0.684515 | Bash=18, Edit=7, Glob=1, Read=4 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | T2 | AS | yes | 588.1s | 6,979 | 104,711 | 4,104,014 | 97,732 | 24,716 | $2.195427 | Bash=56, Edit=7, Read=7, Write=2 | -0.0188 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | T2 | PF | yes | 559.9s | 6,945 | 61,198 | 1,998,436 | 54,253 | 18,745 | $1.213243 | Bash=44, Edit=7, Read=4 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | sonnet | T2 | base | yes | 223.5s | 6,921 | 57,231 | 1,570,339 | 50,310 | 15,371 | $1.010474 | Bash=33, Edit=5, Read=5 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | A | AS | yes | 286.2s | 135,142 | 135,142 | 3,617,280 | 0 | 34,429 | n/a | commandExecution=77, fileChange=3, webSearch=5 | -0.0297 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | A | PF | yes | 120.0s | 58,515 | 58,515 | 1,386,112 | 0 | 20,487 | n/a | commandExecution=46, fileChange=4 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | A | base | yes | 170.3s | 74,953 | 74,953 | 2,827,648 | 0 | 25,021 | n/a | commandExecution=60, fileChange=5, webSearch=1 | -0.0172 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | T | AS | yes | 180.9s | 78,854 | 78,854 | 2,942,080 | 0 | 25,590 | n/a | commandExecution=64, fileChange=5 | -0.0314 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | T | PF | yes | 228.1s | 79,321 | 79,321 | 3,346,048 | 0 | 20,314 | n/a | commandExecution=59, fileChange=3 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | T | base | yes | 186.7s | 96,543 | 96,543 | 3,955,200 | 0 | 25,168 | n/a | commandExecution=73, fileChange=8 | -0.0102 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | T2 | AS | yes | 151.2s | 75,798 | 75,798 | 2,541,312 | 0 | 20,956 | n/a | commandExecution=62, fileChange=4, webSearch=1 | -0.0157 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | T2 | PF | yes | 80.5s | 110,437 | 110,437 | 2,479,360 | 0 | 13,428 | n/a | commandExecution=32, fileChange=3 | -0.0314 | +0.0000 |
| 7 | matplotlib__matplotlib-24026 | spark | T2 | base | yes | 132.9s | 86,479 | 86,479 | 2,235,136 | 0 | 18,866 | n/a | commandExecution=55, fileChange=5 | -0.0128 | +0.0000 |
| 8 | django__django-11163 | haiku | A | AS | yes | 165.9s | 1,051 | 54,612 | 2,005,532 | 53,561 | 12,918 | $0.373316 | Bash=34, Edit=4, Grep=2, Read=8, Write=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | A | PF | yes | 120.4s | 971 | 40,999 | 1,321,808 | 40,028 | 10,277 | $0.264593 | Bash=26, Edit=3, Grep=2, Read=7, Write=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | A | base | yes | 182.1s | 1,051 | 54,075 | 1,875,468 | 53,024 | 17,122 | $0.380256 | Bash=32, Edit=3, Grep=1, Read=8, Write=5 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | T | AS | yes | 145.1s | 8,611 | 44,863 | 1,290,168 | 36,252 | 13,923 | $0.279747 | Bash=28, Edit=2, Read=9, Write=4 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | T | PF | yes | 154.7s | 8,627 | 49,890 | 1,482,038 | 41,263 | 11,627 | $0.297492 | Bash=31, Edit=3, Read=11 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | T | base | yes | 127.2s | 8,571 | 42,832 | 1,047,370 | 34,261 | 9,764 | $0.230650 | Bash=26, Edit=3, Read=9 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | T2 | AS | yes | 169.7s | 6,987 | 58,564 | 1,731,429 | 51,577 | 13,461 | $0.350589 | Bash=29, Edit=4, Read=14 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | T2 | PF | yes | 120.0s | 6,867 | 38,580 | 792,628 | 31,713 | 9,071 | $0.194911 | Bash=23, Edit=4, Read=6 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | haiku | T2 | base | yes | 141.7s | 7,003 | 49,896 | 1,524,656 | 42,893 | 13,400 | $0.312255 | Bash=34, Edit=3, Read=12 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | A | AS | yes | 43.5s | 22,571 | 22,571 | 79,616 | 0 | 1,160 | $0.037493 | commandExecution=7, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | A | PF | yes | 62.5s | 30,342 | 30,342 | 116,224 | 0 | 1,728 | $0.052332 | commandExecution=8, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | A | base | yes | 47.0s | 19,706 | 19,706 | 89,600 | 0 | 1,521 | $0.037792 | commandExecution=7, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | T | AS | yes | 53.9s | 45,988 | 45,988 | 141,056 | 0 | 1,342 | $0.068146 | commandExecution=7, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | T | PF | yes | 77.4s | 41,232 | 41,232 | 205,568 | 0 | 2,079 | $0.074263 | commandExecution=8, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | T | base | yes | 33.5s | 20,103 | 20,103 | 88,576 | 0 | 965 | $0.034751 | commandExecution=5, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | T2 | AS | yes | 41.1s | 33,624 | 33,624 | 85,504 | 0 | 1,031 | $0.048360 | commandExecution=5, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | T2 | PF | yes | 59.5s | 23,848 | 23,848 | 156,160 | 0 | 1,876 | $0.050720 | commandExecution=8, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | luna | T2 | base | yes | 29.7s | 23,335 | 23,335 | 75,264 | 0 | 871 | $0.036087 | commandExecution=5, fileChange=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | A | AS | yes | 88.3s | 694 | 16,734 | 324,652 | 16,040 | 6,424 | $0.290588 | Bash=12, Edit=4, Grep=2, Read=3 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | A | PF | yes | 103.5s | 698 | 16,385 | 376,896 | 15,687 | 5,879 | $0.295990 | Bash=16, Edit=3, Grep=2, Read=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | A | base | yes | 106.9s | 700 | 12,515 | 339,211 | 11,815 | 5,049 | $0.249018 | Bash=15, Edit=3, Grep=3, Read=3 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | T | AS | yes | 119.0s | 8,324 | 43,657 | 870,808 | 35,333 | 7,312 | $0.591196 | Bash=12, Edit=4, Grep=8, Read=8 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | T | PF | yes | 134.8s | 8,304 | 31,602 | 546,360 | 23,298 | 5,620 | $0.396202 | Bash=8, Edit=5, Grep=4, Read=5 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | T | base | yes | 44.0s | 8,282 | 24,628 | 237,413 | 16,346 | 2,412 | $0.213630 | Bash=7, Edit=2, Read=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | T2 | AS | yes | 150.3s | 6,648 | 29,375 | 492,201 | 22,727 | 6,064 | $0.381552 | Bash=11, Edit=3, Grep=3, Read=5 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | T2 | PF | yes | 184.4s | 6,636 | 22,987 | 323,978 | 16,351 | 3,343 | $0.251968 | Bash=8, Edit=3, Grep=1, Read=4 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | sonnet | T2 | base | yes | 121.4s | 6,642 | 27,538 | 444,908 | 20,896 | 4,449 | $0.332135 | Bash=10, Edit=3, Grep=2, Read=4 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | A | AS | yes | 50.7s | 13,197 | 13,197 | 239,232 | 0 | 3,076 | n/a | commandExecution=18, fileChange=3 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | A | PF | yes | 61.9s | 18,218 | 18,218 | 344,576 | 0 | 4,211 | n/a | commandExecution=20, fileChange=2, mcpToolCall=1 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | A | base | yes | 24.6s | 10,155 | 10,155 | 135,424 | 0 | 1,837 | n/a | commandExecution=12, fileChange=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | T | AS | yes | 44.0s | 34,973 | 34,973 | 292,608 | 0 | 3,046 | n/a | commandExecution=16, fileChange=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | T | PF | yes | 40.4s | 34,787 | 34,787 | 365,696 | 0 | 2,908 | n/a | commandExecution=17, fileChange=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | T | base | yes | 29.5s | 16,946 | 16,946 | 279,424 | 0 | 2,361 | n/a | commandExecution=15, fileChange=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | T2 | AS | yes | 64.2s | 21,037 | 21,037 | 466,432 | 0 | 3,425 | n/a | commandExecution=25, fileChange=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | T2 | PF | yes | 44.3s | 39,932 | 39,932 | 475,136 | 0 | 3,911 | n/a | commandExecution=22, fileChange=2 | +0.0000 | +0.0000 |
| 8 | django__django-11163 | spark | T2 | base | yes | 32.1s | 36,791 | 36,791 | 230,016 | 0 | 2,782 | n/a | commandExecution=14, fileChange=2 | +0.0000 | +0.0000 |
| 9 | django__django-16612 | haiku | A | AS | yes | 207.8s | 1,186 | 45,701 | 1,612,244 | 44,515 | 17,177 | $0.337325 | Bash=30, Edit=2, Read=11, Write=6 | +0.0016 | -0.0033 |
| 9 | django__django-16612 | haiku | A | PF | yes | 116.8s | 1,082 | 36,877 | 989,517 | 35,795 | 9,893 | $0.221089 | Bash=22, Edit=3, Read=10, Write=1 | +0.0012 | -0.0029 |
| 9 | django__django-16612 | haiku | A | base | yes | 166.2s | 1,202 | 45,290 | 1,675,669 | 44,088 | 11,685 | $0.315370 | Bash=37, Edit=2, Glob=1, Grep=1, Read=9, Write=1 | +0.0012 | -0.0029 |
| 9 | django__django-16612 | haiku | T | AS | yes | 177.6s | 9,431 | 56,044 | 1,577,216 | 46,613 | 13,663 | $0.328694 | Bash=31, Edit=2, Read=13, Write=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | haiku | T | PF | yes | 218.1s | 9,591 | 71,640 | 3,037,915 | 62,049 | 17,506 | $0.525011 | Bash=44, Edit=3, Read=18, Write=1 | -0.0014 | -0.0007 |
| 9 | django__django-16612 | haiku | T | base | yes | 191.3s | 9,471 | 80,772 | 2,600,850 | 71,301 | 13,539 | $0.479853 | Bash=36, Edit=3, Read=12 | -0.0014 | -0.0007 |
| 9 | django__django-16612 | haiku | T2 | AS | yes | 306.6s | 9,789 | 70,147 | 3,284,087 | 60,358 | 19,438 | $0.556104 | Bash=51, Edit=4, Read=24, Write=1 | -0.0014 | -0.0007 |
| 9 | django__django-16612 | haiku | T2 | PF | yes | 183.6s | 9,621 | 55,631 | 1,837,070 | 46,010 | 13,309 | $0.351893 | Bash=37, Edit=3, Read=18 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | haiku | T2 | base | yes | 251.3s | 9,725 | 71,652 | 2,937,733 | 61,927 | 20,285 | $0.528777 | Bash=47, Edit=3, Read=22 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | A | AS | yes | 124.2s | 42,842 | 42,842 | 182,016 | 0 | 2,927 | $0.078606 | commandExecution=10, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | A | PF | yes | 150.7s | 37,384 | 37,384 | 206,336 | 0 | 3,420 | $0.078538 | commandExecution=12, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | A | base | yes | 59.3s | 26,187 | 26,187 | 128,512 | 0 | 2,017 | $0.051140 | commandExecution=9, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | T | AS | yes | 73.5s | 28,645 | 28,645 | 132,352 | 0 | 1,659 | $0.051834 | commandExecution=5, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | T | PF | yes | 99.7s | 29,115 | 29,115 | 264,960 | 0 | 2,738 | $0.072039 | commandExecution=7, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | T | base | yes | 67.0s | 29,219 | 29,219 | 229,632 | 0 | 2,427 | $0.066744 | commandExecution=9, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | T2 | AS | yes | 73.3s | 26,774 | 26,774 | 219,392 | 0 | 1,905 | $0.060143 | commandExecution=7, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | T2 | PF | yes | 109.3s | 51,466 | 51,466 | 215,040 | 0 | 2,669 | $0.088984 | commandExecution=6, fileChange=1 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | luna | T2 | base | yes | 107.3s | 70,575 | 70,575 | 469,760 | 0 | 3,584 | $0.139055 | commandExecution=11, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | A | AS | yes | 127.3s | 821 | 14,933 | 246,377 | 14,112 | 3,626 | $0.213668 | Bash=4, Edit=4, Glob=1, Grep=4, Read=4 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | A | PF | yes | 89.6s | 829 | 12,817 | 294,687 | 11,988 | 3,526 | $0.213961 | Bash=11, Edit=3, Grep=4, Read=3 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | A | base | yes | 71.8s | 819 | 11,119 | 209,967 | 10,300 | 3,049 | $0.171232 | Bash=7, Edit=2, Grep=3, Read=4 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | T | AS | yes | 95.7s | 9,098 | 35,413 | 550,070 | 26,315 | 4,135 | $0.393922 | Bash=13, Edit=3, Read=5 | +0.0040 | -0.0054 |
| 9 | django__django-16612 | sonnet | T | PF | yes | 112.6s | 9,094 | 34,730 | 478,845 | 25,636 | 4,839 | $0.378989 | Bash=11, Edit=3, Grep=1, Read=4 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | T | base | yes | 50.8s | 9,076 | 29,855 | 233,011 | 20,779 | 2,653 | $0.243282 | Bash=5, Edit=2, Read=3 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | T2 | AS | yes | 144.1s | 9,186 | 35,075 | 480,905 | 25,889 | 4,537 | $0.376702 | Bash=13, Edit=2, Grep=1, Read=2 | +0.0040 | -0.0054 |
| 9 | django__django-16612 | sonnet | T2 | PF | yes | 90.4s | 9,190 | 32,455 | 510,889 | 23,265 | 3,346 | $0.352131 | Bash=13, Edit=3, Read=4 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | sonnet | T2 | base | yes | 257.6s | 9,208 | 40,968 | 787,829 | 31,760 | 5,375 | $0.516622 | Bash=21, Edit=3, Read=5 | +0.0028 | -0.0044 |
| 9 | django__django-16612 | spark | A | AS | yes | 63.4s | 23,431 | 23,431 | 496,256 | 0 | 5,275 | n/a | commandExecution=27, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | A | PF | yes | 66.5s | 34,239 | 34,239 | 293,888 | 0 | 5,121 | n/a | commandExecution=19, fileChange=3 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | A | base | yes | 45.6s | 21,405 | 21,405 | 401,024 | 0 | 5,746 | n/a | commandExecution=24, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | T | AS | yes | 76.2s | 37,080 | 37,080 | 990,464 | 0 | 8,564 | n/a | commandExecution=35, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | T | PF | yes | 95.0s | 33,603 | 33,603 | 898,432 | 0 | 9,793 | n/a | commandExecution=33, fileChange=3 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | T | base | yes | 71.7s | 36,991 | 36,991 | 1,008,128 | 0 | 9,126 | n/a | commandExecution=38, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | T2 | AS | yes | 59.5s | 31,165 | 31,165 | 607,360 | 0 | 6,909 | n/a | commandExecution=24, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | T2 | PF | yes | 44.3s | 24,052 | 24,052 | 459,776 | 0 | 6,166 | n/a | commandExecution=18, fileChange=2 | +0.0008 | -0.0007 |
| 9 | django__django-16612 | spark | T2 | base | yes | 39.9s | 29,727 | 29,727 | 577,024 | 0 | 6,029 | n/a | commandExecution=23, fileChange=2 | +0.0008 | -0.0007 |
| 10 | django__django-11551 | haiku | A | AS | yes | 184.9s | 2,419 | 49,359 | 1,463,206 | 46,940 | 14,491 | $0.315075 | Bash=25, Edit=5, Read=8, Write=3 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | A | PF | yes | 257.3s | 2,619 | 59,206 | 2,738,850 | 56,587 | 16,094 | $0.470148 | Bash=50, Edit=6, Read=9, bash=1 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | A | base | yes | 202.9s | 2,531 | 55,642 | 2,122,671 | 53,111 | 16,686 | $0.404450 | Bash=41, Edit=4, Read=10 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | T | AS | yes | 207.7s | 8,286 | 58,906 | 2,090,483 | 50,620 | 17,724 | $0.407194 | Bash=31, Edit=6, Grep=1, Read=17, Write=3 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | T | PF | yes | 186.0s | 8,262 | 51,439 | 1,745,776 | 43,177 | 14,409 | $0.341239 | Bash=37, Edit=4, Read=14 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | T | base | yes | 201.3s | 8,278 | 67,598 | 2,535,533 | 59,320 | 16,743 | $0.464186 | Bash=32, Edit=8, Read=17 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | T2 | AS | yes | 199.0s | 7,718 | 57,559 | 1,773,017 | 49,841 | 16,286 | $0.366132 | Bash=34, Edit=7, Read=12 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | T2 | PF | yes | 220.2s | 7,782 | 68,133 | 2,807,638 | 60,351 | 16,206 | $0.490278 | Bash=39, Edit=7, Read=14, Write=1 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | haiku | T2 | base | yes | 157.8s | 7,654 | 86,986 | 2,573,568 | 79,332 | 12,695 | $0.487150 | Bash=27, Edit=4, Grep=2, Read=12 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | A | AS | yes | 190.7s | 46,981 | 46,981 | 482,304 | 0 | 5,911 | $0.130677 | commandExecution=18, fileChange=4 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | A | PF | yes | 175.3s | 58,259 | 58,259 | 410,112 | 0 | 7,561 | $0.144636 | commandExecution=15, fileChange=2 | -0.0001 | +0.0003 |
| 10 | django__django-11551 | luna | A | base | yes | 104.8s | 67,044 | 67,044 | 358,400 | 0 | 4,010 | $0.156944 | commandExecution=11, fileChange=2, webSearch=3 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | T | AS | yes | 152.6s | 61,844 | 61,844 | 382,720 | 0 | 5,909 | $0.135570 | commandExecution=13, fileChange=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | T | PF | yes | 169.6s | 42,228 | 42,228 | 499,456 | 0 | 7,045 | $0.134444 | commandExecution=15, fileChange=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | T | base | yes | 145.1s | 40,556 | 40,556 | 380,416 | 0 | 4,934 | $0.108202 | commandExecution=13, fileChange=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | T2 | AS | yes | 160.4s | 67,276 | 67,276 | 697,600 | 0 | 6,089 | $0.173570 | commandExecution=21, fileChange=3 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | T2 | PF | yes | 147.4s | 34,039 | 34,039 | 337,664 | 0 | 6,466 | $0.106601 | commandExecution=12, fileChange=1 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | luna | T2 | base | yes | 130.4s | 49,199 | 49,199 | 605,184 | 0 | 5,706 | $0.143953 | commandExecution=15, fileChange=1 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | A | AS | yes | 117.1s | 2,118 | 21,758 | 313,099 | 19,640 | 6,799 | $0.315745 | Bash=7, Edit=3, Grep=3, Read=4 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | A | PF | yes | 228.7s | 2,128 | 25,253 | 425,980 | 23,125 | 6,593 | $0.367459 | Bash=9, Edit=4, Grep=5, Read=4 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | A | base | yes | 101.1s | 2,126 | 20,630 | 383,055 | 18,504 | 5,746 | $0.314144 | Bash=15, Edit=3, Grep=3 | +0.0000 | -0.0001 |
| 10 | django__django-11551 | sonnet | T | AS | yes | 74.8s | 7,839 | 30,997 | 282,153 | 23,158 | 4,586 | $0.300065 | Bash=9, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | T | PF | yes | 186.4s | 7,833 | 24,087 | 184,780 | 16,254 | 2,676 | $0.200771 | Bash=6, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | T | base | yes | 33.8s | 7,829 | 24,968 | 143,267 | 17,139 | 2,127 | $0.185380 | Bash=4, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | T2 | AS | yes | 256.7s | 7,343 | 33,523 | 691,703 | 26,180 | 6,856 | $0.474690 | Bash=17, Edit=4, Grep=3, Read=4 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | T2 | PF | yes | 129.9s | 7,339 | 41,454 | 787,645 | 34,115 | 9,906 | $0.596840 | Bash=14, Edit=5, Read=7 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | sonnet | T2 | base | yes | 157.3s | 7,333 | 38,919 | 628,039 | 31,586 | 9,051 | $0.520922 | Bash=11, Edit=7, Read=5 | +0.0000 | -0.0001 |
| 10 | django__django-11551 | spark | A | AS | yes | 96.6s | 42,914 | 42,914 | 925,056 | 0 | 10,797 | n/a | commandExecution=40, fileChange=4 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | spark | A | PF | yes | 96.8s | 32,174 | 32,174 | 747,648 | 0 | 11,848 | n/a | commandExecution=32, fileChange=4, webSearch=1 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | spark | A | base | yes | 70.6s | 45,643 | 45,643 | 1,006,848 | 0 | 11,933 | n/a | commandExecution=32, fileChange=3 | -0.0000 | +0.0001 |
| 10 | django__django-11551 | spark | T | AS | yes | 85.5s | 32,249 | 32,249 | 732,800 | 0 | 13,049 | n/a | commandExecution=30, fileChange=2 | -0.0000 | +0.0001 |
| 10 | django__django-11551 | spark | T | PF | yes | 58.9s | 31,288 | 31,288 | 504,704 | 0 | 13,021 | n/a | commandExecution=21, fileChange=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | spark | T | base | yes | 77.7s | 61,000 | 61,000 | 1,042,176 | 0 | 13,035 | n/a | commandExecution=36, fileChange=3, webSearch=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | spark | T2 | AS | yes | 69.0s | 45,976 | 45,976 | 560,128 | 0 | 9,207 | n/a | commandExecution=24, fileChange=2 | -0.0000 | +0.0001 |
| 10 | django__django-11551 | spark | T2 | PF | yes | 72.3s | 27,743 | 27,743 | 547,456 | 0 | 9,073 | n/a | commandExecution=24, fileChange=2 | +0.0000 | +0.0000 |
| 10 | django__django-11551 | spark | T2 | base | yes | 102.9s | 45,884 | 45,884 | 1,386,496 | 0 | 16,475 | n/a | commandExecution=40, fileChange=6 | +0.0001 | -0.0004 |
| 11 | django__django-13658 | haiku | A | AS | yes | 170.6s | 1,256 | 44,328 | 1,256,120 | 43,072 | 11,880 | $0.272412 | Bash=27, Edit=5, Read=9, Write=1 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | haiku | A | PF | yes | 262.8s | 1,352 | 46,672 | 1,551,658 | 45,320 | 17,165 | $0.332983 | Bash=39, Edit=1, Glob=1, Read=7, Write=6 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | haiku | A | base | yes | 242.6s | 1,232 | 41,375 | 1,101,390 | 40,143 | 11,980 | $0.251557 | Bash=31, Edit=1, Grep=1, Read=6 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | haiku | T | AS | yes | 178.4s | 11,409 | 67,018 | 1,779,848 | 55,609 | 12,641 | $0.363817 | Bash=33, Edit=1, Read=8 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | haiku | T | PF | yes | 286.5s | 11,481 | 67,820 | 2,213,052 | 56,339 | 16,855 | $0.429739 | Bash=40, Edit=1, Read=8, Write=2 | +0.0014 | +0.0016 |
| 11 | django__django-13658 | haiku | T | base | yes | 165.4s | 11,305 | 56,786 | 990,223 | 45,481 | 9,098 | $0.246779 | Bash=23, Edit=1, Read=5 | +0.0000 | +0.0000 |
| 11 | django__django-13658 | haiku | T2 | AS | yes | 246.3s | 8,078 | 62,488 | 1,928,814 | 54,410 | 16,058 | $0.390069 | Bash=35, Edit=1, Read=10 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | haiku | T2 | PF | yes | 234.6s | 8,046 | 49,958 | 1,423,373 | 41,912 | 13,131 | $0.299862 | Bash=35, Edit=1, Read=6 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | haiku | T2 | base | no | 217.6s | 8,214 | 73,717 | 3,115,223 | 65,503 | 17,986 | $0.540672 | Bash=52, Edit=1, Read=10 | +0.0000 | +0.0000 |
| 11 | django__django-13658 | luna | A | AS | yes | 154.4s | 51,459 | 51,459 | 405,248 | 0 | 4,937 | $0.121606 | commandExecution=15, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | A | PF | yes | 216.4s | 92,363 | 92,363 | 680,704 | 0 | 7,358 | $0.204581 | commandExecution=19, fileChange=3 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | A | base | yes | 161.2s | 36,453 | 36,453 | 550,144 | 0 | 4,419 | $0.117981 | commandExecution=14, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | T | AS | yes | 158.5s | 48,234 | 48,234 | 574,976 | 0 | 4,119 | $0.130446 | commandExecution=11, fileChange=1 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | T | PF | yes | 190.6s | 91,961 | 91,961 | 718,848 | 0 | 5,670 | $0.197866 | commandExecution=11, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | T | base | yes | 194.1s | 41,447 | 41,447 | 599,552 | 0 | 3,970 | $0.125222 | commandExecution=10, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | T2 | AS | yes | 186.9s | 59,662 | 59,662 | 606,720 | 0 | 4,936 | $0.149950 | commandExecution=10, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | T2 | PF | yes | 184.4s | 61,153 | 61,153 | 707,328 | 0 | 5,338 | $0.163914 | commandExecution=11, fileChange=1 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | luna | T2 | base | yes | 136.6s | 56,417 | 56,417 | 443,136 | 0 | 3,699 | $0.122925 | commandExecution=9, fileChange=1 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | A | AS | yes | 538.7s | 959 | 18,801 | 407,464 | 17,842 | 6,697 | $0.330611 | Bash=17, Edit=1, Grep=3, Read=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | A | PF | yes | 323.9s | 991 | 33,217 | 895,120 | 32,226 | 13,255 | $0.661668 | Bash=18, Edit=6, Grag=1, Grep=6, Read=8 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | A | base | yes | 89.6s | 935 | 6,291 | 119,041 | 5,356 | 1,797 | $0.095596 | Bash=6, Edit=1, Grep=4 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | T | AS | yes | 138.1s | 11,100 | 40,095 | 498,561 | 28,995 | 3,740 | $0.390620 | Bash=14, Edit=1, Read=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | T | PF | yes | 587.4s | 11,132 | 51,445 | 1,158,128 | 40,313 | 11,967 | $0.779909 | Bash=18, Edit=3, Grep=5, Read=7 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | T | base | yes | 228.9s | 11,088 | 30,963 | 268,968 | 19,875 | 2,777 | $0.252531 | Bash=7, Edit=1, Read=3 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | T2 | AS | yes | 294.6s | 7,763 | 38,659 | 844,968 | 30,896 | 7,259 | $0.555428 | Bash=14, Edit=5, Grep=6, Read=5 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | T2 | PF | yes | 682.8s | 7,763 | 53,736 | 1,022,397 | 45,973 | 11,237 | $0.758809 | Bash=20, Edit=3, Grep=3, Read=4 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | sonnet | T2 | base | yes | 93.7s | 7,719 | 22,785 | 159,965 | 15,066 | 2,024 | $0.176311 | Bash=5, Edit=1, Read=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | A | AS | yes | 93.0s | 58,365 | 58,365 | 1,174,784 | 0 | 11,929 | n/a | commandExecution=43, fileChange=4, webSearch=1 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | A | PF | yes | 90.6s | 47,101 | 47,101 | 1,174,912 | 0 | 12,046 | n/a | commandExecution=41, fileChange=4 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | A | base | yes | 94.2s | 54,902 | 54,902 | 982,528 | 0 | 12,888 | n/a | commandExecution=38, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | T | AS | yes | 92.3s | 42,146 | 42,146 | 851,712 | 0 | 11,752 | n/a | commandExecution=29, fileChange=3, webSearch=1 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | T | PF | yes | 86.8s | 63,440 | 63,440 | 1,201,280 | 0 | 9,866 | n/a | commandExecution=32, fileChange=4 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | T | base | yes | 67.2s | 38,238 | 38,238 | 817,408 | 0 | 9,884 | n/a | commandExecution=27, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | T2 | AS | yes | 73.2s | 41,864 | 41,864 | 831,488 | 0 | 12,082 | n/a | commandExecution=30, fileChange=3 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | T2 | PF | yes | 66.6s | 43,843 | 43,843 | 912,384 | 0 | 13,089 | n/a | commandExecution=28, fileChange=2 | +0.0017 | +0.0020 |
| 11 | django__django-13658 | spark | T2 | base | yes | 85.5s | 51,490 | 51,490 | 1,573,632 | 0 | 15,070 | n/a | commandExecution=35, fileChange=6 | +0.0017 | +0.0020 |
| 12 | psf__requests-1724 | haiku | A | AS | no | 221.1s | 1,847 | 60,139 | 2,465,854 | 58,292 | 20,217 | $0.466101 | Bash=41, Edit=3, Read=10, Write=4 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | haiku | A | PF | no | 272.6s | 1,967 | 59,666 | 2,962,097 | 57,699 | 22,685 | $0.527000 | Bash=48, Edit=2, Grep=1, Read=17, Write=5 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | haiku | A | base | yes | 361.3s | 1,967 | 66,386 | 2,956,557 | 64,419 | 16,380 | $0.508361 | Bash=40, Edit=11, Read=21, Write=1 | -0.0141 | +0.0000 |
| 12 | psf__requests-1724 | haiku | T | AS | no | 211.4s | 10,235 | 60,695 | 2,149,390 | 50,460 | 17,404 | $0.413114 | Bash=38, Edit=3, Grep=1, Read=14, Write=1 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | haiku | T | PF | no | 244.9s | 10,347 | 74,279 | 3,316,701 | 63,932 | 18,306 | $0.561411 | Bash=39, Edit=13, Grep=1, Read=18 | -0.0141 | +0.0000 |
| 12 | psf__requests-1724 | haiku | T | base | yes | 185.3s | 10,283 | 56,768 | 2,223,435 | 46,485 | 18,918 | $0.420187 | Bash=37, Edit=11, Read=10, Write=5 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | haiku | T2 | AS | no | 198.3s | 8,705 | 51,253 | 1,668,694 | 42,548 | 16,586 | $0.343600 | Bash=32, Edit=4, Read=17 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | haiku | T2 | PF | no | 257.0s | 8,785 | 76,041 | 2,858,715 | 67,256 | 24,440 | $0.551369 | Bash=34, Edit=12, Read=17 | -0.0135 | -0.0014 |
| 12 | psf__requests-1724 | haiku | T2 | base | yes | 198.7s | 8,793 | 74,569 | 3,115,804 | 65,776 | 15,259 | $0.528220 | Bash=36, Edit=12, Read=16 | -0.0141 | +0.0000 |
| 12 | psf__requests-1724 | luna | A | AS | no | 253.0s | 72,155 | 72,155 | 764,416 | 0 | 7,173 | $0.191635 | commandExecution=25, fileChange=2 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | luna | A | PF | no | 207.8s | 73,052 | 73,052 | 1,270,528 | 0 | 8,443 | $0.290763 | commandExecution=23, fileChange=1, webSearch=4 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | luna | A | base | no | 154.9s | 60,696 | 60,696 | 817,920 | 0 | 6,527 | $0.221650 | commandExecution=18, fileChange=1, webSearch=4 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | luna | T | AS | no | 266.9s | 100,972 | 100,972 | 2,185,728 | 0 | 9,024 | $0.443689 | commandExecution=26, fileChange=3, webSearch=7 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | luna | T | PF | no | 198.7s | 108,824 | 108,824 | 1,522,688 | 0 | 8,625 | $0.362843 | commandExecution=17, fileChange=1, webSearch=5 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | luna | T | base | yes | 145.7s | 93,602 | 93,602 | 1,289,984 | 0 | 5,700 | $0.346800 | commandExecution=10, fileChange=1, webSearch=9 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | luna | T2 | AS | no | 127.4s | 71,717 | 71,717 | 637,696 | 0 | 5,691 | $0.209633 | commandExecution=11, fileChange=2, webSearch=4 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | luna | T2 | PF | no | 267.2s | 110,124 | 110,124 | 1,630,464 | 0 | 7,887 | $0.380492 | commandExecution=21, fileChange=2, webSearch=6 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | luna | T2 | base | no | 183.6s | 87,782 | 87,782 | 897,280 | 0 | 7,813 | $0.284388 | commandExecution=15, fileChange=2, webSearch=6 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | sonnet | A | AS | no | 197.6s | 1,446 | 26,725 | 704,851 | 25,279 | 8,692 | $0.494909 | Bash=23, Edit=6, Grep=3, Read=5 | +0.0002 | -0.0003 |
| 12 | psf__requests-1724 | sonnet | A | PF | no | 228.1s | 1,434 | 32,288 | 743,716 | 30,854 | 8,807 | $0.541648 | Bash=20, Edit=3, Grep=2, Read=4 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | sonnet | A | base | yes | 275.1s | 1,450 | 30,389 | 810,664 | 28,939 | 9,463 | $0.560140 | Bash=23, Edit=3, Grep=6, Read=5 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | sonnet | T | AS | no | 651.8s | 9,860 | 55,066 | 1,505,497 | 45,206 | 16,901 | $0.986190 | Bash=33, Edit=2, Grep=5, Read=4 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | sonnet | T | PF | no | 128.6s | 9,828 | 41,723 | 760,029 | 31,895 | 6,737 | $0.530128 | Bash=19, Edit=3, Grep=2, Read=4 | +0.0005 | -0.0043 |
| 12 | psf__requests-1724 | sonnet | T | base | yes | 152.1s | 9,824 | 44,082 | 802,110 | 34,258 | 7,758 | $0.572233 | Bash=19, Edit=2, Grep=5, Read=3 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | sonnet | T2 | AS | no | 279.1s | 8,332 | 40,525 | 722,877 | 32,193 | 6,680 | $0.518423 | Bash=20, Edit=3, Grep=2, Read=4 | +0.0002 | -0.0003 |
| 12 | psf__requests-1724 | sonnet | T2 | PF | no | 252.8s | 8,372 | 48,054 | 1,544,640 | 39,682 | 10,804 | $0.871926 | Bash=39, Edit=5, Grep=1, Read=3, Write=1 | +0.0007 | +0.0008 |
| 12 | psf__requests-1724 | sonnet | T2 | base | no | 488.1s | 8,408 | 54,750 | 2,193,647 | 46,342 | 14,113 | $1.156271 | Bash=56, Edit=4, Grep=4, Read=4 | +0.0002 | -0.0003 |
| 12 | psf__requests-1724 | spark | A | AS | no | 114.9s | 55,095 | 55,095 | 1,208,192 | 0 | 17,729 | n/a | commandExecution=38, fileChange=5 | -0.0002 | +0.0002 |
| 12 | psf__requests-1724 | spark | A | PF | no | 67.3s | 36,733 | 36,733 | 978,944 | 0 | 14,233 | n/a | commandExecution=39, fileChange=2 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | spark | A | base | yes | 88.6s | 60,076 | 60,076 | 971,136 | 0 | 13,983 | n/a | commandExecution=38, fileChange=5 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | spark | T | AS | no | 63.5s | 43,846 | 43,846 | 979,200 | 0 | 14,020 | n/a | commandExecution=30, fileChange=3 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | spark | T | PF | no | 106.7s | 55,205 | 55,205 | 1,463,936 | 0 | 16,227 | n/a | commandExecution=39, fileChange=4 | +0.0005 | -0.0024 |
| 12 | psf__requests-1724 | spark | T | base | yes | 93.4s | 49,872 | 49,872 | 1,365,376 | 0 | 13,621 | n/a | commandExecution=39, fileChange=3 | -0.0002 | +0.0003 |
| 12 | psf__requests-1724 | spark | T2 | AS | no | 173.9s | 93,449 | 93,449 | 2,888,192 | 0 | 20,235 | n/a | commandExecution=65, fileChange=3 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | spark | T2 | PF | no | 122.2s | 52,260 | 52,260 | 1,164,800 | 0 | 17,375 | n/a | commandExecution=35, fileChange=4 | +0.0000 | +0.0000 |
| 12 | psf__requests-1724 | spark | T2 | base | yes | 66.6s | 44,232 | 44,232 | 1,150,464 | 0 | 14,895 | n/a | commandExecution=33, fileChange=5 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | A | AS | no | 275.2s | 1,386 | 42,747 | 1,894,519 | 41,361 | 20,938 | $0.378250 | Bash=59, Edit=5, Read=10 | +0.0001 | -0.0001 |
| 13 | sympy__sympy-18763 | haiku | A | PF | yes | 207.4s | 1,258 | 35,807 | 1,398,539 | 34,549 | 17,073 | $0.295575 | Bash=45, Edit=3, Read=10 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | A | base | no | 195.3s | 1,218 | 42,291 | 1,255,426 | 41,073 | 14,291 | $0.280362 | Bash=43, Edit=2, Read=8 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | T | AS | no | 324.5s | 8,325 | 66,806 | 2,269,001 | 58,481 | 23,351 | $0.468942 | Bash=38, Edit=3, Read=10 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | T | PF | no | 347.4s | 8,581 | 81,839 | 3,910,164 | 73,258 | 28,318 | $0.687703 | Bash=57, Edit=7, Read=20 | -0.0174 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | T | base | no | 158.2s | 8,213 | 73,284 | 1,846,161 | 65,071 | 12,064 | $0.383291 | Bash=28, Edit=2, Read=7 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | T2 | AS | no | 279.0s | 5,796 | 61,877 | 2,524,370 | 56,081 | 25,409 | $0.497440 | Bash=42, Edit=8, Grep=2, Read=15, Write=1 | -0.0032 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | T2 | PF | no | 303.7s | 5,932 | 72,400 | 3,330,468 | 66,468 | 27,181 | $0.607820 | Bash=51, Edit=12, Read=20, Write=1 | -0.0253 | +0.0000 |
| 13 | sympy__sympy-18763 | haiku | T2 | base | no | 187.4s | 5,620 | 51,136 | 1,263,284 | 45,516 | 15,370 | $0.299830 | Bash=33, Edit=3, Grep=1, Read=10 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | A | AS | yes | 153.1s | 115,314 | 115,314 | 1,323,008 | 0 | 5,270 | $0.369235 | commandExecution=17, fileChange=2, webSearch=9 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | A | PF | yes | 165.9s | 60,199 | 60,199 | 607,488 | 0 | 6,276 | $0.168604 | commandExecution=19, fileChange=2, webSearch=1 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | A | base | no | 88.7s | 41,630 | 41,630 | 252,672 | 0 | 3,391 | $0.087243 | commandExecution=10, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | T | AS | no | 128.7s | 79,889 | 79,889 | 550,400 | 0 | 5,137 | $0.165751 | commandExecution=19, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | T | PF | no | 109.2s | 55,318 | 55,318 | 464,128 | 0 | 4,085 | $0.126241 | commandExecution=14, fileChange=1 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | T | base | yes | 87.6s | 31,659 | 31,659 | 276,992 | 0 | 3,025 | $0.087508 | commandExecution=9, fileChange=1, webSearch=1 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | T2 | AS | no | 92.9s | 45,194 | 45,194 | 152,832 | 0 | 3,476 | $0.081333 | commandExecution=11, fileChange=1 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | T2 | PF | no | 190.3s | 70,455 | 70,455 | 1,042,688 | 0 | 7,367 | $0.268926 | commandExecution=22, fileChange=2, webSearch=5 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | luna | T2 | base | no | 87.7s | 52,821 | 52,821 | 305,664 | 0 | 2,975 | $0.101237 | commandExecution=9, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | A | AS | no | 229.5s | 833 | 17,999 | 370,490 | 17,166 | 5,714 | $0.300572 | Bash=18, Edit=2, Grep=1, Read=2 | +0.0001 | -0.0006 |
| 13 | sympy__sympy-18763 | sonnet | A | PF | no | 332.8s | 879 | 37,697 | 1,078,957 | 36,818 | 14,655 | $0.765277 | Bash=32, Edit=2, Grep=6, Read=6 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | A | base | no | 241.4s | 829 | 13,625 | 297,545 | 12,796 | 5,642 | $0.251346 | Bash=10, Edit=4, Grep=4, Read=3 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | T | AS | no | 113.6s | 7,960 | 33,037 | 620,895 | 25,077 | 6,195 | $0.437539 | Bash=14, Edit=3, Grep=5, Read=3 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | T | PF | no | 146.0s | 7,958 | 33,010 | 572,042 | 25,052 | 5,471 | $0.411848 | Bash=19, Edit=2, Read=3 | +0.0001 | -0.0006 |
| 13 | sympy__sympy-18763 | sonnet | T | base | no | 98.0s | 7,954 | 32,430 | 554,048 | 24,476 | 6,034 | $0.411436 | Bash=12, Edit=4, Grep=1, Read=5 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | T2 | AS | no | 110.4s | 5,315 | 32,399 | 648,721 | 27,084 | 6,194 | $0.455277 | Bash=20, Edit=2, Read=5 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | T2 | PF | no | 229.5s | 5,311 | 25,430 | 521,234 | 20,119 | 5,318 | $0.362089 | Bash=20, Edit=2, Read=3 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | sonnet | T2 | base | no | 70.8s | 5,293 | 19,359 | 285,031 | 14,066 | 3,546 | $0.228276 | Bash=11, Edit=2, Read=3 | +0.0001 | -0.0006 |
| 13 | sympy__sympy-18763 | spark | A | AS | no | 141.8s | 94,510 | 94,510 | 3,036,672 | 0 | 21,376 | n/a | commandExecution=69, fileChange=3, webSearch=4 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | A | PF | no | 126.6s | 64,318 | 64,318 | 1,991,424 | 0 | 20,836 | n/a | commandExecution=62, fileChange=3, webSearch=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | A | base | no | 69.9s | 39,644 | 39,644 | 793,088 | 0 | 13,863 | n/a | commandExecution=32, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | T | AS | no | 84.1s | 55,008 | 55,008 | 1,112,192 | 0 | 11,942 | n/a | commandExecution=39, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | T | PF | no | 109.3s | 50,803 | 50,803 | 1,829,760 | 0 | 16,256 | n/a | commandExecution=52, fileChange=3 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | T | base | no | 59.1s | 43,945 | 43,945 | 582,912 | 0 | 6,083 | n/a | commandExecution=28, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | T2 | AS | no | 242.7s | 66,940 | 66,940 | 2,315,008 | 0 | 17,753 | n/a | commandExecution=65, fileChange=4, webSearch=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | T2 | PF | no | 123.0s | 44,021 | 44,021 | 1,308,800 | 0 | 11,657 | n/a | commandExecution=44, fileChange=2 | +0.0000 | +0.0000 |
| 13 | sympy__sympy-18763 | spark | T2 | base | no | 49.2s | 28,517 | 28,517 | 521,088 | 0 | 4,818 | n/a | commandExecution=26, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | A | AS | yes | 171.3s | 1,042 | 47,950 | 1,474,055 | 46,908 | 13,453 | $0.309528 | Bash=31, Edit=3, Grep=2, Read=9 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | A | PF | yes | 167.3s | 1,146 | 45,034 | 1,732,333 | 43,888 | 11,646 | $0.320385 | Bash=48, Edit=2, Grep=1, Read=7 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | A | base | yes | 138.7s | 1,010 | 36,831 | 1,005,157 | 35,821 | 12,927 | $0.237803 | Bash=35, Edit=1, Grep=1, Read=4 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | T | AS | yes | 130.8s | 2,074 | 25,523 | 700,675 | 23,449 | 8,797 | $0.163025 | Bash=30, Edit=1, Read=10 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | T | PF | yes | 142.0s | 2,066 | 40,153 | 1,183,391 | 38,087 | 8,802 | $0.240589 | Bash=30, Edit=1, Read=9 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | T | base | yes | 110.4s | 2,002 | 34,399 | 825,753 | 32,397 | 9,824 | $0.198491 | Bash=28, Edit=1, Read=3 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | T2 | AS | yes | 114.0s | 1,573 | 34,063 | 637,332 | 32,490 | 8,442 | $0.172496 | Bash=22, Edit=1, Read=6 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | T2 | PF | yes | 126.6s | 1,629 | 40,057 | 974,487 | 38,428 | 8,249 | $0.217179 | Bash=26, Edit=1, Read=9 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | haiku | T2 | base | yes | 131.5s | 1,677 | 37,223 | 1,044,045 | 35,546 | 10,306 | $0.228703 | Bash=31, Edit=1, Glob=2, Grep=2, Read=6 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | A | AS | yes | 75.7s | 59,697 | 59,697 | 206,336 | 0 | 2,558 | $0.095679 | commandExecution=11, fileChange=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | A | PF | yes | 114.5s | 50,922 | 50,922 | 344,320 | 0 | 4,447 | $0.112036 | commandExecution=12, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | A | base | yes | 69.4s | 35,676 | 35,676 | 237,312 | 0 | 2,992 | $0.077359 | commandExecution=10, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | T | AS | yes | 74.5s | 41,679 | 41,679 | 221,696 | 0 | 2,534 | $0.079053 | commandExecution=10, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | T | PF | yes | 72.1s | 33,571 | 33,571 | 118,272 | 0 | 2,772 | $0.062030 | commandExecution=7, fileChange=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | T | base | yes | 77.5s | 47,557 | 47,557 | 175,360 | 0 | 2,994 | $0.083057 | commandExecution=10, fileChange=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | T2 | AS | yes | 100.8s | 34,981 | 34,981 | 231,680 | 0 | 2,601 | $0.073755 | commandExecution=11, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | T2 | PF | yes | 89.3s | 31,862 | 31,862 | 175,360 | 0 | 3,719 | $0.071712 | commandExecution=10, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | luna | T2 | base | yes | 84.2s | 24,456 | 24,456 | 223,488 | 0 | 3,354 | $0.066929 | commandExecution=12, fileChange=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | A | AS | yes | 193.3s | 723 | 21,671 | 425,691 | 20,948 | 7,843 | $0.371693 | Bash=17, Edit=5, Grep=2, Read=2, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | A | PF | yes | 72.0s | 711 | 14,890 | 262,415 | 14,179 | 4,342 | $0.229565 | Bash=10, Edit=2, Grep=4, Read=1, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | A | base | yes | 170.2s | 707 | 8,678 | 194,010 | 7,971 | 2,844 | $0.149304 | Bash=8, Edit=2, Grep=3, Read=2, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | T | AS | yes | 96.3s | 1,773 | 13,999 | 257,568 | 12,226 | 4,199 | $0.215306 | Bash=9, Edit=2, Grep=2, Read=3, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | T | PF | yes | 68.7s | 1,773 | 15,178 | 263,153 | 13,405 | 5,123 | $0.237906 | Bash=6, Edit=3, Grep=5, Read=2, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | T | base | yes | 90.2s | 1,783 | 19,676 | 376,139 | 17,893 | 6,667 | $0.321930 | Bash=13, Edit=3, Grep=3, Read=2, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | T2 | AS | yes | 47.3s | 1,360 | 9,356 | 167,310 | 7,996 | 3,113 | $0.146130 | Bash=5, Edit=3, Grep=3, Read=1, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | T2 | PF | yes | 46.7s | 1,356 | 8,783 | 146,667 | 7,427 | 3,006 | $0.134906 | Bash=6, Edit=2, Grep=1, Read=2, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | sonnet | T2 | base | yes | 44.7s | 1,354 | 8,612 | 127,839 | 7,258 | 3,068 | $0.129168 | Bash=4, Edit=2, Grep=3, Read=2, Write=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | A | AS | yes | 76.2s | 62,986 | 62,986 | 610,176 | 0 | 9,309 | n/a | commandExecution=33, fileChange=3 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | A | PF | yes | 78.1s | 48,152 | 48,152 | 657,024 | 0 | 10,045 | n/a | commandExecution=32, fileChange=4, webSearch=1 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | A | base | yes | 60.7s | 39,046 | 39,046 | 476,288 | 0 | 7,676 | n/a | commandExecution=28, fileChange=4 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | T | AS | yes | 47.4s | 21,408 | 21,408 | 447,104 | 0 | 4,449 | n/a | commandExecution=24, fileChange=4 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | T | PF | yes | 38.0s | 22,786 | 22,786 | 322,432 | 0 | 5,462 | n/a | commandExecution=16, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | T | base | yes | 53.8s | 44,777 | 44,777 | 737,792 | 0 | 6,583 | n/a | commandExecution=26, fileChange=4 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | T2 | AS | yes | 58.1s | 24,376 | 24,376 | 302,208 | 0 | 4,848 | n/a | commandExecution=15, fileChange=2 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | T2 | PF | yes | 42.2s | 34,199 | 34,199 | 305,792 | 0 | 4,098 | n/a | commandExecution=17, fileChange=3 | +0.0000 | +0.0000 |
| 14 | pytest-dev__pytest-7982 | spark | T2 | base | yes | 34.8s | 19,268 | 19,268 | 301,440 | 0 | 4,987 | n/a | commandExecution=20, fileChange=2 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | A | AS | yes | 171.5s | 1,136 | 28,667 | 1,079,835 | 27,531 | 13,728 | $0.232822 | Bash=33, Edit=1, Grep=4, Read=3, Write=3 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | A | PF | yes | 181.3s | 1,128 | 46,523 | 1,310,541 | 45,395 | 17,338 | $0.309662 | Bash=35, Edit=3, Read=5 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | A | base | yes | 159.0s | 1,120 | 51,777 | 1,617,752 | 50,657 | 15,754 | $0.342979 | Bash=32, Edit=4, Read=6 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | T | AS | yes | 226.0s | 9,283 | 65,327 | 1,762,778 | 56,044 | 21,959 | $0.407444 | Bash=33, Edit=3, Read=5 | -0.0013 | +0.0063 |
| 15 | pytest-dev__pytest-7432 | haiku | T | PF | yes | 221.4s | 9,331 | 61,732 | 1,808,195 | 52,401 | 17,763 | $0.383767 | Bash=37, Edit=3, Read=7 | -0.0007 | -0.0029 |
| 15 | pytest-dev__pytest-7432 | haiku | T | base | yes | 182.5s | 9,331 | 58,759 | 1,881,573 | 49,428 | 18,293 | $0.387809 | Bash=36, Edit=2, Read=9 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | T2 | AS | yes | 148.6s | 7,145 | 51,855 | 1,292,333 | 44,710 | 12,165 | $0.286623 | Bash=30, Edit=1, Grep=1, Read=6 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | T2 | PF | yes | 183.8s | 7,225 | 57,872 | 1,776,036 | 50,647 | 15,849 | $0.365368 | Bash=34, Edit=4, Grep=1, Read=9 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | haiku | T2 | base | yes | 137.8s | 7,145 | 51,548 | 1,266,449 | 44,403 | 13,544 | $0.290316 | Bash=30, Edit=3, Read=5 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | luna | A | AS | yes | 101.7s | 35,001 | 35,001 | 340,992 | 0 | 4,107 | $0.093742 | commandExecution=10, fileChange=3 | +0.0000 | +0.0123 |
| 15 | pytest-dev__pytest-7432 | luna | A | PF | yes | 125.4s | 57,887 | 57,887 | 328,960 | 0 | 5,063 | $0.121161 | commandExecution=12, fileChange=4 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | luna | A | base | yes | 136.8s | 33,900 | 33,900 | 453,120 | 0 | 5,384 | $0.111516 | commandExecution=13, fileChange=4 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | luna | T | AS | yes | 166.9s | 77,632 | 77,632 | 610,560 | 0 | 6,676 | $0.178744 | commandExecution=14, fileChange=4 | +0.0039 | +0.0288 |
| 15 | pytest-dev__pytest-7432 | luna | T | PF | yes | 186.6s | 53,840 | 53,840 | 785,920 | 0 | 7,976 | $0.180288 | commandExecution=19, fileChange=4 | +0.0000 | +0.0123 |
| 15 | pytest-dev__pytest-7432 | luna | T | base | yes | 194.6s | 58,620 | 58,620 | 765,184 | 0 | 8,655 | $0.187068 | commandExecution=20, fileChange=2 | +0.0000 | +0.0123 |
| 15 | pytest-dev__pytest-7432 | luna | T2 | AS | yes | 127.3s | 32,434 | 32,434 | 334,080 | 0 | 5,561 | $0.099208 | commandExecution=8, fileChange=3 | +0.0013 | +0.0180 |
| 15 | pytest-dev__pytest-7432 | luna | T2 | PF | yes | 130.8s | 77,214 | 77,214 | 488,192 | 0 | 4,759 | $0.154587 | commandExecution=13, fileChange=3 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | luna | T2 | base | yes | 126.4s | 36,951 | 36,951 | 385,792 | 0 | 5,035 | $0.105740 | commandExecution=9, fileChange=3 | +0.0013 | +0.0180 |
| 15 | pytest-dev__pytest-7432 | sonnet | A | AS | yes | 262.0s | 823 | 18,122 | 436,992 | 17,299 | 8,218 | $0.358901 | Bash=14, Edit=5, Read=3, Write=1 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | A | PF | yes | 120.3s | 819 | 21,973 | 458,215 | 21,154 | 8,925 | $0.398980 | Bash=12, Edit=3, Grep=1, Read=4, Write=1 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | A | base | yes | 48.4s | 795 | 10,443 | 130,544 | 9,648 | 3,522 | $0.150536 | Bash=5, Edit=1, Read=2, Write=1 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | T | AS | yes | 226.0s | 9,032 | 60,797 | 1,721,823 | 51,765 | 14,093 | $1.047546 | Bash=26, Edit=7, Read=8, Write=1 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | T | PF | yes | 136.9s | 8,998 | 43,065 | 793,488 | 34,067 | 9,459 | $0.593245 | Bash=15, Edit=4, Read=5, Write=1 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | T | base | yes | 228.3s | 8,992 | 37,191 | 618,202 | 28,199 | 6,797 | $0.465514 | Bash=13, Edit=4, Grep=1, Read=3, Write=1 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | T2 | AS | yes | 205.7s | 6,862 | 27,847 | 336,450 | 20,985 | 5,032 | $0.309057 | Bash=11, Edit=1, Read=2 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | T2 | PF | yes | 218.7s | 6,862 | 25,500 | 304,155 | 18,638 | 5,360 | $0.290207 | Bash=9, Edit=1, Read=4 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | sonnet | T2 | base | yes | 162.8s | 6,866 | 28,216 | 388,088 | 21,350 | 5,001 | $0.326295 | Bash=11, Edit=1, Read=2, Write=2 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | spark | A | AS | yes | 87.8s | 40,599 | 40,599 | 594,048 | 0 | 8,983 | n/a | commandExecution=24, fileChange=7 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | spark | A | PF | yes | 60.2s | 29,042 | 29,042 | 494,208 | 0 | 8,390 | n/a | commandExecution=20, fileChange=4 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | spark | A | base | yes | 49.6s | 36,659 | 36,659 | 346,624 | 0 | 8,346 | n/a | commandExecution=21, fileChange=3 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | spark | T | AS | yes | 132.1s | 40,609 | 40,609 | 1,000,064 | 0 | 12,037 | n/a | commandExecution=28, fileChange=4 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | spark | T | PF | yes | 59.3s | 45,863 | 45,863 | 730,496 | 0 | 10,142 | n/a | commandExecution=21, fileChange=4 | +0.0039 | +0.0288 |
| 15 | pytest-dev__pytest-7432 | spark | T | base | yes | 105.7s | 74,572 | 74,572 | 2,661,248 | 0 | 27,017 | n/a | commandExecution=49, fileChange=8 | +0.0000 | +0.0000 |
| 15 | pytest-dev__pytest-7432 | spark | T2 | AS | yes | 70.3s | 35,864 | 35,864 | 654,848 | 0 | 10,947 | n/a | commandExecution=22, fileChange=4 | +0.0000 | +0.0123 |
| 15 | pytest-dev__pytest-7432 | spark | T2 | PF | yes | 65.2s | 51,467 | 51,467 | 727,040 | 0 | 10,776 | n/a | commandExecution=22, fileChange=4 | +0.0013 | +0.0180 |
| 15 | pytest-dev__pytest-7432 | spark | T2 | base | yes | 43.8s | 24,057 | 24,057 | 475,136 | 0 | 8,394 | n/a | commandExecution=22, fileChange=2 | +0.0051 | +0.0683 |
| 16 | django__django-12193 | haiku | A | AS | yes | 291.8s | 1,305 | 66,215 | 2,826,129 | 64,910 | 27,305 | $0.550263 | Bash=44, Edit=11, Glob=1, Read=14, Write=1 | -0.0017 | +0.0000 |
| 16 | django__django-12193 | haiku | A | PF | yes | 241.2s | 1,201 | 52,978 | 2,097,010 | 51,777 | 20,252 | $0.415716 | Bash=41, Edit=6, Read=8, Write=3 | -0.0023 | +0.0000 |
| 16 | django__django-12193 | haiku | A | base | yes | 210.1s | 1,257 | 51,296 | 2,295,509 | 50,039 | 18,901 | $0.425391 | Bash=49, Edit=2, Read=14 | +0.0006 | +0.0000 |
| 16 | django__django-12193 | haiku | T | AS | yes | 229.6s | 11,173 | 69,446 | 2,626,775 | 58,273 | 19,376 | $0.487277 | Bash=41, Edit=7, Read=12 | -0.0031 | +0.0000 |
| 16 | django__django-12193 | haiku | T | PF | yes | 246.0s | 11,181 | 76,857 | 2,894,100 | 65,676 | 21,935 | $0.541618 | Bash=37, Edit=3, Grep=1, Read=19, bash=1 | -0.0003 | +0.0000 |
| 16 | django__django-12193 | haiku | T | base | no | 208.5s | 11,125 | 72,353 | 2,447,669 | 61,228 | 20,014 | $0.478418 | Bash=40, Edit=5, Glob=1, Grep=1, Read=10 | -0.0002 | +0.0000 |
| 16 | django__django-12193 | haiku | T2 | AS | yes | 229.1s | 6,193 | 56,600 | 2,082,423 | 50,407 | 22,844 | $0.429469 | Bash=38, Edit=5, Read=19 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | haiku | T2 | PF | yes | 280.9s | 6,249 | 73,324 | 3,059,576 | 67,075 | 26,793 | $0.580322 | Bash=48, Edit=6, Read=13 | -0.0026 | +0.0000 |
| 16 | django__django-12193 | haiku | T2 | base | yes | 236.9s | 6,265 | 66,515 | 3,064,783 | 60,250 | 21,923 | $0.542858 | Bash=49, Edit=3, Grep=1, Read=16 | -0.0003 | +0.0000 |
| 16 | django__django-12193 | luna | A | AS | yes | 185.3s | 66,757 | 66,757 | 661,504 | 0 | 6,726 | $0.213263 | commandExecution=18, fileChange=2, webSearch=4 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | luna | A | PF | yes | 135.4s | 39,567 | 39,567 | 437,760 | 0 | 5,163 | $0.114321 | commandExecution=16, fileChange=1 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | luna | A | base | yes | 121.9s | 45,720 | 45,720 | 467,712 | 0 | 3,979 | $0.146365 | commandExecution=13, fileChange=1, webSearch=3 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | luna | T | AS | yes | 193.9s | 63,059 | 63,059 | 860,928 | 0 | 7,902 | $0.226564 | commandExecution=20, fileChange=2, webSearch=3 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | luna | T | PF | yes | 187.6s | 76,073 | 76,073 | 834,560 | 0 | 7,105 | $0.202159 | commandExecution=26, fileChange=2 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | luna | T | base | yes | 81.6s | 24,800 | 24,800 | 192,768 | 0 | 3,390 | $0.064417 | commandExecution=9, fileChange=1 | -0.0003 | +0.0000 |
| 16 | django__django-12193 | luna | T2 | AS | yes | 157.6s | 54,560 | 54,560 | 617,472 | 0 | 5,343 | $0.188365 | commandExecution=12, fileChange=2, webSearch=4 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | luna | T2 | PF | no | 194.0s | 57,362 | 57,362 | 453,376 | 0 | 7,851 | $0.149806 | commandExecution=20, fileChange=2 | +0.0013 | +0.0000 |
| 16 | django__django-12193 | luna | T2 | base | yes | 95.3s | 63,149 | 63,149 | 332,288 | 0 | 3,565 | $0.147768 | commandExecution=9, fileChange=1, webSearch=3 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | sonnet | A | AS | no | 145.9s | 792 | 28,253 | 661,470 | 27,461 | 8,196 | $0.486887 | Bash=22, Edit=4, Grep=1, Read=3, Write=1 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | sonnet | A | PF | no | 254.8s | 834 | 47,493 | 1,478,706 | 46,659 | 17,317 | $0.984187 | Bash=37, Edit=5, Grep=4, Read=5, Write=1 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | sonnet | A | base | no | 213.4s | 828 | 33,844 | 1,051,131 | 33,016 | 10,922 | $0.678113 | Bash=42, Edit=2, Grep=1, Read=3, Write=1 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | sonnet | T | AS | no | 268.5s | 10,758 | 43,514 | 1,084,170 | 32,756 | 9,307 | $0.672108 | Bash=27, Edit=4, Read=4, Write=1 | +0.0000 | +0.0000 |
| 16 | django__django-12193 | sonnet | T | PF | no | 396.7s | 10,812 | 74,492 | 2,734,453 | 63,680 | 18,279 | $1.487489 | Bash=52, Edit=4, Read=6, Write=1 | +0.0000 | +0.0000 |
| 16 | django__django-12193 | sonnet | T | base | yes | 45.0s | 10,708 | 29,413 | 266,067 | 18,705 | 3,017 | $0.247851 | Bash=7, Edit=1, Read=3 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | sonnet | T2 | AS | no | 139.3s | 5,754 | 32,757 | 572,907 | 27,003 | 6,101 | $0.431069 | Bash=20, Edit=1, Read=2, Write=1 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | sonnet | T2 | PF | no | 365.1s | 5,808 | 49,835 | 1,604,063 | 44,027 | 13,176 | $0.948827 | Bash=44, Edit=2, Read=4, Write=1 | +0.0000 | +0.0000 |
| 16 | django__django-12193 | sonnet | T2 | base | yes | 154.1s | 5,762 | 39,556 | 831,205 | 33,794 | 5,759 | $0.544209 | Bash=25, Edit=1, Read=2 | -0.0006 | +0.0000 |
| 16 | django__django-12193 | spark | A | AS | no | 99.4s | 38,883 | 38,883 | 1,042,816 | 0 | 14,734 | n/a | commandExecution=42, fileChange=3 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | spark | A | PF | no | 162.9s | 57,019 | 57,019 | 1,424,640 | 0 | 17,122 | n/a | commandExecution=52, fileChange=2, webSearch=1 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | spark | A | base | no | 113.5s | 32,816 | 32,816 | 945,280 | 0 | 11,609 | n/a | commandExecution=45, fileChange=4 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | spark | T | AS | no | 126.6s | 54,893 | 54,893 | 2,025,344 | 0 | 19,509 | n/a | commandExecution=57, fileChange=2 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | spark | T | PF | no | 127.2s | 50,616 | 50,616 | 1,819,776 | 0 | 18,231 | n/a | commandExecution=50, fileChange=2 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | spark | T | base | yes | 59.4s | 38,385 | 38,385 | 932,224 | 0 | 8,302 | n/a | commandExecution=30, fileChange=3 | -0.0003 | +0.0000 |
| 16 | django__django-12193 | spark | T2 | AS | no | 97.0s | 41,395 | 41,395 | 1,187,456 | 0 | 15,330 | n/a | commandExecution=39, fileChange=2 | +0.0033 | +0.0000 |
| 16 | django__django-12193 | spark | T2 | PF | no | 113.9s | 46,625 | 46,625 | 1,338,240 | 0 | 15,257 | n/a | commandExecution=47, fileChange=2 | +0.0007 | +0.0000 |
| 16 | django__django-12193 | spark | T2 | base | yes | 71.3s | 37,601 | 37,601 | 961,280 | 0 | 11,194 | n/a | commandExecution=37, fileChange=3, webSearch=1 | -0.0003 | +0.0000 |
| 17 | django__django-16485 | haiku | A | AS | yes | 145.6s | 863 | 39,038 | 876,986 | 38,175 | 14,433 | $0.237077 | Bash=23, Edit=2, Read=4 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | A | PF | yes | 120.2s | 831 | 32,046 | 659,168 | 31,215 | 9,610 | $0.177228 | Bash=18, Edit=2, Read=5 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | A | base | yes | 134.3s | 879 | 47,389 | 915,114 | 46,510 | 12,862 | $0.249720 | Bash=22, Edit=2, Read=7 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | T | AS | yes | 162.7s | 5,528 | 38,911 | 713,013 | 33,383 | 14,712 | $0.217155 | Bash=21, Edit=1, Read=5 | +0.0004 | +0.0010 |
| 17 | django__django-16485 | haiku | T | PF | yes | 173.8s | 5,584 | 44,596 | 1,014,795 | 39,012 | 15,950 | $0.264838 | Bash=27, Edit=2, Read=5 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | T | base | yes | 126.6s | 5,600 | 38,249 | 955,548 | 32,649 | 10,687 | $0.219888 | Bash=28, Edit=2, Read=6 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | T2 | AS | yes | 148.8s | 2,795 | 39,350 | 960,683 | 36,555 | 10,864 | $0.226293 | Bash=26, Edit=2, Read=9 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | T2 | PF | yes | 168.5s | 2,819 | 34,134 | 960,965 | 31,315 | 12,436 | $0.223726 | Bash=31, Edit=2, Read=7 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | haiku | T2 | base | yes | 138.7s | 2,747 | 39,138 | 895,455 | 36,391 | 13,253 | $0.231340 | Bash=22, Edit=2, Read=7 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | A | AS | yes | 72.2s | 39,879 | 39,879 | 218,112 | 0 | 1,852 | $0.102802 | commandExecution=8, fileChange=1, webSearch=3 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | A | PF | yes | 100.0s | 38,680 | 38,680 | 174,848 | 0 | 3,240 | $0.075605 | commandExecution=10, fileChange=1 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | A | base | yes | 81.0s | 27,636 | 27,636 | 180,736 | 0 | 2,721 | $0.062036 | commandExecution=9, fileChange=1 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | T | AS | yes | 98.1s | 37,936 | 37,936 | 249,344 | 0 | 2,472 | $0.077702 | commandExecution=11, fileChange=1 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | T | PF | yes | 124.3s | 51,099 | 51,099 | 275,456 | 0 | 3,576 | $0.100101 | commandExecution=11, fileChange=1 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | T | base | yes | 81.6s | 71,087 | 71,087 | 428,544 | 0 | 2,881 | $0.161227 | commandExecution=8, fileChange=1, webSearch=3 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | T2 | AS | yes | 96.6s | 48,010 | 48,010 | 191,488 | 0 | 2,711 | $0.083425 | commandExecution=10, fileChange=1 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | T2 | PF | yes | 155.1s | 50,783 | 50,783 | 791,296 | 0 | 4,633 | $0.217711 | commandExecution=14, fileChange=1, webSearch=6 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | luna | T2 | base | yes | 51.0s | 15,776 | 15,776 | 97,280 | 0 | 1,942 | $0.037156 | commandExecution=4, fileChange=1 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | sonnet | A | AS | yes | 132.5s | 666 | 17,152 | 321,944 | 16,486 | 5,599 | $0.280068 | Bash=10, Edit=3, Grep=3, Read=5 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | sonnet | A | PF | yes | 89.7s | 658 | 13,255 | 243,247 | 12,597 | 5,498 | $0.231586 | Bash=11, Edit=3, Read=3 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | sonnet | A | base | yes | 65.6s | 660 | 12,181 | 249,201 | 11,521 | 3,955 | $0.203777 | Bash=9, Edit=4, Grep=2, Read=3 | +0.0004 | +0.0010 |
| 17 | django__django-16485 | sonnet | T | AS | yes | 228.0s | 5,349 | 26,529 | 476,100 | 21,180 | 6,144 | $0.367341 | Bash=15, Edit=4, Read=3 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | sonnet | T | PF | yes | 150.0s | 5,349 | 36,082 | 578,909 | 30,733 | 10,324 | $0.518212 | Bash=15, Edit=3, Read=4 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | sonnet | T | base | yes | 148.7s | 5,369 | 35,840 | 835,808 | 30,471 | 9,620 | $0.583199 | Bash=24, Edit=6, Read=2 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | sonnet | T2 | AS | yes | 105.2s | 2,528 | 19,270 | 315,168 | 16,742 | 5,751 | $0.283691 | Bash=12, Edit=4, Read=2 | +0.0004 | +0.0010 |
| 17 | django__django-16485 | sonnet | T2 | PF | yes | 355.5s | 2,544 | 21,131 | 489,410 | 18,587 | 6,699 | $0.361312 | Bash=18, Edit=2, Grep=1, Read=5 | +0.0004 | +0.0010 |
| 17 | django__django-16485 | sonnet | T2 | base | yes | 121.7s | 2,534 | 25,290 | 447,808 | 22,756 | 6,105 | $0.364905 | Bash=15, Edit=3, Read=3 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | A | AS | yes | 56.4s | 34,508 | 34,508 | 275,200 | 0 | 5,138 | n/a | commandExecution=15, fileChange=2 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | A | PF | yes | 63.8s | 38,900 | 38,900 | 338,816 | 0 | 6,734 | n/a | commandExecution=21, fileChange=2 | -0.0004 | -0.0010 |
| 17 | django__django-16485 | spark | A | base | yes | 39.1s | 16,299 | 16,299 | 321,664 | 0 | 5,470 | n/a | commandExecution=20, fileChange=2 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | T | AS | yes | 70.1s | 44,855 | 44,855 | 437,376 | 0 | 7,452 | n/a | commandExecution=16, fileChange=4 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | T | PF | yes | 72.4s | 40,621 | 40,621 | 480,256 | 0 | 9,941 | n/a | commandExecution=24 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | T | base | yes | 30.7s | 16,787 | 16,787 | 215,808 | 0 | 4,241 | n/a | commandExecution=11, fileChange=2 | +0.0008 | +0.0115 |
| 17 | django__django-16485 | spark | T2 | AS | yes | 58.9s | 35,659 | 35,659 | 303,360 | 0 | 6,610 | n/a | commandExecution=17, fileChange=2 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | T2 | PF | yes | 107.4s | 39,123 | 39,123 | 438,400 | 0 | 9,025 | n/a | commandExecution=22, fileChange=2 | +0.0000 | +0.0000 |
| 17 | django__django-16485 | spark | T2 | base | yes | 25.5s | 15,892 | 15,892 | 182,784 | 0 | 6,681 | n/a | commandExecution=11, fileChange=2 | +0.0000 | +0.0000 |
| 18 | django__django-16877 | haiku | A | AS | no | 186.9s | 993 | 35,920 | 1,172,959 | 34,927 | 10,978 | $0.243033 | Bash=31, Edit=3, Read=7, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | haiku | A | PF | no | 204.0s | 1,177 | 54,009 | 2,531,304 | 52,832 | 12,160 | $0.420771 | Bash=50, Edit=2, Grep=1, Read=11, Write=1 | -0.0007 | -0.0018 |
| 18 | django__django-16877 | haiku | A | base | no | 138.1s | 1,025 | 41,946 | 1,311,441 | 40,921 | 10,569 | $0.266856 | Bash=33, Edit=2, Read=10, Write=1 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | haiku | T | AS | no | 153.2s | 8,118 | 83,627 | 1,551,678 | 75,509 | 10,894 | $0.368774 | Bash=27, Edit=2, Read=9, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | haiku | T | PF | no | 201.6s | 8,222 | 54,519 | 1,884,089 | 46,297 | 14,787 | $0.363160 | Bash=39, Edit=3, Read=9, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | haiku | T | base | no | 130.1s | 8,158 | 42,625 | 1,190,968 | 34,467 | 11,937 | $0.255874 | Bash=28, Edit=2, Read=15, Write=1 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | haiku | T2 | AS | no | 171.2s | 7,200 | 44,348 | 1,176,947 | 37,148 | 11,529 | $0.256836 | Bash=26, Edit=1, Read=9, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | haiku | T2 | PF | no | 179.0s | 7,296 | 47,087 | 1,594,730 | 39,791 | 12,468 | $0.308691 | Bash=34, Edit=2, Grep=1, Read=11, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | haiku | T2 | base | no | 131.5s | 7,192 | 49,029 | 1,185,575 | 41,837 | 9,296 | $0.255903 | Bash=25, Edit=1, Grep=1, Read=8, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | luna | A | AS | yes | 150.2s | 69,390 | 69,390 | 669,440 | 0 | 5,772 | $0.210966 | commandExecution=13, fileChange=1, webSearch=4 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | luna | A | PF | yes | 162.1s | 68,427 | 68,427 | 1,050,624 | 0 | 6,153 | $0.250407 | commandExecution=15, fileChange=2, webSearch=4 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | luna | A | base | yes | 194.9s | 105,231 | 105,231 | 1,961,728 | 0 | 7,687 | $0.457526 | commandExecution=18, fileChange=2, webSearch=11 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | luna | T | AS | yes | 190.6s | 65,121 | 65,121 | 997,632 | 0 | 7,508 | $0.269932 | commandExecution=11, fileChange=4, webSearch=6 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | luna | T | PF | no | 195.0s | 61,467 | 61,467 | 456,960 | 0 | 6,976 | $0.149019 | commandExecution=15, fileChange=1 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | luna | T | base | yes | 193.2s | 46,258 | 46,258 | 593,408 | 0 | 8,357 | $0.155741 | commandExecution=14, fileChange=3 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | luna | T2 | AS | yes | 148.4s | 60,339 | 60,339 | 745,728 | 0 | 5,395 | $0.207282 | commandExecution=9, fileChange=2, webSearch=4 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | luna | T2 | PF | yes | 304.4s | 88,272 | 88,272 | 1,521,408 | 0 | 12,385 | $0.364723 | commandExecution=19, fileChange=8, webSearch=5 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | luna | T2 | base | no | 150.2s | 59,466 | 59,466 | 748,288 | 0 | 6,028 | $0.210463 | commandExecution=17, fileChange=2, webSearch=4 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | A | AS | no | 187.7s | 740 | 30,965 | 950,198 | 30,225 | 11,193 | $0.635068 | Bash=31, Edit=4, Read=7, Write=3 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | A | PF | no | 128.7s | 704 | 18,855 | 447,368 | 18,151 | 6,567 | $0.342277 | Bash=13, Edit=5, Grep=5, Read=3, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | A | base | no | 238.2s | 710 | 20,049 | 541,835 | 19,339 | 7,109 | $0.385894 | Bash=18, Edit=5, Grep=1, Read=4, Write=2 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | T | AS | no | 149.6s | 7,849 | 40,197 | 664,156 | 32,348 | 6,548 | $0.499358 | Bash=11, Edit=4, Grep=3, Read=6, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | T | PF | no | 112.8s | 7,851 | 35,264 | 681,098 | 27,413 | 7,802 | $0.493636 | Bash=13, Edit=5, Grep=1, Read=6, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | T | base | yes | 239.7s | 7,857 | 36,486 | 775,738 | 28,629 | 7,310 | $0.521952 | Bash=14, Edit=4, Grep=1, Read=9, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | T2 | AS | no | 248.2s | 6,953 | 32,843 | 680,350 | 25,890 | 7,685 | $0.481629 | Bash=15, Edit=4, Grep=1, Read=7, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | T2 | PF | no | 110.7s | 6,945 | 34,023 | 655,287 | 27,078 | 6,513 | $0.463634 | Bash=13, Edit=3, Read=7, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | sonnet | T2 | base | yes | 227.5s | 6,945 | 31,376 | 568,281 | 24,431 | 5,978 | $0.413625 | Bash=12, Edit=4, Grep=3, Read=4, Write=1 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | spark | A | AS | no | 103.0s | 28,145 | 28,145 | 829,184 | 0 | 8,363 | n/a | commandExecution=40, fileChange=4 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | spark | A | PF | yes | 166.6s | 94,721 | 94,721 | 2,187,648 | 0 | 14,305 | n/a | commandExecution=49, fileChange=5, webSearch=8 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | spark | A | base | no | 131.0s | 47,896 | 47,896 | 1,325,440 | 0 | 13,576 | n/a | commandExecution=54, fileChange=9 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | spark | T | AS | no | 73.0s | 47,284 | 47,284 | 862,592 | 0 | 9,491 | n/a | commandExecution=32, fileChange=5 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | spark | T | PF | no | 79.6s | 43,211 | 43,211 | 784,512 | 0 | 8,808 | n/a | commandExecution=32, fileChange=3 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | spark | T | base | yes | 73.5s | 40,116 | 40,116 | 1,125,248 | 0 | 12,003 | n/a | commandExecution=28, fileChange=7 | -0.0007 | -0.0016 |
| 18 | django__django-16877 | spark | T2 | AS | yes | 91.7s | 30,145 | 30,145 | 667,264 | 0 | 10,994 | n/a | commandExecution=26, fileChange=5 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | spark | T2 | PF | yes | 97.3s | 40,574 | 40,574 | 1,442,944 | 0 | 11,312 | n/a | commandExecution=40, fileChange=7 | -0.0007 | -0.0015 |
| 18 | django__django-16877 | spark | T2 | base | no | 106.0s | 35,947 | 35,947 | 1,054,848 | 0 | 14,404 | n/a | commandExecution=36, fileChange=4 | -0.0007 | -0.0015 |
| 19 | django__django-11451 | haiku | A | AS | yes | 213.1s | 1,495 | 50,241 | 1,776,863 | 48,746 | 17,249 | $0.362918 | Bash=35, Edit=4, Read=7, Write=6 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | A | PF | yes | 125.0s | 1,335 | 36,410 | 826,334 | 35,075 | 8,191 | $0.195073 | Bash=22, Edit=1, Read=8, Write=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | A | base | yes | 78.1s | 1,279 | 38,141 | 668,860 | 36,862 | 6,835 | $0.176064 | Bash=17, Edit=1, Read=6, Write=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | T | AS | yes | 200.7s | 9,911 | 63,673 | 2,185,388 | 53,762 | 14,539 | $0.408669 | Bash=37, Edit=3, Read=8, Write=4 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | T | PF | yes | 143.8s | 9,767 | 47,497 | 1,046,438 | 37,730 | 10,720 | $0.243471 | Bash=24, Edit=1, Read=9 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | T | base | yes | 132.7s | 9,839 | 58,710 | 1,655,533 | 48,871 | 11,337 | $0.329819 | Bash=28, Edit=5, Read=10 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | T2 | AS | yes | 210.0s | 7,891 | 55,670 | 1,864,041 | 47,779 | 15,400 | $0.366853 | Bash=37, Edit=1, Read=5, Write=6 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | haiku | T2 | PF | yes | 162.6s | 7,811 | 57,052 | 1,506,834 | 49,241 | 13,895 | $0.326451 | Bash=23, Edit=4, Read=10, Write=2 | +0.0245 | +0.0000 |
| 19 | django__django-11451 | haiku | T2 | base | yes | 253.9s | 7,795 | 55,015 | 1,400,692 | 47,220 | 12,120 | $0.302904 | Bash=28, Edit=2, Grep=1, Read=6 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | A | AS | yes | 112.4s | 34,064 | 34,064 | 342,272 | 0 | 3,763 | $0.090869 | commandExecution=14, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | A | PF | yes | 107.9s | 38,491 | 38,491 | 275,456 | 0 | 3,855 | $0.089167 | commandExecution=12, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | A | base | yes | 83.9s | 30,436 | 30,436 | 215,296 | 0 | 3,356 | $0.072102 | commandExecution=10, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | T | AS | yes | 75.0s | 46,686 | 46,686 | 196,096 | 0 | 2,691 | $0.082442 | commandExecution=9, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | T | PF | yes | 119.7s | 73,547 | 73,547 | 495,616 | 0 | 4,648 | $0.150997 | commandExecution=15, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | T | base | yes | 53.4s | 22,724 | 22,724 | 126,208 | 0 | 1,848 | $0.046433 | commandExecution=5, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | T2 | AS | yes | 121.2s | 61,797 | 61,797 | 284,160 | 0 | 3,675 | $0.112263 | commandExecution=12, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | T2 | PF | yes | 150.0s | 64,490 | 64,490 | 309,248 | 0 | 4,815 | $0.124305 | commandExecution=13, fileChange=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | luna | T2 | base | yes | 75.8s | 27,010 | 27,010 | 245,504 | 0 | 2,957 | $0.069302 | commandExecution=10, fileChange=2 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | A | AS | yes | 41.7s | 1,094 | 9,344 | 141,844 | 8,250 | 1,589 | $0.116840 | Bash=10, Edit=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | A | PF | yes | 190.5s | 1,106 | 14,707 | 246,800 | 13,601 | 3,775 | $0.213239 | Bash=13, Edit=2, Grep=1, Read=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | A | base | yes | 70.5s | 1,100 | 14,534 | 222,222 | 13,434 | 2,905 | $0.191766 | Bash=8, Edit=2, Grep=1, Read=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | T | AS | yes | 157.6s | 9,526 | 39,742 | 536,629 | 30,216 | 4,635 | $0.421196 | Bash=13, Edit=2, Read=4 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | T | PF | yes | 103.2s | 9,536 | 38,796 | 687,485 | 29,260 | 5,817 | $0.478476 | Bash=16, Edit=2, Read=6 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | T | base | yes | 61.5s | 9,518 | 33,296 | 389,616 | 23,778 | 2,666 | $0.308905 | Bash=10, Edit=2, Read=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | T2 | AS | yes | 167.1s | 7,570 | 42,889 | 1,175,110 | 35,319 | 7,129 | $0.678872 | Bash=33, Edit=2, Grep=1, Read=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | T2 | PF | yes | 247.3s | 7,544 | 37,516 | 711,026 | 29,972 | 6,777 | $0.502227 | Bash=16, Edit=4, Read=6 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | sonnet | T2 | base | yes | 71.3s | 7,524 | 30,437 | 377,993 | 22,913 | 3,620 | $0.312528 | Bash=10, Edit=3, Read=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | A | AS | yes | 60.8s | 28,980 | 28,980 | 660,992 | 0 | 8,531 | n/a | commandExecution=29, fileChange=3, webSearch=1 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | A | PF | yes | 100.8s | 52,697 | 52,697 | 1,393,792 | 0 | 12,103 | n/a | commandExecution=50, fileChange=4 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | A | base | yes | 76.1s | 43,925 | 43,925 | 507,008 | 0 | 8,616 | n/a | commandExecution=25, fileChange=2 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | T | AS | yes | 120.8s | 54,737 | 54,737 | 2,008,832 | 0 | 17,783 | n/a | commandExecution=49, fileChange=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | T | PF | yes | 81.6s | 41,598 | 41,598 | 1,174,016 | 0 | 11,251 | n/a | commandExecution=37, fileChange=2 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | T | base | yes | 61.1s | 56,332 | 56,332 | 809,984 | 0 | 12,612 | n/a | commandExecution=27, fileChange=2 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | T2 | AS | yes | 112.8s | 57,759 | 57,759 | 1,435,520 | 0 | 16,668 | n/a | commandExecution=43, fileChange=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | T2 | PF | yes | 169.6s | 82,526 | 82,526 | 3,175,936 | 0 | 28,517 | n/a | commandExecution=69, fileChange=3 | +0.0041 | +0.0000 |
| 19 | django__django-11451 | spark | T2 | base | yes | 49.9s | 56,572 | 56,572 | 694,272 | 0 | 10,455 | n/a | commandExecution=22, fileChange=4 | +0.0041 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | A | AS | no | 153.5s | 2,264 | 36,094 | 831,601 | 33,830 | 11,281 | $0.209489 | Bash=21, Edit=4, Read=7, Write=2 | +0.0028 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | A | PF | no | 170.5s | 2,336 | 36,959 | 1,066,516 | 34,623 | 10,307 | $0.229769 | Bash=29, Edit=4, Grep=2, Read=8 | +0.0035 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | A | base | no | 118.7s | 2,248 | 34,430 | 760,009 | 32,182 | 9,795 | $0.191588 | Bash=22, Edit=4, Read=6 | +0.0035 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | T | AS | yes | 143.0s | 8,613 | 54,570 | 1,661,600 | 45,957 | 9,958 | $0.316477 | Bash=32, Edit=7, Read=7 | +0.0000 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | T | PF | no | 121.1s | 8,509 | 45,114 | 936,016 | 36,605 | 8,230 | $0.216471 | Bash=22, Edit=4, Glob=1, Grep=1, Read=5 | +0.0035 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | T | base | no | 114.9s | 8,509 | 48,192 | 1,060,996 | 39,683 | 10,692 | $0.247435 | Bash=21, Edit=5, Read=7 | +0.0022 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | T2 | AS | yes | 150.2s | 8,655 | 52,250 | 1,304,321 | 43,595 | 11,642 | $0.284487 | Bash=25, Edit=5, Read=6, Write=4 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | T2 | PF | no | 130.8s | 8,607 | 53,980 | 1,108,327 | 45,373 | 11,239 | $0.266381 | Bash=24, Edit=5, Read=5 | +0.0028 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | haiku | T2 | base | yes | 117.2s | 8,639 | 52,528 | 1,224,458 | 43,889 | 9,746 | $0.267593 | Bash=22, Edit=9, Read=7 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | A | AS | yes | 139.4s | 52,080 | 52,080 | 508,160 | 0 | 5,670 | $0.136916 | commandExecution=15, fileChange=4 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | A | PF | yes | 158.2s | 91,435 | 91,435 | 548,352 | 0 | 6,628 | $0.186038 | commandExecution=14, fileChange=2 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | A | base | yes | 163.1s | 60,231 | 60,231 | 705,536 | 0 | 6,957 | $0.202527 | commandExecution=17, fileChange=4, webSearch=3 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | T | AS | yes | 152.5s | 62,458 | 62,458 | 870,400 | 0 | 5,507 | $0.222540 | commandExecution=12, fileChange=2, webSearch=4 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | T | PF | yes | 203.6s | 100,974 | 100,974 | 982,272 | 0 | 7,903 | $0.286619 | commandExecution=19, fileChange=2, webSearch=4 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | T | base | yes | 149.6s | 58,584 | 58,584 | 701,440 | 0 | 6,478 | $0.167596 | commandExecution=16, fileChange=5 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | T2 | AS | yes | 99.5s | 67,243 | 67,243 | 689,664 | 0 | 3,735 | $0.208619 | commandExecution=8, fileChange=1, webSearch=5 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | T2 | PF | yes | 215.6s | 95,837 | 95,837 | 1,332,992 | 0 | 7,897 | $0.326518 | commandExecution=17, fileChange=4, webSearch=5 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | luna | T2 | base | yes | 179.9s | 95,733 | 95,733 | 1,071,360 | 0 | 7,495 | $0.247839 | commandExecution=19, fileChange=5 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | A | AS | yes | 205.8s | 2,029 | 17,413 | 381,059 | 15,384 | 5,404 | $0.289623 | Bash=13, Edit=5, Grep=1, Read=2, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | A | PF | yes | 324.6s | 2,065 | 41,400 | 1,127,923 | 39,335 | 16,827 | $0.828791 | Bash=21, Edit=11, Grep=1, Read=6, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | A | base | yes | 230.1s | 2,037 | 22,810 | 501,649 | 20,773 | 6,610 | $0.376218 | Bash=13, Edit=6, Grep=1, Read=4, Write=2 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | T | AS | yes | 328.1s | 8,306 | 43,726 | 1,033,128 | 35,420 | 9,407 | $0.671779 | Bash=19, Edit=8, Read=6, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | T | PF | yes | 269.3s | 8,294 | 48,028 | 922,399 | 39,734 | 7,525 | $0.636189 | Bash=15, Edit=7, Read=5, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | T | base | yes | 241.1s | 8,296 | 38,010 | 802,823 | 29,714 | 8,196 | $0.550277 | Bash=15, Edit=7, Read=6, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | T2 | AS | yes | 273.4s | 8,390 | 44,053 | 937,154 | 35,663 | 10,368 | $0.658942 | Bash=18, Edit=8, Read=4, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | T2 | PF | yes | 307.8s | 8,408 | 52,201 | 1,422,842 | 43,793 | 11,559 | $0.871348 | Bash=18, Edit=13, Read=8, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | sonnet | T2 | base | yes | 393.9s | 8,388 | 42,267 | 885,391 | 33,879 | 9,823 | $0.624548 | Bash=17, Edit=7, Read=5, Write=1 | -0.0149 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | A | AS | no | 140.2s | 29,016 | 29,016 | 713,216 | 0 | 10,480 | n/a | commandExecution=34, fileChange=4 | -0.0121 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | A | PF | no | 56.1s | 31,017 | 31,017 | 656,768 | 0 | 11,668 | n/a | commandExecution=24, fileChange=4, mcpToolCall=1 | -0.0121 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | A | base | no | 82.7s | 41,179 | 41,179 | 1,467,392 | 0 | 15,546 | n/a | commandExecution=46, fileChange=10 | -0.0121 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | T | AS | no | 91.2s | 63,902 | 63,902 | 1,089,408 | 0 | 12,304 | n/a | commandExecution=29, fileChange=5 | -0.0104 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | T | PF | no | 83.7s | 58,001 | 58,001 | 1,181,184 | 0 | 13,421 | n/a | commandExecution=37, fileChange=4 | -0.0121 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | T | base | no | 91.7s | 40,380 | 40,380 | 1,041,664 | 0 | 11,572 | n/a | commandExecution=35, fileChange=4 | -0.0104 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | T2 | AS | no | 53.4s | 36,633 | 36,633 | 670,336 | 0 | 8,648 | n/a | commandExecution=21, fileChange=5 | -0.0121 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | T2 | PF | no | 109.8s | 58,461 | 58,461 | 1,787,904 | 0 | 15,478 | n/a | commandExecution=37, fileChange=8, mcpToolCall=1 | -0.0112 | +0.0000 |
| 20 | pytest-dev__pytest-7205 | spark | T2 | base | no | 108.1s | 64,220 | 64,220 | 1,313,536 | 0 | 11,838 | n/a | commandExecution=38, fileChange=6 | -0.0121 | +0.0000 |

## Registered rule and issue #817 disposition

The issue836-v4 rule examines complete, compliant Sonnet evidence in this
order: reject invalid evidence or treatment regressions; then select the arm
with more resolutions, fewer pass-to-pass regressions, or material efficiency.
Its material-efficiency branch requires either at least 15% fewer tokens, or
no more than 5% more tokens together with at least 20% less wall time.

The repaired ledger does not retain the registered pass-to-pass-regression
aggregate, so the rule cannot be computed in full. The available resolution
metric favors **T** (18 resolved versus 17), while +2.9% tokens and −16.2%
wall time would miss the separate 20% material-efficiency threshold.

This is not a formally valid preregistered selector outcome. The final study
changed the registered mandatory-traversal prompt and restrictive runtime
after operational defects were discovered. Therefore this report does **not**
advance #817 under issue #836's registered gate. It supports continuing the
pre-injected-context treatment to a cleanly registered confirmatory run; that
is an operational recommendation, not a retroactive claim that the repaired
study was the originally registered experiment. The registered decision is
therefore **not computed**; the descriptive operational recommendation favors
T, and the formal #817 disposition is no advancement from this evidence.

## Input and output decomposition

Input tokens are cumulative context processed over model turns, not unique
bytes supplied once. T adds 425,461 prompt bytes across the 20 cases—21,273
bytes per case on average—and that prefix can be processed again on later
turns.

### Uncached input

For this table, uncached Sonnet input includes ordinary input plus
cache-creation input. The inclusive Sonnet value also includes the CLI's
auxiliary Haiku model.

| Backend/accounting | A | T | Change |
|---|---:|---:|---:|
| Luna uncached input | 1,036,130 | 1,037,119 | **+989 (+0.10%)** |
| Sonnet main-model uncached input | 442,821 | 557,970 | **+115,149 (+26.0%)** |
| Sonnet inclusive uncached input | 463,028 | 727,235 | **+264,207 (+57.1%)** |
| Sonnet auxiliary Haiku portion | 20,207 | 169,265 | **+149,058 (+737.7%)** |

Luna's uncached input was effectively flat; its input increase came almost
entirely from cached replay (11,890,176 A versus 12,202,496 T). Sonnet's
uncached increase is real. The main model accounts for 115,149 additional
tokens, while auxiliary Haiku processing accounts for 149,058. Provider
receipts do not identify the auxiliary task, so main-only and inclusive values
are kept separate.

### Output

| Backend/component | A | T | Change |
|---|---:|---:|---:|
| Sonnet total output | 146,071 | 131,649 | **−14,422 (−9.9%)** |
| Luna total output | 91,065 | 97,953 | +6,888 (+7.6%) |
| Luna reasoning subset | 44,568 | 53,086 | +8,518 (+19.1%) |
| Luna non-reasoning output | 46,497 | 44,867 | **−1,630 (−3.5%)** |

Sonnet generated less output under T. Luna's aggregate output increase is
entirely an increase in provider-reported reasoning tokens; its non-reasoning
output fell. Luna T output was lower in 12/20 pairs despite the positive
aggregate, because ranks 4, 6, and 15 contributed large increases. Sonnet T
output was lower in 11/20 pairs and its median paired change was −10.6%.

## Why more tokens can still mean faster, better work

Token processing and wall time measure different mechanisms. Input prefill and
cached-context replay are batched model computation. Tool calls, shell
commands, tests, searches, output decoding, and model/tool round trips are
serial and latency-heavy. A larger initial context can therefore increase
processed input while eliminating slower search/read/test cycles.

That mechanism is clearest for Sonnet: T processed 367,698 more total tokens,
but made 94 fewer tool calls, including 52 fewer Bash calls and 33 fewer Grep
calls, generated 14,422 fewer output tokens, and finished 651 seconds sooner.
Luna made 34 fewer tool calls but did not save aggregate wall time; its median
paired time was essentially unchanged and increased reasoning offset the
interaction reduction.

## Outcome-normalized diagnostics

These ratios are descriptive post-hoc diagnostics, not replacements for the
paired raw metrics or the registered decision rule.

| Metric per resolved case | Sonnet A→T | Luna A→T |
|---|---:|---:|
| Tokens | **−2.9%** | **−2.9%** |
| Wall time | **−20.8%** | **−1.1%** |
| Tool calls | **−22.0%** | **−14.8%** |
| Cost | +1.7% | **−6.7%** |

Outcome normalization explains why the efficacy result matters operationally:
raw token totals rose, but tokens per successful patch fell slightly because T
produced one more resolution on each model. Sonnet delivered the clearest time
and interaction improvement; Luna delivered the clearest cost-per-resolution
improvement under API-equivalent pricing.

## Heterogeneity

The aggregate effect is a net of large trajectory changes, not a uniform
2–3% penalty:

- Sonnet T used fewer total tokens in 5/20 cases, with a median paired change
  of +33.3%. Rank 3 increased by 1.13M tokens while rank 7 decreased by 2.65M.
- Luna T used fewer total tokens in 9/20 cases, with a median paired change of
  +3.2%. Rank 5 increased by 840K while rank 18 decreased by 1.43M.
- T used fewer tools in 12/20 pairs on each model.
- Sonnet T was faster in 12/20 pairs; Luna T was faster in 10/20.

The common A-only loss was rank 6. Its 50-record title result was noisy and
contained some template/filter material but missed
`django/template/defaultfilters.py:add`; the deterministic
`django/db/models/sql/query.py:add_filter` seed then supplied an unrelated ORM
graph. This is classified as an RNA retrieval/seed/context failure, not a
runtime failure.


## Per-case details

Each metrics cell reports official efficacy, elapsed wall time, input tokens,
output tokens, and cost. Tool use is independently recounted from each
transcript and broken down by provider-native tool type. The deterministic
pre-injected RNA exposure is treatment context, not a model-initiated tool
call, and is disclosed separately from these counts.

### Metrics by case and condition

| Rank | Case | A-Sonnet | A-Luna | Original T-Sonnet | Original T-Luna | T2-Sonnet | T2-Luna |
|---:|---|---|---|---|---|---|---|
| 1 | `sympy__sympy-23534` | success yes · 199.6s · in 179,511 · out 2,705 · $0.195187 | success yes · 74.5s · in 201,307 · out 2,769 · $0.082907 | success yes · 61.4s · in 234,761 · out 1,938 · $0.259946 | success yes · 53.5s · in 205,312 · out 2,087 · $0.062084 | success yes · 105.2s · in 301,525 · out 2,895 · $0.283434 | success yes · 89.8s · in 441,078 · out 2,761 · $0.102137 |
| 2 | `django__django-11179` | success yes · 233.9s · in 527,787 · out 7,214 · $0.379073 | success yes · 66.0s · in 203,084 · out 2,250 · $0.055304 | success yes · 215.4s · in 662,547 · out 4,879 · $0.423247 | success yes · 70.5s · in 292,042 · out 2,221 · $0.072203 | success yes · 119.3s · in 305,797 · out 2,564 · $0.278292 | success yes · 50.6s · in 211,782 · out 1,845 · $0.080004 |
| 3 | `sympy__sympy-13757` | success yes · 256.0s · in 363,595 · out 5,589 · $0.293556 | success yes · 205.7s · in 1,359,660 · out 7,706 · $0.248827 | success yes · 318.0s · in 1,488,908 · out 12,258 · $0.897869 | success yes · 133.7s · in 654,825 · out 5,419 · $0.154654 | success yes · 149.6s · in 727,844 · out 5,759 · $0.485625 | success yes · 137.7s · in 760,091 · out 5,689 · $0.156247 |
| 4 | `django__django-13033` | success yes · 154.2s · in 850,393 · out 10,768 · $0.594492 | success yes · 79.0s · in 562,223 · out 2,537 · $0.146184 | success yes · 111.4s · in 657,060 · out 7,989 · $0.489729 | success yes · 198.5s · in 649,447 · out 8,693 · $0.162930 | success yes · 195.1s · in 1,571,240 · out 11,394 · $0.911260 | success yes · 107.8s · in 787,744 · out 4,249 · $0.211436 |
| 5 | `pydata__xarray-4075` | success yes · 275.1s · in 840,923 · out 11,149 · $0.598495 | success yes · 175.5s · in 543,974 · out 7,692 · $0.201489 | success yes · 279.7s · in 1,154,953 · out 9,480 · $0.722487 | success yes · 188.3s · in 1,384,232 · out 7,627 · $0.330621 | success yes · 264.3s · in 1,413,844 · out 10,683 · $0.876066 | success yes · 180.5s · in 666,727 · out 7,996 · $0.189852 |
| 6 | `django__django-13794` | success yes · 236.7s · in 1,116,311 · out 15,847 · $0.784036 | success yes · 112.7s · in 831,481 · out 3,749 · $0.213005 | success no · 417.9s · in 1,249,797 · out 16,085 · $0.846995 | success no · 163.9s · in 557,501 · out 7,150 · $0.173979 | success no · 291.0s · in 816,533 · out 10,796 · $0.582291 | success yes · 98.5s · in 566,925 · out 3,770 · $0.162431 |
| 7 | `matplotlib__matplotlib-24026` | success yes · 747.5s · in 3,591,313 · out 24,186 · $1.921416 | success yes · 283.5s · in 2,215,343 · out 9,401 · $0.401640 | success yes · 308.6s · in 951,062 · out 10,986 · $0.684515 | success yes · 258.5s · in 3,061,976 · out 9,132 · $0.513850 | success yes · 223.5s · in 1,627,570 · out 15,371 · $1.010474 | success yes · 286.1s · in 2,881,143 · out 9,621 · $0.590687 |
| 8 | `django__django-11163` | success yes · 106.9s · in 351,726 · out 5,049 · $0.249018 | success yes · 47.0s · in 109,306 · out 1,521 · $0.037792 | success yes · 44.0s · in 262,041 · out 2,412 · $0.213630 | success yes · 33.5s · in 108,679 · out 965 · $0.034751 | success yes · 121.4s · in 472,446 · out 4,449 · $0.332135 | success yes · 29.7s · in 98,599 · out 871 · $0.036087 |
| 9 | `django__django-16612` | success yes · 71.8s · in 221,086 · out 3,049 · $0.171232 | success yes · 59.3s · in 154,699 · out 2,017 · $0.051140 | success yes · 50.8s · in 262,866 · out 2,653 · $0.243282 | success yes · 67.0s · in 258,851 · out 2,427 · $0.066744 | success yes · 257.6s · in 828,797 · out 5,375 · $0.516622 | success yes · 107.3s · in 540,335 · out 3,584 · $0.139055 |
| 10 | `django__django-11551` | success yes · 101.1s · in 403,685 · out 5,746 · $0.314144 | success yes · 104.8s · in 425,444 · out 4,010 · $0.156944 | success yes · 33.8s · in 168,235 · out 2,127 · $0.185380 | success yes · 145.1s · in 420,972 · out 4,934 · $0.108202 | success yes · 157.3s · in 666,958 · out 9,051 · $0.520922 | success yes · 130.4s · in 654,383 · out 5,706 · $0.143953 |
| 11 | `django__django-13658` | success yes · 89.6s · in 125,332 · out 1,797 · $0.095596 | success yes · 161.2s · in 586,597 · out 4,419 · $0.117981 | success yes · 228.9s · in 299,931 · out 2,777 · $0.252531 | success yes · 194.1s · in 640,999 · out 3,970 · $0.125222 | success yes · 93.7s · in 182,750 · out 2,024 · $0.176311 | success yes · 136.6s · in 499,553 · out 3,699 · $0.122925 |
| 12 | `psf__requests-1724` | success yes · 275.1s · in 841,053 · out 9,463 · $0.560140 | success no · 154.9s · in 878,616 · out 6,527 · $0.221650 | success yes · 152.1s · in 846,192 · out 7,758 · $0.572233 | success yes · 145.7s · in 1,383,586 · out 5,700 · $0.346800 | success no · 488.1s · in 2,248,397 · out 14,113 · $1.156271 | success no · 183.6s · in 985,062 · out 7,813 · $0.284388 |
| 13 | `sympy__sympy-18763` | success no · 241.4s · in 311,170 · out 5,642 · $0.251346 | success no · 88.7s · in 294,302 · out 3,391 · $0.087243 | success no · 98.0s · in 586,478 · out 6,034 · $0.411436 | success yes · 87.6s · in 308,651 · out 3,025 · $0.087508 | success no · 70.8s · in 304,390 · out 3,546 · $0.228276 | success no · 87.7s · in 358,485 · out 2,975 · $0.101237 |
| 14 | `pytest-dev__pytest-7982` | success yes · 170.2s · in 202,688 · out 2,844 · $0.149304 | success yes · 69.4s · in 272,988 · out 2,992 · $0.077359 | success yes · 90.2s · in 395,815 · out 6,667 · $0.321930 | success yes · 77.5s · in 222,917 · out 2,994 · $0.083057 | success yes · 44.7s · in 136,451 · out 3,068 · $0.129168 | success yes · 84.2s · in 247,944 · out 3,354 · $0.066929 |
| 15 | `pytest-dev__pytest-7432` | success yes · 48.4s · in 140,987 · out 3,522 · $0.150536 | success yes · 136.8s · in 487,020 · out 5,384 · $0.111516 | success yes · 228.3s · in 655,393 · out 6,797 · $0.465514 | success yes · 194.6s · in 823,804 · out 8,655 · $0.187068 | success yes · 162.8s · in 416,304 · out 5,001 · $0.326295 | success yes · 126.4s · in 422,743 · out 5,035 · $0.105740 |
| 16 | `django__django-12193` | success no · 213.4s · in 1,084,975 · out 10,922 · $0.678113 | success yes · 121.9s · in 513,432 · out 3,979 · $0.146365 | success yes · 45.0s · in 295,480 · out 3,017 · $0.247851 | success yes · 81.6s · in 217,568 · out 3,390 · $0.064417 | success yes · 154.1s · in 870,761 · out 5,759 · $0.544209 | success yes · 95.3s · in 395,437 · out 3,565 · $0.147768 |
| 17 | `django__django-16485` | success yes · 65.6s · in 261,382 · out 3,955 · $0.203777 | success yes · 81.0s · in 208,372 · out 2,721 · $0.062036 | success yes · 148.7s · in 871,648 · out 9,620 · $0.583199 | success yes · 81.6s · in 499,631 · out 2,881 · $0.161227 | success yes · 121.7s · in 473,098 · out 6,105 · $0.364905 | success yes · 51.0s · in 113,056 · out 1,942 · $0.037156 |
| 18 | `django__django-16877` | success no · 238.2s · in 561,884 · out 7,109 · $0.385894 | success yes · 194.9s · in 2,066,959 · out 7,687 · $0.457526 | success yes · 239.7s · in 812,224 · out 7,310 · $0.521952 | success yes · 193.2s · in 639,666 · out 8,357 · $0.155741 | success yes · 227.5s · in 599,657 · out 5,978 · $0.413625 | success no · 150.2s · in 807,754 · out 6,028 · $0.210463 |
| 19 | `django__django-11451` | success yes · 70.5s · in 236,756 · out 2,905 · $0.191766 | success yes · 83.9s · in 245,732 · out 3,356 · $0.072102 | success yes · 61.5s · in 422,912 · out 2,666 · $0.308905 | success yes · 53.4s · in 148,932 · out 1,848 · $0.046433 | success yes · 71.3s · in 408,430 · out 3,620 · $0.312528 | success yes · 75.8s · in 272,514 · out 2,957 · $0.069302 |
| 20 | `pytest-dev__pytest-7205` | success yes · 230.1s · in 524,459 · out 6,610 · $0.376218 | success yes · 163.1s · in 765,767 · out 6,957 · $0.202527 | success yes · 241.1s · in 840,833 · out 8,196 · $0.550277 | success yes · 149.6s · in 760,024 · out 6,478 · $0.167596 | success yes · 393.9s · in 927,658 · out 9,823 · $0.624548 | success yes · 179.9s · in 1,167,093 · out 7,495 · $0.247839 |

### Tool use by case and condition

#### A and original T

| Rank | A-Sonnet | A-Luna | T-Sonnet | T-Luna |
|---:|---|---|---|---|
| 1 | 13 (Bash=8, Edit=1, Grep=2, Read=2) | 9 (commandExecution=9) | 9 (Bash=6, Edit=1, Read=2) | 7 (commandExecution=6, fileChange=1) |
| 2 | 29 (Bash=17, Edit=6, Read=6) | 11 (commandExecution=10, fileChange=1) | 24 (Bash=16, Edit=3, Grep=1, Read=4) | 11 (commandExecution=10, fileChange=1) |
| 3 | 20 (Bash=15, Edit=1, Grep=2, Read=2) | 30 (commandExecution=28, fileChange=2) | 39 (Bash=29, Edit=4, Grep=1, Read=5) | 16 (commandExecution=14, fileChange=2) |
| 4 | 34 (Bash=14, Edit=7, Grep=4, Read=9) | 14 (commandExecution=10, fileChange=1, webSearch=3) | 22 (Bash=16, Edit=6) | 19 (commandExecution=15, fileChange=4) |
| 5 | 34 (Bash=14, Edit=14, Grep=1, Read=5) | 17 (commandExecution=12, fileChange=2, webSearch=3) | 33 (Bash=20, Edit=8, Read=5) | 25 (commandExecution=16, fileChange=3, webSearch=6) |
| 6 | 43 (Bash=24, Edit=8, Grep=5, Read=6) | 16 (commandExecution=10, fileChange=1, webSearch=5) | 38 (Bash=26, Edit=6, Grep=1, Read=5) | 18 (commandExecution=13, fileChange=2, webSearch=3) |
| 7 | 74 (Bash=62, Edit=6, Grep=1, Read=5) | 33 (commandExecution=27, fileChange=3, webSearch=3) | 30 (Bash=18, Edit=7, Glob=1, Read=4) | 34 (commandExecution=26, fileChange=2, imageView=2, webSearch=4) |
| 8 | 24 (Bash=15, Edit=3, Grep=3, Read=3) | 8 (commandExecution=7, fileChange=1) | 11 (Bash=7, Edit=2, Read=2) | 6 (commandExecution=5, fileChange=1) |
| 9 | 16 (Bash=7, Edit=2, Grep=3, Read=4) | 10 (commandExecution=9, fileChange=1) | 10 (Bash=5, Edit=2, Read=3) | 10 (commandExecution=9, fileChange=1) |
| 10 | 21 (Bash=15, Edit=3, Grep=3) | 16 (commandExecution=11, fileChange=2, webSearch=3) | 7 (Bash=4, Edit=1, Read=2) | 15 (commandExecution=13, fileChange=2) |
| 11 | 11 (Bash=6, Edit=1, Grep=4) | 16 (commandExecution=14, fileChange=2) | 11 (Bash=7, Edit=1, Read=3) | 12 (commandExecution=10, fileChange=2) |
| 12 | 37 (Bash=23, Edit=3, Grep=6, Read=5) | 23 (commandExecution=18, fileChange=1, webSearch=4) | 29 (Bash=19, Edit=2, Grep=5, Read=3) | 20 (commandExecution=10, fileChange=1, webSearch=9) |
| 13 | 21 (Bash=10, Edit=4, Grep=4, Read=3) | 12 (commandExecution=10, fileChange=2) | 22 (Bash=12, Edit=4, Grep=1, Read=5) | 11 (commandExecution=9, fileChange=1, webSearch=1) |
| 14 | 16 (Bash=8, Edit=2, Grep=3, Read=2, Write=1) | 12 (commandExecution=10, fileChange=2) | 22 (Bash=13, Edit=3, Grep=3, Read=2, Write=1) | 11 (commandExecution=10, fileChange=1) |
| 15 | 9 (Bash=5, Edit=1, Read=2, Write=1) | 17 (commandExecution=13, fileChange=4) | 22 (Bash=13, Edit=4, Grep=1, Read=3, Write=1) | 22 (commandExecution=20, fileChange=2) |
| 16 | 49 (Bash=42, Edit=2, Grep=1, Read=3, Write=1) | 17 (commandExecution=13, fileChange=1, webSearch=3) | 11 (Bash=7, Edit=1, Read=3) | 10 (commandExecution=9, fileChange=1) |
| 17 | 18 (Bash=9, Edit=4, Grep=2, Read=3) | 10 (commandExecution=9, fileChange=1) | 32 (Bash=24, Edit=6, Read=2) | 12 (commandExecution=8, fileChange=1, webSearch=3) |
| 18 | 30 (Bash=18, Edit=5, Grep=1, Read=4, Write=2) | 31 (commandExecution=18, fileChange=2, webSearch=11) | 29 (Bash=14, Edit=4, Grep=1, Read=9, Write=1) | 17 (commandExecution=14, fileChange=3) |
| 19 | 14 (Bash=8, Edit=2, Grep=1, Read=3) | 11 (commandExecution=10, fileChange=1) | 15 (Bash=10, Edit=2, Read=3) | 6 (commandExecution=5, fileChange=1) |
| 20 | 26 (Bash=13, Edit=6, Grep=1, Read=4, Write=2) | 24 (commandExecution=17, fileChange=4, webSearch=3) | 29 (Bash=15, Edit=7, Read=6, Write=1) | 21 (commandExecution=16, fileChange=5) |

#### T2

| Rank | T2-Sonnet | T2-Luna |
|---:|---|---|
| 1 | 13 (Bash=10, Edit=1, Read=2) | 8 (commandExecution=6, fileChange=2) |
| 2 | 11 (Bash=8, Edit=1, Read=2) | 6 (commandExecution=5, fileChange=1) |
| 3 | 25 (Bash=16, Edit=1, Grep=3, Read=5) | 19 (commandExecution=16, fileChange=3) |
| 4 | 41 (Bash=26, Edit=4, Grep=2, Read=9) | 18 (commandExecution=12, fileChange=1, webSearch=5) |
| 5 | 37 (Bash=20, Edit=10, Read=7) | 18 (commandExecution=16, fileChange=2) |
| 6 | 30 (Bash=16, Edit=5, Glob=1, Grep=4, Read=4) | 16 (commandExecution=11, fileChange=2, webSearch=3) |
| 7 | 43 (Bash=33, Edit=5, Read=5) | 38 (commandExecution=21, fileChange=4, webSearch=13) |
| 8 | 19 (Bash=10, Edit=3, Grep=2, Read=4) | 6 (commandExecution=5, fileChange=1) |
| 9 | 29 (Bash=21, Edit=3, Read=5) | 13 (commandExecution=11, fileChange=2) |
| 10 | 23 (Bash=11, Edit=7, Read=5) | 16 (commandExecution=15, fileChange=1) |
| 11 | 8 (Bash=5, Edit=1, Read=2) | 10 (commandExecution=9, fileChange=1) |
| 12 | 68 (Bash=56, Edit=4, Grep=4, Read=4) | 23 (commandExecution=15, fileChange=2, webSearch=6) |
| 13 | 16 (Bash=11, Edit=2, Read=3) | 11 (commandExecution=9, fileChange=2) |
| 14 | 12 (Bash=4, Edit=2, Grep=3, Read=2, Write=1) | 13 (commandExecution=12, fileChange=1) |
| 15 | 16 (Bash=11, Edit=1, Read=2, Write=2) | 12 (commandExecution=9, fileChange=3) |
| 16 | 28 (Bash=25, Edit=1, Read=2) | 13 (commandExecution=9, fileChange=1, webSearch=3) |
| 17 | 21 (Bash=15, Edit=3, Read=3) | 5 (commandExecution=4, fileChange=1) |
| 18 | 24 (Bash=12, Edit=4, Grep=3, Read=4, Write=1) | 23 (commandExecution=17, fileChange=2, webSearch=4) |
| 19 | 16 (Bash=10, Edit=3, Read=3) | 12 (commandExecution=10, fileChange=2) |
| 20 | 30 (Bash=17, Edit=7, Read=5, Write=1) | 24 (commandExecution=19, fileChange=5) |

## Within-model A → T effects

Both Sonnet A→T and Luna A→T are controlled comparisons under their final clean, arm-symmetric runtimes. Cross-model Sonnet/Luna totals are descriptive because provider-native runtime and tool scaffolding differ.

| Backend | Metric | A total | T total | Aggregate change | Median paired change | T lower |
|---|---|---:|---:|---:|---:|---:|
| Sonnet | Input tokens | 1.2737e+07 | 1.31191e+07 | +3.0% | +34.1% | 5/20 |
| Sonnet | Output tokens | 146071 | 131649 | -9.9% | -10.6% | 11/20 |
| Sonnet | Total tokens | 1.28831e+07 | 1.32508e+07 | +2.9% | +33.3% | 5/20 |
| Sonnet | Elapsed seconds | 4025.18 | 3374.4 | -16.2% | -20.3% | 12/20 |
| Sonnet | Cost (USD) | 8.54334 | 9.20291 | +7.7% | +34.2% | 5/20 |
| Sonnet | Tool calls | 539 | 445 | -17.4% | -7.5% | 12/20 |
| Luna | Input tokens | 1.29263e+07 | 1.32396e+07 | +2.4% | +3.4% | 9/20 |
| Luna | Output tokens | 91065 | 97953 | +7.6% | -2.1% | 12/20 |
| Luna | Total tokens | 1.30174e+07 | 1.33376e+07 | +2.5% | +3.2% | 9/20 |
| Luna | Elapsed seconds | 2463.71 | 2571.48 | +4.4% | -0.1% | 10/20 |
| Luna | Cost (USD) | 3.15154 | 3.10509 | -1.5% | +3.2% | 9/20 |
| Luna | Tool calls | 337 | 303 | -10.1% | -8.3% | 12/20 |

### Efficacy

- Sonnet: 20 evaluated pairs; A 17 resolved vs T 18 resolved (+5.0 percentage points); T-only wins 2, A-only wins 1, same verdict 17.
- Luna: 20 evaluated pairs; A 18 resolved vs T 19 resolved (+5.0 percentage points); T-only wins 2, A-only wins 1, same verdict 17.

### Aggregate tool mix

- Sonnet: Bash A=333, T=281 (-52), Edit A=81, T=74 (-7), Glob A=0, T=1 (+1), Grep A=47, T=14 (-33), Read A=71, T=71 (+0), Write A=7, T=4 (-3).
- Luna: commandExecution A=265, T=238 (-27), fileChange A=34, T=37 (+3), imageView A=0, T=2 (+2), webSearch A=38, T=26 (-12).

## Transcript audit disclosures

- 80/80 canonical transcripts passed structural parsing and independent tool recounting.
- App Server recorded 1 null command outputs across 1 Luna cells; they remain in the efficiency data and are disclosed, not filtered post hoc.
- 0 started items lacked completion records across 0 Luna cells; the receipt-level reconciliation remained auditable and the cells are retained.
- Explicit retained execution adjudications: rank 12 T_luna.
- Web-search exposures under the unrestricted frozen runtime: rank 4 A_luna=3, rank 5 A_luna=3, rank 5 T_luna=6, rank 6 A_luna=5, rank 6 T_luna=3, rank 7 A_luna=3, rank 7 T_luna=4, rank 10 A_luna=3, rank 12 A_luna=4, rank 12 T_luna=9, rank 13 T_luna=1, rank 16 A_luna=3, rank 17 T_luna=3, rank 18 A_luna=11, rank 20 A_luna=3.
- Direct-solution exposures under that same symmetric unrestricted contract: rank 4 A_luna=3, rank 5 A_luna=3, rank 5 T_luna=6, rank 6 A_luna=5, rank 6 T_luna=3, rank 7 A_luna=3, rank 7 T_luna=7, rank 10 A_luna=3, rank 12 A_luna=6, rank 12 T_luna=9, rank 13 T_luna=1, rank 16 A_luna=3, rank 17 T_luna=3, rank 18 A_luna=11, rank 20 A_luna=3; retained and disclosed rather than post-hoc filtered.
- Tool types independently recovered from transcripts but omitted by provider receipts: {'imageView': 2}; report totals use the independent recount.
- Luna runtime audit status: VALID; 0 cells require clean unrestricted reruns before the Luna aggregate is eligible for the final within-model controlled comparison.

## Treatment-context quality

- All 20 final Luna T injections are auditable: 16 clean-runtime rerun injections plus 4 retained, strictly audited injections (ranks 10, 12, 14, and 17). The deterministic graph seed file overlaps the treatment patch in 15 cells.
- Seed/patch overlap is only a coarse relevance proxy, not an inclusion filter. Non-overlap cases remain in the experiment and are inspected as possible RNA retrieval failures.
- Current non-overlap cases: rank 4 seed `django/contrib/gis/utils/layermapping.py` vs patch django/db/models/sql/compiler.py, tests/ordering/models.py, tests/ordering/tests.py; rank 6 seed `django/db/models/sql/query.py` vs patch django/template/defaultfilters.py, tests/template_tests/filter_tests/test_add.py; rank 13 seed `sympy/physics/vector/dyadic.py` vs patch sympy/printing/latex.py, sympy/printing/tests/test_latex.py; rank 19 seed `django/contrib/auth/__init__.py` vs patch django/contrib/auth/backends.py, tests/auth_tests/test_auth_backends.py; rank 20 seed `src/_pytest/runner.py` vs patch src/_pytest/setuponly.py, testing/test_setuponly.py.
- Rank 6 is a confirmed RNA retrieval/seed/context failure, not a runtime or harness failure: title retrieval returned a noisy 50-record result with some related template/filter records but missed `django/template/defaultfilters.py:add`; the deterministic `django/db/models/sql/query.py:add_filter` seed led to an unrelated ORM graph. A resolved and T was unresolved.

## Evaluation and cost notes

- All 80 canonical cells have completed stock SWE-bench 4.1.0 exact-patch verdicts bound to their terminal-patch hashes.
- Retained Luna efficacy verdicts are reused only when their exact terminal-patch hash matches the source verdict.
- Luna cost uses published OpenAI API-equivalent rates; Sonnet cost uses provider receipt totals.
- The evaluator plan contains exactly 77 targets: 40 canonical Sonnet episodes, three retained Luna T replacements, and all 34 predeclared clean-runtime Luna reruns.
- Treatment is deterministic pre-injected RNA context: the canonical prompt contains the RNA query exposure plus compact title result and bounded two-hop graph result. Follow-up RNA calls are not required by the hypothesis and are not added to model tool-call counts.
- The injected RNA exposure is reported separately from model-initiated tools. It was needed because prior harness rules/model tuning resisted voluntary RNA use despite system guidance; conditioning attempts preceding the frozen benchmark are provenance, not extra treatment tool calls.
- Luna T rank 17 passed its independent strict audit 53/53 and its final project tests 10/10. Disclosed transcript events: a recovered wrong initial test command, a rejected `rm -f` followed by successful Python cleanup, two raw pending call records reconciled to zero effective null outputs, and three web searches permitted by the frozen unrestricted runtime contract.

## Harness and runtime repairs

- Prior attempts affected by unnecessary timeouts, restrictive wrappers, or foreign-checkout/PYTHONPATH contamination are repair provenance only; they are not canonical experimental evidence.
- The final Luna cells use cell-bounded clean runtimes with unrestricted network/web access applied symmetrically to A and T.
- Prompt construction was repaired and byte-validated so A matches A and T matches T across Sonnet and Luna; the treatment-only RNA directive and injected context remain the intended arm difference.

## Missing or invalid inputs

- None.

The canonical matrix and audit sections were generated by
`build_canonical_report.py` with zero model calls. The explanatory
decomposition above is recomputed by `verify_results.py` from the checked-in
path-free ledger.

## Faithful unfiltered T2 result

| Backend | A resolved | Original T resolved | T2 resolved | Tokens A→T2 | Time A→T2 | Cost A→T2 | Tools A→T2 |
|---|---:|---:|---:|---:|---:|---:|---:|
| Sonnet | 17/20 | 18/20 | **17/20** | +19.8% | -7.8% | +17.9% | -5.4% |
| Luna | 18/20 | 19/20 | **17/20** | -0.4% | -3.0% | +1.7% | -9.8% |

T2 is additive, post-hoc evidence, not a rewrite of the original T condition or
a retroactive preregistered result. Within-model A→T2 comparisons reuse the frozen
completed A episodes and compare them to newly executed T2 episodes.

T2 resolved 17/20 cases on each backend. On Sonnet that equals A but trades two
case-level wins for two losses, and is one resolution below original T. On Luna it
is one resolution below A and two below original T. The faithful unfiltered graph
therefore did not improve aggregate efficacy in this single-run additive cohort,
despite modest aggregate time/tool savings relative to A.

### T2 input and output decomposition

| Backend | Condition | Cached input | Uncached input | Output | Reasoning subset | Non-reasoning output |
|---|---|---:|---:|---:|---:|---:|
| Sonnet | A | 12,273,988 | 463,028 | 146,071 | 0 | 146,071 |
| Sonnet | Original T | 12,391,901 | 727,235 | 131,649 | 0 | 131,649 |
| Sonnet | T2 | 14,577,901 | 722,549 | 137,374 | 0 | 137,374 |
| Luna | A | 11,890,176 | 1,036,130 | 91,065 | 44,568 | 46,497 |
| Luna | Original T | 12,202,496 | 1,037,119 | 97,953 | 53,086 | 44,867 |
| Luna | T2 | 11,731,712 | 1,146,736 | 90,955 | 44,732 | 46,223 |

Sonnet T2's +19.8% total-token change versus A is primarily repeated cached
context processing: cached input rises by 2.30M tokens, while inclusive uncached
input rises by 259.5K and output falls by 8.7K. Relative to original T, T2's
+16.5% total-token change is likewise trajectory-driven cached replay: cached input
rises by 2.19M while inclusive uncached input is 4.7K lower. T2 made 65 more
Sonnet tool calls than original T, so the smaller average T2 injection was replayed
over more turns. Luna T2, by contrast, processed 2.8% fewer total tokens than
original T and 0.4% fewer than A; its non-reasoning output is essentially unchanged
from A.

T2 changes only the graph traversal: `mode=neighbors`, `hops=2`,
`direction=both`, with **no edge-type filter**. Typed edge labels are retained,
and deterministic whole-record and 32 KiB prompt bounds are applied after
traversal.

### Within-model T2 comparisons

| Backend | Baseline | Metric | Baseline total | T2 total | Aggregate change | Median paired change | T2 lower |
|---|---|---|---:|---:|---:|---:|---:|
| Sonnet | A | Total tokens | 1.28831e+07 | 1.54378e+07 | +19.8% | +66.1% | 6/20 |
| Sonnet | A | Input tokens | 1.2737e+07 | 1.53004e+07 | +20.1% | +66.6% | 6/20 |
| Sonnet | A | Output tokens | 146071 | 137374 | -6.0% | +6.4% | 8/20 |
| Sonnet | A | Elapsed seconds | 4025.18 | 3712.96 | -7.8% | +2.9% | 9/20 |
| Sonnet | A | Cost USD | 8.54334 | 10.0733 | +17.9% | +49.8% | 6/20 |
| Sonnet | A | Tool calls | 539 | 510 | -5.4% | +4.4% | 9/20 |
| Sonnet | Original T | Total tokens | 1.32508e+07 | 1.54378e+07 | +16.5% | +3.6% | 10/20 |
| Sonnet | Original T | Input tokens | 1.31191e+07 | 1.53004e+07 | +16.6% | +3.5% | 10/20 |
| Sonnet | Original T | Output tokens | 131649 | 137374 | +4.3% | +16.3% | 9/20 |
| Sonnet | Original T | Elapsed seconds | 3374.4 | 3712.96 | +10.0% | -5.3% | 11/20 |
| Sonnet | Original T | Cost USD | 9.20291 | 10.0733 | +9.5% | +5.1% | 9/20 |
| Sonnet | Original T | Tool calls | 445 | 510 | +14.6% | +5.1% | 9/20 |
| Luna | A | Total tokens | 1.30174e+07 | 1.29694e+07 | -0.4% | +7.3% | 9/20 |
| Luna | A | Input tokens | 1.29263e+07 | 1.28784e+07 | -0.4% | +7.6% | 9/20 |
| Luna | A | Output tokens | 91065 | 90955 | -0.1% | -3.4% | 11/20 |
| Luna | A | Elapsed seconds | 2463.71 | 2388.91 | -3.0% | -4.4% | 11/20 |
| Luna | A | Cost USD | 3.15154 | 3.20564 | +1.7% | -1.5% | 10/20 |
| Luna | A | Tool calls | 337 | 304 | -9.8% | -4.2% | 10/20 |
| Luna | Original T | Total tokens | 1.33376e+07 | 1.29694e+07 | -2.8% | +13.6% | 8/20 |
| Luna | Original T | Input tokens | 1.32396e+07 | 1.28784e+07 | -2.7% | +13.7% | 8/20 |
| Luna | Original T | Output tokens | 97953 | 90955 | -7.1% | +4.9% | 9/20 |
| Luna | Original T | Elapsed seconds | 2571.48 | 2388.91 | -7.1% | -2.1% | 10/20 |
| Luna | Original T | Cost USD | 3.10509 | 3.20564 | +3.2% | +12.9% | 7/20 |
| Luna | Original T | Tool calls | 303 | 304 | +0.3% | +9.2% | 7/20 |

### T2 prompt and graph audit

| Rank | Query | Traversal root | Whole-record limit | Prompt bytes | Graph bytes |
|---:|---|---|---:|---:|---:|
| 1 | `title-plus-512` | `sympy/core/symbol.py:symbols:function` | 10 | 14,833 | 3,341 |
| 2 | `title-plus-512` | `django/db/models/deletion.py:delete:function` | 20 | 24,120 | 11,723 |
| 3 | `title-plus-512` | `sympy/polys/polytools.py:poly:function` | 10 | 20,271 | 8,962 |
| 4 | `title-plus-512` | `django/contrib/contenttypes/fields.py:GenericRelation:struct` | 10 | 25,015 | 7,843 |
| 5 | `title-plus-512` | `xarray/core/weighted.py:_weighted_mean:function` | 2 | 20,020 | 1,389 |
| 6 | `title-plus-512` | `django/db/models/sql/query.py:add_filter:function` | 5 | 18,192 | 4,641 |
| 7 | `title-plus-512` | `lib/matplotlib/stackplot.py:stackplot:function` | 5 | 17,501 | 2,218 |
| 8 | `title-plus-512` | `django/forms/models.py:model_to_dict:function` | 5 | 17,277 | 4,721 |
| 9 | `title-plus-512` | `django/contrib/admin/sites.py:catch_all_view:function` | 10 | 24,685 | 8,115 |
| 10 | `title-plus-512` | `django/contrib/admin/checks.py:_check_list_display_links:function` | 2 | 19,824 | 1,386 |
| 11 | `title-plus-512` | `django/core/management/__init__.py:ManagementUtility:struct` | 5 | 21,887 | 8,406 |
| 12 | `title-only` | `psf-requests-1724:requests/models.py:prepare_url:function` | 10 | 25,075 | 6,158 |
| 13 | `title-plus-512` | `sympy/physics/vector/dyadic.py:subs:function` | 1 | 11,748 | 496 |
| 14 | `title-plus-512` | `pytest-dev-pytest-7982:testing/test_collection.py:test_collect_symlink_file_arg:function` | 1 | 2,514 | 724 |
| 15 | `title-plus-512` | `src/_pytest/skipping.py:pytest_runtest_setup:function` | 5 | 17,243 | 3,817 |
| 16 | `title-plus-512` | `django/forms/fields.py:BooleanField:struct` | 2 | 14,877 | 1,397 |
| 17 | `title-plus-512` | `django-django-16485:django/template/defaultfilters.py:floatformat:function` | 10 | 5,682 | 4,054 |
| 18 | `title-plus-512` | `django/template/defaultfilters.py:safeseq:function` | 5 | 18,273 | 3,309 |
| 19 | `title-plus-512` | `django/contrib/auth/backends.py:ModelBackend:struct` | 10 | 20,597 | 5,766 |
| 20 | `title-plus-512` | `src/_pytest/runner.py:from_call:function` | 10 | 22,082 | 4,967 |

All 20 selected T2 prompts passed global review before their canonical provider
episodes. Rank 20 was regenerated and re-reviewed before its replacement episodes
after the original projection exposed a harness parser defect. The title
query and compact result are byte-identical to the audited original T exposure;
the original calls-only graph was not reused. Each T2 graph was freshly projected
from the warm exact-tree cache without `edge_types`, retained per-record typed edge
labels, used one of the whole-record limits 20/10/5/2/1, and kept the complete user
prompt at or below 32,768 bytes. Rank 12 alone used the deterministic title-only
fallback; all other ranks used title plus the normalized 512-character body prefix.

### T2 transcript disclosures

- 40/40 T2 transcripts passed structural parsing, prompt and patch hashing, exact
  model/context checks, item reconciliation, independent tool recounting, and
  foreign-rank/checkout reference checks.
- Luna recorded 6 null command outputs across 5 cells; all remain in the data.
- No Luna item was started without a corresponding completion record.
- Every T2 cell had one deterministic pre-injected RNA exposure (40 total).
  The selected canonical cells made 0 model-initiated follow-up RNA calls.
  Follow-up was not required by the context-efficacy hypothesis; injected exposure
  is reported separately from ordinary model tool calls.
- Luna rank 6 emitted a 416,382-character command result because the model ran
  `git show --stat --oneline HEAD` on a root/shallow commit. This was a model-chosen
  command, not harness-injected duplication, so it remains in efficiency metrics.
- Network and web-search behavior remained unrestricted and is retained rather than
  filtered post hoc. Direct-solution exposure counts are present in `t2-results.json`.
- Manual rank-20 transcript review found no foreign checkout or direct-solution
  exposure. Sonnet corrected an initial `/repo` guess, its `pip` lookup failed, and
  a model-chosen root-wide `find` moved to Claude's background handling. Luna's local
  `git log -S` search returned no matching solution commit. All resulting elapsed
  time, tokens, and tool calls remain in the canonical metrics.

### T2 evaluation and accounting

Every T2 terminal patch was evaluated by stock SWE-bench 4.1.0, bound to the exact
instance ID and terminal-patch SHA-256. Sonnet cost is the provider-receipt sum.
Luna cost is the same published API-equivalent estimate used by the original report:
$1.00/M uncached input, $0.10/M cached input, $1.25/M cache write, $6.00/M
output, and $0.01 per web search; it is not a claim about App Server billing.
