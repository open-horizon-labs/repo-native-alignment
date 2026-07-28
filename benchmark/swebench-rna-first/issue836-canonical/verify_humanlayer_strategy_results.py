#!/usr/bin/env python3
"""Fail-closed verification for the 720-cell unified strategy package."""

from __future__ import annotations

import collections
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
from statistics import mean
from typing import Any, Iterable


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "humanlayer-strategy-results.json"
BASELINE_LEDGERS = (
    ROOT / "results.json",
    ROOT / "t2-results.json",
    ROOT / "weaker-model-results.json",
)
REPORT = ROOT / "REPORT.md"
METHOD = ROOT / "METHOD.md"
README = ROOT / "README.md"
MANIFEST = ROOT / "humanlayer-strategy-evidence-manifest.json"

# Replaced only after the complete package is generated and reviewed.
EXPECTED_RESULTS_SHA = "b3c617ea3d23e0865adad6c78b28fbfe8e4bd5af97c96d69fabe6d0ad13685a5"
EXPECTED_BASELINE_SHAS = (
    "20ad9fcff75b91c5e86147de3cd2fbb63d582aec1950e3cbd0cca0e35d8a8a17",
    "26e9ad318e2d3a03f355499326dd644968bbd3770807d69d272c617ca7e62daf",
    "c09fdba9e3eaf058e5c7137a08376bcdf2cce35c9c92d9ef3eceaebb92e2b3a5",
)
EXPECTED_REPORT_SHA = "d6e4eda65545f32c6d4ae2d2795188feb64bbd8c7ea750ce288739c50bf9e4db"
EXPECTED_METHOD_SHA = "ec718074c4fa0205df04dc66e2a66f0faa54254115970d2d5a8b694285c7ff99"
EXPECTED_README_SHA = "30ec278446fb452581a351376443e80a3f66a3cc024cb16a10ca2e1e6059e765"
EXPECTED_MANIFEST_SHA = "0c9636d997e434ce02022a49aa26627600a62048b0a64d17392d9b27d1386550"

MODELS = ("sonnet", "luna", "haiku", "spark")
CONTEXTS = ("A", "T", "T2")
STRATEGIES = ("AS", "PF")
ALL_STRATEGIES = ("base", *STRATEGIES)
MODEL_NAMES = {
    "sonnet": "claude-sonnet-5",
    "luna": "gpt-5.6-luna",
    "haiku": "claude-haiku-4-5-20251001",
    "spark": "gpt-5.3-codex-spark",
}
PROVIDER_SURFACES = {
    "sonnet": "logged-in Claude CLI",
    "haiku": "logged-in Claude CLI",
    "luna": "Codex App Server",
    "spark": "Codex App Server",
}
LUNA_RATES = {
    "uncached_input_per_mtok": 1.0,
    "cached_input_per_mtok": 0.1,
    "cache_write_per_mtok": 1.25,
    "output_per_mtok": 6.0,
    "web_search_per_call": 0.01,
}
EXPECTED_UPSTREAM = {
    "anti_slop_template": {
        "bytes": 1413,
        "sha256": "e334962c38a1ca83f9de87b2120821b07aabd7159f665dfde843f04fe5ed74a5",
    },
    "humanlayer_commit": "a2da7968c7d5cbc8a58e9c559f4d9eea6d460d6c",
    "plan_first_template": {
        "bytes": 1233,
        "sha256": "0fcf89c6f841d9d2d02b3ec50b61ebcf4c4d7fcab812ec4e5d5e66efbec2cf8b",
    },
    "slop_code_bench_commit": "13de1a7a6b8b3dc5cc532a0c322a0997afa5bec7",
}
EXPECTED_PROMPT_PORT = {
    "spec": {
        "bytes": 2814,
        "sha256": "6fb8ab25479a4025751dd039ee242bbb984238050ff78f291fd9598618b4fb82",
    },
    "strategy_instructions": {
        "AS": {
            "bytes": 808,
            "sha256": "8d47fd170081caee967ae54bb0921f520d1b466c37959e9cfd59e2745c5964aa",
        },
        "PF": {
            "bytes": 727,
            "sha256": "3d93456b81f521c34d97312e9745adfecce74333668c42aa2d90d31b4b21e430",
        },
    },
}
EXPECTED_DEVELOPER_INSTRUCTIONS = {
    ("A", "AS"): EXPECTED_PROMPT_PORT["strategy_instructions"]["AS"],
    ("A", "PF"): EXPECTED_PROMPT_PORT["strategy_instructions"]["PF"],
    ("T", "AS"): {
        "bytes": 999,
        "sha256": "075b02b7d2abefb3a894dd5c8deb838d5faea55b0d10e535623e9fb0cf924706",
    },
    ("T2", "AS"): {
        "bytes": 999,
        "sha256": "075b02b7d2abefb3a894dd5c8deb838d5faea55b0d10e535623e9fb0cf924706",
    },
    ("T", "PF"): {
        "bytes": 918,
        "sha256": "ff6dcada51bec5e9228bfb693d5d9e20565fa35fea44c95d6feb9195114ef62a",
    },
    ("T2", "PF"): {
        "bytes": 918,
        "sha256": "ff6dcada51bec5e9228bfb693d5d9e20565fa35fea44c95d6feb9195114ef62a",
    },
}


class VerificationFailure(RuntimeError):
    pass


assertions = 0


def require(value: bool, message: str) -> None:
    global assertions
    assertions += 1
    if not value:
        raise VerificationFailure(message)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"{path.name}: root is not an object")
    return value


def validate_ref(value: Any, label: str) -> None:
    require(isinstance(value, dict), f"{label}: reference is not an object")
    require(set(value) == {"bytes", "sha256"}, f"{label}: reference schema")
    require(isinstance(value["bytes"], int) and value["bytes"] >= 0, f"{label}: bytes")
    require(
        isinstance(value["sha256"], str)
        and len(value["sha256"]) == 64
        and all(character in "0123456789abcdef" for character in value["sha256"]),
        f"{label}: SHA-256",
    )


def require_path_free(value: Any, label: str = "results") -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            require_path_free(item, f"{label}.{key}")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            require_path_free(item, f"{label}[{index}]")
    elif isinstance(value, str):
        require(not PurePosixPath(value).is_absolute(), f"{label}: absolute path leaked")


def close(left: float, right: float) -> bool:
    return math.isclose(left, right, rel_tol=0.0, abs_tol=1e-9)


def require_equivalent(actual: Any, expected: Any, label: str) -> None:
    if isinstance(expected, float):
        require(isinstance(actual, (int, float)) and close(float(actual), expected), label)
    elif isinstance(expected, dict):
        require(isinstance(actual, dict) and actual.keys() == expected.keys(), f"{label}: keys")
        for key in expected:
            require_equivalent(actual[key], expected[key], f"{label}.{key}")
    elif isinstance(expected, list):
        require(isinstance(actual, list) and len(actual) == len(expected), f"{label}: list")
        for index, item in enumerate(expected):
            require_equivalent(actual[index], item, f"{label}[{index}]")
    else:
        require(actual == expected, label)


def percent(before: float, after: float) -> float | None:
    return None if before == 0 else (after - before) / before * 100.0


def exact_binomial_two_sided(wins: int, losses: int) -> float:
    n = wins + losses
    if n == 0:
        return 1.0
    tail = sum(math.comb(n, k) for k in range(min(wins, losses) + 1))
    return min(1.0, 2.0 * tail / (2**n))


def normalized_baseline_breakdown(cell: dict[str, Any]) -> dict[str, int]:
    metrics = cell["metrics"]
    source = metrics["token_breakdown"]
    if "uncached_input_tokens" in source:
        uncached = int(source["uncached_input_tokens"])
        cached = int(source["cached_input_tokens"])
        cache_write = int(source.get("cache_write_input_tokens", 0))
    elif "per_model" in source:
        models = source["per_model"].values()
        uncached = sum(int(model["inputTokens"]) for model in models)
        cached = sum(int(model["cacheReadInputTokens"]) for model in models)
        cache_write = sum(int(model["cacheCreationInputTokens"]) for model in models)
    else:
        uncached = int(source["ordinary_input_tokens"])
        cached = int(source["cache_read_input_tokens"])
        cache_write = int(source["cache_creation_input_tokens"])
    result = {
        "ordinary_input_tokens": uncached,
        "uncached_input_tokens": uncached + cache_write,
        "cache_read_input_tokens": cached,
        "cached_input_tokens": cached,
        "cache_write_input_tokens": cache_write,
        "input_tokens": int(metrics["input_tokens"]),
        "output_tokens": int(metrics["output_tokens"]),
        "total_tokens": int(metrics["total_tokens"]),
    }
    require(result["input_tokens"] == uncached + cached + cache_write, "baseline input accounting")
    require(result["total_tokens"] == result["input_tokens"] + result["output_tokens"], "baseline token accounting")
    return result


def validate_quality(value: dict[str, Any], label: str) -> None:
    validate_ref(value["episode_receipt"], f"{label}.episode_receipt")
    require(set(value["scopes"]) == {"all", "production", "test"}, f"{label}: scopes")
    for scope, item in value["scopes"].items():
        for state in ("base", "final"):
            metrics = item[state]
            if metrics is not None:
                require(metrics["ast_grep_rules_checked"] > 0, f"{label}.{scope}.{state}: ast-grep")
                require(metrics["graph_node_count"] >= 0, f"{label}.{scope}.{state}: graph nodes")
                require(metrics["graph_edge_count"] >= 0, f"{label}.{scope}.{state}: graph edges")
                require(not PurePosixPath(metrics["entry_path"]).is_absolute(), f"{label}.{scope}.{state}: entry path")
        if item["base"] is None or item["final"] is None:
            require(item["delta"] is None, f"{label}.{scope}: missing-side delta")
        else:
            expected = {
                name: item["final"][name] - item["base"][name]
                for name in (
                    "loc",
                    "function_count",
                    "verbosity",
                    "erosion",
                    "ast_grep_violations",
                    "clone_lines",
                )
            }
            require_equivalent(item["delta"], expected, f"{label}.{scope}.delta")


def quality_summary(cells: list[dict[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for scope in ("all", "production", "test"):
        result[scope] = {}
        for metric in ("verbosity", "erosion", "loc", "ast_grep_violations", "clone_lines"):
            values = [
                cell["quality"]["scopes"][scope]["delta"][metric]
                for cell in cells
                if cell["quality"]["scopes"][scope]["delta"] is not None
            ]
            result[scope][metric] = {
                "n": len(values),
                "mean_delta": mean(values) if values else None,
                "sum_delta": sum(values) if values else None,
            }
    return result


def sum_tools(cells: Iterable[dict[str, Any]]) -> dict[str, Any]:
    by_type: collections.Counter[str] = collections.Counter()
    for cell in cells:
        by_type.update(cell["metrics"]["tool_calls"]["by_type"])
    return {"total": sum(by_type.values()), "by_type": dict(sorted(by_type.items()))}


def summarize(cells: list[dict[str, Any]]) -> dict[str, Any]:
    costs = [cell["metrics"]["cost_usd"] for cell in cells]
    token_names = (
        "ordinary_input_tokens",
        "uncached_input_tokens",
        "cache_read_input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "input_tokens",
        "output_tokens",
        "total_tokens",
    )
    return {
        "cells": len(cells),
        "resolved": sum(cell["official"]["verdict"] == "RESOLVED" for cell in cells),
        "elapsed_seconds": sum(cell["metrics"]["elapsed_seconds"] for cell in cells),
        "cost_usd": sum(float(cost) for cost in costs) if all(cost is not None for cost in costs) else None,
        "tokens": {
            name: sum(cell["metrics"]["token_breakdown"][name] for cell in cells)
            for name in token_names
        },
        "tool_calls": sum_tools(cells),
        "quality": quality_summary(cells),
    }


def efficacy_effect(before: list[dict[str, Any]], after: list[dict[str, Any]]) -> dict[str, Any]:
    left = {cell["rank"]: cell for cell in before}
    right = {cell["rank"]: cell for cell in after}
    require(left.keys() == right.keys(), "paired efficacy ranks")
    wins = losses = ties = 0
    for rank in sorted(left):
        a = left[rank]["official"]["verdict"] == "RESOLVED"
        b = right[rank]["official"]["verdict"] == "RESOLVED"
        if b and not a:
            wins += 1
        elif a and not b:
            losses += 1
        else:
            ties += 1
    before_resolved = sum(cell["official"]["verdict"] == "RESOLVED" for cell in before)
    after_resolved = sum(cell["official"]["verdict"] == "RESOLVED" for cell in after)
    return {
        "before_resolved": before_resolved,
        "after_resolved": after_resolved,
        "delta_percentage_points": (after_resolved - before_resolved) / len(before) * 100.0,
        "wins": wins,
        "losses": losses,
        "ties": ties,
        "mcnemar_exact_two_sided_p": exact_binomial_two_sided(wins, losses),
    }


def efficiency_effect(before: list[dict[str, Any]], after: list[dict[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for name in ("elapsed_seconds", "input_tokens", "output_tokens", "total_tokens"):
        left = sum(cell["metrics"][name] for cell in before)
        right = sum(cell["metrics"][name] for cell in after)
        result[name] = {"before": left, "after": right, "percent": percent(left, right)}
    for name in (
        "ordinary_input_tokens",
        "uncached_input_tokens",
        "cache_read_input_tokens",
        "cache_write_input_tokens",
    ):
        left = sum(cell["metrics"]["token_breakdown"][name] for cell in before)
        right = sum(cell["metrics"]["token_breakdown"][name] for cell in after)
        result[name] = {"before": left, "after": right, "percent": percent(left, right)}
    left_mix = sum_tools(before)
    right_mix = sum_tools(after)
    result["tool_calls"] = {
        "before": left_mix["total"],
        "after": right_mix["total"],
        "percent": percent(left_mix["total"], right_mix["total"]),
        "before_by_type": left_mix["by_type"],
        "after_by_type": right_mix["by_type"],
    }
    costs = [cell["metrics"]["cost_usd"] for cell in before + after]
    if all(value is not None for value in costs):
        left_cost = sum(float(cell["metrics"]["cost_usd"]) for cell in before)
        right_cost = sum(float(cell["metrics"]["cost_usd"]) for cell in after)
        result["cost_usd"] = {
            "before": left_cost,
            "after": right_cost,
            "percent": percent(left_cost, right_cost),
        }
    else:
        result["cost_usd"] = {"status": "UNAVAILABLE"}
    return result


def quality_effect(before: list[dict[str, Any]], after: list[dict[str, Any]]) -> dict[str, Any]:
    left = {cell["rank"]: cell for cell in before}
    right = {cell["rank"]: cell for cell in after}
    result: dict[str, Any] = {}
    for scope in ("all", "production", "test"):
        result[scope] = {}
        for metric in ("verbosity", "erosion", "loc", "ast_grep_violations", "clone_lines"):
            pairs = []
            for rank in sorted(left.keys() & right.keys()):
                a = left[rank]["quality"]["scopes"][scope]["delta"]
                b = right[rank]["quality"]["scopes"][scope]["delta"]
                if a is not None and b is not None:
                    pairs.append((a[metric], b[metric]))
            result[scope][metric] = {
                "n": len(pairs),
                "before_mean_delta": mean(item[0] for item in pairs) if pairs else None,
                "after_mean_delta": mean(item[1] for item in pairs) if pairs else None,
                "paired_mean_difference": mean(item[1] - item[0] for item in pairs) if pairs else None,
            }
    return result


def comparison(before: list[dict[str, Any]], after: list[dict[str, Any]]) -> dict[str, Any]:
    require(len(before) == len(after) == 20, "comparison coverage")
    return {
        "efficacy": efficacy_effect(before, after),
        "efficiency": efficiency_effect(before, after),
        "quality": quality_effect(before, after),
    }


def luna_cost(cell: dict[str, Any]) -> float:
    tokens = cell["metrics"]["token_breakdown"]
    tools = cell["metrics"]["tool_calls"]["by_type"]
    return (
        tokens["ordinary_input_tokens"] * LUNA_RATES["uncached_input_per_mtok"]
        + tokens["cache_read_input_tokens"] * LUNA_RATES["cached_input_per_mtok"]
        + tokens["cache_write_input_tokens"] * LUNA_RATES["cache_write_per_mtok"]
        + tokens["output_tokens"] * LUNA_RATES["output_per_mtok"]
    ) / 1_000_000 + tools.get("webSearch", 0) * LUNA_RATES["web_search_per_call"]


def fmt_cost(value: float | None) -> str:
    return "n/a" if value is None else f"${value:.6f}"


def tool_mix(cell: dict[str, Any]) -> str:
    return ", ".join(
        f"{name}={count}"
        for name, count in cell["metrics"]["tool_calls"]["by_type"].items()
    ) or "none"


def compact_cell(cell: dict[str, Any]) -> dict[str, Any]:
    return {
        "rank": cell["rank"],
        "instance_id": cell["instance_id"],
        "key": cell["key"],
        "context": cell["context"],
        "strategy": cell["strategy"],
        "backend": cell["backend"],
        "model": cell["model"],
        "provider_surface": cell["provider_surface"],
        "status": cell["status"],
        "official": {"verdict": cell["official"]["verdict"]},
        "metrics": json.loads(json.dumps(cell["metrics"])),
        "quality": json.loads(json.dumps(cell["quality"])),
    }


def report_detail_line(cell: dict[str, Any]) -> str:
    tokens = cell["metrics"]["token_breakdown"]
    delta = cell["quality"]["scopes"]["production"]["delta"]
    verb = "n/a" if delta is None else f"{delta['verbosity']:+.4f}"
    erosion = "n/a" if delta is None else f"{delta['erosion']:+.4f}"
    return (
        f"| {cell['rank']} | {cell['instance_id']} | {cell['backend']} | {cell['context']} | {cell['strategy']} | "
        f"{'yes' if cell['official']['verdict'] == 'RESOLVED' else 'no'} | {cell['metrics']['elapsed_seconds']:.1f}s | "
        f"{tokens['ordinary_input_tokens']:,} | {tokens['uncached_input_tokens']:,} | "
        f"{tokens['cache_read_input_tokens']:,} | {tokens['cache_write_input_tokens']:,} | "
        f"{tokens['output_tokens']:,} | {fmt_cost(cell['metrics']['cost_usd'])} | "
        f"{tool_mix(cell)} | {verb} | {erosion} |"
    )


def ceiling_report_line(model: str, strategy: str, effects: dict[str, Any]) -> str:
    treatment = effects[model][strategy]
    t = treatment["T"]["efficacy"]
    t2 = treatment["T2"]["efficacy"]
    require(t["before_resolved"] == t2["before_resolved"], f"{model}/{strategy}: A ceiling mismatch")
    a_resolved = t["before_resolved"]
    return (
        f"| {model} | {strategy} | {a_resolved}/20 | {20 - a_resolved} | "
        f"{t['after_resolved']}/20 | {t['wins']}/{t['losses']} | "
        f"{t2['after_resolved']}/20 | {t2['wins']}/{t2['losses']} |"
    )


def main() -> int:
    data = load(RESULTS)
    ledgers = [load(path) for path in BASELINE_LEDGERS]
    manifest = load(MANIFEST)
    report = REPORT.read_text()

    require(digest(RESULTS) == EXPECTED_RESULTS_SHA, "strategy results digest drift")
    for path, expected in zip(BASELINE_LEDGERS, EXPECTED_BASELINE_SHAS):
        require(digest(path) == expected, f"{path.name}: frozen baseline digest drift")
    require(digest(REPORT) == EXPECTED_REPORT_SHA, "unified report digest drift")
    require(digest(METHOD) == EXPECTED_METHOD_SHA, "method digest drift")
    require(digest(README) == EXPECTED_README_SHA, "README digest drift")
    require(digest(MANIFEST) == EXPECTED_MANIFEST_SHA, "strategy manifest digest drift")
    require_path_free(data)
    require(data["schema_version"] == "issue836-humanlayer-strategy-results-v1", "schema")
    require(data["status"] == "COMPLETE", "status")
    require(
        data["cell_accounting"]
        == {"baseline": 240, "new": 480, "total": 720, "conditions": 36},
        "cell accounting",
    )
    require(data["pricing"]["luna"]["rates"] == LUNA_RATES, "Luna rates")
    require(data["upstream"] == EXPECTED_UPSTREAM, "pinned upstream sources")
    require(data["prompt_port"] == EXPECTED_PROMPT_PORT, "frozen prompt port")
    manifest_prompt_refs = {
        "spec": manifest["external_artifacts"]["prompt_port"],
        "AS": manifest["external_artifacts"]["anti_slop_strategy"],
        "PF": manifest["external_artifacts"]["plan_first_strategy"],
        "upstream_AS": manifest["external_artifacts"]["upstream_anti_slop"],
        "upstream_PF": manifest["external_artifacts"]["upstream_plan_first"],
    }
    expected_prompt_refs = {
        "spec": EXPECTED_PROMPT_PORT["spec"],
        "AS": EXPECTED_PROMPT_PORT["strategy_instructions"]["AS"],
        "PF": EXPECTED_PROMPT_PORT["strategy_instructions"]["PF"],
        "upstream_AS": EXPECTED_UPSTREAM["anti_slop_template"],
        "upstream_PF": EXPECTED_UPSTREAM["plan_first_template"],
    }
    for name, expected in expected_prompt_refs.items():
        actual = {
            field: manifest_prompt_refs[name][field]
            for field in ("bytes", "sha256")
        }
        require(actual == expected, f"manifest prompt source: {name}")

    baseline_quality = {
        (row["rank"], row["key"]): row for row in data["baseline_quality_rows"]
    }
    require(len(baseline_quality) == 240, "baseline quality coverage")
    baseline: dict[tuple[int, str], dict[str, Any]] = {}
    for ledger in ledgers:
        for row in ledger["rows"]:
            for key, source_cell in row["conditions"].items():
                slot = (row["rank"], key)
                require(slot not in baseline, f"duplicate baseline slot: {slot}")
                require(slot in baseline_quality, f"missing baseline quality: {slot}")
                cell = json.loads(json.dumps(source_cell))
                cell["rank"] = row["rank"]
                cell["instance_id"] = row["instance_id"]
                context, backend = key.split("_", 1)
                require(context in CONTEXTS and backend in MODELS, f"baseline key: {key}")
                cell["key"] = key
                cell["context"] = context
                cell["strategy"] = "base"
                cell["backend"] = backend
                cell["model"] = MODEL_NAMES[backend]
                cell["provider_surface"] = PROVIDER_SURFACES[backend]
                cell["metrics"]["token_breakdown"] = normalized_baseline_breakdown(cell)
                cell["quality"] = baseline_quality[slot]
                validate_quality(cell["quality"], f"baseline {slot}")
                baseline[slot] = cell
    require(len(baseline) == 240, "baseline cell coverage")

    rows = data["new_strategy_rows"]
    require(len(rows) == 20, "new row count")
    new_cells: list[dict[str, Any]] = []
    expected_keys = {
        f"{context}_{strategy}_{model}"
        for model in MODELS
        for context in CONTEXTS
        for strategy in STRATEGIES
    }
    for expected_rank, row in enumerate(rows, 1):
        require(row["rank"] == expected_rank, f"rank {expected_rank}: order")
        require(set(row["conditions"]) == expected_keys, f"rank {expected_rank}: conditions")
        for key, cell in row["conditions"].items():
            require(cell["rank"] == expected_rank and cell["key"] == key, f"rank {expected_rank} {key}: identity")
            require(cell["model"] == MODEL_NAMES[cell["backend"]], f"rank {expected_rank} {key}: model")
            require(key == f"{cell['context']}_{cell['strategy']}_{cell['backend']}", f"rank {expected_rank} {key}: key")
            require(cell["status"] == "READY", f"rank {expected_rank} {key}: ready")
            for name in ("prompt", "developer_instructions", "episode_receipt", "patch"):
                validate_ref(cell[name], f"rank {expected_rank} {key} {name}")
            baseline_key = f"{cell['context']}_{cell['backend']}"
            require(
                cell["prompt"] == baseline[(expected_rank, baseline_key)]["prompt"],
                f"rank {expected_rank} {key}: unchanged user prompt",
            )
            require(
                cell["developer_instructions"]
                == EXPECTED_DEVELOPER_INSTRUCTIONS[(cell["context"], cell["strategy"])],
                f"rank {expected_rank} {key}: instruction composition",
            )
            validate_ref(cell["official"]["evaluation_receipt"], f"rank {expected_rank} {key} evaluation")
            require(cell["official"]["verdict"] in {"RESOLVED", "UNRESOLVED"}, f"rank {expected_rank} {key}: verdict")
            metrics = cell["metrics"]
            tokens = metrics["token_breakdown"]
            require(metrics["input_tokens"] == tokens["input_tokens"], f"rank {expected_rank} {key}: input")
            require(metrics["output_tokens"] == tokens["output_tokens"], f"rank {expected_rank} {key}: output")
            require(metrics["total_tokens"] == metrics["input_tokens"] + metrics["output_tokens"], f"rank {expected_rank} {key}: total")
            require(
                tokens["input_tokens"]
                == tokens["ordinary_input_tokens"]
                + tokens["cache_read_input_tokens"]
                + tokens["cache_write_input_tokens"],
                f"rank {expected_rank} {key}: input decomposition",
            )
            require(
                tokens["uncached_input_tokens"]
                == tokens["ordinary_input_tokens"] + tokens["cache_write_input_tokens"],
                f"rank {expected_rank} {key}: uncached input",
            )
            require(
                tokens["cached_input_tokens"] == tokens["cache_read_input_tokens"],
                f"rank {expected_rank} {key}: cache-read alias",
            )
            require(metrics["tool_calls"]["total"] == sum(metrics["tool_calls"]["by_type"].values()), f"rank {expected_rank} {key}: tools")
            require(cell["transcript_audit"]["status"] == "PASS", f"rank {expected_rank} {key}: transcript")
            require(cell["transcript_audit"]["foreign_rank_references"] == [], f"rank {expected_rank} {key}: contamination")
            require(cell["transcript_audit"]["foreign_cell_input_references"] == [], f"rank {expected_rank} {key}: foreign-cell input")
            require(cell["transcript_audit"]["foreign_cell_output_references"] == [], f"rank {expected_rank} {key}: foreign-cell output")
            require(
                cell["transcript_audit"].get("own_harness_artifact_output_references", []) == [],
                f"rank {expected_rank} {key}: own-harness output",
            )
            require(cell["transcript_audit"]["tool_calls"] == metrics["tool_calls"], f"rank {expected_rank} {key}: tool recount")
            expected_exposure = 0 if cell["context"] == "A" else 1
            require(cell["transcript_audit"]["injected_rna_exposure_count"] == expected_exposure, f"rank {expected_rank} {key}: RNA exposure")
            if cell["backend"] == "luna":
                require(close(metrics["cost_usd"], luna_cost(cell)), f"rank {expected_rank} {key}: Luna cost")
            elif cell["backend"] == "spark":
                require(metrics["cost_usd"] is None, f"rank {expected_rank} {key}: Spark cost")
            else:
                require(metrics["cost_usd"] is not None and metrics["cost_usd"] >= 0, f"rank {expected_rank} {key}: provider cost")
            validate_quality(cell["quality"], f"rank {expected_rank} {key} quality")

            new_cells.append(cell)
    require(len(new_cells) == 480, "new cell coverage")

    all_cells: dict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for cell in baseline.values():
        all_cells[cell["condition"]].append(cell)
    for cell in new_cells:
        all_cells[cell["key"]].append(cell)
    require(len(all_cells) == 36 and all(len(cells) == 20 for cells in all_cells.values()), "condition coverage")

    expected_cells = {
        (cell["rank"], cell["key"]): cell
        for cells in all_cells.values()
        for cell in cells
    }
    require(len(expected_cells) == 720, "unified expected-cell coverage")
    unified_rows = data["rows"]
    require(len(unified_rows) == 20, "unified row count")
    unified_count = 0
    expected_all_keys = set(all_cells)
    for expected_rank, row in enumerate(unified_rows, 1):
        require(row["rank"] == expected_rank, f"unified rank {expected_rank}: order")
        require(set(row["conditions"]) == expected_all_keys, f"unified rank {expected_rank}: conditions")
        instance_ids = {
            expected_cells[(expected_rank, key)]["instance_id"] for key in expected_all_keys
        }
        require(instance_ids == {row["instance_id"]}, f"unified rank {expected_rank}: instance")
        for key, actual in row["conditions"].items():
            expected = compact_cell(expected_cells[(expected_rank, key)])
            require_equivalent(actual, expected, f"unified rank {expected_rank} {key}")
            tokens = actual["metrics"]["token_breakdown"]
            require(
                tokens["input_tokens"]
                == tokens["ordinary_input_tokens"]
                + tokens["cache_read_input_tokens"]
                + tokens["cache_write_input_tokens"],
                f"unified rank {expected_rank} {key}: input decomposition",
            )
            require(
                tokens["uncached_input_tokens"]
                == tokens["ordinary_input_tokens"] + tokens["cache_write_input_tokens"],
                f"unified rank {expected_rank} {key}: uncached input",
            )
            require(report_detail_line(actual) in report, f"unified rank {expected_rank} {key}: report detail")
            unified_count += 1
    require(unified_count == 720, "unified report detail coverage")

    summaries = {key: summarize(sorted(cells, key=lambda cell: cell["rank"])) for key, cells in sorted(all_cells.items())}
    require_equivalent(data["condition_summaries"], summaries, "condition summaries")

    strategy_effects: dict[str, Any] = {}
    for model in MODELS:
        strategy_effects[model] = {}
        for context in CONTEXTS:
            strategy_effects[model][context] = {}
            before = sorted(all_cells[f"{context}_{model}"], key=lambda cell: cell["rank"])
            for strategy in STRATEGIES:
                after = sorted(all_cells[f"{context}_{strategy}_{model}"], key=lambda cell: cell["rank"])
                strategy_effects[model][context][strategy] = comparison(before, after)
    require_equivalent(data["strategy_effects"], strategy_effects, "strategy effects")

    context_effects: dict[str, Any] = {}
    for model in MODELS:
        context_effects[model] = {}
        for strategy in ALL_STRATEGIES:
            context_effects[model][strategy] = {}
            before_key = f"A_{model}" if strategy == "base" else f"A_{strategy}_{model}"
            before = sorted(all_cells[before_key], key=lambda cell: cell["rank"])
            for context in ("T", "T2"):
                after_key = f"{context}_{model}" if strategy == "base" else f"{context}_{strategy}_{model}"
                after = sorted(all_cells[after_key], key=lambda cell: cell["rank"])
                context_effects[model][strategy][context] = comparison(before, after)
    require_equivalent(data["context_effects_by_strategy"], context_effects, "context effects")
    require(
        "### Ceiling sensitivity across stronger and weaker models" in report,
        "ceiling-sensitivity report section",
    )
    for model in MODELS:
        for strategy in ALL_STRATEGIES:
            require(
                ceiling_report_line(model, strategy, context_effects) in report,
                f"ceiling-sensitivity row: {model}/{strategy}",
            )

    require(report.startswith("# Canonical 20-case × 36-condition report\n"), "unified report title")
    require(
        "Status: **COMPLETE**. Canonical cells: **720/720**; officially evaluated: **720/720**."
        in report,
        "unified report status",
    )
    require("all 36 conditions" in report, "unified report condition prose")
    require("240/240" not in report and "all 12 conditions" not in report, "stale report accounting")
    require("HumanLayer/SlopCodeBench prompt-strategy factorial" in report, "unified report strategy section")
    require(
        "Scope boundary: HumanLayer's reported run used SlopCodeBench's `just-solve`"
        in report,
        "report transfer boundary",
    )
    require("#### Transfer-status audit" in METHOD.read_text(), "method transfer audit")
    require(
        "Adversarial model-review loop | Future treatment proposal"
        in METHOD.read_text(),
        "method future-treatment boundary",
    )
    require("Complete 36-condition summary" in report, "unified report condition summary")
    require("All 720 cells: per-case efficacy" in report, "unified report detail section")
    require(
        "480 deterministic pre-injected RNA exposures" in report
        and "Preconditioning was necessary" in report,
        "unified report RNA exposure disclosure",
    )
    require(manifest["canonical_new_cell_count"] == 480, "manifest canonical cells")
    require(manifest["unified_cell_count"] == 720, "manifest unified cells")
    require(manifest["stock_evaluator_model_calls"] == 0, "manifest evaluator calls")
    require(manifest["superseded_timeout_episode_count"] == 1, "manifest timeout superseded count")
    require(manifest["superseded_cross_cell_exposure_episode_count"] > 0, "manifest isolation superseded count")
    require(
        manifest["superseded_self_transcript_exposure_episode_count"] == 4,
        "manifest self-transcript superseded count",
    )
    require(
        manifest["superseded_harness_censored_episode_count"]
        == manifest["superseded_timeout_episode_count"]
        + manifest["superseded_cross_cell_exposure_episode_count"]
        + manifest["superseded_self_transcript_exposure_episode_count"],
        "manifest superseded accounting",
    )
    require(
        manifest["paid_provider_episode_count"]
        == 480 + manifest["superseded_harness_censored_episode_count"],
        "manifest paid episode accounting",
    )
    for value in data["source_evidence"].values():
        if isinstance(value, list):
            for item in value:
                validate_ref(item, "source evidence")
        else:
            validate_ref(value, "source evidence")

    evidence_root = os.environ.get("ISSUE836_EVIDENCE_ROOT")
    if evidence_root:
        root = Path(evidence_root)
        for name, artifact in manifest["external_artifacts"].items():
            path = root / artifact["path_relative_to_evidence_root"]
            require(path.is_file(), f"external {name}: absent")
            require(path.stat().st_size == artifact["bytes"], f"external {name}: bytes")
            require(digest(path) == artifact["sha256"], f"external {name}: SHA")

    print(
        json.dumps(
            {
                "status": "PASS",
                "assertions": assertions,
                "canonical_new_cells": 480,
                "unified_cells": 720,
                "external_evidence_verified": bool(evidence_root),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
