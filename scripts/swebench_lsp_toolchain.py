#!/usr/bin/env python3
"""Build and verify the frozen SWE-bench LSP toolchain evidence.

The network-enabled acquisition phase populates a bare git cache. Inventory,
artifact acquisition, and lock verification are deliberately separate so
cache verification can run with outbound networking disabled. The inventory
uses an explicit language-addressability boundary: real code, documentation,
configuration, templates, and ambiguous text stay mandatory; only evidenced
data/assets receive non-language exclusions.
"""

from __future__ import annotations

import argparse
import collections
import contextlib
import gzip
import hashlib
import io
import json
import math
import os
import platform
import select
import shlex
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO, Iterator, Mapping, Sequence


SCHEMA_VERSION = 1
STRUCTURAL_CACHE_SCHEMA_VERSION = 1
STRUCTURAL_CACHE_ROOT = "cache"
STRUCTURAL_CACHE_CORE = ".rna-structural-cache-core.json"
STRUCTURAL_CACHE_CATALOG = "structural-cache-catalog.json"
STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV = (
    "RNA_STRUCTURAL_CACHE_AUTHORIZATION_SHA256"
)
STRUCTURAL_CACHE_MAX_MEMBERS = 250_000
STRUCTURAL_CACHE_MAX_MEMBER_BYTES = 8 * 1024 * 1024 * 1024
STRUCTURAL_CACHE_MAX_TOTAL_BYTES = 64 * 1024 * 1024 * 1024
STRUCTURAL_CACHE_FORBIDDEN_COMPONENTS = frozenset(
    {"embedding", "embeddings", "rerank", "reranker", "vectors", "vector-index"}
)
QUALIFICATION_SCAN_FLAGS = [
    "--business-context=disabled",
    "scan",
    "--full",
    "--no-embed",
    "--timings",
]
EXPECTED_POPULATION_SIZE = 70
BINARY_EXTENSIONS = frozenset(
    {
        "7z",
        "bz2",
        "db",
        "dll",
        "dylib",
        "eot",
        "exe",
        "gif",
        "gz",
        "ico",
        "jar",
        "jpeg",
        "jpg",
        "mov",
        "mp3",
        "mp4",
        "o",
        "obj",
        "pdf",
        "png",
        "pyo",
        "pyc",
        "so",
        "sqlite",
        "tar",
        "ttf",
        "wav",
        "webp",
        "woff",
        "woff2",
        "xz",
        "zip",
    }
)
VENDOR_COMPONENTS = frozenset(
    {"vendor", "node_modules", "third_party", "external", ".venv", "venv"}
)
GENERATED_COMPONENTS = frozenset(
    {
        "target",
        "build",
        "dist",
        "out",
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        ".tox",
    }
)

# These are text encodings of data or presentation assets, not documents that
# an author edits as source. Unknown suffixes are intentionally absent: they
# remain mandatory and force an explicit decision instead of being swept into
# a permissive fallback.
TEXT_DATA_EXTENSIONS = frozenset(
    {
        "62-now",
        "afm",
        "csv",
        "dat",
        "dbout",
        "ecsv",
        "eml",
        "fits",
        "geojson",
        "hdr",
        "ict",
        "interp",
        "list",
        "map",
        "pristine",
        "prj",
        "rdb",
        "tab",
        "tokens",
        "vrt",
    }
)
TEXT_ASSET_EXTENSIONS = frozenset(
    {"enc", "eps", "graffle", "pem", "svg"}
)
DOC_EXTENSIONS = frozenset(
    {
        "1",
        "bib",
        "breaking",
        "bugfix",
        "eopc04_iau2000",
        "extension",
        "false_negative",
        "false_positive",
        "feature",
        "finals2000a",
        "inc",
        "internal",
        "lesser",
        "license",
        "md",
        "new_check",
        "old",
        "other",
        "performance",
        "pil",
        "rst",
        "rst_t",
        "user_action",
        "wx",
    }
)
CONFIG_EXTENSIONS = frozenset(
    {
        "cff",
        "cfg",
        "conf",
        "ini",
        "json",
        "lock",
        "mplstyle",
        "rc",
        "toml",
        "yaml",
        "yml",
    }
)
CONFIG_FILENAMES = frozenset(
    {
        "dockerfile",
        "makefile",
        "cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.cfg",
        "tox.ini",
    }
)
DOC_FILENAME_PREFIXES = (
    "authors",
    "changes",
    "changelog",
    "copying",
    "history",
    "license",
    "news",
    "readme",
)
KNOWN_TEST_FIXTURE_EXTENSIONS = frozenset(
    {"foo", "ignoreme", "out", "tmp", "unkn", "unknown", "xyz"}
)
NON_LANGUAGE_MARKER_FILENAMES = frozenset({".gitkeep", ".keep", "py.typed"})
NON_LANGUAGE_TEST_FILENAMES = frozenset(
    {
        ".dot-file",
        ".hidden",
        "backup~",
        "cvs",
        "file_txt",
        "not_utf8.sample",
        "visible",
    }
)


class ToolchainError(RuntimeError):
    """A fail-closed toolchain validation error."""


def canonical_json(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    ).encode()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ToolchainError(f"unable to read {label}") from error
    if not isinstance(value, dict):
        raise ToolchainError(f"{label} must be a JSON object")
    return value


def write_canonical_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_bytes(canonical_json(value))
    temporary.replace(path)


def _publish_canonical_json_exclusive(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() or path.is_symlink():
        raise ToolchainError(f"refusing to overwrite immutable evidence: {path}")
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    if temporary.exists() or temporary.is_symlink():
        raise ToolchainError("immutable evidence staging path already exists")
    try:
        temporary.write_bytes(canonical_json(value))
        try:
            os.link(temporary, path)
        except FileExistsError as error:
            raise ToolchainError(f"refusing to overwrite immutable evidence: {path}") from error
    finally:
        temporary.unlink(missing_ok=True)


def _normalized_tar_info(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 1_577_836_800
    if info.isdir():
        info.mode = 0o755
    elif info.isfile():
        info.mode = 0o755 if info.mode & 0o111 else 0o644
    elif info.issym():
        info.mode = 0o777
    return info


def seal_directory(source: Path, output: Path, root_name: str) -> dict[str, Any]:
    """Create a byte-reproducible gzip-compressed ustar archive."""
    if not source.is_dir():
        raise ToolchainError(f"seal source is not a directory: {source}")
    if not root_name or "/" in root_name or root_name in {".", ".."}:
        raise ToolchainError("seal root name must be one path component")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.seal-{os.getpid()}")
    try:
        with temporary.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(
                    fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT
                ) as archive:
                    paths = [source, *sorted(source.rglob("*"), key=lambda path: path.as_posix())]
                    for path in paths:
                        relative = path.relative_to(source)
                        archive_name = (
                            root_name
                            if relative == Path(".")
                            else f"{root_name}/{relative.as_posix()}"
                        )
                        info = _normalized_tar_info(
                            archive.gettarinfo(str(path), arcname=archive_name)
                        )
                        if info.isfile():
                            with path.open("rb") as handle:
                                archive.addfile(info, handle)
                        else:
                            archive.addfile(info)
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)
    return {
        "schema_version": SCHEMA_VERSION,
        "artifact": output.name,
        "sha256": sha256_file(output),
        "root_name": root_name,
    }


def _require_exact_fields(
    value: Mapping[str, Any], expected: set[str], label: str
) -> None:
    actual = set(value)
    if actual != expected:
        raise ToolchainError(
            f"{label} fields mismatch: missing={sorted(expected - actual)}, "
            f"unknown={sorted(actual - expected)}"
        )


def _require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ToolchainError(f"{label} must be a nonempty string")
    return value


def _require_sha256(value: Any, label: str) -> str:
    value = _require_string(value, label).lower()
    if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
        raise ToolchainError(f"{label} must be a hexadecimal SHA-256")
    return value


def _require_git_oid(value: Any, label: str) -> str:
    value = _require_string(value, label).lower()
    if len(value) != 40 or any(character not in "0123456789abcdef" for character in value):
        raise ToolchainError(f"{label} must be a 40-character Git object ID")
    return value


def _validate_producer_identity(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolchainError("producer identity must be an object")
    _require_exact_fields(
        value,
        {
            "producer_commit",
            "package_version",
            "binary_sha256",
            "graph_schema_version",
            "graph_schema_signature",
            "completeness_schema_version",
            "work_item_schema_version",
            "validation_evidence_schema_version",
        },
        "producer identity",
    )
    _require_git_oid(value["producer_commit"], "producer commit")
    _require_string(value["package_version"], "producer package version")
    _require_sha256(value["binary_sha256"], "producer binary digest")
    _require_sha256(value["graph_schema_signature"], "graph schema signature")
    for field in (
        "graph_schema_version",
        "completeness_schema_version",
        "work_item_schema_version",
        "validation_evidence_schema_version",
    ):
        if type(value[field]) is not int or value[field] <= 0:
            raise ToolchainError(f"producer {field} must be a positive integer")
    return dict(value)


def structural_cache_identity(rna_binary: Path, checkout: Path) -> dict[str, Any]:
    completed = subprocess.run(
        [
            str(rna_binary.resolve()),
            "--business-context",
            "disabled",
            "structural-cache-identity",
            "--repo",
            str(checkout.resolve()),
        ],
        cwd=checkout,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise ToolchainError(
            "RNA structural-cache identity failed: " + completed.stderr.strip()[-2000:]
        )
    try:
        identity = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ToolchainError("RNA structural-cache identity is not JSON") from error
    if not isinstance(identity, dict):
        raise ToolchainError("RNA structural-cache identity must be an object")
    _require_exact_fields(
        identity,
        {
            "schema_version",
            "repository",
            "commit",
            "tree",
            "root_slug",
            "configuration_digest",
            "inventory_policy_digest",
            "context_mode",
            "producer",
            "shared_influence_digest",
            "partitions",
        },
        "RNA structural-cache identity",
    )
    if identity["schema_version"] != STRUCTURAL_CACHE_SCHEMA_VERSION:
        raise ToolchainError("RNA structural-cache identity schema mismatch")
    _require_string(identity["repository"], "repository")
    _require_git_oid(identity["commit"], "target commit")
    _require_git_oid(identity["tree"], "target tree")
    _require_string(identity["root_slug"], "root slug")
    _require_string(identity["configuration_digest"], "configuration digest")
    _require_string(identity["inventory_policy_digest"], "inventory policy digest")
    if identity["context_mode"] != "disabled":
        raise ToolchainError("structural cache identity must use disabled business context")
    identity["producer"] = _validate_producer_identity(identity["producer"])
    _require_sha256(identity["shared_influence_digest"], "shared influence digest")
    partitions = identity["partitions"]
    if not isinstance(partitions, dict) or not partitions:
        raise ToolchainError("structural-cache identity partitions must be nonempty")
    for language, partition in partitions.items():
        if not isinstance(partition, dict):
            raise ToolchainError(f"partition {language} must be an object")
        _require_exact_fields(
            partition,
            {
                "language",
                "descriptor_signature",
                "influence_patterns",
                "influence_digest",
                "signature",
                "matched_file_count",
            },
            f"partition {language}",
        )
        if partition["language"] != language:
            raise ToolchainError(f"partition key/language mismatch for {language}")
        _require_sha256(partition["descriptor_signature"], f"{language} descriptor")
        _require_sha256(partition["influence_digest"], f"{language} influence")
        _require_sha256(partition["signature"], f"{language} partition")
        patterns = partition["influence_patterns"]
        if not isinstance(patterns, list) or any(
            not isinstance(pattern, str) or not pattern for pattern in patterns
        ):
            raise ToolchainError(f"{language} influence patterns must be strings")
        if patterns != sorted(set(patterns)):
            raise ToolchainError(f"{language} influence patterns must be canonical")
        if type(partition["matched_file_count"]) is not int or partition["matched_file_count"] < 0:
            raise ToolchainError(f"{language} matched_file_count is invalid")
    return identity


def _normalized_cache_path(value: str, label: str = "cache member") -> str:
    if not value or "\0" in value or "\\" in value:
        raise ToolchainError(f"{label} path is empty, contains NUL, or uses backslashes")
    pure = PurePosixPath(value)
    if pure.is_absolute() or value.startswith("/"):
        raise ToolchainError(f"{label} path is absolute")
    parts = pure.parts
    if any(part in {"", ".", ".."} for part in parts):
        raise ToolchainError(f"{label} path is not normalized: {value}")
    if parts and len(parts[0]) >= 2 and parts[0][1] == ":":
        raise ToolchainError(f"{label} path has a drive prefix: {value}")
    normalized = pure.as_posix()
    if normalized != value:
        raise ToolchainError(f"{label} path is not canonical: {value}")
    lowered = {part.casefold() for part in parts}
    if lowered & STRUCTURAL_CACHE_FORBIDDEN_COMPONENTS:
        raise ToolchainError(f"{label} contains embeddings/rerank payload: {value}")
    return normalized


def _structural_cache_files(cache_root: Path) -> list[dict[str, Any]]:
    if not cache_root.is_dir() or cache_root.is_symlink():
        raise ToolchainError("structural cache root must be a real directory")
    members: list[dict[str, Any]] = []
    total_size = 0
    for path in sorted(cache_root.rglob("*"), key=lambda candidate: candidate.as_posix()):
        relative = _normalized_cache_path(path.relative_to(cache_root).as_posix())
        stat_result = path.lstat()
        if path.is_symlink():
            raise ToolchainError(f"structural cache contains a symlink: {relative}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ToolchainError(f"structural cache contains a special file: {relative}")
        if relative == STRUCTURAL_CACHE_CORE:
            raise ToolchainError("live cache contains a reserved embedded core manifest")
        size = stat_result.st_size
        if size > STRUCTURAL_CACHE_MAX_MEMBER_BYTES:
            raise ToolchainError(f"structural cache member is oversized: {relative}")
        total_size += size
        if total_size > STRUCTURAL_CACHE_MAX_TOTAL_BYTES:
            raise ToolchainError("structural cache exceeds total size limit")
        members.append(
            {
                "path": relative,
                "sha256": sha256_file(path),
                "size_bytes": size,
                "mode": 0o755 if stat_result.st_mode & 0o111 else 0o644,
            }
        )
        if len(members) > STRUCTURAL_CACHE_MAX_MEMBERS:
            raise ToolchainError("structural cache contains too many members")
    paths = [member["path"] for member in members]
    if len(paths) != len(set(paths)) or len(paths) != len({path.casefold() for path in paths}):
        raise ToolchainError("structural cache paths collide")
    return members


def _structural_tree_digest(members: Sequence[Mapping[str, Any]]) -> str:
    return sha256_bytes(canonical_json(list(members)))


def _validate_structural_core(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolchainError("structural cache core must be an object")
    _require_exact_fields(
        value,
        {
            "schema_version",
            "status",
            "offline_preprocessing",
            "repository",
            "commit",
            "tree",
            "root_slug",
            "producer",
            "toolchain_lock_digest",
            "inventory_digest",
            "inventory_file_sha256",
            "case_inventory_digest",
            "configuration_digest",
            "inventory_policy_digest",
            "scan_flags",
            "completeness_report_digest",
            "completeness_report_sha256",
            "graph_snapshot_digest",
            "shared_influence_digest",
            "partition_signatures",
            "members",
            "structural_cache_tree_digest",
            "base_cache",
        },
        "structural cache core",
    )
    if value["schema_version"] != STRUCTURAL_CACHE_SCHEMA_VERSION:
        raise ToolchainError("structural cache core schema mismatch")
    if value["status"] != "ready" or value["offline_preprocessing"] is not True:
        raise ToolchainError("structural cache core is not reusable READY evidence")
    _require_string(value["repository"], "cache repository")
    _require_git_oid(value["commit"], "cache commit")
    _require_git_oid(value["tree"], "cache tree")
    _require_string(value["root_slug"], "cache root slug")
    value["producer"] = _validate_producer_identity(value["producer"])
    for field in (
        "toolchain_lock_digest",
        "inventory_digest",
        "inventory_file_sha256",
        "case_inventory_digest",
        "completeness_report_sha256",
        "shared_influence_digest",
        "structural_cache_tree_digest",
    ):
        _require_sha256(value[field], field)
    for field in (
        "configuration_digest",
        "inventory_policy_digest",
        "completeness_report_digest",
        "graph_snapshot_digest",
    ):
        _require_string(value[field], field)
    scan_flags = value["scan_flags"]
    if (
        not isinstance(scan_flags, list)
        or not scan_flags
        or any(not isinstance(flag, str) or not flag for flag in scan_flags)
        or scan_flags != list(dict.fromkeys(scan_flags))
    ):
        raise ToolchainError("structural cache scan flags are invalid")
    partitions = value["partition_signatures"]
    if not isinstance(partitions, dict) or any(
        not isinstance(language, str)
        or not language
        or not isinstance(signature, str)
        or len(signature) != 64
        for language, signature in partitions.items()
    ):
        raise ToolchainError("structural cache partition signatures are invalid")
    members = value["members"]
    if not isinstance(members, list) or not members:
        raise ToolchainError("structural cache core must declare members")
    normalized_members = []
    seen: set[str] = set()
    seen_folded: set[str] = set()
    total = 0
    for index, member in enumerate(members):
        if not isinstance(member, dict):
            raise ToolchainError(f"structural cache member {index} must be an object")
        _require_exact_fields(
            member, {"path", "sha256", "size_bytes", "mode"}, f"member {index}"
        )
        path = _normalized_cache_path(_require_string(member["path"], "member path"))
        if path in seen or path.casefold() in seen_folded:
            raise ToolchainError(f"duplicate/casefold cache member: {path}")
        seen.add(path)
        seen_folded.add(path.casefold())
        _require_sha256(member["sha256"], f"{path} digest")
        if type(member["size_bytes"]) is not int or not 0 <= member["size_bytes"] <= STRUCTURAL_CACHE_MAX_MEMBER_BYTES:
            raise ToolchainError(f"invalid cache member size: {path}")
        if member["mode"] not in {0o644, 0o755}:
            raise ToolchainError(f"invalid cache member mode: {path}")
        total += member["size_bytes"]
        normalized_members.append(dict(member))
    if total > STRUCTURAL_CACHE_MAX_TOTAL_BYTES or len(members) > STRUCTURAL_CACHE_MAX_MEMBERS:
        raise ToolchainError("structural cache exceeds safety limits")
    if normalized_members != sorted(normalized_members, key=lambda member: member["path"]):
        raise ToolchainError("structural cache members are not sorted")
    if _structural_tree_digest(normalized_members) != value["structural_cache_tree_digest"]:
        raise ToolchainError("structural cache tree digest mismatch")
    base_cache = value["base_cache"]
    if base_cache is not None:
        if not isinstance(base_cache, dict):
            raise ToolchainError("base cache lineage must be null or object")
        _require_exact_fields(
            base_cache,
            {"archive_sha256", "sidecar_sha256", "core_sha256", "report_digest"},
            "base cache lineage",
        )
        for field in ("archive_sha256", "sidecar_sha256", "core_sha256"):
            _require_sha256(base_cache[field], f"base {field}")
        _require_string(base_cache["report_digest"], "base report digest")
    value["members"] = normalized_members
    return dict(value)


def _write_structural_archive(cache_root: Path, core: Mapping[str, Any], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists() or output.is_symlink():
        raise ToolchainError(f"refusing to overwrite structural cache archive: {output}")
    temporary = output.with_name(f".{output.name}.tmp-{os.getpid()}")
    if temporary.exists() or temporary.is_symlink():
        raise ToolchainError("structural cache archive staging path already exists")
    core_bytes = canonical_json(core)
    try:
        with temporary.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as archive:
                    directories = {STRUCTURAL_CACHE_ROOT}
                    for member in core["members"]:
                        parts = PurePosixPath(member["path"]).parts
                        for length in range(1, len(parts)):
                            directories.add(
                                f"{STRUCTURAL_CACHE_ROOT}/" + "/".join(parts[:length])
                            )
                    for directory in sorted(directories):
                        info = tarfile.TarInfo(directory)
                        info.type = tarfile.DIRTYPE
                        info.mode = 0o755
                        info.uid = info.gid = 0
                        info.mtime = 1_577_836_800
                        archive.addfile(info)
                    for member in core["members"]:
                        source = cache_root / member["path"]
                        info = tarfile.TarInfo(
                            f"{STRUCTURAL_CACHE_ROOT}/{member['path']}"
                        )
                        info.size = member["size_bytes"]
                        info.mode = member["mode"]
                        info.uid = info.gid = 0
                        info.mtime = 1_577_836_800
                        with source.open("rb") as handle:
                            archive.addfile(info, handle)
                    core_info = tarfile.TarInfo(
                        f"{STRUCTURAL_CACHE_ROOT}/{STRUCTURAL_CACHE_CORE}"
                    )
                    core_info.size = len(core_bytes)
                    core_info.mode = 0o644
                    core_info.uid = core_info.gid = 0
                    core_info.mtime = 1_577_836_800
                    archive.addfile(core_info, io.BytesIO(core_bytes))
        try:
            os.link(temporary, output)
        except FileExistsError as error:
            raise ToolchainError(
                f"refusing to overwrite structural cache archive: {output}"
            ) from error
    finally:
        temporary.unlink(missing_ok=True)


def archive_structural_cache(
    checkout: Path,
    archive_path: Path,
    sidecar_path: Path,
    *,
    identity: Mapping[str, Any],
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
    case_inventory_digest: str,
    base_cache: Mapping[str, Any] | None,
) -> dict[str, Any]:
    if sidecar_path.exists() or sidecar_path.is_symlink():
        raise ToolchainError(
            f"refusing to overwrite structural cache manifest: {sidecar_path}"
        )
    staged_sidecar_path = sidecar_path.with_name(
        f".{sidecar_path.name}.verify-{os.getpid()}"
    )
    if staged_sidecar_path.exists() or staged_sidecar_path.is_symlink():
        raise ToolchainError(
            f"structural cache manifest verification path already exists: {staged_sidecar_path}"
        )
    cache_root = checkout / ".oh" / ".cache"
    report_path = cache_root / "lsp_completeness.json"
    report = load_json_object(report_path, "READY completeness report")
    if report.get("violations") != []:
        raise ToolchainError("cannot archive a cache whose completeness report is not READY")
    report_digest = _require_string(report.get("digest"), "completeness report digest")
    graph_snapshot_digest = _require_string(
        report.get("graph_snapshot_digest"), "graph snapshot digest"
    )
    members = _structural_cache_files(cache_root)
    partition_signatures = {
        language: partition["signature"]
        for language, partition in sorted(identity["partitions"].items())
    }
    core = _validate_structural_core(
        {
            "schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION,
            "status": "ready",
            "offline_preprocessing": True,
            "repository": identity["repository"],
            "commit": identity["commit"],
            "tree": identity["tree"],
            "root_slug": identity["root_slug"],
            "producer": identity["producer"],
            "toolchain_lock_digest": toolchain_lock_digest,
            "inventory_digest": inventory_digest,
            "inventory_file_sha256": inventory_file_sha256,
            "case_inventory_digest": case_inventory_digest,
            "configuration_digest": identity["configuration_digest"],
            "inventory_policy_digest": identity["inventory_policy_digest"],
            "scan_flags": QUALIFICATION_SCAN_FLAGS,
            "completeness_report_digest": report_digest,
            "completeness_report_sha256": sha256_file(report_path),
            "graph_snapshot_digest": graph_snapshot_digest,
            "shared_influence_digest": identity["shared_influence_digest"],
            "partition_signatures": partition_signatures,
            "members": members,
            "structural_cache_tree_digest": _structural_tree_digest(members),
            "base_cache": dict(base_cache) if base_cache is not None else None,
        }
    )
    _write_structural_archive(cache_root, core, archive_path)
    core_sha256 = sha256_bytes(canonical_json(core))
    sidecar = {
        "schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION,
        "publication_status": "ready",
        "archive_name": archive_path.name,
        "archive_size_bytes": archive_path.stat().st_size,
        "archive_sha256": sha256_file(archive_path),
        "core_sha256": core_sha256,
        "core": core,
    }
    try:
        # The final sidecar is the publication marker. Verify the complete
        # archive against a staged sidecar first so an archive assembled from
        # bytes that changed after member hashing can never be published READY.
        _publish_canonical_json_exclusive(staged_sidecar_path, sidecar)
        verified = verify_structural_cache_archive(
            archive_path,
            staged_sidecar_path,
            expected={
                "repository": identity["repository"],
                "root_slug": identity["root_slug"],
                "producer": identity["producer"],
                "toolchain_lock_digest": toolchain_lock_digest,
                "inventory_digest": inventory_digest,
                "inventory_file_sha256": inventory_file_sha256,
                "inventory_policy_digest": identity["inventory_policy_digest"],
                "scan_flags": QUALIFICATION_SCAN_FLAGS,
                "shared_influence_digest": identity["shared_influence_digest"],
            },
        )
        expected_verification = {
            "core": core,
            "archive_sha256": sidecar["archive_sha256"],
            "archive_size_bytes": sidecar["archive_size_bytes"],
            "core_sha256": core_sha256,
            "sidecar_sha256": sha256_file(staged_sidecar_path),
            "structural_cache_tree_digest": core["structural_cache_tree_digest"],
        }
        if verified != expected_verification:
            raise ToolchainError(
                "new structural cache archive verification identities differ from publication"
            )
        try:
            os.link(staged_sidecar_path, sidecar_path)
        except FileExistsError as error:
            raise ToolchainError(
                f"refusing to overwrite structural cache manifest: {sidecar_path}"
            ) from error
    finally:
        staged_sidecar_path.unlink(missing_ok=True)
    return {
        "archive_path": str(archive_path.resolve()),
        "archive_sha256": verified["archive_sha256"],
        "archive_size_bytes": verified["archive_size_bytes"],
        "core_sha256": verified["core_sha256"],
        "sidecar_path": str(sidecar_path.resolve()),
        "sidecar_sha256": sha256_file(sidecar_path),
        "structural_cache_tree_digest": verified["structural_cache_tree_digest"],
        "completeness_report_digest": report_digest,
        "graph_snapshot_digest": graph_snapshot_digest,
    }


def _validate_structural_sidecar(
    sidecar_path: Path, archive_path: Path
) -> tuple[dict[str, Any], dict[str, Any]]:
    if not sidecar_path.is_file() or sidecar_path.is_symlink():
        raise ToolchainError("structural cache sidecar is missing or is a symlink")
    sidecar = load_json_object(sidecar_path, "structural cache sidecar")
    _require_exact_fields(
        sidecar,
        {
            "schema_version",
            "publication_status",
            "archive_name",
            "archive_size_bytes",
            "archive_sha256",
            "core_sha256",
            "core",
        },
        "structural cache sidecar",
    )
    if sidecar["schema_version"] != STRUCTURAL_CACHE_SCHEMA_VERSION:
        raise ToolchainError("structural cache sidecar schema mismatch")
    if sidecar["publication_status"] != "ready":
        raise ToolchainError("structural cache sidecar is not a completed publication")
    if sidecar["archive_name"] != archive_path.name:
        raise ToolchainError("structural cache sidecar archive name mismatch")
    if type(sidecar["archive_size_bytes"]) is not int or sidecar["archive_size_bytes"] <= 0:
        raise ToolchainError("structural cache archive size is invalid")
    if not archive_path.is_file() or archive_path.is_symlink():
        raise ToolchainError("structural cache archive is missing or is a symlink")
    if archive_path.stat().st_size != sidecar["archive_size_bytes"]:
        raise ToolchainError("structural cache archive is partial or has changed size")
    if sha256_file(archive_path) != _require_sha256(
        sidecar["archive_sha256"], "archive digest"
    ):
        raise ToolchainError("structural cache archive digest mismatch")
    core = _validate_structural_core(sidecar["core"])
    if sha256_bytes(canonical_json(core)) != _require_sha256(
        sidecar["core_sha256"], "core digest"
    ):
        raise ToolchainError("structural cache core digest mismatch")
    return sidecar, core


def _validated_archive_name(name: str) -> tuple[str, bool]:
    normalized = _normalized_cache_path(name, "archive member")
    parts = PurePosixPath(normalized).parts
    if not parts or parts[0] != STRUCTURAL_CACHE_ROOT:
        raise ToolchainError(f"archive member is outside {STRUCTURAL_CACHE_ROOT}/: {name}")
    if len(parts) == 1:
        return "", True
    relative = "/".join(parts[1:])
    return _normalized_cache_path(relative, "archive cache member"), False


def _safe_checkout_cache_destination(checkout: Path) -> Path:
    if not checkout.is_dir() or checkout.is_symlink():
        raise ToolchainError("cache injection checkout must be a real directory")
    oh_root = checkout / ".oh"
    if oh_root.is_symlink() or (oh_root.exists() and not oh_root.is_dir()):
        raise ToolchainError("cache injection rejects a symlink/non-directory .oh")
    destination = oh_root / ".cache"
    if destination.is_symlink() or destination.exists():
        raise ToolchainError("cache injection destination already exists or is a symlink")
    oh_root.mkdir(parents=False, exist_ok=True)
    if oh_root.is_symlink():
        raise ToolchainError("cache injection .oh changed to a symlink")
    return destination


def verify_structural_cache_archive(
    archive_path: Path,
    sidecar_path: Path,
    *,
    expected: Mapping[str, Any] | None = None,
    inject_checkout: Path | None = None,
    materialize_cache: Path | None = None,
) -> dict[str, Any]:
    archive_before = sha256_file(archive_path) if archive_path.is_file() else None
    sidecar_before = sha256_file(sidecar_path) if sidecar_path.is_file() else None
    sidecar, core = _validate_structural_sidecar(sidecar_path, archive_path)
    if expected is not None:
        comparisons = {
            "repository": core["repository"],
            "root_slug": core["root_slug"],
            "producer": core["producer"],
            "toolchain_lock_digest": core["toolchain_lock_digest"],
            "inventory_digest": core["inventory_digest"],
            "inventory_file_sha256": core["inventory_file_sha256"],
            "inventory_policy_digest": core["inventory_policy_digest"],
            "scan_flags": core["scan_flags"],
            "shared_influence_digest": core["shared_influence_digest"],
        }
        for field, actual in comparisons.items():
            if field in expected and expected[field] != actual:
                raise ToolchainError(f"structural cache {field} mismatch")

    declared = {member["path"]: member for member in core["members"]}
    expected_files = set(declared) | {STRUCTURAL_CACHE_CORE}
    expected_directories = {""}
    for path in expected_files:
        parts = PurePosixPath(path).parts
        for length in range(1, len(parts)):
            expected_directories.add("/".join(parts[:length]))

    with tempfile.TemporaryDirectory(prefix="rna-structural-cache-verify-") as temporary:
        staging = Path(temporary) / "cache"
        staging.mkdir()
        seen: set[str] = set()
        seen_folded: set[str] = set()
        actual_files: set[str] = set()
        actual_directories: set[str] = set()
        total_size = 0
        try:
            archive = tarfile.open(archive_path, mode="r:gz")
        except (tarfile.TarError, OSError) as error:
            raise ToolchainError("structural cache archive is unreadable or partial") from error
        with archive:
            try:
                infos = archive.getmembers()
            except (tarfile.TarError, OSError) as error:
                raise ToolchainError("structural cache archive ended prematurely") from error
            if len(infos) > STRUCTURAL_CACHE_MAX_MEMBERS + len(expected_directories) + 1:
                raise ToolchainError("structural cache archive contains too many headers")
            file_names: set[str] = set()
            for info in infos:
                relative, is_root = _validated_archive_name(info.name)
                collision_key = info.name.casefold()
                if info.name in seen or collision_key in seen_folded:
                    raise ToolchainError(f"duplicate/casefold archive member: {info.name}")
                seen.add(info.name)
                seen_folded.add(collision_key)
                if info.pax_headers or getattr(info, "sparse", None):
                    raise ToolchainError(f"archive member uses sparse/extended metadata: {info.name}")
                if info.issym() or info.islnk() or info.isdev() or info.isfifo():
                    raise ToolchainError(f"archive member is a link or special file: {info.name}")
                if info.isdir():
                    if info.mode & 0o777 != 0o755:
                        raise ToolchainError(f"archive directory mode mismatch: {info.name}")
                    actual_directories.add(relative if not is_root else "")
                    continue
                if not info.isfile():
                    raise ToolchainError(f"archive member is not a regular file: {info.name}")
                if is_root:
                    raise ToolchainError("structural cache archive root cannot be a file")
                file_names.add(relative)
                actual_files.add(relative)
                if relative not in expected_files:
                    raise ToolchainError(f"undeclared structural cache member: {relative}")
                declared_member = declared.get(relative)
                expected_size = (
                    len(canonical_json(core))
                    if relative == STRUCTURAL_CACHE_CORE
                    else declared_member["size_bytes"]
                )
                expected_mode = 0o644 if relative == STRUCTURAL_CACHE_CORE else declared_member["mode"]
                if info.size != expected_size or info.mode & 0o777 != expected_mode:
                    raise ToolchainError(f"archive metadata mismatch for {relative}")
                total_size += info.size
                if info.size > STRUCTURAL_CACHE_MAX_MEMBER_BYTES or total_size > STRUCTURAL_CACHE_MAX_TOTAL_BYTES:
                    raise ToolchainError("structural cache archive exceeds safety limits")

            for file_name in file_names:
                prefix = file_name + "/"
                if any(other.startswith(prefix) for other in file_names if other != file_name):
                    raise ToolchainError(f"archive file/directory prefix conflict: {file_name}")
            if actual_files != expected_files:
                raise ToolchainError(
                    f"structural cache archive is partial: missing={sorted(expected_files - actual_files)}"
                )
            if actual_directories != expected_directories:
                raise ToolchainError("structural cache archive directory set is incomplete or undeclared")

            for info in infos:
                if not info.isfile():
                    continue
                relative, _ = _validated_archive_name(info.name)
                stream = archive.extractfile(info)
                if stream is None:
                    raise ToolchainError(f"unable to read archive member {relative}")
                destination = staging / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                digest = hashlib.sha256()
                remaining = info.size
                with destination.open("xb") as output:
                    while remaining:
                        chunk = stream.read(min(1024 * 1024, remaining))
                        if not chunk:
                            raise ToolchainError(f"archive member is truncated: {relative}")
                        output.write(chunk)
                        digest.update(chunk)
                        remaining -= len(chunk)
                    if stream.read(1):
                        raise ToolchainError(f"archive member exceeds declared size: {relative}")
                expected_digest = (
                    sidecar["core_sha256"]
                    if relative == STRUCTURAL_CACHE_CORE
                    else declared[relative]["sha256"]
                )
                if digest.hexdigest() != expected_digest:
                    raise ToolchainError(f"archive member digest mismatch: {relative}")
                destination.chmod(
                    0o644 if relative == STRUCTURAL_CACHE_CORE else declared[relative]["mode"]
                )

        embedded_core = load_json_object(
            staging / STRUCTURAL_CACHE_CORE, "embedded structural cache core"
        )
        if embedded_core != core or canonical_json(embedded_core) != (
            staging / STRUCTURAL_CACHE_CORE
        ).read_bytes():
            raise ToolchainError("embedded structural cache core differs from sidecar")
        (staging / STRUCTURAL_CACHE_CORE).unlink()
        extracted_members = _structural_cache_files(staging)
        if extracted_members != core["members"]:
            raise ToolchainError("extracted structural cache tree differs from core manifest")
        if _structural_tree_digest(extracted_members) != core["structural_cache_tree_digest"]:
            raise ToolchainError("extracted structural cache tree digest mismatch")

        if inject_checkout is not None:
            destination = _safe_checkout_cache_destination(inject_checkout)
            temporary_destination = destination.with_name(
                f".cache.inject-{os.getpid()}"
            )
            if temporary_destination.exists():
                raise ToolchainError("cache injection staging destination already exists")
            shutil.copytree(staging, temporary_destination, symlinks=False)
            temporary_destination.replace(destination)
        if materialize_cache is not None:
            if materialize_cache.exists() or materialize_cache.is_symlink():
                raise ToolchainError("verified cache materialization destination already exists")
            materialize_cache.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(staging, materialize_cache, symlinks=False)

    if sha256_file(archive_path) != archive_before or sha256_file(sidecar_path) != sidecar_before:
        raise ToolchainError("structural cache verification mutated its immutable base")
    return {
        "core": core,
        "archive_sha256": sidecar["archive_sha256"],
        "archive_size_bytes": sidecar["archive_size_bytes"],
        "core_sha256": sidecar["core_sha256"],
        "sidecar_sha256": sidecar_before,
        "structural_cache_tree_digest": core["structural_cache_tree_digest"],
    }


def _git_diff_paths(
    git_dir: Path, base_commit: str, target_commit: str
) -> dict[str, Any]:
    completed = subprocess.run(
        [
            "git",
            f"--git-dir={git_dir}",
            "diff",
            "--name-status",
            "--find-renames=50%",
            "-z",
            base_commit,
            target_commit,
            "--",
        ],
        check=False,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise ToolchainError("unable to diff structural cache base and target trees")
    fields = completed.stdout.split(b"\0")
    if fields and fields[-1] == b"":
        fields.pop()
    changed: list[str] = []
    added: list[str] = []
    deleted: list[str] = []
    renamed: list[list[str]] = []
    index = 0
    while index < len(fields):
        status = fields[index].decode("ascii", errors="strict")
        index += 1
        if status.startswith("R"):
            if index + 1 >= len(fields):
                raise ToolchainError("truncated Git rename diff")
            old = fields[index].decode("utf-8", errors="strict")
            new = fields[index + 1].decode("utf-8", errors="strict")
            index += 2
            renamed.append([old, new])
            continue
        if index >= len(fields):
            raise ToolchainError("truncated Git name-status diff")
        path = fields[index].decode("utf-8", errors="strict")
        index += 1
        if status.startswith("A"):
            added.append(path)
        elif status.startswith("D"):
            deleted.append(path)
        else:
            changed.append(path)
    for paths in (changed, added, deleted):
        paths.sort()
    renamed.sort()
    return {
        "changed_paths": changed,
        "added_paths": added,
        "deleted_paths": deleted,
        "renamed_paths": renamed,
        "distance": len(changed) + len(added) + len(deleted) + len(renamed),
    }


def _git_tree_blobs(git_dir: Path, commit: str) -> dict[str, str]:
    completed = subprocess.run(
        ["git", f"--git-dir={git_dir}", "ls-tree", "-r", "-z", commit],
        check=False,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise ToolchainError("unable to enumerate target Git tree")
    blobs: dict[str, str] = {}
    for record in completed.stdout.split(b"\0"):
        if not record:
            continue
        metadata, separator, path_bytes = record.partition(b"\t")
        if not separator:
            raise ToolchainError("malformed Git tree record")
        fields = metadata.split()
        if len(fields) != 3 or fields[1] != b"blob":
            continue
        path = path_bytes.decode("utf-8", errors="strict")
        blobs[path] = fields[2].decode("ascii", errors="strict")
    return blobs


def _git_commit_tree(git_dir: Path, commit: str) -> str:
    completed = subprocess.run(
        ["git", f"--git-dir={git_dir}", "rev-parse", f"{commit}^{{tree}}"],
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise ToolchainError("unable to resolve structural cache commit tree")
    tree = completed.stdout.strip()
    _require_git_oid(tree, "structural cache Git tree")
    return tree


def _load_cache_catalog(output_root: Path) -> dict[str, Any]:
    path = output_root / STRUCTURAL_CACHE_CATALOG
    if not path.exists():
        return {"schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION, "entries": []}
    if path.is_symlink() or not path.is_file():
        raise ToolchainError("structural cache catalog must be a regular file")
    catalog = load_json_object(path, "structural cache catalog")
    _require_exact_fields(catalog, {"schema_version", "entries"}, "cache catalog")
    if catalog["schema_version"] != STRUCTURAL_CACHE_SCHEMA_VERSION or not isinstance(
        catalog["entries"], list
    ):
        raise ToolchainError("structural cache catalog is invalid")
    return catalog


def _publish_cache_catalog_entry(output_root: Path, entry: Mapping[str, Any]) -> None:
    catalog = _load_cache_catalog(output_root)
    entries = list(catalog["entries"])
    if any(
        existing.get("instance_id") == entry.get("instance_id")
        and existing.get("attempt_index") == entry.get("attempt_index")
        for existing in entries
    ):
        raise ToolchainError("structural cache catalog attempt collision")
    entries.append(dict(entry))
    entries.sort(
        key=lambda existing: (
            existing["case_index"],
            existing["instance_id"],
            existing.get("attempt_index", 0),
        )
    )
    catalog["entries"] = entries
    write_canonical_json(output_root / STRUCTURAL_CACHE_CATALOG, catalog)


def _next_case_attempt_index(output_root: Path, instance_id: str) -> int:
    catalog = _load_cache_catalog(output_root)
    prior = [
        entry.get("attempt_index", 0)
        for entry in catalog["entries"]
        if entry.get("instance_id") == instance_id
        and type(entry.get("attempt_index", 0)) is int
    ]
    prefix = f"{instance_id}-attempt-"
    for directory in [output_root / "cases", output_root / "structural-caches"]:
        if not directory.is_dir() or directory.is_symlink():
            continue
        for path in directory.iterdir():
            if not path.name.startswith(prefix):
                continue
            suffix = path.name[len(prefix) :].split("-", 1)[0].split(".", 1)[0]
            if suffix.isdigit():
                prior.append(int(suffix))
    return max(prior, default=0) + 1


def select_structural_cache(
    output_root: Path,
    repository: str,
    target_commit: str,
    target_identity: Mapping[str, Any],
    git_dir: Path,
    case_index: int,
    *,
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
) -> dict[str, Any] | None:
    catalog = _load_cache_catalog(output_root)
    expected = {
        "repository": repository,
        "root_slug": target_identity["root_slug"],
        "producer": target_identity["producer"],
        "toolchain_lock_digest": toolchain_lock_digest,
        "inventory_digest": inventory_digest,
        "inventory_file_sha256": inventory_file_sha256,
        "inventory_policy_digest": target_identity["inventory_policy_digest"],
        "scan_flags": QUALIFICATION_SCAN_FLAGS,
    }
    candidates = []
    for entry in catalog["entries"]:
        if (
            entry.get("status") != "ready"
            or entry.get("schema_version") != STRUCTURAL_CACHE_SCHEMA_VERSION
            or entry.get("repository") != repository
            or type(entry.get("case_index")) is not int
            or entry["case_index"] > case_index
        ):
            continue
        archive_path = Path(_require_string(entry.get("archive_path"), "catalog archive path"))
        sidecar_path = Path(_require_string(entry.get("sidecar_path"), "catalog sidecar path"))
        if sha256_file(archive_path) != _require_sha256(
            entry.get("archive_sha256"), "catalog archive digest"
        ):
            raise ToolchainError("catalog/archive digest mismatch")
        if sha256_file(sidecar_path) != _require_sha256(
            entry.get("sidecar_sha256"), "catalog sidecar digest"
        ):
            raise ToolchainError("catalog/sidecar digest mismatch")
        sidecar, core = _validate_structural_sidecar(sidecar_path, archive_path)
        if _git_commit_tree(git_dir, core["commit"]) != core["tree"]:
            raise ToolchainError("structural cache commit/tree binding mismatch")
        if sidecar["core_sha256"] != _require_sha256(
            entry.get("core_sha256"), "catalog core digest"
        ):
            raise ToolchainError("catalog/core digest mismatch")
        for field, actual in {
            "repository": core["repository"],
            "root_slug": core["root_slug"],
            "producer": core["producer"],
            "toolchain_lock_digest": core["toolchain_lock_digest"],
            "inventory_digest": core["inventory_digest"],
            "inventory_file_sha256": core["inventory_file_sha256"],
            "inventory_policy_digest": core["inventory_policy_digest"],
            "scan_flags": core["scan_flags"],
        }.items():
            if expected[field] != actual:
                break
        else:
            verified = {
                "core": core,
                "archive_sha256": sidecar["archive_sha256"],
                "archive_size_bytes": sidecar["archive_size_bytes"],
                "core_sha256": sidecar["core_sha256"],
                "sidecar_sha256": sha256_file(sidecar_path),
                "structural_cache_tree_digest": core["structural_cache_tree_digest"],
            }
            diff = _git_diff_paths(git_dir, core["commit"], target_commit)
            candidates.append(
                (
                    diff["distance"],
                    -entry["case_index"],
                    -int(entry.get("attempt_index", 0)),
                    entry["instance_id"],
                    entry,
                    verified,
                    diff,
                )
            )
            continue
        # A globally incompatible candidate is a cold-rebuild choice, not an
        # extraction candidate. Tamper is still fail-closed above because the
        # sidecar/archive publication identities were fully validated.
        continue
    if not candidates:
        return None
    _, _, _, _, entry, verified, diff = min(
        candidates, key=lambda candidate: candidate[:4]
    )
    invalidate_all = (
        verified["core"]["shared_influence_digest"]
        != target_identity["shared_influence_digest"]
    )
    invalidated_partitions = sorted(
        language
        for language, signature in verified["core"]["partition_signatures"].items()
        if invalidate_all
        or language not in target_identity["partitions"]
        or target_identity["partitions"][language]["signature"] != signature
    )
    return {
        "entry": entry,
        "verified": verified,
        "diff": diff,
        "invalidated_partitions": invalidated_partitions,
    }


def inject_structural_cache(
    selection: Mapping[str, Any],
    checkout: Path,
    target_identity: Mapping[str, Any],
    git_dir: Path,
    *,
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
    verified: Mapping[str, Any] | None = None,
    materialized_cache: Path | None = None,
) -> dict[str, Any]:
    entry = selection["entry"]
    archive_path = Path(entry["archive_path"])
    sidecar_path = Path(entry["sidecar_path"])
    if verified is None:
        verified = verify_structural_cache_archive(
            archive_path,
            sidecar_path,
            expected={
                "repository": target_identity["repository"],
                "root_slug": target_identity["root_slug"],
                "producer": target_identity["producer"],
                "toolchain_lock_digest": toolchain_lock_digest,
                "inventory_digest": inventory_digest,
                "inventory_file_sha256": inventory_file_sha256,
                "inventory_policy_digest": target_identity["inventory_policy_digest"],
                "scan_flags": QUALIFICATION_SCAN_FLAGS,
            },
            inject_checkout=checkout,
        )
    elif materialized_cache is not None:
        destination = _safe_checkout_cache_destination(checkout)
        temporary_destination = destination.with_name(f".cache.inject-{os.getpid()}")
        shutil.copytree(materialized_cache, temporary_destination, symlinks=False)
        temporary_destination.replace(destination)
    else:
        raise ToolchainError("preverified cache injection requires materialized cache bytes")
    core = verified["core"]
    cache_root = checkout / ".oh" / ".cache"
    # A chained target archive may contain its own prior execution receipt.
    # It is immutable evidence for that archive, not authorization to execute
    # against this target; the current scan publishes a new receipt.
    (cache_root / "structural-cache-execution.json").unlink(missing_ok=True)
    base_report_path = cache_root / "lsp_completeness.json"
    base_report = load_json_object(base_report_path, "injected base completeness report")
    if base_report.get("digest") != core["completeness_report_digest"]:
        raise ToolchainError("injected completeness report digest differs from cache core")
    if sha256_file(base_report_path) != core["completeness_report_sha256"]:
        raise ToolchainError("injected completeness report bytes differ from cache core")
    files = base_report.get("files")
    if not isinstance(files, list):
        raise ToolchainError("injected completeness report has no file evidence")
    work_path = cache_root / "lsp_pass1_work_items.json"
    work_store = load_json_object(work_path, "injected LSP work ledger")
    records_value = work_store.get("records")
    if not isinstance(records_value, dict):
        raise ToolchainError("injected LSP work ledger has no records map")
    completed_by_path: dict[str, list[tuple[str, Mapping[str, Any]]]] = collections.defaultdict(list)
    for record_key, record in records_value.items():
        if not isinstance(record_key, str) or not isinstance(record, dict):
            raise ToolchainError("injected LSP work ledger record is malformed")
        if record.get("state") == "completed" and isinstance(record.get("file"), str):
            completed_by_path[record["file"]].append((record_key, record))
    for records in completed_by_path.values():
        records.sort(key=lambda pair: pair[0])

    diff = selection["diff"]
    touched = set(diff["changed_paths"]) | set(diff["added_paths"]) | set(
        diff["deleted_paths"]
    )
    for old, new in diff["renamed_paths"]:
        touched.add(old)
        touched.add(new)
    target_blobs = _git_tree_blobs(git_dir, target_identity["commit"])
    invalidated = set(selection["invalidated_partitions"])
    base_languages = {
        file_record["path"]: file_record["language"]
        for file_record in files
        if isinstance(file_record, dict)
        and isinstance(file_record.get("path"), str)
        and isinstance(file_record.get("language"), str)
    }
    path_partitions: dict[str, str] = {}
    for path in sorted(target_blobs):
        language = base_languages.get(path)
        if language is None:
            absolute = checkout / path
            try:
                prefix = absolute.read_bytes()[:8192]
            except OSError as error:
                raise ToolchainError(f"unable to classify target cache path: {path}") from error
            role, exclusion, _ = classify_path(path, prefix)
            if exclusion is None:
                language = language_for_path(path, prefix, role)
        if language in target_identity["partitions"]:
            path_partitions[path] = language

    # A cached result without a durable producer cannot be selectively
    # invalidated. Escalate its whole language partition before authorizing any
    # inheritance, then rebuild the inherited set once with the final partition set.
    for file_record in files:
        if not isinstance(file_record, dict):
            raise ToolchainError("base completeness file record is malformed")
        path = file_record.get("path")
        language = file_record.get("language")
        expected_ids = file_record.get("expected_result_ids", [])
        if isinstance(path, str) and isinstance(language, str) and isinstance(
            expected_ids, list
        ):
            records = completed_by_path.get(path, [])
            for result_id in expected_ids:
                if not isinstance(result_id, str) or not any(
                    result_id in record.get("produced_result_ids", [])
                    for _, record in records
                    if isinstance(record.get("produced_result_ids", []), list)
                ):
                    invalidated.add(language)
                    break

    inherited_files = []
    inherited_work_count = 0
    for file_record in sorted(files, key=lambda record: record.get("path", "")):
        path = file_record.get("path")
        language = file_record.get("language")
        terminal = file_record.get("terminal_status")
        if (
            not isinstance(path, str)
            or not isinstance(language, str)
            or path in touched
            or path not in target_blobs
            or language in invalidated
            or not isinstance(terminal, dict)
            or terminal.get("status") != "processed"
        ):
            continue
        partition = target_identity["partitions"].get(language)
        if not isinstance(partition, dict):
            invalidated.add(language)
            continue
        records = completed_by_path.get(path, [])
        producer_ids = [record_key for record_key, _ in records]
        input_hashes = sorted(
            {
                record.get("input_hash")
                for _, record in records
                if isinstance(record.get("input_hash"), str)
                and record.get("input_hash")
            }
        )
        operations = sorted(
            {
                operation
                for _, record in records
                for operation in record.get("requested_operations", [])
                if isinstance(operation, str) and operation
            }
        )
        operations.extend(
            sorted(
                {
                    request.get("method")
                    for request in file_record.get("requests_attempted", [])
                    if isinstance(request, dict)
                    and isinstance(request.get("method"), str)
                }
                - set(operations)
            )
        )
        expected_ids = sorted(
            {
                result_id
                for result_id in file_record.get("expected_result_ids", [])
                if isinstance(result_id, str) and result_id
            }
        )
        result_producers = [
            {
                "result_id": result_id,
                "producer_ids": [
                    record_key
                    for record_key, record in records
                    if result_id in record.get("produced_result_ids", [])
                ],
            }
            for result_id in expected_ids
        ]
        if any(not lineage["producer_ids"] for lineage in result_producers):
            # The pre-pass above should already have escalated this partition.
            raise ToolchainError(f"unlineaged inherited result escaped partition rebuild: {path}")
        producer_graph_enrichment_operation_count = sum(
            len(record.get("requested_operations", []))
            for _, record in records
            if isinstance(record.get("requested_operations", []), list)
        )
        inherited_work_count += producer_graph_enrichment_operation_count
        inherited_files.append(
            {
                "path": path,
                "blob": target_blobs[path],
                "language": language,
                "partition_signature": partition["signature"],
                "base_file_sha256": sha256_bytes(canonical_json(file_record)),
                "input_hashes": input_hashes,
                "operations": sorted(set(operations)),
                "producer_work_ids": producer_ids,
                "producer_graph_enrichment_operation_count": (
                    producer_graph_enrichment_operation_count
                ),
                "expected_result_ids": expected_ids,
                "result_producers": result_producers,
            }
        )

    # The verified base archive remains immutable. In the injected copy, retain
    # only work records explicitly named by the target authorization. Touched or
    # invalidated paths must not be eligible for carried-completed recovery;
    # impact-closure paths discovered by RNA are purged immediately before the
    # incremental LSP pass.
    retained_producer_ids = {
        producer_id
        for inherited in inherited_files
        for producer_id in inherited["producer_work_ids"]
    }
    work_store["records"] = {
        record_id: record
        for record_id, record in records_value.items()
        if record_id in retained_producer_ids
    }
    write_canonical_json(work_path, work_store)

    invalidated_paths = sorted(
        {
            file_record["path"]
            for file_record in files
            if isinstance(file_record, dict)
            and isinstance(file_record.get("path"), str)
            and file_record.get("language") in invalidated
            and file_record["path"] in target_blobs
        }
    )
    authorization = {
        "schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION,
        "offline_preprocessing": True,
        "repository": target_identity["repository"],
        "base_commit": core["commit"],
        "base_tree": core["tree"],
        "target_commit": target_identity["commit"],
        "target_tree": target_identity["tree"],
        "root_slug": target_identity["root_slug"],
        "producer": target_identity["producer"],
        "toolchain_lock_digest": toolchain_lock_digest,
        "inventory_digest": inventory_digest,
        "inventory_file_sha256": inventory_file_sha256,
        "configuration_digest": target_identity["configuration_digest"],
        "scan_flags": QUALIFICATION_SCAN_FLAGS,
        "base_archive_sha256": verified["archive_sha256"],
        "base_sidecar_sha256": verified["sidecar_sha256"],
        "base_core_sha256": verified["core_sha256"],
        "base_report_digest": core["completeness_report_digest"],
        "base_report_sha256": core["completeness_report_sha256"],
        "inherited_files": inherited_files,
        "changed_paths": diff["changed_paths"],
        "added_paths": diff["added_paths"],
        "deleted_paths": diff["deleted_paths"],
        "renamed_paths": diff["renamed_paths"],
        "invalidated_partitions": sorted(invalidated),
        "invalidated_paths": invalidated_paths,
        "path_partitions": path_partitions,
        "digest": "",
    }
    authorization["digest"] = sha256_bytes(canonical_json(authorization))
    authorization_path = cache_root / "structural-cache-inheritance.json"
    write_canonical_json(authorization_path, authorization)
    return {
        "base_archive_sha256": verified["archive_sha256"],
        "base_sidecar_sha256": verified["sidecar_sha256"],
        "base_core_sha256": verified["core_sha256"],
        "base_report_digest": core["completeness_report_digest"],
        "authorization_path": str(authorization_path.resolve()),
        "authorization_sha256": sha256_file(authorization_path),
        "inherited_file_count": len(inherited_files),
        "inherited_graph_enrichment_operation_count": inherited_work_count,
        "changed_file_count": diff["distance"],
        "invalidated_partitions": sorted(invalidated),
    }


def build_repo_parser_bundle(repo_root: Path, output: Path) -> dict[str, Any]:
    sources = [
        (repo_root / "scripts/lsp/config_language_server.py", "rna-config-language-server"),
        (repo_root / "scripts/lsp/cohort_language_server.py", "rna-cohort-language-server"),
    ]
    with tempfile.TemporaryDirectory(prefix="rna-lsp-parser-bundle-") as temporary:
        root = Path(temporary) / "issue785-repo-servers"
        root.mkdir()
        source_receipts = []
        for source, destination_name in sources:
            if not source.is_file():
                raise ToolchainError(f"missing repo parser source: {source}")
            destination = root / destination_name
            shutil.copyfile(source, destination)
            destination.chmod(0o755)
            source_receipts.append(
                {
                    "path": source.relative_to(repo_root).as_posix(),
                    "sha256": sha256_file(source),
                }
            )
        receipt = seal_directory(root, output, root.name)
        receipt["sources"] = source_receipts
        return receipt


def run_checked(args: Sequence[str], *, cwd: Path | None = None) -> str:
    completed = subprocess.run(
        list(args), cwd=cwd, check=False, capture_output=True, text=True
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise ToolchainError(f"command failed ({' '.join(args)}): {detail}")
    return completed.stdout.strip()


def included_population(population: Mapping[str, Any]) -> list[dict[str, str]]:
    instances = population.get("instances")
    if not isinstance(instances, list):
        raise ToolchainError("population instances must be an array")
    included: list[dict[str, str]] = []
    for index, candidate in enumerate(instances):
        if not isinstance(candidate, dict):
            raise ToolchainError(f"population instance {index} must be an object")
        if candidate.get("included") is not True:
            continue
        values: dict[str, str] = {}
        for field in ("instance_id", "repo", "base_commit"):
            value = candidate.get(field)
            if not isinstance(value, str) or not value:
                raise ToolchainError(
                    f"population instance {index} has invalid {field}"
                )
            values[field] = value
        included.append(values)
    included.sort(key=lambda value: value["instance_id"])
    if len(included) != EXPECTED_POPULATION_SIZE:
        raise ToolchainError(
            f"population must contain exactly {EXPECTED_POPULATION_SIZE} included "
            f"instances, found {len(included)}"
        )
    return included


def git_cache_path(cache_root: Path, repository: str) -> Path:
    return cache_root / f"{repository.replace('/', '__')}.git"


def git_cache_ref(commit: str) -> str:
    return f"refs/heads/rna-frozen/{commit}"


def acquire_git_cache(population_path: Path, cache_root: Path) -> dict[str, Any]:
    population = load_json_object(population_path, "population")
    instances = included_population(population)
    cache_root.mkdir(parents=True, exist_ok=True)
    fetched = 0
    for instance in instances:
        repository = instance["repo"]
        commit = instance["base_commit"]
        git_dir = git_cache_path(cache_root, repository)
        if not git_dir.exists():
            run_checked(["git", "init", "--bare", "--quiet", str(git_dir)])
            run_checked(
                [
                    "git",
                    "-C",
                    str(git_dir),
                    "remote",
                    "add",
                    "origin",
                    f"https://github.com/{repository}.git",
                ]
            )
        exists = subprocess.run(
            ["git", "-C", str(git_dir), "cat-file", "-e", f"{commit}^{{commit}}"],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if exists.returncode != 0:
            run_checked(
                [
                    "git",
                    "-C",
                    str(git_dir),
                    "fetch",
                    "--quiet",
                    "--depth",
                    "1",
                    "origin",
                    commit,
                ]
            )
            fetched += 1
        run_checked(
            [
                "git",
                "-C",
                str(git_dir),
                "update-ref",
                git_cache_ref(commit),
                commit,
            ]
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "population_sha256": sha256_file(population_path),
        "instance_count": len(instances),
        "repositories": sorted({instance["repo"] for instance in instances}),
        "new_commits_fetched": fetched,
    }


def verify_git_cache(population_path: Path, cache_root: Path) -> dict[str, Any]:
    instances = included_population(load_json_object(population_path, "population"))
    verified_blobs = 0
    shallow_repositories: set[str] = set()
    for instance in instances:
        repository = instance["repo"]
        commit = instance["base_commit"]
        git_dir = git_cache_path(cache_root, repository)
        if not git_dir.is_dir():
            raise ToolchainError(f"missing git cache for {repository}")
        if (git_dir / "objects/info/alternates").exists():
            raise ToolchainError(f"git cache uses object alternates: {repository}")
        for key in ("remote.origin.promisor", "extensions.partialclone"):
            configured = subprocess.run(
                ["git", "-C", str(git_dir), "config", "--get", key],
                check=False,
                capture_output=True,
                text=True,
            )
            if configured.returncode == 0 and configured.stdout.strip():
                raise ToolchainError(
                    f"git cache is partial/promisor for {repository}: {key}"
                )
        if (git_dir / "shallow").exists():
            shallow_repositories.add(repository)
        for object_expression in (f"{commit}^{{commit}}", f"{commit}^{{tree}}"):
            verified = subprocess.run(
                ["git", "-C", str(git_dir), "cat-file", "-e", object_expression],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            if verified.returncode != 0:
                raise ToolchainError(
                    f"git cache lacks {object_expression} for {instance['instance_id']}"
                )
        entries = git_tree_entries(git_dir, commit)
        batch = subprocess.run(
            [
                "git",
                "-C",
                str(git_dir),
                "cat-file",
                "--batch-check=%(objectname) %(objecttype)",
            ],
            input="".join(f"{entry.object_id}\n" for entry in entries).encode(),
            check=False,
            capture_output=True,
        )
        if batch.returncode != 0:
            raise ToolchainError(
                f"git blob preflight failed for {instance['instance_id']}: "
                f"{batch.stderr.decode(errors='replace').strip()}"
            )
        results = batch.stdout.decode("ascii", errors="replace").splitlines()
        if len(results) != len(entries) or any(
            not result.endswith(" blob") for result in results
        ):
            raise ToolchainError(
                f"git cache lacks a tracked blob for {instance['instance_id']}"
            )
        verified_blobs += len(entries)
    evidence = {
        "schema_version": SCHEMA_VERSION,
        "population_sha256": sha256_file(population_path),
        "instance_count": len(instances),
        "repository_count": len({instance["repo"] for instance in instances}),
        "verified_blob_observations": verified_blobs,
        "shallow_repositories": sorted(shallow_repositories),
        "self_contained": True,
    }
    evidence["verification_digest"] = sha256_bytes(canonical_json(evidence))
    return evidence


@dataclass(frozen=True)
class GitEntry:
    mode: str
    object_id: str
    path: str


def git_tree_entries(git_dir: Path, commit: str) -> list[GitEntry]:
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(git_dir),
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            commit,
        ],
        check=False,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise ToolchainError(
            f"unable to inventory {git_dir.name}@{commit}: "
            f"{completed.stderr.decode(errors='replace').strip()}"
        )
    entries: list[GitEntry] = []
    for record in completed.stdout.split(b"\0"):
        if not record:
            continue
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, object_id = metadata.decode("ascii").split(" ")
        if kind != "blob":
            continue
        try:
            path = raw_path.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ToolchainError("git tree contains a non-UTF-8 path") from error
        entries.append(GitEntry(mode=mode, object_id=object_id, path=path))
    entries.sort(key=lambda entry: entry.path)
    return entries


class BlobPrefixReader:
    def __init__(self, git_dir: Path) -> None:
        self.process = subprocess.Popen(
            ["git", "-C", str(git_dir), "cat-file", "--batch"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.cache: dict[str, bytes] = {}

    def prefix(self, object_id: str) -> bytes:
        cached = self.cache.get(object_id)
        if cached is not None:
            return cached
        stdin = self._stdin()
        stdout = self._stdout()
        stdin.write(object_id.encode("ascii") + b"\n")
        stdin.flush()
        header = stdout.readline().decode("ascii", errors="replace").strip()
        parts = header.split(" ")
        if len(parts) != 3 or parts[1] != "blob":
            raise ToolchainError(f"unable to read git blob {object_id}: {header}")
        size = int(parts[2])
        prefix_size = min(size, 8192)
        prefix = stdout.read(prefix_size)
        remaining = size - prefix_size
        while remaining:
            chunk = stdout.read(min(remaining, 1024 * 1024))
            if not chunk:
                raise ToolchainError(f"truncated git blob {object_id}")
            remaining -= len(chunk)
        if stdout.read(1) != b"\n":
            raise ToolchainError(f"invalid git cat-file framing for {object_id}")
        self.cache[object_id] = prefix
        return prefix

    def contains_nul(self, object_id: str) -> bool:
        return b"\0" in self.prefix(object_id)

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        returncode = self.process.wait()
        stderr = b""
        if self.process.stderr:
            stderr = self.process.stderr.read()
            self.process.stderr.close()
        if self.process.stdout:
            self.process.stdout.close()
        if returncode != 0:
            detail = stderr.decode(errors="replace").strip()
            raise ToolchainError(f"git cat-file failed: {detail}")

    def _stdin(self) -> BinaryIO:
        if self.process.stdin is None:
            raise ToolchainError("git cat-file stdin is unavailable")
        return self.process.stdin

    def _stdout(self) -> BinaryIO:
        if self.process.stdout is None:
            raise ToolchainError("git cat-file stdout is unavailable")
        return self.process.stdout

    def __enter__(self) -> BlobPrefixReader:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def path_extension(path: str) -> str:
    suffix = PurePosixPath(path).suffix
    return suffix[1:].lower() if suffix else "<none>"


def classify_path(path: str, prefix: bytes) -> tuple[str, str | None, str]:
    pure = PurePosixPath(path)
    components = [component.lower() for component in pure.parts]
    extension = path_extension(path)
    filename = pure.name.lower()
    if components and components[0] == ".oh":
        return "excluded_generated", "configured_policy", "rna_artifact"
    if any(component in VENDOR_COMPONENTS for component in components):
        return "excluded_vendor", "vendor", "vendored_dependency"
    if any(component in GENERATED_COMPONENTS for component in components) or extension in {
        "pyc",
        "pyo",
        "class",
        "o",
        "obj",
    }:
        return "excluded_generated", "generated", "generated_or_build_output"
    if extension in BINARY_EXTENSIONS or b"\0" in prefix:
        return "excluded_binary", "binary", "binary_suffix_or_nul_prefix"
    if extension in TEXT_ASSET_EXTENSIONS:
        return "excluded_asset", "asset", "presentation_or_secret_test_asset"
    if extension in TEXT_DATA_EXTENSIONS:
        return "excluded_data", "non_language_data", "structured_or_tabular_data"
    if filename in NON_LANGUAGE_MARKER_FILENAMES:
        return "excluded_data", "non_language_data", "empty_or_typing_marker"
    if extension in {"mmd", "vcg"}:
        return "excluded_data", "non_language_data", "generated_graph_output_fixture"
    if extension in {"dot", "puml"} and "pyreverse" in components:
        return "excluded_data", "non_language_data", "generated_graph_output_fixture"
    if filename == "not_utf8.sample":
        return "excluded_data", "non_language_data", "deliberate_encoding_fixture"
    if extension == "txt":
        if filename.startswith("requirements"):
            return "config", None, "requirements_config"
        if any(component in {"doc", "docs"} for component in components) or filename.startswith(
            DOC_FILENAME_PREFIXES
        ):
            return "docs", None, "plain_project_document"
        return "excluded_data", "non_language_data", "plain_test_or_dataset_payload"
    if extension in DOC_EXTENSIONS or filename.startswith(DOC_FILENAME_PREFIXES):
        return "docs", None, "project_document"
    if extension in CONFIG_EXTENSIONS or filename in CONFIG_FILENAMES:
        return "config", None, "project_configuration"
    if filename.startswith("requirements") or filename == "manifest.in":
        return "config", None, "project_configuration"
    if extension == "sample" and filename == "tox.ini.sample":
        return "config", None, "project_configuration_sample"
    if extension == "<none>" and prefix.startswith(b"#!"):
        return "source", None, "executable_script_with_shebang"
    if (
        any(
            component in {"test", "tests", "spec", "specs"}
            for component in components
        )
        or filename.startswith("test_")
        or "_test." in filename
        or ".spec." in filename
        or ".test." in filename
    ):
        if extension in KNOWN_TEST_FIXTURE_EXTENSIONS:
            return "excluded_data", "non_language_data", "deliberate_test_fixture_payload"
        if extension == "<none>" and filename in NON_LANGUAGE_TEST_FILENAMES:
            return "excluded_data", "non_language_data", "deliberate_test_sentinel_payload"
        return "test", None, "test_code_or_language_document"
    # Unknown and uncommon text remains mandatory. This is the fail-closed
    # half of the policy and prevents source subtypes from becoming data merely
    # because the fleet does not support them yet.
    return "source", None, "language_addressability_unresolved"


def language_for_path(path: str, prefix: bytes, role: str) -> str:
    """Return the explicit fleet language for every retained frozen-cohort file."""
    pure = PurePosixPath(path)
    extension = path_extension(path)
    filename = pure.name.lower()
    components = [component.lower() for component in pure.parts]

    if extension == "sample":
        if filename == "code.sample":
            return "python"
        return "config"
    if extension == "in":
        if filename.endswith((".h.in", ".c.in", ".cpp.in")):
            return "c-cpp"
        return "config"
    if extension == "new_t":
        return "batch" if "bat" in filename else "config"
    if extension == "<none>":
        shebang = prefix.splitlines()[0].lower() if prefix.startswith(b"#!") else b""
        if b"python" in shebang:
            return "python"
        if any(shell in shebang for shell in (b"sh", b"bash", b"zsh", b"xonsh")):
            return "shell"
        if filename in {"makefile", "gnumakefile", "dockerfile", "pylintrc", "matplotlibrc"}:
            return "config"
        if filename.startswith(".") or filename in {"codeowners", "procfile"}:
            return "config"
        if filename.startswith(DOC_FILENAME_PREFIXES) or role == "docs":
            return "plaintext"
        if filename in {
            "diagnose_imports", "doctest", "isympy", "strip_whitespace", "test",
            "test_import", "test_isolated", "tm_sympy",
        }:
            return "python"
        return "cohort-text"

    groups = {
        "python": {"py", "pyi", "py-tpl", "py_t", "bench"},
        "markdown": {"md", "markdown"},
        "restructuredtext": {
            "rst", "rst_t", "inc", "breaking", "bugfix", "extension",
            "false_negative", "false_positive", "feature", "internal",
            "new_check", "other", "performance", "user_action",
        },
        "cython": {"pyx", "pxd", "pxi", "tp"},
        "c-cpp": {"c", "h", "cpp", "m"},
        "json": {"json", "ipynb"},
        "yaml": {"yml", "yaml", "cff", "lock"},
        "toml": {"toml"},
        "config": {"ini", "cfg", "conf", "mplstyle", "rc", "template", "hhp", "def"},
        "shell": {"sh", "xsh", "guess", "sub"},
        "html": {"html", "html_t", "thtml", "djtpl", "tpl"},
        "css": {"css", "css_t"},
        "typescript": {"js", "js_t"},
        "xml": {"xml", "xsd", "dtd", "xsl", "kml", "glade", "xrc", "hhc", "ncx_t", "opf_t", "xhtml_t", "stp"},
        "latex": {"tex", "tex_t", "sty", "sty_t", "cls", "bib", "xdy", "ist"},
        "gettext": {"po", "pot", "pot_t"},
        "plaintext": {"txt", "eopc04_iau2000", "finals2000a", "lesser", "license", "old", "python", "pil", "wx"},
        "batch": {"bat", "bat_t", "cmd"},
        "graphviz": {"dot"},
        "plantuml": {"puml"},
        "roff": {"1"},
        "autolev": {"al"},
        "antlr": {"g4"},
        "lex": {"l"},
        "emacs-lisp": {"el"},
        "scheme": {"scm"},
        "lua": {"lua"},
        "autotools": {"ac", "am"},
        "powershell": {"ps1"},
        "starlark": {"star"},
    }
    for language, extensions in groups.items():
        if extension in extensions:
            return language
    # Unknown text remains mandatory and is processed by the repo-owned
    # cohort-text parser. This is an explicit retained-language assignment,
    # never an exclusion or a claim that the file belongs to another language.
    return "cohort-text"


def inventory_population(
    population_path: Path,
    git_cache_root: Path,
    file_evidence_output: Path | None = None,
) -> dict[str, Any]:
    population = load_json_object(population_path, "population")
    instances = included_population(population)
    by_repository: dict[str, list[dict[str, str]]] = collections.defaultdict(list)
    for instance in instances:
        by_repository[instance["repo"]].append(instance)

    extension_files: collections.Counter[str] = collections.Counter()
    extension_cases: collections.Counter[str] = collections.Counter()
    extension_roles: dict[str, collections.Counter[str]] = collections.defaultdict(
        collections.Counter
    )
    extension_samples: dict[str, str] = {}
    language_files: collections.Counter[str] = collections.Counter()
    language_cases: collections.Counter[str] = collections.Counter()
    language_extensions: dict[str, collections.Counter[str]] = collections.defaultdict(
        collections.Counter
    )
    case_reports: list[dict[str, Any]] = []
    evidence_sink: BinaryIO | None = None
    evidence_archive: gzip.GzipFile | None = None
    evidence_temporary: Path | None = None
    if file_evidence_output is not None:
        file_evidence_output.parent.mkdir(parents=True, exist_ok=True)
        evidence_temporary = file_evidence_output.with_name(
            f".{file_evidence_output.name}.tmp-{os.getpid()}"
        )
        evidence_sink = evidence_temporary.open("wb")
        evidence_archive = gzip.GzipFile(
            filename="", mode="wb", fileobj=evidence_sink, mtime=0
        )

    try:
        for repository in sorted(by_repository):
            git_dir = git_cache_path(git_cache_root, repository)
            if not git_dir.is_dir():
                raise ToolchainError(f"missing git cache for {repository}: {git_dir}")
            trees: dict[str, list[GitEntry]] = {}
            for instance in by_repository[repository]:
                commit = instance["base_commit"]
                run_checked(["git", "-C", str(git_dir), "cat-file", "-e", f"{commit}^{{commit}}"])
                trees[commit] = git_tree_entries(git_dir, commit)
            with BlobPrefixReader(git_dir) as blobs:
                for instance in sorted(
                    by_repository[repository], key=lambda value: value["instance_id"]
                ):
                    commit = instance["base_commit"]
                    records: list[dict[str, str]] = []
                    role_counts: collections.Counter[str] = collections.Counter()
                    exclusion_counts: collections.Counter[str] = collections.Counter()
                    case_extensions: set[str] = set()
                    case_languages: set[str] = set()
                    for entry in trees[commit]:
                        extension = path_extension(entry.path)
                        known_binary = extension in BINARY_EXTENSIONS
                        prefix = b"" if known_binary else blobs.prefix(entry.object_id)
                        role, exclusion, classification = classify_path(entry.path, prefix)
                        record = {
                            "path": entry.path,
                            "role": role,
                            "extension": extension,
                            "blob": entry.object_id,
                            "classification": classification,
                        }
                        if exclusion is not None:
                            record["exclusion"] = exclusion
                            exclusion_counts[exclusion] += 1
                        else:
                            language = language_for_path(entry.path, prefix, role)
                            record["language"] = language
                            extension_files[extension] += 1
                            extension_roles[extension][role] += 1
                            extension_samples.setdefault(
                                extension, f"{instance['instance_id']}:{entry.path}"
                            )
                            case_extensions.add(extension)
                            language_files[language] += 1
                            language_extensions[language][extension] += 1
                            case_languages.add(language)
                        role_counts[role] += 1
                        records.append(record)
                    extension_cases.update(case_extensions)
                    language_cases.update(case_languages)
                    per_file_digest = sha256_bytes(canonical_json(records))
                    if evidence_archive is not None:
                        evidence_archive.write(
                            canonical_json(
                                {
                                    "instance_id": instance["instance_id"],
                                    "repo": instance["repo"],
                                    "base_commit": commit,
                                    "per_file_digest": per_file_digest,
                                    "files": records,
                                }
                            )
                        )
                    case_reports.append(
                        {
                            **instance,
                            "tree": run_checked(
                                [
                                    "git",
                                    "-C",
                                    str(git_dir),
                                    "rev-parse",
                                    f"{commit}^{{tree}}",
                                ]
                            ),
                            "tracked_file_count": len(records),
                            "included_file_count": sum(
                                count
                                for role, count in role_counts.items()
                                if not role.startswith("excluded_")
                            ),
                            "role_counts": dict(sorted(role_counts.items())),
                            "exclusion_counts": dict(sorted(exclusion_counts.items())),
                            "per_file_digest": per_file_digest,
                        }
                    )
    finally:
        if evidence_archive is not None:
            evidence_archive.close()
        if evidence_sink is not None:
            evidence_sink.close()
    if evidence_temporary is not None and file_evidence_output is not None:
        evidence_temporary.replace(file_evidence_output)

    extensions = [
        {
            "extension": extension,
            "file_count": extension_files[extension],
            "checkout_count": extension_cases[extension],
            "roles": dict(sorted(extension_roles[extension].items())),
            "sample": extension_samples[extension],
        }
        for extension in sorted(extension_files)
    ]
    languages = [
        {
            "language": language,
            "file_count": language_files[language],
            "checkout_count": language_cases[language],
            "extensions": dict(sorted(language_extensions[language].items())),
        }
        for language in sorted(language_files)
    ]
    evidence = {
        "schema_version": SCHEMA_VERSION,
        "population_sha256": sha256_file(population_path),
        "population_id": population.get("population_id"),
        "included_instance_count": len(instances),
        "repositories": sorted(by_repository),
        "cases": sorted(case_reports, key=lambda value: value["instance_id"]),
        "extensions": extensions,
        "languages": languages,
    }
    if file_evidence_output is not None:
        evidence["file_evidence"] = {
            "format": "canonical-json-lines+gzip",
            "path": file_evidence_output.name,
            "sha256": sha256_file(file_evidence_output),
        }
    evidence["inventory_digest"] = sha256_bytes(canonical_json(evidence))
    return evidence


def validate_hex_digest(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise ToolchainError(f"{label} must be a lowercase SHA-256 digest")
    return value


def verify_lock(
    lock_path: Path,
    inventory_path: Path,
    cache_root: Path | None,
    descriptor_path: Path | None,
) -> dict[str, Any]:
    lock = load_json_object(lock_path, "toolchain lock")
    inventory = load_json_object(inventory_path, "inventory")
    required_lock_keys = {
        "schema_version",
        "platform",
        "inventory_sha256",
        "inventory_digest",
        "repo_parser_bundle",
        "runtimes",
        "servers",
        "unsupported_languages",
    }
    if set(lock) != required_lock_keys:
        raise ToolchainError("toolchain lock has unknown or missing top-level fields")
    if lock.get("schema_version") != SCHEMA_VERSION:
        raise ToolchainError("toolchain lock schema_version must be JSON integer 1")
    expected_inventory_sha = sha256_file(inventory_path)
    if lock.get("inventory_sha256") != expected_inventory_sha:
        raise ToolchainError("toolchain lock inventory_sha256 does not match inventory bytes")
    if lock.get("inventory_digest") != inventory.get("inventory_digest"):
        raise ToolchainError("toolchain lock inventory_digest does not match inventory")
    expected_platform = {"os": "macos", "architecture": "arm64"}
    if lock.get("platform") != expected_platform:
        raise ToolchainError("toolchain lock platform must be macos/arm64")
    servers = lock.get("servers")
    runtimes = lock.get("runtimes")
    repo_parser_bundle = lock.get("repo_parser_bundle")
    unsupported = lock.get("unsupported_languages")
    if not isinstance(servers, list) or not isinstance(runtimes, list):
        raise ToolchainError("toolchain servers and runtimes must be arrays")
    if not isinstance(unsupported, list):
        raise ToolchainError("unsupported_languages must be an array")
    if not isinstance(repo_parser_bundle, dict) or set(repo_parser_bundle) != {
        "artifact",
        "artifact_sha256",
        "sources",
    }:
        raise ToolchainError("repo_parser_bundle has invalid fields")
    validate_hex_digest(
        repo_parser_bundle.get("artifact_sha256"),
        "repo_parser_bundle.artifact_sha256",
    )
    if not isinstance(repo_parser_bundle.get("artifact"), str) or not repo_parser_bundle[
        "artifact"
    ]:
        raise ToolchainError("repo_parser_bundle.artifact must be nonempty")
    sources = repo_parser_bundle.get("sources")
    if not isinstance(sources, list) or not sources:
        raise ToolchainError("repo_parser_bundle.sources must be nonempty")
    for index, source in enumerate(sources):
        if not isinstance(source, dict) or set(source) != {"path", "sha256"}:
            raise ToolchainError(f"repo_parser_bundle source {index} has invalid fields")
        if not isinstance(source.get("path"), str) or not source["path"]:
            raise ToolchainError(f"repo_parser_bundle source {index} path is invalid")
        validate_hex_digest(
            source.get("sha256"), f"repo_parser_bundle source {index}.sha256"
        )
    languages = {
        entry["language"]
        for entry in inventory.get("languages", [])
        if isinstance(entry, dict) and isinstance(entry.get("language"), str)
    }
    covered: set[str] = set()
    covered_extensions: dict[str, set[str]] = collections.defaultdict(set)
    artifacts: list[tuple[str, str]] = []
    runtime_names: set[str] = set()
    server_entries: list[Mapping[str, Any]] = []
    for collection_name, entries in (("runtime", runtimes), ("server", servers)):
        names: set[str] = set()
        for index, entry in enumerate(entries):
            if not isinstance(entry, dict):
                raise ToolchainError(f"{collection_name} {index} must be an object")
            required = {
                "name",
                "version",
                "license",
                "source_url",
                "artifact",
                "artifact_sha256",
                "executable",
                "executable_sha256",
                "command",
                "args",
                "platform",
                "install",
            }
            if collection_name == "server":
                required.update(
                    {
                        "languages",
                        "extensions",
                        "expected_capabilities",
                        "runtime_dependencies",
                        "launcher",
                        "probe",
                    }
                )
            if set(entry) != required:
                raise ToolchainError(
                    f"{collection_name} {index} has unknown or missing fields"
                )
            name = entry.get("name")
            if not isinstance(name, str) or not name or name in names:
                raise ToolchainError(f"{collection_name} names must be unique strings")
            names.add(name)
            if collection_name == "runtime":
                runtime_names.add(name)
            for field in ("version", "license", "source_url", "artifact", "executable", "command"):
                if not isinstance(entry.get(field), str) or not entry[field]:
                    raise ToolchainError(f"{name}.{field} must be a nonempty string")
            validate_hex_digest(entry.get("artifact_sha256"), f"{name}.artifact_sha256")
            validate_hex_digest(
                entry.get("executable_sha256"), f"{name}.executable_sha256"
            )
            if entry.get("platform") != expected_platform:
                raise ToolchainError(f"{name}.platform must be macos/arm64")
            if not isinstance(entry.get("args"), list) or not all(
                isinstance(value, str) for value in entry["args"]
            ):
                raise ToolchainError(f"{name}.args must be an array of strings")
            for field in (
                ()
                if collection_name == "runtime"
                else (
                    "languages",
                    "extensions",
                    "expected_capabilities",
                    "runtime_dependencies",
                )
            ):
                values = entry.get(field)
                if not isinstance(values, list) or not all(
                    isinstance(value, str) and value for value in values
                ):
                    raise ToolchainError(
                        f"{name}.{field} must be an array of nonempty strings"
                    )
                if len(values) != len(set(values)):
                    raise ToolchainError(f"{name}.{field} contains duplicates")
            if collection_name == "server" and not entry["extensions"]:
                raise ToolchainError(f"{name}.extensions must be nonempty")
            if collection_name == "server" and not entry["expected_capabilities"]:
                raise ToolchainError(f"{name}.expected_capabilities must be nonempty")
            install = entry.get("install")
            if not isinstance(install, dict) or set(install) != {
                "kind",
                "destination",
                "member",
            }:
                raise ToolchainError(f"{name}.install has invalid fields")
            if install.get("kind") not in {
                "tar",
                "tar-member",
                "copy",
                "gzip",
                "zip-member",
            }:
                raise ToolchainError(f"{name}.install.kind is invalid")
            if not isinstance(install.get("destination"), str) or not install[
                "destination"
            ]:
                raise ToolchainError(f"{name}.install.destination is invalid")
            if not isinstance(install.get("member"), str):
                raise ToolchainError(f"{name}.install.member is invalid")
            if collection_name == "server":
                launcher = entry.get("launcher")
                if not isinstance(launcher, dict) or set(launcher) != {
                    "kind",
                    "target",
                }:
                    raise ToolchainError(f"{name}.launcher has invalid fields")
                if launcher.get("kind") not in {
                    "direct",
                    "java-jar",
                    "lua",
                    "node",
                    "python-entry",
                    "repo-python",
                }:
                    raise ToolchainError(f"{name}.launcher.kind is invalid")
                if not isinstance(launcher.get("target"), str) or not launcher[
                    "target"
                ]:
                    raise ToolchainError(f"{name}.launcher.target is invalid")
                probe = entry.get("probe")
                if not isinstance(probe, dict) or not {
                    "file_name",
                    "language_id",
                    "operation",
                }.issubset(probe) or not set(probe).issubset(
                    {"file_name", "language_id", "operation", "configuration"}
                ):
                    raise ToolchainError(f"{name}.probe has invalid fields")
                if probe.get("operation") not in {
                    "textDocument/codeAction",
                    "textDocument/documentSymbol",
                }:
                    raise ToolchainError(f"{name}.probe.operation is invalid")
                if not all(
                    isinstance(probe.get(field), str) and probe[field]
                    for field in ("file_name", "language_id")
                ):
                    raise ToolchainError(f"{name}.probe is incomplete")
                if "configuration" in probe and not isinstance(
                    probe["configuration"], dict
                ):
                    raise ToolchainError(f"{name}.probe.configuration must be an object")
            server_languages = entry.get("languages")
            if collection_name == "server":
                overlap = covered.intersection(server_languages)
                if overlap:
                    raise ToolchainError(
                        f"languages have multiple locked servers: {sorted(overlap)}"
                    )
                covered.update(server_languages)
                server_entries.append(entry)
                for language in server_languages:
                    covered_extensions[language].update(entry["extensions"])
            artifact = entry["artifact"]
            artifacts.append((artifact, entry["artifact_sha256"]))
    unsupported_set = set()
    for index, entry in enumerate(unsupported):
        if not isinstance(entry, dict) or set(entry) != {
            "language",
            "reason",
            "sample",
        }:
            raise ToolchainError(f"unsupported language {index} has invalid fields")
        language = entry.get("language")
        if not isinstance(language, str) or language not in languages:
            raise ToolchainError(f"unsupported language {index} is not in inventory")
        if not all(
            isinstance(entry.get(field), str) and entry[field]
            for field in ("reason", "sample")
        ):
            raise ToolchainError(f"unsupported language {language} lacks evidence")
        unsupported_set.add(language)
    if covered.union(unsupported_set) != languages:
        missing = sorted(languages - covered - unsupported_set)
        extra = sorted(covered.union(unsupported_set) - languages)
        raise ToolchainError(
            f"lock/inventory language mismatch: missing={missing}, extra={extra}"
        )
    if covered.intersection(unsupported_set):
        raise ToolchainError("a language cannot be both covered and unsupported")
    inventory_extensions = {
        entry["language"]: set(entry.get("extensions", {}))
        for entry in inventory.get("languages", [])
        if isinstance(entry, dict) and isinstance(entry.get("language"), str)
    }
    for language in sorted(covered):
        if covered_extensions[language] != inventory_extensions.get(language, set()):
            raise ToolchainError(
                f"locked extensions do not match inventory for {language}: "
                f"lock={sorted(covered_extensions[language])}, "
                f"inventory={sorted(inventory_extensions.get(language, set()))}"
            )
    for entry in server_entries:
        missing_runtime = sorted(set(entry["runtime_dependencies"]) - runtime_names)
        if missing_runtime:
            raise ToolchainError(
                f"{entry['name']} has unknown runtime dependencies: {missing_runtime}"
            )
    if descriptor_path is not None:
        descriptors = load_json_object(descriptor_path, "descriptor inventory")
        if set(descriptors) != {"schema_version", "servers"}:
            raise ToolchainError("descriptor inventory has unknown or missing fields")
        if descriptors.get("schema_version") != SCHEMA_VERSION:
            raise ToolchainError("descriptor inventory schema_version mismatch")
        descriptor_entries = descriptors.get("servers")
        if not isinstance(descriptor_entries, list):
            raise ToolchainError("descriptor inventory servers must be an array")
        descriptor_profiles: dict[str, tuple[str, tuple[str, ...], frozenset[str]]] = {}
        for index, descriptor in enumerate(descriptor_entries):
            if not isinstance(descriptor, dict) or set(descriptor) != {
                "languages",
                "extensions",
                "command",
                "args",
            }:
                raise ToolchainError(f"descriptor {index} has invalid fields")
            if not isinstance(descriptor["command"], str) or not descriptor["command"]:
                raise ToolchainError(f"descriptor {index} command is invalid")
            for field in ("languages", "extensions", "args"):
                values = descriptor[field]
                if not isinstance(values, list) or not all(
                    isinstance(value, str) and value for value in values
                ):
                    raise ToolchainError(
                        f"descriptor {index}.{field} must be nonempty strings"
                    )
                if len(values) != len(set(values)):
                    raise ToolchainError(f"descriptor {index}.{field} contains duplicates")
            if not descriptor["languages"] or not descriptor["extensions"]:
                raise ToolchainError(
                    f"descriptor {index} languages and extensions must be nonempty"
                )
            profile = (
                descriptor["command"],
                tuple(descriptor["args"]),
                frozenset(descriptor["extensions"]),
            )
            for language in descriptor["languages"]:
                if language in descriptor_profiles:
                    raise ToolchainError(
                        f"descriptor language is duplicated: {language}"
                    )
                descriptor_profiles[language] = profile

        missing_descriptors = sorted(covered - descriptor_profiles.keys())
        extra_descriptors = sorted(descriptor_profiles.keys() - covered)
        if missing_descriptors or extra_descriptors:
            raise ToolchainError(
                "RNA descriptor language mismatch: "
                f"missing={missing_descriptors}, extra={extra_descriptors}"
            )
        for entry in server_entries:
            expected_profile = (
                entry["command"],
                tuple(entry["args"]),
                frozenset(entry["extensions"]),
            )
            for language in entry["languages"]:
                if descriptor_profiles[language] != expected_profile:
                    raise ToolchainError(
                        f"RNA descriptor profile mismatch for {language}: "
                        f"lock={expected_profile}, "
                        f"descriptor={descriptor_profiles[language]}"
                    )
    if cache_root is not None:
        for artifact, digest in artifacts:
            path = cache_root / artifact
            if not path.is_file():
                raise ToolchainError(f"missing cache artifact: {artifact}")
            if sha256_file(path) != digest:
                raise ToolchainError(f"cache artifact digest mismatch: {artifact}")
        parser_artifact = cache_root / repo_parser_bundle["artifact"]
        if not parser_artifact.is_file():
            raise ToolchainError(
                f"missing cache artifact: {repo_parser_bundle['artifact']}"
            )
        if sha256_file(parser_artifact) != repo_parser_bundle["artifact_sha256"]:
            raise ToolchainError("repo parser bundle digest mismatch")
    lock_digest = sha256_file(lock_path)
    return {
        "schema_version": SCHEMA_VERSION,
        "compatible": not unsupported_set,
        "inventory_sha256": expected_inventory_sha,
        "lock_sha256": lock_digest,
        "covered_languages": sorted(covered),
        "unsupported_languages": sorted(unsupported_set),
        "cache_verified": cache_root is not None,
        "descriptors_verified": descriptor_path is not None,
    }


def acquire_artifacts(lock_path: Path, cache_root: Path) -> dict[str, Any]:
    lock = load_json_object(lock_path, "toolchain lock")
    cache_root.mkdir(parents=True, exist_ok=True)
    downloaded = 0
    for entry in [*lock.get("runtimes", []), *lock.get("servers", [])]:
        if not isinstance(entry, dict):
            raise ToolchainError("toolchain artifact entry must be an object")
        artifact = entry.get("artifact")
        source_url = entry.get("source_url")
        digest = entry.get("artifact_sha256")
        if not all(isinstance(value, str) and value for value in (artifact, source_url, digest)):
            raise ToolchainError("toolchain artifact entry is incomplete")
        target = cache_root / artifact
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.is_file() and sha256_file(target) == digest:
            continue
        temporary = target.with_name(f".{target.name}.download-{os.getpid()}")
        try:
            with urllib.request.urlopen(source_url) as response, temporary.open("wb") as sink:
                shutil.copyfileobj(response, sink)
            if sha256_file(temporary) != digest:
                raise ToolchainError(f"download digest mismatch: {artifact}")
            temporary.replace(target)
            downloaded += 1
        finally:
            temporary.unlink(missing_ok=True)
    return {"schema_version": SCHEMA_VERSION, "downloaded": downloaded}


def _safe_destination(root: Path, relative: str) -> Path:
    candidate = (root / relative).resolve()
    resolved_root = root.resolve()
    if candidate != resolved_root and resolved_root not in candidate.parents:
        raise ToolchainError(f"install destination escapes toolchain root: {relative}")
    return candidate


def _extract_tar(artifact: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(artifact, mode="r:*") as archive:
        for member in archive.getmembers():
            member_path = (destination / member.name).resolve()
            if destination.resolve() not in member_path.parents and member_path != destination.resolve():
                raise ToolchainError(f"archive member escapes destination: {member.name}")
            if member.issym() or member.islnk():
                link_path = (member_path.parent / member.linkname).resolve()
                if destination.resolve() not in link_path.parents:
                    raise ToolchainError(f"archive link escapes destination: {member.name}")
        archive.extractall(destination, filter="data")


def _install_artifact(
    artifact: Path, install: Mapping[str, str], toolchain_root: Path
) -> None:
    destination = _safe_destination(toolchain_root, install["destination"])
    kind = install["kind"]
    if kind == "tar":
        _extract_tar(artifact, destination)
    elif kind == "tar-member":
        member = install["member"]
        if not member:
            raise ToolchainError("tar-member install requires member")
        destination.parent.mkdir(parents=True, exist_ok=True)
        found = False
        with tarfile.open(artifact, mode="r|*") as archive:
            for member_info in archive:
                if member_info.name != member:
                    continue
                source = archive.extractfile(member_info)
                if source is None:
                    raise ToolchainError(f"tar member is not a regular file: {member}")
                with source, destination.open("wb") as sink:
                    shutil.copyfileobj(source, sink)
                found = True
                break
        if not found:
            raise ToolchainError(f"tar member is missing: {member}")
        destination.chmod(0o755)
    elif kind == "copy":
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(artifact, destination)
        destination.chmod(0o755)
    elif kind == "gzip":
        destination.parent.mkdir(parents=True, exist_ok=True)
        with gzip.open(artifact, "rb") as source, destination.open("wb") as sink:
            shutil.copyfileobj(source, sink)
        destination.chmod(0o755)
    elif kind == "zip-member":
        member = install["member"]
        if not member:
            raise ToolchainError("zip-member install requires member")
        destination.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(artifact) as archive, archive.open(member) as source:
            with destination.open("wb") as sink:
                shutil.copyfileobj(source, sink)
        destination.chmod(0o755)
    else:  # pragma: no cover - verify_lock owns this boundary.
        raise ToolchainError(f"unsupported install kind: {kind}")


def _wrapper(
    kind: str, target: str, *, command_name: str = "", version: str = ""
) -> bytes:
    target_literal = target.replace('"', '\\"')
    version_guard = ""
    if kind == "java-jar":
        if command_name and version:
            version_line = shlex.quote(f"{command_name} {version}")
            version_guard = (
                'if [ "${1-}" = "--version" ]; then\n'
                f"  printf '%s\\n' {version_line}\n"
                "  exit 0\n"
                "fi\n"
            )
        command = (
            'exec "$ROOT/runtimes/jdk-21.0.11+10-jre/Contents/Home/bin/java" '
            f'-jar "$ROOT/{target_literal}" "$@"'
        )
    elif kind == "lua":
        command = f'exec "$ROOT/{target_literal}" "$@"'
    elif kind == "node":
        command = (
            'exec "$ROOT/runtimes/node-v22.12.0-darwin-arm64/bin/node" '
            f'"$ROOT/{target_literal}" "$@"'
        )
    elif kind == "python-entry":
        command = (
            'exec "$ROOT/servers/python-env/bin/python" '
            f'"$ROOT/{target_literal}" "$@"'
        )
    elif kind == "repo-python":
        command = (
            'exec "$ROOT/runtimes/python/bin/python3.12" '
            f'"$ROOT/{target_literal}" "$@"'
        )
    else:
        raise ToolchainError(f"unsupported wrapper kind: {kind}")
    return (
        "#!/bin/sh\n"
        "set -eu\n"
        'case "$0" in */*) SCRIPT_DIR=${0%/*} ;; *) SCRIPT_DIR=. ;; esac\n'
        'ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)\n'
        f"{version_guard}{command}\n"
    ).encode()


def _install_launcher(entry: Mapping[str, Any], toolchain_root: Path) -> None:
    command_path = toolchain_root.resolve() / "bin" / entry["command"]
    command_path.parent.mkdir(parents=True, exist_ok=True)
    launcher = entry["launcher"]
    target = _safe_destination(toolchain_root, launcher["target"])
    if launcher["kind"] == "direct":
        if not target.exists():
            raise ToolchainError(f"launcher target is missing: {launcher['target']}")
        if command_path.exists() and command_path.samefile(target):
            target.chmod(target.stat().st_mode | 0o111)
            return
        if command_path != target:
            command_path.unlink(missing_ok=True)
            command_path.symlink_to(os.path.relpath(target, command_path.parent))
        target.chmod(target.stat().st_mode | 0o111)
    else:
        command_path.write_bytes(
            _wrapper(
                launcher["kind"],
                launcher["target"],
                command_name=entry["command"],
                version=entry["version"],
            )
        )
        command_path.chmod(0o755)


def toolchain_environment(toolchain_root: Path) -> dict[str, str]:
    environment = dict(os.environ)
    paths = [
        toolchain_root / "bin",
        toolchain_root / "runtimes/node-v22.12.0-darwin-arm64/bin",
        toolchain_root / "runtimes/python/bin",
        toolchain_root / "runtimes/jdk-21.0.11+10-jre/Contents/Home/bin",
    ]
    environment["PATH"] = os.pathsep.join(str(path) for path in paths)
    environment["PYTHONNOUSERSITE"] = "1"
    environment["PIP_NO_INDEX"] = "1"
    environment["npm_config_offline"] = "true"
    environment["NO_PROXY"] = "*"
    environment["no_proxy"] = "*"
    return environment


def provision_toolchain(
    lock_path: Path,
    inventory_path: Path,
    cache_root: Path,
    toolchain_root: Path,
    receipt_path: Path,
    *,
    offline: bool,
) -> dict[str, Any]:
    if not offline:
        raise ToolchainError("provision requires --offline")
    verification = verify_lock(lock_path, inventory_path, cache_root, None)
    if not verification["compatible"]:
        raise ToolchainError("cannot provision a lock with unsupported languages")
    if toolchain_root.exists() and any(toolchain_root.iterdir()):
        raise ToolchainError("toolchain root must be absent or empty")
    toolchain_root.mkdir(parents=True, exist_ok=True)
    toolchain_root = toolchain_root.resolve()
    lock = load_json_object(lock_path, "toolchain lock")
    entries = [*lock["runtimes"], *lock["servers"]]
    installed: set[bytes] = set()
    for entry in entries:
        install_key = canonical_json(
            {"artifact": entry["artifact"], "install": entry["install"]}
        )
        if install_key in installed:
            continue
        _install_artifact(cache_root / entry["artifact"], entry["install"], toolchain_root)
        installed.add(install_key)

    python_runtime = toolchain_root / "runtimes/python/bin/python3.12"
    wheelhouse = toolchain_root / "servers/issue785-python-wheelhouse"
    if wheelhouse.is_dir():
        python_env = toolchain_root / "servers/python-env"
        run_checked([str(python_runtime), "-m", "venv", str(python_env)])
        completed = subprocess.run(
            [
                str(python_env / "bin/python"),
                "-m",
                "pip",
                "install",
                "--no-index",
                "--find-links",
                str(wheelhouse),
                "esbonio==2.1.0",
            ],
            check=False,
            capture_output=True,
            text=True,
            env=toolchain_environment(toolchain_root),
        )
        if completed.returncode != 0:
            detail = completed.stderr.strip() or completed.stdout.strip()
            raise ToolchainError(f"offline wheel installation failed: {detail}")

    for entry in lock["servers"]:
        _install_launcher(entry, toolchain_root)

    installed_entries = []
    for entry in entries:
        executable = _safe_destination(toolchain_root, entry["executable"])
        if not executable.is_file():
            raise ToolchainError(f"installed executable is missing: {entry['executable']}")
        actual_digest = sha256_file(executable)
        if actual_digest != entry["executable_sha256"]:
            raise ToolchainError(
                f"installed executable digest mismatch: {entry['name']} "
                f"expected={entry['executable_sha256']} actual={actual_digest}"
            )
        installed_entries.append(
            {
                "name": entry["name"],
                "executable": entry["executable"],
                "sha256": actual_digest,
            }
        )
    receipt = {
        "schema_version": SCHEMA_VERSION,
        "offline": True,
        "platform": lock["platform"],
        "lock_sha256": sha256_file(lock_path),
        "inventory_sha256": sha256_file(inventory_path),
        "installed": sorted(installed_entries, key=lambda item: item["name"]),
    }
    receipt["receipt_digest"] = sha256_bytes(canonical_json(receipt))
    write_canonical_json(receipt_path, receipt)
    return receipt


class JsonRpcProcess:
    def __init__(
        self,
        command: Sequence[str],
        cwd: Path,
        environment: Mapping[str, str],
        configuration: Mapping[str, Any] | None = None,
    ):
        self.process = subprocess.Popen(
            list(command),
            cwd=cwd,
            env=dict(environment),
            bufsize=0,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.next_id = 1
        self.configuration = dict(configuration or {})

    def send(self, value: Mapping[str, Any]) -> None:
        if self.process.stdin is None:
            raise ToolchainError("language server stdin unavailable")
        payload = json.dumps(value, separators=(",", ":")).encode()
        self.process.stdin.write(f"Content-Length: {len(payload)}\r\n\r\n".encode())
        self.process.stdin.write(payload)
        self.process.stdin.flush()

    def _line(self, deadline: float) -> bytes:
        if self.process.stdout is None:
            raise ToolchainError("language server stdout unavailable")
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([self.process.stdout], [], [], remaining)[0]:
            raise ToolchainError("language server response timed out")
        line = self.process.stdout.readline()
        if not line:
            raise ToolchainError("language server exited before response")
        return line

    def receive(self, timeout: float) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        headers: dict[str, str] = {}
        while True:
            line = self._line(deadline)
            if line in {b"\n", b"\r\n"}:
                break
            try:
                name, value = line.decode("ascii").split(":", 1)
            except (UnicodeDecodeError, ValueError) as error:
                raise ToolchainError("language server emitted invalid LSP headers") from error
            headers[name.lower()] = value.strip()
        try:
            length = int(headers["content-length"])
        except (KeyError, ValueError) as error:
            raise ToolchainError("language server response lacks Content-Length") from error
        if self.process.stdout is None:
            raise ToolchainError("language server stdout unavailable")
        payload = self.process.stdout.read(length)
        if len(payload) != length:
            raise ToolchainError("language server emitted truncated JSON-RPC")
        try:
            message = json.loads(payload)
        except json.JSONDecodeError as error:
            raise ToolchainError("language server emitted invalid JSON-RPC") from error
        if not isinstance(message, dict):
            raise ToolchainError("language server JSON-RPC must be an object")
        return message

    def request(self, method: str, params: Any, timeout: float) -> Any:
        request_id = self.next_id
        self.next_id += 1
        self.send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        deadline = time.monotonic() + timeout
        while True:
            message = self.receive(max(deadline - time.monotonic(), 0.001))
            if message.get("id") == request_id and ("result" in message or "error" in message):
                if "error" in message:
                    raise ToolchainError(
                        f"language server {method} failed: "
                        f"{json.dumps(message['error'], sort_keys=True)}"
                    )
                return message.get("result")
            if "id" in message and isinstance(message.get("method"), str):
                self.send(
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": self._server_request_result(message),
                    }
                )

    def notify(self, method: str, params: Any) -> None:
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def quiesce(self, quiet_seconds: float, timeout: float) -> int:
        if self.process.stdout is None:
            raise ToolchainError("language server stdout unavailable")
        deadline = time.monotonic() + timeout
        quiet_deadline = time.monotonic() + quiet_seconds
        messages = 0
        while time.monotonic() < deadline:
            wait = min(quiet_deadline - time.monotonic(), deadline - time.monotonic())
            if wait <= 0:
                return messages
            if not select.select([self.process.stdout], [], [], wait)[0]:
                return messages
            message = self.receive(max(deadline - time.monotonic(), 0.001))
            messages += 1
            quiet_deadline = time.monotonic() + quiet_seconds
            if "id" in message and isinstance(message.get("method"), str):
                self.send(
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": self._server_request_result(message),
                    }
                )
        raise ToolchainError("language server did not reach quiescence")

    def _server_request_result(self, message: Mapping[str, Any]) -> Any:
        if message.get("method") == "workspace/configuration":
            params = message.get("params")
            items = params.get("items") if isinstance(params, dict) else None
            return [self.configuration for _ in items] if isinstance(items, list) else []
        if message.get("method") == "workspace/workspaceFolders":
            return []
        return None

    def close(self, timeout: float) -> tuple[int, str]:
        with contextlib.suppress(Exception):
            self.request("shutdown", None, timeout)
            self.notify("exit", None)
        if self.process.stdin:
            self.process.stdin.close()
        try:
            returncode = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise ToolchainError("language server did not shut down")
        stderr = ""
        if self.process.stderr:
            stderr = self.process.stderr.read().decode(errors="replace").strip()
        if returncode != 0:
            raise ToolchainError(
                f"language server exited with {returncode}: {stderr[-1000:]}"
            )
        return returncode, stderr[-1000:]


def _operation_capabilities(capabilities: Mapping[str, Any]) -> list[str]:
    operations = []
    document_symbols = capabilities.get("documentSymbolProvider")
    if document_symbols is not None and document_symbols is not False:
        operations.append("textDocument/documentSymbol")
    code_actions = capabilities.get("codeActionProvider")
    if code_actions is not None and code_actions is not False:
        operations.append("textDocument/codeAction")
    return operations


def probe_server(
    entry: Mapping[str, Any], toolchain_root: Path, timeout: float
) -> dict[str, Any]:
    command = [str(toolchain_root / "bin" / entry["command"]), *entry["args"]]
    with tempfile.TemporaryDirectory(prefix="rna-lsp-probe-") as temporary:
        root = Path(temporary)
        probe = entry["probe"]
        document = root / probe["file_name"]
        document.parent.mkdir(parents=True, exist_ok=True)
        document.write_text("# probe\nprobe = 1\n")
        uri = document.resolve().as_uri()
        configuration = probe.get("configuration")
        rpc = JsonRpcProcess(
            command,
            root,
            toolchain_environment(toolchain_root),
            configuration if isinstance(configuration, dict) else None,
        )
        started = time.monotonic()
        stage = "initialize"
        try:
            initialized = rpc.request(
                "initialize",
                {
                    "processId": None,
                    "rootUri": root.resolve().as_uri(),
                    "rootPath": str(root.resolve()),
                    "capabilities": {
                        "workspace": {
                            "configuration": True,
                            "workspaceFolders": True,
                        },
                        "textDocument": {
                            "documentSymbol": {
                                "hierarchicalDocumentSymbolSupport": True
                            },
                            "codeAction": {
                                "codeActionLiteralSupport": {
                                    "codeActionKind": {"valueSet": ["quickfix"]}
                                }
                            },
                        },
                    },
                    "workspaceFolders": [
                        {"uri": root.resolve().as_uri(), "name": "probe"}
                    ],
                    "clientInfo": {"name": "rna-lsp-toolchain-probe", "version": "1"},
                },
                timeout,
            )
            initialize_ms = int((time.monotonic() - started) * 1000)
            if not isinstance(initialized, dict) or not isinstance(
                initialized.get("capabilities"), dict
            ):
                raise ToolchainError("initialize response lacks capabilities")
            capabilities = initialized["capabilities"]
            operations = _operation_capabilities(capabilities)
            missing_capabilities = sorted(
                set(entry["expected_capabilities"]) - set(operations)
            )
            if missing_capabilities:
                raise ToolchainError(
                    f"{entry['name']} capability drift: "
                    f"missing={missing_capabilities} actual={operations}"
                )
            if probe["operation"] not in operations:
                raise ToolchainError(
                    f"{entry['name']} did not negotiate {probe['operation']}"
                )
            rpc.notify("initialized", {})
            if isinstance(configuration, dict):
                rpc.notify(
                    "workspace/didChangeConfiguration",
                    {"settings": configuration},
                )
            stage = "quiescence"
            quiescence_messages = rpc.quiesce(0.2, timeout)
            workspace_ready_ms = int((time.monotonic() - started) * 1000)
            rpc.notify(
                "textDocument/didOpen",
                {
                    "textDocument": {
                        "uri": uri,
                        "languageId": probe["language_id"],
                        "version": 1,
                        "text": document.read_text(),
                    }
                },
            )
            stage = probe["operation"]
            operation_started = time.monotonic()
            if probe["operation"] == "textDocument/documentSymbol":
                result = rpc.request(
                    probe["operation"], {"textDocument": {"uri": uri}}, timeout
                )
            else:
                result = rpc.request(
                    probe["operation"],
                    {
                        "textDocument": {"uri": uri},
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 1, "character": 0},
                        },
                        "context": {"diagnostics": [], "only": ["quickfix"]},
                    },
                    timeout,
                )
            operation_ms = int((time.monotonic() - operation_started) * 1000)
            if result is not None and not isinstance(result, list):
                raise ToolchainError(
                    f"{entry['name']} {probe['operation']} returned non-array"
                )
            shutdown_started = time.monotonic()
            stage = "shutdown"
            _, stderr = rpc.close(timeout)
            shutdown_ms = int((time.monotonic() - shutdown_started) * 1000)
            return {
                "name": entry["name"],
                "version": entry["version"],
                "languages": entry["languages"],
                "command": entry["command"],
                "args": entry["args"],
                "negotiated_capabilities": operations,
                "operation": probe["operation"],
                "result_count": len(result) if isinstance(result, list) else 0,
                "initialize_ms": initialize_ms,
                "workspace_ready_ms": workspace_ready_ms,
                "operation_ms": operation_ms,
                "shutdown_ms": shutdown_ms,
                "quiescence_messages": quiescence_messages,
                "stderr_tail": stderr,
                "status": "ready",
            }
        except Exception as error:
            if rpc.process.poll() is None:
                rpc.process.kill()
                rpc.process.wait()
            stderr = ""
            if rpc.process.stderr:
                stderr = rpc.process.stderr.read().decode(errors="replace").strip()
            if isinstance(error, ToolchainError) and stderr:
                raise ToolchainError(
                    f"{stage}: {error}; stderr={stderr[-2000:]}"
                ) from error
            if isinstance(error, ToolchainError):
                raise ToolchainError(f"{stage}: {error}") from error
            raise


def probe_toolchain(
    lock_path: Path,
    inventory_path: Path,
    toolchain_root: Path,
    output_path: Path,
    timeout: float,
) -> dict[str, Any]:
    verification = verify_lock(lock_path, inventory_path, None, None)
    if not verification["compatible"]:
        raise ToolchainError("cannot probe a lock with unsupported languages")
    lock = load_json_object(lock_path, "toolchain lock")
    receipts = []
    for entry in sorted(lock["servers"], key=lambda item: item["name"]):
        try:
            receipts.append(probe_server(entry, toolchain_root, timeout))
        except ToolchainError as error:
            raise ToolchainError(f"{entry['name']} probe failed: {error}") from error
    result = {
        "schema_version": SCHEMA_VERSION,
        "lock_sha256": sha256_file(lock_path),
        "server_count": len(receipts),
        "servers": receipts,
    }
    result["probe_digest"] = sha256_bytes(canonical_json(result))
    write_canonical_json(output_path, result)
    return result


def _run_logged(
    args: Sequence[str],
    cwd: Path,
    environment: Mapping[str, str],
    log_path: Path,
    *,
    timeout_seconds: float | None = None,
    timeout_evidence_path: Path | None = None,
) -> float:
    started = time.monotonic()
    process = subprocess.Popen(
        list(args),
        cwd=cwd,
        env=dict(environment),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=timeout_seconds is not None,
    )
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
    duration = time.monotonic() - started
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_bytes(stdout + b"\n--- stderr ---\n" + stderr)
    if timed_out:
        if timeout_evidence_path is None:
            timeout_evidence_path = log_path.with_suffix(".timeout.json")
        timeout_evidence = {
            "schema_version": SCHEMA_VERSION,
            "status": "timed_out",
            "command": list(args),
            "cwd": str(cwd.resolve()),
            "timeout_ms": int((timeout_seconds or 0) * 1000),
            "duration_ms": int(duration * 1000),
            "termination": ["SIGTERM", "SIGKILL-if-needed"],
            "returncode": process.returncode,
            "log_path": str(log_path.resolve()),
            "log_sha256": sha256_file(log_path),
        }
        write_canonical_json(timeout_evidence_path, timeout_evidence)
        raise ToolchainError(
            f"command timed out after {timeout_seconds:.0f}s "
            f"({' '.join(args)}), log={log_path}"
        )
    if process.returncode != 0:
        detail = stderr.decode(errors="replace").strip()
        raise ToolchainError(
            f"command failed ({' '.join(args)}), log={log_path}: {detail[-2000:]}"
        )
    return duration


def _load_logged_json_object(log_path: Path, label: str) -> dict[str, Any]:
    """Read the stdout JSON captured by ``_run_logged``."""
    try:
        logged = log_path.read_bytes()
    except OSError as error:
        raise ToolchainError(f"unable to read {label}") from error
    stdout, separator, _stderr = logged.partition(b"\n--- stderr ---\n")
    if not separator:
        raise ToolchainError(f"{label} is missing the stderr separator")
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise ToolchainError(f"{label} stdout is not valid JSON") from error
    if not isinstance(value, dict):
        raise ToolchainError(f"{label} stdout must be a JSON object")
    return value


def _require_ready_case(
    readiness_log_path: Path,
    persisted_report: Mapping[str, Any],
    instance_id: str,
) -> None:
    readiness = _load_logged_json_object(
        readiness_log_path, f"{instance_id} readiness output"
    )
    if readiness.get("ready") is not True:
        raise ToolchainError(
            f"{instance_id} readiness gate is not ready: "
            f"ready={readiness.get('ready')!r}"
        )
    if readiness.get("compatibility_violations") != []:
        raise ToolchainError(
            f"{instance_id} readiness has compatibility violations"
        )
    live_report = readiness.get("report")
    if not isinstance(live_report, dict):
        raise ToolchainError(f"{instance_id} readiness output is missing its report")
    if live_report.get("violations") != []:
        raise ToolchainError(f"{instance_id} readiness report has violations")
    if live_report != persisted_report:
        raise ToolchainError(
            f"{instance_id} live readiness report does not match persisted evidence"
        )


def _readiness_validation_request_count(report: Mapping[str, Any]) -> int:
    by_language = report.get("readiness_validation_requests_by_language")
    if not isinstance(by_language, dict):
        raise ToolchainError(
            "readiness report is missing validation-request accounting"
        )
    count = 0
    for language, value in by_language.items():
        if not isinstance(language, str) or not language:
            raise ToolchainError("readiness validation language is invalid")
        if type(value) is not int or value < 0:
            raise ToolchainError(
                f"readiness validation request count is invalid for {language}"
            )
        count += value
    return count


def _preserve_failed_case_evidence(
    checkout: Path,
    instance_id: str,
    cases_root: Path,
    scan_log_path: Path,
    error: Exception,
    attempt_slug: str | None = None,
    publication_artifacts: Sequence[Path] = (),
) -> Path:
    """Copy failure diagnostics out of an ephemeral qualification checkout."""
    evidence_stem = attempt_slug or instance_id
    evidence_root = cases_root / f"{evidence_stem}-failure-evidence"
    if evidence_root.exists() or evidence_root.is_symlink():
        raise ToolchainError(f"refusing to overwrite failure evidence: {evidence_root}")
    evidence_root.mkdir(parents=True)
    cache_root = checkout / ".oh" / ".cache"
    evidence_sources = [
        cache_root / "lsp_completeness.json",
        cache_root / "lsp_pass1_work_items.json",
        cache_root / "lsp_completed.json",
        cache_root / "extract_completed.json",
        cache_root / "scan-state.json",
        cache_root / "enrichment_jobs.json",
        cache_root / "operation_reports.json",
        cache_root / "structural-cache-inheritance.json",
        cache_root / "structural-cache-execution.json",
        cache_root / "lance" / "scan_version",
        cache_root / "lance" / "schema_version",
    ]
    full_cache_retained = False
    full_cache_error = None
    if cache_root.is_dir() and not cache_root.is_symlink():
        try:
            members = _structural_cache_files(cache_root)
            evidence_sources.extend(cache_root / member["path"] for member in members)
            full_cache_retained = True
        except ToolchainError as cache_error:
            full_cache_error = str(cache_error)

    copied = []
    seen: set[Path] = set()
    for source in evidence_sources:
        if source in seen or not source.is_file():
            continue
        if source.is_symlink():
            full_cache_retained = False
            full_cache_error = (
                full_cache_error
                or f"refusing symlink in failed cache evidence: {source}"
            )
            continue
        seen.add(source)
        relative = source.relative_to(cache_root)
        destination = evidence_root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        copied.append(
            {
                "cache_path": relative.as_posix(),
                "evidence_path": str(destination.resolve()),
                "sha256": sha256_file(destination),
                "size_bytes": destination.stat().st_size,
            }
        )

    graph_snapshot_digest = None
    completeness_path = evidence_root / "lsp_completeness.json"
    if completeness_path.is_file():
        try:
            completeness = load_json_object(
                completeness_path, f"{instance_id} failed readiness report"
            )
            graph_snapshot_digest = completeness.get("graph_snapshot_digest")
        except ToolchainError:
            # The copied bytes and digest remain useful even when the producer
            # failed while finalizing JSON.
            pass

    retained_publication_artifacts = []
    seen_publication_paths: set[Path] = set()
    for artifact in publication_artifacts:
        resolved = artifact.resolve()
        if resolved in seen_publication_paths or not artifact.is_file():
            continue
        if artifact.is_symlink():
            raise ToolchainError(
                f"refusing symlink publication artifact in failure evidence: {artifact}"
            )
        seen_publication_paths.add(resolved)
        retained_publication_artifacts.append(
            {
                "path": str(resolved),
                "sha256": sha256_file(artifact),
                "size_bytes": artifact.stat().st_size,
            }
        )

    receipt = {
        "schema_version": SCHEMA_VERSION,
        "status": "failed",
        "instance_id": instance_id,
        "error": str(error),
        "graph_snapshot_digest": graph_snapshot_digest,
        "scan_log_path": str(scan_log_path.resolve()),
        "scan_log_sha256": sha256_file(scan_log_path)
        if scan_log_path.is_file()
        else None,
        "evidence": copied,
        "publication_artifacts": retained_publication_artifacts,
        "full_cache_retained": full_cache_retained,
        "full_cache_error": full_cache_error,
    }
    receipt["failure_digest"] = sha256_bytes(canonical_json(receipt))
    receipt_path = cases_root / f"{evidence_stem}-failure.json"
    _publish_canonical_json_exclusive(receipt_path, receipt)
    return receipt_path


def qualify_population(
    lock_path: Path,
    inventory_path: Path,
    population_path: Path,
    git_cache_root: Path,
    toolchain_root: Path,
    rna_binary: Path,
    output_root: Path,
    case_timeout_seconds: float = 1800.0,
) -> dict[str, Any]:
    rna_binary = rna_binary.resolve()
    verification = verify_lock(lock_path, inventory_path, None, None)
    if not verification["compatible"]:
        raise ToolchainError("cannot qualify a lock with unsupported languages")
    if not rna_binary.is_file():
        raise ToolchainError(f"RNA binary is missing: {rna_binary}")
    if not math.isfinite(case_timeout_seconds) or case_timeout_seconds <= 0:
        raise ToolchainError("case timeout must be a positive finite number")
    git_cache_verification = verify_git_cache(population_path, git_cache_root)
    instances = included_population(load_json_object(population_path, "population"))
    inventory = load_json_object(inventory_path, "frozen inventory")
    inventory_digest = _require_sha256(
        inventory.get("inventory_digest"), "frozen inventory digest"
    )
    inventory_file_sha256 = sha256_file(inventory_path)
    inventory_cases = {
        case["instance_id"]: case
        for case in inventory.get("cases", [])
        if isinstance(case, dict) and isinstance(case.get("instance_id"), str)
    }
    if set(inventory_cases) != {instance["instance_id"] for instance in instances}:
        raise ToolchainError("frozen inventory/population case identities differ")
    toolchain_lock_digest = sha256_file(lock_path)
    git_binary = shutil.which("git")
    if git_binary is None:
        raise ToolchainError("git is required to materialize frozen checkouts")
    output_root.mkdir(parents=True, exist_ok=True)
    cases_root = output_root / "cases"
    logs_root = output_root / "logs"
    cases_root.mkdir(exist_ok=True)
    logs_root.mkdir(exist_ok=True)
    archives_root = output_root / "structural-caches"
    archives_root.mkdir(exist_ok=True)
    environment = toolchain_environment(toolchain_root)
    probe_path = output_root / "probe.json"
    probe_started = time.monotonic()
    probe_toolchain(lock_path, inventory_path, toolchain_root, probe_path, 30.0)
    probe_seconds = time.monotonic() - probe_started
    cohort_cases = []
    timing_cases = []
    for index, instance in enumerate(instances, start=1):
        instance_id = instance["instance_id"]
        attempt_index = _next_case_attempt_index(output_root, instance_id)
        attempt_slug = f"{instance_id}-attempt-{attempt_index:03d}"
        git_dir = git_cache_path(git_cache_root, instance["repo"])
        if not git_dir.is_dir():
            raise ToolchainError(f"missing git cache for {instance['repo']}")
        with tempfile.TemporaryDirectory(prefix=f"rna-lsp-n70-{index:02d}-") as temporary:
            checkout = Path(temporary) / "checkout"
            clone_log_path = logs_root / f"{attempt_slug}-clone.log"
            checkout_log_path = logs_root / f"{attempt_slug}-checkout.log"
            try:
                _run_logged(
                    [git_binary, "clone", "--quiet", "--no-checkout", str(git_dir), str(checkout)],
                    Path(temporary),
                    environment,
                    clone_log_path,
                )
                _run_logged(
                    [git_binary, "checkout", "--quiet", "--detach", instance["base_commit"]],
                    checkout,
                    environment,
                    checkout_log_path,
                )
                actual = run_checked([git_binary, "rev-parse", "HEAD"], cwd=checkout)
                if actual != instance["base_commit"]:
                    raise ToolchainError(
                        f"checkout identity drift for {instance_id}: {actual}"
                    )
                # Normalize the offline local clone identity without touching
                # its frozen commit/tree or contacting the network.
                run_checked(
                    [
                        git_binary,
                        "remote",
                        "set-url",
                        "origin",
                        f"https://github.com/{instance['repo']}.git",
                    ],
                    cwd=checkout,
                )
            except Exception as error:
                failure_log = checkout_log_path if checkout_log_path.is_file() else clone_log_path
                failure_path = _preserve_failed_case_evidence(
                    checkout,
                    instance_id,
                    cases_root,
                    failure_log,
                    error,
                    attempt_slug,
                )
                raise ToolchainError(
                    f"checkout preparation failed for {instance_id}; "
                    f"failure evidence={failure_path}"
                ) from error
            cache_preparation_log_path = (
                logs_root / f"{attempt_slug}-cache-preparation.log"
            )
            try:
                target_identity = structural_cache_identity(rna_binary, checkout)
                if target_identity["repository"] != instance["repo"]:
                    raise ToolchainError(
                        f"RNA repository identity mismatch for {instance_id}: "
                        f"{target_identity['repository']}"
                    )
            except Exception as error:
                cache_preparation_log_path.write_text(f"{type(error).__name__}: {error}\n")
                failure_path = _preserve_failed_case_evidence(
                    checkout,
                    instance_id,
                    cases_root,
                    cache_preparation_log_path,
                    error,
                    attempt_slug,
                )
                raise ToolchainError(
                    f"cache identity failed for {instance_id}; "
                    f"failure evidence={failure_path}"
                ) from error
            selection_started = time.monotonic()
            try:
                selection = select_structural_cache(
                    output_root,
                    instance["repo"],
                    instance["base_commit"],
                    target_identity,
                    git_dir,
                    index,
                    toolchain_lock_digest=toolchain_lock_digest,
                    inventory_digest=inventory_digest,
                    inventory_file_sha256=inventory_file_sha256,
                )
            except Exception as error:
                cache_preparation_log_path.write_text(f"{type(error).__name__}: {error}\n")
                failure_path = _preserve_failed_case_evidence(
                    checkout,
                    instance_id,
                    cases_root,
                    cache_preparation_log_path,
                    error,
                    attempt_slug,
                )
                raise ToolchainError(
                    f"cache selection failed for {instance_id}; "
                    f"failure evidence={failure_path}"
                ) from error
            selection_seconds = time.monotonic() - selection_started
            verification_seconds = 0.0
            injection_seconds = 0.0
            injection_receipt = None
            if selection is not None:
                try:
                    materialized_cache = Path(temporary) / "verified-structural-cache"
                    verification_started = time.monotonic()
                    verified = verify_structural_cache_archive(
                        Path(selection["entry"]["archive_path"]),
                        Path(selection["entry"]["sidecar_path"]),
                        expected={
                            "repository": instance["repo"],
                            "root_slug": target_identity["root_slug"],
                            "producer": target_identity["producer"],
                            "toolchain_lock_digest": toolchain_lock_digest,
                            "inventory_digest": inventory_digest,
                            "inventory_file_sha256": inventory_file_sha256,
                            "inventory_policy_digest": target_identity[
                                "inventory_policy_digest"
                            ],
                            "scan_flags": QUALIFICATION_SCAN_FLAGS,
                        },
                        materialize_cache=materialized_cache,
                    )
                    verification_seconds = time.monotonic() - verification_started
                    injection_started = time.monotonic()
                    injection_receipt = inject_structural_cache(
                        selection,
                        checkout,
                        target_identity,
                        git_dir,
                        toolchain_lock_digest=toolchain_lock_digest,
                        inventory_digest=inventory_digest,
                        inventory_file_sha256=inventory_file_sha256,
                        verified=verified,
                        materialized_cache=materialized_cache,
                    )
                    injection_seconds = time.monotonic() - injection_started
                except Exception as error:
                    cache_preparation_log_path.write_text(
                        f"{type(error).__name__}: {error}\n"
                    )
                    failure_path = _preserve_failed_case_evidence(
                        checkout,
                        instance_id,
                        cases_root,
                        cache_preparation_log_path,
                        error,
                        attempt_slug,
                    )
                    raise ToolchainError(
                        f"cache verification/injection failed for {instance_id}; "
                        f"failure evidence={failure_path}"
                    ) from error

            case_environment = dict(environment)
            if injection_receipt is not None:
                case_environment[STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV] = (
                    injection_receipt["authorization_sha256"]
                )

            scan_log_path = logs_root / f"{attempt_slug}-scan.log"
            archive_path = archives_root / f"{attempt_slug}.tar.gz"
            sidecar_path = archives_root / f"{attempt_slug}.manifest.json"
            try:
                scan_seconds = _run_logged(
                    [
                        str(rna_binary),
                        "--business-context",
                        "disabled",
                        "scan",
                        "--repo",
                        str(checkout),
                        "--full",
                        "--no-embed",
                        "--timings",
                    ],
                    checkout,
                    case_environment,
                    scan_log_path,
                    timeout_seconds=case_timeout_seconds,
                    timeout_evidence_path=logs_root
                    / f"{attempt_slug}-scan-timeout.json",
                )
                readiness_log_path = logs_root / f"{attempt_slug}-readiness.log"
                readiness_seconds = _run_logged(
                    [
                        str(rna_binary),
                        "--business-context",
                        "disabled",
                        "lsp-readiness",
                        "--repo",
                        str(checkout),
                        "--json",
                    ],
                    checkout,
                    case_environment,
                    readiness_log_path,
                )
                source_report = checkout / ".oh/.cache/lsp_completeness.json"
                if not source_report.is_file():
                    raise ToolchainError(f"readiness report missing for {instance_id}")
                report = load_json_object(
                    source_report, f"{instance_id} readiness report"
                )
                _require_ready_case(readiness_log_path, report, instance_id)
                execution_path = checkout / ".oh/.cache/structural-cache-execution.json"
                execution = (
                    load_json_object(execution_path, f"{instance_id} cache execution")
                    if execution_path.is_file()
                    else None
                )
                if injection_receipt is not None and execution is None:
                    raise ToolchainError(
                        f"{instance_id} injected cache produced no execution evidence"
                    )
                if execution is not None:
                    for metric in (
                        "inherited_graph_enrichment_operation_count",
                        "executed_graph_enrichment_operation_count",
                        "inherited_readiness_validation_request_count",
                        "executed_readiness_validation_request_count",
                    ):
                        value = execution.get(metric)
                        if type(value) is not int or value < 0:
                            raise ToolchainError(
                                f"{instance_id} cache execution metric {metric} is invalid"
                            )
                cold_executed_graph_enrichment_operation_count = 0
                if injection_receipt is None:
                    cold_work_store = load_json_object(
                        checkout / ".oh/.cache/lsp_pass1_work_items.json",
                        f"{instance_id} LSP work ledger",
                    )
                    cold_records = cold_work_store.get("records")
                    if not isinstance(cold_records, dict):
                        raise ToolchainError(
                            f"{instance_id} LSP work ledger has no records map"
                        )
                    cold_executed_graph_enrichment_operation_count = sum(
                        len(record.get("requested_operations", []))
                        for record in cold_records.values()
                        if isinstance(record, dict)
                        and record.get("state") == "completed"
                        and isinstance(record.get("requested_operations"), list)
                    )
                cold_executed_readiness_validation_request_count = (
                    _readiness_validation_request_count(report)
                    if injection_receipt is None
                    else 0
                )
                archive_started = time.monotonic()
                archive_receipt = archive_structural_cache(
                    checkout,
                    archive_path,
                    sidecar_path,
                    identity=target_identity,
                    toolchain_lock_digest=toolchain_lock_digest,
                    inventory_digest=inventory_digest,
                    inventory_file_sha256=inventory_file_sha256,
                    case_inventory_digest=_require_sha256(
                        inventory_cases[instance_id].get("per_file_digest"),
                        f"{instance_id} inventory digest",
                    ),
                    base_cache=(
                        {
                            "archive_sha256": injection_receipt[
                                "base_archive_sha256"
                            ],
                            "sidecar_sha256": injection_receipt[
                                "base_sidecar_sha256"
                            ],
                            "core_sha256": injection_receipt["base_core_sha256"],
                            "report_digest": injection_receipt[
                                "base_report_digest"
                            ],
                        }
                        if injection_receipt is not None
                        else None
                    ),
                )
                archive_seconds = time.monotonic() - archive_started
            except ToolchainError as error:
                failure_path = _preserve_failed_case_evidence(
                    checkout,
                    instance_id,
                    cases_root,
                    scan_log_path,
                    error,
                    attempt_slug,
                    publication_artifacts=[archive_path, sidecar_path],
                )
                raise ToolchainError(
                    f"{error}; failure evidence={failure_path}"
                ) from error
            publication_artifacts = [archive_path, sidecar_path]
            publication_log_path = logs_root / f"{attempt_slug}-publication.log"
            try:
                report_path = cases_root / f"{attempt_slug}.json"
                _publish_canonical_json_exclusive(report_path, report)
                publication_artifacts.append(report_path)
                execution_counts = execution or {}
                case_receipt = {
                    "schema_version": SCHEMA_VERSION,
                    "status": "ready",
                    "offline_preprocessing": True,
                    "instance_id": instance_id,
                    "attempt_index": attempt_index,
                    "repository": instance["repo"],
                    "base_commit": instance["base_commit"],
                    "tree": target_identity["tree"],
                    "producer": target_identity["producer"],
                    "toolchain_lock_digest": toolchain_lock_digest,
                    "inventory_digest": inventory_digest,
                    "inventory_file_sha256": inventory_file_sha256,
                    "case_inventory_digest": inventory_cases[instance_id][
                        "per_file_digest"
                    ],
                    "configuration_digest": target_identity["configuration_digest"],
                    "scan_flags": QUALIFICATION_SCAN_FLAGS,
                    "report_path": str(report_path.resolve()),
                    "report_sha256": sha256_file(report_path),
                    "report_digest": report["digest"],
                    "graph_snapshot_digest": report["graph_snapshot_digest"],
                    "cache": archive_receipt,
                    "base_cache": injection_receipt,
                    "timings_ms": {
                        "cache_selection": int(selection_seconds * 1000),
                        "cache_verification": int(verification_seconds * 1000),
                        "cache_injection": int(injection_seconds * 1000),
                        "scan_update": int(scan_seconds * 1000),
                        "full_readiness_validation": int(readiness_seconds * 1000),
                        "cache_archive": int(archive_seconds * 1000),
                    },
                    "changed_file_count": execution_counts.get(
                        "changed_file_count",
                        selection["diff"]["distance"] if selection is not None else 0,
                    ),
                    "invalidated_file_count": execution_counts.get(
                        "invalidated_file_count", 0
                    ),
                    "graph_enrichment_operations_reused": execution_counts.get(
                        "inherited_graph_enrichment_operation_count", 0
                    ),
                    "graph_enrichment_operations_executed": execution_counts.get(
                        "executed_graph_enrichment_operation_count",
                        cold_executed_graph_enrichment_operation_count,
                    ),
                    "readiness_validation_requests_reused": execution_counts.get(
                        "inherited_readiness_validation_request_count", 0
                    ),
                    "readiness_validation_requests_executed": execution_counts.get(
                        "executed_readiness_validation_request_count",
                        cold_executed_readiness_validation_request_count,
                    ),
                }
                case_receipt["receipt_digest"] = sha256_bytes(
                    canonical_json(case_receipt)
                )
                case_receipt_path = cases_root / f"{attempt_slug}-receipt.json"
                _publish_canonical_json_exclusive(case_receipt_path, case_receipt)
                publication_artifacts.append(case_receipt_path)
                _publish_cache_catalog_entry(
                    output_root,
                    {
                        "schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION,
                        "status": "ready",
                        "case_index": index,
                        "attempt_index": attempt_index,
                        "instance_id": instance_id,
                        "repository": instance["repo"],
                        "commit": instance["base_commit"],
                        "tree": target_identity["tree"],
                        "archive_path": archive_receipt["archive_path"],
                        "archive_sha256": archive_receipt["archive_sha256"],
                        "core_sha256": archive_receipt["core_sha256"],
                        "sidecar_path": archive_receipt["sidecar_path"],
                        "sidecar_sha256": archive_receipt["sidecar_sha256"],
                        "report_digest": report["digest"],
                        "receipt_path": str(case_receipt_path.resolve()),
                        "receipt_sha256": sha256_file(case_receipt_path),
                    },
                )
                cohort_cases.append(
                    {
                        "instance_id": instance_id,
                        "repository": instance["repo"],
                        "base_commit": instance["base_commit"],
                        "report_path": str(report_path.resolve()),
                        "receipt_path": str(case_receipt_path.resolve()),
                        "receipt_sha256": sha256_file(case_receipt_path),
                        "archive_sha256": archive_receipt["archive_sha256"],
                        "sidecar_sha256": archive_receipt["sidecar_sha256"],
                        "core_sha256": archive_receipt["core_sha256"],
                    }
                )
                timing_cases.append(
                    {
                        "instance_id": instance_id,
                        "scan_ms": int(scan_seconds * 1000),
                        "readiness_ms": int(readiness_seconds * 1000),
                        "cache_selection_ms": int(selection_seconds * 1000),
                        "cache_verification_ms": int(verification_seconds * 1000),
                        "cache_injection_ms": int(injection_seconds * 1000),
                        "cache_archive_ms": int(archive_seconds * 1000),
                        "report_sha256": sha256_file(report_path),
                    }
                )
            except Exception as error:
                publication_log_path.write_text(
                    f"{type(error).__name__}: {error}\n"
                )
                failure_path = _preserve_failed_case_evidence(
                    checkout,
                    instance_id,
                    cases_root,
                    publication_log_path,
                    error,
                    attempt_slug,
                    publication_artifacts=publication_artifacts,
                )
                raise ToolchainError(
                    f"case evidence publication failed for {instance_id}; "
                    f"failure evidence={failure_path}"
                ) from error

    cohort_manifest = {"schema_version": SCHEMA_VERSION, "cases": cohort_cases}
    manifest_path = output_root / "cohort-manifest.json"
    write_canonical_json(manifest_path, cohort_manifest)
    aggregate_path = output_root / "aggregate.json"
    aggregate_log = logs_root / "aggregate.log"
    _run_logged(
        [
            str(rna_binary),
            "lsp-readiness",
            "--cohort-manifest",
            str(manifest_path),
            "--aggregate-output",
            str(aggregate_path),
            "--json",
        ],
        output_root,
        environment,
        aggregate_log,
    )
    aggregate = load_json_object(aggregate_path, "aggregate readiness report")
    if aggregate.get("status") != "ready":
        raise ToolchainError("aggregate readiness status is not ready")
    timings = {
        "schema_version": SCHEMA_VERSION,
        "cases": timing_cases,
        "server_probe": {
            "path": str(probe_path.resolve()),
            "sha256": sha256_file(probe_path),
            "duration_ms": int(probe_seconds * 1000),
        },
    }
    timings["timings_digest"] = sha256_bytes(canonical_json(timings))
    timings_path = output_root / "timings.json"
    write_canonical_json(timings_path, timings)
    seal = {
        "schema_version": SCHEMA_VERSION,
        "population_sha256": sha256_file(population_path),
        "inventory_sha256": sha256_file(inventory_path),
        "lock_sha256": sha256_file(lock_path),
        "cohort_manifest_sha256": sha256_file(manifest_path),
        "aggregate_sha256": sha256_file(aggregate_path),
        "probe_sha256": sha256_file(probe_path),
        "timings_sha256": sha256_file(timings_path),
        "structural_cache_catalog_sha256": sha256_file(
            output_root / STRUCTURAL_CACHE_CATALOG
        ),
        "case_count": len(cohort_cases),
        "git_cache_verification_digest": git_cache_verification[
            "verification_digest"
        ],
        "status": "ready",
    }
    seal["seal_digest"] = sha256_bytes(canonical_json(seal))
    write_canonical_json(output_root / "seal.json", seal)
    return seal


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    acquire_git = subparsers.add_parser("acquire-git")
    acquire_git.add_argument("--population", type=Path, required=True)
    acquire_git.add_argument("--git-cache", type=Path, required=True)

    verify_git = subparsers.add_parser("verify-git-cache")
    verify_git.add_argument("--population", type=Path, required=True)
    verify_git.add_argument("--git-cache", type=Path, required=True)

    inventory = subparsers.add_parser("inventory")
    inventory.add_argument("--population", type=Path, required=True)
    inventory.add_argument("--git-cache", type=Path, required=True)
    inventory.add_argument("--output", type=Path, required=True)
    inventory.add_argument("--file-evidence-output", type=Path, required=True)

    acquire = subparsers.add_parser("acquire-artifacts")
    acquire.add_argument("--lock", type=Path, required=True)
    acquire.add_argument("--cache", type=Path, required=True)

    verify = subparsers.add_parser("verify-lock")
    verify.add_argument("--lock", type=Path, required=True)
    verify.add_argument("--inventory", type=Path, required=True)
    verify.add_argument("--cache", type=Path)
    verify.add_argument("--descriptors", type=Path)

    seal = subparsers.add_parser("seal-directory")
    seal.add_argument("--source", type=Path, required=True)
    seal.add_argument("--output", type=Path, required=True)
    seal.add_argument("--root-name", required=True)

    repo_bundle = subparsers.add_parser("build-repo-parser-bundle")
    repo_bundle.add_argument("--repo", type=Path, default=Path("."))
    repo_bundle.add_argument("--output", type=Path, required=True)

    provision = subparsers.add_parser("provision")
    provision.add_argument("--lock", type=Path, required=True)
    provision.add_argument("--inventory", type=Path, required=True)
    provision.add_argument("--cache", type=Path, required=True)
    provision.add_argument("--root", type=Path, required=True)
    provision.add_argument("--receipt", type=Path, required=True)
    provision.add_argument("--offline", action="store_true")

    probe = subparsers.add_parser("probe")
    probe.add_argument("--lock", type=Path, required=True)
    probe.add_argument("--inventory", type=Path, required=True)
    probe.add_argument("--root", type=Path, required=True)
    probe.add_argument("--output", type=Path, required=True)
    probe.add_argument("--timeout", type=float, default=20.0)

    qualify = subparsers.add_parser("qualify")
    qualify.add_argument("--lock", type=Path, required=True)
    qualify.add_argument("--inventory", type=Path, required=True)
    qualify.add_argument("--population", type=Path, required=True)
    qualify.add_argument("--git-cache", type=Path, required=True)
    qualify.add_argument("--root", type=Path, required=True)
    qualify.add_argument("--rna", type=Path, required=True)
    qualify.add_argument("--output", type=Path, required=True)
    qualify.add_argument("--case-timeout-seconds", type=float, default=1800.0)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "acquire-git":
            result = acquire_git_cache(args.population, args.git_cache)
        elif args.command == "verify-git-cache":
            result = verify_git_cache(args.population, args.git_cache)
        elif args.command == "inventory":
            result = inventory_population(
                args.population, args.git_cache, args.file_evidence_output
            )
            write_canonical_json(args.output, result)
        elif args.command == "acquire-artifacts":
            result = acquire_artifacts(args.lock, args.cache)
        elif args.command == "verify-lock":
            result = verify_lock(
                args.lock, args.inventory, args.cache, args.descriptors
            )
        elif args.command == "seal-directory":
            result = seal_directory(args.source, args.output, args.root_name)
        elif args.command == "build-repo-parser-bundle":
            result = build_repo_parser_bundle(args.repo.resolve(), args.output)
        elif args.command == "provision":
            result = provision_toolchain(
                args.lock,
                args.inventory,
                args.cache,
                args.root,
                args.receipt,
                offline=args.offline,
            )
        elif args.command == "probe":
            result = probe_toolchain(
                args.lock, args.inventory, args.root, args.output, args.timeout
            )
        elif args.command == "qualify":
            result = qualify_population(
                args.lock,
                args.inventory,
                args.population,
                args.git_cache,
                args.root,
                args.rna,
                args.output,
                args.case_timeout_seconds,
            )
        else:  # pragma: no cover - argparse owns this boundary.
            parser.error("unknown command")
        print(canonical_json(result).decode(), end="")
        return 0
    except ToolchainError as error:
        print(f"swebench-lsp-toolchain: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
