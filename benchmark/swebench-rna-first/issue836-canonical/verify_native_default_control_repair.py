#!/usr/bin/env python3
"""Verify the 20-case native-default A_prime/T_PD repair comparison."""

from __future__ import annotations

from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import statistics
from typing import Any


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "native-default-control-repair-results.json"
MANIFEST = ROOT / "native-default-control-repair-evidence-manifest.json"
T_PD_RESULTS = ROOT / "bounded-progressive-disclosure-results.json"
T_PD_MANIFEST = ROOT / "bounded-progressive-disclosure-evidence-manifest.json"
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
EXPECTED_RESULTS_SHA = "5670f88f84d7538b6de1df1dcc8254adde4c1dcbc1b7105ca3f89ce86ff34993"
EXPECTED_MANIFEST_SHA = "f817457e9887255a562a90872091c9b639b2980e747144fa557f5e8d7a157a9c"
EXPECTED_T_PD_RESULTS_SHA = "f56f7967debf7ac78342b4a31e8b69760168ba6d3438662ac26df78baf385c0b"
EXPECTED_REPORT_SHA = "588a983ffabea18000da4eb82fbb753bfdaa967241a9c6f8e15ac57b49368052"
EXPECTED_METHOD_SHA = "a9e60195fd3874e9be03cc35da0fd1eebb32986d0573272b844da13ab0064e72"
RANKS = tuple(range(1, 21))
METRICS = (
    "direct_input_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "total_input_tokens",
    "output_tokens",
    "cost_usd",
    "elapsed_seconds",
    "tool_calls",
    "end_to_end_seconds",
)


class VerificationFailure(RuntimeError):
    pass


assertions = 0


def require(value: bool, message: str) -> None:
    global assertions
    assertions += 1
    if not value:
        raise VerificationFailure(message)


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


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
        return float(row[name]["total"])
    if name == "end_to_end_seconds":
        return float(row.get(name, row["elapsed_seconds"]))
    return float(row[name])


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


def normalize_command(command: list[str], *, remove_treatment: bool) -> list[str]:
    normalized: list[str] = []
    skip = False
    for index, value in enumerate(command):
        if skip:
            skip = False
            continue
        if remove_treatment and value == "--append-system-prompt-file":
            skip = True
            continue
        if value == "--session-id" and index + 1 < len(command):
            normalized.extend([value, "<SESSION>"])
            skip = True
            continue
        normalized.append(value)
    return normalized


def init_surface(path: Path) -> dict[str, Any]:
    event = json.loads(path.read_text().splitlines()[0])
    require(event.get("type") == "system" and event.get("subtype") == "init", f"{path.name}: init event")
    return {
        key: event.get(key)
        for key in (
            "tools",
            "mcp_servers",
            "model",
            "permissionMode",
            "claude_code_version",
            "output_style",
            "agents",
            "skills",
            "plugins",
            "fast_mode_state",
        )
    }


def verify_package_digests() -> None:
    for path, expected in (
        (RESULTS, EXPECTED_RESULTS_SHA),
        (MANIFEST, EXPECTED_MANIFEST_SHA),
        (T_PD_RESULTS, EXPECTED_T_PD_RESULTS_SHA),
        (REPORT, EXPECTED_REPORT_SHA),
        (METHOD, EXPECTED_METHOD_SHA),
    ):
        require(digest(path.read_bytes()) == expected, f"{path.name}: package digest drift")


def verify_offline(data: dict[str, Any], manifest: dict[str, Any]) -> None:
    require(data["schema_version"] == "issue836-native-default-control-repair-v1", "results schema")
    require(data["status"] == "TWENTY_CASE_NATIVE_DEFAULT_CONTROL_REPAIR_COMPLETE", "results status")
    require(data["model"] == "claude-sonnet-5", "model identity")
    require(not absolute_strings(data), "results contain absolute paths")
    require(tuple(data["scope"]["ranks"]) == RANKS, "rank scope")
    require(data["scope"]["new_control_episodes"] == 20, "new control count")
    require(data["scope"]["reused_treatment_episodes"] == 20, "reused treatment count")
    require(data["scope"]["officially_evaluated_pairs"] == 20, "evaluation count")
    require(data["scope"]["provider_failure_invocations_excluded_before_sonnet_output"] == 19, "provider failure disclosure")
    require(data["source_t_pd_ledger"] == {"bytes": T_PD_RESULTS.stat().st_size, "sha256": digest(T_PD_RESULTS.read_bytes())}, "T_PD ledger binding")
    failures = data["provider_failures"]["invocations"]
    require(len(failures) == 19 and tuple(row["sequence"] for row in failures) == tuple(range(1, 20)), "provider failure inventory")
    for failure in failures:
        require(failure["api_error_status"] == 529, f"provider failure {failure['sequence']}: status")
        require(failure["successful_completion"] is False and failure["returncode"] == 1, f"provider failure {failure['sequence']}: completion")
        require(failure["main_model_usage_present"] is False, f"provider failure {failure['sequence']}: Sonnet usage")
        require(failure["tool_call_count"] == 0 and failure["terminal_patch_bytes"] == 0, f"provider failure {failure['sequence']}: output")
    require(
        close(
            data["provider_failures"]["total_provider_reported_cost_usd"],
            sum(row["provider_reported_cost_usd"] for row in failures),
        ),
        "provider failure cost",
    )

    cases = data["cases"]
    require(len(cases) == 20 and tuple(case["rank"] for case in cases) == RANKS, "case inventory")
    old_by_rank = {case["rank"]: case for case in load(T_PD_RESULTS)["cases"]}
    for case in cases:
        rank = case["rank"]
        require(case["T_PD"] == old_by_rank[rank]["T_PD"], f"rank {rank}: immutable T_PD reuse")
        for arm in ("A_prime", "T_PD"):
            row = case[arm]
            require(
                row["total_input_tokens"]
                == row["direct_input_tokens"] + row["cache_creation_input_tokens"] + row["cache_read_input_tokens"],
                f"rank {rank} {arm}: input accounting",
            )
            require(row["tool_calls"]["total"] == sum(row["tool_calls"]["by_type"].values()), f"rank {rank} {arm}: tool accounting")
        control = case["A_prime"]
        require(control["provider_retry_count"] == 0, f"rank {rank}: provider retries")
        require(control["command_parity_minus_treatment_append"] is True, f"rank {rank}: command parity")
        require(control["init_surface_parity"] is True, f"rank {rank}: init parity")
        treatment = case["T_PD"]
        require(close(treatment["end_to_end_seconds"], treatment["elapsed_seconds"] + treatment["rna_retrieval_seconds"]), f"rank {rank}: end-to-end accounting")
        for name in METRICS:
            observed = case["delta_percent"][name]
            require(close(observed, pct(metric(treatment, name), metric(control, name))), f"rank {rank}: delta {name}")

    expected_a = aggregate(cases, "A_prime")
    expected_t = aggregate(cases, "T_PD")
    for arm, expected in (("A_prime", expected_a), ("T_PD", expected_t)):
        observed = data["aggregate"][arm]
        for field, value in expected.items():
            if isinstance(value, float):
                require(close(observed[field], value), f"{arm}: aggregate {field}")
            else:
                require(observed[field] == value, f"{arm}: aggregate {field}")
    require(expected_a["resolved"] == 16 and expected_t["resolved"] == 17, "aggregate efficacy")
    effects = data["aggregate"]["T_PD_vs_A_prime_total_delta_percent"]
    for name in METRICS:
        require(close(effects[name], pct(metric(expected_t, name), metric(expected_a, name))), f"aggregate delta {name}")
        deltas = [case["delta_percent"][name] for case in cases]
        require(close(data["aggregate"]["paired_case_median_delta_percent"][name], statistics.median(deltas)), f"median delta {name}")
        require(data["aggregate"]["cases_with_lower_metric"][name] == sum(value < 0 for value in deltas), f"lower count {name}")
    discordances = [
        {"rank": case["rank"], "instance_id": case["instance_id"], "A_prime": case["A_prime"]["official_verdict"], "T_PD": case["T_PD"]["official_verdict"]}
        for case in cases
        if case["A_prime"]["official_verdict"] != case["T_PD"]["official_verdict"]
    ]
    require(data["aggregate"]["efficacy_matches"] == 20 - len(discordances) == 19, "efficacy match count")
    require(data["aggregate"]["efficacy_discordances"] == discordances, "efficacy discordances")
    require(discordances == [{"rank": 6, "instance_id": "django__django-13794", "A_prime": "UNRESOLVED", "T_PD": "RESOLVED"}], "rank 6 discordance")
    require(close(data["aggregate"]["efficacy_exact_mcnemar_two_sided_p"], 1.0), "efficacy exact test")

    jackknife = data["sensitivity"]["leave_one_rank_out"]
    require(len(jackknife) == 20 and tuple(row["omitted_rank"] for row in jackknife) == RANKS, "jackknife inventory")
    for observed in jackknife:
        selected = [case for case in cases if case["rank"] != observed["omitted_rank"]]
        aa, tt = aggregate(selected, "A_prime"), aggregate(selected, "T_PD")
        require(observed["A_prime_resolved"] == aa["resolved"] and observed["T_PD_resolved"] == tt["resolved"], f"jackknife rank {observed['omitted_rank']}: efficacy")
        for name in METRICS:
            require(close(observed[f"{name}_delta_percent"], pct(metric(tt, name), metric(aa, name))), f"jackknife rank {observed['omitted_rank']}: {name}")
    for name in METRICS:
        values = [row[f"{name}_delta_percent"] for row in jackknife]
        require(close(data["sensitivity"]["ranges"][name]["min"], min(values)), f"sensitivity min {name}")
        require(close(data["sensitivity"]["ranges"][name]["max"], max(values)), f"sensitivity max {name}")

    require(manifest["schema_version"] == "issue836-native-default-control-repair-evidence-manifest-v1", "manifest schema")
    require(manifest["evidence_root_environment"] == "ISSUE836_EVIDENCE_ROOT", "evidence environment")
    require(manifest["artifact_count"] == len(manifest["artifacts"]) == 122, "artifact count")
    roles = [item["role"] for item in manifest["artifacts"]]
    require(len(roles) == len(set(roles)), "artifact roles unique")
    for item in manifest["artifacts"]:
        path = PurePosixPath(item["path"])
        require(not path.is_absolute() and ".." not in path.parts, f"unsafe evidence path: {item['role']}")
        require(item["bytes"] > 0 and len(item["sha256"]) == 64, f"invalid evidence reference: {item['role']}")

    report = REPORT.read_text()
    require("### Per-case A_prime/T_PD details" in report, "report detail section")
    for arm in ("A_prime", "T_PD"):
        row = data["aggregate"][arm]
        tools = ", ".join(f"{name}={count}" for name, count in row["tool_calls"]["by_type"].items())
        rna = "—" if arm == "A_prime" else f"{row['rna_retrieval_seconds']:.1f}s"
        report_row = (
            f"| {arm} | {row['resolved']}/20 | {row['elapsed_seconds']:.1f}s | {rna} | "
            f"{row.get('end_to_end_seconds', row['elapsed_seconds']):.1f}s | {row['direct_input_tokens']:,} | "
            f"{row['cache_creation_input_tokens']:,} | {row['cache_read_input_tokens']:,} | {row['total_input_tokens']:,} | "
            f"{row['output_tokens']:,} | ${row['cost_usd']:.6f} | {row['tool_calls']['total']} | {tools} |"
        )
        require(report_row in report, f"report aggregate row: {arm}")
    for case in cases:
        for arm in ("A_prime", "T_PD"):
            row = case[arm]
            tools = ", ".join(f"{name}={count}" for name, count in row["tool_calls"]["by_type"].items())
            report_row = (
                f"| {case['rank']} | {arm} | {row['official_verdict']} | {row['elapsed_seconds']:.1f} / "
                f"{row.get('rna_retrieval_seconds', 0):.1f} / {row.get('end_to_end_seconds', row['elapsed_seconds']):.1f} | "
                f"{row['direct_input_tokens']:,} / {row['cache_creation_input_tokens']:,} / {row['cache_read_input_tokens']:,} | "
                f"{row['total_input_tokens']:,} / {row['output_tokens']:,} | ${row['cost_usd']:.6f} | "
                f"{row['tool_calls']['total']} ({tools}) |"
            )
            require(report_row in report, f"rank {case['rank']} {arm}: report detail row")
    require("### Twenty-case bounded progressive-disclosure treatment" in METHOD.read_text(), "method section")
    require("prompt-channel mismatch" in METHOD.read_text(), "method mismatch disclosure")


def verify_external(data: dict[str, Any], manifest: dict[str, Any], evidence_root: Path) -> int:
    artifacts: dict[str, Path] = {}
    for reference in manifest["artifacts"]:
        path = evidence_root / reference["path"]
        payload = path.read_bytes()
        require(len(payload) == reference["bytes"], f"{reference['role']}: bytes")
        require(digest(payload) == reference["sha256"], f"{reference['role']}: SHA")
        artifacts[reference["role"]] = path

    admitted = load(artifacts["admitted-cohort.json"])
    require(admitted["admitted"] == 20 and len(admitted["rows"]) == 20, "admitted cohort count")
    admitted_by_rank = {row["rank"]: row for row in admitted["rows"]}
    t_manifest = load(T_PD_MANIFEST)
    t_paths = {item["role"]: evidence_root / item["path"] for item in t_manifest["artifacts"]}
    for failure in data["provider_failures"]["invocations"]:
        receipt = load(artifacts[f"provider failure invocation {failure['sequence']:02d} receipt"])
        require(receipt["successful_completion"] is False and receipt["returncode"] == 1, f"provider failure {failure['sequence']}: receipt completion")
        require(receipt["result_metadata"]["api_error_status"] == 529, f"provider failure {failure['sequence']}: receipt status")
        require("claude-sonnet-5" not in receipt["result_metadata"]["modelUsage"], f"provider failure {failure['sequence']}: receipt Sonnet usage")
        require(receipt["tool_call_count"] == 0 and receipt["terminal_patch"]["bytes"] == 0, f"provider failure {failure['sequence']}: receipt output")
        require(close(receipt["result_metadata"]["total_cost_usd"], failure["provider_reported_cost_usd"]), f"provider failure {failure['sequence']}: receipt cost")
    for case in data["cases"]:
        rank = case["rank"]
        row = case["A_prime"]
        source = admitted_by_rank[rank]
        receipt = load(artifacts[f"rank {rank:02d} A_prime episode receipt"])
        official = load(artifacts[f"rank {rank:02d} A_prime official evaluation receipt"])
        transcript = artifacts[f"rank {rank:02d} A_prime provider transcript"]
        prompt = artifacts[f"rank {rank:02d} A_prime user prompt"].read_bytes()
        patch = artifacts[f"rank {rank:02d} A_prime terminal patch"].read_bytes()
        require(receipt["successful_completion"] is True and not receipt["timed_out"] and receipt["returncode"] == 0, f"rank {rank}: completion")
        require(receipt["result_metadata"]["api_error_status"] is None, f"rank {rank}: provider status")
        require(receipt["result_metadata"]["permission_denials"] == [], f"rank {rank}: permission denials")
        require(patch and digest(patch) == source["terminal_patch"]["sha256"], f"rank {rank}: terminal patch")
        require(
            b"rna_tool_search(" not in prompt
            and b"Prefer RNA" not in prompt
            and b"repository-native context" not in prompt,
            f"rank {rank}: clean control prompt",
        )
        usage = usage_metrics(receipt["result_metadata"]["modelUsage"])
        for name, value in usage.items():
            require(row[name] == value, f"rank {rank}: {name}")
        require(close(row["elapsed_seconds"], receipt["elapsed_seconds"]), f"rank {rank}: elapsed")
        require(close(row["cost_usd"], receipt["result_metadata"]["total_cost_usd"]), f"rank {rank}: cost")
        require(close(row["cost_usd"], sum(float(item["costUSD"]) for item in receipt["result_metadata"]["modelUsage"].values())), f"rank {rank}: per-model cost cross-check")
        require(row["tool_calls"] == {"total": receipt["tool_call_count"], "by_type": receipt["tool_counts"]}, f"rank {rank}: tools")
        require(official["status"] == "completed" and official["verdict"] == row["official_verdict"], f"rank {rank}: official verdict")
        require(official["paid_model_calls"] == 0 and official["returncode"] == 0, f"rank {rank}: official evaluator")
        t_receipt = load(t_paths[f"rank {rank:02d} T_PD episode receipt"])
        require(normalize_command(receipt["command"], remove_treatment=False) == normalize_command(t_receipt["command"], remove_treatment=True), f"rank {rank}: independent command parity")
        require(init_surface(transcript) == init_surface(t_paths[f"rank {rank:02d} T_PD provider transcript"]), f"rank {rank}: independent init parity")
    return len(artifacts)


def main() -> int:
    global assertions
    assertions = 0
    verify_package_digests()
    data = load(RESULTS)
    manifest = load(MANIFEST)
    verify_offline(data, manifest)
    external_count = 0
    configured = os.environ.get(manifest["evidence_root_environment"])
    if configured:
        external_count = verify_external(data, manifest, Path(configured).resolve(strict=True))
    print(
        f"PASS: native-default control repair verified ({assertions} assertions; "
        f"external artifacts {external_count}/{len(manifest['artifacts'])})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
