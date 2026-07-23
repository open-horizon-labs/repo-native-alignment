#!/usr/bin/env python3
"""Consume one #827 Bash request in either the offline or trusted-RNA plane."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import time
from typing import Mapping

from isolation import (
    BROKER_READY_SCHEMA,
    BROKER_OS_INJECTED_ENV_NAMES,
    BROKER_TRIGGER_SCHEMA,
    HashChainLedger,
    IsolationViolation,
    MAX_WORKER_REQUEST_BYTES,
    REQUEST_ID_RE,
    REQUEST_SCHEMA,
    TEARDOWN_SCHEMA,
    build_docker_worker_argv,
    canonical,
    is_secret_env_name,
    parse_strace_directory,
    sha256_bytes,
    sha256_file,
)


RECEIPT_SCHEMA = "issue827-bash-gateway-receipt-v1"


def atomic_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.parent.is_symlink():
        raise IsolationViolation("receipt_directory_is_link")
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        os.write(descriptor, canonical(value))
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.replace(temporary, path)


def exclusive_json(path: Path, value: object) -> None:
    """Persist a canonical single-use broker message without replacement."""

    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.parent.is_symlink():
        raise IsolationViolation("trusted_rna_broker_directory_is_link")
    if path.exists():
        raise IsolationViolation("trusted_rna_broker_message_already_exists")
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        os.write(descriptor, canonical(value))
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    if path.exists():
        temporary.unlink(missing_ok=True)
        raise IsolationViolation("trusted_rna_broker_message_already_exists")
    os.replace(temporary, path)


def _self_hash_valid(value: Mapping[str, object]) -> bool:
    observed = value.get("receipt_sha256")
    body = dict(value)
    body.pop("receipt_sha256", None)
    return isinstance(observed, str) and observed == sha256_bytes(canonical(body))


def _load_broker_ready(
    config: Mapping[str, object], config_sha256: str
) -> dict:
    path = Path(str(config["trusted_rna_broker_ready"]))
    if path.is_symlink() or not path.is_file():
        raise IsolationViolation("trusted_rna_broker_not_ready")
    try:
        value = json.loads(path.read_bytes())
    except json.JSONDecodeError as exc:
        raise IsolationViolation("trusted_rna_broker_ready_json_invalid") from exc
    broker = value.get("broker") if isinstance(value, dict) else None
    expected_environment = config.get("trusted_rna_env")
    injected_environment = (
        value.get("os_injected_environment") if isinstance(value, dict) else None
    )
    effective_environment = (
        {
            **expected_environment,
            **injected_environment,
        }
        if isinstance(expected_environment, dict)
        and isinstance(injected_environment, dict)
        else {}
    )
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != BROKER_READY_SCHEMA
        or value.get("config_sha256") != config_sha256
        or not isinstance(value.get("pid"), int)
        or int(value["pid"]) <= 0
        or value.get("provider_environment_inherited") is not False
        or value.get("credential_environment_names") != []
        or not isinstance(injected_environment, dict)
        or not set(injected_environment).issubset(
            BROKER_OS_INJECTED_ENV_NAMES
        )
        or any(
            not isinstance(item, str)
            or "\x00" in item
            or "\n" in item
            for item in injected_environment.values()
        )
        or value.get("canonical_environment_sha256")
        != sha256_bytes(canonical(expected_environment))
        or value.get("process_environment_sha256")
        != sha256_bytes(canonical(effective_environment))
        or value.get("environment_names")
        != sorted(effective_environment)
        or not isinstance(broker, dict)
        or broker.get("path") != config.get("trusted_rna_broker")
        or broker.get("sha256") != config.get("trusted_rna_broker_sha256")
        or not _self_hash_valid(value)
    ):
        raise IsolationViolation("trusted_rna_broker_ready_contract_invalid")
    return value


def _load_broker_output(
    value: object,
    *,
    directory: Path,
    request_id: str,
    suffix: str,
) -> bytes:
    expected = directory / f"{request_id}.{suffix}"
    if not isinstance(value, dict) or value.get("path") != str(expected):
        raise IsolationViolation("trusted_rna_broker_output_binding_mismatch")
    if expected.is_symlink() or not expected.is_file():
        raise IsolationViolation("trusted_rna_broker_output_missing")
    data = expected.read_bytes()
    if (
        value.get("bytes") != len(data)
        or value.get("sha256") != sha256_bytes(data)
    ):
        raise IsolationViolation("trusted_rna_broker_output_digest_mismatch")
    return data


def _wait_trusted_rna_broker(
    *,
    request: Mapping[str, object],
    request_path: Path,
    config: Mapping[str, object],
    config_sha256: str,
) -> tuple[int, dict[str, object], bytes, bytes]:
    """Submit one exact request and only wait for the broker-owned receipt.

    This path deliberately contains no ``subprocess`` invocation.  It executes
    below Claude's outer Seatbelt and therefore must never attempt a nested
    ``sandbox-exec``.
    """

    _load_broker_ready(config, config_sha256)
    request_id = str(request["request_id"])
    request_sha256 = sha256_file(request_path)
    trigger = {
        "schema_version": BROKER_TRIGGER_SCHEMA,
        "config_sha256": config_sha256,
        "request_id": request_id,
        "request_sha256": request_sha256,
        "claimed_request": str(request_path),
    }
    trigger_path = (
        Path(str(config["trusted_rna_broker_request_directory"]))
        / f"{request_id}.json"
    )
    exclusive_json(trigger_path, trigger)
    receipt_path = (
        Path(str(config["gateway_receipt_directory"])) / f"{request_id}.json"
    )
    deadline = time.monotonic() + float(
        config["trusted_rna_broker_client_timeout_seconds"]
    )
    while not receipt_path.exists():
        teardown_path = Path(str(config["trusted_rna_broker_teardown"]))
        if teardown_path.exists():
            raise IsolationViolation("trusted_rna_broker_exited_before_receipt")
        if time.monotonic() >= deadline:
            raise IsolationViolation("trusted_rna_broker_response_timeout")
        time.sleep(0.02)
    if receipt_path.is_symlink() or not receipt_path.is_file():
        raise IsolationViolation("trusted_rna_broker_receipt_invalid")
    try:
        receipt = json.loads(receipt_path.read_bytes())
    except json.JSONDecodeError as exc:
        raise IsolationViolation("trusted_rna_broker_receipt_json_invalid") from exc
    if (
        not isinstance(receipt, dict)
        or receipt.get("schema_version") != RECEIPT_SCHEMA
        or receipt.get("request_id") != request_id
        or receipt.get("request_sha256") != request_sha256
        or receipt.get("session_id") != request.get("session_id")
        or receipt.get("tool_use_id") != request.get("tool_use_id")
        or receipt.get("arm") != request.get("arm")
        or receipt.get("execution_plane") != "trusted_rna"
        or receipt.get("broker_owned") is not True
        or not _self_hash_valid(receipt)
    ):
        raise IsolationViolation("trusted_rna_broker_receipt_binding_mismatch")
    claimed_trigger = (
        Path(str(config["trusted_rna_broker_claimed_directory"]))
        / f"{request_id}.json"
    )
    if (
        claimed_trigger.is_symlink()
        or not claimed_trigger.is_file()
        or receipt.get("broker_trigger_sha256") != sha256_file(claimed_trigger)
        or trigger_path.exists()
    ):
        raise IsolationViolation("trusted_rna_broker_trigger_claim_mismatch")
    output_directory = Path(str(config["trusted_rna_broker_output_directory"]))
    stdout = _load_broker_output(
        receipt.get("stdout"),
        directory=output_directory,
        request_id=request_id,
        suffix="stdout",
    )
    stderr = _load_broker_output(
        receipt.get("stderr"),
        directory=output_directory,
        request_id=request_id,
        suffix="stderr",
    )
    if (
        receipt.get("stdout_bytes") != len(stdout)
        or receipt.get("stdout_sha256") != sha256_bytes(stdout)
        or receipt.get("stderr_bytes") != len(stderr)
        or receipt.get("stderr_sha256") != sha256_bytes(stderr)
    ):
        raise IsolationViolation("trusted_rna_broker_receipt_output_mismatch")
    return int(receipt.get("returncode", 85)), receipt, stdout, stderr


def load_config(path: Path, expected_sha256: str) -> dict:
    if (
        not path.is_absolute()
        or path.is_symlink()
        or not path.is_file()
        or sha256_file(path) != expected_sha256
    ):
        raise IsolationViolation("gateway_config_identity_mismatch")
    raw = path.read_bytes()
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise IsolationViolation("gateway_config_json_invalid") from exc
    if not isinstance(value, dict):
        raise IsolationViolation("gateway_config_not_object")
    return value


def claim_request(
    *,
    config: Mapping[str, object],
    request_id: str,
    expected_sha256: str,
) -> tuple[dict, Path]:
    if REQUEST_ID_RE.fullmatch(request_id) is None:
        raise IsolationViolation("request_id_invalid")
    request_directory = Path(str(config["gateway_request_directory"]))
    claimed_directory = Path(str(config["gateway_claimed_directory"]))
    if (
        not request_directory.is_absolute()
        or not claimed_directory.is_absolute()
        or request_directory.is_symlink()
        or claimed_directory.is_symlink()
    ):
        raise IsolationViolation("request_directory_invalid")
    claimed_directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(claimed_directory, 0o700)
    source = request_directory / f"{request_id}.json"
    try:
        metadata = os.lstat(source)
    except OSError as exc:
        raise IsolationViolation("request_missing_or_consumed") from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_uid != os.getuid()
    ):
        raise IsolationViolation("request_file_identity_invalid")
    raw = source.read_bytes()
    if sha256_bytes(raw) != expected_sha256:
        raise IsolationViolation("request_digest_mismatch")
    try:
        request = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise IsolationViolation("request_json_invalid") from exc
    if not isinstance(request, dict) or canonical(request) != raw:
        raise IsolationViolation("request_not_canonical")
    if (
        request.get("schema_version") != REQUEST_SCHEMA
        or request.get("request_id") != request_id
        or request.get("run_in_background") is not False
    ):
        raise IsolationViolation("request_contract_invalid")
    command = request.get("command")
    if (
        not isinstance(command, str)
        or not command
        or "\x00" in command
        or request.get("command_sha256")
        != sha256_bytes(command.encode("utf-8"))
    ):
        raise IsolationViolation("request_command_invalid")
    if not isinstance(request.get("tool_use_id"), str) or not request["tool_use_id"]:
        raise IsolationViolation("request_tool_use_id_missing")
    if not isinstance(request.get("session_id"), str) or not request["session_id"]:
        raise IsolationViolation("request_session_id_missing")
    destination = claimed_directory / f"{request_id}.json"
    try:
        os.replace(source, destination)
    except OSError as exc:
        raise IsolationViolation("request_atomic_claim_failed") from exc
    return request, destination


def _trusted_rna_argv(command: str, config: Mapping[str, object]) -> list[str]:
    if any(
        token in command
        for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n")
    ):
        raise IsolationViolation("trusted_rna_command_chained")
    try:
        argv = shlex.split(command)
    except ValueError as exc:
        raise IsolationViolation("trusted_rna_command_parse_failed") from exc
    if (
        len(argv) != 5
        or argv[0] != config.get("wrapper")
        or argv[1] != "--node"
        or argv[3] != "--mode"
        or argv[4] not in {"neighbors", "impact"}
        or not argv[2]
    ):
        raise IsolationViolation("trusted_rna_command_not_exact_wrapper")
    return argv


def _fixed_host_env(config: Mapping[str, object], key: str) -> dict[str, str]:
    value = config.get(key)
    if not isinstance(value, dict):
        raise IsolationViolation(f"{key}_invalid")
    permitted = {
        "PATH",
        "HOME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "TZ",
        "DOCKER_HOST",
        "DOCKER_CONFIG",
        "PYTHONDONTWRITEBYTECODE",
        "PYTHONNOUSERSITE",
    }
    result: dict[str, str] = {}
    canonical_trusted: dict[str, object] | None = None
    if key == "trusted_rna_env":
        environment_path = Path(str(config.get("trusted_rna_environment", "")))
        try:
            loaded = json.loads(environment_path.read_bytes())
        except (OSError, json.JSONDecodeError) as exc:
            raise IsolationViolation("trusted_rna_canonical_environment_invalid") from exc
        if not isinstance(loaded, dict):
            raise IsolationViolation("trusted_rna_canonical_environment_invalid")
        canonical_trusted = loaded
        if value != canonical_trusted:
            raise IsolationViolation("trusted_rna_environment_not_canonical")
    for name, raw in value.items():
        if (
            (
                name not in permitted
                if canonical_trusted is None
                else re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) is None
                or is_secret_env_name(name)
            )
            or not isinstance(raw, str)
            or "\x00" in raw
            or "\n" in raw
        ):
            raise IsolationViolation(f"{key}_entry_invalid", name=name)
        result[name] = raw
    if not result:
        raise IsolationViolation(f"{key}_empty")
    return result


def _run_trusted_rna(
    request: Mapping[str, object], config: Mapping[str, object]
) -> tuple[subprocess.CompletedProcess[bytes], dict[str, object]]:
    requested_argv = _trusted_rna_argv(str(request["command"]), config)
    gateway_python = config.get("gateway_python")
    gateway_python_sha256 = config.get("gateway_python_sha256")
    if (
        not isinstance(gateway_python, str)
        or not Path(gateway_python).is_absolute()
        or Path(gateway_python).resolve(strict=True)
        != Path(sys.executable).resolve(strict=True)
        or not isinstance(gateway_python_sha256, str)
        or sha256_file(Path(gateway_python)) != gateway_python_sha256
    ):
        raise IsolationViolation("trusted_rna_python_identity_mismatch")
    sandbox_exec = config.get("sandbox_exec")
    sandbox_exec_sha256 = config.get("sandbox_exec_sha256")
    if (
        sandbox_exec != "/usr/bin/sandbox-exec"
        or not Path(sandbox_exec).is_file()
        or Path(sandbox_exec).is_symlink()
        or not isinstance(sandbox_exec_sha256, str)
        or sha256_file(Path(sandbox_exec)) != sandbox_exec_sha256
    ):
        raise IsolationViolation("trusted_rna_sandbox_exec_identity_mismatch")
    profile_value = config.get("trusted_rna_seatbelt_profile")
    profile_sha256 = config.get("trusted_rna_seatbelt_profile_sha256")
    if not isinstance(profile_value, str):
        raise IsolationViolation("trusted_rna_seatbelt_profile_invalid")
    profile = Path(profile_value)
    try:
        profile_metadata = os.lstat(profile)
    except OSError as exc:
        raise IsolationViolation("trusted_rna_seatbelt_profile_invalid") from exc
    if (
        not profile.is_absolute()
        or not stat.S_ISREG(profile_metadata.st_mode)
        or stat.S_ISLNK(profile_metadata.st_mode)
        or stat.S_IMODE(profile_metadata.st_mode) & 0o222
        or not isinstance(profile_sha256, str)
        or sha256_file(profile) != profile_sha256
    ):
        raise IsolationViolation("trusted_rna_seatbelt_profile_identity_mismatch")
    read_roots = config.get("trusted_rna_read_roots")
    write_roots = config.get("trusted_rna_write_roots")
    if (
        not isinstance(read_roots, list)
        or not read_roots
        or not isinstance(write_roots, list)
        or not write_roots
        or any(
            not isinstance(item, str) or not Path(item).is_absolute()
            for item in [*read_roots, *write_roots]
        )
    ):
        raise IsolationViolation("trusted_rna_declared_roots_invalid")
    git_value = config.get("git_binary")
    git_sha256 = config.get("git_binary_sha256")
    if not isinstance(git_value, str) or not Path(git_value).is_absolute():
        raise IsolationViolation("trusted_rna_git_identity_mismatch")
    git_binary = Path(git_value)
    try:
        git_metadata = os.lstat(git_binary)
    except OSError as exc:
        raise IsolationViolation("trusted_rna_git_identity_mismatch") from exc
    if (
        not stat.S_ISREG(git_metadata.st_mode)
        or stat.S_ISLNK(git_metadata.st_mode)
        or not isinstance(git_sha256, str)
        or sha256_file(git_binary) != git_sha256
        or str(git_binary) not in read_roots
        or "/dev/null" not in read_roots
        or "/dev/null" not in write_roots
    ):
        raise IsolationViolation("trusted_rna_git_identity_mismatch")
    environment_value = config.get("trusted_rna_environment")
    environment_sha256 = config.get("trusted_rna_environment_sha256")
    if not isinstance(environment_value, str):
        raise IsolationViolation("trusted_rna_environment_invalid")
    environment = Path(environment_value)
    if (
        not environment.is_absolute()
        or not environment.is_file()
        or environment.is_symlink()
        or not isinstance(environment_sha256, str)
        or sha256_file(environment) != environment_sha256
        or str(environment) not in read_roots
    ):
        raise IsolationViolation("trusted_rna_environment_identity_mismatch")
    trusted_environment = _fixed_host_env(config, "trusted_rna_env")
    if (
        trusted_environment.get("GIT_CONFIG_GLOBAL") != "/dev/null"
        or trusted_environment.get("GIT_CONFIG_NOSYSTEM") != "1"
        or trusted_environment.get("GIT_TERMINAL_PROMPT") != "0"
    ):
        raise IsolationViolation("trusted_rna_git_environment_mismatch")
    # The trusted wrappers are pinned Python programs.  Invoke them through the
    # already-bound gateway interpreter instead of relying on /usr/bin/env and
    # the provider parent's PATH.  The exact inner Seatbelt profile removes the
    # provider parent's credentials and outbound network authority.
    argv = [
        sandbox_exec,
        "-f",
        str(profile),
        gateway_python,
        *requested_argv,
    ]
    started = time.monotonic_ns()
    result = subprocess.run(
        argv,
        cwd=str(config["checkout"]),
        env=trusted_environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=float(config["trusted_rna_timeout_seconds"]),
    )
    return result, {
        "execution_plane": "trusted_rna",
        "argv_sha256": sha256_bytes(canonical(argv)),
        "started_monotonic_ns": started,
        "finished_monotonic_ns": time.monotonic_ns(),
        "sandbox_exec": {
            "path": sandbox_exec,
            "sha256": sandbox_exec_sha256,
        },
        "seatbelt_profile": {
            "path": str(profile),
            "bytes": profile_metadata.st_size,
            "sha256": profile_sha256,
        },
        "canonical_environment": {
            "path": str(environment),
            "bytes": environment.stat().st_size,
            "sha256": environment_sha256,
        },
        "git_binary": {
            "path": str(git_binary),
            "bytes": git_metadata.st_size,
            "sha256": git_sha256,
        },
        "git_config_global_write_target": "/dev/null",
        "network_inbound": "denied",
        "network_outbound": "denied",
        "read_roots": list(read_roots),
        "write_roots": list(write_roots),
        "trace": None,
    }


def _docker_control(
    config: Mapping[str, object], args: list[str]
) -> tuple[subprocess.CompletedProcess[bytes], dict[str, object]]:
    argv = [str(config["docker_binary"]), *args]
    started = time.monotonic_ns()
    exception: str | None = None
    try:
        result = subprocess.run(
            argv,
            env=_fixed_host_env(config, "docker_host_env"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=float(config["docker_control_timeout_seconds"]),
        )
    except subprocess.TimeoutExpired as exc:
        exception = "TimeoutExpired"
        result = subprocess.CompletedProcess(
            argv, 124, exc.stdout or b"", exc.stderr or b""
        )
    except OSError as exc:
        exception = type(exc).__name__
        result = subprocess.CompletedProcess(argv, 125, b"", b"")
    evidence = {
        "argv_sha256": sha256_bytes(canonical(argv)),
        "returncode": result.returncode,
        "stdout_bytes": len(result.stdout),
        "stdout_sha256": sha256_bytes(result.stdout),
        "stderr_bytes": len(result.stderr),
        "stderr_sha256": sha256_bytes(result.stderr),
        "exception": exception,
        "elapsed_monotonic_ns": time.monotonic_ns() - started,
    }
    return result, evidence


def _classify_container_probe(
    result: subprocess.CompletedProcess[bytes],
    evidence: Mapping[str, object],
    container_name: str,
) -> str:
    """Classify only exact inspect outcomes; arbitrary nonzero is unknown."""

    if evidence.get("exception") is not None:
        return "unknown"
    exact_missing = (
        result.returncode == 1
        and result.stdout == b"[]\n"
        and result.stderr
        == f"Error response from daemon: No such container: {container_name}\n".encode()
    )
    if exact_missing:
        return "absent"
    if result.returncode != 0 or result.stderr:
        return "unknown"
    try:
        document = json.loads(result.stdout)
    except json.JSONDecodeError:
        return "unknown"
    if (
        isinstance(document, list)
        and len(document) == 1
        and isinstance(document[0], dict)
        and isinstance(document[0].get("Id"), str)
        and document[0]["Id"]
    ):
        return "present"
    return "unknown"


def _run_offline(
    *,
    request: Mapping[str, object],
    request_path: Path,
    config: Mapping[str, object],
) -> tuple[subprocess.CompletedProcess[bytes], dict[str, object]]:
    request_id = str(request["request_id"])
    request_bytes = canonical(dict(request))
    if (
        len(request_bytes) > MAX_WORKER_REQUEST_BYTES
        or request_path.is_symlink()
        or not request_path.is_file()
    ):
        raise IsolationViolation("worker_request_stdin_source_invalid")
    try:
        retained_request_bytes = request_path.read_bytes()
    except OSError as exc:
        raise IsolationViolation(
            "worker_request_stdin_source_unreadable",
            errno=exc.errno,
        ) from exc
    if retained_request_bytes != request_bytes:
        raise IsolationViolation("worker_request_stdin_binding_mismatch")
    trace_root = Path(str(config["gateway_trace_directory"]))
    trace_root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if trace_root.is_symlink():
        raise IsolationViolation("gateway_trace_directory_is_link")
    trace_directory = trace_root / request_id
    trace_directory.mkdir(mode=0o700)
    container_name = f"rna827-{request_id}"
    argv = build_docker_worker_argv(
        config=config,
        request_path=request_path,
        trace_directory=trace_directory,
        container_name=container_name,
    )
    started = time.monotonic_ns()
    timed_out = False
    primary_failure: str | None = None
    result = subprocess.CompletedProcess(argv, 125, b"", b"")
    try:
        result = subprocess.run(
            argv,
            input=request_bytes,
            env=_fixed_host_env(config, "docker_host_env"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=float(config["worker_timeout_seconds"]),
        )
    except subprocess.TimeoutExpired as exc:
        timed_out = True
        primary_failure = "worker_timeout"
        result = subprocess.CompletedProcess(
            argv,
            124,
            exc.stdout or b"",
            exc.stderr or b"",
        )
    except OSError as exc:
        primary_failure = f"worker_launch_{type(exc).__name__}"
    finally:
        cleanup, cleanup_evidence = _docker_control(
            config, ["rm", "-f", container_name]
        )
        inspect, inspect_evidence = _docker_control(
            config, ["container", "inspect", container_name]
        )
    container_state = _classify_container_probe(
        inspect, inspect_evidence, container_name
    )
    cleanup_verified = (
        cleanup_evidence.get("exception") is None
        and cleanup.returncode == 0
    )
    container_absent: bool | None = (
        True if container_state == "absent" else False if container_state == "present" else None
    )
    process_tree_retained: bool | None = (
        False if container_state == "absent" else True if container_state == "present" else None
    )
    teardown = {
        "schema_version": TEARDOWN_SCHEMA,
        "request_id": request_id,
        "container_name": container_name,
        "worker_returncode": result.returncode,
        "timed_out": timed_out,
        "primary_failure": primary_failure,
        "stdin_transport": "canonical_eof_pipe",
        "stdin_bytes": len(request_bytes),
        "stdin_sha256": sha256_bytes(request_bytes),
        "cleanup": cleanup_evidence,
        "inspect_after_cleanup": inspect_evidence,
        "cleanup_verified": cleanup_verified,
        "container_state": container_state,
        "container_absent": container_absent,
        "process_tree_retained": process_tree_retained,
        "finished_monotonic_ns": time.monotonic_ns(),
    }
    teardown["receipt_sha256"] = sha256_bytes(canonical(teardown))
    teardown_path = (
        Path(str(config["gateway_teardown_directory"])) / f"{request_id}.json"
    )
    atomic_json(teardown_path, teardown)
    if not cleanup_verified:
        raise IsolationViolation("docker_cleanup_unverified", teardown=teardown)
    if container_state == "present":
        raise IsolationViolation(
            "worker_process_tree_not_torn_down", teardown=teardown
        )
    if container_state != "absent":
        raise IsolationViolation("docker_teardown_unverified", teardown=teardown)
    if primary_failure is not None:
        raise IsolationViolation(primary_failure, teardown=teardown)
    trace_error: IsolationViolation | None = None
    trace: dict[str, object] | None = None
    try:
        trace = parse_strace_directory(
            trace_directory,
            allowed_path_prefixes=list(config["trace_allowed_path_prefixes"]),
            forbidden_path_fragments=list(
                config["trace_forbidden_path_fragments"]
            ),
        )
    except IsolationViolation as exc:
        trace_error = exc
    if trace_error is not None:
        raise IsolationViolation(
            trace_error.code,
            **trace_error.details,
            teardown=teardown,
        )
    assert trace is not None
    return result, {
        "execution_plane": "offline_bash",
        "argv_sha256": sha256_bytes(canonical(argv)),
        "stdin_transport": "canonical_eof_pipe",
        "stdin_bytes": len(request_bytes),
        "stdin_sha256": sha256_bytes(request_bytes),
        "started_monotonic_ns": started,
        "finished_monotonic_ns": time.monotonic_ns(),
        "trace": trace,
        "teardown": teardown,
    }


def execute(
    *,
    config: Mapping[str, object],
    request: Mapping[str, object],
    request_path: Path,
    trusted_rna_broker: bool = False,
) -> tuple[int, dict[str, object], bytes, bytes]:
    plane = request.get("execution_plane")
    if plane == "trusted_rna":
        if not trusted_rna_broker:
            raise IsolationViolation("trusted_rna_requires_external_broker")
        result, execution = _run_trusted_rna(request, config)
    elif plane == "offline_bash":
        result, execution = _run_offline(
            request=request, request_path=request_path, config=config
        )
    else:
        raise IsolationViolation("request_execution_plane_invalid")
    violations = (
        list(execution["trace"]["violations"])
        if isinstance(execution.get("trace"), dict)
        else []
    )
    status = (
        "success"
        if result.returncode == 0 and not violations
        else "nonadherent" if violations else "failed"
    )
    receipt = {
        "schema_version": RECEIPT_SCHEMA,
        "request_id": request["request_id"],
        "request_sha256": sha256_file(request_path),
        "session_id": request["session_id"],
        "tool_use_id": request["tool_use_id"],
        "arm": request["arm"],
        "execution_plane": plane,
        "original_command_sha256": request["command_sha256"],
        "status": status,
        "returncode": result.returncode,
        "stdout_bytes": len(result.stdout),
        "stdout_sha256": sha256_bytes(result.stdout),
        "stderr_bytes": len(result.stderr),
        "stderr_sha256": sha256_bytes(result.stderr),
        "execution": execution,
        "violations": violations,
    }
    receipt["receipt_sha256"] = sha256_bytes(canonical(receipt))
    exit_code = result.returncode
    if violations:
        exit_code = 86
    return exit_code, receipt, result.stdout, result.stderr


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--config-sha256", required=True)
    parser.add_argument("--request-id", required=True)
    parser.add_argument("--request-sha256", required=True)
    args = parser.parse_args()

    request: dict | None = None
    receipt_path: Path | None = None
    ledger: HashChainLedger | None = None
    try:
        config = load_config(args.config, args.config_sha256)
        ledger = HashChainLedger(Path(str(config["isolation_ledger"])))
        request, claimed = claim_request(
            config=config,
            request_id=args.request_id,
            expected_sha256=args.request_sha256,
        )
        receipt_path = (
            Path(str(config["gateway_receipt_directory"]))
            / f"{args.request_id}.json"
        )
        ledger.append(
            actor="bash_gateway",
            event_type="request_claimed",
            outcome="allow",
            arm=str(request["arm"]),
            tool_use_id=str(request["tool_use_id"]),
            payload={
                "request_id": args.request_id,
                "request_sha256": args.request_sha256,
                "execution_plane": request["execution_plane"],
            },
        )
        broker_owned = request["execution_plane"] == "trusted_rna"
        if broker_owned:
            exit_code, receipt, stdout, stderr = _wait_trusted_rna_broker(
                request=request,
                request_path=claimed,
                config=config,
                config_sha256=args.config_sha256,
            )
        else:
            exit_code, receipt, stdout, stderr = execute(
                config=config, request=request, request_path=claimed
            )
        teardown = (receipt.get("execution") or {}).get("teardown")
        if isinstance(teardown, dict):
            teardown_path = (
                Path(str(config["gateway_teardown_directory"]))
                / f"{args.request_id}.json"
            )
            if teardown_path.exists():
                if teardown_path.is_symlink() or json.loads(
                    teardown_path.read_bytes()
                ) != teardown:
                    raise IsolationViolation("teardown_persistence_mismatch")
            else:
                atomic_json(teardown_path, teardown)
            receipt["teardown"] = {
                "path": str(teardown_path),
                "bytes": len(teardown_path.read_bytes()),
                "sha256": sha256_file(teardown_path),
            }
            receipt.pop("receipt_sha256", None)
            receipt["receipt_sha256"] = sha256_bytes(canonical(receipt))
        if not broker_owned:
            atomic_json(receipt_path, receipt)
            ledger.append(
                actor="bash_gateway",
                event_type="request_completed",
                outcome=str(receipt["status"]),
                arm=str(request["arm"]),
                tool_use_id=str(request["tool_use_id"]),
                payload={
                    "request_id": args.request_id,
                    "receipt_sha256": receipt["receipt_sha256"],
                    "returncode": receipt["returncode"],
                },
                violation=(
                    {
                        "schema_version": "issue827-isolation-violation-v1",
                        "code": "traced_forbidden_attempt",
                        "fatal": True,
                        "details": {"violations": receipt["violations"]},
                    }
                    if receipt["violations"]
                    else None
                ),
            )
        sys.stdout.buffer.write(stdout)
        sys.stderr.buffer.write(stderr)
        return exit_code
    except (IsolationViolation, OSError, ValueError, subprocess.SubprocessError) as exc:
        violation = (
            exc
            if isinstance(exc, IsolationViolation)
            else IsolationViolation(
                "gateway_internal_failure", exception=type(exc).__name__
            )
        )
        if receipt_path is not None and not receipt_path.exists():
            teardown = violation.details.get("teardown")
            if isinstance(teardown, dict):
                teardown_path = (
                    Path(str(config["gateway_teardown_directory"]))
                    / f"{args.request_id}.json"
                )
                if teardown_path.exists():
                    if teardown_path.is_symlink() or json.loads(
                        teardown_path.read_bytes()
                    ) != teardown:
                        raise IsolationViolation("teardown_persistence_mismatch")
                else:
                    atomic_json(teardown_path, teardown)
            atomic_json(
                receipt_path,
                {
                    "schema_version": RECEIPT_SCHEMA,
                    "request_id": args.request_id,
                    "status": "violation",
                    "violation": violation.as_dict(),
                    "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
                },
            )
        if ledger is not None:
            ledger.append(
                actor="bash_gateway",
                event_type="gateway_failure",
                outcome="deny",
                arm=(
                    str(request.get("arm", "unknown"))
                    if isinstance(request, dict)
                    else "unknown"
                ),
                tool_use_id=(
                    str(request.get("tool_use_id"))
                    if isinstance(request, dict)
                    and request.get("tool_use_id")
                    else None
                ),
                payload={"request_id": args.request_id},
                violation=violation.as_dict(),
            )
        print(f"issue827 gateway denied: {violation.code}", file=sys.stderr)
        return 85


if __name__ == "__main__":
    raise SystemExit(main())
