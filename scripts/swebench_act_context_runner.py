#!/usr/bin/env python3
"""Direct Anthropic runner for the frozen SWE-bench ActContext B/C packets.

The public process validates inputs and applies edits.  Only the private
``_api-worker`` subprocess reads the credential file.  This first-stage runner
is intentionally compact so the registered paid qualification can exercise the
complete prompt -> response -> patch -> official evaluator path early.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from scripts import swebench_context_packets as PACKETS


PROTOCOL_PATH = ROOT / "benchmark/swebench-act-context/protocol.json"
POPULATION_PATH = ROOT / "benchmark/swebench-act-context/population.json"
PARSER_PATH = ROOT / "benchmark/swebench-act-context/upstream/edit_patch_v2.py"
LOCK_PATH = ROOT / "scripts/requirements/swebench-act-context-runner.lock"

QUALIFICATION_INSTANCE = "astropy__astropy-13398"
QUALIFICATION_MANIFEST_SHA256 = (
    "169f652c7df910f7d4dd2201a3fa56452954e6a794ecc140cddb7c5bddf777ab"
)
QUALIFICATION_PACKET_SHA256 = {
    "B": "1348a652616d21ecc9f6b9cdd9f1465894c839a9a9510ee6adba10c1fde0c736",
    "C": "264e2a10baef828932b6787da49a374f4c967e0e761e50306c420402243fd01d",
}
MODEL = "claude-sonnet-4-6"
KEY_FILE_ENV = "SWE_BENCH_ANTHROPIC_KEY_FILE"
SUCCESS_STATUSES = {"matched", "created"}


class RunnerError(RuntimeError):
    """Fail-closed runner error whose messages never include secret values."""


def _read_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def _canonical(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _write_exclusive(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def _write_json(path: Path, value: Any) -> None:
    _write_exclusive(path, _canonical(value) + b"\n")


def _load_parser():
    if _sha256_file(PARSER_PATH) != (
        "68b44b5b39ff7fbf3e7417b4f16f0c37513a4cd7a96be8ba00611c825f462c2e"
    ):
        raise RunnerError("frozen edit parser digest mismatch")
    spec = importlib.util.spec_from_file_location("frozen_edit_patch_v2", PARSER_PATH)
    if spec is None or spec.loader is None:
        raise RunnerError("cannot load frozen edit parser")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _protocol() -> dict[str, Any]:
    return _read_json(PROTOCOL_PATH)


def registered_arm_order(instance_id: str) -> tuple[str, str]:
    population = _read_json(POPULATION_PATH)
    schedule = population.get("run_schedule", {}).get("episodes", [])
    matches = [
        row
        for row in schedule
        if row.get("instance_id") == instance_id
    ]
    if len(matches) != 1 or matches[0].get("arm_order") not in (["B", "C"], ["C", "B"]):
        raise RunnerError("instance has no unique frozen arm order")
    return tuple(matches[0]["arm_order"])


def format_prompt(row: Mapping[str, Any], packet: bytes) -> str:
    try:
        context = packet.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise RunnerError("packet is not strict UTF-8") from error
    template = _protocol()["instrument"]["prompt"]["template"]
    return template.format(
        repo=str(row["repo"]), issue=str(row["problem_statement"]), context=context
    )


def assemble_retry_prompt(base: str, previous: str, results: list[dict]) -> str:
    parser = _load_parser()
    suffix = _protocol()["instrument"]["retry"]["retry_suffix"]
    return base + suffix.format(
        prev=(previous or "")[-6000:], feedback=parser.failure_feedback(results)
    )


def _normalize_results(results: list[dict]) -> list[dict]:
    return [dict(result, success=result.get("status") in SUCCESS_STATUSES) for result in results]


def apply_response(
    checkout: Path, raw: str, state: dict[str, str] | None = None
) -> tuple[dict[str, str], list[dict]]:
    parser = _load_parser()
    edits = parser.parse_edits(raw)
    if not edits:
        return dict(state or {}), []
    next_state, results = parser.apply_edits_detailed(checkout, edits, state)
    return next_state, _normalize_results(results)


def prediction_from_state(
    instance_id: str, checkout: Path, state: dict[str, str]
) -> dict[str, str]:
    parser = _load_parser()
    patch = parser.make_diff(checkout, state)
    return {
        "instance_id": instance_id,
        "model_name_or_path": MODEL,
        "model_patch": patch,
    }


def _credential() -> str:
    raw_path = os.environ.get(KEY_FILE_ENV)
    if not raw_path:
        raise RunnerError("credential file environment is missing")
    path = Path(raw_path)
    try:
        metadata = path.lstat()
    except OSError as error:
        raise RunnerError("credential file is unavailable") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or path.is_symlink()
        or stat.S_IMODE(metadata.st_mode) != 0o600
    ):
        raise RunnerError("credential file must be regular, non-symlink, and mode 0600")
    value = path.read_text(encoding="utf-8", errors="strict").strip()
    if not value.startswith("sk-ant-") or len(value) < 80 or any(ch.isspace() for ch in value):
        raise RunnerError("credential shape is invalid")
    return value


def _usage(response: Any) -> dict[str, int | None]:
    usage = response.usage
    return {
        field: getattr(usage, field, None)
        for field in (
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        )
    }


def api_worker(request_path: Path, response_path: Path) -> None:
    if response_path.exists() or request_path.is_symlink() or not request_path.is_file():
        raise RunnerError("API worker paths are invalid")
    request = _read_json(request_path)
    required = {"model", "temperature", "max_tokens", "messages"}
    if not required.issubset(request) or set(request) - (required | {"stream"}):
        raise RunnerError("API request contract mismatch")
    request.setdefault("stream", False)
    if request != {
        "model": MODEL,
        "temperature": 0.0,
        "max_tokens": 8000,
        "messages": request.get("messages"),
        "stream": False,
    }:
        raise RunnerError("API request contract mismatch")
    messages = request["messages"]
    if (
        not isinstance(messages, list)
        or len(messages) != 1
        or messages[0].get("role") != "user"
        or not isinstance(messages[0].get("content"), str)
    ):
        raise RunnerError("API message contract mismatch")
    key = _credential()
    started = time.monotonic_ns()
    try:
        from anthropic import Anthropic

        response = Anthropic(api_key=key).messages.create(**request)
    finally:
        key = ""
    elapsed = (time.monotonic_ns() - started) // 1_000_000
    if response.model != MODEL:
        raise RunnerError("resolved model mismatch")
    text = "".join(
        block.text
        for block in response.content
        if getattr(block, "type", None) == "text"
    )
    _write_json(
        response_path,
        {
            "message_id": getattr(response, "id", None),
            "model": response.model,
            "stop_reason": response.stop_reason,
            "text": text,
            "usage": _usage(response),
            "latency_ms": elapsed,
        },
    )


def _git(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *args], cwd=cwd, check=False, capture_output=True, text=True
    )
    if result.returncode:
        raise RunnerError("git operation failed")
    return result.stdout.strip()


def _arm_checkout(source: Path, destination: Path, commit: str) -> None:
    if destination.exists():
        raise RunnerError("arm checkout already exists")
    _git("clone", "--quiet", "--shared", "--no-checkout", str(source), str(destination))
    _git("checkout", "--quiet", "--detach", commit, cwd=destination)
    if _git("status", "--porcelain", "--untracked-files=all", cwd=destination):
        raise RunnerError("arm checkout is not clean")


def _verify_packets(packet_root: Path, dataset_row: Path) -> dict[str, Any]:
    if _sha256_file(packet_root / "manifest.json") != QUALIFICATION_MANIFEST_SHA256:
        raise RunnerError("qualification packet manifest mismatch")
    for arm, digest in QUALIFICATION_PACKET_SHA256.items():
        if _sha256_file(packet_root / f"packet-{arm}.bin") != digest:
            raise RunnerError(f"qualification packet {arm} mismatch")
    manifest = PACKETS.verify_output(packet_root, dataset_row=dataset_row)
    if manifest.get("instance_id") != QUALIFICATION_INSTANCE:
        raise RunnerError("qualification instance mismatch")
    return manifest


def _invoke_worker(request: Path, response: Path) -> None:
    environment = os.environ.copy()
    environment.pop("ANTHROPIC_API_KEY", None)
    result = subprocess.run(
        [
            sys.executable,
            str(Path(__file__).resolve()),
            "_api-worker",
            "--request",
            str(request),
            "--response",
            str(response),
        ],
        env=environment,
        check=False,
        capture_output=True,
        timeout=600,
    )
    if result.returncode or result.stdout or result.stderr:
        raise RunnerError("isolated API worker failed or emitted output")


def _run_arm(
    arm: str,
    row: Mapping[str, Any],
    packet_root: Path,
    checkout_source: Path,
    run_root: Path,
) -> dict[str, Any]:
    arm_root = run_root / "arms" / arm
    arm_root.mkdir(parents=True, exist_ok=False)
    checkout = run_root / "work" / arm
    _arm_checkout(checkout_source, checkout, str(row["base_commit"]))
    prompt = format_prompt(row, (packet_root / f"packet-{arm}.bin").read_bytes())
    state: dict[str, str] = {}
    all_results: list[dict] = []
    raw = ""
    calls: list[dict[str, Any]] = []
    current_prompt = prompt
    for round_number in range(3):
        request_path = arm_root / f"request-{round_number:02d}.json"
        response_path = arm_root / f"response-{round_number:02d}.json"
        _write_json(
            request_path,
            {
                "model": MODEL,
                "temperature": 0.0,
                "max_tokens": 8000,
                "messages": [{"role": "user", "content": current_prompt}],
                "stream": False,
            },
        )
        _invoke_worker(request_path, response_path)
        response = _read_json(response_path)
        raw = str(response.get("text", ""))
        edits = _load_parser().parse_edits(raw)
        if edits:
            state, round_results = apply_response(checkout, raw, state)
        else:
            round_results = []
        successful = [result for result in all_results if result.get("success")]
        all_results = successful + round_results
        calls.append(
            {
                "round": round_number,
                "request_sha256": _sha256_file(request_path),
                "response_sha256": _sha256_file(response_path),
                "model": response.get("model"),
                "stop_reason": response.get("stop_reason"),
                "usage": response.get("usage"),
                "latency_ms": response.get("latency_ms"),
                "edit_results": round_results,
            }
        )
        failed = [result for result in round_results if not result.get("success")]
        if not edits or not failed:
            break
        if round_number == 2:
            break
        current_prompt = assemble_retry_prompt(prompt, raw, all_results)
    prediction = prediction_from_state(str(row["instance_id"]), checkout, state)
    prediction_path = arm_root / "prediction.json"
    _write_json(prediction_path, prediction)
    receipt = {
        "arm": arm,
        "instance_id": row["instance_id"],
        "packet_sha256": QUALIFICATION_PACKET_SHA256[arm],
        "prompt_sha256": _sha256(prompt.encode("utf-8")),
        "calls": calls,
        "prediction_sha256": _sha256_file(prediction_path),
        "model_patch_sha256": _sha256(prediction["model_patch"].encode("utf-8")),
        "model_patch_bytes": len(prediction["model_patch"].encode("utf-8")),
        "terminal": "patch" if prediction["model_patch"] else "empty_patch",
    }
    _write_json(arm_root / "receipt.json", receipt)
    return receipt


def build(args: argparse.Namespace) -> None:
    output = args.output.resolve()
    if output.exists() or not output.parent.is_dir():
        raise RunnerError("output must be a new path below an existing directory")
    row = _read_json(args.dataset_row)
    if row.get("instance_id") != QUALIFICATION_INSTANCE:
        raise RunnerError("build is limited to the registered qualification instance")
    packet_manifest = _verify_packets(args.packets, args.dataset_row)
    if _git("rev-parse", "HEAD", cwd=args.checkout) != row.get("base_commit"):
        raise RunnerError("base checkout commit mismatch")
    output.mkdir(mode=0o700)
    (output / "work").mkdir()
    order = registered_arm_order(str(row["instance_id"]))
    if order != ("C", "B"):
        raise RunnerError("qualification arm order drift")
    started = time.monotonic_ns()
    receipts = [
        _run_arm(arm, row, args.packets, args.checkout, output) for arm in order
    ]
    _write_json(
        output / "run.json",
        {
            "schema": "rna-swebench-direct-run-v1",
            "status": "built",
            "instance_id": row["instance_id"],
            "arm_order": list(order),
            "packet_manifest_sha256": QUALIFICATION_MANIFEST_SHA256,
            "packet_content_digest": packet_manifest["content_digest"],
            "runner_lock_sha256": _sha256_file(LOCK_PATH),
            "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
            "arms": receipts,
        },
    )


def evaluate(args: argparse.Namespace) -> None:
    run_root = args.run_root.resolve()
    run = _read_json(run_root / "run.json")
    row = _read_json(args.dataset_row)
    arms = tuple(run.get("arm_order", [])) if args.arm == "all" else (args.arm,)
    for arm in arms:
        arm_root = run_root / "arms" / arm
        prediction = _read_json(arm_root / "prediction.json")
        predictions = arm_root / "predictions.jsonl"
        _write_exclusive(predictions, _canonical(prediction) + b"\n")
        dataset = arm_root / "dataset.json"
        _write_json(dataset, [row])
        eval_root = arm_root / "evaluation"
        eval_root.mkdir()
        environment = os.environ.copy()
        environment.pop("ANTHROPIC_API_KEY", None)
        environment.pop(KEY_FILE_ENV, None)
        command = [
            sys.executable,
            "-m",
            "swebench.harness.run_evaluation",
            "--dataset_name",
            str(dataset),
            "--split",
            "test",
            "--instance_ids",
            str(row["instance_id"]),
            "--predictions_path",
            str(predictions),
            "--max_workers",
            "1",
            "--timeout",
            "1800",
            "--cache_level",
            "env",
            "--run_id",
            f"rna-qualification-{arm.lower()}",
            "--namespace",
            "swebench",
            "--report_dir",
            str(eval_root),
        ]
        started = time.monotonic_ns()
        result = subprocess.run(
            command,
            cwd=eval_root,
            env=environment,
            check=False,
            capture_output=True,
            timeout=3600,
        )
        _write_exclusive(eval_root / "stdout.log", result.stdout)
        _write_exclusive(eval_root / "stderr.log", result.stderr)
        _write_json(
            eval_root / "receipt.json",
            {
                "argv": command,
                "exit_code": result.returncode,
                "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
                "stdout_sha256": _sha256(result.stdout),
                "stderr_sha256": _sha256(result.stderr),
            },
        )
        if result.returncode:
            raise RunnerError(f"official evaluator failed for arm {arm}")


def verify(args: argparse.Namespace) -> None:
    root = args.run_root.resolve()
    run_path = root / "run.json"
    run = _read_json(run_path)
    if run.get("schema") != "rna-swebench-direct-run-v1" or run.get("arm_order") != ["C", "B"]:
        raise RunnerError("run manifest mismatch")
    if run.get("packet_manifest_sha256") != QUALIFICATION_MANIFEST_SHA256:
        raise RunnerError("run packet identity mismatch")
    for arm in ("C", "B"):
        arm_root = root / "arms" / arm
        receipt = _read_json(arm_root / "receipt.json")
        prediction = arm_root / "prediction.json"
        if (
            receipt.get("packet_sha256") != QUALIFICATION_PACKET_SHA256[arm]
            or receipt.get("prediction_sha256") != _sha256_file(prediction)
            or any(call.get("model") != MODEL for call in receipt.get("calls", []))
        ):
            raise RunnerError(f"arm {arm} evidence mismatch")
        for path in arm_root.rglob("*"):
            if path.is_symlink():
                raise RunnerError("evidence contains a symlink")
            if path.is_file():
                data = path.read_bytes()
                if b"sk-ant-" in data:
                    raise RunnerError("evidence contains credential-shaped bytes")
    print(json.dumps({"status": "ready", "run_sha256": _sha256_file(run_path)}))


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    worker = commands.add_parser("_api-worker", help=argparse.SUPPRESS)
    worker.add_argument("--request", type=Path, required=True)
    worker.add_argument("--response", type=Path, required=True)
    build_parser = commands.add_parser("build")
    build_parser.add_argument("--dataset-row", type=Path, required=True)
    build_parser.add_argument("--packets", type=Path, required=True)
    build_parser.add_argument("--checkout", type=Path, required=True)
    build_parser.add_argument("--output", type=Path, required=True)
    eval_parser = commands.add_parser("eval")
    eval_parser.add_argument("--run-root", type=Path, required=True)
    eval_parser.add_argument("--dataset-row", type=Path, required=True)
    eval_parser.add_argument("--arm", choices=("B", "C", "all"), default="all")
    verify_parser = commands.add_parser("verify")
    verify_parser.add_argument("--run-root", type=Path, required=True)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "_api-worker":
            api_worker(args.request, args.response)
        elif args.command == "build":
            build(args)
        elif args.command == "eval":
            evaluate(args)
        else:
            verify(args)
    except (RunnerError, PACKETS.PacketError, OSError, ValueError, subprocess.TimeoutExpired):
        print("runner failed closed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
