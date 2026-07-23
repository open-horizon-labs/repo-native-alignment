#!/usr/bin/env python3
"""Fail-closed launcher for the fresh #827 hermetic paired A/T selector.

The default action is a read-only preflight.  Paid model execution requires
both the ``run`` subcommand and ``--execute``.  This program never runs the
official evaluator: it records whether exactly one evaluation is authorized
for a policy-compliant nonempty terminal patch.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
from itertools import zip_longest
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import time
import uuid
from typing import Any, Mapping, Sequence

import provider_usage
import isolation
import registration_contract
import frontier_replay


RUN_SCHEMA = "issue827-hermetic-selector-run-v1"
REGISTRATION_SCHEMA = registration_contract.REGISTRATION_SCHEMA
SELECTION_SCHEMA = "issue827-fresh-pair-selection-v1"
IDENTITY_SCHEMA = "issue827-runtime-identity-v1"
RECEIPT_SCHEMA = "issue827-episode-receipt-v1"
QUERY_EVIDENCE_SCHEMA = "issue827-query-evidence-v1"
ISOLATION_REGISTRATION_SCHEMA = "issue827-isolation-registration-v1"
ISOLATION_HOST_SCHEMA = "issue827-isolation-host-v1"
WORKER_IMAGE_SCHEMA = "issue827-worker-image-manifest-v1"
WORKER_PREFLIGHT_SCHEMA = "issue827-worker-preflight-v1"
WORKER_SELF_TEST_SCHEMA = "issue827-worker-self-test-v1"
EMPTY_MCP_BYTES = b'{"mcpServers":{}}\n'
READY_SENTINEL = "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"
TRUSTED_GIT_BINARY = Path(
    "/Library/Developer/CommandLineTools/usr/bin/git"
)
TRUSTED_GIT_BINARY_SHA256 = (
    "be4afb2b003904725826250de9fb76567bbacf82323457b5a1ec26706b66bcae"
)
TRUSTED_GIT_CONFIG_WRITE_TARGET = Path("/dev/null")
TRUSTED_RNA_CACHE_EVIDENCE_REFS = (
    "archive",
    "manifest",
    "verification_receipt",
    "readiness_report",
)
SOURCE = Path(__file__).resolve().parent
CODE_KINDS = {
    "class", "const", "enum", "function", "interface", "method", "module",
    "struct", "trait", "type", "type_alias", "union",
}

REGISTERED_FILE_NAMES = registration_contract.REGISTERED_FILE_NAMES


class FailClosed(RuntimeError):
    """A frozen identity or evidence precondition did not hold."""


class TreatmentAcquisitionFailure(FailClosed):
    """RNA acquisition failed after immutable query-attempt evidence was retained."""

    def __init__(self, message: str, evidence: dict[str, Any], elapsed: float):
        super().__init__(message)
        self.evidence = evidence
        self.elapsed = elapsed


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def stable_code_ids(text: str) -> list[str]:
    ids: set[str] = set()
    for candidate in re.findall(r"`([^`\r\n]+)`", text):
        parts = candidate.rsplit(":", 1)
        if (
            len(parts) == 2
            and parts[1] in CODE_KINDS
            and ":" in parts[0]
            and not any(ch.isspace() for ch in candidate)
        ):
            ids.add(candidate)
    return sorted(ids)


def cache_inventory_sha256(cache: Path) -> str:
    """Hash the live operational cache by path, size, and file content."""
    require(cache.is_dir() and not cache.is_symlink(), "operational cache missing or symlinked")
    members: list[dict[str, Any]] = []
    for path in sorted(cache.rglob("*"), key=lambda item: item.relative_to(cache).as_posix()):
        relative = path.relative_to(cache).as_posix()
        require(not path.is_symlink(), f"operational cache contains symlink: {relative}")
        if path.is_dir():
            continue
        require(path.is_file(), f"operational cache contains non-file: {relative}")
        before = path.stat()
        digest = sha_file(path)
        after = path.stat()
        require(
            (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
            == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns),
            f"operational cache changed while hashing: {relative}",
        )
        members.append({"path": relative, "bytes": after.st_size, "sha256": digest})
    require(bool(members), "operational cache inventory is empty")
    return sha_bytes(canonical({
        "schema_version": "issue827-operational-cache-inventory-v1",
        "members": members,
    }))


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FailClosed(message)


def exact_keys(value: Mapping[str, Any], keys: set[str], where: str) -> None:
    missing = keys - set(value)
    extra = set(value) - keys
    require(not missing, f"{where}: missing fields {sorted(missing)}")
    require(not extra, f"{where}: unexpected fields {sorted(extra)}")


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as exc:
        raise FailClosed(f"invalid JSON {path}: {exc}") from exc


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    with temporary.open("xb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def file_ref(path: Path) -> dict[str, Any]:
    return {"path": str(path), "bytes": path.stat().st_size, "sha256": sha_file(path)}


def check_ref(value: Mapping[str, Any], where: str, *, materialize: bool = True) -> tuple[Path, bytes]:
    exact_keys(value, {"path", "bytes", "sha256"}, where)
    path = Path(value["path"])
    require(path.is_absolute(), f"{where}.path must be absolute")
    require(path.is_file() and not path.is_symlink(), f"{where}.path must be a nonsymlink file")
    require(path.stat().st_size == value["bytes"], f"{where}.bytes mismatch")
    require(sha_file(path) == value["sha256"], f"{where}.sha256 mismatch")
    return path, path.read_bytes() if materialize else b""


def json_pointer(document: Any, pointer: str) -> Any:
    require(pointer == "" or pointer.startswith("/"), f"invalid JSON pointer {pointer}")
    current = document
    if not pointer:
        return current
    for raw in pointer[1:].split("/"):
        token = raw.replace("~1", "/").replace("~0", "~")
        if isinstance(current, list):
            try:
                current = current[int(token)]
            except (ValueError, IndexError) as exc:
                raise FailClosed(f"JSON pointer does not resolve: {pointer}") from exc
        elif isinstance(current, dict):
            require(token in current, f"JSON pointer does not resolve: {pointer}")
            current = current[token]
        else:
            raise FailClosed(f"JSON pointer does not resolve: {pointer}")
    return current


def git(checkout: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        ["git", "-C", str(checkout), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        raise FailClosed(f"git {' '.join(args)} failed in {checkout}: {result.stderr.decode(errors='replace')}")
    return result


def clean_status(checkout: Path) -> bytes:
    return git(checkout, "status", "--porcelain=v1", "--untracked-files=all").stdout


def checkout_untracked_material(
    checkout: Path, *, ignored: bool
) -> tuple[bytes, ...]:
    args = ["ls-files", "--others"]
    if ignored:
        args.append("--ignored")
    args.extend(["--exclude-standard", "-z"])
    output = git(checkout, *args).stdout
    require(
        not output or output.endswith(b"\0"),
        "git untracked-material inventory is not NUL terminated",
    )
    return tuple(item for item in output.split(b"\0") if item)


def is_cache_material(path: bytes) -> bool:
    return path.startswith(b".oh/.cache/")


GITHUB_REPOSITORY_PATTERN = re.compile(
    r"(?P<owner>[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)/"
    r"(?P<repository>[A-Za-z0-9._-]+)"
)


def canonical_repository_slug(value: Any, where: str) -> str:
    require(isinstance(value, str), f"{where} must be a canonical owner/repository identity")
    match = GITHUB_REPOSITORY_PATTERN.fullmatch(value)
    require(match is not None, f"{where} must be a canonical owner/repository identity")
    return f"{match.group('owner').lower()}/{match.group('repository').lower()}"


def canonical_github_origin(value: Any, where: str) -> str:
    require(
        isinstance(value, str),
        f"{where} is not a canonical GitHub HTTPS or SSH URL",
    )
    candidate = value.removesuffix("/").removesuffix(".git")
    for prefix in (
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ):
        if candidate.startswith(prefix):
            return canonical_repository_slug(candidate.removeprefix(prefix), where)
    raise FailClosed(f"{where} is not a canonical GitHub HTTPS or SSH URL")


def verify_index_repository_identity(
    checkout: Path,
    cache_manifest: Any,
    where: str,
) -> tuple[str, str]:
    require(isinstance(cache_manifest, dict), f"{where}.cache_manifest must be an object")
    core = cache_manifest.get("core")
    require(isinstance(core, dict), f"{where}.cache_manifest.core must be an object")
    expected = canonical_repository_slug(
        core.get("repository"),
        f"{where}.cache_manifest.core.repository",
    )
    origin = git(checkout, "remote", "get-url", "--all", "origin", check=False)
    urls = origin.stdout.decode("utf-8", errors="replace").splitlines()
    require(
        origin.returncode == 0 and len(urls) == 1,
        f"{where}.origin must resolve to exactly one GitHub remote URL",
    )
    live = canonical_github_origin(urls[0], f"{where}.origin")
    require(
        live == expected,
        f"{where} repository identity mismatch: cache manifest={expected}, git origin={live}",
    )
    return expected, live


def verify_checkout(path_text: str, commit: str, tree: str, where: str, *, cache: bool = False) -> Path:
    checkout = Path(path_text)
    require(checkout.is_absolute() and checkout.is_dir() and not checkout.is_symlink(), f"{where} invalid")
    require(git(checkout, "rev-parse", "--is-inside-work-tree").stdout.strip() == b"true", f"{where} not git")
    require(git(checkout, "rev-parse", "HEAD").stdout.decode().strip() == commit, f"{where} HEAD mismatch")
    require(git(checkout, "rev-parse", "HEAD^{tree}").stdout.decode().strip() == tree, f"{where} tree mismatch")
    tracked_status = git(
        checkout, "status", "--porcelain=v1", "--untracked-files=no"
    ).stdout
    require(tracked_status == b"", f"{where} tracked state is not pristine")
    untracked = checkout_untracked_material(checkout, ignored=False)
    ignored = checkout_untracked_material(checkout, ignored=True)
    if cache:
        unexpected = tuple(
            path
            for path in (*untracked, *ignored)
            if not is_cache_material(path)
        )
        require(
            not unexpected,
            f"{where} contains material outside .oh/.cache",
        )
        cache_path = checkout / ".oh/.cache"
        require(cache_path.is_dir() and not cache_path.is_symlink(), f"{where} missing nonsymlink .oh/.cache")
        try:
            cache_path.resolve(strict=True).relative_to(checkout.resolve(strict=True))
        except (OSError, ValueError) as exc:
            raise FailClosed(f"{where} cache escapes checkout") from exc
    else:
        require(
            not untracked and not ignored,
            f"{where} contains untracked or ignored material",
        )
    return checkout


def verify_model_checkout(
    path_text: str,
    commit: str,
    tree: str,
    where: str,
) -> Path:
    """Verify model checkout identity and its private-tree isolation boundary."""

    checkout = verify_checkout(path_text, commit, tree, where)
    try:
        isolation.audit_private_tree(checkout)
    except isolation.IsolationViolation as exc:
        raise FailClosed(
            f"{where} private-tree audit failed: {exc.code}"
        ) from exc
    return checkout


def title_bytes(problem: bytes) -> bytes:
    offset = problem.find(b"\n")
    title = problem if offset < 0 else problem[:offset]
    if offset >= 0 and title.endswith(b"\r"):
        title = title[:-1]
    require(bool(title), "empty exact title is forbidden")
    title.decode("utf-8", errors="strict")
    return title


def safe_summary(stdout_path: Path) -> dict[str, Any]:
    try:
        value = json.loads(stdout_path.read_bytes())
    except (OSError, json.JSONDecodeError) as exc:
        return {"valid_json": False, "error": str(exc)}
    if not isinstance(value, dict):
        return {"valid_json": True, "top_level_type": type(value).__name__}
    allowed = {
        "type", "subtype", "is_error", "duration_ms", "duration_api_ms",
        "num_turns", "session_id", "total_cost_usd", "usage", "modelUsage",
        "permission_denials",
    }
    return {"valid_json": True, **{key: value[key] for key in allowed if key in value}}


def token_ledger(
    raw_result: Mapping[str, Any],
    *,
    model_invoked: bool = True,
    model_events: Sequence[Mapping[str, Any]] | None = None,
    provider_responses: int | None = None,
    provider_requests: int | None = None,
) -> dict[str, Any]:
    """Return strict observed provider evidence, retaining parser failures."""
    try:
        return provider_usage.parse_claude_usage(
            raw_result,
            model_events=model_events,
            model_invoked=model_invoked,
            provider_responses=provider_responses,
            provider_requests=provider_requests,
        )
    except provider_usage.ProviderUsageError as exc:
        return dict(exc.receipt)


def process_tree_rss_kib(root_pid: int) -> int:
    try:
        output = subprocess.run(
            ["ps", "-axo", "pid=,ppid=,rss="],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=True,
        ).stdout.decode()
    except (OSError, subprocess.CalledProcessError):
        return 0
    children: dict[int, list[int]] = {}
    rss: dict[int, int] = {}
    for line in output.splitlines():
        parts = line.split()
        if len(parts) != 3:
            continue
        try:
            pid, ppid, value = map(int, parts)
        except ValueError:
            continue
        children.setdefault(ppid, []).append(pid)
        rss[pid] = value
    pending = [root_pid]
    seen: set[int] = set()
    total = 0
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        total += rss.get(pid, 0)
        pending.extend(children.get(pid, ()))
    return total


def capture_patch(checkout: Path) -> tuple[bytes, list[dict[str, Any]]]:
    patch = bytearray(git(checkout, "diff", "--binary", "--no-ext-diff", "--no-color", "HEAD").stdout)
    names = sorted(name for name in git(checkout, "ls-files", "--others", "--exclude-standard", "-z").stdout.split(b"\0") if name)
    untracked: list[dict[str, Any]] = []
    for raw in names:
        name = raw.decode("utf-8", errors="surrogateescape")
        path = checkout / name
        require(path.is_file() and not path.is_symlink(), f"unsupported untracked path {name}")
        data = path.read_bytes()
        result = git(checkout, "diff", "--no-index", "--binary", "--no-ext-diff", "--no-color", "--", "/dev/null", name, check=False)
        require(result.returncode in (0, 1), f"cannot render untracked patch {name}")
        patch.extend(result.stdout)
        untracked.append({"path": name, "bytes": len(data), "sha256": sha_bytes(data)})
    return bytes(patch), untracked


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    result: list[dict[str, Any]] = []
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line:
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as exc:
            raise FailClosed(f"invalid JSONL {path}:{number}: {exc}") from exc
        require(isinstance(value, dict), f"non-object JSONL {path}:{number}")
        result.append(value)
    return result


def find_transcripts(session_id: str) -> list[Path]:
    root = Path.home() / ".claude/projects"
    if not root.is_dir():
        return []
    return sorted(path for path in root.rglob(f"{session_id}.jsonl") if path.is_file())


@dataclass(frozen=True)
class PreparedCase:
    rank: int
    case_id: str
    base_commit: str
    base_tree: str
    problem: bytes
    prompt: bytes
    title: bytes
    index_checkout: Path
    root: str
    cache_refs: dict[str, dict[str, Any]]
    cache_bindings: tuple[dict[str, Any], ...]
    cache_inventory_sha256: str
    expected_repository_identity: str
    live_repository_identity: str
    arm_order: tuple[str, str]
    checkouts: dict[str, Path]
    sessions: dict[str, str]
    isolation_worker: dict[str, Any]


@dataclass(frozen=True)
class PreparedRun:
    manifest_path: Path
    manifest_ref: dict[str, Any]
    registration_path: Path
    registration_ref: dict[str, Any]
    registration: dict[str, Any]
    selection_path: Path
    selection_ref: dict[str, Any]
    selection: dict[str, Any]
    claude_path: Path
    claude_version: str
    launcher_path: Path
    binary_path: Path
    rna_refs: dict[str, dict[str, Any]]
    mcp_path: Path
    output_root: Path
    cases: tuple[PreparedCase, ...]
    isolation_host: dict[str, Any]


def _absolute_real_directory(value: Any, where: str) -> Path:
    require(isinstance(value, str) and value, f"{where} invalid")
    path = Path(value)
    require(path.is_absolute() and path.is_dir() and not path.is_symlink(), f"{where} invalid")
    require(path.resolve(strict=True) == path, f"{where} is an alias")
    return path


def _fixed_isolation_registration(registration: Mapping[str, Any]) -> dict[str, Any]:
    value = registration.get("isolation_runtime")
    require(isinstance(value, dict), "registration.isolation_runtime missing")
    required = {
        "schema_version",
        "gateway_python_sha256",
        "git_binary_sha256",
        "docker_binary_sha256",
        "docker_server",
        "sandbox_exec_sha256",
        "worker_entrypoint",
        "worker_entrypoint_sha256",
        "strace_path",
        "strace_artifact_sha256",
        "worker_landlock_abi_min",
        "worker_uid",
        "worker_gid",
        "worker_pids_limit",
        "worker_memory_bytes",
        "worker_cpus",
        "worker_env",
        "trace_allowed_path_prefixes",
        "trace_forbidden_static_fragments",
        "gateway_tool_timeout_ms",
        "worker_timeout_seconds",
        "trusted_rna_timeout_seconds",
        "docker_control_timeout_seconds",
    }
    exact_keys(value, required, "registration.isolation_runtime")
    require(value["schema_version"] == ISOLATION_REGISTRATION_SCHEMA, "isolation registration schema mismatch")
    for key in (
        "gateway_python_sha256",
        "git_binary_sha256",
        "docker_binary_sha256",
        "sandbox_exec_sha256",
        "worker_entrypoint_sha256",
        "strace_artifact_sha256",
    ):
        require(isinstance(value[key], str) and re.fullmatch(r"[0-9a-f]{64}", value[key]) is not None, f"isolation {key} invalid")
    require(
        value["git_binary_sha256"] == TRUSTED_GIT_BINARY_SHA256,
        "trusted RNA Git digest differs from the exact CommandLineTools binary",
    )
    require(isinstance(value["worker_entrypoint"], str) and value["worker_entrypoint"].startswith("/"), "worker entrypoint invalid")
    require(isinstance(value["strace_path"], str) and value["strace_path"].startswith("/"), "strace path invalid")
    for key in ("worker_landlock_abi_min", "worker_uid", "worker_gid", "worker_pids_limit", "worker_memory_bytes", "gateway_tool_timeout_ms"):
        require(type(value[key]) is int and value[key] > 0, f"isolation {key} invalid")
    require(value["worker_uid"] != 0 and value["worker_gid"] != 0, "worker must be non-root")
    require(type(value["worker_cpus"]) in {int, float} and value["worker_cpus"] > 0, "worker CPU limit invalid")
    for key in ("worker_timeout_seconds", "trusted_rna_timeout_seconds", "docker_control_timeout_seconds"):
        require(type(value[key]) in {int, float} and value[key] > 0, f"isolation {key} invalid")
    require(value["worker_timeout_seconds"] == registration["model_runtime"]["wall_seconds"], "worker timeout differs from model wall")
    require(isinstance(value["docker_server"], str) and value["docker_server"].count("|") == 3, "worker Docker server identity invalid")
    require(value["gateway_tool_timeout_ms"] > value["worker_timeout_seconds"] * 1000, "gateway timeout cannot cover worker teardown")
    validation_probe = {
        **value,
        "worker_image": "probe/image@sha256:" + "0" * 64,
        "worker_image_manifest_sha256": "0" * 64,
        "worker_image_preflight_verified": True,
        "worker_landlock_required": True,
        "worker_landlock_preflight_verified": True,
        "docker_binary": "/dev/null",
        "docker_binary_sha256": sha_file(Path("/dev/null")),
    }
    # Validate the environment independently of host/image paths.
    env = validation_probe["worker_env"]
    require(isinstance(env, dict) and env, "worker environment invalid")
    for name, item in env.items():
        require(
            name in isolation.SAFE_WORKER_ENV
            and not isolation.is_secret_env_name(name)
            and isinstance(item, str)
            and "\x00" not in item,
            f"worker environment entry forbidden: {name}",
        )
    for key in ("trace_allowed_path_prefixes", "trace_forbidden_static_fragments"):
        require(isinstance(value[key], list) and value[key] and all(isinstance(item, str) and item for item in value[key]), f"isolation {key} invalid")
    return dict(value)


def verify_gateway_python_identity(gateway_python: Path) -> Path:
    """Require the invoking interpreter to be the registered gateway Python."""

    registered = gateway_python.resolve(strict=True)
    invoking = Path(sys.executable).resolve(strict=True)
    require(registered == invoking, "gateway Python differs from runner Python")
    return registered


def verify_isolation_host(
    manifest: Mapping[str, Any],
    registration: Mapping[str, Any],
) -> dict[str, Any]:
    fixed = _fixed_isolation_registration(registration)
    value = manifest.get("isolation")
    require(isinstance(value, dict), "manifest.isolation missing")
    exact_keys(
        value,
        {
            "schema_version",
            "gateway_python",
            "git_binary",
            "docker_binary",
            "sandbox_exec",
            "docker_host",
            "docker_server",
            "declared_toolchain_root",
            "declared_toolchain_tree_sha256",
            "system_read_roots",
            "provider_read_roots",
            "provider_write_roots",
        },
        "manifest.isolation",
    )
    require(value["schema_version"] == ISOLATION_HOST_SCHEMA, "isolation host schema mismatch")
    paths: dict[str, Path] = {}
    for name, expected_key in (
        ("gateway_python", "gateway_python_sha256"),
        ("git_binary", "git_binary_sha256"),
        ("docker_binary", "docker_binary_sha256"),
        ("sandbox_exec", "sandbox_exec_sha256"),
    ):
        path, _ = check_ref(value[name], f"manifest.isolation.{name}", materialize=False)
        require(path.is_absolute() and path.is_file() and not path.is_symlink(), f"isolation {name} path invalid")
        require(value[name]["sha256"] == fixed[expected_key], f"isolation {name} not registration bound")
        paths[name] = path
    require(
        paths["git_binary"] == TRUSTED_GIT_BINARY,
        "trusted RNA Git path not registration bound",
    )
    verify_gateway_python_identity(paths["gateway_python"])
    require(
        paths["sandbox_exec"] == Path("/usr/bin/sandbox-exec"),
        "trusted RNA requires exact /usr/bin/sandbox-exec",
    )
    docker_host = value["docker_host"]
    require(isinstance(docker_host, str) and docker_host.startswith("unix://") and Path(docker_host[7:]).is_absolute(), "Docker host must be an absolute unix socket")
    require(value["docker_server"] == fixed["docker_server"], "Docker server not registration bound")
    server = subprocess.run(
        [str(paths["docker_binary"]), "version", "--format", "{{.Server.Version}}|{{.Server.GitCommit}}|{{.Server.Os}}|{{.Server.Arch}}"],
        env={
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": "/var/empty",
            "TMPDIR": "/tmp",
            "LANG": "C",
            "LC_ALL": "C",
            "DOCKER_HOST": docker_host,
            "DOCKER_CONFIG": "/var/empty",
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=float(fixed["docker_control_timeout_seconds"]),
    )
    require(server.returncode == 0 and server.stdout.decode().strip() == fixed["docker_server"], "live Docker server identity mismatch")
    toolchain = _absolute_real_directory(value["declared_toolchain_root"], "declared toolchain root")
    toolchain_audit = isolation.audit_private_tree(toolchain)
    require(toolchain_audit["tree_digest"] == value["declared_toolchain_tree_sha256"], "declared toolchain tree digest mismatch")
    roots: dict[str, list[str]] = {}
    for key in ("system_read_roots", "provider_read_roots", "provider_write_roots"):
        raw = value[key]
        require(isinstance(raw, list), f"manifest.isolation.{key} invalid")
        resolved = [_absolute_real_directory(item, f"manifest.isolation.{key}") for item in raw]
        require(len(set(resolved)) == len(resolved), f"manifest.isolation.{key} duplicates")
        roots[key] = [str(item) for item in resolved]
    return {
        "schema_version": ISOLATION_HOST_SCHEMA,
        "fixed": fixed,
        "gateway_python": paths["gateway_python"],
        "git_binary": paths["git_binary"],
        "docker_binary": paths["docker_binary"],
        "sandbox_exec": paths["sandbox_exec"],
        "docker_host": docker_host,
        "docker_server": fixed["docker_server"],
        "declared_toolchain_root": toolchain,
        "declared_toolchain_audit": toolchain_audit,
        **roots,
    }


def _docker_preflight_env(host: Mapping[str, Any]) -> dict[str, str]:
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "HOME": "/var/empty",
        "TMPDIR": "/tmp",
        "LANG": "C",
        "LC_ALL": "C",
        "DOCKER_HOST": str(host["docker_host"]),
        "DOCKER_CONFIG": "/var/empty",
    }


def verify_isolation_worker(
    value: Mapping[str, Any],
    *,
    case_id: str,
    commit: str,
    tree: str,
    host: Mapping[str, Any],
) -> dict[str, Any]:
    require(isinstance(value, dict), f"{case_id}.isolation_worker invalid")
    exact_keys(value, {"image", "image_manifest", "preflight_receipt"}, f"{case_id}.isolation_worker")
    image = value["image"]
    require(isinstance(image, str) and isolation.IMAGE_RE.fullmatch(image) is not None, f"{case_id} worker image is not digest pinned")
    manifest_path, image_manifest = check_ref(value["image_manifest"], f"{case_id}.worker.image_manifest")
    preflight_path, preflight = check_ref(value["preflight_receipt"], f"{case_id}.worker.preflight_receipt")
    del manifest_path, preflight_path
    try:
        image_document = json.loads(image_manifest)
        preflight_document = json.loads(preflight)
    except json.JSONDecodeError as exc:
        raise FailClosed(f"{case_id} worker evidence is not JSON") from exc
    fixed = host["fixed"]
    required_manifest = {
        "schema_version": WORKER_IMAGE_SCHEMA,
        "case_id": case_id,
        "base_commit": commit,
        "base_tree": tree,
        "image": image,
        "worker_entrypoint": fixed["worker_entrypoint"],
        "worker_entrypoint_sha256": fixed["worker_entrypoint_sha256"],
        "strace_path": fixed["strace_path"],
        "strace_artifact_sha256": fixed["strace_artifact_sha256"],
    }
    for key, expected in required_manifest.items():
        require(image_document.get(key) == expected, f"{case_id} worker image manifest {key} mismatch")
    require(image in image_document.get("repo_digests", []), f"{case_id} image manifest lacks exact RepoDigest")
    require(image_document.get("os") == "linux", f"{case_id} worker image is not Linux")
    required_preflight = {
        "schema_version": WORKER_PREFLIGHT_SCHEMA,
        "verified": True,
        "case_id": case_id,
        "image": image,
        "image_manifest_sha256": value["image_manifest"]["sha256"],
        "docker_binary_sha256": fixed["docker_binary_sha256"],
        "pull_policy": "never",
        "network": "none",
        "non_root": True,
        "cap_drop": "ALL",
        "no_new_privileges": True,
        "read_only_root": True,
        "strace_ff": True,
        "landlock_enforced": True,
    }
    for key, expected in required_preflight.items():
        require(preflight_document.get(key) == expected, f"{case_id} worker preflight {key} mismatch")

    inspect = subprocess.run(
        [str(host["docker_binary"]), "image", "inspect", image],
        env=_docker_preflight_env(host),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    require(inspect.returncode == 0, f"{case_id} digest-pinned worker image unavailable offline")
    try:
        inspected = json.loads(inspect.stdout)
    except json.JSONDecodeError as exc:
        raise FailClosed(f"{case_id} Docker image inspect invalid") from exc
    require(isinstance(inspected, list) and len(inspected) == 1 and isinstance(inspected[0], dict), f"{case_id} Docker image inspect shape invalid")
    repo_digests = inspected[0].get("RepoDigests")
    require(isinstance(repo_digests, list) and image in repo_digests, f"{case_id} live Docker image digest mismatch")

    self_test_argv = [
        str(host["docker_binary"]), "run", "--rm", "--pull=never",
        "--network=none", "--user", f"{fixed['worker_uid']}:{fixed['worker_gid']}",
        "--cap-drop=ALL", "--security-opt=no-new-privileges:true", "--read-only",
        "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=64m",
        "--entrypoint", fixed["worker_entrypoint"], image,
        "--self-test-json", "--require-landlock", "--landlock-abi-min",
        str(fixed["worker_landlock_abi_min"]),
    ]
    self_test = subprocess.run(
        self_test_argv,
        env=_docker_preflight_env(host),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=float(fixed["docker_control_timeout_seconds"]),
    )
    require(self_test.returncode == 0, f"{case_id} worker self-test failed")
    try:
        self_test_document = json.loads(self_test.stdout)
    except json.JSONDecodeError as exc:
        raise FailClosed(f"{case_id} worker self-test JSON invalid") from exc
    expected_self_test = {
        "schema_version": WORKER_SELF_TEST_SCHEMA,
        "verified": True,
        "worker_entrypoint_sha256": fixed["worker_entrypoint_sha256"],
        "strace_artifact_sha256": fixed["strace_artifact_sha256"],
        "network": "none",
        "uid": fixed["worker_uid"],
        "gid": fixed["worker_gid"],
    }
    for key, expected in expected_self_test.items():
        require(self_test_document.get(key) == expected, f"{case_id} worker self-test {key} mismatch")
    require(
        type(self_test_document.get("landlock_abi")) is int
        and self_test_document["landlock_abi"] >= fixed["worker_landlock_abi_min"],
        f"{case_id} Landlock ABI unavailable",
    )
    return {
        "image": image,
        "image_manifest": dict(value["image_manifest"]),
        "preflight_receipt": dict(value["preflight_receipt"]),
        "live_image_inspect_sha256": sha_bytes(canonical(inspected[0])),
        "live_self_test": self_test_document,
        "live_self_test_argv_sha256": sha_bytes(canonical(self_test_argv)),
    }


def validate_registered_sources(registration: Mapping[str, Any]) -> None:
    try:
        registration_contract.validate_registration(
            registration,
            source_root=SOURCE,
        )
    except registration_contract.RegistrationContractError as exc:
        raise FailClosed(str(exc)) from exc


def validate_authoritative_selection(
    selection: Mapping[str, Any], registration_bytes: bytes
) -> None:
    require(selection.get("authoritative") is True, "selection is not authoritative")
    require(selection.get("state") == "selected_pre_model", "selection state is not selected_pre_model")
    require(
        selection.get("problem_statements_inspected_by_human_before_selection") is False,
        "selection permits prior human problem-statement inspection",
    )
    require(
        selection.get("gold_or_outcomes_inspected_before_selection") is False,
        "selection permits prior gold/outcome inspection",
    )
    require(selection.get("fresh_case_claim") is True, "selection does not claim fresh cases")
    require(selection.get("prior_model_calls") == 0, "selection imports prior model outcomes")
    require(
        selection.get("registration_sha256") == sha_bytes(registration_bytes),
        "selection binds another registration",
    )


def verify_runtime(manifest: Mapping[str, Any], registration: Mapping[str, Any]) -> tuple[Path, str]:
    runtime = registration["model_runtime"]
    require(
        runtime == registration_contract.FROZEN_MODEL_RUNTIME,
        "registration model runtime is not the frozen #827 runtime",
    )
    claude = manifest["claude"]
    exact_keys(claude, {"path", "sha256", "version_output"}, "manifest.claude")
    path = Path(claude["path"])
    require(path.is_absolute() and path.is_file() and not path.is_symlink(), "Claude CLI path invalid")
    require(claude["sha256"] == runtime["cli_sha256"] == sha_file(path), "Claude CLI SHA mismatch")
    result = subprocess.run([str(path), "--version"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    require(result.returncode == 0, "Claude CLI --version failed")
    version = result.stdout.decode().strip()
    require(version == claude["version_output"], "Claude CLI version output mismatch")
    require(version.startswith(runtime["cli_version"] + " ") or version == runtime["cli_version"], "Claude CLI version mismatch")
    return path, version


def verify_rna_artifact(manifest: Mapping[str, Any], registration: Mapping[str, Any]) -> tuple[Path, Path, dict[str, dict[str, Any]]]:
    artifact = manifest["rna_artifact"]
    exact_keys(
        artifact,
        {
            "launcher", "binary", "bundle_manifest", "archive",
            "upload_attestation", "verification_receipt",
            "canonical_environment", "runtime_receipt",
        },
        "manifest.rna_artifact",
    )
    expected = registration["rna_artifact"]
    keys = {
        "launcher": "launcher_sha256",
        "binary": "binary_sha256",
        "bundle_manifest": "bundle_manifest_sha256",
        "archive": "archive_sha256",
        "upload_attestation": "upload_attestation_sha256",
        "verification_receipt": "verification_receipt_sha256",
        "canonical_environment": "canonical_environment_sha256",
        "runtime_receipt": "runtime_receipt_sha256",
    }
    refs: dict[str, dict[str, Any]] = {}
    paths: dict[str, Path] = {}
    documents: dict[str, bytes] = {}
    trust_documents = {
        "bundle_manifest",
        "upload_attestation",
        "verification_receipt",
        "canonical_environment",
        "runtime_receipt",
    }
    for name, expected_key in keys.items():
        path, data = check_ref(
            artifact[name],
            f"manifest.rna_artifact.{name}",
            materialize=name in trust_documents,
        )
        require(artifact[name]["sha256"] == expected[expected_key], f"RNA {name} does not match registration")
        refs[name] = dict(artifact[name])
        paths[name] = path
        documents[name] = data
    try:
        registration_contract.validate_rna_trust_documents(
            registration,
            {
                name: documents[name]
                for name in trust_documents
            },
        )
    except registration_contract.RegistrationContractError as exc:
        raise FailClosed(str(exc)) from exc
    return paths["launcher"], paths["binary"], refs


def verify_qualification_closure(
    manifest: Mapping[str, Any],
    registration: Mapping[str, Any],
) -> None:
    value = manifest.get("qualification_closure")
    require(isinstance(value, dict), "manifest.qualification_closure missing")
    exact_keys(
        value,
        {"manifest", "archive"},
        "manifest.qualification_closure",
    )
    _, manifest_bytes = check_ref(
        value["manifest"],
        "manifest.qualification_closure.manifest",
    )
    _, _ = check_ref(
        value["archive"],
        "manifest.qualification_closure.archive",
        materialize=False,
    )
    expected = registration["qualification_closure"]
    require(
        value["manifest"]["sha256"] == expected["manifest_sha256"],
        "qualification manifest does not match registration",
    )
    require(
        value["archive"]["sha256"] == expected["archive_sha256"],
        "qualification archive does not match registration",
    )
    try:
        registration_contract.validate_qualification_manifest(
            registration,
            manifest_bytes,
            value["archive"]["sha256"],
        )
    except registration_contract.RegistrationContractError as exc:
        raise FailClosed(str(exc)) from exc


REQUIRED_CACHE_BINDINGS = {
    "repository_commit", "repository_tree", "root", "producer_commit",
    "rna_binary_sha256", "rna_launcher_sha256", "cache_archive_sha256",
    "cache_manifest_sha256", "operational_cache_inventory_sha256",
    "fresh_reopen_status", "readiness_sentinel",
}


def verify_cache(
    value: Mapping[str, Any],
    *,
    case_id: str,
    commit: str,
    tree: str,
    registration: Mapping[str, Any],
) -> tuple[
    Path,
    str,
    dict[str, dict[str, Any]],
    tuple[dict[str, Any], ...],
    str,
    str,
    str,
]:
    exact_keys(
        value,
        {"index_checkout", "root", "archive", "manifest", "verification_receipt", "readiness_report", "bindings"},
        f"{case_id}.cache",
    )
    root = value["root"]
    require(isinstance(root, str) and root and not any(ch.isspace() for ch in root), f"{case_id} root invalid")
    checkout = verify_checkout(value["index_checkout"], commit, tree, f"{case_id}.cache.index_checkout", cache=True)
    refs: dict[str, dict[str, Any]] = {}
    documents: dict[str, Any] = {}
    for name in ("archive", "manifest", "verification_receipt", "readiness_report"):
        path, data = check_ref(value[name], f"{case_id}.cache.{name}", materialize=name != "archive")
        refs[name] = dict(value[name])
        if name != "archive":
            try:
                documents[name] = json.loads(data)
            except json.JSONDecodeError as exc:
                raise FailClosed(f"{case_id}.cache.{name} is not JSON") from exc
    expected_repository, live_repository = verify_index_repository_identity(
        checkout,
        documents["manifest"],
        f"{case_id}.cache.index_checkout",
    )
    expected = {
        "repository_commit": commit,
        "repository_tree": tree,
        "root": root,
        "producer_commit": registration["rna_artifact"]["producer_commit"],
        "rna_binary_sha256": registration["rna_artifact"]["binary_sha256"],
        "rna_launcher_sha256": registration["rna_artifact"]["launcher_sha256"],
        "cache_archive_sha256": refs["archive"]["sha256"],
        "cache_manifest_sha256": refs["manifest"]["sha256"],
        "operational_cache_inventory_sha256": cache_inventory_sha256(checkout / ".oh/.cache"),
        "fresh_reopen_status": "READY",
        "readiness_sentinel": READY_SENTINEL,
    }
    bindings = value["bindings"]
    require(isinstance(bindings, list), f"{case_id}.cache.bindings must be a list")
    seen: set[str] = set()
    normalized: list[dict[str, Any]] = []
    for index, binding in enumerate(bindings):
        where = f"{case_id}.cache.bindings[{index}]"
        require(isinstance(binding, dict), f"{where} must be an object")
        exact_keys(binding, {"label", "document", "pointer", "equals"}, where)
        label = binding["label"]
        require(label in REQUIRED_CACHE_BINDINGS and label not in seen, f"{where}.label invalid or duplicate")
        require(binding["document"] in documents, f"{where}.document invalid")
        require(binding["equals"] == expected[label], f"{where}.equals not frozen identity")
        require(json_pointer(documents[binding["document"]], binding["pointer"]) == binding["equals"], f"{where} pointer mismatch")
        seen.add(label)
        normalized.append(dict(binding))
    require(seen == REQUIRED_CACHE_BINDINGS, f"{case_id} cache bindings incomplete: {sorted(REQUIRED_CACHE_BINDINGS - seen)}")
    return (
        checkout,
        root,
        refs,
        tuple(normalized),
        expected["operational_cache_inventory_sha256"],
        expected_repository,
        live_repository,
    )


def prepare(manifest_path: Path, *, permit_output: bool = False, permit_sessions: bool = False) -> PreparedRun:
    manifest_path = manifest_path.resolve(strict=True)
    manifest = read_json(manifest_path)
    require(isinstance(manifest, dict), "manifest must be an object")
    exact_keys(
        manifest,
        {
            "schema_version", "evidence_root", "registration", "selection", "runner",
            "common_supervisor", "claude", "rna_artifact", "mcp_config",
            "qualification_closure", "isolation", "output_root", "cases",
        },
        "manifest",
    )
    require(manifest["schema_version"] == RUN_SCHEMA, "run manifest schema mismatch")
    evidence_root = Path(manifest["evidence_root"])
    require(evidence_root.is_absolute() and evidence_root.is_dir(), "evidence_root invalid")
    require(manifest_path.is_relative_to(evidence_root), "manifest must live below evidence_root")

    runner_path, _ = check_ref(manifest["runner"], "manifest.runner")
    require(runner_path.resolve() == Path(__file__).resolve(), "manifest binds another runner")
    common_path, _ = check_ref(manifest["common_supervisor"], "manifest.common_supervisor")
    require(common_path.resolve() == (SOURCE / "common_supervisor.py").resolve(), "manifest binds another common supervisor")

    registration_path, registration_bytes = check_ref(manifest["registration"], "manifest.registration")
    registration = read_json(registration_path)
    require(registration.get("schema_version") == REGISTRATION_SCHEMA, "registration schema mismatch")
    validate_registered_sources(registration)
    registration_ref = dict(manifest["registration"])

    selection_path, _ = check_ref(manifest["selection"], "manifest.selection")
    selection = read_json(selection_path)
    require(selection.get("schema_version") == SELECTION_SCHEMA, "selection schema mismatch")
    validate_authoritative_selection(selection, registration_bytes)
    selected = selection.get("cases")
    require(isinstance(selected, list) and len(selected) == 2, "selection must contain exactly two cases")

    claude_path, claude_version = verify_runtime(manifest, registration)
    launcher_path, binary_path, rna_refs = verify_rna_artifact(manifest, registration)
    verify_qualification_closure(manifest, registration)
    mcp_path, mcp_bytes = check_ref(manifest["mcp_config"], "manifest.mcp_config")
    require(mcp_bytes == EMPTY_MCP_BYTES, "MCP config is not canonical strict-empty")
    require(manifest["mcp_config"]["sha256"] == registration["model_runtime"]["strict_empty_mcp_sha256"], "MCP hash differs from registration")
    isolation_host = verify_isolation_host(manifest, registration)

    output_root = Path(manifest["output_root"])
    require(output_root.is_absolute() and output_root.is_relative_to(evidence_root), "output_root invalid")
    if permit_output:
        require(output_root.is_dir(), "output_root missing")
    else:
        require(not output_root.exists(), "output_root already exists")

    case_values = manifest["cases"]
    require(isinstance(case_values, list) and len(case_values) == 2, "manifest must contain exactly two cases")
    cases: list[PreparedCase] = []
    sessions: set[str] = set()
    checkouts: set[Path] = set()
    for index, (case, chosen) in enumerate(zip(case_values, selected)):
        where = f"manifest.cases[{index}]"
        require(isinstance(case, dict), f"{where} must be an object")
        exact_keys(
            case,
            {
                "rank", "instance_id", "base_commit", "base_tree",
                "problem_statement", "user_prompt", "cache", "arms",
                "isolation_worker",
            },
            where,
        )
        require(case["rank"] == chosen.get("rank") and case["instance_id"] == chosen.get("instance_id"), f"{where} differs from selection")
        require(case["base_commit"] == chosen.get("base_commit") and case["base_tree"] == chosen.get("base_tree"), f"{where} base differs from selection")
        case_id = case["instance_id"]
        require(re.fullmatch(r"[A-Za-z0-9_.+-]+__[A-Za-z0-9_.+-]+-[0-9]+", case_id) is not None, f"{where}.instance_id invalid")
        problem_path, problem = check_ref(case["problem_statement"], f"{where}.problem_statement")
        del problem_path
        require(sha_bytes(problem) == chosen.get("problem_statement_sha256"), f"{where} problem statement mismatch")
        _, prompt = check_ref(case["user_prompt"], f"{where}.user_prompt")
        require(prompt.count(problem) == 1 and prompt.endswith(problem), f"{where} prompt must contain exact problem once at end")
        (
            index_checkout,
            root,
            cache_refs,
            cache_bindings,
            cache_inventory,
            expected_repository,
            live_repository,
        ) = verify_cache(
            case["cache"], case_id=case_id, commit=case["base_commit"], tree=case["base_tree"], registration=registration
        )
        require(index_checkout not in checkouts, f"index checkout reused: {index_checkout}")
        checkouts.add(index_checkout)
        arms = case["arms"]
        exact_keys(arms, {"A", "T"}, f"{where}.arms")
        arm_order = tuple(chosen.get("arm_order", []))
        require(arm_order in (("A", "T"), ("T", "A")), f"{where} arm order invalid")
        arm_checkouts: dict[str, Path] = {}
        arm_sessions: dict[str, str] = {}
        for arm in ("A", "T"):
            exact_keys(arms[arm], {"checkout", "session_id"}, f"{where}.arms.{arm}")
            checkout = verify_model_checkout(
                arms[arm]["checkout"],
                case["base_commit"],
                case["base_tree"],
                f"{where}.arms.{arm}.checkout",
            )
            require(checkout not in checkouts, f"checkout reused: {checkout}")
            checkouts.add(checkout)
            try:
                uuid.UUID(arms[arm]["session_id"])
            except (TypeError, ValueError) as exc:
                raise FailClosed(f"{where}.arms.{arm}.session_id invalid") from exc
            session = arms[arm]["session_id"]
            require(session not in sessions, f"session reused: {session}")
            sessions.add(session)
            if not permit_sessions:
                require(not find_transcripts(session), f"session already exists: {session}")
            arm_checkouts[arm] = checkout
            arm_sessions[arm] = session
        isolation_worker = verify_isolation_worker(
            case["isolation_worker"],
            case_id=case_id,
            commit=case["base_commit"],
            tree=case["base_tree"],
            host=isolation_host,
        )
        cases.append(
            PreparedCase(
                rank=case["rank"], case_id=case_id, base_commit=case["base_commit"], base_tree=case["base_tree"],
                problem=problem, prompt=prompt, title=title_bytes(problem), index_checkout=index_checkout,
                root=root, cache_refs=cache_refs, cache_bindings=cache_bindings,
                cache_inventory_sha256=cache_inventory,
                expected_repository_identity=expected_repository,
                live_repository_identity=live_repository,
                arm_order=arm_order,
                checkouts=arm_checkouts, sessions=arm_sessions,
                isolation_worker=isolation_worker,
            )
        )
    require({case.rank for case in cases} == {1, 2}, "selected ranks must be 1 and 2")
    return PreparedRun(
        manifest_path=manifest_path,
        manifest_ref=file_ref(manifest_path),
        registration_path=registration_path,
        registration_ref=registration_ref,
        registration=registration,
        selection_path=selection_path,
        selection_ref=dict(manifest["selection"]),
        selection=selection,
        claude_path=claude_path,
        claude_version=claude_version,
        launcher_path=launcher_path,
        binary_path=binary_path,
        rna_refs=rna_refs,
        mcp_path=mcp_path,
        output_root=output_root,
        cases=tuple(cases),
        isolation_host=isolation_host,
    )


def render_template(value: Any, replacements: Mapping[str, str]) -> Any:
    if isinstance(value, str):
        result = value
        for old, new in replacements.items():
            result = result.replace(old, new)
        return result
    if isinstance(value, list):
        return [render_template(item, replacements) for item in value]
    if isinstance(value, dict):
        return {key: render_template(item, replacements) for key, item in value.items()}
    return value


PLACEHOLDER_RE = re.compile(r"__[A-Z0-9_]+__")


def require_fully_rendered(value: Any, where: str) -> None:
    if isinstance(value, str):
        require(PLACEHOLDER_RE.search(value) is None, f"{where} contains unresolved placeholder: {value}")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            require_fully_rendered(item, f"{where}[{index}]")
    elif isinstance(value, dict):
        for key, item in value.items():
            require_fully_rendered(item, f"{where}.{key}")


def materialize_harness(case_root: Path, arm: str) -> dict[str, Path]:
    require(arm in {"A", "T"}, "unknown arm for harness materialization")
    harness = case_root / arm / "harness"
    bin_dir = harness / "bin"
    config_dir = harness / "config"
    bin_dir.mkdir(parents=True, exist_ok=False)
    config_dir.mkdir(parents=True, exist_ok=False)
    paths: dict[str, Path] = {}
    copied: dict[str, dict[str, Any]] = {}
    for name in (
        "rna_query.py", "rna_traverse.py", "frontier_replay.py",
        "tool_supervisor.py", "common_supervisor.py",
        "hook_guard.py",
        "live_identity.py", "isolation.py", "bash_gateway.py",
        "trusted_rna_broker.py",
    ):
        source = SOURCE / name
        require(source.is_file() and not source.is_symlink(), f"harness source invalid: {name}")
        destination = bin_dir / name
        shutil.copyfile(source, destination)
        destination.chmod(0o555)
        require(destination.stat().st_nlink == 1, f"harness copy is hardlinked: {name}")
        require(sha_file(destination) == sha_file(source), f"harness copy digest mismatch: {name}")
        paths[name] = destination
        copied[name] = {
            "source_sha256": sha_file(source),
            "destination": file_ref(destination),
            "mode": "0555",
            "link_count": 1,
        }
    paths["harness"] = harness
    paths["config"] = config_dir / "supervisor.json"
    manifest_path = harness / "materialization.json"
    atomic_write(
        manifest_path,
        canonical(
            {
                "schema_version": "issue827-harness-materialization-v1",
                "arm": arm,
                "files": copied,
            }
        ),
    )
    manifest_path.chmod(0o444)
    paths["materialization"] = manifest_path
    return paths


def make_identity(prepared: PreparedRun, case: PreparedCase) -> dict[str, Any]:
    return {
        "schema_version": IDENTITY_SCHEMA,
        "case_id": case.case_id,
        "base_commit": case.base_commit,
        "base_tree": case.base_tree,
        "root": case.root,
        "index_checkout": str(case.index_checkout),
        "expected_repository_identity": case.expected_repository_identity,
        "live_repository_identity": case.live_repository_identity,
        "producer_commit": prepared.registration["rna_artifact"]["producer_commit"],
        "launcher_path": str(prepared.launcher_path),
        "launcher_sha256": prepared.rna_refs["launcher"]["sha256"],
        "binary_path": str(prepared.binary_path),
        "binary_sha256": prepared.rna_refs["binary"]["sha256"],
        "canonical_environment": prepared.rna_refs["canonical_environment"],
        "canonical_environment_sha256": prepared.rna_refs["canonical_environment"]["sha256"],
        "cache_archive": case.cache_refs["archive"],
        "cache_archive_sha256": case.cache_refs["archive"]["sha256"],
        "cache_manifest": case.cache_refs["manifest"],
        "cache_manifest_sha256": case.cache_refs["manifest"]["sha256"],
        "operational_cache_inventory_sha256": case.cache_inventory_sha256,
        "cache_verification_receipt": case.cache_refs["verification_receipt"],
        "readiness_report": case.cache_refs["readiness_report"],
        "cache_bindings": list(case.cache_bindings),
        "cache_bindings_verified": True,
        "fresh_reopen_ready": True,
        "readiness_sentinel": READY_SENTINEL,
    }


def trusted_rna_cache_evidence_read_roots(
    case: PreparedCase,
) -> tuple[Path, ...]:
    """Return only the exact immutable cache files live identity revalidates."""

    roots: list[Path] = []
    for name in TRUSTED_RNA_CACHE_EVIDENCE_REFS:
        ref = case.cache_refs.get(name)
        require(
            isinstance(ref, Mapping)
            and set(ref) == {"path", "bytes", "sha256"},
            f"{case.case_id}.cache.{name} reference invalid",
        )
        path_value = ref["path"]
        expected_bytes = ref["bytes"]
        path = Path(path_value) if isinstance(path_value, str) else Path()
        require(
            isinstance(path_value, str)
            and path.is_absolute()
            and path.is_file()
            and not path.is_symlink(),
            f"{case.case_id}.cache.{name} path must be a nonsymlink file",
        )
        require(
            type(expected_bytes) is int
            and expected_bytes >= 0
            and path.stat().st_size == expected_bytes,
            f"{case.case_id}.cache.{name} bytes mismatch",
        )
        roots.append(path.resolve(strict=True))
    require(
        len(set(roots)) == len(TRUSTED_RNA_CACHE_EVIDENCE_REFS),
        f"{case.case_id}.cache evidence paths must be distinct",
    )
    return tuple(roots)


def path_is_covered_by_root(path: Path, roots: Sequence[Path]) -> bool:
    resolved = path.resolve(strict=False)
    return any(root == resolved or root in resolved.parents for root in roots)


def validate_trusted_rna_write_scope(
    *,
    write_roots: Sequence[Path],
    required_paths: Sequence[Path],
    unrelated_paths: Sequence[Path],
) -> None:
    """Prove the inner RNA sandbox can write only its owned episode state."""

    require(bool(write_roots), "trusted RNA write roots are empty")
    roots = tuple(path.resolve(strict=True) for path in write_roots)
    for path in required_paths:
        require(
            path_is_covered_by_root(path, roots),
            f"trusted RNA write roots do not cover owned path: {path}",
        )
    for path in unrelated_paths:
        require(
            not path_is_covered_by_root(path, roots),
            f"trusted RNA write root exposes unrelated episode path: {path}",
        )


def bind_trusted_rna_git(
    config: dict[str, Any],
    *,
    git_binary: Path,
    git_binary_sha256: str,
    canonical_environment: Mapping[str, str],
) -> None:
    """Bind exact Git identity without changing the registered RNA environment."""

    require(
        git_binary.is_absolute()
        and git_binary.is_file()
        and not git_binary.is_symlink()
        and sha_file(git_binary) == git_binary_sha256,
        "trusted RNA Git identity mismatch",
    )
    require(
        canonical_environment.get("GIT_CONFIG_GLOBAL")
        == str(TRUSTED_GIT_CONFIG_WRITE_TARGET)
        and canonical_environment.get("GIT_CONFIG_NOSYSTEM") == "1"
        and canonical_environment.get("GIT_TERMINAL_PROMPT") == "0",
        "trusted RNA canonical Git environment mismatch",
    )
    environment_bytes = canonical(dict(canonical_environment))
    config["git_binary"] = str(git_binary)
    config["git_binary_sha256"] = git_binary_sha256
    config["trusted_rna_env"] = {
        str(name): str(value)
        for name, value in canonical_environment.items()
    }
    require(
        canonical(config["trusted_rna_env"]) == environment_bytes,
        "trusted RNA Git binding changed the canonical environment",
    )


def configure_episode(
    prepared: PreparedRun,
    case: PreparedCase,
    arm: str,
    case_root: Path,
    harness_paths: Mapping[str, Path],
) -> tuple[Path, Path, Path, Path, dict[str, Any]]:
    episode = case_root / arm
    evidence = episode / "evidence"
    evidence.mkdir(parents=True, exist_ok=False)
    isolation_root = evidence / "isolation"
    model_private = episode / "private"
    gateway_private = isolation_root / "gateway-private"
    directories = {
        "model_private": model_private,
        "model_home": model_private / "home",
        "model_tmp": model_private / "tmp",
        "gateway_home": gateway_private / "home",
        "gateway_tmp": gateway_private / "tmp",
        "gateway_docker_config": gateway_private / "docker-config",
        "requests": isolation_root / "requests",
        "claimed": isolation_root / "claimed",
        "revoked": isolation_root / "revoked",
        "receipts": isolation_root / "receipts",
        "traces": isolation_root / "traces",
        "teardowns": isolation_root / "teardowns",
        "broker_requests": isolation_root / "trusted-rna-broker/requests",
        "broker_claimed": isolation_root / "trusted-rna-broker/claimed",
        "broker_output": isolation_root / "trusted-rna-broker/output",
        "preflight": isolation_root / "preflight",
        "hooks": evidence / "hooks",
        "rna_events": evidence / "rna-events",
        "query_events": evidence / "query",
        "trusted_rna_state": isolation_root / "trusted-rna-state",
    }
    for directory in directories.values():
        directory.mkdir(parents=True, exist_ok=False, mode=0o700)
        directory.chmod(0o700)
    native_tool_state_path = directories["hooks"] / "native-tool-state.json"
    atomic_write(
        native_tool_state_path,
        canonical(
            {
                "schema_version": "issue827-native-tool-state-v1",
                "active": {},
            }
        ),
    )

    identity_path = evidence / "runtime-identity.json"
    identity = make_identity(prepared, case)
    atomic_write(identity_path, canonical(identity))
    identity_sha = sha_file(identity_path)
    title_path = harness_paths["harness"] / "title-query.txt"
    atomic_write(title_path, case.title + b"\n")
    wrapper = harness_paths["rna_traverse.py"]
    query_wrapper = harness_paths["rna_query.py"]
    fixed = prepared.isolation_host["fixed"]
    live_self_test_path = isolation_root / "live-worker-self-test.json"
    atomic_write(live_self_test_path, canonical(case.isolation_worker["live_self_test"]))

    checkout_audit = isolation.audit_private_tree(case.checkouts[arm])
    model_private_audit = isolation.audit_private_tree(model_private)
    harness_audit = isolation.audit_private_tree(harness_paths["harness"])
    private_audits = {
        "schema_version": "issue827-private-tree-audits-v1",
        "checkout": checkout_audit,
        "model_private": model_private_audit,
        "harness": harness_audit,
        "declared_toolchain": prepared.isolation_host["declared_toolchain_audit"],
    }
    private_audits_path = isolation_root / "private-tree-audits.json"
    atomic_write(private_audits_path, canonical(private_audits))

    read_roots = [
        *map(Path, prepared.isolation_host["system_read_roots"]),
        *map(Path, prepared.isolation_host["provider_read_roots"]),
        case.checkouts[arm],
        episode,
        harness_paths["harness"],
        prepared.claude_path.parent,
        prepared.mcp_path.parent,
        prepared.isolation_host["declared_toolchain_root"],
        Path(sys.prefix),
    ]
    macos_selector_root = Path("/private/var/select")
    if macos_selector_root.is_dir():
        read_roots.append(macos_selector_root)
    write_roots = [
        *map(Path, prepared.isolation_host["provider_write_roots"]),
        case.checkouts[arm],
        episode,
        model_private,
    ]
    seatbelt_profile = isolation.generate_outer_seatbelt_profile(
        read_roots=read_roots,
        write_roots=write_roots,
    )
    for forbidden in (
        case.index_checkout,
        prepared.launcher_path.parent,
        prepared.binary_path.parent,
    ):
        require(
            str(forbidden.resolve(strict=True)) not in seatbelt_profile,
            f"outer Seatbelt exposes trusted RNA root: {forbidden}",
        )
    seatbelt_path = isolation_root / "outer.sb"
    atomic_write(seatbelt_path, seatbelt_profile.encode("utf-8"))
    seatbelt_path.chmod(0o444)

    credential_fragments = prepared.isolation_host["provider_read_roots"]
    sibling = case.checkouts["T" if arm == "A" else "A"]
    trusted_rna_environment = Path(
        prepared.rna_refs["canonical_environment"]["path"]
    )
    require(
        trusted_rna_environment.is_file()
        and not trusted_rna_environment.is_symlink()
        and sha_file(trusted_rna_environment)
        == prepared.registration["rna_artifact"]["canonical_environment_sha256"],
        "trusted RNA canonical environment identity mismatch",
    )
    canonical_trusted_environment = read_json(trusted_rna_environment)
    require(
        isinstance(canonical_trusted_environment, dict)
        and canonical_trusted_environment
        and all(
            isinstance(name, str)
            and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name)
            and not isolation.is_secret_env_name(name)
            and isinstance(value, str)
            and "\x00" not in value
            and "\n" not in value
            for name, value in canonical_trusted_environment.items()
        ),
        "trusted RNA canonical environment contract invalid",
    )
    canonical_environment_roots: list[Path] = []
    for name, value in canonical_trusted_environment.items():
        candidates = (
            value.split(os.pathsep)
            if name == "PATH"
            else [value] if value.startswith("/") else []
        )
        for candidate in candidates:
            if not candidate:
                continue
            candidate_path = Path(candidate)
            require(
                candidate_path.is_absolute() and candidate_path.exists(),
                f"trusted RNA canonical environment path unavailable: {name}",
            )
            canonical_environment_roots.append(
                candidate_path.resolve(strict=True)
            )
    trusted_rna_read_roots = [
        *map(Path, prepared.isolation_host["system_read_roots"]),
        case.checkouts[arm],
        case.index_checkout,
        *trusted_rna_cache_evidence_read_roots(case),
        harness_paths["harness"],
        prepared.launcher_path.parent,
        prepared.binary_path.parent,
        prepared.isolation_host["declared_toolchain_root"],
        Path(sys.prefix),
        directories["gateway_home"],
        directories["gateway_tmp"],
        directories["rna_events"],
        directories["query_events"],
        directories["trusted_rna_state"],
        identity_path,
        trusted_rna_environment,
        prepared.isolation_host["git_binary"],
        TRUSTED_GIT_CONFIG_WRITE_TARGET,
        *canonical_environment_roots,
    ]
    if macos_selector_root.is_dir():
        trusted_rna_read_roots.append(macos_selector_root)
    trusted_rna_write_roots = [
        directories["gateway_home"],
        directories["gateway_tmp"],
        directories["rna_events"],
        directories["query_events"],
        directories["trusted_rna_state"],
        TRUSTED_GIT_CONFIG_WRITE_TARGET,
    ]
    trusted_rna_read_roots = sorted(
        {path.resolve(strict=True) for path in trusted_rna_read_roots}
    )
    trusted_rna_write_roots = sorted(
        {path.resolve(strict=True) for path in trusted_rna_write_roots}
    )
    treatment_state_path = (
        directories["trusted_rna_state"] / "supervisor-state.json"
    )
    treatment_lock_path = directories["trusted_rna_state"] / "supervisor.lock"
    validate_trusted_rna_write_scope(
        write_roots=trusted_rna_write_roots,
        required_paths=[
            treatment_state_path,
            treatment_lock_path,
            directories["rna_events"],
            directories["query_events"],
            TRUSTED_GIT_CONFIG_WRITE_TARGET,
        ],
        unrelated_paths=[
            episode,
            evidence,
            identity_path,
            evidence / "supervisor-config.json",
            evidence / "common-supervisor-state.json",
            evidence / "common-supervisor.lock",
            evidence / "hook-guard-state.json",
            directories["hooks"],
            directories["requests"],
            directories["claimed"],
            directories["revoked"],
            directories["receipts"],
            directories["traces"],
            directories["teardowns"],
            directories["broker_requests"],
            directories["broker_claimed"],
            directories["broker_output"],
            directories["preflight"],
            directories["gateway_docker_config"],
        ],
    )
    forbidden_trusted_roots = [
        sibling.resolve(strict=True),
        prepared.output_root.resolve(strict=True),
        *(
            Path(path).resolve(strict=True)
            for path in credential_fragments
        ),
    ]
    isolation.validate_trusted_rna_root_separation(
        allowed_roots=[
            *trusted_rna_read_roots,
            *trusted_rna_write_roots,
        ],
        forbidden_roots=forbidden_trusted_roots,
    )
    trusted_rna_profile = isolation.generate_trusted_rna_seatbelt_profile(
        read_roots=trusted_rna_read_roots,
        write_roots=trusted_rna_write_roots,
    )
    trusted_rna_profile_path = isolation_root / "trusted-rna.sb"
    atomic_write(
        trusted_rna_profile_path,
        trusted_rna_profile.encode("utf-8"),
    )
    trusted_rna_profile_path.chmod(0o444)

    replacements = {
        "__CONTROL_OR_TREATMENT__": "control" if arm == "A" else "treatment",
        "__PINNED_RNA_LAUNCHER__": str(prepared.launcher_path),
        "__PINNED_RNA_BINARY__": str(prepared.binary_path),
        "__IMMUTABLE_INDEX_CHECKOUT__": str(case.index_checkout),
        "__EDITABLE_CHECKOUT__": str(case.checkouts[arm]),
        "__RNA_ROOT__": case.root,
        "__CANONICAL_REPOSITORY_IDENTITY__": case.expected_repository_identity,
        "__HARNESS_RESPONSE__": str(evidence / "query/projection.stdout"),
        "__STABLE_ID__": "",
        "__TRAVERSAL_WRAPPER__": str(wrapper),
        "__QUERY_WRAPPER__": str(query_wrapper),
        "__HARNESS_ROOT__": str(harness_paths["harness"]),
        "__EPISODE_EVIDENCE_ROOT__": str(evidence),
        "__SUPERVISOR_STATE__": str(treatment_state_path),
        "__COMMON_SUPERVISOR_STATE__": str(evidence / "common-supervisor-state.json"),
        "__SUPERVISOR_LOCK__": str(treatment_lock_path),
        "__COMMON_SUPERVISOR_LOCK__": str(evidence / "common-supervisor.lock"),
        "__HOOK_LEDGER__": str(evidence / "hooks/treatment-events.jsonl"),
        "__COMMON_HOOK_LEDGER__": str(evidence / "hooks/common-events.jsonl"),
        "__PINNED_HOOK_GUARD__": str(harness_paths["hook_guard.py"]),
        "__PINNED_HOOK_GUARD_SHA256__": sha_file(harness_paths["hook_guard.py"]),
        "__HOOK_GUARD_STATE__": str(evidence / "hook-guard-state.json"),
        "__HOOK_GUARD_LEDGER__": str(evidence / "hooks/hook-guard-events.jsonl"),
        "__RNA_EVENTS_DIRECTORY__": str(evidence / "rna-events"),
        "__QUERY_EVENTS_DIRECTORY__": str(evidence / "query"),
        "__RUNTIME_IDENTITY_RECEIPT__": str(identity_path),
        "__RUNTIME_IDENTITY_SHA256__": identity_sha,
        "__BASE_COMMIT__": case.base_commit,
        "__BASE_TREE__": case.base_tree,
        "__PRODUCER_COMMIT__": prepared.registration["rna_artifact"]["producer_commit"],
        "__CACHE_MANIFEST_SHA256__": case.cache_refs["manifest"]["sha256"],
        "__CACHE_ARCHIVE_SHA256__": case.cache_refs["archive"]["sha256"],
        "__OPERATIONAL_CACHE_INVENTORY_SHA256__": case.cache_inventory_sha256,
        "__LAUNCHER_SHA256__": prepared.rna_refs["launcher"]["sha256"],
        "__BINARY_SHA256__": prepared.rna_refs["binary"]["sha256"],
        "__CANONICAL_ENVIRONMENT_SHA256__": prepared.rna_refs["canonical_environment"]["sha256"],
        "__TITLE_QUERY_SHA256__": sha_bytes(case.title),
        "__PRIVATE_EPISODE_TMP__": str(model_private),
        "__DECLARED_TOOLCHAIN_ROOT__": str(prepared.isolation_host["declared_toolchain_root"]),
        "__PINNED_GATEWAY_PYTHON__": str(prepared.isolation_host["gateway_python"]),
        "__PINNED_GATEWAY_PYTHON_SHA256__": fixed["gateway_python_sha256"],
        "__PINNED_GIT_BINARY__": str(prepared.isolation_host["git_binary"]),
        "__PINNED_GIT_BINARY_SHA256__": fixed["git_binary_sha256"],
        "__PINNED_BASH_GATEWAY__": str(harness_paths["bash_gateway.py"]),
        "__PINNED_BASH_GATEWAY_SHA256__": sha_file(harness_paths["bash_gateway.py"]),
        "__PINNED_TRUSTED_RNA_BROKER__": str(
            harness_paths["trusted_rna_broker.py"]
        ),
        "__PINNED_TRUSTED_RNA_BROKER_SHA256__": sha_file(
            harness_paths["trusted_rna_broker.py"]
        ),
        "__PINNED_ISOLATION_MODULE_SHA256__": sha_file(harness_paths["isolation.py"]),
        "__PINNED_COMMON_SUPERVISOR_SHA256__": sha_file(harness_paths["common_supervisor.py"]),
        "__PINNED_TOOL_SUPERVISOR_SHA256__": sha_file(harness_paths["tool_supervisor.py"]),
        "__SUPERVISOR_CONFIG__": str(harness_paths["config"]),
        "__GATEWAY_REQUEST_DIRECTORY__": str(directories["requests"]),
        "__GATEWAY_CLAIMED_DIRECTORY__": str(directories["claimed"]),
        "__GATEWAY_REVOKED_DIRECTORY__": str(directories["revoked"]),
        "__GATEWAY_RECEIPT_DIRECTORY__": str(directories["receipts"]),
        "__GATEWAY_TRACE_DIRECTORY__": str(directories["traces"]),
        "__GATEWAY_TEARDOWN_DIRECTORY__": str(directories["teardowns"]),
        "__TRUSTED_RNA_BROKER_REQUEST_DIRECTORY__": str(
            directories["broker_requests"]
        ),
        "__TRUSTED_RNA_BROKER_CLAIMED_DIRECTORY__": str(
            directories["broker_claimed"]
        ),
        "__TRUSTED_RNA_BROKER_OUTPUT_DIRECTORY__": str(
            directories["broker_output"]
        ),
        "__TRUSTED_RNA_BROKER_READY__": str(
            isolation_root / "trusted-rna-broker/ready.json"
        ),
        "__TRUSTED_RNA_BROKER_STOP__": str(
            isolation_root / "trusted-rna-broker/stop.json"
        ),
        "__TRUSTED_RNA_BROKER_TEARDOWN__": str(
            isolation_root / "trusted-rna-broker/teardown.json"
        ),
        "__ISOLATION_LEDGER__": str(isolation_root / "isolation-events.jsonl"),
        "__HARNESS_MATERIALIZATION__": str(harness_paths["materialization"]),
        "__SEATBELT_PROFILE__": str(seatbelt_path),
        "__SEATBELT_PROFILE_SHA256__": sha_file(seatbelt_path),
        "__TRUSTED_RNA_SEATBELT_PROFILE__": str(trusted_rna_profile_path),
        "__TRUSTED_RNA_SEATBELT_PROFILE_SHA256__": sha_file(
            trusted_rna_profile_path
        ),
        "__TRUSTED_RNA_ENVIRONMENT__": str(trusted_rna_environment),
        "__TRUSTED_RNA_ENVIRONMENT_SHA256__": (
            prepared.rna_refs["canonical_environment"]["sha256"]
        ),
        "__PINNED_SANDBOX_EXEC__": str(prepared.isolation_host["sandbox_exec"]),
        "__PINNED_SANDBOX_EXEC_SHA256__": fixed["sandbox_exec_sha256"],
        "__PRIVATE_TREE_AUDITS__": str(private_audits_path),
        "__PINNED_DOCKER_BINARY__": str(prepared.isolation_host["docker_binary"]),
        "__PINNED_DOCKER_BINARY_SHA256__": fixed["docker_binary_sha256"],
        "__PINNED_DOCKER_SERVER__": fixed["docker_server"],
        "__GATEWAY_HOST_PATH__": "/usr/bin:/bin:/usr/sbin:/sbin",
        "__GATEWAY_PRIVATE_HOME__": str(directories["gateway_home"]),
        "__GATEWAY_PRIVATE_TMP__": str(directories["gateway_tmp"]),
        "__PINNED_DOCKER_HOST__": prepared.isolation_host["docker_host"],
        "__GATEWAY_PRIVATE_DOCKER_CONFIG__": str(directories["gateway_docker_config"]),
        "__TRUSTED_RNA_PATH__": "/usr/bin:/bin:/usr/sbin:/sbin",
        "__CASE_IMAGE_AT_SHA256__": case.isolation_worker["image"],
        "__CASE_IMAGE_MANIFEST_SHA256__": case.isolation_worker["image_manifest"]["sha256"],
        "__CASE_IMAGE_MANIFEST__": case.isolation_worker["image_manifest"]["path"],
        "__CASE_WORKER_PREFLIGHT__": case.isolation_worker["preflight_receipt"]["path"],
        "__CASE_LIVE_IMAGE_INSPECT_SHA256__": case.isolation_worker["live_image_inspect_sha256"],
        "__CASE_LIVE_WORKER_SELF_TEST__": str(live_self_test_path),
        "__OFFLINE_WORKER_SHA256__": fixed["worker_entrypoint_sha256"],
        "__STRACE_ARTIFACT_SHA256__": fixed["strace_artifact_sha256"],
        "__SIBLING_ARM_PATH_FRAGMENT__": str(sibling),
        "__SHARED_EVIDENCE_PATH_FRAGMENT__": str(prepared.output_root),
        "__IMMUTABLE_INDEX_PATH_FRAGMENT__": str(case.index_checkout),
        "__CREDENTIAL_PATH_FRAGMENT__": (
            credential_fragments[0] if credential_fragments else "/__no_provider_state__"
        ),
    }
    config_template = read_json(SOURCE / "supervisor.template.json")
    config = render_template(config_template, replacements)
    config["initial_ids"] = []
    config["trusted_rna_read_roots"] = [
        str(path) for path in trusted_rna_read_roots
    ]
    config["trusted_rna_write_roots"] = [
        str(path) for path in trusted_rna_write_roots
    ]
    bind_trusted_rna_git(
        config,
        git_binary=prepared.isolation_host["git_binary"],
        git_binary_sha256=fixed["git_binary_sha256"],
        canonical_environment=canonical_trusted_environment,
    )
    config["worker_entrypoint"] = fixed["worker_entrypoint"]
    config["strace_path"] = fixed["strace_path"]
    for key in (
        "gateway_tool_timeout_ms",
        "worker_timeout_seconds",
        "trusted_rna_timeout_seconds",
        "docker_control_timeout_seconds",
        "worker_landlock_abi_min",
        "worker_uid",
        "worker_gid",
        "worker_pids_limit",
        "worker_memory_bytes",
        "worker_cpus",
        "worker_env",
        "trace_allowed_path_prefixes",
    ):
        config[key] = fixed[key]
    config["trusted_rna_broker_client_timeout_seconds"] = (
        float(fixed["trusted_rna_timeout_seconds"]) + 5.0
    )
    config["trace_forbidden_path_fragments"] = [
        *fixed["trace_forbidden_static_fragments"],
        str(sibling),
        str(prepared.output_root),
        str(case.index_checkout),
        *credential_fragments,
    ]
    config["worker_image_preflight_verified"] = True
    config["worker_landlock_required"] = True
    config["worker_landlock_preflight_verified"] = True
    require_fully_rendered(config, "supervisor config")
    isolation.validate_worker_config(config)

    preflight_request = directories["preflight"] / "request.json"
    atomic_write(preflight_request, canonical({"schema_version": "issue827-worker-argv-preflight-v1"}))
    preflight_trace = directories["preflight"] / "trace"
    preflight_trace.mkdir(mode=0o700)
    preflight_argv = isolation.build_docker_worker_argv(
        config=config,
        request_path=preflight_request,
        trace_directory=preflight_trace,
        container_name="rna827-" + "0" * 32,
    )
    worker_preflight_path = isolation_root / "worker-config-preflight.json"
    atomic_write(
        worker_preflight_path,
        canonical(
            {
                "schema_version": "issue827-worker-config-preflight-v1",
                "validated": True,
                "argv": preflight_argv,
                "argv_sha256": sha_bytes(canonical(preflight_argv)),
                "live_self_test": file_ref(live_self_test_path),
                "private_tree_audits": file_ref(private_audits_path),
            }
        ),
    )
    config["worker_config_preflight"] = str(worker_preflight_path)

    atomic_write(harness_paths["config"], canonical(config))
    snapshot = evidence / "supervisor-config.json"
    atomic_write(snapshot, canonical(config))

    settings_template = read_json(SOURCE / "claude-settings.template.json")
    guard_base = [
        str(prepared.isolation_host["gateway_python"]),
        str(harness_paths["hook_guard.py"]),
        "--config",
        str(harness_paths["config"]),
        "--evidence-root",
        str(evidence),
    ]
    common_guard = shlex.join(
        [
            *guard_base,
            "--child",
            str(harness_paths["common_supervisor.py"]),
            "--child-sha256",
            sha_file(harness_paths["common_supervisor.py"]),
            "--role",
            "common",
            "--timeout-ms",
            "3500",
        ]
    )
    treatment_guard = shlex.join(
        [
            *guard_base,
            "--child",
            str(harness_paths["tool_supervisor.py"]),
            "--child-sha256",
            sha_file(harness_paths["tool_supervisor.py"]),
            "--role",
            "treatment",
            "--timeout-ms",
            "3500",
        ]
    )
    settings = render_template(
        settings_template,
        {
            "__COMMON_HOOK_GUARD_COMMAND__": common_guard,
            "__TOOL_HOOK_GUARD_COMMAND__": treatment_guard,
            "__GATEWAY_PYTHON__": str(prepared.isolation_host["gateway_python"]),
            "__BASH_GATEWAY__": str(harness_paths["bash_gateway.py"]),
            "__SUPERVISOR_CONFIG__": str(harness_paths["config"]),
            "__EDITABLE_CHECKOUT__": str(case.checkouts[arm]),
            "__PRIVATE_EPISODE_TMP__": str(model_private),
            "__DECLARED_TOOLCHAIN_ROOT__": str(prepared.isolation_host["declared_toolchain_root"]),
        },
    )
    require_fully_rendered(settings, "Claude settings")
    settings_path = episode / "claude-settings.json"
    atomic_write(settings_path, canonical(settings))
    config["claude_settings_sha256"] = sha_file(settings_path)
    atomic_write(harness_paths["config"], canonical(config))
    atomic_write(snapshot, canonical(config))
    return episode, evidence, identity_path, settings_path, config


def acquire_treatment(
    case: PreparedCase,
    harness_paths: Mapping[str, Path],
    evidence: Path,
    config: dict[str, Any],
) -> tuple[bytes, list[str], float, dict[str, Any]]:
    wrapper_command = [
        str(harness_paths["rna_query.py"]),
        "--query-sha256",
        config["expected_query_sha256"],
    ]
    command = [
        str(config["sandbox_exec"]),
        "-f",
        str(config["trusted_rna_seatbelt_profile"]),
        str(config["gateway_python"]),
        *wrapper_command,
    ]
    trusted_environment = {
        str(name): str(value)
        for name, value in config["trusted_rna_env"].items()
    }
    require(
        all(
            not isolation.is_secret_env_name(name)
            for name in trusted_environment
        ),
        "trusted RNA acquisition environment contains credential material",
    )
    started_at = utc_now()
    started = time.monotonic()
    process_started = True
    timed_out = False
    try:
        result = subprocess.run(
            command,
            cwd=str(config["checkout"]),
            env=trusted_environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=float(config["trusted_rna_timeout_seconds"]),
        )
    except subprocess.TimeoutExpired as exc:
        timed_out = True
        result = subprocess.CompletedProcess(
            command,
            124,
            exc.stdout or b"",
            exc.stderr or b"",
        )
    except OSError as exc:
        process_started = False
        result = subprocess.CompletedProcess(command, 126, b"", str(exc).encode())
    elapsed = time.monotonic() - started
    projection_path = evidence / "query/projection.stdout"
    wrapper_stderr = evidence / "query/wrapper.stderr"
    atomic_write(projection_path, result.stdout)
    atomic_write(wrapper_stderr, result.stderr)
    receipt_path = evidence / "query/title-query.json"
    query_evidence = {
        "schema_version": QUERY_EVIDENCE_SCHEMA,
        "acquisition_started": True,
        "process_started": process_started,
        "started_at": started_at,
        "succeeded": False,
        "failure": None,
        "wrapper_command": command,
        "requested_wrapper_command": wrapper_command,
        "wrapper_returncode": result.returncode,
        "timed_out": timed_out,
        "trusted_rna_confinement": {
            "execution_plane": "top_level_trusted_rna_seatbelt",
            "sandbox_exec": file_ref(Path(str(config["sandbox_exec"]))),
            "seatbelt_profile": file_ref(
                Path(str(config["trusted_rna_seatbelt_profile"]))
            ),
            "canonical_environment": file_ref(
                Path(str(config["trusted_rna_environment"]))
            ),
            "git_binary": file_ref(Path(str(config["git_binary"]))),
            "git_config_global_write_target": str(
                TRUSTED_GIT_CONFIG_WRITE_TARGET
            ),
            "process_environment_sha256": sha_bytes(
                canonical(trusted_environment)
            ),
            "network_inbound": "denied",
            "network_outbound": "denied",
            "read_roots": list(config["trusted_rna_read_roots"]),
            "write_roots": list(config["trusted_rna_write_roots"]),
        },
        "wrapper_stdout": file_ref(projection_path),
        "wrapper_stderr": file_ref(wrapper_stderr),
        "raw_receipt": file_ref(receipt_path) if receipt_path.is_file() else None,
        "raw_stdout": None,
        "raw_stderr": None,
        "projected_stable_code_ids": [],
        "raw_stable_code_ids": [],
        "projection_authorization_sha256": None,
        "elapsed_seconds": elapsed,
    }
    try:
        receipt: Mapping[str, Any] | None = None
        if receipt_path.is_file():
            loaded_receipt = read_json(receipt_path)
            require(isinstance(loaded_receipt, dict), "query raw receipt must be an object")
            receipt = loaded_receipt
            query_evidence["raw_stdout"] = receipt.get("stdout")
            query_evidence["raw_stderr"] = receipt.get("stderr")
            query_evidence["raw_stable_code_ids"] = receipt.get(
                "raw_stable_code_ids_observational_only"
            )
        require(process_started, f"{case.case_id} exact-title RNA query did not start")
        require(not timed_out, f"{case.case_id} exact-title RNA query timed out")
        require(result.returncode == 0, f"{case.case_id} exact-title RNA query failed: {result.stderr.decode(errors='replace')}")
        require(receipt is not None, f"{case.case_id} query wrapper did not retain raw receipt")
        require(receipt.get("identity_sha256") == config["expected_identity_sha256"], "query identity mismatch")
        require(receipt.get("root") == case.root, "query root mismatch")
        require(receipt.get("returncode") == 0, "query raw launcher failed")
        text = result.stdout.decode("utf-8", errors="strict")
        require(READY_SENTINEL in text, "query projection missing exact READY sentinel")
        ids = stable_code_ids(text)
        require(ids, "query projection returned no stable code IDs")
        expected_authorization_source = frontier_replay.source(
            0,
            "injected_query_projection",
            "INJECTED_QUERY",
            result.stdout,
            result.stdout,
        )
        projection_authorization = receipt.get("projection_authorization")
        projection_authorization_sha256 = receipt.get(
            "projection_authorization_sha256"
        )
        require(
            isinstance(projection_authorization, dict),
            "query raw receipt missing projection authorization",
        )
        require(
            projection_authorization
            == expected_authorization_source["projection_authorization"],
            "query projection authorization does not match injected response",
        )
        require(
            projection_authorization_sha256
            == expected_authorization_source[
                "projection_authorization_sha256"
            ],
            "query projection authorization hash mismatch",
        )
        require(
            projection_authorization.get("stable_code_ids") == ids,
            "query projection authorization stable IDs mismatch",
        )
        require(
            all(f"`{item}`".encode() in result.stdout for item in ids),
            "query authorized an ID absent from injected response bytes",
        )
        query_evidence["projected_stable_code_ids"] = ids
        query_evidence["projection_authorization_sha256"] = (
            projection_authorization_sha256
        )
        config["initial_ids"] = ids
        config["initial_response"] = str(projection_path)
        config["initial_response_sha256"] = sha_bytes(result.stdout)
        config["initial_authorization_sha256"] = (
            projection_authorization_sha256
        )
        atomic_write(harness_paths["config"], canonical(config))
        atomic_write(evidence / "supervisor-config.json", canonical(config))
        prefix = (SOURCE / "system-prefix.txt").read_bytes()
        suffix = (SOURCE / "system-suffix.txt").read_bytes().replace(b"__TRAVERSAL_WRAPPER__", str(harness_paths["rna_traverse.py"]).encode())
        opaque_call = f"rna_query --query-sha256 {config['expected_query_sha256']}\n".encode()
        system = prefix + opaque_call + b"\nRNA TOOL RESPONSE\n" + result.stdout + suffix
    except (FailClosed, OSError, UnicodeError, json.JSONDecodeError) as exc:
        query_evidence["failure"] = f"{type(exc).__name__}:{exc}"
        raise TreatmentAcquisitionFailure(str(exc), query_evidence, elapsed) from exc
    query_evidence["succeeded"] = True
    return system, ids, elapsed, query_evidence


def claude_command(
    prepared: PreparedRun,
    session: str,
    settings: Path,
    treatment_system: Path | None,
    seatbelt_profile: Path | None = None,
) -> list[str]:
    runtime = prepared.registration["model_runtime"]
    claude = [
        str(prepared.claude_path), "-p", "--strict-mcp-config", "--mcp-config", str(prepared.mcp_path),
        "--model", runtime["model"], "--effort", runtime["effort"],
        "--permission-mode", runtime["permission_mode"],
        "--tools", ",".join(runtime["tools"]),
        "--disallowed-tools", ",".join(runtime["disallowed_tools"]),
        "--max-budget-usd", str(runtime["budget_usd"]),
        "--output-format", "json", "--session-id", session,
        "--settings", str(settings),
    ]
    if treatment_system is not None:
        claude.extend(["--append-system-prompt-file", str(treatment_system)])
    if seatbelt_profile is not None:
        require(hasattr(prepared, "isolation_host"), "production run missing isolation host")
        require(seatbelt_profile.is_file() and not seatbelt_profile.is_symlink(), "Seatbelt profile invalid")
        command = [
            str(prepared.isolation_host["sandbox_exec"]),
            "-f",
            str(seatbelt_profile),
            *claude,
        ]
    else:
        command = claude
    require("--safe-mode" not in command and "--resume" not in command, "forbidden Claude mode")
    return command


def provider_parent_env(model_private: Path) -> dict[str, str]:
    """Keep provider authentication while confining all Claude temporary state.

    The model never receives this environment: Bash is replaced by the gateway,
    whose trusted-RNA and Docker planes each construct an explicit scrubbed
    environment.  Native tools are independently effective-path checked.
    """

    private_tmp = (model_private / "tmp").resolve(strict=True)
    env = dict(os.environ)
    for name in (
        "TMPDIR",
        "CLAUDE_TMPDIR",
        "CLAUDE_CODE_TMPDIR",
        "BUN_TMPDIR",
    ):
        env[name] = str(private_tmp)
    return env


def start_trusted_rna_broker(
    prepared: PreparedRun,
    config: Mapping[str, Any],
    evidence: Path,
) -> dict[str, Any]:
    """Start the credential-free broker before entering Claude's Seatbelt."""

    config_path = Path(str(config["gateway_config"]))
    config_sha256 = sha_file(config_path)
    stdout_path = evidence / "isolation/trusted-rna-broker.stdout"
    stderr_path = evidence / "isolation/trusted-rna-broker.stderr"
    command = [
        str(prepared.isolation_host["gateway_python"]),
        str(config["trusted_rna_broker"]),
        "--config",
        str(config_path),
        "--config-sha256",
        config_sha256,
    ]
    environment = {
        str(name): str(value)
        for name, value in config["trusted_rna_env"].items()
    }
    require(
        all(
            not isolation.is_secret_env_name(name)
            for name in environment
        ),
        "trusted RNA broker environment contains credential material",
    )
    with stdout_path.open("xb") as stdout_handle, stderr_path.open(
        "xb"
    ) as stderr_handle:
        process = subprocess.Popen(
            command,
            cwd=str(config["checkout"]),
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=stdout_handle,
            stderr=stderr_handle,
            start_new_session=True,
        )
    try:
        ready_path = Path(str(config["trusted_rna_broker_ready"]))
        deadline = time.monotonic() + 5.0
        while not ready_path.exists():
            returncode = process.poll()
            if returncode is not None:
                raise FailClosed(
                    f"trusted RNA broker exited before ready: {returncode}"
                )
            if time.monotonic() >= deadline:
                raise FailClosed("trusted RNA broker readiness timeout")
            time.sleep(0.02)
        require(
            ready_path.is_file() and not ready_path.is_symlink(),
            "trusted RNA broker ready receipt invalid",
        )
        ready = read_json(ready_path)
        body = dict(ready)
        observed = body.pop("receipt_sha256", None)
        injected_environment = ready.get("os_injected_environment")
        effective_environment = (
            {**environment, **injected_environment}
            if isinstance(injected_environment, dict)
            else {}
        )
        require(
            ready.get("schema_version") == isolation.BROKER_READY_SCHEMA
            and ready.get("config_sha256") == config_sha256
            and ready.get("pid") == process.pid
            and ready.get("provider_environment_inherited") is False
            and ready.get("credential_environment_names") == []
            and isinstance(injected_environment, dict)
            and set(injected_environment).issubset(
                isolation.BROKER_OS_INJECTED_ENV_NAMES
            )
            and all(
                isinstance(item, str)
                and "\x00" not in item
                and "\n" not in item
                for item in injected_environment.values()
            )
            and ready.get("canonical_environment_sha256")
            == sha_bytes(canonical(environment))
            and ready.get("process_environment_sha256")
            == sha_bytes(canonical(effective_environment))
            and ready.get("environment_names") == sorted(effective_environment)
            and ready.get("broker")
            == {
                "path": str(config["trusted_rna_broker"]),
                "sha256": str(config["trusted_rna_broker_sha256"]),
            }
            and observed == sha_bytes(canonical(body)),
            "trusted RNA broker ready receipt contract mismatch",
        )
    except Exception:
        terminate_group(process)
        raise
    return {
        "process": process,
        "command": command,
        "config_sha256": config_sha256,
        "ready": file_ref(ready_path),
        "stdout_path": stdout_path,
        "stderr_path": stderr_path,
    }


def stop_trusted_rna_broker(
    runtime: Mapping[str, Any],
    config: Mapping[str, Any],
) -> dict[str, Any]:
    """Request bounded broker shutdown and require a clean hashed teardown."""

    process = runtime["process"]
    require(
        isinstance(process, subprocess.Popen),
        "trusted RNA broker process handle invalid",
    )
    stop_path = Path(str(config["trusted_rna_broker_stop"]))
    atomic_write(
        stop_path,
        canonical(
            {
                "schema_version": isolation.BROKER_STOP_SCHEMA,
                "config_sha256": runtime["config_sha256"],
            }
        ),
    )
    stop_path.chmod(0o600)
    try:
        returncode = process.wait(
            timeout=float(config["trusted_rna_broker_client_timeout_seconds"])
        )
    except subprocess.TimeoutExpired as exc:
        terminate_group(process)
        raise FailClosed("trusted RNA broker teardown timeout") from exc
    require(returncode == 0, f"trusted RNA broker exit {returncode}")
    teardown_path = Path(str(config["trusted_rna_broker_teardown"]))
    require(
        teardown_path.is_file() and not teardown_path.is_symlink(),
        "trusted RNA broker teardown receipt missing",
    )
    teardown = read_json(teardown_path)
    body = dict(teardown)
    observed = body.pop("receipt_sha256", None)
    require(
        teardown.get("schema_version") == isolation.BROKER_TEARDOWN_SCHEMA
        and teardown.get("config_sha256") == runtime["config_sha256"]
        and teardown.get("pid") == process.pid
        and teardown.get("pending") == []
        and teardown.get("active_child") is False
        and teardown.get("fatal") is None
        and teardown.get("clean") is True
        and observed == sha_bytes(canonical(body)),
        "trusted RNA broker teardown contract mismatch",
    )
    return {
        "ready": runtime["ready"],
        "teardown": file_ref(teardown_path),
        "stdout": file_ref(Path(str(runtime["stdout_path"]))),
        "stderr": file_ref(Path(str(runtime["stderr_path"]))),
        "returncode": returncode,
    }


def terminate_group(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def state_fatal(path: Path) -> bool:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError):
        return False
    return value.get("fatal") is True


def copy_transcripts(session: str, evidence: Path) -> list[dict[str, Any]]:
    destinations: list[dict[str, Any]] = []
    target = evidence / "transcripts"
    for index, source in enumerate(find_transcripts(session), 1):
        target.mkdir(parents=True, exist_ok=True)
        destination = target / f"{index:02d}-{source.name}"
        require(not destination.exists(), f"transcript destination exists: {destination}")
        shutil.copyfile(source, destination)
        destinations.append({"source_path": str(source), "retained": file_ref(destination)})
    return destinations


def transcript_model_events(
    transcripts: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]] | None:
    """Return one observed usage event per unique provider assistant message."""
    messages: dict[str, dict[str, Any]] = {}
    if not transcripts:
        return None
    try:
        for transcript in transcripts:
            retained = transcript.get("retained")
            path, data = check_ref(retained, "retained transcript")
            del path
            for line in data.splitlines():
                if not line:
                    continue
                event = json.loads(line)
                if not isinstance(event, dict) or event.get("type") != "assistant":
                    continue
                message = event.get("message")
                if not isinstance(message, dict) or message.get("role") != "assistant":
                    return None
                message_id = message.get("id")
                if not isinstance(message_id, str) or not message_id:
                    return None
                usage = message.get("usage")
                if not isinstance(usage, dict):
                    return None
                observed = {"message_id": message_id, "usage": usage}
                previous = messages.get(message_id)
                if previous is not None and previous != observed:
                    return None
                messages[message_id] = observed
    except (FailClosed, OSError, json.JSONDecodeError):
        return None
    return list(messages.values()) if messages else None


def transcript_provider_response_count(
    transcripts: Sequence[Mapping[str, Any]],
) -> int | None:
    events = transcript_model_events(transcripts)
    return len(events) if events is not None else None


def build_actor_tool_ledger(
    arm: str,
    common_hooks: list[dict[str, Any]],
    treatment_hooks: list[dict[str, Any]],
    query: dict[str, Any] | None,
    authorization_requested: bool,
) -> dict[str, Any]:
    actions: list[dict[str, Any]] = []
    sequence = 0
    if query is not None:
        sequence += 1
        actions.append({
            "sequence": sequence,
            "actor": "harness",
            "action": "rna_exact_title_query",
            "tool": "RNA CLI",
            "elapsed_seconds": query["elapsed_seconds"],
            "raw_receipt_sha256": query["raw_receipt"]["sha256"],
        })
    common_pre = [item for item in common_hooks if item.get("event", {}).get("hook_event_name") == "PreToolUse"]
    treatment_pre = [item for item in treatment_hooks if item.get("event", {}).get("hook_event_name") == "PreToolUse"]
    for index, pair in enumerate(zip_longest(common_pre, treatment_pre, fillvalue={}), 1):
        common, treatment = pair
        common_event = common.get("event")
        treatment_event = treatment.get("event")
        event = common_event or treatment_event or {}
        sequence += 1
        tool_input = event.get("tool_input") or {}
        command = tool_input.get("command") if event.get("tool_name") == "Bash" else None
        actions.append({
            "sequence": sequence,
            "actor": "model",
            "action": "tool_attempt",
            "model_action_index": index,
            "tool": event.get("tool_name"),
            "tool_input": tool_input,
            "bash_command": command,
            "common_decision": common.get("decision"),
            "treatment_decision": treatment.get("decision"),
            "hook_events_equal": common_event == treatment_event,
            "looks_like_visible_test": bool(command and re.search(r"(?:^|\s)(?:pytest|cargo\s+test|python\s+-m\s+(?:pytest|unittest)|tox|npm\s+test|go\s+test)(?:\s|$)", command)),
        })
    sequence += 1
    actions.append({
        "sequence": sequence,
        "actor": "harness",
        "action": "official_evaluator_authorization_request",
        "requested": authorization_requested,
        "authorized": False,
        "invoked": False,
    })
    counts: dict[str, int] = {}
    for action in actions:
        if action.get("actor") == "model" and isinstance(action.get("tool"), str):
            counts[action["tool"]] = counts.get(action["tool"], 0) + 1
    return {
        "schema_version": "issue827-actor-tool-ledger-v1",
        "arm": arm,
        "actions": actions,
        "model_tool_counts": dict(sorted(counts.items())),
        "visible_test_tool_attempts": sum(bool(action.get("looks_like_visible_test")) for action in actions),
    }


def treatment_compliance(config: Mapping[str, Any], evidence: Path, arm: str) -> tuple[bool, list[str]]:
    errors: list[str] = []
    common_hooks = load_jsonl(Path(config["common_hook_ledger"]))
    treatment_hooks = load_jsonl(Path(config["hook_ledger"]))
    if any(item.get("decision") == "deny" for item in common_hooks):
        errors.append("common_supervisor_denial")
    if arm == "A":
        if any(item.get("decision") == "deny" for item in treatment_hooks):
            errors.append("control_supervisor_denial")
        if list((evidence / "rna-events").glob("*.json")):
            errors.append("control_has_rna_events")
        return not errors, errors

    pre = [item for item in treatment_hooks if item.get("event", {}).get("hook_event_name") == "PreToolUse"]
    if not pre:
        errors.append("missing_first_model_tool")
    else:
        first = pre[0]
        if first.get("decision") != "allow":
            errors.append("first_tool_not_allowed")
        event = first.get("event") or {}
        command = (event.get("tool_input") or {}).get("command")
        try:
            import shlex
            argv = shlex.split(command) if isinstance(command, str) else []
        except ValueError:
            argv = []
        exact = (
            event.get("tool_name") == "Bash"
            and isinstance(command, str)
            and not any(token in command for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n"))
            and len(argv) == 5
            and argv[0] == config["wrapper"]
            and argv[1] == "--node"
            and argv[2] in config["initial_ids"]
            and argv[3:] == ["--mode", "neighbors"]
        )
        if not exact:
            errors.append("first_tool_not_exact_injected_rna_neighbors")
    state_path = Path(config["state"])
    state = read_json(state_path) if state_path.is_file() else {}
    if state.get("fatal"):
        errors.append("fatal_rna_state")
    if state.get("first_traversal_succeeded") is not True or state.get("first_traversal_status") not in {"OK_NONEMPTY", "OK_EMPTY"}:
        errors.append("first_traversal_not_verified")
    receipts = sorted((evidence / "rna-events").glob("*.json"))
    if not receipts:
        errors.append("missing_rna_receipt")
    else:
        first_receipt = read_json(receipts[0])
        if first_receipt.get("classification") not in {"OK_NONEMPTY", "OK_EMPTY"}:
            errors.append("first_rna_classification_invalid")
        if first_receipt.get("identity_sha256") != config["expected_identity_sha256"]:
            errors.append("first_rna_identity_mismatch")
        if first_receipt.get("root") != config["root"]:
            errors.append("first_rna_root_mismatch")
        try:
            initial_projection = Path(str(config["initial_response"])).read_bytes()
            loaded_receipts = [read_json(path) for path in receipts]
            replayed = replay_treatment_frontier(
                initial_projection,
                loaded_receipts,
                evidence / "rna-events",
            )
            if (
                state.get("authorization_frontier") != replayed
                or state.get("rna_calls") != len(loaded_receipts)
            ):
                errors.append("rna_authorization_frontier_state_mismatch")
        except (FailClosed, OSError, frontier_replay.FrontierReplayError) as exc:
            errors.append(f"rna_authorization_frontier_replay:{exc}")
    if any(item.get("decision") == "deny" for item in treatment_hooks):
        errors.append("treatment_supervisor_denial")
    return not errors, errors


def replay_treatment_frontier(
    initial_projection: bytes,
    receipts: Sequence[Mapping[str, Any]],
    event_directory: Path,
) -> dict[str, Any]:
    event_directory = event_directory.resolve(strict=True)

    def load_projection(reference: Mapping[str, Any], where: str) -> bytes:
        try:
            sequence = int(where.rsplit("_", 1)[-1])
        except ValueError as exc:
            raise frontier_replay.FrontierReplayError(
                f"{where}:sequence"
            ) from exc
        path, data = check_ref(reference, f"{where}.model_visible_projection")
        require(
            path.resolve(strict=True)
            == event_directory / f"{sequence:04d}.projection",
            f"{where} projection path mismatch",
        )
        return data

    return frontier_replay.replay(
        initial_projection,
        receipts,
        load_projection,
    )


def isolation_compliance(
    config: Mapping[str, Any],
    common_hooks: Sequence[Mapping[str, Any]],
    treatment_hooks: Sequence[Mapping[str, Any]],
) -> tuple[bool, list[str]]:
    errors: list[str] = []
    broker_config_sha256 = sha_file(Path(str(config["gateway_config"])))
    broker_ready: dict[str, Any] | None = None
    broker_teardown: dict[str, Any] | None = None
    try:
        broker_ready = read_json(Path(str(config["trusted_rna_broker_ready"])))
        ready_body = dict(broker_ready)
        ready_hash = ready_body.pop("receipt_sha256", None)
        broker_injected = broker_ready.get("os_injected_environment")
        canonical_broker_environment = config.get("trusted_rna_env")
        effective_broker_environment = (
            {
                **canonical_broker_environment,
                **broker_injected,
            }
            if isinstance(canonical_broker_environment, dict)
            and isinstance(broker_injected, dict)
            else {}
        )
        if (
            broker_ready.get("schema_version") != isolation.BROKER_READY_SCHEMA
            or broker_ready.get("config_sha256") != broker_config_sha256
            or broker_ready.get("provider_environment_inherited") is not False
            or broker_ready.get("credential_environment_names") != []
            or not isinstance(broker_injected, dict)
            or not set(broker_injected).issubset(
                isolation.BROKER_OS_INJECTED_ENV_NAMES
            )
            or broker_ready.get("canonical_environment_sha256")
            != sha_bytes(canonical(canonical_broker_environment))
            or broker_ready.get("process_environment_sha256")
            != sha_bytes(canonical(effective_broker_environment))
            or broker_ready.get("environment_names")
            != sorted(effective_broker_environment)
            or broker_ready.get("broker")
            != {
                "path": str(config["trusted_rna_broker"]),
                "sha256": str(config["trusted_rna_broker_sha256"]),
            }
            or ready_hash != sha_bytes(canonical(ready_body))
        ):
            errors.append("trusted_rna_broker_ready_contract")
        broker_teardown = read_json(
            Path(str(config["trusted_rna_broker_teardown"]))
        )
        teardown_body = dict(broker_teardown)
        teardown_hash = teardown_body.pop("receipt_sha256", None)
        if (
            broker_teardown.get("schema_version")
            != isolation.BROKER_TEARDOWN_SCHEMA
            or broker_teardown.get("config_sha256") != broker_config_sha256
            or broker_teardown.get("pid") != broker_ready.get("pid")
            or broker_teardown.get("pending") != []
            or broker_teardown.get("active_child") is not False
            or broker_teardown.get("fatal") is not None
            or broker_teardown.get("clean") is not True
            or teardown_hash != sha_bytes(canonical(teardown_body))
        ):
            errors.append("trusted_rna_broker_teardown_contract")
    except (FailClosed, OSError, json.JSONDecodeError) as exc:
        errors.append(f"trusted_rna_broker_lifecycle:{exc}")
    guard_state_path = Path(str(config["hook_guard_state"]))
    guard_ledger_path = Path(str(config["hook_guard_ledger"]))
    if guard_state_path.exists():
        errors.append("hook_guard_fatal_state")
    native_tool_state_path = (
        Path(str(config["episode_evidence_root"]))
        / "hooks/native-tool-state.json"
    )
    try:
        native_tool_state = read_json(native_tool_state_path)
        if native_tool_state != {
            "schema_version": "issue827-native-tool-state-v1",
            "active": {},
        }:
            errors.append("native_tool_state_not_quiescent")
    except (FailClosed, OSError, json.JSONDecodeError) as exc:
        errors.append(f"native_tool_state_invalid:{exc}")
    guard_records: list[dict[str, Any]] = []
    if guard_ledger_path.exists():
        try:
            guard_records = load_jsonl(guard_ledger_path)
            previous: str | None = None
            for index, record in enumerate(guard_records):
                if record.get("schema_version") != "issue827-hook-guard-ledger-v1":
                    raise FailClosed(f"guard record {index} schema mismatch")
                if record.get("previous_record_sha256") != previous:
                    raise FailClosed(f"guard record {index} chain mismatch")
                observed = record.get("record_sha256")
                body = {key: value for key, value in record.items() if key != "record_sha256"}
                if observed != sha_bytes(canonical(body)):
                    raise FailClosed(f"guard record {index} hash mismatch")
                if record.get("outcome") not in {"allow", "child_deny", "terminate"}:
                    raise FailClosed(f"guard record {index} outcome invalid")
                previous = observed
        except (FailClosed, OSError, json.JSONDecodeError) as exc:
            errors.append(f"hook_guard_hash_chain:{exc}")
    common_guard = [item for item in guard_records if item.get("role") == "common"]
    treatment_guard = [item for item in guard_records if item.get("role") == "treatment"]
    if len(common_guard) != len(common_hooks):
        errors.append("common_hook_guard_count_mismatch")
    if len(treatment_guard) != len(treatment_hooks):
        errors.append("treatment_hook_guard_count_mismatch")
    if any(item.get("fatal") is True for item in guard_records):
        errors.append("hook_guard_fatal_record")
    for label, path in (
        ("common", Path(str(config["common_hook_ledger"]))),
        ("treatment", Path(str(config["hook_ledger"]))),
    ):
        try:
            records = load_jsonl(path)
            isolation.verify_event_chain(records)
        except (FailClosed, OSError, isolation.IsolationViolation) as exc:
            errors.append(f"{label}_hook_hash_chain:{exc}")

    bash_pre = [
        item for item in common_hooks
        if item.get("event", {}).get("hook_event_name") == "PreToolUse"
        and item.get("event", {}).get("tool_name") == "Bash"
    ]
    request_root = Path(str(config["gateway_request_directory"]))
    claimed_root = Path(str(config["gateway_claimed_directory"]))
    revoked_root = Path(str(config["gateway_revoked_directory"]))
    receipt_root = Path(str(config["gateway_receipt_directory"]))
    trace_root = Path(str(config["gateway_trace_directory"]))
    teardown_root = Path(str(config["gateway_teardown_directory"]))
    pending = sorted(request_root.glob("*.json")) if request_root.is_dir() else []
    revoked = sorted(revoked_root.glob("*.json")) if revoked_root.is_dir() else []
    if pending:
        errors.append("unconsumed_gateway_request")
    if revoked:
        errors.append("revoked_gateway_request")

    receipts: list[dict[str, Any]] = []
    for path in sorted(receipt_root.glob("*.json")) if receipt_root.is_dir() else []:
        try:
            value = read_json(path)
        except (FailClosed, OSError, json.JSONDecodeError) as exc:
            errors.append(f"gateway_receipt_invalid:{path.name}:{exc}")
            continue
        if value.get("schema_version") != "issue827-bash-gateway-receipt-v1":
            errors.append(f"gateway_receipt_schema:{path.name}")
        if value.get("status") != "success" or value.get("violations") != []:
            errors.append(f"gateway_receipt_not_clean:{path.name}")
        if value.get("receipt_sha256") != sha_bytes(
            canonical({key: item for key, item in value.items() if key != "receipt_sha256"})
        ):
            errors.append(f"gateway_receipt_self_hash:{path.name}")
        execution = value.get("execution")
        if not isinstance(execution, dict):
            errors.append(f"gateway_execution_missing:{path.name}")
        elif value.get("execution_plane") == "offline_bash":
            trace = execution.get("trace")
            teardown = execution.get("teardown")
            if (
                not isinstance(trace, dict)
                or trace.get("complete") is not True
                or trace.get("landlock_enforced") is not True
                or trace.get("violations") != []
            ):
                errors.append(f"gateway_trace_not_clean:{path.name}")
            if (
                not isinstance(teardown, dict)
                or teardown.get("cleanup_verified") is not True
                or teardown.get("container_state") != "absent"
                or teardown.get("container_absent") is not True
                or teardown.get("process_tree_retained") is not False
                or teardown.get("primary_failure") is not None
                or teardown.get("receipt_sha256")
                != sha_bytes(
                    canonical(
                        {
                            key: item
                            for key, item in teardown.items()
                            if key != "receipt_sha256"
                        }
                    )
                )
            ):
                errors.append(f"gateway_teardown_not_clean:{path.name}")
            request_id = value.get("request_id")
            teardown_path = teardown_root / f"{request_id}.json"
            if not teardown_path.is_file() or teardown_path.is_symlink():
                errors.append(f"gateway_teardown_missing:{path.name}")
            else:
                try:
                    if read_json(teardown_path) != teardown:
                        errors.append(f"gateway_teardown_mismatch:{path.name}")
                except (OSError, json.JSONDecodeError):
                    errors.append(f"gateway_teardown_invalid:{path.name}")
            if isinstance(trace, dict):
                for member in trace.get("members", []):
                    member_path = trace_root / str(request_id) / str(member.get("name"))
                    if (
                        not member_path.is_file()
                        or member_path.is_symlink()
                        or sha_file(member_path) != member.get("sha256")
                        or member_path.stat().st_size != member.get("bytes")
                    ):
                        errors.append(f"gateway_trace_member_mismatch:{path.name}")
        elif value.get("execution_plane") == "trusted_rna":
            expected_sandbox = {
                "path": str(config["sandbox_exec"]),
                "sha256": str(config["sandbox_exec_sha256"]),
            }
            profile_path = Path(str(config["trusted_rna_seatbelt_profile"]))
            expected_profile = (
                file_ref(profile_path)
                if profile_path.is_file() and not profile_path.is_symlink()
                else None
            )
            if execution.get("sandbox_exec") != expected_sandbox:
                errors.append(f"trusted_rna_sandbox_identity:{path.name}")
            if execution.get("seatbelt_profile") != expected_profile:
                errors.append(f"trusted_rna_profile_identity:{path.name}")
            environment_path = Path(str(config["trusted_rna_environment"]))
            expected_environment = (
                file_ref(environment_path)
                if environment_path.is_file()
                and not environment_path.is_symlink()
                else None
            )
            if execution.get("canonical_environment") != expected_environment:
                errors.append(
                    f"trusted_rna_environment_identity:{path.name}"
                )
            git_path = Path(str(config["git_binary"]))
            expected_git = (
                file_ref(git_path)
                if git_path.is_file() and not git_path.is_symlink()
                else None
            )
            if execution.get("git_binary") != expected_git:
                errors.append(f"trusted_rna_git_identity:{path.name}")
            if (
                execution.get("git_config_global_write_target")
                != str(TRUSTED_GIT_CONFIG_WRITE_TARGET)
            ):
                errors.append(
                    f"trusted_rna_git_config_target:{path.name}"
                )
            if (
                execution.get("network_inbound") != "denied"
                or execution.get("network_outbound") != "denied"
            ):
                errors.append(f"trusted_rna_network_not_denied:{path.name}")
            if execution.get("read_roots") != config.get(
                "trusted_rna_read_roots"
            ):
                errors.append(f"trusted_rna_read_roots_mismatch:{path.name}")
            if execution.get("write_roots") != config.get(
                "trusted_rna_write_roots"
            ):
                errors.append(f"trusted_rna_write_roots_mismatch:{path.name}")
            if execution.get("trace") is not None:
                errors.append(f"trusted_rna_unregistered_trace:{path.name}")
            if value.get("broker_owned") is not True:
                errors.append(f"trusted_rna_not_broker_owned:{path.name}")
            request_id = str(value.get("request_id"))
            trigger_path = (
                Path(str(config["trusted_rna_broker_claimed_directory"]))
                / f"{request_id}.json"
            )
            if (
                not trigger_path.is_file()
                or trigger_path.is_symlink()
                or value.get("broker_trigger_sha256")
                != sha_file(trigger_path)
            ):
                errors.append(f"trusted_rna_broker_trigger_mismatch:{path.name}")
            output_root = Path(
                str(config["trusted_rna_broker_output_directory"])
            )
            for stream in ("stdout", "stderr"):
                stream_path = output_root / f"{request_id}.{stream}"
                expected_ref = (
                    file_ref(stream_path)
                    if stream_path.is_file() and not stream_path.is_symlink()
                    else None
                )
                if value.get(stream) != expected_ref:
                    errors.append(
                        f"trusted_rna_broker_{stream}_mismatch:{path.name}"
                    )
        receipts.append(value)

    claimed = sorted(claimed_root.glob("*.json")) if claimed_root.is_dir() else []
    if len(claimed) != len(receipts):
        errors.append("gateway_claimed_receipt_count_mismatch")
    if len(receipts) != len(bash_pre):
        errors.append("gateway_bash_receipt_count_mismatch")
    trusted_receipts = [
        item for item in receipts
        if item.get("execution_plane") == "trusted_rna"
    ]
    pending_broker = sorted(
        Path(str(config["trusted_rna_broker_request_directory"])).glob("*.json")
    )
    claimed_broker = sorted(
        Path(str(config["trusted_rna_broker_claimed_directory"])).glob("*.json")
    )
    if pending_broker:
        errors.append("trusted_rna_broker_unconsumed_trigger")
    if len(claimed_broker) != len(trusted_receipts):
        errors.append("trusted_rna_broker_trigger_receipt_count_mismatch")
    tool_ids = [item.get("tool_use_id") for item in receipts]
    if any(not isinstance(item, str) or not item for item in tool_ids) or len(set(tool_ids)) != len(tool_ids):
        errors.append("gateway_tool_use_identity_invalid")

    isolation_path = Path(str(config["isolation_ledger"]))
    if receipts:
        try:
            isolation_records = load_jsonl(isolation_path)
            isolation.verify_event_chain(isolation_records)
            if len(isolation_records) != 2 * len(receipts):
                errors.append("gateway_isolation_ledger_count_mismatch")
        except (FailClosed, OSError, isolation.IsolationViolation) as exc:
            errors.append(f"gateway_isolation_hash_chain:{exc}")
    elif isolation_path.exists():
        errors.append("unexpected_gateway_isolation_ledger")
    if any(item.get("decision") == "deny" for item in common_hooks):
        errors.append("common_isolation_violation")
    if any(item.get("decision") == "deny" for item in treatment_hooks):
        errors.append("treatment_isolation_violation")
    return not errors, errors


def isolation_evidence(config: Mapping[str, Any]) -> dict[str, Any]:
    def refs(directory_key: str, pattern: str = "*.json") -> list[dict[str, Any]]:
        directory = Path(str(config[directory_key]))
        return [
            file_ref(path)
            for path in sorted(directory.rglob(pattern))
            if path.is_file() and not path.is_symlink()
        ] if directory.is_dir() else []

    ledger_path = Path(str(config["isolation_ledger"]))
    return {
        "schema_version": "issue827-isolation-evidence-v1",
        "harness_materialization": file_ref(Path(str(config["harness_materialization"]))),
        "seatbelt_profile": file_ref(Path(str(config["seatbelt_profile"]))),
        "trusted_rna_seatbelt_profile": file_ref(
            Path(str(config["trusted_rna_seatbelt_profile"]))
        ),
        "private_tree_audits": file_ref(Path(str(config["private_tree_audits"]))),
        "worker_config_preflight": file_ref(Path(str(config["worker_config_preflight"]))),
        "isolation_ledger": file_ref(ledger_path) if ledger_path.is_file() else None,
        "hook_guard_state": (
            file_ref(Path(str(config["hook_guard_state"])))
            if Path(str(config["hook_guard_state"])).is_file()
            else None
        ),
        "native_tool_state": file_ref(
            Path(str(config["episode_evidence_root"]))
            / "hooks/native-tool-state.json"
        ),
        "hook_guard_ledger": (
            file_ref(Path(str(config["hook_guard_ledger"])))
            if Path(str(config["hook_guard_ledger"])).is_file()
            else None
        ),
        "gateway_requests": refs("gateway_request_directory"),
        "gateway_claimed": refs("gateway_claimed_directory"),
        "gateway_revoked": refs("gateway_revoked_directory"),
        "gateway_receipts": refs("gateway_receipt_directory"),
        "gateway_traces": refs("gateway_trace_directory", "trace*"),
        "gateway_teardowns": refs("gateway_teardown_directory"),
        "trusted_rna_broker_requests": refs(
            "trusted_rna_broker_request_directory"
        ),
        "trusted_rna_broker_claimed": refs(
            "trusted_rna_broker_claimed_directory"
        ),
        "trusted_rna_broker_outputs": refs(
            "trusted_rna_broker_output_directory", "*"
        ),
        "trusted_rna_broker_ready": (
            file_ref(Path(str(config["trusted_rna_broker_ready"])))
            if Path(str(config["trusted_rna_broker_ready"])).is_file()
            else None
        ),
        "trusted_rna_broker_stop": (
            file_ref(Path(str(config["trusted_rna_broker_stop"])))
            if Path(str(config["trusted_rna_broker_stop"])).is_file()
            else None
        ),
        "trusted_rna_broker_teardown": (
            file_ref(Path(str(config["trusted_rna_broker_teardown"])))
            if Path(str(config["trusted_rna_broker_teardown"])).is_file()
            else None
        ),
    }


def failed_pre_model_receipt(
    prepared: PreparedRun,
    case: PreparedCase,
    arm: str,
    episode: Path,
    evidence: Path,
    identity_path: Path,
    config: Mapping[str, Any],
    reason: str,
    rna_seconds: float,
    query_evidence: Mapping[str, Any] | None,
) -> dict[str, Any]:
    receipt = {
        "schema_version": RECEIPT_SCHEMA,
        "case_id": case.case_id,
        "rank": case.rank,
        "arm": arm,
        "policy": "control" if arm == "A" else "treatment",
        "session_id": case.sessions[arm],
        "base_commit": case.base_commit,
        "base_tree": case.base_tree,
        "run_manifest": prepared.manifest_ref,
        "registration": prepared.registration_ref,
        "selection": prepared.selection_ref,
        "runtime_identity": file_ref(identity_path),
        "prompt": None,
        "treatment_system": None,
        "query_evidence": dict(query_evidence) if query_evidence is not None else None,
        "command": None,
        "started_at": None,
        "ended_at": utc_now(),
        "timed_out": False,
        "returncode": None,
        "peak_process_tree_rss_kib": 0,
        "stdout": None,
        "stderr": None,
        "transcripts": [],
        "supervisor": {
            "config": file_ref(evidence / "supervisor-config.json"),
            "state": None,
            "common_state": None,
            "common_hook_ledger": None,
            "treatment_hook_ledger": None,
            "native_tool_state": file_ref(
                evidence / "hooks/native-tool-state.json"
            ),
            "rna_events": [],
            "isolation": isolation_evidence(config),
        },
        "actor_tool_ledger": None,
        "token_ledger": token_ledger(
            {"num_turns": 0},
            model_invoked=False,
            provider_responses=0,
            provider_requests=0,
        ),
        "timing_ledger": {
            "rna_preprocessing_seconds": rna_seconds,
            "model_wall_seconds": 0.0,
            "combined_pre_evaluator_wall_seconds": rna_seconds,
        },
        "terminal_patch": None,
        "untracked": [],
        "post_status": None,
        "policy_compliant": False,
        "evidence_complete": True,
        "errors": [reason],
        "authorization_requested": False,
        "evaluator_authorized": False,
        "official_evaluator_invoked": False,
    }
    path = episode / "episode-receipt.json"
    atomic_write(path, canonical(receipt))
    return {"episode_receipt": file_ref(path), **receipt}


def launch_episode(
    prepared: PreparedRun,
    case: PreparedCase,
    arm: str,
    case_root: Path,
    harness_paths: Mapping[str, Path],
) -> dict[str, Any]:
    verify_checkout(str(case.checkouts[arm]), case.base_commit, case.base_tree, f"{case.case_id}/{arm} prelaunch")
    episode, evidence, identity_path, settings_path, config = configure_episode(prepared, case, arm, case_root, harness_paths)
    treatment_system_path: Path | None = None
    query_evidence: dict[str, Any] | None = None
    rna_seconds = 0.0
    if arm == "T":
        query_started = time.monotonic()
        try:
            treatment_system, _, rna_seconds, query_evidence = acquire_treatment(case, harness_paths, evidence, config)
        except TreatmentAcquisitionFailure as exc:
            return failed_pre_model_receipt(
                prepared, case, arm, episode, evidence, identity_path, config,
                f"rna_preprocessing_failed:{type(exc).__name__}:{exc}", exc.elapsed,
                exc.evidence,
            )
        except Exception as exc:
            rna_seconds = time.monotonic() - query_started
            return failed_pre_model_receipt(
                prepared, case, arm, episode, evidence, identity_path, config,
                f"rna_preprocessing_failed:{type(exc).__name__}:{exc}", rna_seconds,
                query_evidence,
            )
        treatment_system_path = episode / "treatment-system.bin"
        atomic_write(treatment_system_path, treatment_system)
    prompt_path = episode / "user-prompt.bin"
    atomic_write(prompt_path, case.prompt)
    command = claude_command(
        prepared,
        case.sessions[arm],
        settings_path,
        treatment_system_path,
        Path(config["seatbelt_profile"]),
    )
    stdout_path = episode / "claude.stdout.json"
    stderr_path = episode / "claude.stderr"
    started_at = utc_now()
    started = time.monotonic()
    model_ended = started
    timed_out = False
    supervisor_fatal = False
    peak_rss = 0
    broker_runtime = start_trusted_rna_broker(prepared, config, evidence)
    broker_teardown: dict[str, Any] | None = None
    broker_teardown_error: str | None = None
    try:
        with stdout_path.open("xb") as stdout_handle, stderr_path.open("xb") as stderr_handle:
            process = subprocess.Popen(
                command,
                cwd=case.checkouts[arm],
                env=provider_parent_env(episode / "private"),
                stdin=subprocess.PIPE,
                stdout=stdout_handle,
                stderr=stderr_handle,
                start_new_session=True,
            )
            assert process.stdin is not None
            process.stdin.write(case.prompt)
            process.stdin.close()
            while process.poll() is None:
                peak_rss = max(peak_rss, process_tree_rss_kib(process.pid))
                if (
                    state_fatal(Path(config["state"]))
                    or state_fatal(Path(config["common_state"]))
                    or state_fatal(Path(config["hook_guard_state"]))
                ):
                    supervisor_fatal = True
                    terminate_group(process)
                    break
                if time.monotonic() - started > prepared.registration["model_runtime"]["wall_seconds"]:
                    timed_out = True
                    terminate_group(process)
                    break
                time.sleep(0.25)
            returncode = process.wait()
            model_ended = time.monotonic()
    finally:
        try:
            broker_teardown = stop_trusted_rna_broker(
                broker_runtime, config
            )
        except Exception as exc:
            broker_teardown_error = (
                f"{type(exc).__name__}:{exc}"
            )
    actual_model_wall = model_ended - started
    model_wall_limit = float(
        prepared.registration["model_runtime"]["wall_seconds"]
    )
    if actual_model_wall > model_wall_limit:
        timed_out = True
    wall = min(actual_model_wall, model_wall_limit)
    ended_at = utc_now()
    summary = safe_summary(stdout_path)
    try:
        loaded_result = json.loads(stdout_path.read_bytes())
        raw_result = loaded_result if isinstance(loaded_result, dict) else {}
    except (OSError, json.JSONDecodeError):
        raw_result = {}
    patch, untracked = capture_patch(case.checkouts[arm])
    patch_path = episode / "terminal.patch"
    atomic_write(patch_path, patch)
    terminal_patch = file_ref(patch_path) if patch else None
    status = clean_status(case.checkouts[arm])
    status_path = evidence / "post-status.bin"
    atomic_write(status_path, status)
    transcripts = copy_transcripts(case.sessions[arm], evidence)
    model_events = transcript_model_events(transcripts)
    tokens = token_ledger(
        raw_result,
        model_invoked=True,
        model_events=model_events,
        provider_responses=(len(model_events) if model_events is not None else None),
    )
    compliant, errors = treatment_compliance(config, evidence, arm)
    if timed_out:
        errors.append("model_wall_timeout")
    if supervisor_fatal:
        errors.append("supervisor_fatal_termination")
    if returncode != 0:
        errors.append(f"model_exit_{returncode}")
    if broker_teardown_error is not None:
        errors.append(f"trusted_rna_broker_teardown:{broker_teardown_error}")
    if summary.get("valid_json") is not True:
        errors.append("invalid_claude_json")
    if summary.get("session_id") not in (None, case.sessions[arm]):
        errors.append("session_id_mismatch")
    common_hooks = load_jsonl(Path(config["common_hook_ledger"]))
    treatment_hooks = load_jsonl(Path(config["hook_ledger"]))
    isolated, isolation_errors = isolation_compliance(
        config, common_hooks, treatment_hooks
    )
    errors.extend(isolation_errors)
    post_private_audit_path = evidence / "isolation/post-private-tree-audit.json"
    try:
        post_private_audit = isolation.audit_private_tree(
            case.checkouts[arm]
        )
        atomic_write(post_private_audit_path, canonical(post_private_audit))
    except isolation.IsolationViolation as exc:
        errors.append(f"post_private_tree_audit:{exc}")
    token_evidence_complete = tokens.get("valid") is True
    authorization_requested = (
        not errors
        and compliant
        and isolated
        and token_evidence_complete
        and terminal_patch is not None
    )
    actor_ledger = build_actor_tool_ledger(
        arm,
        common_hooks,
        treatment_hooks,
        query_evidence,
        authorization_requested,
    )
    actor_path = evidence / "actor-tool-ledger.json"
    atomic_write(actor_path, canonical(actor_ledger))
    rna_receipts = [file_ref(path) for path in sorted((evidence / "rna-events").glob("*.json"))]
    state_path = Path(config["state"])
    common_state_path = Path(config["common_state"])
    common_ledger_path = Path(config["common_hook_ledger"])
    treatment_ledger_path = Path(config["hook_ledger"])
    guard_state_path = Path(config["hook_guard_state"])
    guard_ledger_path = Path(config["hook_guard_ledger"])
    receipt = {
        "schema_version": RECEIPT_SCHEMA,
        "case_id": case.case_id,
        "rank": case.rank,
        "arm": arm,
        "policy": "control" if arm == "A" else "treatment",
        "session_id": case.sessions[arm],
        "base_commit": case.base_commit,
        "base_tree": case.base_tree,
        "run_manifest": prepared.manifest_ref,
        "registration": prepared.registration_ref,
        "selection": prepared.selection_ref,
        "runtime_identity": file_ref(identity_path),
        "prompt": file_ref(prompt_path),
        "treatment_system": file_ref(treatment_system_path) if treatment_system_path else None,
        "query_evidence": query_evidence,
        "command": command,
        "started_at": started_at,
        "ended_at": ended_at,
        "timed_out": timed_out,
        "returncode": returncode,
        "peak_process_tree_rss_kib": peak_rss,
        "stdout": file_ref(stdout_path),
        "stderr": file_ref(stderr_path),
        "transcripts": transcripts,
        "supervisor": {
            "config": file_ref(evidence / "supervisor-config.json"),
            "state": file_ref(state_path) if state_path.is_file() else None,
            "common_state": file_ref(common_state_path) if common_state_path.is_file() else None,
            "common_hook_ledger": file_ref(common_ledger_path) if common_ledger_path.is_file() else None,
            "treatment_hook_ledger": file_ref(treatment_ledger_path) if treatment_ledger_path.is_file() else None,
            "hook_guard_state": file_ref(guard_state_path) if guard_state_path.is_file() else None,
            "hook_guard_ledger": file_ref(guard_ledger_path) if guard_ledger_path.is_file() else None,
            "native_tool_state": file_ref(
                evidence / "hooks/native-tool-state.json"
            ),
            "rna_events": rna_receipts,
            "isolation": isolation_evidence(config),
            "trusted_rna_broker": broker_teardown,
            "post_private_tree_audit": (
                file_ref(post_private_audit_path)
                if post_private_audit_path.is_file()
                else None
            ),
        },
        "actor_tool_ledger": file_ref(actor_path),
        "token_ledger": tokens,
        "timing_ledger": {
            "rna_preprocessing_seconds": rna_seconds,
            "model_wall_seconds": wall,
            "combined_pre_evaluator_wall_seconds": rna_seconds + wall,
        },
        "terminal_patch": terminal_patch,
        "untracked": untracked,
        "post_status": file_ref(status_path),
        "policy_compliant": compliant and isolated and not errors,
        "evidence_complete": token_evidence_complete,
        "errors": errors,
        "authorization_requested": authorization_requested,
        "evaluator_authorized": False,
        "official_evaluator_invoked": False,
    }
    receipt_path = episode / "episode-receipt.json"
    atomic_write(receipt_path, canonical(receipt))
    return {"episode_receipt": file_ref(receipt_path), **receipt}


def execute_case(prepared: PreparedRun, case: PreparedCase) -> tuple[list[dict[str, Any]], list[str]]:
    case_root = prepared.output_root / f"rank-{case.rank:02d}-{case.case_id}"
    case_root.mkdir(parents=True, exist_ok=False)
    receipts: list[dict[str, Any]] = []
    errors: list[str] = []
    for arm in case.arm_order:
        try:
            harness_paths = materialize_harness(case_root, arm)
            receipts.append(launch_episode(prepared, case, arm, case_root, harness_paths))
        except Exception as exc:
            errors.append(f"{case.case_id}/{arm}: {type(exc).__name__}: {exc}")
            # Same-case serialization is frozen.  A harness failure does not
            # authorize launching the following arm because comparability is
            # no longer verifier-clean.
            break
    return receipts, errors


def execution_cases(prepared: PreparedRun) -> tuple[PreparedCase, ...]:
    require(len(prepared.cases) == 2, "fresh selector requires exactly two cases")
    return prepared.cases


def execute(prepared: PreparedRun) -> int:
    cases = execution_cases(prepared)
    prepared.output_root.mkdir(parents=True, exist_ok=False)
    start = {
        "schema_version": "issue827-selector-invocation-v1",
        "started_at": utc_now(),
        "run_manifest": prepared.manifest_ref,
        "parallel_cases": len(cases),
        "same_case_serial": True,
        "models_authorized": len(cases) * 2,
        "execution_episode_keys": [
            {"case_id": case.case_id, "rank": case.rank, "arm": arm}
            for case in cases
            for arm in case.arm_order
        ],
        "official_evaluator_invoked": False,
    }
    atomic_write(prepared.output_root / "invocation-start.json", canonical(start))
    receipts: list[dict[str, Any]] = []
    errors: list[str] = []
    with ThreadPoolExecutor(max_workers=2, thread_name_prefix="issue827") as executor:
        futures = {executor.submit(execute_case, prepared, case): case for case in cases}
        for future in as_completed(futures):
            case = futures[future]
            try:
                case_receipts, case_errors = future.result()
                receipts.extend(case_receipts)
                errors.extend(case_errors)
            except Exception as exc:
                errors.append(f"{case.case_id}: {type(exc).__name__}: {exc}")
    result = {
        **start,
        "ended_at": utc_now(),
        "episode_receipts": sorted(
            (receipt["episode_receipt"] for receipt in receipts),
            key=lambda ref: ref["path"],
        ),
        "worker_errors": sorted(errors),
        "all_authorized_episodes_recorded": len(receipts) == 4,
        "all_four_episodes_recorded": len(receipts) == 4,
        "evaluator_authorizations": 0,
        "authorization_requests": sum(
            receipt.get("authorization_requested") is True for receipt in receipts
        ),
        "official_evaluator_invoked": False,
    }
    atomic_write(prepared.output_root / "invocation-result.json", canonical(result))
    return 0 if not errors and len(receipts) == len(cases) * 2 else 1


def preflight_summary(prepared: PreparedRun) -> dict[str, Any]:
    cases = execution_cases(prepared)
    return {
        "status": "READY_TO_EXECUTE_SELECTOR",
        "run_manifest": prepared.manifest_ref,
        "registration": prepared.registration_ref,
        "selection": prepared.selection_ref,
        "claude": {
            "path": str(prepared.claude_path),
            "sha256": sha_file(prepared.claude_path),
            "version_output": prepared.claude_version,
        },
        "rna": {
            "launcher": prepared.rna_refs["launcher"],
            "binary": prepared.rna_refs["binary"],
            "canonical_environment": prepared.rna_refs["canonical_environment"],
        },
        "output_root_absent": not prepared.output_root.exists(),
        "cases": [
            {
                "rank": case.rank,
                "case_id": case.case_id,
                "base_commit": case.base_commit,
                "base_tree": case.base_tree,
                "root": case.root,
                "expected_repository_identity": case.expected_repository_identity,
                "live_repository_identity": case.live_repository_identity,
                "cache_archive_sha256": case.cache_refs["archive"]["sha256"],
                "cache_manifest_sha256": case.cache_refs["manifest"]["sha256"],
                "prompt_sha256_A": sha_bytes(case.prompt),
                "prompt_sha256_T": sha_bytes(case.prompt),
                "prompt_bytes_A": len(case.prompt),
                "prompt_bytes_T": len(case.prompt),
                "prompt_equal": True,
                "arm_order": list(case.arm_order),
                "sessions": case.sessions,
                "checkouts": {arm: str(path) for arm, path in case.checkouts.items()},
            }
            for case in prepared.cases
        ],
        "same_case_serial": True,
        "maximum_concurrent_cases": 2,
        "execution_episode_keys": [
            {"case_id": case.case_id, "rank": case.rank, "arm": arm}
            for case in cases
            for arm in case.arm_order
        ],
        "models_launched": 0,
        "official_evaluator_invoked": False,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="command", required=True)
    preflight = sub.add_parser("preflight", help="read-only frozen-identity preflight")
    preflight.add_argument("--manifest", type=Path, required=True)
    run = sub.add_parser("run", help="preflight and optionally execute the four episodes")
    run.add_argument("--manifest", type=Path, required=True)
    run.add_argument("--execute", action="store_true", help="launch the four paid Claude episodes")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        prepared = prepare(args.manifest)
        print(json.dumps(preflight_summary(prepared), sort_keys=True, indent=2))
        if args.command == "preflight" or not args.execute:
            if args.command == "run":
                print("DRY RUN ONLY: add --execute to launch paid model episodes", file=sys.stderr)
            return 0
        return execute(prepared)
    except FailClosed as exc:
        print(f"FAIL CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
