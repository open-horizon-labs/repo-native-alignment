#!/usr/bin/env python3
"""Verify the three-case D/E compact progressive-disclosure diagnostic."""

from __future__ import annotations

from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "compact-progressive-disclosure-results.json"
MANIFEST = ROOT / "compact-progressive-disclosure-evidence-manifest.json"
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
EXPECTED_RESULTS_SHA = "afb4d909b9f502f2b871ed1bd41f34a94e578634fe463b37b14fa33a85862b06"
EXPECTED_MANIFEST_SHA = "6d35bc195184f40a26d7dbe6a5f0a9160e5e545d44428714696a843cbbae2d94"
EXPECTED_REPORT_SHA = "bac0f7ccf844f4ad936a7fd93ecbb576bc36af32cff8626da6de759fcbe6f41c"
EXPECTED_METHOD_SHA = "4751bd30bfd0e2de8f67ddfbc1690a3eb48972ff0f4de448f682cd29dfd775f2"
RANKS = (6, 16, 18)
CONDITIONS = ("A", "previous_T", "B_T_PD", "D", "E")


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


def pct(before: float, after: float) -> float:
    return 100.0 * (after - before) / before


def path_free(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: path_free(item) for key, item in value.items() if key != "path"}
    if isinstance(value, list):
        return [path_free(item) for item in value]
    return value


def verify_package_digests() -> None:
    for path, expected in (
        (RESULTS, EXPECTED_RESULTS_SHA),
        (MANIFEST, EXPECTED_MANIFEST_SHA),
        (REPORT, EXPECTED_REPORT_SHA),
        (METHOD, EXPECTED_METHOD_SHA),
    ):
        require(digest(path.read_bytes()) == expected, f"{path.name}: package digest drift")


def metric(condition: dict[str, Any], name: str) -> float:
    if name == "tool_calls":
        return float(condition["tool_calls"]["total"])
    return float(condition[name])


def aggregate(cases: list[dict[str, Any]], name: str) -> dict[str, Any]:
    rows = [case[name] for case in cases]
    tools: Counter[str] = Counter()
    for row in rows:
        tools.update(row["tool_calls"]["by_type"])
    result = {
        "cases": len(rows),
        "resolved": sum(row["official_verdict"] == "RESOLVED" for row in rows),
        "elapsed_seconds": sum(float(row["elapsed_seconds"]) for row in rows),
        "direct_input_tokens": sum(int(row["direct_input_tokens"]) for row in rows),
        "cache_creation_input_tokens": sum(int(row["cache_creation_input_tokens"]) for row in rows),
        "cache_read_input_tokens": sum(int(row["cache_read_input_tokens"]) for row in rows),
        "total_input_tokens": sum(int(row["total_input_tokens"]) for row in rows),
        "output_tokens": sum(int(row["output_tokens"]) for row in rows),
        "cost_usd": sum(float(row["cost_usd"]) for row in rows),
        "tool_calls": {"total": sum(tools.values()), "by_type": dict(sorted(tools.items()))},
    }
    if name in ("B_T_PD", "D", "E"):
        result["rna_retrieval_seconds"] = sum(float(row["rna_retrieval_seconds"]) for row in rows)
        result["end_to_end_seconds"] = sum(float(row["end_to_end_seconds"]) for row in rows)
    return result


def verify_offline(data: dict[str, Any], manifest: dict[str, Any]) -> None:
    require(data["schema_version"] == "issue836-compact-progressive-disclosure-diagnostic-v1", "results schema")
    require(data["status"] == "THREE_CASE_PILOT_COMPLETE_NO_SCALE", "diagnostic status")
    scope = data["scope"]
    require(tuple(scope["ranks"]) == RANKS, "rank inventory")
    require(scope["paid_episode_count"] == 6, "paid episode count")
    require(scope["new_control_episode_count"] == 0, "no control reruns")
    require(scope["D_episode_count"] == scope["E_episode_count"] == 3, "D/E episode count")
    require(scope["scale_authorized"] is False, "no scale authorization")
    require(data["design"]["rna_source_or_defaults_changed"] is False, "RNA unchanged")
    cases = data["cases"]
    require(len(cases) == 3 and tuple(case["rank"] for case in cases) == RANKS, "case rows")
    for case in cases:
        rank = case["rank"]
        require(case["prompt_identity"]["D_equals_E"] is True, f"rank {rank}: prompt identity flag")
        require(case["D"]["prompt"] == case["E"]["prompt"], f"rank {rank}: D/E prompt identity")
        require(case["prompt_identity"]["sha256"] == case["D"]["prompt"]["sha256"], f"rank {rank}: prompt SHA")
        for name in ("D", "E"):
            row = case[name]
            require(row["pre_injected_rna_calls"] == 1, f"rank {rank} {name}: injected RNA")
            require(row["model_follow_up_rna_calls"] == 0, f"rank {rank} {name}: follow-up RNA")
            require(
                row["total_input_tokens"]
                == row["direct_input_tokens"]
                + row["cache_creation_input_tokens"]
                + row["cache_read_input_tokens"],
                f"rank {rank} {name}: input accounting",
            )
            require(row["tool_calls"]["total"] == sum(row["tool_calls"]["by_type"].values()), f"rank {rank} {name}: tool accounting")
            require(close(row["end_to_end_seconds"], row["elapsed_seconds"] + row["rna_retrieval_seconds"]), f"rank {rank} {name}: time accounting")
            for reference in (
                "prompt", "developer_instructions", "transcript", "terminal_patch",
                "rna_injection", "rna_preprocessing_log", "episode_receipt",
                "official_evaluation", "input_manifest",
            ):
                require(row[reference]["bytes"] > 0 and len(row[reference]["sha256"]) == 64, f"rank {rank} {name}: {reference}")
        require(case["E"]["first_tool"]["name"] == ("Grep" if rank == 6 else "Read"), f"rank {rank}: first tool")

    for name in CONDITIONS:
        observed = data["aggregate"][name]
        expected = aggregate(cases, name)
        require(observed == expected, f"{name}: aggregate")

    require(data["observed"]["D_followup_rna_calls"] == 0, "D follow-up aggregate")
    require(data["observed"]["E_followup_rna_calls"] == 0, "E follow-up aggregate")
    require(data["observed"]["D_resolved"] == data["observed"]["E_resolved"] == 1, "D/E efficacy")
    for label, after, before in (
        ("E_vs_D", "E", "D"),
        ("E_vs_B_T_PD", "E", "B_T_PD"),
    ):
        observed = data["effects_percent"][label]
        for field, value in observed.items():
            expected = pct(metric(data["aggregate"][before], field), metric(data["aggregate"][after], field))
            require(close(value, expected), f"{label}: {field}")
    observed_d = data["effects_percent"]["D_vs_B_T_PD"]
    for field, value in observed_d.items():
        expected = pct(metric(data["aggregate"]["B_T_PD"], field), metric(data["aggregate"]["D"], field))
        require(close(value, expected), f"D_vs_B_T_PD: {field}")
    require(manifest["schema_version"] == "issue836-compact-progressive-disclosure-evidence-manifest-v1", "manifest schema")
    require(manifest["evidence_root_environment"] == "ISSUE836_EVIDENCE_ROOT", "evidence environment")
    require(len(manifest["artifacts"]) == 59, "artifact count")
    roles = [item["role"] for item in manifest["artifacts"]]
    require(len(roles) == len(set(roles)), "artifact roles unique")
    require("## Compact progressive-disclosure pilot" in REPORT.read_text(), "report section")
    require("### Three-case D/E compact progressive-disclosure diagnostic" in METHOD.read_text(), "method section")


def usage_metrics(model_usage: dict[str, dict[str, Any]]) -> dict[str, int]:
    direct = sum(int(item.get("inputTokens", 0)) for item in model_usage.values())
    created = sum(int(item.get("cacheCreationInputTokens", 0)) for item in model_usage.values())
    read = sum(int(item.get("cacheReadInputTokens", 0)) for item in model_usage.values())
    output = sum(int(item.get("outputTokens", 0)) for item in model_usage.values())
    return {
        "direct_input_tokens": direct,
        "cache_creation_input_tokens": created,
        "cache_read_input_tokens": read,
        "total_input_tokens": direct + created + read,
        "output_tokens": output,
    }


def transcript_tools(path: Path) -> tuple[Counter[str], int]:
    tools: Counter[str] = Counter()
    rna_calls = 0
    for line in path.read_text().splitlines():
        event = json.loads(line)
        if event.get("type") != "assistant":
            continue
        for block in event.get("message", {}).get("content", []):
            if block.get("type") != "tool_use":
                continue
            tools[block["name"]] += 1
            if "rna_tool_search" in json.dumps(block.get("input", {}), sort_keys=True):
                rna_calls += 1
    return tools, rna_calls


def verify_external(data: dict[str, Any], manifest: dict[str, Any], evidence_root: Path) -> int:
    artifacts: dict[str, Path] = {}
    for reference in manifest["artifacts"]:
        path = evidence_root / reference["path"]
        payload = path.read_bytes()
        require(len(payload) == reference["bytes"], f"{reference['role']}: bytes")
        require(digest(payload) == reference["sha256"], f"{reference['role']}: SHA")
        artifacts[reference["role"]] = path
    source = load(artifacts["source unified V8 report"])
    require(source["schema_version"] == "issue836-sonnet-a-vs-t-pd-report-v8", "source schema")
    require(source["exploratory_E_pilot"]["aggregate"] == data["aggregate"], "source aggregate")
    require(source["exploratory_D_pilot"]["D_vs_B_T_PD_delta_percent"] == data["effects_percent"]["D_vs_B_T_PD"], "source D effect")
    require(source["exploratory_E_pilot"]["E_vs_D_delta_percent"] == data["effects_percent"]["E_vs_D"], "source E/D effect")
    for label in ("D", "E"):
        summary = load(artifacts[f"{label} official summary"])
        require(summary["completed"] == 3 and summary["failed"] == 0, f"{label}: official completion")
        require(summary["resolved"] == 1 and summary["unresolved"] == 2, f"{label}: official efficacy")
    for case in data["cases"]:
        rank = case["rank"]
        d_prompt = artifacts[f"D rank {rank:02d} prompt"].read_bytes()
        e_prompt = artifacts[f"E rank {rank:02d} prompt"].read_bytes()
        require(d_prompt == e_prompt, f"rank {rank}: external prompt identity")
        require(d_prompt.startswith(b"rna_tool_search("), f"rank {rank}: priming call")
        require(b")\n\n# RNA tool search context" in d_prompt, f"rank {rank}: adjacent priming result")
        d_dev = artifacts[f"D rank {rank:02d} developer instructions"].read_text()
        e_dev = artifacts[f"E rank {rank:02d} developer instructions"].read_text()
        require("RNA use is not mandatory" in d_dev, f"rank {rank}: D optional directive")
        require("Prefer RNA over ordinary repository search" in e_dev, f"rank {rank}: E preferred directive")
        for label in ("D", "E"):
            row = case[label]
            receipt = load(artifacts[f"{label} rank {rank:02d} episode receipt"])
            official = load(artifacts[f"{label} rank {rank:02d} official evaluation receipt"])
            require(receipt["successful_completion"] is True and receipt["returncode"] == 0, f"rank {rank} {label}: provider completion")
            observed_usage = usage_metrics(receipt["result_metadata"]["modelUsage"])
            for field, value in observed_usage.items():
                require(row[field] == value, f"rank {rank} {label}: {field}")
            require(close(row["elapsed_seconds"], receipt["elapsed_seconds"]), f"rank {rank} {label}: elapsed")
            require(close(row["cost_usd"], receipt["result_metadata"]["total_cost_usd"]), f"rank {rank} {label}: cost")
            require(row["tool_calls"] == {"total": receipt["tool_call_count"], "by_type": receipt["tool_counts"]}, f"rank {rank} {label}: receipt tools")
            tools, rna_calls = transcript_tools(artifacts[f"{label} rank {rank:02d} provider transcript"])
            require(dict(tools) == receipt["tool_counts"], f"rank {rank} {label}: transcript tools")
            require(sum(tools.values()) == receipt["tool_call_count"], f"rank {rank} {label}: transcript tool total")
            require(rna_calls == 0, f"rank {rank} {label}: transcript RNA calls")
            calls = artifacts[f"{label} rank {rank:02d} RNA preprocessing log"].read_text().splitlines()
            require(len(calls) == 1, f"rank {rank} {label}: preprocessing call")
            call = json.loads(calls[0])
            require(close(row["rna_retrieval_seconds"], call["elapsed_seconds"]), f"rank {rank} {label}: RNA time")
            require(row["rna_visible_bytes"] == call["visible_bytes"], f"rank {rank} {label}: RNA bytes")
            require(official["status"] == "completed" and official["verdict"] == row["official_verdict"], f"rank {rank} {label}: verdict")
    return len(artifacts)


def main() -> int:
    global assertions
    assertions = 0
    verify_package_digests()
    data = load(RESULTS)
    manifest = load(MANIFEST)
    verify_offline(data, manifest)
    external_count = 0
    configured_root = os.environ.get(manifest["evidence_root_environment"])
    if configured_root:
        external_count = verify_external(data, manifest, Path(configured_root).resolve(strict=True))
    print(
        f"PASS: compact progressive disclosure verified ({assertions} assertions; "
        f"external artifacts {external_count}/{len(manifest['artifacts'])})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
