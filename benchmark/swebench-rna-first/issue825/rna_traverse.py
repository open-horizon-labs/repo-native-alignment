#!/usr/bin/env python3
"""Fail-closed projection of pinned RNA graph traversal output."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time


HERE = Path(__file__).resolve().parent.parent
CONFIG = json.loads((HERE / "config/supervisor.json").read_text())
EMPTY_RE = re.compile(r"^No (?:dependents|neighbors) found for `[^`]+` within [0-9]+ hops\.$")
READY = "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"
CODE_KINDS = {
    "class", "const", "enum", "function", "interface", "method", "module",
    "struct", "trait", "type", "type_alias", "union",
}
GITHUB_REPOSITORY_PATTERN = re.compile(
    r"(?P<owner>[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)/"
    r"(?P<repository>[A-Za-z0-9._-]+)"
)


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cache_inventory_sha256(cache: Path) -> str:
    if not cache.is_dir() or cache.is_symlink():
        raise ValueError("operational_cache")
    members: list[dict] = []
    for path in sorted(cache.rglob("*"), key=lambda item: item.relative_to(cache).as_posix()):
        relative = path.relative_to(cache).as_posix()
        if path.is_symlink():
            raise ValueError(f"operational_cache_symlink:{relative}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError(f"operational_cache_non_file:{relative}")
        before = path.stat()
        digest = sha_file(path)
        after = path.stat()
        if (
            before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns
        ) != (
            after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns
        ):
            raise ValueError(f"operational_cache_changed:{relative}")
        members.append({"path": relative, "bytes": after.st_size, "sha256": digest})
    if not members:
        raise ValueError("operational_cache_empty")
    return sha(canonical({
        "schema_version": "issue825-operational-cache-inventory-v1",
        "members": members,
    }))


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise ValueError(f"git_{'_'.join(args)}")
    return result.stdout.decode("utf-8", errors="strict").strip()


def canonical_repository_slug(value: object) -> str:
    if not isinstance(value, str):
        raise ValueError("repository_identity")
    match = GITHUB_REPOSITORY_PATTERN.fullmatch(value)
    if match is None:
        raise ValueError("repository_identity")
    return f"{match.group('owner').lower()}/{match.group('repository').lower()}"


def canonical_github_origin(value: str) -> str:
    candidate = value.removesuffix("/").removesuffix(".git")
    for prefix in (
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ):
        if candidate.startswith(prefix):
            return canonical_repository_slug(candidate.removeprefix(prefix))
    raise ValueError("live_repository_origin")


def require_live_state(identity: dict) -> None:
    repo = Path(CONFIG["repo"])
    if not repo.is_dir() or repo.is_symlink():
        raise ValueError("live_repo")
    if git(repo, "rev-parse", "HEAD") != CONFIG["expected_base_commit"]:
        raise ValueError("live_HEAD")
    if git(repo, "rev-parse", "HEAD^{tree}") != CONFIG["expected_base_tree"]:
        raise ValueError("live_tree")
    if git(repo, "status", "--porcelain=v1", "--untracked-files=all"):
        raise ValueError("live_checkout_not_pristine")
    origins = git(repo, "remote", "get-url", "--all", "origin").splitlines()
    if len(origins) != 1:
        raise ValueError("live_repository_origin_count")
    live_repository = canonical_github_origin(origins[0])
    if live_repository != CONFIG["expected_repository_identity"]:
        raise ValueError("live_repository_identity")
    if identity.get("live_repository_identity") != live_repository:
        raise ValueError("identity_live_repository_identity")
    observed = cache_inventory_sha256(repo / ".oh/.cache")
    if observed != CONFIG["expected_cache_inventory_sha256"]:
        raise ValueError("live_cache_inventory")
    if identity.get("operational_cache_inventory_sha256") != observed:
        raise ValueError("identity_cache_inventory")


def stable_code_ids(text: str) -> list[str]:
    ids: set[str] = set()
    for candidate in re.findall(r"`([^`\r\n]+)`", text):
        parts = candidate.rsplit(":", 1)
        if len(parts) == 2 and parts[1] in CODE_KINDS and ":" in parts[0] and not any(ch.isspace() for ch in candidate):
            ids.add(candidate)
    return sorted(ids)


def require_identity() -> dict:
    if CONFIG.get("schema_version") != "rna-supervisor-config-v3":
        raise ValueError("supervisor_config_schema")
    path = Path(CONFIG["identity_receipt"])
    if not path.is_file() or path.is_symlink() or sha_file(path) != CONFIG["expected_identity_sha256"]:
        raise ValueError("identity_receipt")
    identity = json.loads(path.read_text())
    expected = {
        "schema_version": "issue825-runtime-identity-v2",
        "root": CONFIG["root"],
        "expected_repository_identity": CONFIG["expected_repository_identity"],
        "live_repository_identity": CONFIG["expected_repository_identity"],
        "base_commit": CONFIG["expected_base_commit"],
        "base_tree": CONFIG["expected_base_tree"],
        "producer_commit": CONFIG["expected_producer_commit"],
        "cache_manifest_sha256": CONFIG["expected_cache_manifest_sha256"],
        "cache_archive_sha256": CONFIG["expected_cache_archive_sha256"],
        "operational_cache_inventory_sha256": CONFIG["expected_cache_inventory_sha256"],
        "launcher_sha256": CONFIG["expected_launcher_sha256"],
        "binary_sha256": CONFIG["expected_binary_sha256"],
        "cache_bindings_verified": True,
        "fresh_reopen_ready": True,
    }
    for key, value in expected.items():
        if identity.get(key) != value:
            raise ValueError(f"identity_{key}")
    if Path(identity.get("index_checkout", "")).resolve(strict=True) != Path(CONFIG["repo"]).resolve(strict=True):
        raise ValueError("identity_index_checkout")
    if Path(identity.get("launcher_path", "")).resolve(strict=True) != Path(CONFIG["launcher"]).resolve(strict=True):
        raise ValueError("identity_launcher_path")
    if sha_file(Path(CONFIG["launcher"])) != CONFIG["expected_launcher_sha256"]:
        raise ValueError("launcher_tampered")
    binary = Path(identity.get("binary_path", ""))
    if not binary.is_file() or binary.is_symlink() or sha_file(binary) != CONFIG["expected_binary_sha256"]:
        raise ValueError("binary_tampered")
    for name in ("cache_manifest", "cache_verification_receipt", "readiness_report"):
        ref = identity.get(name)
        if not isinstance(ref, dict) or set(ref) != {"path", "sha256", "bytes"}:
            raise ValueError(f"identity_{name}")
        bound = Path(ref["path"])
        if not bound.is_file() or bound.is_symlink() or bound.stat().st_size != ref["bytes"] or sha_file(bound) != ref["sha256"]:
            raise ValueError(f"identity_{name}_tampered")
    require_live_state(identity)
    return identity


def read_state() -> dict:
    path = Path(CONFIG["state"])
    if path.exists():
        return json.loads(path.read_text())
    return {"schema_version": "rna-supervisor-state-v1", "fatal": False, "first_traversal_succeeded": False, "model_tool_attempts": 0, "rna_calls": 0}


def write_state(state: dict) -> None:
    path = Path(CONFIG["state"])
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_bytes(canonical(state))
    temporary.replace(path)


def fail(state: dict, reason: str, receipt: dict | None = None) -> int:
    state["fatal"] = True
    state["fatal_reason"] = reason
    write_state(state)
    if receipt is not None:
        receipt["classification"] = "ERROR"
        receipt["fatal_reason"] = reason
        persist(receipt)
    print(f"RNA_STATUS=ERROR reason={reason}", file=sys.stderr)
    return 43


def persist(receipt: dict) -> None:
    directory = Path(CONFIG["rna_events"])
    directory.mkdir(parents=True, exist_ok=True)
    sequence = int(receipt["sequence"])
    (directory / f"{sequence:04d}.stdout").write_bytes(receipt.pop("_stdout"))
    (directory / f"{sequence:04d}.stderr").write_bytes(receipt.pop("_stderr"))
    (directory / f"{sequence:04d}.json").write_bytes(canonical(receipt))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--node", required=True)
    parser.add_argument("--mode", required=True, choices=("neighbors", "impact"))
    args = parser.parse_args()

    state = read_state()
    if state.get("fatal"):
        print("RNA_STATUS=ERROR reason=episode_already_fatal", file=sys.stderr)
        return 43

    try:
        identity = require_identity()
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        return fail(state, f"identity:{exc}")

    sequence = int(state.get("rna_calls", 0)) + 1
    state["rna_calls"] = sequence
    response_path = Path(CONFIG["initial_response"])
    try:
        if (
            not response_path.is_file()
            or response_path.is_symlink()
            or sha_file(response_path) != CONFIG["initial_response_sha256"]
        ):
            raise ValueError("initial_response_identity")
        response = response_path.read_text(encoding="utf-8", errors="strict")
        initial_ids = set(stable_code_ids(response))
        if initial_ids != set(CONFIG["initial_ids"]):
            raise ValueError("initial_response_ids")
    except (OSError, UnicodeError, ValueError) as exc:
        return fail(state, f"injected_response:{exc}")
    if not state.get("first_traversal_succeeded"):
        if args.mode != "neighbors":
            receipt = {"schema_version":"rna-traversal-receipt-v1","sequence":sequence,"node":args.node,"mode":args.mode,"argv":[],"returncode":None,"elapsed_seconds":0.0,"stdout_bytes":0,"stdout_sha256":sha(b""),"stderr_bytes":0,"stderr_sha256":sha(b""),"_stdout":b"","_stderr":b""}
            return fail(state, "first_traversal_must_use_neighbors", receipt)
        if args.node not in initial_ids:
            receipt = {"schema_version":"rna-traversal-receipt-v1","sequence":sequence,"node":args.node,"mode":args.mode,"argv":[],"returncode":None,"elapsed_seconds":0.0,"stdout_bytes":0,"stdout_sha256":sha(b""),"stderr_bytes":0,"stderr_sha256":sha(b""),"_stdout":b"","_stderr":b""}
            return fail(state, "first_node_not_in_injected_response", receipt)

    argv = [
        CONFIG["launcher"], "search", "--repo", CONFIG["repo"], "--root", CONFIG["root"],
        "--node", args.node, "--mode", args.mode, "--compact",
    ]
    started = time.monotonic()
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    elapsed = time.monotonic() - started
    stdout = result.stdout
    stderr = result.stderr
    text = stdout.decode("utf-8", errors="replace")
    receipt = {
        "schema_version": "rna-traversal-receipt-v1",
        "sequence": sequence,
        "node": args.node,
        "mode": args.mode,
        "argv": argv,
        "returncode": result.returncode,
        "root": CONFIG["root"],
        "identity_sha256": CONFIG["expected_identity_sha256"],
        "cache_manifest_sha256": identity["cache_manifest_sha256"],
        "cache_archive_sha256": identity["cache_archive_sha256"],
        "launcher_sha256": identity["launcher_sha256"],
        "binary_sha256": identity["binary_sha256"],
        "repository_identity": identity["live_repository_identity"],
        "elapsed_seconds": elapsed,
        "stdout_bytes": len(stdout),
        "stdout_sha256": sha(stdout),
        "stderr_bytes": len(stderr),
        "stderr_sha256": sha(stderr),
        "_stdout": stdout,
        "_stderr": stderr,
    }

    if result.returncode != 0:
        return fail(state, f"launcher_exit_{result.returncode}", receipt)
    if "*Index:" not in text or "### Capability readiness" not in text:
        return fail(state, "missing_terminal_identity", receipt)
    if not any(line.strip("`") == READY for line in text.splitlines()):
        return fail(state, "readiness_not_exact", receipt)

    graph_start = text.find("## Graph")
    index_start = text.find("*Index:")
    if graph_start >= 0 and index_start > graph_start:
        projection = text[graph_start:index_start].rstrip() + "\n"
        # Require at least one rendered result, not merely a heading.
        if not re.search(r"(?m)^- \*\*[^*]+\*\*", projection):
            return fail(state, "empty_or_malformed_graph", receipt)
        classification = "OK_NONEMPTY"
    else:
        empty_lines = [line for line in text.splitlines() if EMPTY_RE.fullmatch(line)]
        if len(empty_lines) != 1:
            return fail(state, "unrecognized_success_rendering", receipt)
        projection = empty_lines[0] + "\n"
        classification = "OK_EMPTY"

    receipt["classification"] = classification
    receipt["projection_bytes"] = len(projection.encode())
    receipt["projection_sha256"] = sha(projection.encode())
    persist(receipt)
    state["last_classification"] = classification
    if sequence == 1:
        state["first_traversal_succeeded"] = True
        state["first_traversal_status"] = classification
        state["first_node"] = args.node
    write_state(state)
    sys.stdout.write(f"RNA_STATUS={classification}\n{projection}")
    return 0


if __name__ == "__main__":
    lock = Path(CONFIG["lock"])
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("ab") as handle_lock:
        fcntl.flock(handle_lock, fcntl.LOCK_EX)
        raise SystemExit(main())
