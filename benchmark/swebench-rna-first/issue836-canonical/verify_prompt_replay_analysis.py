#!/usr/bin/env python3
"""Verify the canonical replay decomposition and matched A6/T6 micro-gate."""

from __future__ import annotations

import hashlib
import json
import math
import os
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "prompt-replay-analysis.json"
MANIFEST = ROOT / "prompt-replay-analysis-evidence-manifest.json"
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
EXPECTED_RESULTS_SHA = "9402e0cc1b0c0f61f69a6aefb2f86b72db38c7d0f478a64a44ed7256228be191"
EXPECTED_MANIFEST_SHA = "4b95b119490c46d7fe155d2e91e1d74040a7598063cd2950615c6359429947d0"
EXPECTED_REPORT_SHA = "88ab5b5a2bf90374ee41d2cdb93698310e65585a6cf22ace019ec9dd37f175a4"
EXPECTED_METHOD_SHA = "fbaff4a325a4f108a949d91e3ebd9a1e7dee98f0dcceae12a238cd43c6d91488"
MATCHED_CONDITIONS = {
    "A6_AS_sonnet", "T6_AS_sonnet", "A6_AS_spark", "T6_AS_spark",
}


class VerificationFailure(RuntimeError):
    pass


assertions = 0


def require(value: bool, message: str) -> None:
    global assertions
    assertions += 1
    if not value:
        raise VerificationFailure(message)


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    require(isinstance(value, dict), f"{path.name}: root is not an object")
    return value


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def verify_package_digests() -> None:
    for path, expected in (
        (RESULTS, EXPECTED_RESULTS_SHA),
        (MANIFEST, EXPECTED_MANIFEST_SHA),
        (REPORT, EXPECTED_REPORT_SHA),
        (METHOD, EXPECTED_METHOD_SHA),
    ):
        require(digest(path.read_bytes()) == expected, f"{path.name}: package digest drift")


def pct(before: float, after: float) -> float:
    return round((after / before - 1.0) * 100.0, 1)


def close(left: float, right: float) -> bool:
    return math.isclose(left, right, rel_tol=0.0, abs_tol=1e-9)


def path_free_ref(reference: dict[str, Any]) -> dict[str, Any]:
    return {key: reference[key] for key in ("bytes", "sha256")}


def verify_canonical(data: dict[str, Any]) -> None:
    canonical = data["canonical_strategy_factorial"]
    rows = canonical["comparisons"]
    require(canonical["cell_count"] == 480, "canonical cell count")
    require(canonical["comparison_count"] == 16 and len(rows) == 16, "comparison count")
    require(len({(row["model"], row["strategy"], row["treatment"]) for row in rows}) == 16, "comparison identity")
    exact = []
    for row in rows:
        require(row["case_count"] == 20, "comparison case count")
        delta = row["treatment_main_input_tokens"] - row["control_main_input_tokens"]
        require(row["actual_delta_tokens"] == delta, "actual input delta")
        require(
            row["actual_delta_tokens"]
            == row["static_prefix_replay_tokens"]
            + row["interaction_trajectory_delta_tokens"],
            "replay decomposition identity",
        )
        require(row["identity_check"] is True, "recorded decomposition identity")
        require(row["actual_delta_percent"] == pct(row["control_main_input_tokens"], row["treatment_main_input_tokens"]), "input percent")
        require(row["request_delta_percent"] == pct(row["control_requests"], row["treatment_requests"]), "request percent")
        require(row["tool_call_delta_percent"] == pct(row["control_tool_calls"], row["treatment_tool_calls"]), "tool percent")
        require(
            row["tool_result_character_delta_percent"]
            == pct(row["control_tool_result_characters"], row["treatment_tool_result_characters"]),
            "tool-result percent",
        )
        expected_status = "TRANSCRIPT_VISIBLE_APPROXIMATION" if row["model"] == "haiku" else "EXACT"
        require(row["decomposition_status"] == expected_status, "decomposition status")
        if expected_status == "EXACT":
            require(row["transcript_sequence_matches_audit_cells"] == 20, "exact audit match")
            require(row["treatment_audit_main_input_tokens"] == row["treatment_transcript_main_input_tokens"], "audit input match")
            exact.append(row)
    require(len(exact) == 12, "exact comparison count")
    for aggregate_name, selected in (
        ("aggregate_all_transcript_visible", rows),
        ("aggregate_exact_only", exact),
    ):
        aggregate = canonical[aggregate_name]
        require(aggregate["comparison_count"] == len(selected), f"{aggregate_name}: count")
        for field in (
            "actual_delta_tokens", "static_prefix_replay_tokens",
            "interaction_trajectory_delta_tokens",
        ):
            require(aggregate[field] == sum(row[field] for row in selected), f"{aggregate_name}: {field}")
        require(
            aggregate["actual_delta_tokens"]
            == aggregate["static_prefix_replay_tokens"]
            + aggregate["interaction_trajectory_delta_tokens"],
            f"{aggregate_name}: identity",
        )


def metric(cell: dict[str, Any], name: str) -> float:
    if name == "tool_calls":
        return cell["metrics"]["tool_calls"]["total"]
    return cell["metrics"][name]


def verify_matched(data: dict[str, Any]) -> None:
    matched = data["matched_micro_gate"]
    require(matched["case"] == {"rank": 10, "instance_id": "django__django-11551"}, "matched case")
    require(matched["paid_cell_count"] == 4 and matched["scale_authorized"] is False, "micro-gate scope")
    require(set(matched["conditions"]) == MATCHED_CONDITIONS, "matched condition inventory")
    for key, cell in matched["conditions"].items():
        metrics = cell["metrics"]
        require(cell["official_verdict"] == "RESOLVED", f"{key}: verdict")
        require(cell["followup_rna_tool_call_count"] == 0, f"{key}: follow-up RNA")
        require(
            metrics["input_tokens"]
            == metrics["ordinary_input_tokens"]
            + metrics["cache_write_input_tokens"]
            + metrics["cache_read_input_tokens"],
            f"{key}: input accounting",
        )
        require(metrics["uncached_input_tokens"] == metrics["ordinary_input_tokens"] + metrics["cache_write_input_tokens"], f"{key}: uncached accounting")
        require(metrics["total_tokens"] == metrics["input_tokens"] + metrics["output_tokens"], f"{key}: total accounting")
        require(metrics["tool_calls"]["total"] == sum(metrics["tool_calls"]["by_type"].values()), f"{key}: tool accounting")
        for reference_name in ("prompt", "developer_instructions", "terminal_patch", "episode_receipt", "transcript"):
            reference = cell[reference_name]
            require(reference["bytes"] > 0 and len(reference["sha256"]) == 64, f"{key}: {reference_name}")

    for model in ("sonnet", "spark"):
        control = matched["conditions"][f"A6_AS_{model}"]
        treatment = matched["conditions"][f"T6_AS_{model}"]
        require(
            control["runtime_preflight_summary"]
            == treatment["runtime_preflight_summary"],
            f"{model}: A/T runtime parity",
        )
        observed = matched["effects_percent_vs_A6"][model]
        for name in (
            "input_tokens", "uncached_input_tokens", "cache_read_input_tokens",
            "output_tokens", "total_tokens", "elapsed_seconds", "tool_calls",
        ):
            require(observed[name] == pct(metric(control, name), metric(treatment, name)), f"{model}: {name} effect")
        if model == "sonnet":
            require(observed["cost_usd"] == pct(metric(control, "cost_usd"), metric(treatment, "cost_usd")), "sonnet: cost effect")
        else:
            require("cost_usd" not in observed, "Spark cost unavailable")
            for key in (f"A6_AS_{model}", f"T6_AS_{model}"):
                runtime = matched["conditions"][key]["runtime_preflight_summary"]
                require(runtime["distutils_compatibility_returncode"] == 0, f"{key}: distutils")
                require(runtime["per_cell_isolation_kind"] == "venv-with-shared-site-packages-on-pythonpath", f"{key}: isolation")

        decomposition = matched["input_decomposition"][model]
        require(
            decomposition["actual_delta_tokens"]
            == decomposition["treatment_cumulative_input_tokens"]
            - decomposition["control_cumulative_input_tokens"],
            f"{model}: cumulative delta",
        )
        require(
            decomposition["static_prefix_replay_tokens"]
            == (decomposition["treatment_first_context_tokens"] - decomposition["control_first_context_tokens"])
            * decomposition["treatment_request_count"],
            f"{model}: static replay",
        )
        require(
            decomposition["interaction_trajectory_delta_tokens"]
            == decomposition["actual_delta_tokens"] - decomposition["static_prefix_replay_tokens"],
            f"{model}: trajectory delta",
        )
        require(decomposition["identity_check"] is True, f"{model}: identity")
        require(decomposition["static_prefix_replay_tokens"] > 0, f"{model}: positive prefix cost")
        require(decomposition["interaction_trajectory_delta_tokens"] < 0, f"{model}: trajectory saving")
        require(decomposition["actual_delta_tokens"] < 0, f"{model}: net input saving")

    require(matched["conditions"]["A6_AS_sonnet"]["prompt"] == matched["conditions"]["A6_AS_spark"]["prompt"], "A6 prompt parity")
    require(matched["conditions"]["T6_AS_sonnet"]["prompt"] == matched["conditions"]["T6_AS_spark"]["prompt"], "T6 prompt parity")
    require(matched["conditions"]["A6_AS_sonnet"]["developer_instructions"] == matched["conditions"]["A6_AS_spark"]["developer_instructions"], "A6 developer parity")
    require(matched["conditions"]["T6_AS_sonnet"]["developer_instructions"] == matched["conditions"]["T6_AS_spark"]["developer_instructions"], "T6 developer parity")


def verify_external(data: dict[str, Any], manifest: dict[str, Any], evidence_root: Path) -> int:
    artifacts = {}
    for reference in manifest["artifacts"]:
        path = evidence_root / reference["path"]
        payload = path.read_bytes()
        require(len(payload) == reference["bytes"], f"{reference['role']}: bytes")
        require(digest(payload) == reference["sha256"], f"{reference['role']}: SHA")
        artifacts[reference["role"]] = path
    require(len(artifacts) == len(manifest["artifacts"]), "external artifact roles are unique")
    external = load(artifacts["external replay analysis"])
    require(external["schema_version"] == "issue836-prompt-replay-mechanism-v2", "external replay schema")
    require(external["canonical_strategy_factorial"]["comparisons"] == data["canonical_strategy_factorial"]["comparisons"], "external canonical comparisons")
    require(external["canonical_strategy_factorial"]["aggregate_exact_only"] == data["canonical_strategy_factorial"]["aggregate_exact_only"], "external exact aggregate")
    require(external["matched_micro_gate"]["effects_percent_vs_A6"] == data["matched_micro_gate"]["effects_percent_vs_A6"], "external matched effects")
    require(external["matched_micro_gate"]["input_decomposition"] == data["matched_micro_gate"]["input_decomposition"], "external matched decomposition")
    for key, cell in data["matched_micro_gate"]["conditions"].items():
        external_cell = external["matched_micro_gate"]["conditions"][key]
        require(external_cell["metrics"] == cell["metrics"], f"{key}: external metrics")
        require(path_free_ref(external_cell["prompt"]) == cell["prompt"], f"{key}: external prompt")
        require(path_free_ref(external_cell["developer_instructions"]) == cell["developer_instructions"], f"{key}: external developer")
        require(path_free_ref(external_cell["terminal_patch"]) == cell["terminal_patch"], f"{key}: external patch")
        require(external_cell["official_verdict"] == cell["official_verdict"], f"{key}: external verdict")
    for model in ("sonnet", "spark"):
        status = load(artifacts[f"T6 {model.capitalize()} official evaluation status"])
        require(status["failure_count"] == 0 and status["episode_count"] == 2, f"{model}: official status")
        require({episode["verdict"] for episode in status["episodes"]} == {"RESOLVED"}, f"{model}: official verdicts")
        audit = load(artifacts[f"T6 {model.capitalize()} transcript audit"])
        require(audit["status"] == "PASS" and len(audit["rows"]) == 2, f"{model}: transcript audit")
    preflight = load(artifacts["T6 zero-spend runtime preflight"])
    require(preflight["status"] == "PASS" and preflight["provider_processes_started"] == 0, "runtime preflight")
    run_manifest = load(artifacts["T6 run manifest"])
    require(run_manifest["episode_count"] == 4 and run_manifest["scale_authorized"] is False, "run manifest scope")
    return len(artifacts)


def main() -> int:
    verify_package_digests()
    data = load(RESULTS)
    manifest = load(MANIFEST)
    report = REPORT.read_text()
    method = METHOD.read_text()
    require(data["schema_version"] == "issue836-prompt-replay-analysis-v1", "results schema")
    require(data["status"] == "MATCHED_MICRO_GATE_COMPLETE_NO_SCALE", "results status")
    verify_canonical(data)
    verify_matched(data)
    require("## Why helpful context still increased billed input" in report, "mechanism report section")
    require("## Matched A6/T6 micro-gate" in report, "matched report section")
    require("### Matched A6/T6 causal-replay micro-gate" in method, "matched method section")
    require(manifest["schema_version"] == "issue836-prompt-replay-analysis-evidence-manifest-v1", "manifest schema")
    require(len(manifest["artifacts"]) == 18, "manifest artifact count")
    external_count = 0
    configured_root = os.environ.get(manifest["evidence_root_environment"])
    if configured_root:
        external_count = verify_external(data, manifest, Path(configured_root).resolve(strict=True))
    print(
        f"PASS: prompt replay analysis verified ({assertions} assertions; "
        f"external artifacts {external_count}/{len(manifest['artifacts'])})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
