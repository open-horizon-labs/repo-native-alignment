#!/usr/bin/env python3
"""Run one SWE-bench Verified instance through RNA and an external executor."""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import datetime as dt
import hashlib
import importlib.util
import importlib.metadata
import json
import os
import re
import signal
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


DATASET_NAME = "princeton-nlp/SWE-bench_Verified"
DATASET_SPLIT = "test"
STAGE_NAMES = (
    "frontier_before_first_edit",
    "rna_tool_results_orientation_and_planning",
    "first_edit_through_handoff",
    "inherited_or_replayed_context_at_handoff",
    "executor_after_handoff",
    "verification_and_debugging_after_patch",
)
TOKEN_FIELDS = (
    "input_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cost_usd",
)
BUSINESS_CONTEXT_MODE = "disabled"


class HarnessError(RuntimeError):
    """A user-actionable harness failure."""


@dataclasses.dataclass
class CommandResult:
    command: list[str]
    exit_code: int
    started_at: str
    finished_at: str
    duration_seconds: float


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def write_jsonl(path: Path, rows: Iterable[Mapping[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, sort_keys=True) + "\n")


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def command_version(command: str) -> str:
    try:
        completed = subprocess.run(
            [command, "--version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return "unknown"
    output = (completed.stdout or completed.stderr).strip()
    return output.splitlines()[0] if output else "unknown"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def rna_command(rna_binary: Path, *args: str) -> list[str]:
    """Build every benchmark RNA command with isolation made explicit."""
    command = [
        str(rna_binary),
        "--business-context",
        BUSINESS_CONTEXT_MODE,
        *args,
    ]
    if args and args[0] == "scan":
        command.append("--timings")
    return command


def run_logged(
    command: Sequence[str],
    *,
    cwd: Path,
    stdout_path: Path,
    stderr_path: Path,
    env: Mapping[str, str] | None = None,
    timeout: float | None = None,
) -> CommandResult:
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    started_wall = utc_now()
    started = time.monotonic()
    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        try:
            completed = subprocess.run(
                list(command),
                cwd=cwd,
                env=dict(env) if env is not None else None,
                stdout=stdout,
                stderr=stderr,
                timeout=timeout,
                check=False,
            )
            exit_code = completed.returncode
        except subprocess.TimeoutExpired:
            exit_code = 124
            stderr.write(f"\nHarness timeout after {timeout} seconds\n".encode())
    return CommandResult(
        command=list(command),
        exit_code=exit_code,
        started_at=started_wall,
        finished_at=utc_now(),
        duration_seconds=round(time.monotonic() - started, 3),
    )


def git_output(cwd: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise HarnessError(
            f"git {' '.join(args)} failed in {cwd}: {completed.stderr.strip()}"
        )
    return completed.stdout.strip()


def safe_extract(archive: tarfile.TarFile, destination: Path) -> None:
    root = destination.resolve()
    for member in archive.getmembers():
        candidate = (destination / member.name).resolve()
        if candidate != root and root not in candidate.parents:
            raise HarnessError(f"unsafe archive path: {member.name}")
    archive.extractall(destination)


def load_instance(
    instance_id: str,
    *,
    dataset_revision: str,
    instance_json: Path | None,
) -> tuple[dict[str, Any], str]:
    if instance_json:
        instance = read_json(instance_json)
        if instance.get("instance_id") != instance_id:
            raise HarnessError(
                f"fixture instance_id {instance.get('instance_id')!r} does not match "
                f"{instance_id!r}"
            )
        return instance, instance.get("dataset_revision", dataset_revision)

    missing = [
        package
        for package in ("datasets", "huggingface_hub")
        if importlib.util.find_spec(package) is None
    ]
    if missing:
        raise HarnessError(
            "dataset materialization requires Python packages: "
            + ", ".join(missing)
            + ". Install with `python -m pip install datasets huggingface_hub`."
        )

    from datasets import load_dataset  # type: ignore[import-not-found]
    from huggingface_hub import HfApi  # type: ignore[import-not-found]

    resolved_revision = HfApi().dataset_info(
        DATASET_NAME, revision=dataset_revision
    ).sha
    dataset = load_dataset(
        DATASET_NAME,
        split=DATASET_SPLIT,
        revision=resolved_revision,
    )
    matches = [dict(row) for row in dataset if row["instance_id"] == instance_id]
    if len(matches) != 1:
        raise HarnessError(
            f"expected exactly one {instance_id!r} row in {DATASET_NAME}, "
            f"found {len(matches)}"
        )
    return matches[0], resolved_revision


def copy_fixture_source(source: Path, checkout: Path) -> None:
    if not source.is_dir():
        raise HarnessError(f"fixture source does not exist: {source}")
    shutil.copytree(
        source,
        checkout,
        ignore=shutil.ignore_patterns(".git", ".oh", "__pycache__"),
    )


def fetch_base_snapshot(instance: Mapping[str, Any], checkout: Path) -> None:
    repository = str(instance["repo"])
    base_commit = str(instance["base_commit"])
    repository_url = (
        repository
        if repository.startswith(("https://", "ssh://", "git@"))
        else f"https://github.com/{repository}.git"
    )
    with tempfile.TemporaryDirectory(prefix="swebench-upstream-") as temporary:
        source = Path(temporary) / "source"
        source.mkdir()
        git_output(source, "init", "--quiet")
        git_output(source, "remote", "add", "origin", repository_url)
        completed = subprocess.run(
            ["git", "fetch", "--quiet", "--depth", "1", "origin", base_commit],
            cwd=source,
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise HarnessError(
                f"unable to fetch {repository}@{base_commit}: "
                f"{completed.stderr.strip()}"
            )
        archive_path = Path(temporary) / "snapshot.tar"
        with archive_path.open("wb") as archive_file:
            archived = subprocess.run(
                ["git", "archive", "--format=tar", "FETCH_HEAD"],
                cwd=source,
                check=False,
                stdout=archive_file,
                stderr=subprocess.PIPE,
            )
        if archived.returncode != 0:
            raise HarnessError(
                f"unable to archive {repository}@{base_commit}: "
                f"{archived.stderr.decode(errors='replace').strip()}"
            )
        checkout.mkdir(parents=True)
        with tarfile.open(archive_path) as archive:
            safe_extract(archive, checkout)


def initialize_isolated_checkout(checkout: Path) -> dict[str, Any]:
    git_output(checkout, "init", "--quiet")
    git_output(checkout, "config", "user.name", "SWE-bench RNA Harness")
    git_output(checkout, "config", "user.email", "rna-harness@example.invalid")
    git_output(checkout, "add", "--all")
    git_output(
        checkout,
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "SWE-bench benchmark base snapshot",
    )
    proof = {
        "head": git_output(checkout, "rev-parse", "HEAD"),
        "history_commit_count": int(
            git_output(checkout, "rev-list", "--count", "HEAD")
        ),
        "remotes": git_output(checkout, "remote", "-v").splitlines(),
        "status": git_output(checkout, "status", "--short"),
    }
    if proof["history_commit_count"] != 1 or proof["remotes"]:
        raise HarnessError(f"checkout isolation failed: {proof}")
    return proof


def materialize_checkout(
    instance: Mapping[str, Any],
    checkout: Path,
    *,
    fixture_source: Path | None,
) -> dict[str, Any]:
    if checkout.exists():
        raise HarnessError(f"checkout path already exists: {checkout}")
    if fixture_source:
        copy_fixture_source(fixture_source, checkout)
    else:
        fetch_base_snapshot(instance, checkout)
    return initialize_isolated_checkout(checkout)


def unknown_metric(reason: str = "executor/provider did not report this category") -> dict[str, Any]:
    return {"status": "unknown", "value": None, "source": None, "reason": reason}


def stage_ledger_skeleton() -> dict[str, Any]:
    stages: dict[str, Any] = {}
    for stage in STAGE_NAMES:
        stages[stage] = {field: unknown_metric() for field in TOKEN_FIELDS}
    stages["rna_tool_results_orientation_and_planning"]["delivered_bytes"] = {
        "status": "pending",
        "value": None,
        "source": "mcp_trace",
        "reason": None,
    }
    stages["rna_tool_results_orientation_and_planning"]["mcp_calls"] = {
        "status": "pending",
        "value": None,
        "source": "mcp_trace",
        "reason": None,
    }
    return {
        "schema_version": 2,
        "generated_at": utc_now(),
        "rule": (
            "Token counts are recorded only when reported by the executor/provider; "
            "byte sizes are never converted into inferred tokens."
        ),
        "stages": stages,
        "executor_total": {
            field: unknown_metric("no provider total reported") for field in TOKEN_FIELDS
        },
    }


def make_task_prompt(instance: Mapping[str, Any], path: Path) -> None:
    problem = str(instance.get("problem_statement", "")).strip()
    hints = str(instance.get("hints_text", "")).strip()
    path.write_text(
        "\n".join(
            [
                f"# SWE-bench task: {instance['instance_id']}",
                "",
                " ".join(
                    [
                        "Solve the issue in the current isolated checkout.",
                        "Use the configured `rna-server` MCP tools for repository",
                        "orientation before editing. Do not search the network for",
                        "the upstream patch or commit history. Make the smallest",
                        "correct implementation and run focused verification.",
                    ]
                ),
                "",
                "## Problem statement",
                "",
                problem,
                "",
                "## Hints supplied by the benchmark",
                "",
                hints or "(none)",
                "",
            ]
        ),
        encoding="utf-8",
    )


def proxy_config(
    *,
    proxy_script: Path,
    rna_binary: Path,
    checkout: Path,
    trace_path: Path,
    stderr_path: Path,
) -> dict[str, Any]:
    return {
        "mcpServers": {
            "rna-server": {
                "type": "stdio",
                "command": sys.executable,
                "args": [
                    str(proxy_script),
                    "--rna-binary",
                    str(rna_binary),
                    "--business-context",
                    BUSINESS_CONTEXT_MODE,
                    "--repo",
                    str(checkout),
                    "--trace",
                    str(trace_path),
                    "--server-stderr",
                    str(stderr_path),
                ],
            }
        }
    }


def build_evaluator_command(
    *,
    python: str,
    dataset_name: str,
    predictions_path: Path,
    instance_id: str,
    run_id: str,
) -> list[str]:
    return [
        python,
        "-m",
        "swebench.harness.run_evaluation",
        "--dataset_name",
        dataset_name,
        "--split",
        DATASET_SPLIT,
        "--predictions_path",
        str(predictions_path),
        "--max_workers",
        "1",
        "--run_id",
        run_id,
        "--instance_ids",
        instance_id,
    ]


def parse_executor_config(
    executor_command: str | None, executor_config: Path | None
) -> tuple[list[str], dict[str, Any]]:
    if bool(executor_command) == bool(executor_config):
        raise HarnessError(
            "provide exactly one of --executor-command or --executor-config"
        )
    if executor_config:
        config = read_json(executor_config)
        command = config.get("command")
        if isinstance(command, str):
            argv = ["/bin/sh", "-lc", command]
        elif isinstance(command, list) and all(isinstance(item, str) for item in command):
            argv = list(command)
        else:
            raise HarnessError("executor config `command` must be a string or string array")
        return argv, config
    assert executor_command is not None
    return ["/bin/sh", "-lc", executor_command], {
        "command": executor_command,
        "model": {"status": "unspecified"},
    }


def meaningful_checkout_changes(root: Path) -> list[str]:
    completed = subprocess.run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all", "-z"],
        cwd=root,
        check=False,
        capture_output=True,
    )
    if completed.returncode != 0:
        return []
    ignored_parts = {
        ".git",
        ".oh",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        "__pycache__",
        "node_modules",
        "target",
    }
    paths: list[str] = []
    entries = iter(
        completed.stdout.decode("utf-8", errors="replace").split("\0")
    )
    for entry in entries:
        if not entry:
            continue
        status = entry[:2]
        changed_paths = [entry[3:] if len(entry) > 3 else ""]
        if "R" in status or "C" in status:
            original_path = next(entries, "")
            if original_path:
                changed_paths.append(original_path)
        for path in changed_paths:
            if path and not ignored_parts.intersection(Path(path).parts):
                paths.append(path)
    return sorted(set(paths))


class FirstEditMonitor:
    def __init__(self, checkout: Path) -> None:
        self.checkout = checkout
        self.baseline = meaningful_checkout_changes(checkout)
        self.started_monotonic: float | None = None
        self.started_at: str | None = None
        self.first_edit_monotonic: float | None = None
        self.first_edit_at: str | None = None
        self.first_changed_paths: list[str] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        self.started_monotonic = time.monotonic()
        self.started_at = utc_now()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _run(self) -> None:
        while not self._stop.wait(0.1):
            current = meaningful_checkout_changes(self.checkout)
            changed = sorted(set(current) - set(self.baseline))
            if changed:
                self.first_edit_monotonic = time.monotonic()
                self.first_edit_at = utc_now()
                self.first_changed_paths = changed
                return

    def stop(self) -> dict[str, Any]:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=2)
        elapsed = None
        if self.started_monotonic is not None and self.first_edit_monotonic is not None:
            elapsed = round(self.first_edit_monotonic - self.started_monotonic, 3)
        return {
            "monitor_started_at": self.started_at,
            "first_edit_at": self.first_edit_at,
            "seconds_to_first_edit": elapsed,
            "first_changed_paths": self.first_changed_paths,
            "status": "observed" if elapsed is not None else "not_observed",
        }


def run_executor(
    command: Sequence[str],
    *,
    cwd: Path,
    env: Mapping[str, str],
    stdout_path: Path,
    stderr_path: Path,
    timed_trace_path: Path,
    timeout: float | None,
    monitor: FirstEditMonitor,
) -> CommandResult:
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    started_wall = utc_now()
    started = time.monotonic()
    monitor.start()
    process = subprocess.Popen(
        list(command),
        cwd=cwd,
        env=dict(env),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=0,
        start_new_session=True,
    )
    trace_lock = threading.Lock()

    def pump(source: Any, destination: Path, stream: str) -> None:
        with destination.open("wb") as raw:
            while True:
                line = source.readline()
                if not line:
                    break
                observed_at = utc_now()
                observed_monotonic = time.monotonic()
                raw.write(line)
                raw.flush()
                with trace_lock, timed_trace_path.open("a", encoding="utf-8") as trace:
                    trace.write(
                        json.dumps(
                            {
                                "observed_at": observed_at,
                                "observed_monotonic": observed_monotonic,
                                "stream": stream,
                                "line": line.decode("utf-8", errors="replace").rstrip("\n"),
                            },
                            sort_keys=True,
                        )
                        + "\n"
                    )

    stdout_thread = threading.Thread(
        target=pump, args=(process.stdout, stdout_path, "stdout"), daemon=True
    )
    stderr_thread = threading.Thread(
        target=pump, args=(process.stderr, stderr_path, "stderr"), daemon=True
    )
    stdout_thread.start()
    stderr_thread.start()
    try:
        exit_code = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        exit_code = 124
    stdout_thread.join()
    stderr_thread.join()
    if process.stdout is not None:
        process.stdout.close()
    if process.stderr is not None:
        process.stderr.close()
    return CommandResult(
        command=list(command),
        exit_code=exit_code,
        started_at=started_wall,
        finished_at=utc_now(),
        duration_seconds=round(time.monotonic() - started, 3),
    )


def parse_json_lines(path: Path) -> Iterable[dict[str, Any]]:
    if not path.exists():
        return []
    rows: list[dict[str, Any]] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            rows.append(value)
    return rows


def number_from(mapping: Mapping[str, Any], keys: Sequence[str]) -> float | int | None:
    for key in keys:
        value = mapping.get(key)
        if isinstance(value, (int, float)):
            return value
    return None


def collect_usage_and_fallbacks(
    timed_trace: Path,
    *,
    first_edit_monotonic: float | None,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    usage_fields = (
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
        "reasoning_tokens",
    )
    before = {field: 0 for field in usage_fields}
    after = {field: 0 for field in usage_fields}
    observed = {"before": False, "after": False}
    usage_by_message_id: dict[str, tuple[str, dict[str, float | int | None]]] = {}
    anonymous_usage: list[tuple[str, dict[str, float | int | None]]] = []
    totals: dict[str, float | int | None] = {
        "input_tokens": None,
        "cache_creation_input_tokens": None,
        "cache_read_input_tokens": None,
        "output_tokens": None,
        "reasoning_tokens": None,
        "cost_usd": None,
    }
    fallback_events: list[dict[str, Any]] = []
    for wrapper in parse_json_lines(timed_trace):
        raw_line = wrapper.get("line")
        if not isinstance(raw_line, str):
            continue
        try:
            event = json.loads(raw_line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        event_time = wrapper.get("observed_monotonic")
        bucket_name = (
            "before"
            if first_edit_monotonic is None
            or not isinstance(event_time, (int, float))
            or event_time < first_edit_monotonic
            else "after"
        )
        event_type = event.get("type")
        if event_type == "result":
            total_usage = event.get("usage")
            if isinstance(total_usage, dict):
                totals["input_tokens"] = number_from(
                    total_usage, ("input_tokens", "inputTokens", "prompt_tokens")
                )
                totals["cache_creation_input_tokens"] = number_from(
                    total_usage,
                    ("cache_creation_input_tokens", "cacheCreationInputTokens"),
                )
                totals["cache_read_input_tokens"] = number_from(
                    total_usage,
                    ("cache_read_input_tokens", "cacheReadInputTokens"),
                )
                totals["output_tokens"] = number_from(
                    total_usage,
                    ("output_tokens", "outputTokens", "completion_tokens"),
                )
                totals["reasoning_tokens"] = number_from(
                    total_usage, ("reasoning_tokens", "reasoningTokens")
                )
            cost = number_from(
                event,
                ("total_cost_usd", "totalCostUsd", "cost_usd", "costUsd"),
            )
            if isinstance(cost, (int, float)):
                totals["cost_usd"] = float(cost)
            candidates: list[Any] = []
        elif event_type == "assistant" and isinstance(event.get("message"), dict):
            message = event["message"]
            candidates = [(message.get("id"), message.get("usage"))]
        else:
            candidates = [(None, event.get("usage"))]
        for message_id, usage in candidates:
            if not isinstance(usage, dict):
                continue
            values = {
                "input_tokens": number_from(
                    usage, ("input_tokens", "inputTokens", "prompt_tokens")
                ),
                "cache_creation_input_tokens": number_from(
                    usage,
                    ("cache_creation_input_tokens", "cacheCreationInputTokens"),
                ),
                "cache_read_input_tokens": number_from(
                    usage,
                    ("cache_read_input_tokens", "cacheReadInputTokens"),
                ),
                "output_tokens": number_from(
                    usage, ("output_tokens", "outputTokens", "completion_tokens")
                ),
                "reasoning_tokens": number_from(
                    usage, ("reasoning_tokens", "reasoningTokens")
                ),
            }
            if any(value is not None for value in values.values()):
                if isinstance(message_id, str) and message_id:
                    previous = usage_by_message_id.get(message_id)
                    stable_bucket = previous[0] if previous else bucket_name
                    usage_by_message_id[message_id] = (stable_bucket, values)
                else:
                    anonymous_usage.append((bucket_name, values))
        tool = event.get("tool_name") or event.get("name")
        if tool in {"Read", "Grep", "Glob", "Bash", "Shell", "Computer"}:
            fallback_events.append(
                {
                    "observed_at": wrapper.get("observed_at"),
                    "tool": tool,
                    "source": "executor_transcript",
                    "event": event,
                }
            )
        message = event.get("message")
        if isinstance(message, dict):
            content = message.get("content")
            if isinstance(content, list):
                for item in content:
                    if not isinstance(item, dict) or item.get("type") != "tool_use":
                        continue
                    nested_tool = item.get("name")
                    if nested_tool not in {
                        "Read",
                        "Grep",
                        "Glob",
                        "Bash",
                        "Shell",
                        "Computer",
                    }:
                        continue
                    fallback_events.append(
                        {
                            "observed_at": wrapper.get("observed_at"),
                            "tool": nested_tool,
                            "source": "executor_transcript",
                            "event": item,
                        }
                    )
    for bucket_name, values in [
        *usage_by_message_id.values(),
        *anonymous_usage,
    ]:
        observed[bucket_name] = True
        bucket = before if bucket_name == "before" else after
        for key, value in values.items():
            if value is not None:
                bucket[key] += value
    return {
        "before": before if observed["before"] else None,
        "after": after if observed["after"] else None,
        "totals": totals,
    }, fallback_events


def summarize_mcp_trace(
    path: Path, *, first_edit_at: str | None
) -> dict[str, Any]:
    rows = list(parse_json_lines(path))
    calls = [
        row
        for row in rows
        if row.get("direction") == "client_to_server"
        and isinstance(row.get("method"), str)
    ]
    responses = [
        row
        for row in rows
        if row.get("direction") == "server_to_client"
        and row.get("response_to_method")
    ]
    tool_calls = [
        row
        for row in calls
        if row.get("method") == "tools/call"
    ]
    tool_responses = [
        row
        for row in responses
        if row.get("response_to_method") == "tools/call"
    ]
    successful_tool_responses = [
        row for row in tool_responses if not row.get("is_error", False)
    ]
    orientation_calls = [
        row
        for row in tool_calls
        if first_edit_at is None or str(row.get("observed_at", "")) < first_edit_at
    ]
    orientation_responses = [
        row
        for row in tool_responses
        if first_edit_at is None or str(row.get("observed_at", "")) < first_edit_at
    ]
    successful_orientation_responses = [
        row for row in orientation_responses if not row.get("is_error", False)
    ]
    return {
        "trace_rows": len(rows),
        "calls": len(calls),
        "tool_calls": len(tool_calls),
        "methods": [row.get("method") for row in calls],
        "tools": [
            row.get("tool_name") for row in tool_calls if row.get("tool_name")
        ],
        "delivered_tool_result_bytes": sum(
            int(row.get("message_bytes", 0)) for row in tool_responses
        ),
        "orientation_tool_calls": len(orientation_calls),
        "orientation_tools": [
            row.get("tool_name") for row in orientation_calls if row.get("tool_name")
        ],
        "orientation_delivered_tool_result_bytes": sum(
            int(row.get("message_bytes", 0)) for row in orientation_responses
        ),
        "successful_tool_responses": len(successful_tool_responses),
        "successful_orientation_tool_responses": len(
            successful_orientation_responses
        ),
        "observed_real_mcp_use": bool(successful_orientation_responses),
    }


def known_metric(value: Any, source: str) -> dict[str, Any]:
    return {"status": "reported", "value": value, "source": source, "reason": None}


def merge_stage_ledger(
    ledger: dict[str, Any],
    *,
    executor_report: Path,
    usage: Mapping[str, Any],
    mcp_summary: Mapping[str, Any],
) -> None:
    if executor_report.exists():
        report = read_json(executor_report)
        reported_stages = report.get("stages", {})
        if isinstance(reported_stages, dict):
            for stage_name, metrics in reported_stages.items():
                if stage_name not in ledger["stages"] or not isinstance(metrics, dict):
                    continue
                for field, value in metrics.items():
                    if field in ledger["stages"][stage_name] and value is not None:
                        ledger["stages"][stage_name][field] = known_metric(
                            value, "executor_report"
                        )
    before = usage.get("before")
    if isinstance(before, dict):
        for field, value in before.items():
            metric = ledger["stages"]["frontier_before_first_edit"].get(field)
            if isinstance(metric, dict) and metric.get("status") == "unknown":
                ledger["stages"]["frontier_before_first_edit"][field] = known_metric(
                    value, "timestamped_executor_transcript"
                )
    after = usage.get("after")
    if isinstance(after, dict):
        ledger["observed_intervals"] = {
            "post_first_edit_until_executor_exit": {
                field: known_metric(
                    value, "timestamped_executor_transcript_until_executor_exit"
                )
                for field, value in after.items()
            },
            "note": (
                "No handoff or patch-complete boundary was observed, so this interval "
                "is not assigned to a required handoff or verification stage."
            ),
        }
    totals = usage.get("totals")
    if isinstance(totals, dict):
        for field, value in totals.items():
            if field in ledger["executor_total"] and value is not None:
                ledger["executor_total"][field] = known_metric(
                    value, "executor_transcript"
                )
    orientation = ledger["stages"]["rna_tool_results_orientation_and_planning"]
    orientation["delivered_bytes"] = known_metric(
        mcp_summary["orientation_delivered_tool_result_bytes"], "mcp_trace"
    )
    orientation["mcp_calls"] = known_metric(
        mcp_summary["orientation_tool_calls"], "mcp_trace"
    )
    ledger["generated_at"] = utc_now()


def parse_enrichment_state(
    scan: CommandResult,
    scan_stdout: Path,
    scan_stderr: Path,
    embedding: CommandResult | None,
    readiness: CommandResult,
) -> dict[str, Any]:
    log_paths = {
        "scan_stdout": scan_stdout,
        "scan_stderr": scan_stderr,
    }
    if embedding is not None:
        log_paths["embedding_stdout"] = scan_stdout.parent / "embeddings.stdout.log"
        log_paths["embedding_stderr"] = scan_stdout.parent / "embeddings.stderr.log"
    combined = "\n".join(
        path.read_text(encoding="utf-8", errors="replace")
        for path in log_paths.values()
        if path.exists()
    )
    lower = combined.lower()
    if "degraded" in lower or (embedding is not None and embedding.exit_code != 0):
        observed = "degraded"
    elif scan.exit_code == 0 and readiness.exit_code == 0:
        observed = "ready"
    else:
        observed = "failed"
    return {
        "observed": observed,
        "scan_exit_code": scan.exit_code,
        "embedding_exit_code": embedding.exit_code if embedding else None,
        "readiness_probe_exit_code": readiness.exit_code,
        "degraded_evidence": [
            line.strip()
            for line in combined.splitlines()
            if "degraded" in line.lower()
            or "skipped" in line.lower()
            or "not compiled in" in line.lower()
        ],
        "raw_logs": {name: str(path) for name, path in log_paths.items()},
    }


def parse_business_context_state(log_paths: Mapping[str, Path]) -> dict[str, Any]:
    """Fail closed unless RNA reports the benchmark's selected isolation mode."""
    mode_pattern = re.compile(r"business context:\s*(enabled|disabled)", re.IGNORECASE)
    counts_pattern = re.compile(
        r"excluded producer inputs:\s*(\d+)\s+\.oh file\(s\),\s*"
        r"(\d+)\s+Git-history producer\(s\)",
        re.IGNORECASE,
    )
    observations: list[dict[str, Any]] = []
    observed_modes: list[str] = []
    observed_counts: list[tuple[int, int]] = []
    for name, path in log_paths.items():
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        modes = [match.lower() for match in mode_pattern.findall(text)]
        counts = [
            (int(files), int(producers))
            for files, producers in counts_pattern.findall(text)
        ]
        observed_modes.extend(modes)
        observed_counts.extend(counts)
        if modes or counts:
            observations.append(
                {
                    "source": name,
                    "modes": modes,
                    "counts": [
                        {
                            "business_artifact_files": files,
                            "git_history_producers": producers,
                        }
                        for files, producers in counts
                    ],
                }
            )

    if not observed_modes or not observed_counts:
        raise HarnessError(
            "RNA pre-warm omitted business-context mode and exclusion diagnostics"
        )
    unexpected = sorted(set(observed_modes) - {BUSINESS_CONTEXT_MODE})
    if unexpected:
        raise HarnessError(
            "RNA benchmark command escaped disabled business-context mode: "
            + ", ".join(unexpected)
        )
    return {
        "selected_mode": BUSINESS_CONTEXT_MODE,
        "diagnostic_status": "validated",
        "business_artifact_files": max(files for files, _ in observed_counts),
        "git_history_producers": max(producers for _, producers in observed_counts),
        "observations": observations,
    }


def prewarm_rna(
    *,
    rna_binary: Path,
    checkout: Path,
    run_dir: Path,
    condition: str,
    timeout: float | None,
) -> tuple[dict[str, Any], list[CommandResult]]:
    logs = run_dir / "prewarm"
    commands: list[CommandResult] = []
    scan_stdout = logs / "scan.stdout.log"
    scan_stderr = logs / "scan.stderr.log"
    scan_command = rna_command(rna_binary, "scan", "--repo", str(checkout))
    if condition == "structural":
        scan_command.append("--extract-only")
    else:
        scan_command.extend(["--full", "--no-embed"])
    scan = run_logged(
        scan_command,
        cwd=checkout,
        stdout_path=scan_stdout,
        stderr_path=scan_stderr,
        timeout=timeout,
    )
    commands.append(scan)
    embedding: CommandResult | None = None
    if condition == "full":
        embedding = run_logged(
            rna_command(
                rna_binary,
                "enrich",
                "--capability",
                "embeddings",
                "--scope",
                "repo",
                "--repo",
                str(checkout),
                "--no-background-continuation",
            ),
            cwd=checkout,
            stdout_path=logs / "embeddings.stdout.log",
            stderr_path=logs / "embeddings.stderr.log",
            timeout=timeout,
        )
        commands.append(embedding)
    readiness_stdout = logs / "readiness.stdout.log"
    readiness_stderr = logs / "readiness.stderr.log"
    readiness = run_logged(
        rna_command(
            rna_binary,
            "search",
            "--repo",
            str(checkout),
            "--sort-by",
            "importance",
            "--limit",
            "1",
            "--verbose",
        ),
        cwd=checkout,
        stdout_path=readiness_stdout,
        stderr_path=readiness_stderr,
        timeout=60,
    )
    commands.append(readiness)
    state = parse_enrichment_state(
        scan, scan_stdout, scan_stderr, embedding, readiness
    )
    business_context_logs = {
        "scan_stdout": scan_stdout,
        "scan_stderr": scan_stderr,
        "readiness_stdout": readiness_stdout,
        "readiness_stderr": readiness_stderr,
    }
    if embedding is not None:
        business_context_logs["embedding_stdout"] = logs / "embeddings.stdout.log"
        business_context_logs["embedding_stderr"] = logs / "embeddings.stderr.log"
    state["business_context"] = parse_business_context_state(business_context_logs)
    state["selected_condition"] = condition
    if readiness.exit_code != 0:
        raise HarnessError(
            "RNA pre-warm did not produce a queryable extracted graph; see prewarm logs"
        )
    return state, commands


def collect_patch(checkout: Path) -> str:
    # Intent-to-add exposes untracked content to diff without staging it.
    git_output(checkout, "add", "--intent-to-add", "--all")
    completed = subprocess.run(
        ["git", "diff", "--binary", "--no-ext-diff", "HEAD"],
        cwd=checkout,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise HarnessError(f"unable to collect patch: {completed.stderr.strip()}")
    return completed.stdout


def evaluator_result(evaluation_dir: Path, instance_id: str) -> dict[str, Any]:
    candidates: list[Path] = []
    for path in evaluation_dir.rglob("*.json"):
        candidates.append(path)
        try:
            value = read_json(path)
        except (OSError, json.JSONDecodeError):
            continue
        serialized = json.dumps(value)
        if instance_id not in serialized:
            continue
        lowered = serialized.lower()
        resolved_ids = value.get("resolved_ids", []) if isinstance(value, dict) else []
        if '"resolved": true' in lowered or instance_id in resolved_ids:
            return {"status": "resolved", "evidence": str(path)}
        if '"resolved": false' in lowered:
            return {"status": "unresolved", "evidence": str(path)}
    return {
        "status": "unknown",
        "evidence": [str(path) for path in candidates],
        "reason": "no machine-readable resolved flag was found; inspect evaluator logs",
    }


def validate_prerequisites(args: argparse.Namespace, *, dry_run: bool) -> dict[str, Any]:
    results = {
        "git": shutil.which("git"),
        "rna_binary": str(args.rna_binary),
        "rna_binary_available": args.rna_binary.is_file(),
        "rna_version": command_version(str(args.rna_binary)),
        "python": sys.executable,
        "docker": shutil.which("docker"),
        "docker_server_version": None,
        "swebench_package": importlib.util.find_spec("swebench") is not None,
        "swebench_package_version": None,
        "datasets_package": importlib.util.find_spec("datasets") is not None,
        "datasets_package_version": None,
        "huggingface_hub_package": importlib.util.find_spec("huggingface_hub")
        is not None,
        "huggingface_hub_package_version": None,
    }
    for package, key in (
        ("swebench", "swebench_package_version"),
        ("datasets", "datasets_package_version"),
        ("huggingface_hub", "huggingface_hub_package_version"),
    ):
        if results[f"{package}_package"]:
            with contextlib.suppress(importlib.metadata.PackageNotFoundError):
                results[key] = importlib.metadata.version(package)
    if not results["git"]:
        raise HarnessError("git is required")
    if not args.rna_binary.is_file() and not dry_run:
        raise HarnessError(f"RNA binary not found: {args.rna_binary}")
    if not dry_run:
        if not results["docker"]:
            raise HarnessError("Docker is required for official SWE-bench evaluation")
        docker = subprocess.run(
            ["docker", "info", "--format", "{{.ServerVersion}}"],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if docker.returncode != 0:
            raise HarnessError(
                "Docker CLI is installed but the daemon is unavailable: "
                + docker.stderr.strip()
            )
        results["docker_server_version"] = docker.stdout.strip()
        if not results["swebench_package"]:
            raise HarnessError(
                "the official `swebench` package is required for evaluation"
            )
    return results


def arguments(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run one SWE-bench Verified instance through a pre-warmed RNA MCP "
            "endpoint, an external executor, and the official evaluator."
        )
    )
    parser.add_argument("instance_id")
    executor = parser.add_mutually_exclusive_group(required=True)
    executor.add_argument(
        "--executor-command",
        help=(
            "shell command for the agent executor; it receives SWEBENCH_* "
            "environment variables including SWEBENCH_MCP_CONFIG"
        ),
    )
    executor.add_argument(
        "--executor-config",
        type=Path,
        help="JSON file with `command` and recorded model/provider configuration",
    )
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument(
        "--dataset-revision",
        default="main",
        help="Hugging Face dataset revision, resolved to a commit in the manifest",
    )
    parser.add_argument(
        "--rna-binary",
        type=Path,
        default=Path(shutil.which("repo-native-alignment") or ""),
    )
    parser.add_argument(
        "--enrichment-condition",
        choices=("full", "call-references", "structural"),
        default="full",
    )
    parser.add_argument("--model-name", default="external-executor")
    parser.add_argument("--executor-timeout-seconds", type=float, default=7200)
    parser.add_argument("--prewarm-timeout-seconds", type=float, default=3600)
    parser.add_argument("--evaluator-timeout-seconds", type=float, default=7200)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--instance-json",
        type=Path,
        help="local instance record for fixture/dry-run use",
    )
    parser.add_argument(
        "--fixture-source",
        type=Path,
        help="local source tree for fixture/dry-run checkout materialization",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = arguments(argv)
    if (args.instance_json or args.fixture_source) and not args.dry_run:
        print(
            "ERROR: --instance-json and --fixture-source are restricted to --dry-run",
            file=sys.stderr,
        )
        return 1
    run_dir = args.output_dir.resolve()
    if run_dir.exists() and any(run_dir.iterdir()):
        print(
            f"ERROR: output directory must be empty or absent: {run_dir}",
            file=sys.stderr,
        )
        return 1
    run_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = run_dir / "manifest.json"
    started = time.monotonic()
    manifest: dict[str, Any] = {
        "schema_version": 1,
        "status": "initializing",
        "started_at": utc_now(),
        "instance_id": args.instance_id,
        "dataset": {
            "name": DATASET_NAME,
            "split": DATASET_SPLIT,
            "requested_revision": args.dataset_revision,
            "resolved_revision": None,
        },
        "timings": {},
        "artifacts": {},
    }
    write_json(manifest_path, manifest)
    try:
        executor_command, executor_config = parse_executor_config(
            args.executor_command, args.executor_config
        )
        prerequisites = validate_prerequisites(args, dry_run=args.dry_run)
        instance, resolved_revision = load_instance(
            args.instance_id,
            dataset_revision=args.dataset_revision,
            instance_json=args.instance_json,
        )
        manifest["dataset"]["resolved_revision"] = resolved_revision
        dataset_snapshot = run_dir / "dataset-instance.json"
        manifest["instance"] = {
            "repo": instance["repo"],
            "base_commit": instance["base_commit"],
        }
        manifest["prerequisites"] = prerequisites
        manifest["executor"] = {
            "command": executor_command,
            "configuration": executor_config,
        }
        manifest["rna"] = {
            "binary": str(args.rna_binary.resolve()),
            "revision": {
                "version": prerequisites["rna_version"],
                "sha256": (
                    sha256_file(args.rna_binary.resolve())
                    if args.rna_binary.is_file()
                    else None
                ),
            },
            "selected_enrichment_condition": args.enrichment_condition,
            "business_context": {
                "selected_mode": BUSINESS_CONTEXT_MODE,
                "diagnostic_status": "not_run",
                "business_artifact_files": None,
                "git_history_producers": None,
                "observations": [],
            },
        }
        checkout = run_dir / "checkout"
        materialize_started = time.monotonic()
        isolation = materialize_checkout(
            instance,
            checkout,
            fixture_source=args.fixture_source,
        )
        manifest["checkout_isolation"] = isolation
        manifest["timings"]["materialize_seconds"] = round(
            time.monotonic() - materialize_started, 3
        )
        task_prompt = run_dir / "task.md"
        make_task_prompt(instance, task_prompt)
        ledger = stage_ledger_skeleton()
        ledger_path = run_dir / "stage-ledger.json"
        write_json(ledger_path, ledger)
        proxy_script = Path(__file__).with_name("swebench_rna_mcp_proxy.py").resolve()
        mcp_trace = run_dir / "mcp-trace.jsonl"
        mcp_config_path = run_dir / "mcp-config.json"
        write_json(
            mcp_config_path,
            proxy_config(
                proxy_script=proxy_script,
                rna_binary=args.rna_binary.resolve(),
                checkout=checkout,
                trace_path=mcp_trace,
                stderr_path=run_dir / "rna-mcp.stderr.log",
            ),
        )
        predictions_path = run_dir / "prediction.jsonl"
        evaluation_dir = run_dir / "evaluation"
        run_id = (
            f"rna-{args.instance_id.replace('__', '-').replace('/', '-')}-"
            f"{dt.datetime.now().strftime('%Y%m%d%H%M%S')}"
        )
        evaluator_command = build_evaluator_command(
            python=sys.executable,
            dataset_name=str(dataset_snapshot),
            predictions_path=predictions_path,
            instance_id=args.instance_id,
            run_id=run_id,
        )
        write_json(
            evaluation_dir / "command.json",
            {"argv": evaluator_command, "cwd": str(evaluation_dir)},
        )
        manifest["artifacts"] = {
            "checkout": str(checkout),
            "task_prompt": str(task_prompt),
            "dataset_snapshot": str(dataset_snapshot),
            "mcp_config": str(mcp_config_path),
            "mcp_trace": str(mcp_trace),
            "executor_stdout": str(run_dir / "executor.stdout.log"),
            "executor_stderr": str(run_dir / "executor.stderr.log"),
            "executor_timed_trace": str(run_dir / "executor-timed-trace.jsonl"),
            "executor_report": str(run_dir / "executor-report.json"),
            "fallback_events": str(run_dir / "fallback-events.jsonl"),
            "prediction": str(predictions_path),
            "stage_ledger": str(ledger_path),
            "evaluation": str(evaluation_dir),
        }
        if args.dry_run:
            write_json(dataset_snapshot, [instance])
            prediction = {
                "instance_id": args.instance_id,
                "model_name_or_path": args.model_name,
                "model_patch": "",
            }
            write_jsonl(predictions_path, [prediction])
            manifest["status"] = "dry_run_complete"
            manifest["dry_run"] = {
                "executor_would_run": executor_command,
                "evaluator_would_run": evaluator_command,
                "tokens_spent": 0,
            }
            manifest["finished_at"] = utc_now()
            manifest["timings"]["wall_clock_seconds"] = round(
                time.monotonic() - started, 3
            )
            write_json(manifest_path, manifest)
            print(f"Dry run bundle: {run_dir}")
            return 0

        prewarm_started = time.monotonic()
        enrichment_state, prewarm_commands = prewarm_rna(
            rna_binary=args.rna_binary.resolve(),
            checkout=checkout,
            run_dir=run_dir,
            condition=args.enrichment_condition,
            timeout=args.prewarm_timeout_seconds,
        )
        manifest["rna"]["enrichment_state"] = enrichment_state
        manifest["rna"]["business_context"] = enrichment_state["business_context"]
        manifest["rna"]["prewarm_commands"] = [
            dataclasses.asdict(command) for command in prewarm_commands
        ]
        manifest["timings"]["prewarm_seconds"] = round(
            time.monotonic() - prewarm_started, 3
        )
        write_json(manifest_path, manifest)

        executor_report = run_dir / "executor-report.json"
        executor_env = os.environ.copy()
        executor_env.update(
            {
                "SWEBENCH_INSTANCE_ID": args.instance_id,
                "SWEBENCH_CHECKOUT": str(checkout),
                "SWEBENCH_TASK_PROMPT": str(task_prompt),
                "SWEBENCH_RUN_DIR": str(run_dir),
                "SWEBENCH_MCP_CONFIG": str(mcp_config_path),
                "SWEBENCH_EXECUTOR_REPORT": str(executor_report),
                "SWEBENCH_STAGE_LEDGER": str(ledger_path),
                "SWEBENCH_MCP_TRACE": str(mcp_trace),
            }
        )
        monitor = FirstEditMonitor(checkout)
        executor_result = run_executor(
            executor_command,
            cwd=checkout,
            env=executor_env,
            stdout_path=run_dir / "executor.stdout.log",
            stderr_path=run_dir / "executor.stderr.log",
            timed_trace_path=run_dir / "executor-timed-trace.jsonl",
            timeout=args.executor_timeout_seconds,
            monitor=monitor,
        )
        first_edit = monitor.stop()
        manifest["executor"]["result"] = dataclasses.asdict(executor_result)
        manifest["timings"]["executor_seconds"] = executor_result.duration_seconds
        manifest["time_to_first_edit"] = first_edit

        patch = collect_patch(checkout)
        prediction = {
            "instance_id": args.instance_id,
            "model_name_or_path": args.model_name,
            "model_patch": patch,
        }
        write_jsonl(predictions_path, [prediction])
        mcp_summary = summarize_mcp_trace(
            mcp_trace, first_edit_at=first_edit["first_edit_at"]
        )
        manifest["rna"]["mcp_usage"] = mcp_summary
        usage, fallback_events = collect_usage_and_fallbacks(
            run_dir / "executor-timed-trace.jsonl",
            first_edit_monotonic=monitor.first_edit_monotonic,
        )
        if executor_report.exists():
            report = read_json(executor_report)
            supplied_fallbacks = report.get("fallback_events", [])
            if isinstance(supplied_fallbacks, list):
                fallback_events.extend(
                    event for event in supplied_fallbacks if isinstance(event, dict)
                )
        write_jsonl(run_dir / "fallback-events.jsonl", fallback_events)
        merge_stage_ledger(
            ledger,
            executor_report=executor_report,
            usage=usage,
            mcp_summary=mcp_summary,
        )
        write_json(ledger_path, ledger)
        if executor_result.exit_code != 0:
            raise HarnessError(
                f"executor exited {executor_result.exit_code}; artifacts were preserved"
            )
        if not mcp_summary["observed_real_mcp_use"]:
            raise HarnessError(
                "executor completed without observable traffic through the provided "
                "RNA stdio MCP endpoint"
            )
        if not patch.strip():
            raise HarnessError("executor produced no patch")

        # The exact dataset row can contain the gold patch and test patch.
        # Persist it only after the executor has exited so it cannot leak the answer.
        write_json(dataset_snapshot, [instance])
        evaluation_dir.mkdir(parents=True, exist_ok=True)
        evaluator_started = time.monotonic()
        evaluator = run_logged(
            evaluator_command,
            cwd=evaluation_dir,
            stdout_path=evaluation_dir / "stdout.log",
            stderr_path=evaluation_dir / "stderr.log",
            timeout=args.evaluator_timeout_seconds,
        )
        manifest["evaluator"] = {
            "result": dataclasses.asdict(evaluator),
            "outcome": evaluator_result(evaluation_dir, args.instance_id),
        }
        manifest["timings"]["evaluation_seconds"] = round(
            time.monotonic() - evaluator_started, 3
        )
        if evaluator.exit_code != 0:
            manifest["status"] = "evaluation_failed"
        else:
            manifest["status"] = "complete"
        manifest["finished_at"] = utc_now()
        manifest["timings"]["wall_clock_seconds"] = round(
            time.monotonic() - started, 3
        )
        write_json(manifest_path, manifest)
        print(
            f"Run bundle: {run_dir}\n"
            f"Evaluator outcome: {manifest['evaluator']['outcome']['status']}"
        )
        return 0 if evaluator.exit_code == 0 else 1
    except Exception as error:
        manifest["status"] = "failed"
        manifest["error"] = f"{type(error).__name__}: {error}"
        manifest["finished_at"] = utc_now()
        manifest["timings"]["wall_clock_seconds"] = round(
            time.monotonic() - started, 3
        )
        write_json(manifest_path, manifest)
        print(f"ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
