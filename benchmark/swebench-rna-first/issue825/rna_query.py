#!/usr/bin/env python3
"""Opaque exact-title acquisition with raw, identity-bound evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time


HERE = Path(__file__).resolve().parent.parent
CONFIG = json.loads((HERE / "config/supervisor.json").read_text())

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


def stable_code_ids(text: str) -> list[str]:
    ids: set[str] = set()
    for candidate in re.findall(r"`([^`\r\n]+)`", text):
        parts = candidate.rsplit(":", 1)
        if len(parts) == 2 and parts[1] in CODE_KINDS and ":" in parts[0] and not any(ch.isspace() for ch in candidate):
            ids.add(candidate)
    return sorted(ids)


def persist_raw(
    argv: list[str],
    result: subprocess.CompletedProcess[bytes],
    elapsed: float,
    identity: dict,
    raw_ids: list[str],
    projected_ids: list[str],
    projection_sha256: str | None,
) -> None:
    directory = Path(CONFIG["query_events"])
    directory.mkdir(parents=True, exist_ok=True)
    stdout_path = directory / "title-query.stdout"
    stderr_path = directory / "title-query.stderr"
    receipt_path = directory / "title-query.json"
    if any(path.exists() for path in (stdout_path, stderr_path, receipt_path)):
        raise ValueError("query_evidence_already_exists")
    stdout_path.write_bytes(result.stdout)
    stderr_path.write_bytes(result.stderr)
    receipt = {
        "schema_version": "issue825-title-query-receipt-v1",
        "argv": argv,
        "root": CONFIG["root"],
        "identity_sha256": CONFIG["expected_identity_sha256"],
        "cache_manifest_sha256": identity["cache_manifest_sha256"],
        "cache_archive_sha256": identity["cache_archive_sha256"],
        "launcher_sha256": identity["launcher_sha256"],
        "binary_sha256": identity["binary_sha256"],
        "repository_identity": identity["live_repository_identity"],
        "elapsed_seconds": elapsed,
        "returncode": result.returncode,
        "stdout": {"path": str(stdout_path), "bytes": len(result.stdout), "sha256": sha(result.stdout)},
        "stderr": {"path": str(stderr_path), "bytes": len(result.stderr), "sha256": sha(result.stderr)},
        "raw_stable_code_ids": raw_ids,
        "projected_stable_code_ids": projected_ids,
        "projection_sha256": projection_sha256,
    }
    receipt_path.write_bytes(canonical(receipt))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--query-sha256", required=True)
    args = parser.parse_args()
    query = (HERE / "title-query.txt").read_bytes()
    if query.endswith(b"\n"):
        query = query[:-1]
    expected_query_sha = CONFIG["expected_query_sha256"]
    if args.query_sha256 != expected_query_sha or hashlib.sha256(query).hexdigest() != expected_query_sha:
        print("RNA_QUERY_STATUS=ERROR query_identity", file=sys.stderr)
        return 42
    try:
        identity = require_identity()
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"RNA_QUERY_STATUS=ERROR identity:{exc}", file=sys.stderr)
        return 42
    argv = [
        CONFIG["launcher"], "search", "--repo", CONFIG["repo"], "--root", CONFIG["root"],
        "--compact", "--include-artifacts=false", f"--limit={CONFIG['result_limit']}", query.decode("utf-8"),
    ]
    started = time.monotonic()
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    elapsed = time.monotonic() - started
    text = result.stdout.decode("utf-8", errors="replace")
    marker = "### Strict semantic qualification"
    projection_body = text.split(marker, 1)[0]
    ready = next((line for line in text.splitlines() if line.strip("`") == READY), None)
    projection = None
    projected_ids: list[str] = []
    if result.returncode == 0 and ready is not None:
        projection = projection_body.rstrip() + "\n\n" + ready + "\n"
        projected_ids = stable_code_ids(projection)
    try:
        persist_raw(
            argv,
            result,
            elapsed,
            identity,
            stable_code_ids(text),
            projected_ids,
            sha(projection.encode()) if projection is not None else None,
        )
    except (OSError, ValueError) as exc:
        print(f"RNA_QUERY_STATUS=ERROR evidence:{exc}", file=sys.stderr)
        return 42
    if result.returncode != 0:
        sys.stderr.buffer.write(result.stderr)
        return 42
    if ready is None:
        print("RNA_QUERY_STATUS=ERROR readiness", file=sys.stderr)
        return 42
    if not projected_ids:
        print("RNA_QUERY_STATUS=ERROR no_stable_code_id", file=sys.stderr)
        return 42
    assert projection is not None
    sys.stdout.write(projection)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
