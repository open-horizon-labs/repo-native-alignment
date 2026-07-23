#!/usr/bin/env python3
"""Claude hook enforcing the ordinary A/T confinement surface identically.

Treatment-specific first-action and RNA error rules remain in
``tool_supervisor.py``.  This hook is deliberately installed before it for
*both* arms, so the control cannot inspect harness or immutable-index paths
that the treatment cannot inspect.
"""

from __future__ import annotations

import datetime as dt
import fcntl
import json
from pathlib import Path
import shlex
import sys


HERE = Path(__file__).resolve().parent.parent


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def load_config() -> dict:
    return json.loads((HERE / "config/supervisor.json").read_text())


def inside(path_text: str, checkout: Path, cwd: str | None) -> bool:
    candidate = Path(path_text)
    if not candidate.is_absolute():
        candidate = Path(cwd or checkout) / candidate
    try:
        candidate.resolve(strict=False).relative_to(checkout.resolve(strict=True))
    except (OSError, ValueError):
        return False
    return True


def command_tokens(command: str) -> list[str]:
    try:
        return shlex.split(command)
    except ValueError:
        return []


def exact_treatment_wrapper(command: str, config: dict) -> bool:
    if config["policy"] != "treatment" or any(token in command for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n")):
        return False
    argv = command_tokens(command)
    return (
        len(argv) == 5
        and argv[0] == config["wrapper"]
        and argv[1] == "--node"
        and argv[3] == "--mode"
        and argv[4] in {"neighbors", "impact"}
    )


def forbidden_command_path(command: str, config: dict) -> str | None:
    """Reject direct references to pair-private and immutable harness paths.

    Exact traversal-wrapper use is intentionally allowed; the treatment hook
    validates its grammar and the control has no reason to know it.  We compare
    both tokenized and raw command text because shell redirections can otherwise
    hide a forbidden absolute path from token equality checks.
    """
    protected = {
        "immutable_index_checkout": config["repo"],
        "pinned_launcher": config["launcher"],
        "pinned_binary": config["binary"],
        "query_wrapper": config["query_wrapper"],
        "traversal_wrapper": config["wrapper"],
        "identity_receipt": config["identity_receipt"],
        "harness_root": config["harness_root"],
        "episode_evidence": config["episode_evidence_root"],
    }
    if exact_treatment_wrapper(command, config):
        return None
    tokens = command_tokens(command)
    for label, raw in protected.items():
        resolved = str(Path(raw).resolve(strict=False))
        if raw in command or resolved in command:
            return label
        if raw in tokens or resolved in tokens:
            return label
    return None


def append_ledger(path: Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("ab") as handle:
        handle.write(canonical(record))
        handle.flush()


def write_common_state(config: dict, reason: str) -> None:
    path = Path(config["common_state"])
    path.parent.mkdir(parents=True, exist_ok=True)
    value = {
        "schema_version": "issue825-common-supervisor-state-v1",
        "fatal": True,
        "fatal_reason": reason,
    }
    temporary = path.with_suffix(".tmp")
    temporary.write_bytes(canonical(value))
    temporary.replace(path)


def deny(reason: str) -> None:
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": f"common episode confinement: {reason}",
                }
            }
        )
    )


def handle(event: dict, config: dict) -> int:
    decision = "observe"
    reason: str | None = None
    if event.get("hook_event_name") == "PreToolUse":
        decision = "allow"
        tool = event.get("tool_name")
        tool_input = event.get("tool_input") or {}
        if tool in {"Read", "Edit", "Write"}:
            raw = tool_input.get("file_path")
            if not isinstance(raw, str) or not raw or not inside(raw, Path(config["checkout"]), event.get("cwd")):
                decision = "deny"
                reason = "repository_tool_path_outside_edit_checkout"
        elif tool == "Bash":
            command = tool_input.get("command")
            if not isinstance(command, str):
                decision = "deny"
                reason = "bash_command_not_text"
            else:
                forbidden = forbidden_command_path(command, config)
                if forbidden:
                    decision = "deny"
                    reason = f"direct_{forbidden}_access_forbidden"

    append_ledger(
        Path(config["common_hook_ledger"]),
        {
            "schema_version": "issue825-common-hook-event-v1",
            "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "arm": "A" if config["policy"] == "control" else "T",
            "event": event,
            "decision": decision,
            "reason": reason,
        },
    )
    if decision == "deny":
        write_common_state(config, reason or "denied")
        deny(reason or "denied")
    return 0


def main() -> int:
    event = json.load(sys.stdin)
    config = load_config()
    lock = Path(config["common_lock"])
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("ab") as handle_lock:
        fcntl.flock(handle_lock, fcntl.LOCK_EX)
        return handle(event, config)


if __name__ == "__main__":
    raise SystemExit(main())
