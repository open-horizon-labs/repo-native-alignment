#!/usr/bin/env python3
"""Fail-closed verification for the issue #836 canonical review package."""

from __future__ import annotations

from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import re
from statistics import median
from typing import Any


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results.json"
REPORT = ROOT / "REPORT.md"
MANIFEST = ROOT / "evidence-manifest.json"
REGISTRATION = ROOT.parent / "issue836-v4" / "registration.json"
SELECTION = ROOT.parent / "issue836-v4" / "selection.json"
CONDITIONS = ("A_sonnet", "A_luna", "T_sonnet", "T_luna")
BASE_SHA = "da68ef814351f2953d9954f4cc309bf755605ac4e672c3d5096106cc664e3d49"
DIRECTIVE_SHA = "f91a19798b6fbee94e3e1ae17848991154d31ad2d60317f2f0436abfe327143b"
EXPECTED_RESULTS_SHA = "20ad9fcff75b91c5e86147de3cd2fbb63d582aec1950e3cbd0cca0e35d8a8a17"
EXPECTED_REPORT_SHA = "88ab5b5a2bf90374ee41d2cdb93698310e65585a6cf22ace019ec9dd37f175a4"
EXPECTED_MANIFEST_SHA = "e0da386a3372f43b703c01658fc76eb1d6edf685cb7785aa5b9087c5195bc13f"
EXPECTED_METHOD_SHA = "fbaff4a325a4f108a949d91e3ebd9a1e7dee98f0dcceae12a238cd43c6d91488"
EXPECTED_README_SHA = "3541b6c20a620a72e355b6542f8940668bd9ca93e42b6cefe983d159c7088ca2"
EXPECTED_REGISTRATION_SHA = "2b070bb61ea2c5de6fe6b1d8cf840d6d4e53732b4e524d3f26aefc3504a6523b"
EXPECTED_SELECTION_SHA = "8d198247774ee6793c46ab61ba1b5005af90a8f6cc6abe2e54f2c613c2b26cc8"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
GIT_SHA_RE = re.compile(r"^[0-9a-f]{40}$")

EXPECTED_EXTERNAL_ARTIFACTS = {
    "canonical_report_json": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/canonical-full-20x4-report.json",
        "bytes": 530377,
        "sha256": "cc4d174c0537b5eb7fe1d06a37a582b4c212985626ac25799f0910d8a77aa57b",
    },
    "canonical_report_source_markdown": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/canonical-full-20x4-report.md",
        "bytes": 16600,
        "sha256": "5499f95468ed9c368237a3f69f2d3ad50037990e8f01b02901a852fcc3f6892c",
    },
    "canonical_validation": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/canonical-full-20x4-validation.json",
        "bytes": 781,
        "sha256": "82bbbc0227124823b6f0207c5f440ea9b0d56f809b9d46b4495382067db62604",
        "status": "PASS",
        "assertions": 2333,
        "failures": 0,
    },
    "canonical_report_builder": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/build_canonical_report.py",
        "bytes": 67483,
        "sha256": "3680d528fb3271432c880d367b16a5bd0b6394731e5a19763376f7ff6629f178",
    },
    "canonical_report_validator": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/validate_canonical_report.py",
        "bytes": 16681,
        "sha256": "e1ff843551db22e809f8f26d48dc44003320eb092cce1a6cae2e779f6ee6c252",
    },
    "luna_clean_runtime_audit": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/luna-clean-runtime-incremental-audit.json",
        "bytes": 60989,
        "sha256": "cb6ac599f0d52b2f31ee7d4c5ee75e44a89e4ca4d77260c971cdf7b225a8f6c6",
    },
    "luna_clean_rerun_matrix": {
        "path_relative_to_evidence_root": "luna-canonical-34cell-rerun-prepared-attempt-001/canonical-34cell-manifest.json",
        "bytes": 79937,
        "sha256": "b4b52989f21deb03bf355da31c4a38a6104c3d7b07f11be7b4c38a1a2360618f",
        "cells": 34,
    },
    "luna_clean_exact_patch_verdicts": {
        "path_relative_to_evidence_root": "full-20x4-canonical-report-attempt-001/official-evaluation/luna-clean-rerun-exact-patch-verdicts.json",
        "bytes": 23269,
        "sha256": "8149cf0ccf6e6349de16a93dbc16f96b766b160fd71caaf53fc7418166b0e10d",
    },
    "prompt_repair_audit": {
        "path_relative_to_evidence_root": "cross-backend-prompt-repair-audit-attempt-001/prompt-repair-manifest.json",
        "bytes": 80928,
        "sha256": "bacafc35238f290254c735ce2cd277cbdea7216a4f33f54787756975a8d2cf17",
    },
}


class VerificationFailure(RuntimeError):
    """The checked-in report package is inconsistent or unbound."""


def require(value: bool, message: str) -> None:
    if not value:
        raise VerificationFailure(message)


def close(left: float, right: float) -> bool:
    return math.isclose(left, right, rel_tol=0.0, abs_tol=1e-9)


def percent(left: float, right: float) -> float:
    return (right / left - 1.0) * 100.0


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    require(isinstance(value, dict), f"{path.name} is not an object")
    return value


def validate_ref(value: Any, label: str) -> None:
    require(isinstance(value, dict), f"{label} is not an object")
    require(set(value) == {"bytes", "sha256"}, f"{label} reference schema")
    require(isinstance(value["bytes"], int) and value["bytes"] >= 0, f"{label} bytes")
    require(isinstance(value["sha256"], str) and SHA256_RE.fullmatch(value["sha256"]) is not None, f"{label} sha256")


def sonnet_uncached(cell: dict[str, Any], *, include_auxiliary: bool) -> int:
    models = cell["metrics"]["token_breakdown"]["per_model"]
    main = models["claude-sonnet-5"]
    total = main["inputTokens"] + main["cacheCreationInputTokens"]
    if include_auxiliary:
        auxiliary = models["claude-haiku-4-5-20251001"]
        total += auxiliary["inputTokens"] + auxiliary["cacheCreationInputTokens"]
    return total


def aggregate(rows: list[dict[str, Any]], condition: str) -> dict[str, Any]:
    cells = [row["conditions"][condition] for row in rows]
    by_type: Counter[str] = Counter()
    for cell in cells:
        by_type.update(cell["metrics"]["tool_calls"]["by_type"])
    return {
        "ready_n": sum(cell["status"] == "READY" for cell in cells),
        "evaluated_n": len(cells),
        "resolved_n": sum(cell["official"]["verdict"] == "RESOLVED" for cell in cells),
        "input_tokens": sum(cell["metrics"]["input_tokens"] for cell in cells),
        "output_tokens": sum(cell["metrics"]["output_tokens"] for cell in cells),
        "total_tokens": sum(cell["metrics"]["total_tokens"] for cell in cells),
        "elapsed_seconds": sum(cell["metrics"]["elapsed_seconds"] for cell in cells),
        "cost_usd": sum(cell["metrics"]["cost_usd"] for cell in cells),
        "tool_calls": {"total": sum(by_type.values()), "by_type": dict(by_type)},
    }


def metric_value(cell: dict[str, Any], key: str) -> float:
    if key == "tool_calls":
        return cell["metrics"]["tool_calls"]["total"]
    return cell["metrics"][key]


def verify_effects(data: dict[str, Any], rows: list[dict[str, Any]], observed: dict[str, Any]) -> None:
    for backend in ("sonnet", "luna"):
        a_cells = [row["conditions"][f"A_{backend}"] for row in rows]
        t_cells = [row["conditions"][f"T_{backend}"] for row in rows]
        effects = data["within_model_treatment_effects"][backend]
        require(effects["backend"] == backend, f"{backend} effect backend")
        efficacy = effects["efficacy"]
        a_resolved = [cell["official"]["verdict"] == "RESOLVED" for cell in a_cells]
        t_resolved = [cell["official"]["verdict"] == "RESOLVED" for cell in t_cells]
        expected_efficacy = {
            "A_only_resolved_n": sum(a and not t for a, t in zip(a_resolved, t_resolved)),
            "A_resolution_rate": sum(a_resolved) / 20,
            "A_resolved_n": sum(a_resolved),
            "T_only_resolved_n": sum(t and not a for a, t in zip(a_resolved, t_resolved)),
            "T_resolution_rate": sum(t_resolved) / 20,
            "T_resolved_n": sum(t_resolved),
            "evaluated_pairs_n": 20,
            "resolution_rate_change_percentage_points": (sum(t_resolved) - sum(a_resolved)) * 5.0,
            "same_verdict_n": sum(a == t for a, t in zip(a_resolved, t_resolved)),
        }
        require(efficacy == expected_efficacy, f"{backend} efficacy effect")
        for key in ("input_tokens", "output_tokens", "total_tokens", "elapsed_seconds", "cost_usd", "tool_calls"):
            a_values = [metric_value(cell, key) for cell in a_cells]
            t_values = [metric_value(cell, key) for cell in t_cells]
            changes = [percent(a, t) for a, t in zip(a_values, t_values)]
            expected = effects["efficiency"][key]
            require(close(expected["A_total"], sum(a_values)), f"{backend} {key} A total")
            require(close(expected["T_total"], sum(t_values)), f"{backend} {key} T total")
            require(close(expected["aggregate_change_percent"], percent(sum(a_values), sum(t_values))), f"{backend} {key} aggregate effect")
            require(close(expected["median_paired_change_percent"], median(changes)), f"{backend} {key} median effect")
            require(expected["T_lower_n"] == sum(t < a for a, t in zip(a_values, t_values)), f"{backend} {key} lower count")
            require(expected["T_equal_n"] == sum(t == a for a, t in zip(a_values, t_values)), f"{backend} {key} equal count")
            require(expected["pairs_n"] == 20, f"{backend} {key} pair count")
        by_type: dict[str, dict[str, int]] = {}
        all_types = set(observed[f"A_{backend}"]["tool_calls"]["by_type"]) | set(observed[f"T_{backend}"]["tool_calls"]["by_type"])
        for tool_type in sorted(all_types):
            a_count = observed[f"A_{backend}"]["tool_calls"]["by_type"].get(tool_type, 0)
            t_count = observed[f"T_{backend}"]["tool_calls"]["by_type"].get(tool_type, 0)
            by_type[tool_type] = {"A": a_count, "T": t_count, "change": t_count - a_count}
        require(effects["tool_calls_by_type"] == by_type, f"{backend} tool-type effects")


def report_cell(cell: dict[str, Any]) -> str:
    metrics = cell["metrics"]
    success = "yes" if cell["official"]["verdict"] == "RESOLVED" else "no"
    return f"success {success} · {metrics['elapsed_seconds']:.1f}s · in {metrics['input_tokens']:,} · out {metrics['output_tokens']:,} · ${metrics['cost_usd']:.6f}"


def report_tools(cell: dict[str, Any]) -> str:
    tools = cell["metrics"]["tool_calls"]
    parts = ", ".join(f"{name}={count}" for name, count in sorted(tools["by_type"].items()))
    return f"{tools['total']} ({parts})"


def verify_report(report: str, data: dict[str, Any], rows: list[dict[str, Any]], observed: dict[str, Any]) -> None:
    require("## Registered rule and issue #817 disposition" in report, "registered decision missing")
    require("does **not**\nadvance #817" in report, "#817 disposition drift")
    for backend, label in (("sonnet", "Sonnet"), ("luna", "Luna")):
        a = observed[f"A_{backend}"]
        t = observed[f"T_{backend}"]
        changes = {
            "tokens": percent(a["total_tokens"], t["total_tokens"]),
            "time": percent(a["elapsed_seconds"], t["elapsed_seconds"]),
            "cost": percent(a["cost_usd"], t["cost_usd"]),
            "tools": percent(a["tool_calls"]["total"], t["tool_calls"]["total"]),
        }
        rendered = {name: f"{value:+.1f}%".replace("-", "−") for name, value in changes.items()}
        for name in (("time", "tools") if backend == "sonnet" else ("cost", "tools")):
            rendered[name] = f"**{rendered[name]}**"
        line = f"| {label} | {a['resolved_n']}/20 | **{t['resolved_n']}/20** | {rendered['tokens']} | {rendered['time']} | {rendered['cost']} | {rendered['tools']} |"
        require(line in report, f"{backend} executive result drift")
    for row in rows:
        cells = row["conditions"]
        matrix = f"| {row['rank']} | `{row['instance_id']}` | " + " | ".join(report_cell(cells[name]) for name in CONDITIONS) + " |"
        tools = f"| {row['rank']} | " + " | ".join(report_tools(cells[name]) for name in CONDITIONS) + " |"
        require(matrix in report, f"matrix row {row['rank']} drift")
        require(tools in report, f"tool row {row['rank']} drift")
    for backend, label in (("sonnet", "Sonnet"), ("luna", "Luna")):
        a = observed[f"A_{backend}"]
        t = observed[f"T_{backend}"]
        require(f"- {label}: 20 evaluated pairs; A {a['resolved_n']} resolved vs T {t['resolved_n']} resolved (+5.0 percentage points); T-only wins 2, A-only wins 1, same verdict 17." in report, f"{backend} efficacy prose drift")
        effects = data["within_model_treatment_effects"][backend]
        for key, metric_label in (("input_tokens", "Input tokens"), ("output_tokens", "Output tokens"), ("total_tokens", "Total tokens"), ("elapsed_seconds", "Elapsed seconds"), ("cost_usd", "Cost (USD)"), ("tool_calls", "Tool calls")):
            effect = effects["efficiency"][key]
            line = f"| {label} | {metric_label} | {effect['A_total']:.6g} | {effect['T_total']:.6g} | {effect['aggregate_change_percent']:+.1f}% | {effect['median_paired_change_percent']:+.1f}% | {effect['T_lower_n']}/20 |"
            require(line in report, f"{backend} {key} effect table drift")
        tool_parts = []
        for tool_type, counts in effects["tool_calls_by_type"].items():
            tool_parts.append(f"{tool_type} A={counts['A']}, T={counts['T']} ({counts['change']:+d})")
        require(f"- {label}: " + ", ".join(tool_parts) + "." in report, f"{backend} aggregate tool mix drift")

    luna_uncached = {arm: sum(row["conditions"][f"{arm}_luna"]["metrics"]["token_breakdown"]["uncached_input_tokens"] for row in rows) for arm in ("A", "T")}
    sonnet_main = {arm: sum(sonnet_uncached(row["conditions"][f"{arm}_sonnet"], include_auxiliary=False) for row in rows) for arm in ("A", "T")}
    sonnet_all = {arm: sum(sonnet_uncached(row["conditions"][f"{arm}_sonnet"], include_auxiliary=True) for row in rows) for arm in ("A", "T")}
    luna_reasoning = {arm: sum(row["conditions"][f"{arm}_luna"]["metrics"]["token_breakdown"]["reasoning_output_tokens_subset"] for row in rows) for arm in ("A", "T")}

    def change_text(a: int, t: int, *, bold: bool) -> str:
        change = percent(a, t)
        precision = 2 if abs(change) < 1 else 1
        rendered = f"{t - a:+,} ({change:+.{precision}f}%)".replace("-", "−")
        return f"**{rendered}**" if bold else rendered

    decomposition = (
        f"| Luna uncached input | {luna_uncached['A']:,} | {luna_uncached['T']:,} | {change_text(luna_uncached['A'], luna_uncached['T'], bold=True)} |",
        f"| Sonnet main-model uncached input | {sonnet_main['A']:,} | {sonnet_main['T']:,} | {change_text(sonnet_main['A'], sonnet_main['T'], bold=True)} |",
        f"| Sonnet inclusive uncached input | {sonnet_all['A']:,} | {sonnet_all['T']:,} | {change_text(sonnet_all['A'], sonnet_all['T'], bold=True)} |",
        f"| Sonnet auxiliary Haiku portion | {sonnet_all['A'] - sonnet_main['A']:,} | {sonnet_all['T'] - sonnet_main['T']:,} | {change_text(sonnet_all['A'] - sonnet_main['A'], sonnet_all['T'] - sonnet_main['T'], bold=True)} |",
        f"| Sonnet total output | {observed['A_sonnet']['output_tokens']:,} | {observed['T_sonnet']['output_tokens']:,} | {change_text(observed['A_sonnet']['output_tokens'], observed['T_sonnet']['output_tokens'], bold=True)} |",
        f"| Luna total output | {observed['A_luna']['output_tokens']:,} | {observed['T_luna']['output_tokens']:,} | {change_text(observed['A_luna']['output_tokens'], observed['T_luna']['output_tokens'], bold=False)} |",
        f"| Luna reasoning subset | {luna_reasoning['A']:,} | {luna_reasoning['T']:,} | {change_text(luna_reasoning['A'], luna_reasoning['T'], bold=False)} |",
        f"| Luna non-reasoning output | {observed['A_luna']['output_tokens'] - luna_reasoning['A']:,} | {observed['T_luna']['output_tokens'] - luna_reasoning['T']:,} | {change_text(observed['A_luna']['output_tokens'] - luna_reasoning['A'], observed['T_luna']['output_tokens'] - luna_reasoning['T'], bold=True)} |",
    )
    for line in decomposition:
        require(line in report, "input/output decomposition drift")
    normalized_lines = []
    for key, label in (("total_tokens", "Tokens"), ("elapsed_seconds", "Wall time"), ("tool_calls", "Tool calls"), ("cost_usd", "Cost")):
        values = []
        for backend in ("sonnet", "luna"):
            a = observed[f"A_{backend}"]
            t = observed[f"T_{backend}"]
            a_value = a["tool_calls"]["total"] if key == "tool_calls" else a[key]
            t_value = t["tool_calls"]["total"] if key == "tool_calls" else t[key]
            change = percent(a_value / a["resolved_n"], t_value / t["resolved_n"])
            rendered = f"{change:+.1f}%".replace("-", "−")
            if change < 0:
                rendered = f"**{rendered}**"
            values.append(rendered)
        normalized_lines.append(f"| {label} | {values[0]} | {values[1]} |")
    for line in normalized_lines:
        require(line in report, "outcome-normalized table drift")


def verify_external(manifest: dict[str, Any], evidence_root: Path | None) -> int:
    if evidence_root is None:
        configured = os.environ.get(manifest["evidence_root_environment_variable"])
        evidence_root = Path(configured) if configured else None
    if evidence_root is None:
        return 0
    root = evidence_root.resolve(strict=True)
    verified = 0
    for name, artifact in manifest["external_artifacts"].items():
        candidate = (root / artifact["path_relative_to_evidence_root"]).resolve(strict=True)
        try:
            candidate.relative_to(root)
        except ValueError as exc:
            raise VerificationFailure(f"{name} escapes evidence root") from exc
        require(candidate.stat().st_size == artifact["bytes"], f"{name} external byte count")
        require(digest(candidate) == artifact["sha256"], f"{name} external digest")
        verified += 1
    validation = load(root / manifest["external_artifacts"]["canonical_validation"]["path_relative_to_evidence_root"])
    require(validation["status"] == "PASS" and validation["assertions"] == 2333 and validation["failures"] == [], "external validation receipt")
    return verified


def verify(
    *,
    results_path: Path = RESULTS,
    report_path: Path = REPORT,
    manifest_path: Path = MANIFEST,
    evidence_root: Path | None = None,
) -> dict[str, Any]:
    require(digest(results_path) == EXPECTED_RESULTS_SHA, "results ledger digest drift")
    require(digest(report_path) == EXPECTED_REPORT_SHA, "report digest drift")
    require(digest(manifest_path) == EXPECTED_MANIFEST_SHA, "evidence manifest digest drift")
    require(digest(report_path.parent / "METHOD.md") == EXPECTED_METHOD_SHA, "method digest drift")
    require(digest(report_path.parent / "README.md") == EXPECTED_README_SHA, "README digest drift")
    require(digest(REGISTRATION) == EXPECTED_REGISTRATION_SHA, "registration digest drift")
    require(digest(SELECTION) == EXPECTED_SELECTION_SHA, "selection digest drift")
    data = load(results_path)
    manifest = load(manifest_path)
    registration = load(REGISTRATION)
    selection = load(SELECTION)

    package_paths = (results_path, report_path, manifest_path, report_path.parent / "METHOD.md", report_path.parent / "README.md")
    leak_patterns = (re.compile(r"/(?:Users|home|private|tmp|var|opt|root)/"), re.compile(r"file://"), re.compile(r"(?:^|[\s\"'(])[A-Za-z]:[\\/]", re.MULTILINE), re.compile(r"sk-[A-Za-z0-9_-]{16,}"), re.compile(r"BEGIN [A-Z ]*PRIVATE KEY"))
    for path in package_paths:
        text = path.read_text()
        require(not any(pattern.search(text) for pattern in leak_patterns), f"{path.name} leaks a host path or credential")

    require(data["schema_version"] == "issue836-canonical-review-results-v1", "results schema drift")
    require(data["status"] == "COMPLETE", "results are not complete")
    require(data["source_validation"] == {"status": "PASS", "assertions": 2333, "sha256": EXPECTED_EXTERNAL_ARTIFACTS["canonical_validation"]["sha256"]}, "source validation drift")
    require(manifest["schema_version"] == "issue836-canonical-evidence-manifest-v1", "manifest schema drift")
    require(manifest["external_artifacts"] == EXPECTED_EXTERNAL_ARTIFACTS, "external artifact manifest drift")
    require(manifest["model_calls_to_build_review_package"] == 0, "review package used a model")
    require(data["source_report"]["sha256"] == EXPECTED_EXTERNAL_ARTIFACTS["canonical_report_json"]["sha256"], "source report hash mismatch")

    require(registration["issue"] == 836 and registration["schema_version"] == "issue836-treatment-registration-v4", "registration identity")
    require(registration["episode_design"]["case_count"] == 20 and registration["episode_design"]["episode_count"] == 40, "registered counts")
    require(registration["model_runtime"]["budget_usd"] * registration["episode_design"]["episode_count"] == 240.0, "registered $240 authorization")
    require(registration["selector"]["counterbalance"] == {"a_first_case_count": 10, "even_rank_arm_order": ["T", "A"], "odd_rank_arm_order": ["A", "T"], "t_first_case_count": 10}, "registered parity")
    require(registration["selection_rule"]["precedence"][4] == "select_more_resolutions", "registered decision precedence")
    selection_cases = selection["cases"]
    require(selection["authoritative"] is True and len(selection_cases) == 20, "authoritative selection")

    rows = data["rows"]
    require(len(rows) == 20 and [row["rank"] for row in rows] == list(range(1, 21)), "rank drift")
    require(len({row["instance_id"] for row in rows}) == 20, "case IDs repeat")
    cells = 0
    for row, selected in zip(rows, selection_cases):
        rank = row["rank"]
        require(row["instance_id"] == selected["instance_id"] and rank == selected["rank"], f"rank {rank} selection identity")
        require(row["registered_order"] == selected["arm_order"], f"rank {rank} arm order")
        require(row["registered_order"] == (["A", "T"] if rank % 2 else ["T", "A"]), f"rank {rank} parity")
        require(set(row["conditions"]) == set(CONDITIONS), f"rank {rank} conditions")
        require(row["cross_backend_byte_parity"] == {"A": {"base_equal": True, "directive_equal": True, "prompt_equal": True}, "T": {"base_equal": True, "directive_equal": True, "prompt_equal": True}}, f"rank {rank} parity audit")
        contract = row["canonical_contract"]
        for arm in ("A", "T"):
            validate_ref(contract[arm]["user_prompt"], f"rank {rank} {arm} contract prompt")
            validate_ref(contract[arm]["base_system"], f"rank {rank} {arm} contract base")
            require(contract[arm]["base_system"] == {"bytes": 166, "sha256": BASE_SHA}, f"rank {rank} {arm} base contract")
            if arm == "A":
                require(contract[arm]["appended_system"] is None, f"rank {rank} A directive contract")
            else:
                require(contract[arm]["appended_system"] == {"bytes": 189, "sha256": DIRECTIVE_SHA}, f"rank {rank} T directive contract")
        for condition, cell in row["conditions"].items():
            cells += 1
            arm, backend = condition.split("_")
            expected_model = "claude-sonnet-5" if backend == "sonnet" else "gpt-5.6-luna"
            require(cell["condition"] == condition and cell["arm"] == arm and cell["backend"] == backend and cell["model"] == expected_model, f"rank {rank} {condition} identity")
            require(cell["base_commit"] == selected["base_commit"] and cell["base_tree"] == selected["base_tree"], f"rank {rank} {condition} checkout")
            require(GIT_SHA_RE.fullmatch(cell["base_commit"]) is not None and GIT_SHA_RE.fullmatch(cell["base_tree"]) is not None, f"rank {rank} {condition} git identity")
            require(cell["status"] == "READY", f"rank {rank} {condition} not ready")
            for name in ("official", "episode_receipt", "prompt", "base_instructions", "patch"):
                reference = cell[name]["source"] if name == "official" else cell[name]
                validate_ref(reference, f"rank {rank} {condition} {name}")
            require(cell["prompt"] == contract[arm]["user_prompt"] and cell["base_instructions"] == contract[arm]["base_system"], f"rank {rank} {condition} prompt contract")
            require(cell["directive"] == contract[arm]["appended_system"], f"rank {rank} {condition} directive contract")
            require(cell["official"]["verdict"] in {"RESOLVED", "UNRESOLVED"}, f"rank {rank} {condition} verdict")
            metrics = cell["metrics"]
            require(metrics["input_tokens"] >= 0 and metrics["output_tokens"] >= 0 and metrics["elapsed_seconds"] >= 0 and metrics["cost_usd"] >= 0, f"rank {rank} {condition} metrics")
            require(metrics["input_tokens"] + metrics["output_tokens"] == metrics["total_tokens"], f"rank {rank} {condition} token total")
            require(metrics["tool_calls"]["total"] == sum(metrics["tool_calls"]["by_type"].values()), f"rank {rank} {condition} tool total")
            require(cell["transcript_audit"]["status"] == "PASS" and metrics["tool_calls"] == cell["transcript_audit"]["tool_calls"], f"rank {rank} {condition} transcript recount")
            if condition == "T_luna":
                context = cell["treatment_context"]
                require(context is not None, f"rank {rank} missing treatment context")
                validate_ref(context["injection"], f"rank {rank} injection")
                validate_ref(context["inputs_manifest"], f"rank {rank} injection manifest")
                require(context["injection_bytes"] == context["injection"]["bytes"], f"rank {rank} injection bytes")
                require(0 < context["selected_graph_limit"] <= 20 and 0 < context["injection_bytes"] < 32768, f"rank {rank} bounded graph")
            else:
                require(cell["treatment_context"] is None, f"rank {rank} {condition} unexpected treatment provenance")
    require(cells == 80, "expected 80 cells")
    require(data["readiness"] == {"episode_status_counts": {"READY": 80}, "evaluated_episodes": 80, "missing_or_invalid": [], "pending_official_evaluation": [], "ready_episodes": 80, "total_episodes": 80}, "readiness summary drift")
    quality = data["transcript_quality_summary"]
    require(quality["audited_cells"] == 80 and quality["passing_cells"] == 80 and quality["pending_or_invalid_cells"] == 0, "transcript quality summary")

    observed = {condition: aggregate(rows, condition) for condition in CONDITIONS}
    for condition, summary in observed.items():
        expected = data["condition_summaries"][condition]
        for key in ("ready_n", "evaluated_n", "resolved_n", "input_tokens", "output_tokens", "total_tokens"):
            require(summary[key] == expected[key], f"{condition} {key} aggregate")
        for key in ("elapsed_seconds", "cost_usd"):
            require(close(summary[key], expected[key]), f"{condition} {key} aggregate")
        require(summary["tool_calls"] == expected["tool_calls"], f"{condition} tools")
    verify_effects(data, rows, observed)

    luna_uncached = {arm: sum(row["conditions"][f"{arm}_luna"]["metrics"]["token_breakdown"]["uncached_input_tokens"] for row in rows) for arm in ("A", "T")}
    sonnet_main = {arm: sum(sonnet_uncached(row["conditions"][f"{arm}_sonnet"], include_auxiliary=False) for row in rows) for arm in ("A", "T")}
    sonnet_all = {arm: sum(sonnet_uncached(row["conditions"][f"{arm}_sonnet"], include_auxiliary=True) for row in rows) for arm in ("A", "T")}
    luna_reasoning = {arm: sum(row["conditions"][f"{arm}_luna"]["metrics"]["token_breakdown"]["reasoning_output_tokens_subset"] for row in rows) for arm in ("A", "T")}
    require(luna_uncached == {"A": 1036130, "T": 1037119}, "Luna uncached")
    require(sonnet_main == {"A": 442821, "T": 557970}, "Sonnet main uncached")
    require(sonnet_all == {"A": 463028, "T": 727235}, "Sonnet inclusive uncached")
    require(luna_reasoning == {"A": 44568, "T": 53086}, "Luna reasoning")

    verify_report(report_path.read_text(), data, rows, observed)
    external_verified = verify_external(manifest, evidence_root)
    return {
        "status": "PASS",
        "rows": len(rows),
        "cells": cells,
        "source_assertions": data["source_validation"]["assertions"],
        "external_artifacts_verified": external_verified,
        "registered_mechanical_decision": "NOT_COMPUTED",
        "registered_gate_disposition": "NOT_FORMALLY_APPLICABLE_TO_REPAIRED_STUDY",
        "sonnet_total_token_change_percent": percent(observed["A_sonnet"]["total_tokens"], observed["T_sonnet"]["total_tokens"]),
        "luna_total_token_change_percent": percent(observed["A_luna"]["total_tokens"], observed["T_luna"]["total_tokens"]),
    }


def main() -> int:
    print(json.dumps(verify(), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
