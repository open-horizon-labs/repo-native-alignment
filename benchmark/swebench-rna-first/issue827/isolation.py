#!/usr/bin/env python3
"""Fail-closed isolation primitives for the #827 selector.

This module deliberately contains no Claude- or benchmark-specific orchestration.
It defines the boundary shared by both arms:

* effective-path checks for Claude's native filesystem tools;
* immutable, single-use Bash requests;
* an exact offline Docker worker invocation;
* a mandatory strace contract and parser;
* append-only, monotonic, hash-chained enforcement events.

The production harness must still qualify the generated Seatbelt profile and the
exact Claude Code permission behavior before any model authorization.
"""

from __future__ import annotations

from dataclasses import dataclass
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import time
import uuid
from typing import Iterable, Mapping, Sequence


EVENT_SCHEMA = "issue827-isolation-event-v1"
REQUEST_SCHEMA = "issue827-bash-request-v1"
TRACE_SCHEMA = "issue827-strace-report-v1"
TEARDOWN_SCHEMA = "issue827-worker-teardown-v1"
BROKER_TRIGGER_SCHEMA = "issue827-trusted-rna-broker-trigger-v1"
BROKER_READY_SCHEMA = "issue827-trusted-rna-broker-ready-v1"
BROKER_STOP_SCHEMA = "issue827-trusted-rna-broker-stop-v1"
BROKER_TEARDOWN_SCHEMA = "issue827-trusted-rna-broker-teardown-v1"
BROKER_OS_INJECTED_ENV_NAMES = {"__CF_USER_TEXT_ENCODING"}
ZERO_SHA256 = "0" * 64
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
IMAGE_RE = re.compile(r"^[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}$")
REQUEST_ID_RE = re.compile(r"^[0-9a-f]{32}$")
ENV_NAME_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
MAX_WORKER_REQUEST_BYTES = 1024 * 1024
SAFE_WORKER_ENV = {
    "HOME",
    "TMPDIR",
    "PATH",
    "LANG",
    "LC_ALL",
    "TZ",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_TERMINAL_PROMPT",
    "PIP_NO_INDEX",
    "PIP_DISABLE_PIP_VERSION_CHECK",
    "PYTHONDONTWRITEBYTECODE",
    "PYTHONNOUSERSITE",
}
SECRET_ENV_PARTS = (
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
    "AUTH",
    "COOKIE",
)
NON_SECRET_ENV_NAMES = frozenset({"RNA_EMBEDDING_TOKENIZER_SHA256"})
NATIVE_TOOLS = {"Read", "Edit", "Write", "Glob", "Grep"}
WRITE_TOOLS = {"Edit", "Write"}
TRACE_TERMINAL_RE = re.compile(
    r"(?:^|\s)\+\+\+ (?:exited with [0-9]+|killed by [A-Z0-9]+(?: \(core dumped\))?) \+\+\+\s*$"
)
TRACE_ABSOLUTE_PATH_RE = re.compile(r'"(/(?:[^"\\]|\\.)*)"')
TRACE_EXECVE_RE = re.compile(r"(?:^|\s)execve(?:at)?\(")
TRACE_MISSING_SELINUX_STATFS_RE = re.compile(
    r'(?:^|\s)statfs\("(?P<path>/sys/fs/selinux|/selinux)",'
    r".*\)\s*=\s*-1 ENOENT(?:\s|$)"
)
TRACE_BLOCKED_RESULT_RE = re.compile(
    r"\)\s*=\s*-1\s+E[A-Z0-9_]+(?:\s|$)"
)
TRACE_INET_RE = re.compile(
    r"(?:socket\(\s*AF_INET6?\b|sa_family=AF_INET6?\b|"
    r"connect\([^,\n]+,\s*\{[^}\n]*AF_INET6?\b)"
)
TRACE_LANDLOCK_SUCCESS_RE = re.compile(
    r"\blandlock_restrict_self\([^)]*\)\s*=\s*0(?:\s|$)"
)


def canonical(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    ).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_secret_env_name(name: object) -> bool:
    """Preserve fail-closed substring checks except for exact proven-safe names."""

    return (
        isinstance(name, str)
        and name not in NON_SECRET_ENV_NAMES
        and any(part in name.upper() for part in SECRET_ENV_PARTS)
    )


class IsolationViolation(RuntimeError):
    """A structured fail-closed isolation failure."""

    def __init__(self, code: str, **details: object):
        super().__init__(code)
        self.code = code
        self.details = details

    def as_dict(self) -> dict[str, object]:
        return {
            "schema_version": "issue827-isolation-violation-v1",
            "code": self.code,
            "fatal": True,
            "details": self.details,
        }


def _event_hash(record: Mapping[str, object]) -> str:
    unhashed = dict(record)
    unhashed.pop("event_sha256", None)
    return sha256_bytes(canonical(unhashed))


def verify_event_chain(records: Sequence[Mapping[str, object]]) -> str:
    previous = ZERO_SHA256
    previous_monotonic = -1
    for expected_sequence, record in enumerate(records, start=1):
        if record.get("schema_version") != EVENT_SCHEMA:
            raise IsolationViolation(
                "ledger_schema_mismatch", sequence=expected_sequence
            )
        if record.get("sequence") != expected_sequence:
            raise IsolationViolation(
                "ledger_sequence_mismatch",
                expected=expected_sequence,
                actual=record.get("sequence"),
            )
        if record.get("previous_event_sha256") != previous:
            raise IsolationViolation(
                "ledger_previous_hash_mismatch", sequence=expected_sequence
            )
        monotonic_ns = record.get("monotonic_ns")
        if (
            not isinstance(monotonic_ns, int)
            or monotonic_ns <= previous_monotonic
        ):
            raise IsolationViolation(
                "ledger_monotonicity_failure", sequence=expected_sequence
            )
        if record.get("event_sha256") != _event_hash(record):
            raise IsolationViolation(
                "ledger_event_hash_mismatch", sequence=expected_sequence
            )
        previous = str(record["event_sha256"])
        previous_monotonic = monotonic_ns
    return previous


class HashChainLedger:
    """Append-only JSONL ledger with an independently verifiable hash chain."""

    def __init__(self, path: Path):
        self.path = path
        self.lock_path = path.with_name(f".{path.name}.lock")

    def read(self) -> list[dict]:
        if not self.path.exists():
            return []
        records: list[dict] = []
        for line_number, raw in enumerate(
            self.path.read_bytes().splitlines(), start=1
        ):
            try:
                value = json.loads(raw)
            except json.JSONDecodeError as exc:
                raise IsolationViolation(
                    "ledger_json_invalid", line=line_number
                ) from exc
            if not isinstance(value, dict):
                raise IsolationViolation(
                    "ledger_record_not_object", line=line_number
                )
            records.append(value)
        verify_event_chain(records)
        return records

    def append(
        self,
        *,
        actor: str,
        event_type: str,
        outcome: str,
        arm: str,
        tool_use_id: str | None = None,
        payload: Mapping[str, object] | None = None,
        violation: Mapping[str, object] | None = None,
        extra_fields: Mapping[str, object] | None = None,
    ) -> dict:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        with self.lock_path.open("ab") as lock_handle:
            fcntl.flock(lock_handle, fcntl.LOCK_EX)
            records = self.read()
            previous = (
                str(records[-1]["event_sha256"]) if records else ZERO_SHA256
            )
            previous_monotonic = (
                int(records[-1]["monotonic_ns"]) if records else -1
            )
            monotonic_ns = max(time.monotonic_ns(), previous_monotonic + 1)
            record: dict[str, object] = {
                "schema_version": EVENT_SCHEMA,
                "sequence": len(records) + 1,
                "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
                "monotonic_ns": monotonic_ns,
                "previous_event_sha256": previous,
                "actor": actor,
                "event_type": event_type,
                "outcome": outcome,
                "arm": arm,
                "tool_use_id": tool_use_id,
                "payload": dict(payload or {}),
                "violation": dict(violation) if violation is not None else None,
            }
            for key, value in (extra_fields or {}).items():
                if key in record or key == "event_sha256":
                    raise IsolationViolation(
                        "ledger_extra_field_reserved", field=key
                    )
                record[key] = value
            record["event_sha256"] = _event_hash(record)
            encoded = canonical(record)
            descriptor = os.open(
                self.path,
                os.O_WRONLY | os.O_CREAT | os.O_APPEND,
                0o600,
            )
            try:
                os.write(descriptor, encoded)
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
            return record


def _path_has_symlink(path: Path, stop: Path) -> str | None:
    """Return the first symlink from ``stop`` through ``path``."""
    try:
        relative = path.relative_to(stop)
    except ValueError:
        return str(path)
    current = stop
    for part in relative.parts:
        current = current / part
        try:
            mode = os.lstat(current).st_mode
        except FileNotFoundError:
            break
        except OSError as exc:
            raise IsolationViolation(
                "path_component_stat_failed", path=str(current), errno=exc.errno
            ) from exc
        if stat.S_ISLNK(mode):
            return str(current)
    return None


def _resolved_roots(raw_roots: Iterable[str]) -> list[Path]:
    roots: list[Path] = []
    for raw in raw_roots:
        if not isinstance(raw, str) or not raw or "\x00" in raw:
            raise IsolationViolation("allowed_root_invalid")
        root = Path(raw)
        if not root.is_absolute():
            raise IsolationViolation("allowed_root_not_absolute", path=raw)
        try:
            mode = os.lstat(root).st_mode
            resolved = root.resolve(strict=True)
        except OSError as exc:
            raise IsolationViolation(
                "allowed_root_unavailable", path=raw, errno=exc.errno
            ) from exc
        if stat.S_ISLNK(mode) or resolved != root:
            raise IsolationViolation(
                "allowed_root_is_link_or_alias", path=raw, resolved=str(resolved)
            )
        roots.append(resolved)
    if not roots:
        raise IsolationViolation("allowed_roots_empty")
    return roots


def validate_effective_path(
    *,
    tool_name: str,
    tool_input: Mapping[str, object],
    cwd: str | None,
    read_roots: Iterable[str],
    write_roots: Iterable[str],
) -> dict[str, object]:
    """Validate the actual base path used by a native Claude filesystem tool.

    ``Glob`` and ``Grep`` default to the event cwd when ``path`` is omitted.
    New ``Write`` targets are accepted only when their resolved, link-free
    location remains below an allowed write root.
    """

    if tool_name not in NATIVE_TOOLS:
        raise IsolationViolation("native_tool_unknown", tool=tool_name)
    glob_pattern: str | None = None
    if tool_name == "Glob":
        pattern = tool_input.get("pattern")
        if (
            not isinstance(pattern, str)
            or not pattern
            or "\x00" in pattern
        ):
            raise IsolationViolation("native_glob_pattern_invalid")
        if pattern.startswith("/") or ".." in pattern:
            raise IsolationViolation(
                "native_glob_pattern_escape",
                pattern_sha256=sha256_bytes(pattern.encode("utf-8")),
            )
        glob_pattern = pattern
    key = "file_path" if tool_name in {"Read", "Edit", "Write"} else "path"
    raw = tool_input.get(key)
    if raw is None and tool_name in {"Glob", "Grep"}:
        raw = cwd
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise IsolationViolation(
            "native_tool_path_missing_or_invalid", tool=tool_name, field=key
        )

    allowed = _resolved_roots(
        write_roots if tool_name in WRITE_TOOLS else read_roots
    )
    if cwd is None:
        base = allowed[0]
    else:
        if not isinstance(cwd, str) or not cwd or "\x00" in cwd:
            raise IsolationViolation("native_tool_cwd_invalid", tool=tool_name)
        base = Path(cwd)
        if not base.is_absolute():
            raise IsolationViolation(
                "native_tool_cwd_not_absolute", tool=tool_name, cwd=cwd
            )
        try:
            base = base.resolve(strict=True)
        except OSError as exc:
            raise IsolationViolation(
                "native_tool_cwd_unavailable", tool=tool_name, cwd=cwd
            ) from exc
        if not any(base == root or base.is_relative_to(root) for root in allowed):
            raise IsolationViolation(
                "native_tool_cwd_outside_allowed_roots",
                tool=tool_name,
                cwd=str(base),
            )

    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = base / candidate
    try:
        resolved = candidate.resolve(strict=False)
    except OSError as exc:
        raise IsolationViolation(
            "native_tool_path_resolution_failed",
            tool=tool_name,
            path=raw,
            errno=exc.errno,
        ) from exc
    matching = [
        root
        for root in allowed
        if resolved == root or resolved.is_relative_to(root)
    ]
    if not matching:
        raise IsolationViolation(
            "native_tool_path_outside_allowed_roots",
            tool=tool_name,
            path=raw,
            resolved=str(resolved),
        )
    root = max(matching, key=lambda item: len(item.parts))
    linked = _path_has_symlink(candidate, root)
    if linked is not None:
        raise IsolationViolation(
            "native_tool_path_uses_link",
            tool=tool_name,
            path=raw,
            link=linked,
        )
    result = {
        "tool": tool_name,
        "field": key,
        "requested_path_sha256": sha256_bytes(raw.encode("utf-8")),
        "effective_path": str(resolved),
        "effective_root": str(root),
        "access": "write" if tool_name in WRITE_TOOLS else "read",
    }
    if glob_pattern is not None:
        result["pattern_sha256"] = sha256_bytes(
            glob_pattern.encode("utf-8")
        )
    return result


def audit_private_tree(root: Path) -> dict[str, object]:
    """Reject aliases, links, hardlinks, and special files in an episode tree."""

    if not root.is_absolute():
        raise IsolationViolation("private_tree_root_not_absolute", path=str(root))
    try:
        root_mode = os.lstat(root).st_mode
        resolved_root = root.resolve(strict=True)
    except OSError as exc:
        raise IsolationViolation(
            "private_tree_root_unavailable", path=str(root), errno=exc.errno
        ) from exc
    if (
        stat.S_ISLNK(root_mode)
        or not stat.S_ISDIR(root_mode)
        or resolved_root != root
    ):
        raise IsolationViolation(
            "private_tree_root_not_private_directory",
            path=str(root),
            resolved=str(resolved_root),
        )

    regular_files = 0
    directories = 0
    total_bytes = 0
    digest = hashlib.sha256()
    stack = [root]
    while stack:
        directory = stack.pop()
        directories += 1
        with os.scandir(directory) as entries:
            for entry in sorted(entries, key=lambda item: os.fsencode(item.name)):
                path = Path(entry.path)
                metadata = entry.stat(follow_symlinks=False)
                relative = path.relative_to(root).as_posix()
                if entry.is_symlink():
                    raise IsolationViolation(
                        "private_tree_contains_symlink", path=relative
                    )
                if stat.S_ISDIR(metadata.st_mode):
                    stack.append(path)
                    digest.update(canonical(["directory", relative]))
                elif stat.S_ISREG(metadata.st_mode):
                    if metadata.st_nlink != 1:
                        raise IsolationViolation(
                            "private_tree_contains_hardlink",
                            path=relative,
                            link_count=metadata.st_nlink,
                        )
                    regular_files += 1
                    total_bytes += metadata.st_size
                    digest.update(
                        canonical(
                            [
                                "file",
                                relative,
                                metadata.st_size,
                                sha256_file(path),
                            ]
                        )
                    )
                else:
                    raise IsolationViolation(
                        "private_tree_contains_special_file",
                        path=relative,
                        mode=stat.S_IFMT(metadata.st_mode),
                    )
    return {
        "schema_version": "issue827-private-tree-audit-v1",
        "root": str(root),
        "directories": directories,
        "regular_files": regular_files,
        "bytes": total_bytes,
        "tree_digest": digest.hexdigest(),
        "links": 0,
        "hardlinks": 0,
        "special_files": 0,
    }


def _seatbelt_literal(path: Path) -> str:
    value = str(path)
    if not path.is_absolute() or "\x00" in value or "\n" in value:
        raise IsolationViolation("seatbelt_path_invalid", path=value)
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _seatbelt_any(
    paths: Sequence[Path], *, include_read_ancestors: bool = False
) -> str:
    roots = sorted(set(paths))
    literals = set(roots)
    if include_read_ancestors:
        for path in roots:
            literals.update(path.parents)
    clauses = " ".join(
        [
            *(f"(subpath {_seatbelt_literal(path)})" for path in roots),
            *(f"(literal {_seatbelt_literal(path)})" for path in sorted(literals)),
        ]
    )
    return f"(require-any {clauses})"


def generate_outer_seatbelt_profile(
    *,
    read_roots: Sequence[Path],
    write_roots: Sequence[Path],
    loopback_only_outbound: bool = False,
) -> str:
    """Generate the deterministic provider-parent filesystem boundary.

    The profile intentionally keeps provider network transport available while
    denying filesystem reads/writes outside explicitly declared roots.  It must
    be compiled and exercised by the exact no-spend launch qualification; mere
    generation is not evidence that a host accepts or enforces it.
    """

    if not read_roots or not write_roots:
        raise IsolationViolation("seatbelt_roots_empty")
    read = [path.resolve(strict=True) for path in read_roots]
    write = [path.resolve(strict=True) for path in write_roots]
    network_rules = ["(deny network-inbound)"]
    if loopback_only_outbound:
        network_rules.append(
            '(deny network-outbound (require-not (remote ip "localhost:*")))'
        )
    profile = "\n".join(
        [
            "(version 1)",
            "(allow default)",
            *network_rules,
            "(deny file-read*",
            f"  (require-not {_seatbelt_any(read, include_read_ancestors=True)}))",
            "(deny file-write*",
            f"  (require-not {_seatbelt_any(write)}))",
            "",
        ]
    )
    return profile


def generate_trusted_rna_seatbelt_profile(
    *,
    read_roots: Sequence[Path],
    write_roots: Sequence[Path],
) -> str:
    """Generate the inner, no-network boundary for one trusted RNA command.

    The provider parent needs outbound network access and provider credentials.
    The RNA traversal subprocess needs neither.  This second profile therefore
    removes both network directions and grants filesystem access only to the
    exact runtime, checkout, immutable index, harness, and private evidence
    roots supplied by the episode configurator.
    """

    if not read_roots or not write_roots:
        raise IsolationViolation("trusted_rna_seatbelt_roots_empty")
    read = [path.resolve(strict=True) for path in read_roots]
    write = [path.resolve(strict=True) for path in write_roots]
    for path in write:
        if not any(path == root or root in path.parents for root in read):
            raise IsolationViolation(
                "trusted_rna_write_root_not_readable", path=str(path)
            )
    return "\n".join(
        [
            "(version 1)",
            "(allow default)",
            "(deny network-inbound)",
            "(deny network-outbound)",
            "(deny file-read*",
            f"  (require-not {_seatbelt_any(read, include_read_ancestors=True)}))",
            "(deny file-write*",
            f"  (require-not {_seatbelt_any(write)}))",
            "",
        ]
    )


def validate_trusted_rna_root_separation(
    *,
    allowed_roots: Sequence[Path],
    forbidden_roots: Sequence[Path],
) -> None:
    """Reject an allowed root that would expose any forbidden subtree.

    An episode-specific child of the shared output root is valid; granting the
    shared root itself (or one of its parents) is not.
    """

    if not allowed_roots or not forbidden_roots:
        raise IsolationViolation("trusted_rna_root_separation_empty")
    allowed = [path.resolve(strict=True) for path in allowed_roots]
    forbidden = [path.resolve(strict=True) for path in forbidden_roots]
    for root in allowed:
        for denied in forbidden:
            if root == denied or root in denied.parents:
                raise IsolationViolation(
                    "trusted_rna_root_exposes_forbidden_path",
                    allowed=str(root),
                    forbidden=str(denied),
                )


def validate_worker_config(config: Mapping[str, object]) -> dict[str, object]:
    image = config.get("worker_image")
    if not isinstance(image, str) or IMAGE_RE.fullmatch(image) is None:
        raise IsolationViolation("worker_image_not_digest_pinned")
    docker_binary = config.get("docker_binary")
    if (
        not isinstance(docker_binary, str)
        or not Path(docker_binary).is_absolute()
        or not Path(docker_binary).is_file()
    ):
        raise IsolationViolation("docker_binary_invalid")
    docker_sha = config.get("docker_binary_sha256")
    if (
        not isinstance(docker_sha, str)
        or SHA256_RE.fullmatch(docker_sha) is None
        or sha256_file(Path(docker_binary)) != docker_sha
    ):
        raise IsolationViolation("docker_binary_digest_mismatch")
    uid = config.get("worker_uid")
    gid = config.get("worker_gid")
    if not isinstance(uid, int) or not isinstance(gid, int) or uid <= 0 or gid <= 0:
        raise IsolationViolation("worker_identity_not_non_root")
    for key in (
        "worker_entrypoint",
        "strace_path",
    ):
        value = config.get(key)
        if not isinstance(value, str) or not value.startswith("/"):
            raise IsolationViolation(f"{key}_invalid")
    for key in (
        "worker_entrypoint_sha256",
        "strace_artifact_sha256",
        "worker_image_manifest_sha256",
    ):
        value = config.get(key)
        if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
            raise IsolationViolation(f"{key}_invalid")
    if config.get("worker_image_preflight_verified") is not True:
        raise IsolationViolation("worker_image_preflight_not_verified")
    if (
        config.get("worker_landlock_required") is not True
        or not isinstance(config.get("worker_landlock_abi_min"), int)
        or int(config["worker_landlock_abi_min"]) < 1
        or config.get("worker_landlock_preflight_verified") is not True
    ):
        raise IsolationViolation("worker_landlock_not_preflight_verified")
    env = config.get("worker_env")
    if not isinstance(env, dict) or not env:
        raise IsolationViolation("worker_env_invalid")
    for name, value in env.items():
        if (
            not isinstance(name, str)
            or ENV_NAME_RE.fullmatch(name) is None
            or name not in SAFE_WORKER_ENV
            or is_secret_env_name(name)
            or not isinstance(value, str)
            or "\x00" in value
        ):
            raise IsolationViolation("worker_env_entry_forbidden", name=name)
    return {
        "worker_image": image,
        "worker_uid": uid,
        "worker_gid": gid,
        "worker_env": dict(sorted(env.items())),
    }


def _validate_mount(
    value: object, *, writable: bool, label: str
) -> tuple[Path, str]:
    if not isinstance(value, dict):
        raise IsolationViolation("worker_mount_invalid", mount=label)
    source = value.get("source")
    target = value.get("target")
    mode = value.get("mode")
    if (
        not isinstance(source, str)
        or not Path(source).is_absolute()
        or not Path(source).exists()
        or not isinstance(target, str)
        or not target.startswith("/")
        or mode != ("rw" if writable else "ro")
    ):
        raise IsolationViolation("worker_mount_invalid", mount=label)
    source_path = Path(source)
    if source_path.is_symlink() or source_path.resolve(strict=True) != source_path:
        raise IsolationViolation("worker_mount_is_link_or_alias", mount=label)
    return source_path, target


def build_docker_worker_argv(
    *,
    config: Mapping[str, object],
    request_path: Path,
    trace_directory: Path,
    container_name: str,
) -> list[str]:
    """Return the exact no-network, non-root, immutable worker invocation."""

    validated = validate_worker_config(config)
    if not REQUEST_ID_RE.fullmatch(container_name.rsplit("-", 1)[-1]):
        raise IsolationViolation("worker_container_name_invalid")
    if request_path.is_symlink() or not request_path.is_file():
        raise IsolationViolation("worker_request_mount_invalid")
    if trace_directory.is_symlink() or not trace_directory.is_dir():
        raise IsolationViolation("worker_trace_mount_invalid")
    checkout_source, checkout_target = _validate_mount(
        config.get("checkout_mount"), writable=True, label="checkout"
    )
    private_source, private_target = _validate_mount(
        config.get("private_tmp_mount"), writable=True, label="private_tmp"
    )
    declared = config.get("declared_toolchain_mounts", [])
    if not isinstance(declared, list):
        raise IsolationViolation("declared_toolchain_mounts_invalid")
    readonly_mounts = [
        _validate_mount(value, writable=False, label=f"toolchain_{index}")
        for index, value in enumerate(declared)
    ]
    cwd = config.get("worker_cwd")
    if (
        not isinstance(cwd, str)
        or not cwd.startswith(checkout_target.rstrip("/") + "/")
        and cwd != checkout_target
    ):
        raise IsolationViolation("worker_cwd_invalid")
    pids_limit = config.get("worker_pids_limit")
    memory_bytes = config.get("worker_memory_bytes")
    cpus = config.get("worker_cpus")
    if not isinstance(pids_limit, int) or pids_limit < 16:
        raise IsolationViolation("worker_pids_limit_invalid")
    if not isinstance(memory_bytes, int) or memory_bytes < 64 * 1024 * 1024:
        raise IsolationViolation("worker_memory_limit_invalid")
    if not isinstance(cpus, (int, float)) or cpus <= 0:
        raise IsolationViolation("worker_cpu_limit_invalid")

    docker = str(config["docker_binary"])
    trace_target = "/run/rna-trace"
    argv = [
        docker,
        "run",
        "--rm",
        "--init",
        "-i",
        "--pull=never",
        "--network=none",
        "--user",
        f"{validated['worker_uid']}:{validated['worker_gid']}",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges:true",
        "--read-only",
        "--pids-limit",
        str(pids_limit),
        "--memory",
        str(memory_bytes),
        "--cpus",
        str(cpus),
        "--stop-timeout",
        "1",
        "--name",
        container_name,
        "--workdir",
        cwd,
        "--mount",
        f"type=bind,src={checkout_source},dst={checkout_target}",
        "--mount",
        f"type=bind,src={private_source},dst={private_target}",
        "--mount",
        f"type=bind,src={trace_directory},dst={trace_target}",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=256m",
    ]
    for source, target in sorted(readonly_mounts, key=lambda item: item[1]):
        argv.extend(
            [
                "--mount",
                f"type=bind,src={source},dst={target},readonly=true",
            ]
        )
    for name, value in validated["worker_env"].items():
        argv.extend(["--env", f"{name}={value}"])
    argv.extend(
        [
            str(validated["worker_image"]),
            str(config["strace_path"]),
            "-ff",
            "-q",
            "-ttt",
            "-yy",
            "-s",
            "4096",
            "-e",
            (
                "trace=%file,%network,%process,landlock_create_ruleset,"
                "landlock_add_rule,landlock_restrict_self"
            ),
            "-o",
            f"{trace_target}/trace",
            str(config["worker_entrypoint"]),
            "--require-landlock",
            "--landlock-abi-min",
            str(config["worker_landlock_abi_min"]),
            "--deny-path",
            trace_target,
        ]
    )
    return argv


def parse_strace_directory(
    trace_directory: Path,
    *,
    allowed_path_prefixes: Sequence[str],
    forbidden_path_fragments: Sequence[str],
) -> dict[str, object]:
    """Parse mandatory ``strace -ff`` outputs and retain denied attempts."""

    if trace_directory.is_symlink() or not trace_directory.is_dir():
        raise IsolationViolation("trace_directory_invalid")
    files = sorted(trace_directory.glob("trace*"))
    if not files:
        raise IsolationViolation("trace_output_missing")
    allowed = tuple(
        sorted(
            {
                prefix.rstrip("/") or "/"
                for prefix in allowed_path_prefixes
                if isinstance(prefix, str) and prefix.startswith("/")
            },
            key=len,
            reverse=True,
        )
    )
    if not allowed:
        raise IsolationViolation("trace_allowed_paths_empty")
    forbidden = tuple(
        value for value in forbidden_path_fragments if isinstance(value, str) and value
    )
    violations: list[dict[str, object]] = []
    observations: list[dict[str, object]] = []
    receipts: list[dict[str, object]] = []
    landlock_enforced = False
    for path in files:
        if path.is_symlink() or not path.is_file():
            raise IsolationViolation("trace_member_invalid", path=path.name)
        data = path.read_bytes()
        if not data:
            raise IsolationViolation("trace_member_empty", path=path.name)
        try:
            text = data.decode("utf-8", errors="strict")
        except UnicodeError as exc:
            raise IsolationViolation(
                "trace_member_not_utf8", path=path.name
            ) from exc
        terminal = any(
            TRACE_TERMINAL_RE.search(line) is not None
            for line in text.splitlines()
        )
        if not terminal:
            raise IsolationViolation(
                "trace_member_missing_terminal_record", path=path.name
            )
        for line_number, line in enumerate(text.splitlines(), start=1):
            if TRACE_LANDLOCK_SUCCESS_RE.search(line):
                landlock_enforced = True
            if TRACE_INET_RE.search(line):
                # Docker's network=none boundary is the enforcement layer.
                # Socket use (including loopback tests) is retained as
                # telemetry but cannot reach the host or provider network.
                observations.append(
                    {
                        "code": "network_syscall_observed",
                        "trace": path.name,
                        "line": line_number,
                        "line_sha256": sha256_bytes(line.encode("utf-8")),
                    }
                )
            for fragment in forbidden:
                if fragment in line:
                    # A fragment in argv or diagnostic text is not itself an
                    # access. Actual path-bearing syscalls are classified
                    # separately below.
                    observations.append(
                        {
                            "code": "forbidden_fragment_observed",
                            "trace": path.name,
                            "line": line_number,
                            "line_sha256": sha256_bytes(
                                line.encode("utf-8")
                            ),
                        }
                    )
            encoded_paths = TRACE_ABSOLUTE_PATH_RE.findall(line)
            # Only argv[0] is a filesystem access made by execve/execveat.
            # Later quoted strings are arguments and may contain shell source,
            # diagnostic text, or other absolute-looking data.
            if TRACE_EXECVE_RE.search(line):
                encoded_paths = encoded_paths[:1]
            missing_selinux_probe = TRACE_MISSING_SELINUX_STATFS_RE.search(
                line
            )
            blocked_result = TRACE_BLOCKED_RESULT_RE.search(line) is not None
            for encoded_path in encoded_paths:
                attempted = encoded_path.replace(r"\/", "/")
                if (
                    missing_selinux_probe is not None
                    and attempted == missing_selinux_probe.group("path")
                ):
                    # libselinux/coreutils probe these conventional locations
                    # with statfs. ENOENT proves that no filesystem object was
                    # reached; the probe is not an undeclared data access.
                    continue
                if any(fragment in attempted for fragment in forbidden):
                    destination = observations if blocked_result else violations
                    destination.append(
                        {
                            "code": (
                                "blocked_forbidden_path_attempt"
                                if blocked_result
                                else "forbidden_path_access"
                            ),
                            "trace": path.name,
                            "line": line_number,
                            "path_sha256": sha256_bytes(
                                attempted.encode("utf-8")
                            ),
                        }
                    )
                elif attempted != "/" and not any(
                    attempted == prefix
                    or attempted.startswith(prefix.rstrip("/") + "/")
                    for prefix in allowed
                ):
                    destination = observations if blocked_result else violations
                    destination.append(
                        {
                            "code": (
                                "blocked_undeclared_path_attempt"
                                if blocked_result
                                else "undeclared_path_access"
                            ),
                            "trace": path.name,
                            "line": line_number,
                            "path_sha256": sha256_bytes(
                                attempted.encode("utf-8")
                            ),
                        }
                    )
        receipts.append(
            {
                "name": path.name,
                "bytes": len(data),
                "sha256": sha256_bytes(data),
                "terminal_record": True,
            }
        )
    if not landlock_enforced:
        raise IsolationViolation("trace_landlock_enforcement_missing")
    unique = {
        canonical(item): item
        for item in violations
    }
    violations = [unique[key] for key in sorted(unique)]
    unique_observations = {
        canonical(item): item
        for item in observations
    }
    observations = [
        unique_observations[key] for key in sorted(unique_observations)
    ]
    report = {
        "schema_version": TRACE_SCHEMA,
        "complete": True,
        "tracer": "strace-ff",
        "landlock_enforced": True,
        "members": receipts,
        "observations": observations,
        "observation_count": len(observations),
        "violations": violations,
        "violation_count": len(violations),
    }
    report["report_sha256"] = sha256_bytes(canonical(report))
    return report


def mint_request(
    *,
    config: Mapping[str, object],
    event: Mapping[str, object],
    execution_plane: str,
    command: str,
) -> tuple[dict[str, object], Path, str]:
    """Atomically create a mode-0600 request outside model-visible roots."""

    if execution_plane not in {"offline_bash", "trusted_rna"}:
        raise IsolationViolation("request_execution_plane_invalid")
    request_directory = Path(str(config["gateway_request_directory"]))
    if not request_directory.is_absolute():
        raise IsolationViolation("gateway_request_directory_not_absolute")
    request_directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    if (
        request_directory.is_symlink()
        or request_directory.resolve(strict=True) != request_directory
    ):
        raise IsolationViolation("gateway_request_directory_is_link")
    os.chmod(request_directory, 0o700)
    request_id = uuid.uuid4().hex
    request = {
        "schema_version": REQUEST_SCHEMA,
        "request_id": request_id,
        "arm": "A" if config.get("policy") == "control" else "T",
        "execution_plane": execution_plane,
        "issued_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "issued_monotonic_ns": time.monotonic_ns(),
        "session_id": event.get("session_id"),
        "tool_use_id": event.get("tool_use_id"),
        "cwd": event.get("cwd"),
        "command": command,
        "command_sha256": sha256_bytes(command.encode("utf-8")),
        "run_in_background": False,
    }
    encoded = canonical(request)
    request_sha = sha256_bytes(encoded)
    destination = request_directory / f"{request_id}.json"
    descriptor = os.open(
        destination,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        os.write(descriptor, encoded)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.chmod(destination, 0o600)
    return request, destination, request_sha


def gateway_command(
    *,
    config: Mapping[str, object],
    request_id: str,
    request_sha256: str,
) -> str:
    """Construct the sole Bash command statically authorized by settings."""

    import shlex

    values = [
        str(config["gateway_python"]),
        str(config["bash_gateway"]),
        "--config",
        str(config["gateway_config"]),
        "--config-sha256",
        str(config["gateway_config_sha256"]),
        "--request-id",
        request_id,
        "--request-sha256",
        request_sha256,
    ]
    if any("\n" in value or "\x00" in value for value in values):
        raise IsolationViolation("gateway_command_value_invalid")
    return " ".join(shlex.quote(value) for value in values)
