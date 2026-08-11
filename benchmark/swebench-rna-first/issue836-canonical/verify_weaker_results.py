#!/usr/bin/env python3
"""Fail-closed verification for the Haiku/Spark A/T/T2 sensitivity package."""

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
RESULTS = ROOT / "weaker-model-results.json"
FROZEN_RESULTS = ROOT / "results.json"
FROZEN_T2_RESULTS = ROOT / "t2-results.json"
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
README = ROOT / "README.md"
MANIFEST = ROOT / "weaker-model-evidence-manifest.json"
EXPECTED_RESULTS_SHA = "c09fdba9e3eaf058e5c7137a08376bcdf2cce35c9c92d9ef3eceaebb92e2b3a5"
EXPECTED_FROZEN_RESULTS_SHA = "20ad9fcff75b91c5e86147de3cd2fbb63d582aec1950e3cbd0cca0e35d8a8a17"
EXPECTED_FROZEN_T2_RESULTS_SHA = "26e9ad318e2d3a03f355499326dd644968bbd3770807d69d272c617ca7e62daf"
EXPECTED_REPORT_SHA = "588a983ffabea18000da4eb82fbb753bfdaa967241a9c6f8e15ac57b49368052"
EXPECTED_METHOD_SHA = "a9e60195fd3874e9be03cc35da0fd1eebb32986d0573272b844da13ab0064e72"
EXPECTED_README_SHA = "c6737096f2491d4d0e2dcdfc956b68334370567878e1355c699246d47d554e25"
EXPECTED_MANIFEST_SHA = "a3f254abcbe892fbdcd31c5576082ec936e8c0c1211352fbb741197e26a4887b"
BASE_SHA = "da68ef814351f2953d9954f4cc309bf755605ac4e672c3d5096106cc664e3d49"
DIRECTIVE_SHA = "f91a19798b6fbee94e3e1ae17848991154d31ad2d60317f2f0436abfe327143b"
CONDITIONS = (
    "A_haiku",
    "T_haiku",
    "T2_haiku",
    "A_spark",
    "T_spark",
    "T2_spark",
)


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
    require(
        isinstance(value["sha256"], str)
        and len(value["sha256"]) == 64
        and all(character in "0123456789abcdef" for character in value["sha256"]),
        f"{label}: SHA",
    )


def pct(left: float, right: float) -> float:
    return (right / left - 1.0) * 100.0


def metric(cell: dict[str, Any], name: str) -> float:
    if name == "tool_calls":
        return cell["metrics"]["tool_calls"]["total"]
    return cell["metrics"][name]


def aggregate(cells: list[dict[str, Any]]) -> dict[str, Any]:
    tools: collections.Counter[str] = collections.Counter()
    for cell in cells:
        tools.update(cell["metrics"]["tool_calls"]["by_type"])
    costs = [cell["metrics"]["cost_usd"] for cell in cells]
    return {
        "ready_n": sum(cell["status"] == "READY" for cell in cells),
        "evaluated_n": len(cells),
        "resolved_n": sum(cell["official"]["verdict"] == "RESOLVED" for cell in cells),
        "input_tokens": sum(cell["metrics"]["input_tokens"] for cell in cells),
        "output_tokens": sum(cell["metrics"]["output_tokens"] for cell in cells),
        "total_tokens": sum(cell["metrics"]["total_tokens"] for cell in cells),
        "elapsed_seconds": sum(cell["metrics"]["elapsed_seconds"] for cell in cells),
        "cost_usd": sum(costs) if all(cost is not None for cost in costs) else None,
        "cost_available_n": sum(cost is not None for cost in costs),
        "median_elapsed_seconds": median(cell["metrics"]["elapsed_seconds"] for cell in cells),
        "median_total_tokens": median(cell["metrics"]["total_tokens"] for cell in cells),
        "tool_calls": {"total": sum(tools.values()), "by_type": dict(sorted(tools.items()))},
    }


def verify_comparison(value: dict[str, Any], left: list[dict], right: list[dict], label: str) -> None:
    a = [cell["official"]["verdict"] == "RESOLVED" for cell in left]
    b = [cell["official"]["verdict"] == "RESOLVED" for cell in right]
    efficacy = value["efficacy"]
    require(efficacy["baseline_resolved_n"] == sum(a), f"{label}: baseline efficacy")
    require(efficacy["comparison_resolved_n"] == sum(b), f"{label}: comparison efficacy")
    require(efficacy["baseline_only_resolved_n"] == sum(x and not y for x, y in zip(a, b)), f"{label}: baseline-only")
    require(efficacy["comparison_only_resolved_n"] == sum(y and not x for x, y in zip(a, b)), f"{label}: comparison-only")
    require(efficacy["same_verdict_n"] == sum(x == y for x, y in zip(a, b)), f"{label}: same verdict")
    require(efficacy["evaluated_pairs_n"] == 20, f"{label}: efficacy n")
    for name in ("input_tokens", "output_tokens", "total_tokens", "elapsed_seconds", "tool_calls"):
        before = [metric(cell, name) for cell in left]
        after = [metric(cell, name) for cell in right]
        observed = value["efficiency"][name]
        require(close(observed["baseline_total"], sum(before)), f"{label}: {name} baseline")
        require(close(observed["comparison_total"], sum(after)), f"{label}: {name} comparison")
        require(close(observed["aggregate_change_percent"], pct(sum(before), sum(after))), f"{label}: {name} aggregate")
        require(close(observed["median_paired_change_percent"], median(pct(x, y) for x, y in zip(before, after))), f"{label}: {name} median")
        require(observed["comparison_lower_n"] == sum(y < x for x, y in zip(before, after)), f"{label}: {name} lower")
        require(observed["comparison_equal_n"] == sum(y == x for x, y in zip(before, after)), f"{label}: {name} equal")
        require(observed["pairs_n"] == 20, f"{label}: {name} n")
    costs = [cell["metrics"]["cost_usd"] for cell in left + right]
    if all(cost is not None for cost in costs):
        before = [float(cell["metrics"]["cost_usd"]) for cell in left]
        after = [float(cell["metrics"]["cost_usd"]) for cell in right]
        observed = value["efficiency"]["cost_usd"]
        require(close(observed["baseline_total"], sum(before)), f"{label}: cost baseline")
        require(close(observed["comparison_total"], sum(after)), f"{label}: cost comparison")
        require(close(observed["aggregate_change_percent"], pct(sum(before), sum(after))), f"{label}: cost aggregate")
    else:
        require(value["efficiency"]["cost_usd"]["status"] == "UNAVAILABLE", f"{label}: cost unavailable")


def report_cell(cell: dict[str, Any]) -> str:
    metrics = cell["metrics"]
    success = "yes" if cell["official"]["verdict"] == "RESOLVED" else "no"
    cost = "n/a" if metrics["cost_usd"] is None else f"${metrics['cost_usd']:.6f}"
    breakdown = metrics["token_breakdown"]
    if cell["backend"] == "haiku":
        uncached = breakdown["ordinary_input_tokens"] + breakdown["cache_creation_input_tokens"]
        cached = breakdown["cache_read_input_tokens"]
    else:
        uncached = breakdown["uncached_input_tokens"]
        cached = breakdown["cached_input_tokens"]
    return (
        f"success {success} · {metrics['elapsed_seconds']:.1f}s · "
        f"in {metrics['input_tokens']:,} (uncached {uncached:,}, cached {cached:,}) · "
        f"out {metrics['output_tokens']:,} · {cost}"
    )


def report_tools(cell: dict[str, Any]) -> str:
    tools = cell["metrics"]["tool_calls"]
    detail = ", ".join(f"{name}={count}" for name, count in sorted(tools["by_type"].items()))
    return f"{tools['total']} ({detail})"


def main() -> int:
    data = load(RESULTS)
    frozen = load(FROZEN_RESULTS)
    frozen_t2 = load(FROZEN_T2_RESULTS)
    manifest = load(MANIFEST)
    report = REPORT.read_text()
    require(digest(RESULTS) == EXPECTED_RESULTS_SHA, "weaker-model results digest drift")
    require(digest(FROZEN_RESULTS) == EXPECTED_FROZEN_RESULTS_SHA, "frozen A/T results digest drift")
    require(digest(FROZEN_T2_RESULTS) == EXPECTED_FROZEN_T2_RESULTS_SHA, "frozen T2 results digest drift")
    require(digest(REPORT) == EXPECTED_REPORT_SHA, "unified report digest drift")
    require(digest(METHOD) == EXPECTED_METHOD_SHA, "method digest drift")
    require(digest(README) == EXPECTED_README_SHA, "README digest drift")
    require(digest(MANIFEST) == EXPECTED_MANIFEST_SHA, "weaker-model manifest digest drift")
    require(data["schema_version"] == "issue836-weaker-model-results-v1", "schema")
    require(data["status"] == "COMPLETE", "status")
    rows = data["rows"]
    frozen_by_rank = {row["rank"]: row for row in frozen["rows"]}
    frozen_t2_by_rank = {row["rank"]: row for row in frozen_t2["rows"]}
    require(len(rows) == 20, "row count")
    for expected_rank, row in enumerate(rows, 1):
        require(row["rank"] == expected_rank, f"rank {expected_rank}: order")
        require(set(row["conditions"]) == set(CONDITIONS), f"rank {expected_rank}: conditions")
        require(row["cross_backend_user_prompt_byte_parity"] is True, f"rank {expected_rank}: parity")
        for model in ("haiku", "spark"):
            prompts = []
            for arm in ("A", "T", "T2"):
                condition = f"{arm}_{model}"
                cell = row["conditions"][condition]
                require(cell["condition"] == condition and cell["arm"] == arm and cell["backend"] == model, f"rank {expected_rank} {condition}: identity")
                require(cell["status"] == "READY", f"rank {expected_rank} {condition}: ready")
                require(cell["official"]["verdict"] in {"RESOLVED", "UNRESOLVED"}, f"rank {expected_rank} {condition}: verdict")
                for name in ("official", "episode_receipt", "prompt", "base_instructions", "patch"):
                    validate_ref(cell["official"]["source"] if name == "official" else cell[name], f"rank {expected_rank} {condition} {name}")
                require(cell["base_instructions"]["sha256"] == BASE_SHA, f"rank {expected_rank} {condition}: base")
                if arm == "A":
                    require(cell["directive"] is None, f"rank {expected_rank} {condition}: directive")
                    require(cell["data_quality"]["injected_rna_exposure_count"] == 0, f"rank {expected_rank} {condition}: exposure")
                    require(cell["treatment_context"] is None, f"rank {expected_rank} {condition}: treatment context")
                elif arm == "T":
                    validate_ref(cell["directive"], f"rank {expected_rank} {condition} directive")
                    require(cell["directive"]["sha256"] == DIRECTIVE_SHA, f"rank {expected_rank} {condition}: directive SHA")
                    require(cell["data_quality"]["injected_rna_exposure_count"] == 1, f"rank {expected_rank} {condition}: exposure")
                    require(cell["treatment_context"] is None, f"rank {expected_rank} {condition}: inherited T context schema")
                else:
                    validate_ref(cell["directive"], f"rank {expected_rank} {condition} directive")
                    require(cell["directive"]["sha256"] == DIRECTIVE_SHA, f"rank {expected_rank} {condition}: directive SHA")
                    require(cell["data_quality"]["injected_rna_exposure_count"] == 1, f"rank {expected_rank} {condition}: exposure")
                    require(isinstance(cell["treatment_context"], dict), f"rank {expected_rank} {condition}: treatment context")
                metrics = cell["metrics"]
                require(metrics["total_tokens"] == metrics["input_tokens"] + metrics["output_tokens"], f"rank {expected_rank} {condition}: token total")
                require(metrics["tool_calls"]["total"] == sum(metrics["tool_calls"]["by_type"].values()), f"rank {expected_rank} {condition}: tool total")
                require(cell["transcript_audit"]["status"] == "PASS", f"rank {expected_rank} {condition}: audit")
                require(
                    cell["metrics"]["tool_calls"] == cell["transcript_audit"]["tool_calls"],
                    f"rank {expected_rank} {condition}: independent tool recount",
                )
                require(cell["transcript_audit"]["foreign_rank_references"] == [], f"rank {expected_rank} {condition}: contamination")
                if model == "haiku":
                    breakdown = metrics["token_breakdown"]
                    require(metrics["input_tokens"] == breakdown["ordinary_input_tokens"] + breakdown["cache_creation_input_tokens"] + breakdown["cache_read_input_tokens"], f"rank {expected_rank} {condition}: input accounting")
                    require(close(metrics["cost_usd"], sum(item["costUSD"] for item in breakdown["per_model"].values())), f"rank {expected_rank} {condition}: cost cross-check")
                else:
                    breakdown = metrics["token_breakdown"]
                    require(metrics["input_tokens"] == breakdown["uncached_input_tokens"] + breakdown["cached_input_tokens"], f"rank {expected_rank} {condition}: input accounting")
                    require(metrics["cost_usd"] is None, f"rank {expected_rank} {condition}: Spark cost")
                prompts.append(cell["prompt"])
                require(report_cell(cell) in report, f"rank {expected_rank} {condition}: report cell")
            require(prompts[0] != prompts[1] and prompts[1] != prompts[2], f"rank {expected_rank} {model}: condition prompt distinction")
        for arm in ("A", "T", "T2"):
            require(row["conditions"][f"{arm}_haiku"]["prompt"] == row["conditions"][f"{arm}_spark"]["prompt"], f"rank {expected_rank} {arm}: backend prompt parity")
            reference = (
                frozen_t2_by_rank[expected_rank]["conditions"]["T2_sonnet"]["prompt"]
                if arm == "T2"
                else frozen_by_rank[expected_rank]["conditions"][f"{arm}_sonnet"]["prompt"]
            )
            require(
                row["conditions"][f"{arm}_haiku"]["prompt"] == reference,
                f"rank {expected_rank} {arm}: frozen Sonnet/Luna prompt parity",
            )
        for model in ("haiku", "spark"):
            tool_row = f"| {expected_rank} | " + " | ".join(report_tools(row["conditions"][f"{arm}_{model}"]) for arm in ("A", "T", "T2")) + " |"
            require(tool_row in report, f"rank {expected_rank} {model}: report tools")

    observed = {
        condition: aggregate([row["conditions"][condition] for row in rows])
        for condition in CONDITIONS
    }
    for condition, summary in observed.items():
        expected = data["condition_summaries"][condition]
        for field, value in summary.items():
            if field in {"elapsed_seconds", "cost_usd"} and value is not None:
                require(close(expected[field], value), f"{condition}: condition summary {field}")
            else:
                require(expected[field] == value, f"{condition}: condition summary {field}")
    prompt_summary = {}
    for arm in ("A", "T", "T2"):
        sizes = [row["conditions"][f"{arm}_haiku"]["prompt"]["bytes"] for row in rows]
        prompt_summary[arm] = {
            "total_bytes": sum(sizes),
            "mean_bytes": sum(sizes) / 20,
            "minimum_bytes": min(sizes),
            "maximum_bytes": max(sizes),
        }
    require(data["prompt_byte_summary"] == prompt_summary, "prompt byte summary")
    decompositions = {}
    for condition in CONDITIONS:
        cells = [row["conditions"][condition] for row in rows]
        if condition.endswith("_haiku"):
            decompositions[condition] = {
                "uncached_input_tokens": sum(
                    cell["metrics"]["token_breakdown"]["ordinary_input_tokens"]
                    + cell["metrics"]["token_breakdown"]["cache_creation_input_tokens"]
                    for cell in cells
                ),
                "cached_input_tokens": sum(
                    cell["metrics"]["token_breakdown"]["cache_read_input_tokens"]
                    for cell in cells
                ),
                "output_tokens": sum(cell["metrics"]["output_tokens"] for cell in cells),
                "reasoning_output_tokens_subset": None,
            }
        else:
            decompositions[condition] = {
                "uncached_input_tokens": sum(
                    cell["metrics"]["token_breakdown"]["uncached_input_tokens"]
                    for cell in cells
                ),
                "cached_input_tokens": sum(
                    cell["metrics"]["token_breakdown"]["cached_input_tokens"]
                    for cell in cells
                ),
                "cache_write_input_tokens": sum(
                    cell["metrics"]["token_breakdown"]["cache_write_input_tokens"]
                    for cell in cells
                ),
                "output_tokens": sum(cell["metrics"]["output_tokens"] for cell in cells),
                "reasoning_output_tokens_subset": sum(
                    cell["metrics"]["token_breakdown"]["reasoning_output_tokens_subset"]
                    for cell in cells
                ),
            }
    require(data["token_decomposition"] == decompositions, "token decomposition")
    for condition, item in decompositions.items():
        reasoning = item["reasoning_output_tokens_subset"]
        report_row = (
            f"| {condition} | {item['uncached_input_tokens']:,} | "
            f"{item['cached_input_tokens']:,} | {item['output_tokens']:,} | "
            f"{'n/a' if reasoning is None else f'{reasoning:,}'} |"
        )
        require(report_row in report, f"{condition}: report token decomposition")
    for model in ("haiku", "spark"):
        arms = {arm: [row["conditions"][f"{arm}_{model}"] for row in rows] for arm in ("A", "T", "T2")}
        verify_comparison(data["within_model_comparisons"][model]["A_to_T"], arms["A"], arms["T"], f"{model} A→T")
        verify_comparison(data["within_model_comparisons"][model]["A_to_T2"], arms["A"], arms["T2"], f"{model} A→T2")
        verify_comparison(data["within_model_comparisons"][model]["T_to_T2"], arms["T"], arms["T2"], f"{model} T→T2")
    quality = data["transcript_quality_summary"]
    require(quality["audited_cells"] == 120 and quality["passing_cells"] == 120 and quality["pending_or_invalid_cells"] == 0, "audit coverage")
    require(quality["injected_rna_exposure_count"] == 80, "injected RNA exposure coverage")
    require(quality["model_followup_rna_tool_call_count"] == 0, "follow-up RNA count")
    require(data["episode_accounting"] == {"canonical_cells": 120, "paid_provider_episodes": 120, "superseded_cells": 0}, "episode accounting")
    require("Weaker-model sensitivity: Haiku and Spark" in report, "report section")
    require("Spark cost is reported as unavailable" in report, "report cost disclosure")
    require(manifest["canonical_cell_count"] == 120, "manifest canonical cells")
    require(manifest["paid_provider_episode_count"] == 120, "manifest paid episodes")
    require(manifest["stock_evaluator_model_calls"] == 0, "manifest evaluator model calls")
    evidence_root = os.environ.get("ISSUE836_EVIDENCE_ROOT")
    if evidence_root:
        root = Path(evidence_root)
        for name, artifact in manifest["external_artifacts"].items():
            path = root / artifact["path_relative_to_evidence_root"]
            require(path.is_file(), f"external {name}: absent")
            require(path.stat().st_size == artifact["bytes"], f"external {name}: bytes")
            require(digest(path) == artifact["sha256"], f"external {name}: SHA")
    print(json.dumps({"status": "PASS", "assertions": assertions, "weaker_model_cells": 120, "external_evidence_verified": bool(evidence_root)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
