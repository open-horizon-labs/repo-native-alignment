#!/usr/bin/env python3
"""Symmetric #827 Claude hook for filesystem and Bash-plane confinement."""

from __future__ import annotations

import fcntl
import json
import os
from pathlib import Path
import shlex
import sys
from typing import Mapping

from isolation import (
    HashChainLedger,
    IsolationViolation,
    canonical,
    gateway_command,
    mint_request,
    sha256_bytes,
    sha256_file,
    validate_effective_path,
)


HERE = Path(__file__).resolve().parent.parent
SUPPORTED_TOOLS = {"Read", "Edit", "Write", "Glob", "Grep", "Bash"}


def load_config() -> dict:
    path = HERE / "config/supervisor.json"
    value = json.loads(path.read_text())
    if value.get("schema_version") != "issue827-supervisor-config-v4":
        raise IsolationViolation("supervisor_config_schema_mismatch")
    return value


def command_tokens(command: str) -> list[str]:
    try:
        return shlex.split(command)
    except ValueError:
        return []


def exact_treatment_wrapper(command: str, config: Mapping[str, object]) -> bool:
    if config.get("policy") != "treatment" or any(
        token in command
        for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n")
    ):
        return False
    argv = command_tokens(command)
    return (
        len(argv) == 5
        and argv[0] == config.get("wrapper")
        and argv[1] == "--node"
        and bool(argv[2])
        and argv[3] == "--mode"
        and argv[4] in {"neighbors", "impact"}
    )


def write_common_state(config: Mapping[str, object], reason: str) -> None:
    path = Path(str(config["common_state"]))
    path.parent.mkdir(parents=True, exist_ok=True)
    existing: dict = {}
    if path.exists():
        try:
            existing = json.loads(path.read_bytes())
        except (OSError, json.JSONDecodeError):
            existing = {}
    value = {
        "schema_version": "issue827-common-supervisor-state-v1",
        "fatal": True,
        "fatal_reason": existing.get("fatal_reason", reason),
    }
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_bytes(canonical(value))
    temporary.replace(path)


def common_fatal(config: Mapping[str, object]) -> bool:
    path = Path(str(config["common_state"]))
    if not path.exists():
        return False
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError):
        return True
    return value.get("fatal") is True


def hook_output(
    *,
    event_name: str,
    decision: str,
    reason: str,
    updated_input: Mapping[str, object] | None = None,
    terminate: bool = False,
) -> None:
    document: dict[str, object] = {}
    if event_name == "PreToolUse":
        output: dict[str, object] = {
            "hookEventName": event_name,
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
        if updated_input is not None:
            output["updatedInput"] = dict(updated_input)
        document["hookSpecificOutput"] = output
    elif event_name in {"PostToolUse", "PostToolUseFailure"}:
        if updated_input is not None:
            raise ValueError("updated input is only valid for PreToolUse")
        if decision == "deny":
            document["decision"] = "block"
            document["reason"] = reason
    if terminate:
        document["continue"] = False
        document["stopReason"] = reason
    print(json.dumps(document, sort_keys=True))


def _ledger(
    config: Mapping[str, object],
    event: Mapping[str, object],
    *,
    decision: str,
    reason: str | None,
    payload: Mapping[str, object] | None = None,
    violation: IsolationViolation | None = None,
) -> None:
    HashChainLedger(Path(str(config["common_hook_ledger"]))).append(
        actor="common_supervisor",
        event_type=str(event.get("hook_event_name", "unknown")),
        outcome=decision,
        arm="A" if config.get("policy") == "control" else "T",
        tool_use_id=(
            str(event["tool_use_id"]) if event.get("tool_use_id") else None
        ),
        payload=payload,
        violation=violation.as_dict() if violation is not None else None,
        extra_fields={
            "event": dict(event),
            "decision": decision,
            "reason": reason,
        },
    )


def _deny(
    event: Mapping[str, object],
    config: Mapping[str, object],
    violation: IsolationViolation,
) -> int:
    write_common_state(config, violation.code)
    _ledger(
        config,
        event,
        decision="deny",
        reason=violation.code,
        violation=violation,
    )
    hook_output(
        event_name=str(event.get("hook_event_name", "unknown")),
        decision="deny",
        reason=f"common episode confinement: {violation.code}",
        terminate=True,
    )
    return 0


def _gateway_updated_input(
    event: Mapping[str, object],
    config: Mapping[str, object],
    command: str,
) -> tuple[dict[str, object], dict[str, object]]:
    tool_input = event.get("tool_input")
    assert isinstance(tool_input, dict)
    if tool_input.get("run_in_background") is True:
        raise IsolationViolation("background_bash_forbidden")
    plane = (
        "trusted_rna"
        if exact_treatment_wrapper(command, config)
        else "offline_bash"
    )
    request, _request_path, request_sha = mint_request(
        config=config,
        event=event,
        execution_plane=plane,
        command=command,
    )
    command_config = dict(config)
    command_config["gateway_config_sha256"] = sha256_file(
        Path(str(config["gateway_config"]))
    )
    replacement = gateway_command(
        config=command_config,
        request_id=str(request["request_id"]),
        request_sha256=request_sha,
    )
    updated = dict(tool_input)
    updated["command"] = replacement
    updated["run_in_background"] = False
    updated["timeout"] = int(config["gateway_tool_timeout_ms"])
    return updated, {
        "request_id": request["request_id"],
        "request_sha256": request_sha,
        "execution_plane": plane,
        "original_command_sha256": request["command_sha256"],
        "replacement_command_sha256": sha256_bytes(
            replacement.encode("utf-8")
        ),
    }


def _receipt_for_gateway_command(
    command: str, config: Mapping[str, object]
) -> dict:
    argv = command_tokens(command)
    try:
        request_id = argv[argv.index("--request-id") + 1]
        request_sha = argv[argv.index("--request-sha256") + 1]
    except (ValueError, IndexError) as exc:
        raise IsolationViolation("gateway_post_command_malformed") from exc
    expected_prefix = [
        str(config["gateway_python"]),
        str(config["bash_gateway"]),
        "--config",
        str(config["gateway_config"]),
        "--config-sha256",
        sha256_file(Path(str(config["gateway_config"]))),
    ]
    if argv[: len(expected_prefix)] != expected_prefix:
        raise IsolationViolation("gateway_post_command_identity_mismatch")
    path = (
        Path(str(config["gateway_receipt_directory"]))
        / f"{request_id}.json"
    )
    if path.is_symlink() or not path.is_file():
        raise IsolationViolation("gateway_receipt_missing")
    try:
        receipt = json.loads(path.read_bytes())
    except json.JSONDecodeError as exc:
        raise IsolationViolation("gateway_receipt_invalid") from exc
    if (
        not isinstance(receipt, dict)
        or receipt.get("request_id") != request_id
        or receipt.get("request_sha256") != request_sha
    ):
        raise IsolationViolation("gateway_receipt_binding_mismatch")
    if receipt.get("status") != "success":
        raise IsolationViolation(
            "gateway_receipt_not_success", status=receipt.get("status")
        )
    return receipt


def _revoke_denied_request(
    event: Mapping[str, object], config: Mapping[str, object]
) -> int:
    tool_use_id = event.get("tool_use_id")
    revoked = 0
    request_dir = Path(str(config["gateway_request_directory"]))
    if request_dir.is_dir() and not request_dir.is_symlink():
        for path in request_dir.glob("*.json"):
            try:
                value = json.loads(path.read_bytes())
            except (OSError, json.JSONDecodeError):
                continue
            if value.get("tool_use_id") == tool_use_id:
                revoked_dir = Path(str(config["gateway_revoked_directory"]))
                revoked_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
                os.replace(path, revoked_dir / path.name)
                revoked += 1
    _ledger(
        config,
        event,
        decision="revoked",
        reason=None,
        payload={"revoked_requests": revoked},
    )
    return 0


def handle(event: dict, config: dict) -> int:
    name = event.get("hook_event_name")
    if name == "PermissionDenied":
        return _revoke_denied_request(event, config)
    if name not in {"PreToolUse", "PostToolUse", "PostToolUseFailure"}:
        _ledger(config, event, decision="observe", reason=None)
        return 0
    if common_fatal(config) and name == "PreToolUse":
        return _deny(
            event,
            config,
            IsolationViolation("tool_after_fatal_isolation_event"),
        )
    tool = event.get("tool_name")
    if name == "PreToolUse":
        try:
            if tool not in SUPPORTED_TOOLS:
                raise IsolationViolation(
                    "tool_outside_frozen_surface", tool=tool
                )
            tool_input = event.get("tool_input")
            if not isinstance(tool_input, dict):
                raise IsolationViolation("tool_input_not_object", tool=tool)
            if tool in {"Read", "Edit", "Write", "Glob", "Grep"}:
                validated = validate_effective_path(
                    tool_name=str(tool),
                    tool_input=tool_input,
                    cwd=event.get("cwd"),
                    read_roots=list(config["native_read_roots"]),
                    write_roots=list(config["native_write_roots"]),
                )
                _ledger(
                    config,
                    event,
                    decision="allow",
                    reason=None,
                    payload=validated,
                )
                return 0
            command = tool_input.get("command")
            if not isinstance(command, str) or not command or "\x00" in command:
                raise IsolationViolation("bash_command_not_text")
            updated, payload = _gateway_updated_input(event, config, command)
            _ledger(
                config,
                event,
                decision="replace_allow",
                reason=None,
                payload=payload,
            )
            hook_output(
                event_name="PreToolUse",
                decision="allow",
                reason="Bash replaced by single-use #827 gateway request",
                updated_input=updated,
            )
            return 0
        except (IsolationViolation, KeyError, OSError, ValueError) as exc:
            violation = (
                exc
                if isinstance(exc, IsolationViolation)
                else IsolationViolation(
                    "pretool_supervisor_failure",
                    exception=type(exc).__name__,
                )
            )
            return _deny(event, config, violation)

    if tool == "Bash":
        tool_input = event.get("tool_input")
        command = tool_input.get("command") if isinstance(tool_input, dict) else None
        try:
            if not isinstance(command, str):
                raise IsolationViolation("gateway_post_command_missing")
            receipt = _receipt_for_gateway_command(command, config)
            if name == "PostToolUseFailure":
                raise IsolationViolation("gateway_tool_reported_failure")
            _ledger(
                config,
                event,
                decision="verified",
                reason=None,
                payload={
                    "request_id": receipt["request_id"],
                    "receipt_sha256": receipt["receipt_sha256"],
                },
            )
            return 0
        except (IsolationViolation, KeyError, OSError, ValueError) as exc:
            violation = (
                exc
                if isinstance(exc, IsolationViolation)
                else IsolationViolation(
                    "posttool_supervisor_failure",
                    exception=type(exc).__name__,
                )
            )
            return _deny(event, config, violation)
    _ledger(config, event, decision="observe", reason=None)
    return 0


def main() -> int:
    event_name = "unknown"
    try:
        event = json.load(sys.stdin)
        if isinstance(event, dict) and isinstance(
            event.get("hook_event_name"), str
        ):
            event_name = event["hook_event_name"]
        config = load_config()
        lock = Path(str(config["common_lock"]))
        lock.parent.mkdir(parents=True, exist_ok=True)
        with lock.open("ab") as handle_lock:
            fcntl.flock(handle_lock, fcntl.LOCK_EX)
            return handle(event, config)
    except BaseException as exc:
        # A normal Python failure is denied in-band. Process death/timeout is
        # covered by the static allow-only gateway permission configuration.
        hook_output(
            event_name=event_name,
            decision="deny",
            reason=f"common supervisor failed closed: {type(exc).__name__}",
            terminate=True,
        )
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
