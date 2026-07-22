#!/usr/bin/env python3
"""Claude Code hook: require RNA first and freeze tools after fatal RNA errors."""

from __future__ import annotations

import datetime as dt
import json
from pathlib import Path
import shlex
import sys


HERE = Path(__file__).resolve().parent.parent
CONFIG = json.loads((HERE / "config/supervisor.json").read_text())


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def state() -> dict:
    path = Path(CONFIG["state"])
    if path.exists():
        return json.loads(path.read_text())
    return {"schema_version": "rna-supervisor-state-v1", "fatal": False, "first_traversal_succeeded": False, "model_tool_attempts": 0, "rna_calls": 0}


def write_state(value: dict) -> None:
    path = Path(CONFIG["state"])
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_bytes(canonical(value))
    tmp.replace(path)


def log(event: dict, decision: str, reason: str | None = None) -> None:
    path = Path(CONFIG["hook_ledger"])
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {
        "schema_version": "rna-hook-event-v1",
        "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "event": event,
        "decision": decision,
        "reason": reason,
    }
    with path.open("ab") as handle:
        handle.write(canonical(record))


def deny(event: dict, current: dict, reason: str) -> int:
    if not current.get("fatal"):
        current["fatal"] = True
        current["fatal_reason"] = reason
    write_state(current)
    log(event, "deny", reason)
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": f"RNA treatment terminated: {reason}"}}))
    return 0


def is_exact_wrapper(command: str, require_first: bool) -> bool:
    if any(token in command for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n")):
        return False
    try:
        argv = shlex.split(command)
    except ValueError:
        return False
    if len(argv) != 5 or argv[0] != CONFIG["wrapper"]:
        return False
    if argv[1] != "--node" or argv[3] != "--mode":
        return False
    if argv[4] not in ("neighbors", "impact"):
        return False
    return not require_first or argv[4] == "neighbors"


def path_inside_checkout(event: dict) -> bool:
    raw = event.get("tool_input", {}).get("file_path")
    if not isinstance(raw, str) or not raw:
        return False
    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = Path(event.get("cwd", CONFIG["checkout"])) / candidate
    try:
        candidate.resolve(strict=False).relative_to(Path(CONFIG["checkout"]).resolve(strict=True))
    except (OSError, ValueError):
        return False
    return True


def handle(event: dict) -> int:
    current = state()
    name = event.get("hook_event_name")
    tool_name = event.get("tool_name")
    command = event.get("tool_input", {}).get("command", "") if tool_name == "Bash" else ""
    if CONFIG["policy"] == "control":
        if name == "PreToolUse":
            current["model_tool_attempts"] = int(current.get("model_tool_attempts", 0)) + 1
            write_state(current)
        log(event, "observe_allow")
        return 0
    if name == "PostToolUseFailure" and CONFIG["wrapper"] in command:
        if not current.get("fatal"):
            current["fatal"] = True
            current["fatal_reason"] = "rna_wrapper_tool_failure"
            write_state(current)
        log(event, "freeze", current.get("fatal_reason"))
        return 0
    if name == "PostToolUse" and CONFIG["wrapper"] in command and not current.get("first_traversal_succeeded"):
        current["fatal"] = True
        current["fatal_reason"] = "rna_wrapper_success_without_verified_state"
        write_state(current)
        log(event, "freeze", current["fatal_reason"])
        return 0
    if name != "PreToolUse":
        log(event, "observe")
        return 0

    current["model_tool_attempts"] = int(current.get("model_tool_attempts", 0)) + 1
    if current.get("fatal"):
        return deny(event, current, "tool_after_fatal_rna_error")

    if current["model_tool_attempts"] == 1:
        if tool_name != "Bash" or not is_exact_wrapper(command, require_first=True):
            return deny(event, current, "first_tool_not_exact_rna_neighbors_wrapper")
    elif not current.get("first_traversal_succeeded"):
        return deny(event, current, "tool_before_verified_first_rna_response")
    elif tool_name in ("Read", "Edit", "Write") and not path_inside_checkout(event):
        return deny(event, current, "repository_tool_path_outside_edit_checkout")
    elif CONFIG["repo"] in command:
        return deny(event, current, "immutable_index_checkout_access_forbidden")
    elif CONFIG["launcher"] in command:
        return deny(event, current, "direct_launcher_bypass")
    elif CONFIG["query_wrapper"] in command:
        return deny(event, current, "harness_query_wrapper_is_not_a_model_tool")
    elif CONFIG["wrapper"] in command and not is_exact_wrapper(command, require_first=False):
        return deny(event, current, "malformed_rna_wrapper_call")

    write_state(current)
    log(event, "allow")
    return 0


def main() -> int:
    import fcntl

    event = json.load(sys.stdin)
    lock = Path(CONFIG["lock"])
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("ab") as handle_lock:
        fcntl.flock(handle_lock, fcntl.LOCK_EX)
        return handle(event)


if __name__ == "__main__":
    raise SystemExit(main())
