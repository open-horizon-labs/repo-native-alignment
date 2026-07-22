#!/usr/bin/env python3
"""Fail-closed launcher for the frozen #825 paired A/T selector.

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
import shutil
import signal
import subprocess
import sys
import time
import uuid
from typing import Any, Mapping, Sequence


RUN_SCHEMA = "issue825-selector-run-v1"
IDENTITY_SCHEMA = "issue825-runtime-identity-v1"
RECEIPT_SCHEMA = "issue825-episode-receipt-v1"
EMPTY_MCP_BYTES = b'{"mcpServers":{}}\n'
READY_SENTINEL = "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"
SOURCE = Path(__file__).resolve().parent
CODE_KINDS = {
    "class", "const", "enum", "function", "interface", "method", "module",
    "struct", "trait", "type", "type_alias", "union",
}

REGISTERED_FILE_NAMES = {
    "system_prefix_sha256": "system-prefix.txt",
    "system_suffix_sha256": "system-suffix.txt",
    "rna_query_sha256": "rna_query.py",
    "rna_traverse_sha256": "rna_traverse.py",
    "tool_supervisor_sha256": "tool_supervisor.py",
    "supervisor_template_sha256": "supervisor.template.json",
    "claude_settings_template_sha256": "claude-settings.template.json",
    "validator_sha256": "validate_episode.py",
    "common_supervisor_sha256": "common_supervisor.py",
    "runner_sha256": "run_selector.py",
    "verifier_sha256": "verify_selector.py",
}


class FailClosed(RuntimeError):
    """A frozen identity or evidence precondition did not hold."""


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
        "schema_version": "issue825-operational-cache-inventory-v1",
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


def verify_checkout(path_text: str, commit: str, tree: str, where: str, *, cache: bool = False) -> Path:
    checkout = Path(path_text)
    require(checkout.is_absolute() and checkout.is_dir() and not checkout.is_symlink(), f"{where} invalid")
    require(git(checkout, "rev-parse", "--is-inside-work-tree").stdout.strip() == b"true", f"{where} not git")
    require(git(checkout, "rev-parse", "HEAD").stdout.decode().strip() == commit, f"{where} HEAD mismatch")
    require(git(checkout, "rev-parse", "HEAD^{tree}").stdout.decode().strip() == tree, f"{where} tree mismatch")
    require(clean_status(checkout) == b"", f"{where} is not pristine")
    if cache:
        cache_path = checkout / ".oh/.cache"
        require(cache_path.is_dir() and not cache_path.is_symlink(), f"{where} missing nonsymlink .oh/.cache")
        try:
            cache_path.resolve(strict=True).relative_to(checkout.resolve(strict=True))
        except (OSError, ValueError) as exc:
            raise FailClosed(f"{where} cache escapes checkout") from exc
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


def numeric(value: Any) -> int:
    return value if type(value) is int and value >= 0 else 0


def observed_numeric(value: Any) -> int | None:
    return value if type(value) is int and value >= 0 else None


def usage_value(usage: Mapping[str, Any], snake: str, camel: str) -> int | None:
    value = usage.get(snake)
    if value is None:
        value = usage.get(camel)
    return observed_numeric(value)


def token_ledger(summary: Mapping[str, Any]) -> dict[str, Any]:
    """Use one provider source; never add top-level usage to modelUsage."""
    source = "unavailable"
    normalized: dict[str, int] | None = None
    usage = summary.get("usage")
    if isinstance(usage, dict):
        input_tokens = usage_value(usage, "input_tokens", "inputTokens")
        output_tokens = usage_value(usage, "output_tokens", "outputTokens")
        if input_tokens is not None and output_tokens is not None:
            source = "top_level_usage"
            reasoning = usage_value(usage, "reasoning_tokens", "reasoningTokens")
            details = usage.get("output_tokens_details")
            if reasoning is None and isinstance(details, dict):
                reasoning = usage_value(details, "reasoning_tokens", "reasoningTokens")
            normalized = {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_creation_input_tokens": usage_value(
                    usage, "cache_creation_input_tokens", "cacheCreationInputTokens"
                ) or 0,
                "cache_read_input_tokens": usage_value(
                    usage, "cache_read_input_tokens", "cacheReadInputTokens"
                ) or 0,
                "reasoning_tokens": reasoning or 0,
            }
    if normalized is None:
        model_usage = summary.get("modelUsage")
        candidates: list[Mapping[str, Any]] = []
        if isinstance(model_usage, dict):
            for entry in model_usage.values():
                if not isinstance(entry, dict):
                    continue
                candidate = entry.get("usage", entry)
                if isinstance(candidate, dict):
                    candidates.append(candidate)
        if candidates:
            totals = {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "reasoning_tokens": 0,
            }
            valid = True
            for candidate in candidates:
                input_tokens = usage_value(candidate, "input_tokens", "inputTokens")
                output_tokens = usage_value(candidate, "output_tokens", "outputTokens")
                if input_tokens is None or output_tokens is None:
                    valid = False
                    break
                totals["input_tokens"] += input_tokens
                totals["output_tokens"] += output_tokens
                totals["cache_creation_input_tokens"] += usage_value(
                    candidate, "cache_creation_input_tokens", "cacheCreationInputTokens"
                ) or 0
                totals["cache_read_input_tokens"] += usage_value(
                    candidate, "cache_read_input_tokens", "cacheReadInputTokens"
                ) or 0
                totals["reasoning_tokens"] += usage_value(
                    candidate, "reasoning_tokens", "reasoningTokens"
                ) or 0
            if valid:
                source = "model_usage_sum"
                normalized = totals
    provider_requests = observed_numeric(summary.get("num_turns"))
    if normalized is None or provider_requests is None:
        return {
            "schema_version": "issue825-token-ledger-v2",
            "valid": False,
            "errors": ["missing_or_invalid_observed_provider_usage"],
            "source": source,
            "input_tokens": None,
            "output_tokens": None,
            "input_plus_output_tokens": None,
            "cache_creation_input_tokens": None,
            "cache_read_input_tokens": None,
            "reasoning_tokens": None,
            "provider_requests": provider_requests,
        }
    return {
        "schema_version": "issue825-token-ledger-v2",
        "valid": True,
        "errors": [],
        "source": source,
        **normalized,
        "input_plus_output_tokens": normalized["input_tokens"] + normalized["output_tokens"],
        "provider_requests": provider_requests,
    }


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
    arm_order: tuple[str, str]
    checkouts: dict[str, Path]
    sessions: dict[str, str]


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


def validate_registered_sources(registration: Mapping[str, Any]) -> None:
    registered = registration.get("registered_files")
    require(isinstance(registered, dict), "registration.registered_files missing")
    for key, filename in REGISTERED_FILE_NAMES.items():
        require(registered.get(key) == sha_file(SOURCE / filename), f"registered source hash mismatch: {filename}")


def verify_runtime(manifest: Mapping[str, Any], registration: Mapping[str, Any]) -> tuple[Path, str]:
    runtime = registration["model_runtime"]
    require(runtime == {
        "cli": "Claude Code",
        "cli_version": "2.1.216",
        "cli_sha256": "d01b49210d72ecbe277a2665d104bacccddf2d22185be99446d2929e0edfc48d",
        "model": "claude-sonnet-5",
        "effort": "high",
        "wall_seconds": 600,
        "budget_usd": 3.0,
        "invocations_per_episode": 1,
        "resume_allowed": False,
        "model_retry_allowed": False,
        "permission_mode": "bypassPermissions",
        "safe_mode": False,
        "tools": ["Bash", "Edit", "Read", "Write", "Glob", "Grep"],
        "disallowed_tools": ["WebSearch", "WebFetch"],
        "strict_empty_mcp_sha256": "e93fc8db2b1bd77107fe6c758bca9545fa864cf7cce8ab93a7b2b93a1d566a7b",
    }, "registration model runtime is not the frozen #825 runtime")
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
    exact_keys(artifact, {"launcher", "binary", "bundle_manifest", "archive", "verification_receipt"}, "manifest.rna_artifact")
    expected = registration["rna_artifact"]
    keys = {
        "launcher": "launcher_sha256",
        "binary": "binary_sha256",
        "bundle_manifest": "bundle_manifest_sha256",
        "archive": "archive_sha256",
        "verification_receipt": "verification_receipt_sha256",
    }
    refs: dict[str, dict[str, Any]] = {}
    paths: dict[str, Path] = {}
    for name, expected_key in keys.items():
        path, _ = check_ref(artifact[name], f"manifest.rna_artifact.{name}", materialize=False)
        require(artifact[name]["sha256"] == expected[expected_key], f"RNA {name} does not match registration")
        refs[name] = dict(artifact[name])
        paths[name] = path
    require(expected.get("local_source_build_allowed") is False, "registration permits local source artifact")
    return paths["launcher"], paths["binary"], refs


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
) -> tuple[Path, str, dict[str, dict[str, Any]], tuple[dict[str, Any], ...], str]:
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
    return checkout, root, refs, tuple(normalized), expected["operational_cache_inventory_sha256"]


def prepare(manifest_path: Path, *, permit_output: bool = False, permit_sessions: bool = False) -> PreparedRun:
    manifest_path = manifest_path.resolve(strict=True)
    manifest = read_json(manifest_path)
    require(isinstance(manifest, dict), "manifest must be an object")
    exact_keys(
        manifest,
        {
            "schema_version", "evidence_root", "registration", "selection", "runner",
            "common_supervisor", "claude", "rna_artifact", "mcp_config",
            "output_root", "cases",
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
    require(registration.get("schema_version") == "issue825-treatment-registration-v2", "registration schema mismatch")
    validate_registered_sources(registration)
    registration_ref = dict(manifest["registration"])

    selection_path, _ = check_ref(manifest["selection"], "manifest.selection")
    selection = read_json(selection_path)
    require(selection.get("schema_version") == "issue825-fresh-pair-selection-v2", "selection schema mismatch")
    require(selection.get("registration_sha256") == sha_bytes(registration_bytes), "selection binds another registration")
    selected = selection.get("cases")
    require(isinstance(selected, list) and len(selected) == 2, "selection must contain exactly two cases")

    claude_path, claude_version = verify_runtime(manifest, registration)
    launcher_path, binary_path, rna_refs = verify_rna_artifact(manifest, registration)
    mcp_path, mcp_bytes = check_ref(manifest["mcp_config"], "manifest.mcp_config")
    require(mcp_bytes == EMPTY_MCP_BYTES, "MCP config is not canonical strict-empty")
    require(manifest["mcp_config"]["sha256"] == registration["model_runtime"]["strict_empty_mcp_sha256"], "MCP hash differs from registration")

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
        exact_keys(case, {"rank", "instance_id", "base_commit", "base_tree", "problem_statement", "user_prompt", "cache", "arms"}, where)
        require(case["rank"] == chosen.get("rank") and case["instance_id"] == chosen.get("instance_id"), f"{where} differs from selection")
        require(case["base_commit"] == chosen.get("base_commit") and case["base_tree"] == chosen.get("base_tree"), f"{where} base differs from selection")
        case_id = case["instance_id"]
        require(re.fullmatch(r"[A-Za-z0-9_.+-]+__[A-Za-z0-9_.+-]+-[0-9]+", case_id) is not None, f"{where}.instance_id invalid")
        problem_path, problem = check_ref(case["problem_statement"], f"{where}.problem_statement")
        del problem_path
        require(sha_bytes(problem) == chosen.get("problem_statement_sha256"), f"{where} problem statement mismatch")
        _, prompt = check_ref(case["user_prompt"], f"{where}.user_prompt")
        require(prompt.count(problem) == 1 and prompt.endswith(problem), f"{where} prompt must contain exact problem once at end")
        index_checkout, root, cache_refs, cache_bindings, cache_inventory = verify_cache(
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
            checkout = verify_checkout(arms[arm]["checkout"], case["base_commit"], case["base_tree"], f"{where}.arms.{arm}.checkout")
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
        cases.append(
            PreparedCase(
                rank=case["rank"], case_id=case_id, base_commit=case["base_commit"], base_tree=case["base_tree"],
                problem=problem, prompt=prompt, title=title_bytes(problem), index_checkout=index_checkout,
                root=root, cache_refs=cache_refs, cache_bindings=cache_bindings,
                cache_inventory_sha256=cache_inventory, arm_order=arm_order,
                checkouts=arm_checkouts, sessions=arm_sessions,
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


def materialize_harness(case_root: Path) -> dict[str, Path]:
    harness = case_root / "harness"
    bin_dir = harness / "bin"
    config_dir = harness / "config"
    bin_dir.mkdir(parents=True, exist_ok=False)
    config_dir.mkdir(parents=True, exist_ok=False)
    paths: dict[str, Path] = {}
    for name in ("rna_query.py", "rna_traverse.py", "tool_supervisor.py", "common_supervisor.py"):
        destination = bin_dir / name
        shutil.copyfile(SOURCE / name, destination)
        destination.chmod(0o555)
        paths[name] = destination
    paths["harness"] = harness
    paths["config"] = config_dir / "supervisor.json"
    return paths


def make_identity(prepared: PreparedRun, case: PreparedCase) -> dict[str, Any]:
    return {
        "schema_version": IDENTITY_SCHEMA,
        "case_id": case.case_id,
        "base_commit": case.base_commit,
        "base_tree": case.base_tree,
        "root": case.root,
        "index_checkout": str(case.index_checkout),
        "producer_commit": prepared.registration["rna_artifact"]["producer_commit"],
        "launcher_path": str(prepared.launcher_path),
        "launcher_sha256": prepared.rna_refs["launcher"]["sha256"],
        "binary_path": str(prepared.binary_path),
        "binary_sha256": prepared.rna_refs["binary"]["sha256"],
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
    identity_path = evidence / "runtime-identity.json"
    identity = make_identity(prepared, case)
    atomic_write(identity_path, canonical(identity))
    identity_sha = sha_file(identity_path)
    title_path = harness_paths["harness"] / "title-query.txt"
    atomic_write(title_path, case.title + b"\n")
    wrapper = harness_paths["rna_traverse.py"]
    query_wrapper = harness_paths["rna_query.py"]
    config = {
        "schema_version": "rna-supervisor-config-v2",
        "policy": "control" if arm == "A" else "treatment",
        "launcher": str(prepared.launcher_path),
        "binary": str(prepared.binary_path),
        "repo": str(case.index_checkout),
        "checkout": str(case.checkouts[arm]),
        "root": case.root,
        "initial_response": str(evidence / "query/projection.stdout"),
        "initial_response_sha256": None,
        "initial_ids": [],
        "wrapper": str(wrapper),
        "query_wrapper": str(query_wrapper),
        "harness_root": str(harness_paths["harness"]),
        "episode_evidence_root": str(evidence),
        "state": str(evidence / "supervisor-state.json"),
        "common_state": str(evidence / "common-supervisor-state.json"),
        "lock": str(evidence / "supervisor.lock"),
        "common_lock": str(evidence / "common-supervisor.lock"),
        "hook_ledger": str(evidence / "hooks/treatment-events.jsonl"),
        "common_hook_ledger": str(evidence / "hooks/common-events.jsonl"),
        "rna_events": str(evidence / "rna-events"),
        "query_events": str(evidence / "query"),
        "identity_receipt": str(identity_path),
        "expected_identity_sha256": identity_sha,
        "expected_base_commit": case.base_commit,
        "expected_base_tree": case.base_tree,
        "expected_producer_commit": prepared.registration["rna_artifact"]["producer_commit"],
        "expected_cache_manifest_sha256": case.cache_refs["manifest"]["sha256"],
        "expected_cache_archive_sha256": case.cache_refs["archive"]["sha256"],
        "expected_cache_inventory_sha256": case.cache_inventory_sha256,
        "expected_launcher_sha256": prepared.rna_refs["launcher"]["sha256"],
        "expected_binary_sha256": prepared.rna_refs["binary"]["sha256"],
        "expected_query_sha256": sha_bytes(case.title),
        "result_limit": 10,
    }
    atomic_write(harness_paths["config"], canonical(config))
    snapshot = evidence / "supervisor-config.json"
    atomic_write(snapshot, canonical(config))

    settings_template = read_json(SOURCE / "claude-settings.template.json")
    settings = render_template(
        settings_template,
        {
            "__COMMON_SUPERVISOR__": str(harness_paths["common_supervisor.py"]),
            "__TOOL_SUPERVISOR__": str(harness_paths["tool_supervisor.py"]),
        },
    )
    settings_path = episode / "claude-settings.json"
    atomic_write(settings_path, canonical(settings))
    return episode, evidence, identity_path, settings_path, config


def acquire_treatment(
    case: PreparedCase,
    harness_paths: Mapping[str, Path],
    evidence: Path,
    config: dict[str, Any],
) -> tuple[bytes, list[str], float, dict[str, Any]]:
    command = [
        str(harness_paths["rna_query.py"]),
        "--query-sha256",
        config["expected_query_sha256"],
    ]
    started = time.monotonic()
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    elapsed = time.monotonic() - started
    projection_path = evidence / "query/projection.stdout"
    wrapper_stderr = evidence / "query/wrapper.stderr"
    atomic_write(projection_path, result.stdout)
    atomic_write(wrapper_stderr, result.stderr)
    require(result.returncode == 0, f"{case.case_id} exact-title RNA query failed: {result.stderr.decode(errors='replace')}")
    receipt_path = evidence / "query/title-query.json"
    require(receipt_path.is_file(), f"{case.case_id} query wrapper did not retain raw receipt")
    receipt = read_json(receipt_path)
    require(receipt.get("identity_sha256") == config["expected_identity_sha256"], "query identity mismatch")
    require(receipt.get("root") == case.root, "query root mismatch")
    require(receipt.get("returncode") == 0, "query raw launcher failed")
    text = result.stdout.decode("utf-8", errors="strict")
    require(READY_SENTINEL in text, "query projection missing exact READY sentinel")
    ids = stable_code_ids(text)
    require(ids, "query projection returned no stable code IDs")
    require(receipt.get("projected_stable_code_ids") == ids, "query projected stable IDs mismatch")
    require(
        all(f"`{item}`".encode() in result.stdout for item in ids),
        "query authorized an ID absent from injected response bytes",
    )
    config["initial_ids"] = ids
    config["initial_response"] = str(projection_path)
    config["initial_response_sha256"] = sha_bytes(result.stdout)
    atomic_write(harness_paths["config"], canonical(config))
    atomic_write(evidence / "supervisor-config.json", canonical(config))
    prefix = (SOURCE / "system-prefix.txt").read_bytes()
    suffix = (SOURCE / "system-suffix.txt").read_bytes().replace(b"__TRAVERSAL_WRAPPER__", str(harness_paths["rna_traverse.py"]).encode())
    opaque_call = f"rna_query --query-sha256 {config['expected_query_sha256']}\n".encode()
    system = prefix + opaque_call + b"\nRNA TOOL RESPONSE\n" + result.stdout + suffix
    query_evidence = {
        "wrapper_command": command,
        "wrapper_returncode": result.returncode,
        "wrapper_stdout": file_ref(projection_path),
        "wrapper_stderr": file_ref(wrapper_stderr),
        "raw_receipt": file_ref(receipt_path),
        "raw_stdout": receipt["stdout"],
        "raw_stderr": receipt["stderr"],
        "projected_stable_code_ids": ids,
        "raw_stable_code_ids": receipt.get("raw_stable_code_ids"),
        "elapsed_seconds": elapsed,
    }
    return system, ids, elapsed, query_evidence


def claude_command(prepared: PreparedRun, session: str, settings: Path, treatment_system: Path | None) -> list[str]:
    runtime = prepared.registration["model_runtime"]
    command = [
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
        command.extend(["--append-system-prompt-file", str(treatment_system)])
    require("--safe-mode" not in command and "--resume" not in command, "forbidden Claude mode")
    return command


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


def build_actor_tool_ledger(
    arm: str,
    common_hooks: list[dict[str, Any]],
    treatment_hooks: list[dict[str, Any]],
    query: dict[str, Any] | None,
    evaluator_authorized: bool,
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
        "action": "official_evaluator_authorization",
        "authorized": evaluator_authorized,
        "invoked": False,
    })
    counts: dict[str, int] = {}
    for action in actions:
        if action.get("actor") == "model" and isinstance(action.get("tool"), str):
            counts[action["tool"]] = counts.get(action["tool"], 0) + 1
    return {
        "schema_version": "issue825-actor-tool-ledger-v1",
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
    if any(item.get("decision") == "deny" for item in treatment_hooks):
        errors.append("treatment_supervisor_denial")
    return not errors, errors


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
        "query_evidence": None,
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
            "rna_events": [],
        },
        "actor_tool_ledger": None,
        "token_ledger": token_ledger({}),
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
        except Exception as exc:
            rna_seconds = time.monotonic() - query_started
            return failed_pre_model_receipt(
                prepared, case, arm, episode, evidence, identity_path, config,
                f"rna_preprocessing_failed:{type(exc).__name__}:{exc}", rna_seconds,
            )
        treatment_system_path = episode / "treatment-system.bin"
        atomic_write(treatment_system_path, treatment_system)
    prompt_path = episode / "user-prompt.bin"
    atomic_write(prompt_path, case.prompt)
    command = claude_command(prepared, case.sessions[arm], settings_path, treatment_system_path)
    stdout_path = episode / "claude.stdout.json"
    stderr_path = episode / "claude.stderr"
    started_at = utc_now()
    started = time.monotonic()
    timed_out = False
    supervisor_fatal = False
    peak_rss = 0
    with stdout_path.open("xb") as stdout_handle, stderr_path.open("xb") as stderr_handle:
        process = subprocess.Popen(
            command,
            cwd=case.checkouts[arm],
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
            if state_fatal(Path(config["state"])) or state_fatal(Path(config["common_state"])):
                supervisor_fatal = True
                terminate_group(process)
                break
            if time.monotonic() - started > prepared.registration["model_runtime"]["wall_seconds"]:
                timed_out = True
                terminate_group(process)
                break
            time.sleep(0.25)
        returncode = process.wait()
    wall = time.monotonic() - started
    ended_at = utc_now()
    summary = safe_summary(stdout_path)
    tokens = token_ledger(summary)
    patch, untracked = capture_patch(case.checkouts[arm])
    patch_path = episode / "terminal.patch"
    atomic_write(patch_path, patch)
    terminal_patch = file_ref(patch_path) if patch else None
    status = clean_status(case.checkouts[arm])
    status_path = evidence / "post-status.bin"
    atomic_write(status_path, status)
    transcripts = copy_transcripts(case.sessions[arm], evidence)
    compliant, errors = treatment_compliance(config, evidence, arm)
    if timed_out:
        errors.append("model_wall_timeout")
    if supervisor_fatal:
        errors.append("supervisor_fatal_termination")
    if returncode != 0:
        errors.append(f"model_exit_{returncode}")
    if summary.get("valid_json") is not True:
        errors.append("invalid_claude_json")
    if summary.get("session_id") not in (None, case.sessions[arm]):
        errors.append("session_id_mismatch")
    common_hooks = load_jsonl(Path(config["common_hook_ledger"]))
    treatment_hooks = load_jsonl(Path(config["hook_ledger"]))
    token_evidence_complete = tokens.get("valid") is True
    evaluator_authorized = not errors and compliant and token_evidence_complete and terminal_patch is not None
    actor_ledger = build_actor_tool_ledger(arm, common_hooks, treatment_hooks, query_evidence, evaluator_authorized)
    actor_path = evidence / "actor-tool-ledger.json"
    atomic_write(actor_path, canonical(actor_ledger))
    rna_receipts = [file_ref(path) for path in sorted((evidence / "rna-events").glob("*.json"))]
    state_path = Path(config["state"])
    common_state_path = Path(config["common_state"])
    common_ledger_path = Path(config["common_hook_ledger"])
    treatment_ledger_path = Path(config["hook_ledger"])
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
            "rna_events": rna_receipts,
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
        "policy_compliant": compliant and not errors,
        "evidence_complete": token_evidence_complete,
        "errors": errors,
        "evaluator_authorized": evaluator_authorized,
        "official_evaluator_invoked": False,
    }
    receipt_path = episode / "episode-receipt.json"
    atomic_write(receipt_path, canonical(receipt))
    return {"episode_receipt": file_ref(receipt_path), **receipt}


def execute_case(prepared: PreparedRun, case: PreparedCase) -> tuple[list[dict[str, Any]], list[str]]:
    case_root = prepared.output_root / f"rank-{case.rank:02d}-{case.case_id}"
    case_root.mkdir(parents=True, exist_ok=False)
    harness_paths = materialize_harness(case_root)
    receipts: list[dict[str, Any]] = []
    errors: list[str] = []
    for arm in case.arm_order:
        try:
            receipts.append(launch_episode(prepared, case, arm, case_root, harness_paths))
        except Exception as exc:
            errors.append(f"{case.case_id}/{arm}: {type(exc).__name__}: {exc}")
            # Same-case serialization is frozen.  A harness failure does not
            # authorize launching the following arm because comparability is
            # no longer verifier-clean.
            break
    return receipts, errors


def execute(prepared: PreparedRun) -> int:
    prepared.output_root.mkdir(parents=True, exist_ok=False)
    start = {
        "schema_version": "issue825-selector-invocation-v1",
        "started_at": utc_now(),
        "run_manifest": prepared.manifest_ref,
        "parallel_cases": 2,
        "same_case_serial": True,
        "models_authorized": 4,
        "official_evaluator_invoked": False,
    }
    atomic_write(prepared.output_root / "invocation-start.json", canonical(start))
    receipts: list[dict[str, Any]] = []
    errors: list[str] = []
    with ThreadPoolExecutor(max_workers=2, thread_name_prefix="issue825") as executor:
        futures = {executor.submit(execute_case, prepared, case): case for case in prepared.cases}
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
        "all_four_episodes_recorded": len(receipts) == 4,
        "evaluator_authorizations": sum(receipt.get("evaluator_authorized") is True for receipt in receipts),
        "official_evaluator_invoked": False,
    }
    atomic_write(prepared.output_root / "invocation-result.json", canonical(result))
    return 0 if not errors and len(receipts) == 4 else 1


def preflight_summary(prepared: PreparedRun) -> dict[str, Any]:
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
        },
        "output_root_absent": not prepared.output_root.exists(),
        "cases": [
            {
                "rank": case.rank,
                "case_id": case.case_id,
                "base_commit": case.base_commit,
                "base_tree": case.base_tree,
                "root": case.root,
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
