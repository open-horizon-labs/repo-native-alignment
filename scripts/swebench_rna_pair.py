#!/usr/bin/env python3
"""Run and aggregate an auditable paired baseline-vs-RNA SWE-bench pilot."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Mapping, Sequence


class PairError(RuntimeError):
    """A user-actionable paired-pilot failure."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def metric_value(ledger: Mapping[str, Any], field: str) -> Any:
    metric = ledger.get("executor_total", {}).get(field, {})
    return metric.get("value") if metric.get("status") == "reported" else None


def child_summary(run_dir: Path, exit_code: int) -> dict[str, Any]:
    manifest_path = run_dir / "manifest.json"
    if not manifest_path.exists():
        return {
            "status": "missing_manifest",
            "exit_code": exit_code,
            "bundle": str(run_dir),
        }
    manifest = read_json(manifest_path)
    ledger_path = run_dir / "stage-ledger.json"
    ledger = read_json(ledger_path) if ledger_path.exists() else {}
    outcome = manifest.get("evaluator", {}).get("outcome", {}).get("status")
    mcp = manifest.get("rna", {}).get("mcp_usage", {})
    prompt_path = run_dir / "task.md"
    fallback_path = run_dir / "fallback-events.jsonl"
    fallback_count = (
        len(fallback_path.read_text(encoding="utf-8").splitlines())
        if fallback_path.exists()
        else 0
    )
    return {
        "status": manifest.get("status", "unknown"),
        "exit_code": exit_code,
        "bundle": str(run_dir),
        "manifest": str(manifest_path),
        "prediction": str(run_dir / "prediction.jsonl"),
        "evaluation": str(run_dir / "evaluation"),
        "outcome": outcome or "not_evaluated",
        "resolved": outcome == "resolved",
        "task_prompt_sha256": (
            sha256_file(prompt_path) if prompt_path.exists() else None
        ),
        "time_to_first_edit_seconds": manifest.get("time_to_first_edit", {}).get(
            "seconds_to_first_edit"
        ),
        "input_tokens": metric_value(ledger, "input_tokens"),
        "cache_creation_input_tokens": metric_value(
            ledger, "cache_creation_input_tokens"
        ),
        "cache_read_input_tokens": metric_value(ledger, "cache_read_input_tokens"),
        "output_tokens": metric_value(ledger, "output_tokens"),
        "reasoning_tokens": metric_value(ledger, "reasoning_tokens"),
        "cost_usd": metric_value(ledger, "cost_usd"),
        "fallback_events": fallback_count,
        "mcp_orientation_calls": mcp.get("orientation_tool_calls"),
        "mcp_orientation_result_bytes": mcp.get(
            "orientation_delivered_tool_result_bytes"
        ),
        "real_mcp_use_before_edit": mcp.get("observed_real_mcp_use"),
    }


def validate_design(
    task_spec: Mapping[str, Any],
    executor_config: Mapping[str, Any],
    artifact: Mapping[str, Any],
) -> list[str]:
    instances = task_spec.get("instances")
    if not isinstance(instances, list) or not instances:
        raise PairError("task spec must contain a non-empty `instances` list")
    if len(instances) != len(set(instances)) or not all(
        isinstance(item, str) and item for item in instances
    ):
        raise PairError("task instances must be unique non-empty strings")
    if not isinstance(task_spec.get("dataset_revision"), str):
        raise PairError("task spec must pin `dataset_revision`")
    model = executor_config.get("model", {})
    if not isinstance(model, dict) or not model.get("immutable_id"):
        raise PairError("executor config must record model.immutable_id")
    required_controls = (
        "provider",
        "temperature",
        "budget_usd",
        "timeout_seconds",
        "executor_version",
    )
    missing = [key for key in required_controls if model.get(key) is None]
    if missing:
        raise PairError("executor model controls missing: " + ", ".join(missing))
    required_artifact = (
        "workflow_run_id",
        "commit_sha",
        "artifact_id",
        "artifact_name",
        "artifact_digest",
        "binary_sha256",
    )
    missing_artifact = [key for key in required_artifact if not artifact.get(key)]
    if missing_artifact:
        raise PairError(
            "RNA artifact metadata missing: " + ", ".join(missing_artifact)
        )
    return list(instances)


def build_child_command(
    *,
    child_script: Path,
    instance_id: str,
    arm: str,
    output_dir: Path,
    task_spec: Mapping[str, Any],
    executor_config_path: Path,
    executor_config: Mapping[str, Any],
    artifact: Mapping[str, Any],
    dry_run: bool,
    instance_json: Path | None,
    fixture_source: Path | None,
) -> list[str]:
    model = executor_config["model"]
    command = [
        sys.executable,
        str(child_script),
        instance_id,
        "--arm",
        arm,
        "--executor-config",
        str(executor_config_path),
        "--output-dir",
        str(output_dir),
        "--dataset-revision",
        str(task_spec["dataset_revision"]),
        "--rna-binary",
        str(artifact["binary_path"]),
        "--enrichment-condition",
        str(artifact.get("enrichment_condition", "call-references")),
        "--model-name",
        str(model["immutable_id"]),
        "--executor-timeout-seconds",
        str(model["timeout_seconds"]),
    ]
    if dry_run:
        command.append("--dry-run")
        if instance_json:
            command.extend(["--instance-json", str(instance_json)])
        if fixture_source:
            command.extend(["--fixture-source", str(fixture_source)])
    return command


def aggregate(
    *,
    output_dir: Path,
    task_spec: Mapping[str, Any],
    executor_config: Mapping[str, Any],
    artifact: Mapping[str, Any],
    commands: list[dict[str, Any]],
) -> dict[str, Any]:
    rows: list[dict[str, Any]] = []
    for instance_id in task_spec["instances"]:
        baseline_command = next(
            item
            for item in commands
            if item["instance_id"] == instance_id and item["arm"] == "baseline"
        )
        rna_command = next(
            item
            for item in commands
            if item["instance_id"] == instance_id and item["arm"] == "rna"
        )
        baseline = child_summary(
            Path(baseline_command["bundle"]), baseline_command["exit_code"]
        )
        rna = child_summary(Path(rna_command["bundle"]), rna_command["exit_code"])
        prompt_match = (
            baseline["task_prompt_sha256"] is not None
            and baseline["task_prompt_sha256"] == rna["task_prompt_sha256"]
        )
        rows.append(
            {
                "instance_id": instance_id,
                "baseline": baseline,
                "rna": rna,
                "parity": {
                    "task_prompt_sha256_match": prompt_match,
                    "model_executor_config_shared": True,
                    "only_intentional_difference": "RNA availability",
                },
                "paired_outcome": (
                    "both_resolved"
                    if baseline["resolved"] and rna["resolved"]
                    else "rna_only"
                    if rna["resolved"]
                    else "baseline_only"
                    if baseline["resolved"]
                    else "neither_resolved"
                ),
            }
        )
    arms: dict[str, Any] = {}
    for arm in ("baseline", "rna"):
        arm_rows = [row[arm] for row in rows]
        arms[arm] = {
            "tasks": len(arm_rows),
            "evaluated": sum(
                row["outcome"] in {"resolved", "unresolved"} for row in arm_rows
            ),
            "resolved": sum(row["resolved"] for row in arm_rows),
            "resolved_rate": (
                sum(row["resolved"] for row in arm_rows) / len(arm_rows)
                if arm_rows
                else None
            ),
            "cost_usd": (
                sum(row["cost_usd"] for row in arm_rows)
                if all(row["cost_usd"] is not None for row in arm_rows)
                else None
            ),
            "telemetry_rule": "null means unknown; no values are inferred",
        }
    return {
        "schema_version": 1,
        "label": "paired pilot; not a full-suite benchmark score",
        "generated_at": utc_now(),
        "dataset": {
            "name": task_spec.get(
                "dataset_name", "princeton-nlp/SWE-bench_Verified"
            ),
            "revision": task_spec["dataset_revision"],
        },
        "task_list": list(task_spec["instances"]),
        "selection": task_spec.get("selection"),
        "publication": task_spec.get("publication"),
        "known_harness_differences": task_spec.get("known_harness_differences"),
        "executor": executor_config,
        "rna_artifact": artifact,
        "arms": arms,
        "pairs": rows,
        "release_decision": "pending human interpretation",
    }


def arguments(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a frozen paired baseline-vs-RNA SWE-bench Verified pilot."
    )
    parser.add_argument("--task-spec", type=Path, required=True)
    parser.add_argument("--executor-config", type=Path, required=True)
    parser.add_argument("--rna-artifact", type=Path, required=True)
    parser.add_argument(
        "--rna-binary",
        type=Path,
        help="local path to the downloaded artifact binary; overrides metadata path",
    )
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--instance-json", type=Path)
    parser.add_argument("--fixture-source", type=Path)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = arguments(argv)
    output_dir = args.output_dir.resolve()
    if output_dir.exists() and any(output_dir.iterdir()):
        print(f"ERROR: output directory must be empty or absent: {output_dir}", file=sys.stderr)
        return 1
    output_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = output_dir / "paired-manifest.json"
    task_spec = read_json(args.task_spec.resolve())
    executor_config = read_json(args.executor_config.resolve())
    artifact = read_json(args.rna_artifact.resolve())
    started = time.monotonic()
    manifest: dict[str, Any] = {
        "schema_version": 1,
        "status": "initializing",
        "started_at": utc_now(),
        "inputs": {
            "task_spec": str(args.task_spec.resolve()),
            "task_spec_sha256": sha256_file(args.task_spec.resolve()),
            "executor_config": str(args.executor_config.resolve()),
            "executor_config_sha256": sha256_file(args.executor_config.resolve()),
            "rna_artifact": str(args.rna_artifact.resolve()),
            "rna_artifact_sha256": sha256_file(args.rna_artifact.resolve()),
        },
        "commands": [],
    }
    write_json(manifest_path, manifest)
    try:
        instances = validate_design(task_spec, executor_config, artifact)
        configured_binary = args.rna_binary or Path(str(artifact.get("binary_path", "")))
        binary_path = configured_binary.resolve()
        if not args.dry_run:
            if not binary_path.is_file():
                raise PairError(f"RNA artifact binary not found: {binary_path}")
            actual = sha256_file(binary_path)
            if actual != artifact["binary_sha256"]:
                raise PairError(
                    f"RNA artifact binary digest mismatch: expected "
                    f"{artifact['binary_sha256']}, got {actual}"
                )
        child_script = Path(__file__).with_name("swebench_rna_one.py").resolve()
        for instance_id in instances:
            for arm in ("baseline", "rna"):
                bundle = output_dir / "runs" / instance_id / arm
                command = build_child_command(
                    child_script=child_script,
                    instance_id=instance_id,
                    arm=arm,
                    output_dir=bundle,
                    task_spec=task_spec,
                    executor_config_path=args.executor_config.resolve(),
                    executor_config=executor_config,
                    artifact={**artifact, "binary_path": str(binary_path)},
                    dry_run=args.dry_run,
                    instance_json=args.instance_json,
                    fixture_source=args.fixture_source,
                )
                record = {
                    "instance_id": instance_id,
                    "arm": arm,
                    "bundle": str(bundle),
                    "argv": command,
                    "started_at": utc_now(),
                }
                completed = subprocess.run(command, check=False)
                record["finished_at"] = utc_now()
                record["exit_code"] = completed.returncode
                manifest["commands"].append(record)
                write_json(manifest_path, manifest)
        report = aggregate(
            output_dir=output_dir,
            task_spec=task_spec,
            executor_config=executor_config,
            artifact={**artifact, "binary_path": str(binary_path)},
            commands=manifest["commands"],
        )
        write_json(output_dir / "paired-report.json", report)
        manifest["status"] = (
            "dry_run_complete"
            if args.dry_run
            else "complete"
            if all(item["exit_code"] == 0 for item in manifest["commands"])
            else "partial"
        )
        manifest["report"] = str(output_dir / "paired-report.json")
        manifest["finished_at"] = utc_now()
        manifest["wall_clock_seconds"] = round(time.monotonic() - started, 3)
        write_json(manifest_path, manifest)
        print(f"Paired bundle: {output_dir}")
        return 0 if all(item["exit_code"] == 0 for item in manifest["commands"]) else 1
    except Exception as error:
        manifest["status"] = "failed"
        manifest["error"] = f"{type(error).__name__}: {error}"
        manifest["finished_at"] = utc_now()
        manifest["wall_clock_seconds"] = round(time.monotonic() - started, 3)
        write_json(manifest_path, manifest)
        print(f"ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
