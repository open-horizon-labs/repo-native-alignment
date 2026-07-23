#!/usr/bin/env python3
"""Prestarted host broker for #827 trusted-RNA requests.

The exact Claude CLI runs below an outer macOS Seatbelt profile.  macOS rejects
attempts by that process tree to apply a second ``sandbox-exec`` profile.  This
broker is therefore started by the harness parent before Claude.  It accepts
only canonical, digest-bound requests from the single-use Bash gateway and is
the sole process allowed to launch the no-network, least-path trusted-RNA
Seatbelt plane.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time
from typing import Mapping

import bash_gateway
from isolation import (
    BROKER_READY_SCHEMA,
    BROKER_OS_INJECTED_ENV_NAMES,
    BROKER_STOP_SCHEMA,
    BROKER_TEARDOWN_SCHEMA,
    BROKER_TRIGGER_SCHEMA,
    HashChainLedger,
    IsolationViolation,
    REQUEST_ID_RE,
    REQUEST_SCHEMA,
    canonical,
    is_secret_env_name,
    sha256_bytes,
    sha256_file,
)


def _exclusive_bytes(path: Path, data: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.parent.is_symlink():
        raise IsolationViolation("trusted_rna_broker_directory_is_link")
    if path.exists():
        raise IsolationViolation("trusted_rna_broker_message_already_exists")
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        mode,
    )
    try:
        os.write(descriptor, data)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    if path.exists():
        temporary.unlink(missing_ok=True)
        raise IsolationViolation("trusted_rna_broker_message_already_exists")
    os.replace(temporary, path)


def _self_hashed(value: Mapping[str, object]) -> dict[str, object]:
    result = dict(value)
    result["receipt_sha256"] = sha256_bytes(canonical(result))
    return result


def _regular_private_file(path: Path, code: str) -> bytes:
    try:
        metadata = os.lstat(path)
    except OSError as exc:
        raise IsolationViolation(code) from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_uid != os.getuid()
    ):
        raise IsolationViolation(code)
    return path.read_bytes()


def _load_trigger(
    path: Path,
    *,
    config: Mapping[str, object],
    config_sha256: str,
) -> tuple[dict, dict, Path, str]:
    raw = _regular_private_file(path, "trusted_rna_broker_trigger_identity_invalid")
    try:
        trigger = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise IsolationViolation("trusted_rna_broker_trigger_json_invalid") from exc
    if not isinstance(trigger, dict) or canonical(trigger) != raw:
        raise IsolationViolation("trusted_rna_broker_trigger_not_canonical")
    request_id = trigger.get("request_id")
    request_sha256 = trigger.get("request_sha256")
    if (
        trigger.get("schema_version") != BROKER_TRIGGER_SCHEMA
        or trigger.get("config_sha256") != config_sha256
        or not isinstance(request_id, str)
        or REQUEST_ID_RE.fullmatch(request_id) is None
        or not isinstance(request_sha256, str)
        or len(request_sha256) != 64
    ):
        raise IsolationViolation("trusted_rna_broker_trigger_contract_invalid")
    expected_request = (
        Path(str(config["gateway_claimed_directory"])) / f"{request_id}.json"
    )
    if trigger.get("claimed_request") != str(expected_request):
        raise IsolationViolation("trusted_rna_broker_claimed_path_mismatch")
    request_raw = _regular_private_file(
        expected_request, "trusted_rna_broker_claimed_request_identity_invalid"
    )
    if sha256_bytes(request_raw) != request_sha256:
        raise IsolationViolation("trusted_rna_broker_claimed_request_digest_mismatch")
    try:
        request = json.loads(request_raw)
    except json.JSONDecodeError as exc:
        raise IsolationViolation("trusted_rna_broker_claimed_request_json_invalid") from exc
    if not isinstance(request, dict) or canonical(request) != request_raw:
        raise IsolationViolation("trusted_rna_broker_claimed_request_not_canonical")
    if (
        request.get("schema_version") != REQUEST_SCHEMA
        or request.get("request_id") != request_id
        or request.get("execution_plane") != "trusted_rna"
        or request.get("run_in_background") is not False
    ):
        raise IsolationViolation("trusted_rna_broker_request_contract_invalid")
    return trigger, request, expected_request, request_sha256


def _write_output(
    directory: Path, request_id: str, suffix: str, data: bytes
) -> dict[str, object]:
    path = directory / f"{request_id}.{suffix}"
    _exclusive_bytes(path, data)
    return {
        "path": str(path),
        "bytes": len(data),
        "sha256": sha256_bytes(data),
    }


def _write_failure(
    *,
    config: Mapping[str, object],
    request_id: str,
    request_sha256: str | None,
    request: Mapping[str, object] | None,
    violation: IsolationViolation,
) -> None:
    receipt_directory = Path(str(config["gateway_receipt_directory"]))
    receipt_path = receipt_directory / f"{request_id}.json"
    if receipt_path.exists():
        return
    receipt = _self_hashed(
        {
            "schema_version": bash_gateway.RECEIPT_SCHEMA,
            "request_id": request_id,
            "request_sha256": request_sha256,
            "session_id": request.get("session_id") if request else None,
            "tool_use_id": request.get("tool_use_id") if request else None,
            "arm": request.get("arm") if request else None,
            "execution_plane": "trusted_rna",
            "status": "violation",
            "returncode": 85,
            "stdout_bytes": 0,
            "stdout_sha256": sha256_bytes(b""),
            "stderr_bytes": 0,
            "stderr_sha256": sha256_bytes(b""),
            "execution": None,
            "violations": [violation.as_dict()],
            "violation": violation.as_dict(),
            "broker_owned": True,
        }
    )
    _exclusive_bytes(receipt_path, canonical(receipt))


def process_trigger(
    path: Path,
    *,
    config: Mapping[str, object],
    config_sha256: str,
) -> str:
    request_id = path.stem
    trigger: dict | None = None
    request: dict | None = None
    request_sha256: str | None = None
    claimed_path: Path | None = None
    claimed_trigger = (
        Path(str(config["trusted_rna_broker_claimed_directory"])) / path.name
    )
    try:
        if REQUEST_ID_RE.fullmatch(request_id) is None:
            raise IsolationViolation("trusted_rna_broker_trigger_name_invalid")
        trigger, request, claimed_path, request_sha256 = _load_trigger(
            path, config=config, config_sha256=config_sha256
        )
        claimed_trigger.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.replace(path, claimed_trigger)
        exit_code, receipt, stdout, stderr = bash_gateway.execute(
            config=config,
            request=request,
            request_path=claimed_path,
            trusted_rna_broker=True,
        )
        output_directory = Path(str(config["trusted_rna_broker_output_directory"]))
        stdout_ref = _write_output(output_directory, request_id, "stdout", stdout)
        stderr_ref = _write_output(output_directory, request_id, "stderr", stderr)
        receipt["broker_owned"] = True
        receipt["broker_trigger_sha256"] = sha256_file(claimed_trigger)
        receipt["stdout"] = stdout_ref
        receipt["stderr"] = stderr_ref
        receipt.pop("receipt_sha256", None)
        receipt["receipt_sha256"] = sha256_bytes(canonical(receipt))
        receipt_path = (
            Path(str(config["gateway_receipt_directory"])) / f"{request_id}.json"
        )
        _exclusive_bytes(receipt_path, canonical(receipt))
        HashChainLedger(Path(str(config["isolation_ledger"]))).append(
            actor="trusted_rna_broker",
            event_type="request_completed",
            outcome=str(receipt["status"]),
            arm=str(request["arm"]),
            tool_use_id=str(request["tool_use_id"]),
            payload={
                "request_id": request_id,
                "request_sha256": request_sha256,
                "receipt_sha256": receipt["receipt_sha256"],
                "returncode": receipt["returncode"],
            },
        )
        return "success" if exit_code == 0 else "request_failed"
    except (
        IsolationViolation,
        OSError,
        ValueError,
        subprocess.SubprocessError,
    ) as exc:
        violation = (
            exc
            if isinstance(exc, IsolationViolation)
            else IsolationViolation(
                "trusted_rna_broker_internal_failure",
                exception=type(exc).__name__,
            )
        )
        _write_failure(
            config=config,
            request_id=request_id,
            request_sha256=request_sha256,
            request=request,
            violation=violation,
        )
        HashChainLedger(Path(str(config["isolation_ledger"]))).append(
            actor="trusted_rna_broker",
            event_type="broker_failure",
            outcome="deny",
            arm=str(request.get("arm", "unknown")) if request else "unknown",
            tool_use_id=(
                str(request["tool_use_id"])
                if request and request.get("tool_use_id")
                else None
            ),
            payload={"request_id": request_id},
            violation=violation.as_dict(),
        )
        return violation.code


def _stop_requested(path: Path, config_sha256: str) -> bool:
    if not path.exists():
        return False
    raw = _regular_private_file(path, "trusted_rna_broker_stop_identity_invalid")
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise IsolationViolation("trusted_rna_broker_stop_json_invalid") from exc
    if (
        not isinstance(value, dict)
        or canonical(value) != raw
        or value
        != {
            "schema_version": BROKER_STOP_SCHEMA,
            "config_sha256": config_sha256,
        }
    ):
        raise IsolationViolation("trusted_rna_broker_stop_contract_invalid")
    return True


def serve(config: Mapping[str, object], config_sha256: str) -> int:
    broker_path = Path(str(config["trusted_rna_broker"]))
    if (
        broker_path.resolve(strict=True) != Path(__file__).resolve(strict=True)
        or sha256_file(broker_path) != config.get("trusted_rna_broker_sha256")
    ):
        raise IsolationViolation("trusted_rna_broker_identity_mismatch")
    expected_environment = config.get("trusted_rna_env")
    actual_environment = dict(os.environ)
    injected_environment = {
        name: actual_environment.pop(name)
        for name in sorted(BROKER_OS_INJECTED_ENV_NAMES)
        if name in actual_environment and name not in expected_environment
    } if isinstance(expected_environment, dict) else {}
    if (
        not isinstance(expected_environment, dict)
        or actual_environment != expected_environment
        or any(
            not isinstance(value, str)
            or "\x00" in value
            or "\n" in value
            for value in injected_environment.values()
        )
        or any(
            is_secret_env_name(name)
            for name in os.environ
        )
    ):
        raise IsolationViolation("trusted_rna_broker_environment_mismatch")
    ready_path = Path(str(config["trusted_rna_broker_ready"]))
    stop_path = Path(str(config["trusted_rna_broker_stop"]))
    teardown_path = Path(str(config["trusted_rna_broker_teardown"]))
    trigger_directory = Path(str(config["trusted_rna_broker_request_directory"]))
    trigger_directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    started = time.monotonic_ns()
    processed: list[dict[str, object]] = []
    fatal: IsolationViolation | None = None
    ready = _self_hashed(
        {
            "schema_version": BROKER_READY_SCHEMA,
            "config_sha256": config_sha256,
            "broker": {
                "path": str(broker_path),
                "sha256": sha256_file(broker_path),
            },
            "pid": os.getpid(),
            "started_monotonic_ns": started,
            "process_environment_sha256": sha256_bytes(canonical(dict(os.environ))),
            "canonical_environment_sha256": sha256_bytes(
                canonical(expected_environment)
            ),
            "environment_names": sorted(os.environ),
            "os_injected_environment": injected_environment,
            "credential_environment_names": sorted(
                name
                for name in os.environ
                if is_secret_env_name(name)
            ),
            "provider_environment_inherited": not all(
                not is_secret_env_name(name)
                for name in os.environ
            ),
        }
    )
    _exclusive_bytes(ready_path, canonical(ready))
    try:
        while True:
            if _stop_requested(stop_path, config_sha256):
                break
            for path in sorted(trigger_directory.glob("*.json")):
                outcome = process_trigger(
                    path, config=config, config_sha256=config_sha256
                )
                processed.append(
                    {
                        "request_id": path.stem,
                        "outcome": outcome,
                    }
                )
                if outcome not in {"success", "request_failed"}:
                    fatal = IsolationViolation(outcome)
                    break
            if fatal is not None:
                break
            time.sleep(0.02)
    except IsolationViolation as exc:
        fatal = exc
    pending = sorted(path.name for path in trigger_directory.glob("*.json"))
    teardown = _self_hashed(
        {
            "schema_version": BROKER_TEARDOWN_SCHEMA,
            "config_sha256": config_sha256,
            "pid": os.getpid(),
            "started_monotonic_ns": started,
            "finished_monotonic_ns": time.monotonic_ns(),
            "processed": processed,
            "pending": pending,
            "active_child": False,
            "fatal": fatal.as_dict() if fatal is not None else None,
            "clean": fatal is None and not pending,
        }
    )
    _exclusive_bytes(teardown_path, canonical(teardown))
    return 0 if teardown["clean"] else 85


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--config-sha256", required=True)
    args = parser.parse_args()
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    try:
        config = bash_gateway.load_config(args.config, args.config_sha256)
        return serve(config, args.config_sha256)
    except (IsolationViolation, OSError, ValueError) as exc:
        violation = (
            exc
            if isinstance(exc, IsolationViolation)
            else IsolationViolation(
                "trusted_rna_broker_startup_failure",
                exception=type(exc).__name__,
            )
        )
        print(
            f"issue827 trusted RNA broker denied: {violation.code}",
            file=sys.stderr,
        )
        return 85


if __name__ == "__main__":
    raise SystemExit(main())
