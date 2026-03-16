#!/usr/bin/env python3
"""Aggregate benchmark scores into a summary table."""
import json, glob, sys, os

scores_dir = sys.argv[1] if len(sys.argv) > 1 else "benchmark/results/scores"

vanilla_scores = []
rna_scores = []

for f in sorted(glob.glob(os.path.join(scores_dir, "*.json"))):
    try:
        with open(f) as fh:
            d = json.load(fh)
            result = d.get("result", "")
            # Extract JSON from the result text
            start = result.find("{")
            end = result.rfind("}") + 1
            if start >= 0 and end > start:
                scores = json.loads(result[start:end])
            else:
                continue
    except:
        continue

    name = os.path.basename(f)
    if name.startswith("vanilla-"):
        vanilla_scores.append(scores)
    elif name.startswith("rna-"):
        rna_scores.append(scores)

def avg(lst, key):
    vals = [s.get(key, 0) for s in lst if key in s]
    return sum(vals) / len(vals) if vals else 0

criteria = ["q1_volume_flow", "q2_adapter_checklist", "q3_test_coverage", "q4_zone_blast_radius", "q5_complexity_ranking"]
weights = [0.25, 0.20, 0.20, 0.20, 0.15]

print(f"{'Criterion':<25} {'Vanilla':>10} {'RNA':>10} {'Delta':>10}")
print("-" * 55)
for c, w in zip(criteria, weights):
    v = avg(vanilla_scores, c)
    r = avg(rna_scores, c)
    delta = r - v
    print(f"{c:<25} {v:>10.2f} {r:>10.2f} {delta:>+10.2f}")

v_weighted = avg(vanilla_scores, "weighted_score")
r_weighted = avg(rna_scores, "weighted_score")
print("-" * 55)
print(f"{'weighted_score':<25} {v_weighted:>10.2f} {r_weighted:>10.2f} {r_weighted - v_weighted:>+10.2f}")

print(f"\n{'functions_named':<25} {avg(vanilla_scores, 'functions_named'):>10.1f} {avg(rna_scores, 'functions_named'):>10.1f}")
print(f"{'factual_errors':<25} {avg(vanilla_scores, 'factual_errors'):>10.1f} {avg(rna_scores, 'factual_errors'):>10.1f}")
print(f"{'max_hop_depth':<25} {avg(vanilla_scores, 'max_hop_depth'):>10.1f} {avg(rna_scores, 'max_hop_depth'):>10.1f}")

print(f"\nSample sizes: vanilla={len(vanilla_scores)}, rna={len(rna_scores)}")
