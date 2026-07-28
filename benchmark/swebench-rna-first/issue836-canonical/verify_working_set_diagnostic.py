#!/usr/bin/env python3
"""Verify the bounded rank-3 working-set diagnostic and retained evidence."""

from __future__ import annotations

import hashlib
import json
import math
import os
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "working-set-diagnostic-results.json"
MANIFEST = ROOT / "working-set-diagnostic-evidence-manifest.json"
CANONICAL = ROOT / "humanlayer-strategy-results.json"
REPORT = ROOT / "REPORT.md"
CONDITIONS = {
    "A_AS_sonnet",
    "A_AS_spark",
    "T3_AS_sonnet",
    "T3_AS_spark",
    "T4_AS_sonnet",
    "T4_AS_spark",
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


def close(left: float, right: float) -> bool:
    return math.isclose(left, right, rel_tol=0.0, abs_tol=1e-9)


def pct(left: float, right: float) -> float:
    return round((right / left - 1.0) * 100.0, 1)


def metric(cell: dict[str, Any], name: str) -> float:
    metrics = cell["metrics"]
    if name == "tool_calls":
        return metrics["tool_calls"]["total"]
    return metrics[name]


def verify_cell(key: str, cell: dict[str, Any]) -> None:
    metrics = cell["metrics"]
    require(cell["official_verdict"] == "RESOLVED", f"{key}: verdict")
    require(metrics["input_tokens"] == (
        metrics["ordinary_input_tokens"]
        + metrics["cache_write_input_tokens"]
        + metrics["cache_read_input_tokens"]
    ), f"{key}: input accounting")
    require(metrics["uncached_input_tokens"] == (
        metrics["ordinary_input_tokens"] + metrics["cache_write_input_tokens"]
    ), f"{key}: uncached accounting")
    require(metrics["total_tokens"] == metrics["input_tokens"] + metrics["output_tokens"], f"{key}: total tokens")
    require(metrics["tool_calls"]["total"] == sum(metrics["tool_calls"]["by_type"].values()), f"{key}: tools")
    for name in ("prompt", "developer_instructions"):
        reference = cell[name]
        require(reference["bytes"] > 0, f"{key}: {name} bytes")
        require(len(reference["sha256"]) == 64, f"{key}: {name} SHA")


def verify_canonical_controls(data: dict[str, Any], canonical: dict[str, Any]) -> None:
    rank = next(row for row in canonical["rows"] if row["rank"] == 3)
    for model in ("sonnet", "spark"):
        key = f"A_AS_{model}"
        observed = data["conditions"][key]
        frozen = rank["conditions"][key]
        require(observed["official_verdict"] == frozen["official"]["verdict"], f"{key}: frozen verdict")
        require(close(observed["metrics"]["elapsed_seconds"], frozen["metrics"]["elapsed_seconds"]), f"{key}: frozen time")
        for name in ("input_tokens", "output_tokens", "total_tokens", "cost_usd"):
            left = observed["metrics"][name]
            right = frozen["metrics"][name]
            require(left == right or (left is not None and right is not None and close(left, right)), f"{key}: frozen {name}")
        breakdown = frozen["metrics"]["token_breakdown"]
        for name in (
            "ordinary_input_tokens",
            "cache_write_input_tokens",
            "cache_read_input_tokens",
            "uncached_input_tokens",
        ):
            require(observed["metrics"][name] == breakdown[name], f"{key}: frozen {name}")
        require(observed["metrics"]["tool_calls"] == frozen["metrics"]["tool_calls"], f"{key}: frozen tools")


def verify_effects(data: dict[str, Any]) -> None:
    fields = (
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "elapsed_seconds",
        "tool_calls",
        "tool_result_characters",
        "cache_read_input_tokens",
        "uncached_input_tokens",
    )
    for model in ("sonnet", "spark"):
        control = data["conditions"][f"A_AS_{model}"]
        for treatment in ("T3", "T4"):
            comparison = data["conditions"][f"{treatment}_AS_{model}"]
            observed = data["effects_percent_vs_A"][model][treatment]
            for name in fields:
                require(observed[name] == pct(metric(control, name), metric(comparison, name)), f"{model} {treatment}: {name} effect")
            if model == "sonnet":
                require(observed["cost_usd"] == pct(metric(control, "cost_usd"), metric(comparison, "cost_usd")), f"{model} {treatment}: cost effect")
            else:
                require("cost_usd" not in observed, f"{model} {treatment}: unavailable cost")


def verify_external(data: dict[str, Any], manifest: dict[str, Any], evidence_root: Path) -> int:
    artifacts: dict[str, Path] = {}
    for reference in manifest["artifacts"]:
        path = evidence_root / reference["path"]
        payload = path.read_bytes()
        require(len(payload) == reference["bytes"], f"{reference['role']}: bytes")
        require(digest(payload) == reference["sha256"], f"{reference['role']}: SHA")
        artifacts[reference["role"]] = path

    for treatment in ("T3", "T4"):
        for model in ("sonnet", "spark"):
            key = f"{treatment}_AS_{model}"
            audit = load(artifacts[f"{key} transcript audit"])
            require(audit["status"] == "PASS" and len(audit["rows"]) == 1, f"{key}: audit status")
            row = audit["rows"][0]
            require(row["key"] == key and row["status"] == "PASS", f"{key}: audit identity")
            cell = data["conditions"][key]
            usage = row["token_usage"]
            require(cell["prompt"] == row["prompt"], f"{key}: prompt evidence")
            require(cell["developer_instructions"] == row["developer_instructions"], f"{key}: developer evidence")
            require(cell["patch_sha256"] == row["patch"]["sha256"], f"{key}: patch evidence")
            require(close(cell["metrics"]["elapsed_seconds"], row["elapsed_seconds"]), f"{key}: time evidence")
            require(cell["metrics"]["input_tokens"] == usage["input_tokens"], f"{key}: input evidence")
            require(cell["metrics"]["output_tokens"] == usage["output_tokens"], f"{key}: output evidence")
            require(cell["metrics"]["total_tokens"] == usage["total_tokens"], f"{key}: total evidence")
            require(cell["metrics"]["tool_calls"] == row["tool_calls"], f"{key}: tool evidence")
            require(cell["metrics"]["tool_result_characters"] == row["tool_result_output"]["characters"], f"{key}: tool-output evidence")
            status = load(artifacts[f"{key} official evaluation status"])
            require(status["failure_count"] == 0 and len(status["episodes"]) == 1, f"{key}: official status")
            episode = status["episodes"][0]
            require(episode["provider"] == key and episode["verdict"] == cell["official_verdict"], f"{key}: official verdict")
            require(episode["terminal_patch_sha256"] == cell["patch_sha256"], f"{key}: official patch")
    for treatment in ("T3", "T4"):
        prompt_audit = load(artifacts[f"{treatment} prompt audit"])
        require(prompt_audit["status"] == "PASS", f"{treatment}: prompt audit")
    return len(manifest["artifacts"])


def main() -> int:
    data = load(RESULTS)
    manifest = load(MANIFEST)
    canonical = load(CANONICAL)
    report = REPORT.read_text()
    require(data["schema_version"] == "issue836-working-set-diagnostic-results-v1", "results schema")
    require(data["status"] == "TWO_CELL_T4_GATE_COMPLETE_NO_SCALE", "results status")
    require(data["paid_diagnostic_cell_count"] == 4, "paid diagnostic count")
    require(data["case"] == {"instance_id": "sympy__sympy-13757", "rank": 3}, "case identity")
    require(set(data["conditions"]) == CONDITIONS, "condition inventory")
    require(data["design_diagnosis"]["scale_decision"] == "DO_NOT_SCALE", "scale decision")
    require("## Post-hoc working-set gate: stopped after one case" in report, "report section")
    require("**DO NOT SCALE T4**" in report, "report decision")
    for key, cell in data["conditions"].items():
        verify_cell(key, cell)
    for treatment in ("T3", "T4"):
        require(data["conditions"][f"{treatment}_AS_sonnet"]["prompt"] == data["conditions"][f"{treatment}_AS_spark"]["prompt"], f"{treatment}: prompt parity")
        require(data["conditions"][f"{treatment}_AS_sonnet"]["developer_instructions"] == data["conditions"][f"{treatment}_AS_spark"]["developer_instructions"], f"{treatment}: developer parity")
    verify_canonical_controls(data, canonical)
    verify_effects(data)
    require(manifest["schema_version"] == "issue836-working-set-diagnostic-evidence-manifest-v1", "manifest schema")
    require(len(manifest["artifacts"]) == 12, "manifest artifact count")
    external_count = 0
    configured_root = os.environ.get(manifest["evidence_root_environment"])
    if configured_root:
        external_count = verify_external(data, manifest, Path(configured_root).resolve(strict=True))
    print(
        f"PASS: working-set diagnostic verified ({assertions} assertions; "
        f"external artifacts {external_count}/{len(manifest['artifacts'])})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
