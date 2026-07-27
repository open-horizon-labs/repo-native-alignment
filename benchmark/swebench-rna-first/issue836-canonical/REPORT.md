# Canonical 20-case × 4-condition report

Status: **COMPLETE**. Ready episodes: **80/80**; officially evaluated: **80/80**.

Every available cell passed exact prompt/base/directive hashes, checkout identity, transcript parsing, independent tool recounting, token extraction, and cost cross-checking. Missing or invalid cells are not replaced by historical attempts.

Read [METHOD.md](METHOD.md) for the treatment construction, canonicalization
rules, runtime contracts, and limitations. The most important result is not a
raw token reduction. T resolved one additional case on each model and used
fewer tools, while processing slightly more total input context.

## Executive result

| Backend | A resolved | T resolved | Total tokens A→T | Time A→T | Cost A→T | Tools A→T |
|---|---:|---:|---:|---:|---:|---:|
| Sonnet | 17/20 | **18/20** | +2.9% | **−16.2%** | +7.7% | **−17.4%** |
| Luna | 18/20 | **19/20** | +2.5% | +4.4% | **−1.5%** | **−10.1%** |

For each model, there were two T-only resolutions, one A-only resolution, and
17 equal verdicts. Thus the five-percentage-point change is one net case, not
a precise estimate of a population effect. It is directionally encouraging
and was observed on both backends, but the sample remains small.

The raw token-reduction hypothesis is not supported. The observed operational
trade is larger pre-injected context for fewer interactions and slightly more
successful patches.

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

## Full A–A–T–T matrix

| Rank | Case | A-Sonnet | A-Luna | T-Sonnet | T-Luna |
|---:|---|---|---|---|---|
| 1 | `sympy__sympy-23534` | success yes · 199.6s · in 179,511 · out 2,705 · $0.195187 | success yes · 74.5s · in 201,307 · out 2,769 · $0.082907 | success yes · 61.4s · in 234,761 · out 1,938 · $0.259946 | success yes · 53.5s · in 205,312 · out 2,087 · $0.062084 |
| 2 | `django__django-11179` | success yes · 233.9s · in 527,787 · out 7,214 · $0.379073 | success yes · 66.0s · in 203,084 · out 2,250 · $0.055304 | success yes · 215.4s · in 662,547 · out 4,879 · $0.423247 | success yes · 70.5s · in 292,042 · out 2,221 · $0.072203 |
| 3 | `sympy__sympy-13757` | success yes · 256.0s · in 363,595 · out 5,589 · $0.293556 | success yes · 205.7s · in 1,359,660 · out 7,706 · $0.248827 | success yes · 318.0s · in 1,488,908 · out 12,258 · $0.897869 | success yes · 133.7s · in 654,825 · out 5,419 · $0.154654 |
| 4 | `django__django-13033` | success yes · 154.2s · in 850,393 · out 10,768 · $0.594492 | success yes · 79.0s · in 562,223 · out 2,537 · $0.146184 | success yes · 111.4s · in 657,060 · out 7,989 · $0.489729 | success yes · 198.5s · in 649,447 · out 8,693 · $0.162930 |
| 5 | `pydata__xarray-4075` | success yes · 275.1s · in 840,923 · out 11,149 · $0.598495 | success yes · 175.5s · in 543,974 · out 7,692 · $0.201489 | success yes · 279.7s · in 1,154,953 · out 9,480 · $0.722487 | success yes · 188.3s · in 1,384,232 · out 7,627 · $0.330621 |
| 6 | `django__django-13794` | success yes · 236.7s · in 1,116,311 · out 15,847 · $0.784036 | success yes · 112.7s · in 831,481 · out 3,749 · $0.213005 | success no · 417.9s · in 1,249,797 · out 16,085 · $0.846995 | success no · 163.9s · in 557,501 · out 7,150 · $0.173979 |
| 7 | `matplotlib__matplotlib-24026` | success yes · 747.5s · in 3,591,313 · out 24,186 · $1.921416 | success yes · 283.5s · in 2,215,343 · out 9,401 · $0.401640 | success yes · 308.6s · in 951,062 · out 10,986 · $0.684515 | success yes · 258.5s · in 3,061,976 · out 9,132 · $0.513850 |
| 8 | `django__django-11163` | success yes · 106.9s · in 351,726 · out 5,049 · $0.249018 | success yes · 47.0s · in 109,306 · out 1,521 · $0.037792 | success yes · 44.0s · in 262,041 · out 2,412 · $0.213630 | success yes · 33.5s · in 108,679 · out 965 · $0.034751 |
| 9 | `django__django-16612` | success yes · 71.8s · in 221,086 · out 3,049 · $0.171232 | success yes · 59.3s · in 154,699 · out 2,017 · $0.051140 | success yes · 50.8s · in 262,866 · out 2,653 · $0.243282 | success yes · 67.0s · in 258,851 · out 2,427 · $0.066744 |
| 10 | `django__django-11551` | success yes · 101.1s · in 403,685 · out 5,746 · $0.314144 | success yes · 104.8s · in 425,444 · out 4,010 · $0.156944 | success yes · 33.8s · in 168,235 · out 2,127 · $0.185380 | success yes · 145.1s · in 420,972 · out 4,934 · $0.108202 |
| 11 | `django__django-13658` | success yes · 89.6s · in 125,332 · out 1,797 · $0.095596 | success yes · 161.2s · in 586,597 · out 4,419 · $0.117981 | success yes · 228.9s · in 299,931 · out 2,777 · $0.252531 | success yes · 194.1s · in 640,999 · out 3,970 · $0.125222 |
| 12 | `psf__requests-1724` | success yes · 275.1s · in 841,053 · out 9,463 · $0.560140 | success no · 154.9s · in 878,616 · out 6,527 · $0.221650 | success yes · 152.1s · in 846,192 · out 7,758 · $0.572233 | success yes · 145.7s · in 1,383,586 · out 5,700 · $0.346800 |
| 13 | `sympy__sympy-18763` | success no · 241.4s · in 311,170 · out 5,642 · $0.251346 | success no · 88.7s · in 294,302 · out 3,391 · $0.087243 | success no · 98.0s · in 586,478 · out 6,034 · $0.411436 | success yes · 87.6s · in 308,651 · out 3,025 · $0.087508 |
| 14 | `pytest-dev__pytest-7982` | success yes · 170.2s · in 202,688 · out 2,844 · $0.149304 | success yes · 69.4s · in 272,988 · out 2,992 · $0.077359 | success yes · 90.2s · in 395,815 · out 6,667 · $0.321930 | success yes · 77.5s · in 222,917 · out 2,994 · $0.083057 |
| 15 | `pytest-dev__pytest-7432` | success yes · 48.4s · in 140,987 · out 3,522 · $0.150536 | success yes · 136.8s · in 487,020 · out 5,384 · $0.111516 | success yes · 228.3s · in 655,393 · out 6,797 · $0.465514 | success yes · 194.6s · in 823,804 · out 8,655 · $0.187068 |
| 16 | `django__django-12193` | success no · 213.4s · in 1,084,975 · out 10,922 · $0.678113 | success yes · 121.9s · in 513,432 · out 3,979 · $0.146365 | success yes · 45.0s · in 295,480 · out 3,017 · $0.247851 | success yes · 81.6s · in 217,568 · out 3,390 · $0.064417 |
| 17 | `django__django-16485` | success yes · 65.6s · in 261,382 · out 3,955 · $0.203777 | success yes · 81.0s · in 208,372 · out 2,721 · $0.062036 | success yes · 148.7s · in 871,648 · out 9,620 · $0.583199 | success yes · 81.6s · in 499,631 · out 2,881 · $0.161227 |
| 18 | `django__django-16877` | success no · 238.2s · in 561,884 · out 7,109 · $0.385894 | success yes · 194.9s · in 2,066,959 · out 7,687 · $0.457526 | success yes · 239.7s · in 812,224 · out 7,310 · $0.521952 | success yes · 193.2s · in 639,666 · out 8,357 · $0.155741 |
| 19 | `django__django-11451` | success yes · 70.5s · in 236,756 · out 2,905 · $0.191766 | success yes · 83.9s · in 245,732 · out 3,356 · $0.072102 | success yes · 61.5s · in 422,912 · out 2,666 · $0.308905 | success yes · 53.4s · in 148,932 · out 1,848 · $0.046433 |
| 20 | `pytest-dev__pytest-7205` | success yes · 230.1s · in 524,459 · out 6,610 · $0.376218 | success yes · 163.1s · in 765,767 · out 6,957 · $0.202527 | success yes · 241.1s · in 840,833 · out 8,196 · $0.550277 | success yes · 149.6s · in 760,024 · out 6,478 · $0.167596 |

## Tool calls by type

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
