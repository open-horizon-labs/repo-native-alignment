#!/usr/bin/env python3
"""Fail-closed verification for the additive faithful unfiltered T2 package."""

from __future__ import annotations

import collections
import hashlib
import json
import math
import os
from pathlib import Path
from statistics import median
from typing import Any


ROOT = Path(__file__).resolve().parent
T2_RESULTS = ROOT / "t2-results.json"
OLD_RESULTS = ROOT / "results.json"
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
MANIFEST = ROOT / "t2-evidence-manifest.json"
EXPECTED_T2_RESULTS_SHA = "26e9ad318e2d3a03f355499326dd644968bbd3770807d69d272c617ca7e62daf"
EXPECTED_REPORT_SHA = "588a983ffabea18000da4eb82fbb753bfdaa967241a9c6f8e15ac57b49368052"
EXPECTED_METHOD_SHA = "a9e60195fd3874e9be03cc35da0fd1eebb32986d0573272b844da13ab0064e72"
EXPECTED_MANIFEST_SHA = "518dc273c59fc548008c66f84ded5dcf1c377696aef0befb6824e3a2151b28cc"
BASE_SHA = "da68ef814351f2953d9954f4cc309bf755605ac4e672c3d5096106cc664e3d49"
DIRECTIVE_SHA = "f91a19798b6fbee94e3e1ae17848991154d31ad2d60317f2f0436abfe327143b"
CONDITIONS = ("T2_sonnet", "T2_luna")
SHA_RE = __import__("re").compile(r"^[0-9a-f]{64}$")


class VerificationFailure(RuntimeError):
    pass


assertions = 0


def require(value: bool, message: str) -> None:
    global assertions
    assertions += 1
    if not value:
        raise VerificationFailure(message)


def close(left: float, right: float) -> bool:
    return math.isclose(left, right, rel_tol=0.0, abs_tol=1e-9)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    require(isinstance(value, dict), f"{path.name}: root is not an object")
    return value


def validate_ref(value: Any, label: str) -> None:
    require(isinstance(value, dict), f"{label}: not an object")
    require(set(value) == {"bytes", "sha256"}, f"{label}: reference schema")
    require(isinstance(value["bytes"], int) and value["bytes"] >= 0, f"{label}: bytes")
    require(isinstance(value["sha256"], str) and SHA_RE.fullmatch(value["sha256"]) is not None, f"{label}: SHA")


def percent(left: float, right: float) -> float:
    return (right / left - 1.0) * 100.0


def metric(cell: dict[str, Any], name: str) -> float:
    if name == "tool_calls":
        return cell["metrics"]["tool_calls"]["total"]
    return cell["metrics"][name]


def aggregate(cells: list[dict[str, Any]]) -> dict[str, Any]:
    tools: collections.Counter[str] = collections.Counter()
    for cell in cells:
        tools.update(cell["metrics"]["tool_calls"]["by_type"])
    return {
        "ready_n": sum(cell["status"] == "READY" for cell in cells),
        "evaluated_n": len(cells),
        "resolved_n": sum(cell["official"]["verdict"] == "RESOLVED" for cell in cells),
        "input_tokens": sum(cell["metrics"]["input_tokens"] for cell in cells),
        "output_tokens": sum(cell["metrics"]["output_tokens"] for cell in cells),
        "total_tokens": sum(cell["metrics"]["total_tokens"] for cell in cells),
        "elapsed_seconds": sum(cell["metrics"]["elapsed_seconds"] for cell in cells),
        "cost_usd": sum(cell["metrics"]["cost_usd"] for cell in cells),
        "median_elapsed_seconds": median(cell["metrics"]["elapsed_seconds"] for cell in cells),
        "median_total_tokens": median(cell["metrics"]["total_tokens"] for cell in cells),
        "tool_calls": {"total": sum(tools.values()), "by_type": dict(sorted(tools.items()))},
    }


def token_decomposition(cells: list[dict[str, Any]], backend: str) -> dict[str, int]:
    if backend == "sonnet":
        per_model = [cell["metrics"]["token_breakdown"]["per_model"] for cell in cells]
        cached = sum(sum(model["cacheReadInputTokens"] for model in models.values()) for models in per_model)
        creation = sum(sum(model["cacheCreationInputTokens"] for model in models.values()) for models in per_model)
        ordinary = sum(sum(model["inputTokens"] for model in models.values()) for models in per_model)
        return {
            "cached_input_tokens": cached,
            "cache_creation_input_tokens": creation,
            "ordinary_input_tokens": ordinary,
            "uncached_input_tokens": creation + ordinary,
            "output_tokens": sum(cell["metrics"]["output_tokens"] for cell in cells),
        }
    breakdowns = [cell["metrics"]["token_breakdown"] for cell in cells]
    output = sum(cell["metrics"]["output_tokens"] for cell in cells)
    reasoning = sum(item["reasoning_output_tokens_subset"] for item in breakdowns)
    return {
        "cached_input_tokens": sum(item["cached_input_tokens"] for item in breakdowns),
        "cache_write_input_tokens": sum(item["cache_write_input_tokens"] for item in breakdowns),
        "uncached_input_tokens": sum(item["uncached_input_tokens"] for item in breakdowns),
        "output_tokens": output,
        "reasoning_output_tokens_subset": reasoning,
        "non_reasoning_output_tokens": output - reasoning,
    }


def verify_comparison(
    value: dict[str, Any], baseline: list[dict[str, Any]], t2: list[dict[str, Any]], label: str
) -> None:
    baseline_resolved = [cell["official"]["verdict"] == "RESOLVED" for cell in baseline]
    t2_resolved = [cell["official"]["verdict"] == "RESOLVED" for cell in t2]
    efficacy = value["efficacy"]
    require(efficacy["baseline_resolved_n"] == sum(baseline_resolved), f"{label}: baseline efficacy")
    require(efficacy["T2_resolved_n"] == sum(t2_resolved), f"{label}: T2 efficacy")
    require(efficacy["baseline_only_resolved_n"] == sum(a and not b for a, b in zip(baseline_resolved, t2_resolved)), f"{label}: baseline-only")
    require(efficacy["T2_only_resolved_n"] == sum(b and not a for a, b in zip(baseline_resolved, t2_resolved)), f"{label}: T2-only")
    require(efficacy["same_verdict_n"] == sum(a == b for a, b in zip(baseline_resolved, t2_resolved)), f"{label}: same verdict")
    require(close(efficacy["resolution_rate_change_percentage_points"], (sum(t2_resolved) - sum(baseline_resolved)) * 5.0), f"{label}: efficacy delta")
    require(efficacy["evaluated_pairs_n"] == 20, f"{label}: efficacy n")
    for name in ("input_tokens", "output_tokens", "total_tokens", "elapsed_seconds", "cost_usd", "tool_calls"):
        left = [metric(cell, name) for cell in baseline]
        right = [metric(cell, name) for cell in t2]
        observed = value["efficiency"][name]
        require(close(observed["baseline_total"], sum(left)), f"{label}: {name} baseline")
        require(close(observed["T2_total"], sum(right)), f"{label}: {name} T2")
        require(close(observed["aggregate_change_percent"], percent(sum(left), sum(right))), f"{label}: {name} aggregate")
        require(close(observed["median_paired_change_percent"], median(percent(a, b) for a, b in zip(left, right))), f"{label}: {name} median")
        require(observed["T2_lower_n"] == sum(b < a for a, b in zip(left, right)), f"{label}: {name} lower")
        require(observed["T2_equal_n"] == sum(b == a for a, b in zip(left, right)), f"{label}: {name} equal")
        require(observed["pairs_n"] == 20, f"{label}: {name} n")


def report_cell(cell: dict[str, Any]) -> str:
    metrics = cell["metrics"]
    success = "yes" if cell["official"]["verdict"] == "RESOLVED" else "no"
    return f"success {success} · {metrics['elapsed_seconds']:.1f}s · in {metrics['input_tokens']:,} · out {metrics['output_tokens']:,} · ${metrics['cost_usd']:.6f}"


def report_tools(cell: dict[str, Any]) -> str:
    tools = cell["metrics"]["tool_calls"]
    parts = ", ".join(f"{name}={count}" for name, count in sorted(tools["by_type"].items()))
    return f"{tools['total']} ({parts})"


def main() -> int:
    data = load(T2_RESULTS)
    old = load(OLD_RESULTS)
    manifest = load(MANIFEST)
    report = REPORT.read_text()
    require(digest(T2_RESULTS) == EXPECTED_T2_RESULTS_SHA, "T2 results digest drift")
    require(digest(REPORT) == EXPECTED_REPORT_SHA, "unified report digest drift")
    require(digest(METHOD) == EXPECTED_METHOD_SHA, "method digest drift")
    require(digest(MANIFEST) == EXPECTED_MANIFEST_SHA, "manifest digest drift")
    require(data["schema_version"] == "issue836-t2-unfiltered-results-v1", "T2 schema")
    require(data["status"] == "COMPLETE", "T2 status")
    require(data["old_results"] == {"bytes": OLD_RESULTS.stat().st_size, "sha256": digest(OLD_RESULTS)}, "old results binding")
    contract = data["prompt_contract"]["graph"]
    require(contract == {"mode": "neighbors", "hops": 2, "direction": "both", "edge_types": None, "include_body": True, "minify_body": True, "preserve_edge_type_labels": True, "whole_record_limit_ladder": [20, 10, 5, 2, 1], "prompt_max_bytes": 32768}, "faithful T2 graph contract")
    rows = data["rows"]
    require(len(rows) == 20, "T2 row count")
    old_by_rank = {row["rank"]: row for row in old["rows"]}
    for expected_rank, row in enumerate(rows, 1):
        require(row["rank"] == expected_rank, f"rank {expected_rank}: order")
        require(row["instance_id"] == old_by_rank[expected_rank]["instance_id"], f"rank {expected_rank}: identity")
        require(set(row["conditions"]) == set(CONDITIONS), f"rank {expected_rank}: conditions")
        require(row["cross_backend_byte_parity"] is True, f"rank {expected_rank}: parity flag")
        graph = row["t2_contract"]["graph"]
        require(graph["mode"] == "neighbors" and graph["hops"] == 2 and graph["direction"] == "both" and graph["edge_types"] is None, f"rank {expected_rank}: graph traversal")
        require(graph["whole_record_limit"] in {20, 10, 5, 2, 1}, f"rank {expected_rank}: graph limit")
        require(row["t2_contract"]["user_prompt"]["bytes"] <= 32768, f"rank {expected_rank}: prompt cap")
        prompts = []
        for condition in CONDITIONS:
            cell = row["conditions"][condition]
            require(cell["condition"] == condition and cell["arm"] == "T2", f"rank {expected_rank} {condition}: identity")
            require(cell["status"] == "READY", f"rank {expected_rank} {condition}: status")
            require(cell["official"]["verdict"] in {"RESOLVED", "UNRESOLVED"}, f"rank {expected_rank} {condition}: verdict")
            for name in ("official", "episode_receipt", "prompt", "base_instructions", "directive", "patch"):
                validate_ref(cell["official"]["source"] if name == "official" else cell[name], f"rank {expected_rank} {condition} {name}")
            require(cell["base_instructions"]["sha256"] == BASE_SHA, f"rank {expected_rank} {condition}: base")
            require(cell["directive"]["sha256"] == DIRECTIVE_SHA, f"rank {expected_rank} {condition}: directive")
            metrics = cell["metrics"]
            require(metrics["total_tokens"] == metrics["input_tokens"] + metrics["output_tokens"], f"rank {expected_rank} {condition}: tokens")
            require(metrics["tool_calls"]["total"] == sum(metrics["tool_calls"]["by_type"].values()), f"rank {expected_rank} {condition}: tools")
            if condition == "T2_sonnet":
                models = metrics["token_breakdown"]["per_model"]
                require(metrics["input_tokens"] == sum(model["inputTokens"] + model["cacheCreationInputTokens"] + model["cacheReadInputTokens"] for model in models.values()), f"rank {expected_rank} {condition}: input accounting")
                require(metrics["output_tokens"] == sum(model["outputTokens"] for model in models.values()), f"rank {expected_rank} {condition}: output accounting")
                require(close(metrics["cost_usd"], sum(model["costUSD"] for model in models.values())), f"rank {expected_rank} {condition}: cost cross-check")
            else:
                breakdown = metrics["token_breakdown"]
                require(metrics["input_tokens"] == breakdown["cached_input_tokens"] + breakdown["uncached_input_tokens"], f"rank {expected_rank} {condition}: input accounting")
                expected_cost = (
                    breakdown["uncached_input_tokens"] * 1.0
                    + breakdown["cached_input_tokens"] * 0.1
                    + breakdown["cache_write_input_tokens"] * 1.25
                    + metrics["output_tokens"] * 6.0
                ) / 1_000_000 + metrics["tool_calls"]["by_type"].get("webSearch", 0) * 0.01
                require(close(metrics["cost_usd"], expected_cost), f"rank {expected_rank} {condition}: cost cross-check")
            require(cell["transcript_audit"]["status"] == "PASS", f"rank {expected_rank} {condition}: audit")
            require(cell["transcript_audit"]["foreign_rank_references"] == [], f"rank {expected_rank} {condition}: foreign rank")
            require(cell["data_quality"]["injected_rna_exposure_count"] == 1, f"rank {expected_rank} {condition}: injected RNA exposure")
            expected_followup = 0
            require(cell["data_quality"]["model_followup_rna_tool_call_count"] == expected_followup, f"rank {expected_rank} {condition}: follow-up RNA calls")
            require(cell["data_quality"]["successful_model_followup_rna_tool_call_count"] == expected_followup, f"rank {expected_rank} {condition}: successful follow-up RNA calls")
            require(cell["treatment_context"]["graph_limit"] == graph["whole_record_limit"], f"rank {expected_rank} {condition}: context limit")
            prompts.append(cell["prompt"])
        require(prompts[0] == prompts[1] == row["t2_contract"]["user_prompt"], f"rank {expected_rank}: prompt bytes")
        matrix_fragment = f"| {expected_rank} | `{row['instance_id']}` |"
        require(matrix_fragment in report, f"rank {expected_rank}: report matrix")
        for condition in CONDITIONS:
            require(report_cell(row["conditions"][condition]) in report, f"rank {expected_rank} {condition}: report cell")
        tool_row = f"| {expected_rank} | " + " | ".join(
            report_tools(row["conditions"][condition]) for condition in CONDITIONS
        ) + " |"
        require(tool_row in report, f"rank {expected_rank}: report tool row")

    observed = {}
    for condition in CONDITIONS:
        cells = [row["conditions"][condition] for row in rows]
        observed[condition] = aggregate(cells)
        expected = data["condition_summaries"][condition]
        for field, value in observed[condition].items():
            if field in {"elapsed_seconds", "cost_usd", "median_elapsed_seconds", "median_total_tokens"}:
                require(close(expected[field], value), f"{condition}: aggregate {field}")
            else:
                require(expected[field] == value, f"{condition}: aggregate {field}")
    for backend in ("sonnet", "luna"):
        t2 = [row["conditions"][f"T2_{backend}"] for row in rows]
        for arm in ("A", "T"):
            condition = f"{arm}_{backend}"
            cells = [old_by_rank[rank]["conditions"][condition] for rank in range(1, 21)]
            require(data["token_decomposition"][condition] == token_decomposition(cells, backend), f"{condition}: token decomposition")
        require(data["token_decomposition"][f"T2_{backend}"] == token_decomposition(t2, backend), f"T2_{backend}: token decomposition")
        for key, baseline in (("A_to_T2", "A"), ("T_to_T2", "T")):
            baseline_cells = [old_by_rank[rank]["conditions"][f"{baseline}_{backend}"] for rank in range(1, 21)]
            verify_comparison(data["within_model_comparisons"][backend][key], baseline_cells, t2, f"{backend} {key}")
    quality = data["transcript_quality_summary"]
    require(quality["audited_cells"] == 40 and quality["passing_cells"] == 40 and quality["pending_or_invalid_cells"] == 0, "transcript quality coverage")
    require(quality["luna_started_without_completion_total"] == 0, "Luna item completion")
    require(quality["injected_rna_exposure_count"] == 40, "injected RNA exposure coverage")
    require(quality["model_followup_rna_tool_call_count"] == 0, "canonical follow-up RNA count")
    require(quality["successful_model_followup_rna_tool_call_count"] == 0, "canonical successful follow-up RNA count")
    require(quality["model_followup_rna_cells"] == [], "canonical follow-up RNA cells")
    manual_rank20 = quality["manual_rank20_transcript_review"]
    require(manual_rank20["status"] == "PASS_WITH_DISCLOSURE", "manual rank-20 transcript review")
    require(manual_rank20["foreign_checkout_exposure"] is False, "manual rank-20 foreign checkout exposure")
    require(manual_rank20["direct_solution_exposure"] is False, "manual rank-20 direct solution exposure")
    require(
        data["episode_accounting"] == {
            "canonical_cells": 40,
            "superseded_harness_bug_cells": 2,
            "total_paid_provider_episodes": 42,
            "selection_manifest": data["source_evidence"]["canonical_selection"],
        },
        "T2 episode accounting",
    )
    require("no edge-type filter" in report, "report faithful graph statement")
    require("original 80-cell A/T evidence" in report, "report preserves original evidence")
    require(
        "Sonnet/Luna T2 extension executed 42 paid provider episodes" in report,
        "report superseded episode accounting",
    )
    require(manifest["schema_version"] == "issue836-t2-unfiltered-evidence-manifest-v2", "T2 manifest schema")
    require(manifest["paid_model_calls_to_build_review_package"] == 0, "T2 review package model calls")
    require(manifest["paid_t2_episode_count"] == 42, "paid T2 episode count")
    require(manifest["canonical_t2_cell_count"] == 40, "canonical T2 cell count")
    require(manifest["superseded_harness_bug_episode_count"] == 2, "superseded T2 episode count")
    require(manifest["stock_evaluator_model_calls"] == 0, "stock evaluator model calls")
    t2_artifacts = manifest["external_artifacts"]
    require(isinstance(t2_artifacts, dict), "manifest T2 artifacts")
    require(len(t2_artifacts) == 26, "manifest T2 artifact count")
    evidence_root = os.environ.get("ISSUE836_EVIDENCE_ROOT")
    if evidence_root:
        root = Path(evidence_root)
        for name, artifact in t2_artifacts.items():
            path = root / artifact["path_relative_to_evidence_root"]
            require(path.is_file(), f"external {name}: absent")
            require(path.stat().st_size == artifact["bytes"], f"external {name}: bytes")
            require(digest(path) == artifact["sha256"], f"external {name}: SHA")
    print(json.dumps({"status": "PASS", "assertions": assertions, "t2_cells": 40, "external_evidence_verified": bool(evidence_root)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
