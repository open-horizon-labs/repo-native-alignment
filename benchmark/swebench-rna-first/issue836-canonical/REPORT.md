# Canonical 20-case × 12-condition report

Status: **COMPLETE**. Canonical cells: **240/240**; officially evaluated: **240/240**.

This is the single reader-facing report for all 12 conditions: control A,
original calls-only treatment T, and faithful unfiltered typed-graph treatment
T2 on Claude Sonnet 5, GPT-5.6 Luna, Claude Haiku 4.5, and GPT-5.3 Codex
Spark. The original 80-cell A/T evidence remains intact; the additive T2
execution contributes 40 Sonnet/Luna cells, and the weaker-model sensitivity
execution contributes 120 Haiku/Spark cells.

The Sonnet/Luna T2 extension executed 42 paid provider episodes: two original
rank-20 cells were superseded after a whole-record projection parser defect was
found, and their two audited replacements are canonical. T2 resolved 17/20
cases on both Sonnet and Luna. The separate weaker-model sensitivity execution
contributed 120 canonical Haiku/Spark episodes with no paid reruns.

Every canonical cell passed exact prompt/base/directive hashes, checkout
identity, transcript parsing, independent tool recounting, token extraction,
and cost cross-checking. Missing or invalid cells are not replaced by
historical attempts.

Read [METHOD.md](METHOD.md) for treatment construction, canonicalization rules,
runtime contracts, and limitations. On Sonnet and Luna, the original T
treatment resolved one additional case per backend and used fewer tools while
processing slightly more total input context; T2 did not improve efficacy on
those two backends. The weaker-model results are reported separately below.

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
