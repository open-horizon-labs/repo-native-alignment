#!/usr/bin/env python3
"""Aggregate benchmark scores and efficiency metrics."""
import json, glob, sys, os

bench_dir = sys.argv[1] if len(sys.argv) > 1 else "benchmark/results"
scores_dir = os.path.join(bench_dir, "scores")

conditions = {
    "vanilla": {"costs": [], "times": [], "scores": []},
    "rna": {"costs": [], "times": [], "scores": []},
    "rna-mcp": {"costs": [], "times": [], "scores": []},
}

# Collect efficiency metrics from run files
for cond in conditions:
    for f in sorted(glob.glob(os.path.join(bench_dir, cond, "v3-run-*.json"))):
        try:
            d = json.load(open(f))
            conditions[cond]["costs"].append(d["total_cost_usd"])
            conditions[cond]["times"].append(d["duration_ms"] / 1000)
        except:
            pass

# Collect quality scores
for f in sorted(glob.glob(os.path.join(scores_dir, "*.json"))):
    try:
        d = json.load(open(f))
        result = d.get("result", "")
        start = result.find("{")
        end = result.rfind("}") + 1
        if start >= 0 and end > start:
            scores = json.loads(result[start:end])
        else:
            continue
    except:
        continue

    name = os.path.basename(f)
    for cond in conditions:
        if name.startswith(cond + "-"):
            conditions[cond]["scores"].append(scores)

def avg(lst):
    return sum(lst) / len(lst) if lst else 0

def avg_score(scores, key):
    vals = [s.get(key, 0) for s in scores if key in s]
    return sum(vals) / len(vals) if vals else 0

# Efficiency table
print("## Efficiency Metrics")
print(f"{'Condition':<12} {'Avg Cost':>10} {'Avg Time':>10} {'N':>4}")
print("-" * 38)
for cond in ["vanilla", "rna", "rna-mcp"]:
    c = conditions[cond]
    n = len(c["costs"])
    print(f"{cond:<12} ${avg(c['costs']):>8.4f} {avg(c['times']):>8.0f}s {n:>4}")

# Quality table
criteria = [
    ("q1_volume_flow", 0.25),
    ("q2_adapter_checklist", 0.20),
    ("q3_test_coverage", 0.20),
    ("q4_zone_blast_radius", 0.20),
    ("q5_complexity_ranking", 0.15),
]

print("\n## Quality Scores (1-5)")
header = f"{'Criterion':<25}"
for cond in ["vanilla", "rna", "rna-mcp"]:
    header += f" {cond:>10}"
print(header)
print("-" * 57)
for key, weight in criteria:
    row = f"{key:<25}"
    for cond in ["vanilla", "rna", "rna-mcp"]:
        row += f" {avg_score(conditions[cond]['scores'], key):>10.2f}"
    print(row)

print("-" * 57)
row = f"{'weighted_score':<25}"
for cond in ["vanilla", "rna", "rna-mcp"]:
    row += f" {avg_score(conditions[cond]['scores'], 'weighted_score'):>10.2f}"
print(row)

# Quantitative sub-metrics
print("\n## Quantitative Metrics")
for key in ["functions_named", "factual_errors", "max_hop_depth"]:
    row = f"{key:<25}"
    for cond in ["vanilla", "rna", "rna-mcp"]:
        row += f" {avg_score(conditions[cond]['scores'], key):>10.1f}"
    print(row)

print(f"\n## Sample sizes")
for cond in ["vanilla", "rna", "rna-mcp"]:
    c = conditions[cond]
    print(f"  {cond}: {len(c['costs'])} runs, {len(c['scores'])} scored")
