#!/usr/bin/env python3
"""Treatment-only #827 RNA-first manipulation supervisor."""

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
    sha256_file,
)


HERE = Path(__file__).resolve().parent.parent


def terminal_denial(reason: str, event_name: str) -> dict[str, object]:
    message = f"RNA treatment terminated: {reason}"
    document: dict[str, object] = {
        "continue": False,
        "stopReason": message,
    }
    if event_name == "PreToolUse":
        document["hookSpecificOutput"] = {
            "hookEventName": event_name,
            "permissionDecision": "deny",
            "permissionDecisionReason": message,
        }
    elif event_name in {"PostToolUse", "PostToolUseFailure"}:
        document["decision"] = "block"
        document["reason"] = message
    return document


def load_config() -> dict:
    value = json.loads((HERE / "config/supervisor.json").read_text())
    if value.get("schema_version") != "issue827-supervisor-config-v4":
        raise IsolationViolation("supervisor_config_schema_mismatch")
    return value


def state(config: Mapping[str, object]) -> dict:
    path = Path(str(config["state"]))
    if path.exists():
        try:
            value = json.loads(path.read_bytes())
        except (OSError, json.JSONDecodeError) as exc:
            raise IsolationViolation("rna_supervisor_state_invalid") from exc
        if not isinstance(value, dict):
            raise IsolationViolation("rna_supervisor_state_not_object")
        return value
    return {
        "schema_version": "issue827-rna-supervisor-state-v1",
        "fatal": False,
        "first_traversal_succeeded": False,
        "model_tool_attempts": 0,
        "rna_calls": 0,
    }


def write_state(config: Mapping[str, object], value: Mapping[str, object]) -> None:
    path = Path(str(config["state"]))
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        try:
            previous = json.loads(path.read_bytes())
        except (OSError, json.JSONDecodeError):
            previous = {"fatal": True, "fatal_reason": "state_invalid"}
        if isinstance(previous, dict) and previous.get("fatal") is True:
            value = previous
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_bytes(canonical(dict(value)))
    temporary.replace(path)


def log(
    config: Mapping[str, object],
    event: Mapping[str, object],
    decision: str,
    reason: str | None = None,
    *,
    payload: Mapping[str, object] | None = None,
    violation: IsolationViolation | None = None,
) -> None:
    HashChainLedger(Path(str(config["hook_ledger"]))).append(
        actor="rna_first_supervisor",
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


def deny(
    event: Mapping[str, object],
    config: Mapping[str, object],
    current: dict,
    reason: str,
) -> int:
    if not current.get("fatal"):
        current["fatal"] = True
        current["fatal_reason"] = reason
    write_state(config, current)
    violation = IsolationViolation(reason)
    log(config, event, "deny", reason, violation=violation)
    print(
        json.dumps(
            terminal_denial(
                reason, str(event.get("hook_event_name", "unknown"))
            ),
            sort_keys=True,
        )
    )
    return 0


def is_exact_wrapper(
    command: str, config: Mapping[str, object], require_first: bool
) -> bool:
    if any(
        token in command
        for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n")
    ):
        return False
    try:
        argv = shlex.split(command)
    except ValueError:
        return False
    if len(argv) != 5 or argv[0] != config.get("wrapper"):
        return False
    if argv[1] != "--node" or not argv[2] or argv[3] != "--mode":
        return False
    if argv[4] not in ("neighbors", "impact"):
        return False
    return not require_first or argv[4] == "neighbors"


def _gateway_request_id(
    command: str, config: Mapping[str, object]
) -> tuple[str, str]:
    try:
        argv = shlex.split(command)
    except ValueError as exc:
        raise IsolationViolation("gateway_command_parse_failed") from exc
    prefix = [
        str(config["gateway_python"]),
        str(config["bash_gateway"]),
        "--config",
        str(config["gateway_config"]),
        "--config-sha256",
        sha256_file(Path(str(config["gateway_config"]))),
    ]
    if argv[: len(prefix)] != prefix:
        raise IsolationViolation("gateway_command_identity_mismatch")
    try:
        request_id = argv[argv.index("--request-id") + 1]
        request_sha = argv[argv.index("--request-sha256") + 1]
    except (ValueError, IndexError) as exc:
        raise IsolationViolation("gateway_command_request_missing") from exc
    return request_id, request_sha


def gateway_receipt(
    event: Mapping[str, object], config: Mapping[str, object]
) -> dict:
    tool_input = event.get("tool_input")
    command = tool_input.get("command") if isinstance(tool_input, dict) else None
    if not isinstance(command, str):
        raise IsolationViolation("gateway_post_command_missing")
    request_id, request_sha = _gateway_request_id(command, config)
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
        or receipt.get("tool_use_id") != event.get("tool_use_id")
    ):
        raise IsolationViolation("gateway_receipt_binding_mismatch")
    return receipt


def handle(event: dict, config: dict) -> int:
    current = state(config)
    name = event.get("hook_event_name")
    tool_name = event.get("tool_name")
    tool_input = event.get("tool_input")
    command = (
        tool_input.get("command", "")
        if tool_name == "Bash" and isinstance(tool_input, dict)
        else ""
    )

    if config["policy"] == "control":
        if name == "PreToolUse":
            current["model_tool_attempts"] = (
                int(current.get("model_tool_attempts", 0)) + 1
            )
            write_state(config, current)
        log(config, event, "observe_allow")
        return 0

    if name in {"PostToolUse", "PostToolUseFailure"} and tool_name == "Bash":
        try:
            receipt = gateway_receipt(event, config)
        except IsolationViolation as exc:
            return deny(event, config, current, exc.code)
        if (
            name == "PostToolUseFailure"
            or receipt.get("status") != "success"
        ):
            return deny(event, config, current, "rna_gateway_tool_failure")
        if receipt.get("execution_plane") == "trusted_rna":
            if not current.get("first_traversal_succeeded"):
                return deny(
                    event,
                    config,
                    current,
                    "rna_wrapper_success_without_verified_state",
                )
            log(
                config,
                event,
                "verified_rna_success",
                payload={
                    "request_id": receipt["request_id"],
                    "receipt_sha256": receipt["receipt_sha256"],
                },
            )
            return 0
        log(config, event, "verified_ordinary_bash")
        return 0

    if name != "PreToolUse":
        log(config, event, "observe")
        return 0

    current["model_tool_attempts"] = (
        int(current.get("model_tool_attempts", 0)) + 1
    )
    if current.get("fatal"):
        return deny(event, config, current, "tool_after_fatal_rna_error")
    if current["model_tool_attempts"] == 1:
        if (
            tool_name != "Bash"
            or not isinstance(command, str)
            or not is_exact_wrapper(command, config, require_first=True)
        ):
            return deny(
                event,
                config,
                current,
                "first_tool_not_exact_rna_neighbors_wrapper",
            )
    elif not current.get("first_traversal_succeeded"):
        return deny(
            event,
            config,
            current,
            "tool_before_verified_first_rna_response",
        )
    elif (
        tool_name == "Bash"
        and isinstance(command, str)
        and str(config["wrapper"]) in command
        and not is_exact_wrapper(command, config, require_first=False)
    ):
        return deny(
            event, config, current, "malformed_rna_wrapper_call"
        )

    write_state(config, current)
    log(config, event, "allow")
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
        lock = Path(str(config["lock"]))
        lock.parent.mkdir(parents=True, exist_ok=True)
        with lock.open("ab") as handle_lock:
            fcntl.flock(handle_lock, fcntl.LOCK_EX)
            return handle(event, config)
    except BaseException as exc:
        print(
            json.dumps(
                terminal_denial(
                    "RNA supervisor failed closed: "
                    f"{type(exc).__name__}",
                    event_name,
                ),
                sort_keys=True,
            )
        )
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
