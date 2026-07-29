#!/usr/bin/env python3
"""Verify the complete 20-case Sonnet bounded RNA treatment cohort."""

from __future__ import annotations

from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
from typing import Any


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "bounded-progressive-disclosure-results.json"
MANIFEST = ROOT / "bounded-progressive-disclosure-evidence-manifest.json"
CANONICAL_RESULTS = ROOT / "results.json"
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
EXPECTED_RESULTS_SHA = "f56f7967debf7ac78342b4a31e8b69760168ba6d3438662ac26df78baf385c0b"
EXPECTED_MANIFEST_SHA = "41c8e5eb4d13172c494225302ffb55bf8166c6808bde81b5bd096a5f29271e76"
EXPECTED_REPORT_SHA = "588a983ffabea18000da4eb82fbb753bfdaa967241a9c6f8e15ac57b49368052"
EXPECTED_METHOD_SHA = "a9e60195fd3874e9be03cc35da0fd1eebb32986d0573272b844da13ab0064e72"
EXPECTED_CANONICAL_RESULTS_SHA = "20ad9fcff75b91c5e86147de3cd2fbb63d582aec1950e3cbd0cca0e35d8a8a17"
RANKS = tuple(range(1, 21))


class VerificationFailure(RuntimeError):
    pass


assertions = 0


def require(value: bool, message: str) -> None:
    global assertions
    assertions += 1
    if not value:
        raise VerificationFailure(message)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    require(isinstance(value, dict), f"{path.name}: root is not an object")
    return value


def close(left: float, right: float) -> bool:
    return math.isclose(left, right, rel_tol=0.0, abs_tol=1e-9)


def pct(after: float, before: float) -> float:
    return 100.0 * (after - before) / before


def metric(row: dict[str, Any], name: str) -> float:
    if name == "tool_calls":
        return float(row["tool_calls"]["total"])
    return float(row[name])


def path_free(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: path_free(item) for key, item in value.items() if key != "path"}
    if isinstance(value, list):
        return [path_free(item) for item in value]
    if isinstance(value, str) and value.startswith("/"):
        marker = "/checkout/"
        require(marker in value, f"unexpected absolute source string: {value}")
        return value.split(marker, 1)[1]
    return value


def absolute_strings(value: Any) -> list[str]:
    if isinstance(value, dict):
        return [found for item in value.values() for found in absolute_strings(item)]
    if isinstance(value, list):
        return [found for item in value for found in absolute_strings(item)]
    if isinstance(value, str) and value.startswith("/"):
        return [value]
    return []


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


def aggregate(cases: list[dict[str, Any]], arm: str) -> dict[str, Any]:
    rows = [case[arm] for case in cases]
    tools: Counter[str] = Counter()
    for row in rows:
        tools.update(row["tool_calls"]["by_type"])
    result = {
        "cases": len(rows),
        "resolved": sum(row["official_verdict"] == "RESOLVED" for row in rows),
        "unresolved": sum(row["official_verdict"] == "UNRESOLVED" for row in rows),
        "elapsed_seconds": sum(float(row["elapsed_seconds"]) for row in rows),
        "direct_input_tokens": sum(int(row["direct_input_tokens"]) for row in rows),
        "cache_creation_input_tokens": sum(int(row["cache_creation_input_tokens"]) for row in rows),
        "cache_read_input_tokens": sum(int(row["cache_read_input_tokens"]) for row in rows),
        "total_input_tokens": sum(int(row["total_input_tokens"]) for row in rows),
        "output_tokens": sum(int(row["output_tokens"]) for row in rows),
        "cost_usd": sum(float(row["cost_usd"]) for row in rows),
        "tool_calls": {"total": sum(tools.values()), "by_type": dict(sorted(tools.items()))},
    }
    if arm == "T_PD":
        result["rna_retrieval_seconds"] = sum(float(row["rna_retrieval_seconds"]) for row in rows)
        result["end_to_end_seconds"] = sum(float(row["end_to_end_seconds"]) for row in rows)
    return result


def verify_package_digests() -> None:
    for path, expected in (
        (RESULTS, EXPECTED_RESULTS_SHA),
        (MANIFEST, EXPECTED_MANIFEST_SHA),
        (CANONICAL_RESULTS, EXPECTED_CANONICAL_RESULTS_SHA),
        (REPORT, EXPECTED_REPORT_SHA),
        (METHOD, EXPECTED_METHOD_SHA),
    ):
        require(digest(path.read_bytes()) == expected, f"{path.name}: package digest drift")


def verify_offline(data: dict[str, Any], manifest: dict[str, Any]) -> None:
    require(data["schema_version"] == "issue836-bounded-progressive-disclosure-population-v1", "results schema")
    require(data["status"] == "TWENTY_CASE_PAIRED_COHORT_COMPLETE", "results status")
    require(data["model"] == "claude-sonnet-5", "model identity")
    require(not absolute_strings(data), "results contain absolute paths")
    scope = data["scope"]
    require(tuple(scope["ranks"]) == RANKS, "rank scope")
    require(scope["reused_control_episodes"] == 20 and scope["new_control_episodes"] == 0, "control reuse")
    require(scope["paid_treatment_episodes"] == 20, "treatment episode count")
    require(scope["officially_evaluated_pairs"] == 20, "evaluation count")
    require(scope["rank12_cached_retrieval_exception"] is True, "rank 12 disclosure")
    cases = data["cases"]
    require(len(cases) == 20 and tuple(case["rank"] for case in cases) == RANKS, "case inventory")
    canonical_rows = {
        int(row["rank"]): row["conditions"]["A_sonnet"]
        for row in load(CANONICAL_RESULTS)["rows"]
    }
    require(tuple(sorted(canonical_rows)) == RANKS, "canonical A inventory")
    for case in cases:
        rank = case["rank"]
        canonical_a = canonical_rows[rank]
        canonical_usage = usage_metrics(canonical_a["metrics"]["token_breakdown"]["per_model"])
        require(case["A"]["official_verdict"] == canonical_a["official"]["verdict"], f"rank {rank}: canonical A verdict")
        for field, value in canonical_usage.items():
            require(case["A"][field] == value, f"rank {rank}: canonical A {field}")
        require(close(case["A"]["elapsed_seconds"], canonical_a["metrics"]["elapsed_seconds"]), f"rank {rank}: canonical A elapsed")
        require(close(case["A"]["cost_usd"], canonical_a["metrics"]["cost_usd"]), f"rank {rank}: canonical A cost")
        require(case["A"]["tool_calls"] == canonical_a["metrics"]["tool_calls"], f"rank {rank}: canonical A tools")
        require(case["A"]["episode_receipt"] == canonical_a["episode_receipt"], f"rank {rank}: canonical A receipt")
        require(case["A"]["official_verdict"] == case["T_PD"]["official_verdict"], f"rank {rank}: efficacy match")
        for arm in ("A", "T_PD"):
            row = case[arm]
            require(
                row["total_input_tokens"]
                == row["direct_input_tokens"]
                + row["cache_creation_input_tokens"]
                + row["cache_read_input_tokens"],
                f"rank {rank} {arm}: input accounting",
            )
            require(
                row["tool_calls"]["total"] == sum(row["tool_calls"]["by_type"].values()),
                f"rank {rank} {arm}: tool accounting",
            )
        treatment = case["T_PD"]
        require(treatment["pre_injected_rna_calls"] == 1, f"rank {rank}: injected RNA")
        require(treatment["model_follow_up_rna_calls"] == 0, f"rank {rank}: follow-up RNA")
        require(
            close(treatment["end_to_end_seconds"], treatment["elapsed_seconds"] + treatment["rna_retrieval_seconds"]),
            f"rank {rank}: end-to-end accounting",
        )
        require(0 < treatment["rna_visible_bytes"] <= 8192, f"rank {rank}: bounded visible RNA")
        for arm in ("A", "T_PD"):
            for field in ("episode_receipt",):
                require(case[arm][field]["bytes"] > 0 and len(case[arm][field]["sha256"]) == 64, f"rank {rank} {arm}: {field}")
        for field in ("official_evaluation", "input_manifest"):
            require(treatment[field]["bytes"] > 0 and len(treatment[field]["sha256"]) == 64, f"rank {rank}: {field}")

    expected_a = aggregate(cases, "A")
    expected_t = aggregate(cases, "T_PD")
    for arm, expected in (("A", expected_a), ("T_PD", expected_t)):
        observed = data["aggregate"][arm]
        for field, value in expected.items():
            if isinstance(value, float):
                require(close(observed[field], value), f"{arm}: aggregate {field}")
            else:
                require(observed[field] == value, f"{arm}: aggregate {field}")
    require(expected_a["resolved"] == expected_t["resolved"] == 17, "aggregate efficacy")
    effects = data["aggregate"]["T_PD_vs_A_total_delta_percent"]
    for field, value in effects.items():
        before_field = "elapsed_seconds" if field == "end_to_end_seconds" else field
        before = metric(expected_a, before_field)
        after = metric(expected_t, field)
        require(close(value, pct(after, before)), f"aggregate effect {field}")
    require(data["aggregate"]["efficacy_matches"] == 20, "efficacy match count")
    require(data["aggregate"]["manifest_guided_first_action"] == 18, "manifest-guidance count")

    require(manifest["schema_version"] == "issue836-bounded-progressive-disclosure-evidence-manifest-v1", "manifest schema")
    require(manifest["evidence_root_environment"] == "ISSUE836_EVIDENCE_ROOT", "evidence environment")
    require(manifest["artifact_count"] == len(manifest["artifacts"]) == 202, "artifact count")
    roles = [item["role"] for item in manifest["artifacts"]]
    require(len(roles) == len(set(roles)), "artifact roles unique")
    for item in manifest["artifacts"]:
        path = PurePosixPath(item["path"])
        require(not path.is_absolute() and ".." not in path.parts, f"unsafe evidence path: {item['role']}")
        require(item["bytes"] > 0 and len(item["sha256"]) == 64, f"invalid evidence reference: {item['role']}")
        require(item.get("content_scope", "full") in ("full", "prefix"), f"invalid content scope: {item['role']}")
        if item.get("content_scope") == "prefix":
            require(
                item["role"] in {
                    "rank 06 T_PD RNA preprocessing log",
                    "rank 16 T_PD RNA preprocessing log",
                    "rank 18 T_PD RNA preprocessing log",
                },
                f"unexpected prefix-scoped artifact: {item['role']}",
            )
    report = REPORT.read_text()
    require("## Bounded progressive-disclosure population run" in report, "report section")
    require("legacy A/T_PD" in report and "efficiency contrast is superseded" in report, "report supersession disclosure")
    require("native-default-control-repair-results.json" in report, "report repair ledger")
    require("### Twenty-case bounded progressive-disclosure treatment" in METHOD.read_text(), "method section")


def verify_external(data: dict[str, Any], manifest: dict[str, Any], evidence_root: Path) -> int:
    artifacts: dict[str, Path] = {}
    bound_payloads: dict[str, bytes] = {}
    for reference in manifest["artifacts"]:
        path = evidence_root / reference["path"]
        payload = path.read_bytes()
        if reference.get("content_scope") == "prefix":
            require(len(payload) >= reference["bytes"], f"{reference['role']}: prefix bytes")
            payload = payload[: reference["bytes"]]
        else:
            require(len(payload) == reference["bytes"], f"{reference['role']}: bytes")
        require(digest(payload) == reference["sha256"], f"{reference['role']}: SHA")
        artifacts[reference["role"]] = path
        bound_payloads[reference["role"]] = payload
    source = load(artifacts["source unified V10 report"])
    require(source["schema_version"] == "issue836-sonnet-a-vs-t-pd-report-v10", "source schema")
    require(path_free(source["aggregate"]) == data["aggregate"], "source aggregate")
    require(path_free(source["rows"]) == data["cases"], "source rows")
    for case in data["cases"]:
        rank = case["rank"]
        a_receipt = load(artifacts[f"rank {rank:02d} A episode receipt"])
        t_receipt = load(artifacts[f"rank {rank:02d} T_PD episode receipt"])
        official = load(artifacts[f"rank {rank:02d} T_PD official evaluation receipt"])
        require(a_receipt["successful_completion"] is True, f"rank {rank}: A completion")
        require(t_receipt["successful_completion"] is True and not t_receipt["timed_out"], f"rank {rank}: T completion")
        for arm, receipt in (("A", a_receipt), ("T_PD", t_receipt)):
            row = case[arm]
            observed_usage = usage_metrics(receipt["result_metadata"]["modelUsage"])
            for field, value in observed_usage.items():
                require(row[field] == value, f"rank {rank} {arm}: {field}")
            require(close(row["elapsed_seconds"], receipt["elapsed_seconds"]), f"rank {rank} {arm}: elapsed")
            require(close(row["cost_usd"], receipt["result_metadata"]["total_cost_usd"]), f"rank {rank} {arm}: cost")
            require(
                close(
                    row["cost_usd"],
                    sum(float(item["costUSD"]) for item in receipt["result_metadata"]["modelUsage"].values()),
                ),
                f"rank {rank} {arm}: per-model cost cross-check",
            )
            require(row["tool_calls"] == {"total": receipt["tool_call_count"], "by_type": receipt["tool_counts"]}, f"rank {rank} {arm}: tools")
        transcript = artifacts[f"rank {rank:02d} T_PD provider transcript"]
        tools, rna_calls = transcript_tools(transcript)
        require(dict(tools) == t_receipt["tool_counts"], f"rank {rank}: transcript tools")
        require(rna_calls == 0, f"rank {rank}: transcript follow-up RNA")
        calls = bound_payloads[f"rank {rank:02d} T_PD RNA preprocessing log"].decode().splitlines()
        require(len(calls) == 1, f"rank {rank}: preprocessing call count")
        call = json.loads(calls[0])
        require(close(case["T_PD"]["rna_retrieval_seconds"], call["elapsed_seconds"]), f"rank {rank}: RNA time")
        require(case["T_PD"]["rna_visible_bytes"] == call["visible_bytes"], f"rank {rank}: RNA bytes")
        require(official["status"] == "completed" and official["verdict"] == case["T_PD"]["official_verdict"], f"rank {rank}: official verdict")
        prompt = artifacts[f"rank {rank:02d} T_PD runner prompt"].read_bytes()
        require(prompt.startswith(b"rna_tool_search("), f"rank {rank}: primed prompt")
        require(artifacts[f"rank {rank:02d} T_PD bounded RNA result"].read_bytes() in prompt, f"rank {rank}: injected result")
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
        f"PASS: bounded progressive disclosure verified ({assertions} assertions; "
        f"external artifacts {external_count}/{len(manifest['artifacts'])})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
