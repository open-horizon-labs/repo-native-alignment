#!/usr/bin/env python3
"""Fail-stop wrapper for the registered #827 Claude hook supervisors.

Claude applies its own five-second command-hook timeout.  This wrapper gives
the registered child supervisor a smaller internal deadline, validates the
child's complete response, and turns every crash, timeout, malformed response,
or explicit denial into a sticky ``continue: false`` episode termination.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time
from typing import Any, Mapping, Sequence


CONFIG_SCHEMA = "issue827-supervisor-config-v4"
STATE_SCHEMA = "issue827-hook-guard-state-v1"
LEDGER_SCHEMA = "issue827-hook-guard-ledger-v1"
NATIVE_TOOL_STATE_SCHEMA = "issue827-native-tool-state-v1"
MAX_HOOK_INPUT_BYTES = 2 * 1024 * 1024
MAX_CHILD_OUTPUT_BYTES = 10_000
MAX_INTERNAL_TIMEOUT_MS = 4_000
NATIVE_READ_TOOLS = frozenset({"Read", "Glob", "Grep"})
NATIVE_EXCLUSIVE_TOOLS = frozenset({"Bash", "Edit", "Write"})
NATIVE_TOOLS = NATIVE_READ_TOOLS | NATIVE_EXCLUSIVE_TOOLS
NATIVE_COMPLETION_EVENTS = frozenset(
    {"PostToolUse", "PostToolUseFailure", "PermissionDenied"}
)
ROLE_FILES = {
    "common": ("common_supervisor.py", "common_supervisor_sha256"),
    "treatment": ("tool_supervisor.py", "tool_supervisor_sha256"),
}


class GuardFailure(RuntimeError):
    """A hook response could not be trusted."""

    def __init__(self, code: str, **details: object):
        super().__init__(code)
        self.code = code
        self.details = details


class RaisingArgumentParser(argparse.ArgumentParser):
    """Keep parser failures inside the structured fail-stop response."""

    def error(self, message: str) -> None:
        raise GuardFailure("hook_guard_arguments_invalid")


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def canonical(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def stop_document(reason: str, event_name: str) -> dict[str, object]:
    message = f"#827 hook guard terminated the episode: {reason}"
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


def emit_stop(reason: str, event_name: str) -> None:
    sys.stdout.buffer.write(canonical(stop_document(reason, event_name)))
    sys.stdout.buffer.flush()


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = RaisingArgumentParser(add_help=False, allow_abbrev=False)
    parser.add_argument("--config", required=True)
    parser.add_argument("--evidence-root", required=True)
    parser.add_argument("--child", required=True)
    parser.add_argument("--child-sha256", required=True)
    parser.add_argument("--role", choices=sorted(ROLE_FILES), required=True)
    parser.add_argument("--timeout-ms", required=True, type=int)
    values = parser.parse_args(argv)
    if not (1 <= values.timeout_ms <= MAX_INTERNAL_TIMEOUT_MS):
        raise GuardFailure("hook_guard_timeout_out_of_range")
    if (
        len(values.child_sha256) != 64
        or any(character not in "0123456789abcdef" for character in values.child_sha256)
    ):
        raise GuardFailure("hook_guard_child_digest_invalid")
    return values


def exact_regular_file(path: Path, label: str) -> Path:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise GuardFailure(f"{label}_missing") from exc
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise GuardFailure(f"{label}_not_regular")
    return path.resolve(strict=True)


def validated_runtime(
    values: argparse.Namespace,
) -> tuple[dict[str, Any], Path, Path, Path]:
    config_path = exact_regular_file(Path(values.config), "hook_guard_config")
    evidence_root = Path(values.evidence_root)
    try:
        evidence_metadata = evidence_root.lstat()
    except OSError as exc:
        raise GuardFailure("hook_guard_evidence_root_missing") from exc
    if (
        stat.S_ISLNK(evidence_metadata.st_mode)
        or not stat.S_ISDIR(evidence_metadata.st_mode)
    ):
        raise GuardFailure("hook_guard_evidence_root_invalid")
    evidence_root = evidence_root.resolve(strict=True)
    try:
        config = json.loads(config_path.read_bytes())
    except (OSError, json.JSONDecodeError) as exc:
        raise GuardFailure("hook_guard_config_invalid") from exc
    if not isinstance(config, dict) or config.get("schema_version") != CONFIG_SCHEMA:
        raise GuardFailure("hook_guard_config_schema_mismatch")
    try:
        configured_evidence = Path(str(config["episode_evidence_root"])).resolve(
            strict=True
        )
    except (KeyError, OSError) as exc:
        raise GuardFailure("hook_guard_config_evidence_invalid") from exc
    if configured_evidence != evidence_root:
        raise GuardFailure("hook_guard_evidence_binding_mismatch")

    expected_name, digest_field = ROLE_FILES[values.role]
    child = exact_regular_file(Path(values.child), "hook_guard_child")
    expected_child = (
        Path(str(config.get("harness_root", ""))) / "bin" / expected_name
    )
    try:
        expected_child = expected_child.resolve(strict=True)
    except OSError as exc:
        raise GuardFailure("hook_guard_expected_child_missing") from exc
    if child != expected_child:
        raise GuardFailure("hook_guard_child_path_mismatch")
    observed_digest = sha256_file(child)
    if (
        observed_digest != values.child_sha256
        or config.get(digest_field) != values.child_sha256
    ):
        raise GuardFailure("hook_guard_child_digest_mismatch")
    return config, config_path, evidence_root, child


def guard_paths(evidence_root: Path) -> tuple[Path, Path, Path]:
    hooks = evidence_root / "hooks"
    if hooks.exists() and (hooks.is_symlink() or not hooks.is_dir()):
        raise GuardFailure("hook_guard_evidence_directory_invalid")
    hooks.mkdir(mode=0o700, parents=True, exist_ok=True)
    return (
        evidence_root / "hook-guard-state.json",
        hooks / "hook-guard-events.jsonl",
        hooks / "hook-guard.lock",
    )


def _safe_existing_regular(path: Path, code: str) -> None:
    if path.exists() and (path.is_symlink() or not path.is_file()):
        raise GuardFailure(code)


def _read_state_unlocked(path: Path) -> dict[str, Any] | None:
    _safe_existing_regular(path, "hook_guard_state_invalid")
    if not path.exists():
        return None
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as exc:
        raise GuardFailure("hook_guard_state_invalid") from exc
    if not isinstance(value, dict) or value.get("schema_version") != STATE_SCHEMA:
        raise GuardFailure("hook_guard_state_schema_mismatch")
    if value.get("fatal") is not True:
        raise GuardFailure("hook_guard_state_not_fatal")
    return value


def _atomic_write(path: Path, value: Mapping[str, object]) -> None:
    _safe_existing_regular(path, "hook_guard_state_target_invalid")
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_bytes(canonical(dict(value)))
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _native_tool_state_path(evidence_root: Path) -> Path:
    return evidence_root / "hooks" / "native-tool-state.json"


def _read_native_tool_state_unlocked(path: Path) -> dict[str, Any]:
    _safe_existing_regular(path, "native_tool_state_invalid")
    if not path.exists():
        return {
            "schema_version": NATIVE_TOOL_STATE_SCHEMA,
            "active": {},
        }
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as exc:
        raise GuardFailure("native_tool_state_invalid") from exc
    if (
        not isinstance(value, dict)
        or set(value) != {"schema_version", "active"}
        or value.get("schema_version") != NATIVE_TOOL_STATE_SCHEMA
        or not isinstance(value.get("active"), dict)
    ):
        raise GuardFailure("native_tool_state_schema_mismatch")
    for tool_use_id, item in value["active"].items():
        if (
            not isinstance(tool_use_id, str)
            or not tool_use_id
            or not isinstance(item, dict)
            or set(item) != {"tool_name", "access", "post_pending"}
            or item.get("tool_name") not in NATIVE_TOOLS
            or item.get("access")
            != (
                "read"
                if item.get("tool_name") in NATIVE_READ_TOOLS
                else "exclusive"
            )
            or type(item.get("post_pending")) is not bool
        ):
            raise GuardFailure("native_tool_state_entry_invalid")
    return value


def _write_native_tool_state_unlocked(
    path: Path, value: Mapping[str, object]
) -> None:
    _safe_existing_regular(path, "native_tool_state_target_invalid")
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_bytes(canonical(dict(value)))
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def begin_native_tool_transition(
    *,
    evidence_root: Path,
    role: str,
    event: Mapping[str, object],
) -> bool:
    """Reserve a native tool before its child hook or begin its completion.

    Only the common hook owns this state.  Bash/Edit/Write reservations are
    exclusive against every native tool; Read/Glob/Grep may coexist.  A
    completion remains reserved until the child hook validates successfully.
    """

    if role != "common":
        return False
    event_name = event.get("hook_event_name")
    tool_name = event.get("tool_name")
    if (
        event_name not in {"PreToolUse", *NATIVE_COMPLETION_EVENTS}
        or tool_name not in NATIVE_TOOLS
    ):
        return False
    tool_use_id = event.get("tool_use_id")
    if not isinstance(tool_use_id, str) or not tool_use_id:
        raise GuardFailure("native_tool_use_id_invalid")

    state_path = _native_tool_state_path(evidence_root)
    _state_path, _ledger, lock = guard_paths(evidence_root)
    with lock.open("ab") as lock_handle:
        fcntl.flock(lock_handle, fcntl.LOCK_EX)
        state = _read_native_tool_state_unlocked(state_path)
        active = state["active"]
        assert isinstance(active, dict)
        if event_name == "PreToolUse":
            if tool_use_id in active:
                raise GuardFailure("native_tool_duplicate_pre")
            if any(
                isinstance(item, dict) and item.get("post_pending") is True
                for item in active.values()
            ):
                raise GuardFailure("native_tool_post_unresolved")
            access = (
                "read" if tool_name in NATIVE_READ_TOOLS else "exclusive"
            )
            if active and (
                access == "exclusive"
                or any(
                    isinstance(item, dict)
                    and item.get("access") == "exclusive"
                    for item in active.values()
                )
            ):
                raise GuardFailure("native_tool_rw_overlap")
            active[tool_use_id] = {
                "tool_name": tool_name,
                "access": access,
                "post_pending": False,
            }
        else:
            item = active.get(tool_use_id)
            if (
                not isinstance(item, dict)
                or item.get("tool_name") != tool_name
            ):
                raise GuardFailure("native_tool_post_without_matching_pre")
            if item.get("post_pending") is True:
                raise GuardFailure("native_tool_duplicate_post")
            item["post_pending"] = True
        _write_native_tool_state_unlocked(state_path, state)
    return event_name in NATIVE_COMPLETION_EVENTS


def finish_native_tool_transition(
    *,
    evidence_root: Path,
    role: str,
    event: Mapping[str, object],
) -> None:
    if (
        role != "common"
        or event.get("hook_event_name") not in NATIVE_COMPLETION_EVENTS
        or event.get("tool_name") not in NATIVE_TOOLS
    ):
        return
    tool_use_id = event.get("tool_use_id")
    state_path = _native_tool_state_path(evidence_root)
    _state_path, _ledger, lock = guard_paths(evidence_root)
    with lock.open("ab") as lock_handle:
        fcntl.flock(lock_handle, fcntl.LOCK_EX)
        state = _read_native_tool_state_unlocked(state_path)
        active = state["active"]
        assert isinstance(active, dict)
        item = active.get(tool_use_id)
        if (
            not isinstance(tool_use_id, str)
            or not isinstance(item, dict)
            or item.get("tool_name") != event.get("tool_name")
            or item.get("post_pending") is not True
        ):
            raise GuardFailure("native_tool_post_finish_mismatch")
        del active[tool_use_id]
        _write_native_tool_state_unlocked(state_path, state)


def _previous_record_sha256(ledger: Path) -> str | None:
    _safe_existing_regular(ledger, "hook_guard_ledger_invalid")
    if not ledger.exists():
        return None
    try:
        lines = ledger.read_bytes().splitlines()
        if not lines:
            return None
        previous = json.loads(lines[-1])
    except (OSError, json.JSONDecodeError) as exc:
        raise GuardFailure("hook_guard_ledger_invalid") from exc
    value = previous.get("record_sha256") if isinstance(previous, dict) else None
    if not isinstance(value, str) or len(value) != 64:
        raise GuardFailure("hook_guard_ledger_chain_invalid")
    return value


def _append_record_unlocked(ledger: Path, record: Mapping[str, object]) -> None:
    previous_sha = _previous_record_sha256(ledger)
    body = {
        "schema_version": LEDGER_SCHEMA,
        "previous_record_sha256": previous_sha,
        **dict(record),
    }
    body["record_sha256"] = sha256_bytes(canonical(body))
    flags = os.O_APPEND | os.O_CREAT | os.O_WRONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(ledger, flags, 0o600)
    try:
        os.write(descriptor, canonical(body))
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _common_state_path(
    config: Mapping[str, object], evidence_root: Path
) -> Path | None:
    value = config.get("common_state")
    if not isinstance(value, str):
        return None
    candidate = Path(value)
    try:
        parent = candidate.parent.resolve(strict=True)
    except OSError:
        return None
    if parent != evidence_root or candidate.name != "common-supervisor-state.json":
        return None
    return candidate


def record(
    *,
    config: Mapping[str, object],
    evidence_root: Path,
    role: str,
    event: Mapping[str, object],
    outcome: str,
    reason: str | None,
    child: Path | None,
    child_sha256: str | None,
    child_returncode: int | None,
    child_stdout: bytes,
    child_stderr: bytes,
    elapsed_ms: int,
    fatal: bool,
) -> None:
    state_path, ledger, lock = guard_paths(evidence_root)
    with lock.open("ab") as lock_handle:
        fcntl.flock(lock_handle, fcntl.LOCK_EX)
        existing = _read_state_unlocked(state_path)
        if fatal and existing is None:
            state = {
                "schema_version": STATE_SCHEMA,
                "fatal": True,
                "fatal_reason": reason,
                "role": role,
                "event_name": event.get("hook_event_name"),
                "tool_name": event.get("tool_name"),
                "created_at": utc_now(),
            }
            _atomic_write(state_path, state)
            common_state = _common_state_path(config, evidence_root)
            if common_state is not None:
                current_reason = reason
                if common_state.exists() and not common_state.is_symlink():
                    try:
                        current = json.loads(common_state.read_bytes())
                        if isinstance(current, dict) and current.get("fatal") is True:
                            current_reason = current.get("fatal_reason", reason)
                    except (OSError, json.JSONDecodeError):
                        current_reason = "common_state_invalid"
                _atomic_write(
                    common_state,
                    {
                        "schema_version": "issue827-common-supervisor-state-v1",
                        "fatal": True,
                        "fatal_reason": current_reason,
                    },
                )
        _append_record_unlocked(
            ledger,
            {
                "recorded_at": utc_now(),
                "role": role,
                "event_name": event.get("hook_event_name"),
                "tool_name": event.get("tool_name"),
                "tool_use_id": event.get("tool_use_id"),
                "outcome": outcome,
                "reason": reason,
                "child": str(child) if child is not None else None,
                "child_sha256": child_sha256,
                "child_returncode": child_returncode,
                "child_stdout_bytes": len(child_stdout),
                "child_stdout_sha256": sha256_bytes(child_stdout),
                "child_stderr_bytes": len(child_stderr),
                "child_stderr_sha256": sha256_bytes(child_stderr),
                "elapsed_ms": elapsed_ms,
                "fatal": fatal,
            },
        )


def fatal_state(evidence_root: Path) -> dict[str, Any] | None:
    state_path, _ledger, lock = guard_paths(evidence_root)
    with lock.open("ab") as lock_handle:
        fcntl.flock(lock_handle, fcntl.LOCK_EX)
        return _read_state_unlocked(state_path)


def validate_event(raw: bytes) -> dict[str, Any]:
    if len(raw) > MAX_HOOK_INPUT_BYTES:
        raise GuardFailure("hook_guard_input_too_large")
    try:
        event = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GuardFailure("hook_guard_input_invalid") from exc
    if not isinstance(event, dict) or not isinstance(
        event.get("hook_event_name"), str
    ):
        raise GuardFailure("hook_guard_event_invalid")
    return event


def run_child(
    child: Path, raw_event: bytes, timeout_ms: int
) -> tuple[int, bytes, bytes, int]:
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            [sys.executable, str(child)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as exc:
        raise GuardFailure("hook_guard_child_launch_failed") from exc
    try:
        stdout, stderr = process.communicate(
            raw_event, timeout=timeout_ms / 1000.0
        )
    except subprocess.TimeoutExpired as exc:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        stdout, stderr = process.communicate()
        elapsed_ms = int((time.monotonic() - started) * 1000)
        raise GuardFailure(
            "hook_guard_child_timeout",
            returncode=process.returncode,
            stdout=stdout,
            stderr=stderr,
            elapsed_ms=elapsed_ms,
        ) from exc
    elapsed_ms = int((time.monotonic() - started) * 1000)
    return process.returncode, stdout, stderr, elapsed_ms


def validate_child_response(
    returncode: int, stdout: bytes, stderr: bytes, event_name: str
) -> tuple[str, dict[str, object] | None]:
    if returncode != 0:
        raise GuardFailure("hook_guard_child_crash", returncode=returncode)
    if stderr:
        raise GuardFailure("hook_guard_child_stderr")
    if len(stdout) > MAX_CHILD_OUTPUT_BYTES:
        raise GuardFailure("hook_guard_child_output_too_large")
    if not stdout:
        return "allow", None
    try:
        document = json.loads(stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GuardFailure("hook_guard_child_output_invalid") from exc
    if not isinstance(document, dict):
        raise GuardFailure("hook_guard_child_output_not_object")
    if event_name in {"PostToolUse", "PostToolUseFailure"}:
        if (
            document.get("continue") is not False
            or not isinstance(document.get("stopReason"), str)
            or not document["stopReason"]
            or document.get("decision") != "block"
            or not isinstance(document.get("reason"), str)
            or not document["reason"]
            or "hookSpecificOutput" in document
        ):
            raise GuardFailure("hook_guard_child_denial_not_terminal")
        return "deny", document
    if event_name != "PreToolUse":
        raise GuardFailure("hook_guard_child_event_unsupported")
    specific = document.get("hookSpecificOutput")
    if not isinstance(specific, dict):
        raise GuardFailure("hook_guard_child_specific_output_missing")
    if specific.get("hookEventName") != event_name:
        raise GuardFailure("hook_guard_child_event_name_mismatch")
    decision = specific.get("permissionDecision")
    if decision == "deny":
        if (
            document.get("continue") is not False
            or not isinstance(document.get("stopReason"), str)
            or not document["stopReason"]
            or not isinstance(specific.get("permissionDecisionReason"), str)
            or not specific["permissionDecisionReason"]
        ):
            raise GuardFailure("hook_guard_child_denial_not_terminal")
        return "deny", document
    if decision == "allow":
        if document.get("continue") is False:
            raise GuardFailure("hook_guard_child_allow_is_terminal")
        updated = specific.get("updatedInput")
        if updated is not None and not isinstance(updated, dict):
            raise GuardFailure("hook_guard_child_updated_input_invalid")
        return "allow", document
    raise GuardFailure("hook_guard_child_decision_invalid")


def _detail_bytes(
    failure: GuardFailure, name: str
) -> bytes:
    value = failure.details.get(name, b"")
    return value if isinstance(value, bytes) else b""


def fail(
    *,
    code: str,
    config: Mapping[str, object],
    evidence_root: Path,
    role: str,
    event: Mapping[str, object],
    child: Path | None,
    child_sha256: str | None,
    returncode: int | None = None,
    stdout: bytes = b"",
    stderr: bytes = b"",
    elapsed_ms: int = 0,
) -> int:
    try:
        record(
            config=config,
            evidence_root=evidence_root,
            role=role,
            event=event,
            outcome="terminate",
            reason=code,
            child=child,
            child_sha256=child_sha256,
            child_returncode=returncode,
            child_stdout=stdout,
            child_stderr=stderr,
            elapsed_ms=elapsed_ms,
            fatal=True,
        )
    except BaseException:
        # The stop response is the final containment boundary even if evidence
        # storage itself is unavailable.
        pass
    emit_stop(code, str(event.get("hook_event_name", "unknown")))
    return 0


def execute(argv: Sequence[str], raw_event: bytes) -> int:
    values = parse_args(argv)
    config, _config_path, evidence_root, child = validated_runtime(values)
    event = validate_event(raw_event)
    previous = fatal_state(evidence_root)
    if previous is not None:
        return fail(
            code="hook_guard_already_fatal",
            config=config,
            evidence_root=evidence_root,
            role=values.role,
            event=event,
            child=None,
            child_sha256=None,
        )

    try:
        completion_pending = begin_native_tool_transition(
            evidence_root=evidence_root,
            role=values.role,
            event=event,
        )
    except GuardFailure as failure:
        return fail(
            code=failure.code,
            config=config,
            evidence_root=evidence_root,
            role=values.role,
            event=event,
            child=None,
            child_sha256=None,
        )

    try:
        returncode, stdout, stderr, elapsed_ms = run_child(
            child, raw_event, values.timeout_ms
        )
        try:
            disposition, document = validate_child_response(
                returncode,
                stdout,
                stderr,
                str(event.get("hook_event_name", "unknown")),
            )
        except GuardFailure as failure:
            failure.details.setdefault("returncode", returncode)
            failure.details.setdefault("stdout", stdout)
            failure.details.setdefault("stderr", stderr)
            failure.details.setdefault("elapsed_ms", elapsed_ms)
            raise
    except GuardFailure as failure:
        return fail(
            code=failure.code,
            config=config,
            evidence_root=evidence_root,
            role=values.role,
            event=event,
            child=child,
            child_sha256=values.child_sha256,
            returncode=(
                failure.details.get("returncode")
                if isinstance(failure.details.get("returncode"), int)
                else None
            ),
            stdout=_detail_bytes(failure, "stdout"),
            stderr=_detail_bytes(failure, "stderr"),
            elapsed_ms=(
                failure.details.get("elapsed_ms")
                if isinstance(failure.details.get("elapsed_ms"), int)
                else 0
            ),
        )

    concurrent = fatal_state(evidence_root)
    if concurrent is not None:
        return fail(
            code="hook_guard_concurrent_fatal",
            config=config,
            evidence_root=evidence_root,
            role=values.role,
            event=event,
            child=child,
            child_sha256=values.child_sha256,
            returncode=returncode,
            stdout=stdout,
            stderr=stderr,
            elapsed_ms=elapsed_ms,
        )
    if disposition == "deny":
        record(
            config=config,
            evidence_root=evidence_root,
            role=values.role,
            event=event,
            outcome="child_deny",
            reason="hook_guard_child_denied",
            child=child,
            child_sha256=values.child_sha256,
            child_returncode=returncode,
            child_stdout=stdout,
            child_stderr=stderr,
            elapsed_ms=elapsed_ms,
            fatal=True,
        )
        assert document is not None
        sys.stdout.buffer.write(canonical(document))
        sys.stdout.buffer.flush()
        return 0

    if completion_pending:
        try:
            finish_native_tool_transition(
                evidence_root=evidence_root,
                role=values.role,
                event=event,
            )
        except GuardFailure as failure:
            return fail(
                code=failure.code,
                config=config,
                evidence_root=evidence_root,
                role=values.role,
                event=event,
                child=child,
                child_sha256=values.child_sha256,
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
                elapsed_ms=elapsed_ms,
            )

    record(
        config=config,
        evidence_root=evidence_root,
        role=values.role,
        event=event,
        outcome="allow",
        reason=None,
        child=child,
        child_sha256=values.child_sha256,
        child_returncode=returncode,
        child_stdout=stdout,
        child_stderr=stderr,
        elapsed_ms=elapsed_ms,
        fatal=False,
    )
    if document is not None:
        sys.stdout.buffer.write(canonical(document))
        sys.stdout.buffer.flush()
    return 0


def _evidence_hint(argv: Sequence[str]) -> Path | None:
    try:
        index = list(argv).index("--evidence-root")
        return Path(argv[index + 1])
    except (ValueError, IndexError):
        return None


def main(argv: Sequence[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    raw_event = sys.stdin.buffer.read(MAX_HOOK_INPUT_BYTES + 1)
    try:
        return execute(arguments, raw_event)
    except BaseException as exc:
        code = (
            exc.code
            if isinstance(exc, GuardFailure)
            else f"hook_guard_internal_{type(exc).__name__}"
        )
        try:
            event = validate_event(raw_event)
        except GuardFailure:
            event = {"hook_event_name": "unknown"}
        hint = _evidence_hint(arguments)
        if hint is not None and hint.is_dir() and not hint.is_symlink():
            try:
                record(
                    config={},
                    evidence_root=hint.resolve(strict=True),
                    role="unknown",
                    event=event,
                    outcome="terminate",
                    reason=code,
                    child=None,
                    child_sha256=None,
                    child_returncode=None,
                    child_stdout=b"",
                    child_stderr=b"",
                    elapsed_ms=0,
                    fatal=True,
                )
            except BaseException:
                pass
        emit_stop(
            code,
            str(event.get("hook_event_name", "unknown")),
        )
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
