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
import re
import resource
import select
import selectors
import shlex
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO, Iterator, Mapping, Sequence


SCHEMA_VERSION = 1
PROVISION_RECEIPT_FILE = ".rna-provision-receipt.json"
STRUCTURAL_CACHE_SCHEMA_VERSION = 1
STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION = 2
STRUCTURAL_CACHE_ROOT = "cache"
STRUCTURAL_CACHE_CORE = ".rna-structural-cache-core.json"
STRUCTURAL_CACHE_CATALOG = "structural-cache-catalog.json"
STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV = (
    "RNA_STRUCTURAL_CACHE_AUTHORIZATION_SHA256"
)
STRUCTURAL_CACHE_MAX_MEMBERS = 250_000
STRUCTURAL_CACHE_MAX_MEMBER_BYTES = 8 * 1024 * 1024 * 1024
STRUCTURAL_CACHE_MAX_TOTAL_BYTES = 64 * 1024 * 1024 * 1024
STRUCTURAL_CACHE_MAX_EXECUTED_LSP_OPERATIONS = 12_288
STRUCTURAL_CACHE_OPERATION_BUDGET_BASIS = (
    "verified_base_capability_aware_per_path_work_ledger_with_language_median_for_unseen_paths"
)
STRUCTURAL_CACHE_LSP_OPERATIONS = frozenset(
    {
        "call_hierarchy",
        "references",
        "definitions",
        "implementations",
        "type_hierarchy",
        "document_symbols",
        "document_links",
    }
)
STRUCTURAL_CACHE_FORBIDDEN_COMPONENTS = frozenset(
    {"embedding", "embeddings", "rerank", "reranker", "vectors", "vector-index"}
)
STRUCTURAL_CACHE_FORBIDDEN_PATHS = frozenset({"lance/artifacts.lance"})
STRUCTURAL_CACHE_LSP_IMPACT_MARKERS = (
    "->calls->",
    "->referenced_by->",
    "->references->",
    "->depends_on->",
    "->implements->",
    "->re_exports->",
    "->tested_by->",
)
QUALIFICATION_SCAN_FLAGS = [
    "--business-context=disabled",
    "scan",
    "--full",
    "--no-embed",
    "--timings",
]
COMBINED_QUALIFICATION_SCAN_FLAGS = [
    "--business-context=disabled",
    "scan",
    "--full",
    "--timings",
]
COMBINED_QUERY = "function returns value"
COMBINED_STRICT_SEARCH_SENTINEL = (
    "status=READY embeddings=true retrieval=hybrid rerank=true "
    "metal=true fallback=false"
)
COMBINED_QUERY_PROBE_NAMES = (
    "first_hybrid_rerank",
    "graph_traversal",
    "full_body",
    "minified_body",
    "repeat_hybrid_1",
    "repeat_hybrid_2",
    "warm_hybrid_rerank",
)
REPLAY_RECEIPT_FIELDS = {
    "schema_version",
    "diagnostic_only",
    "publishable",
    "checkout_rebuilt",
    "lsp_calls",
    "archive_created",
    "catalog_updated",
    "failure_receipt_sha256",
    "failure_digest",
    "authorization_sha256",
    "source_producer_commit",
    "replay_producer_commit",
    "source_producer",
    "replay_producer",
    "target_commit",
    "target_tree",
    "target_tree_source",
    "source_checkout_identity_verified",
    "source_tree_diff_replayed",
    "source_rescanned",
    "full_target_readiness_recomputed",
    "incremental_enrichment_job_id",
    "pass1_job_ids",
    "initial_node_count",
    "initial_edge_count",
    "stale_path_count",
    "stale_node_count_before",
    "stale_edge_count_before",
    "removed_node_count",
    "removed_edge_count",
    "final_node_count",
    "final_edge_count",
    "completed_work_item_count",
    "executed_operation_count",
    "readiness_validation_request_count",
    "base_completeness_digest",
    "target_inventory_path_count",
    "validated_inventory_path_count",
    "observed_result_count",
    "persisted_observed_result_count",
    "persisted_result_id_count",
    "unresolved_endpoint_count",
    "discarded_required_result_count",
    "checkpoint_validation_digest",
    "diagnostic_checkpoint_validation_passed",
    "target_completeness_digest",
    "coverage_violation_count",
    "compatibility_violation_count",
    "full_target_ready",
}
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
    normalized_folded = normalized.casefold()
    if (
        lowered & STRUCTURAL_CACHE_FORBIDDEN_COMPONENTS
        or any(
            normalized_folded == forbidden
            or normalized_folded.startswith(f"{forbidden}/")
            for forbidden in STRUCTURAL_CACHE_FORBIDDEN_PATHS
        )
    ):
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


def _partition_invalidation_plan(
    core: Mapping[str, Any], target_identity: Mapping[str, Any]
) -> tuple[list[str], list[str], dict[str, list[dict[str, str]]]]:
    base_signatures = core["partition_signatures"]
    target_partitions = target_identity["partitions"]
    shared_changed = (
        core["shared_influence_digest"]
        != target_identity["shared_influence_digest"]
    )
    configuration_changed = (
        core["configuration_digest"] != target_identity["configuration_digest"]
    )
    invalidated: list[str] = []
    compatible: list[str] = []
    reasons: dict[str, list[dict[str, str]]] = {}
    for language, base_signature in sorted(base_signatures.items()):
        partition_reasons: list[dict[str, str]] = []
        target_partition = target_partitions.get(language)
        if shared_changed:
            partition_reasons.append(
                {
                    "code": "shared_influence_digest_mismatch",
                    "base_digest": core["shared_influence_digest"],
                    "target_digest": target_identity["shared_influence_digest"],
                }
            )
        if configuration_changed:
            partition_reasons.append(
                {
                    "code": "configuration_digest_mismatch",
                    "base_digest": core["configuration_digest"],
                    "target_digest": target_identity["configuration_digest"],
                }
            )
        if not isinstance(target_partition, dict):
            partition_reasons.append(
                {
                    "code": "target_partition_missing",
                    "base_digest": base_signature,
                    "target_digest": "missing",
                }
            )
        elif target_partition["signature"] != base_signature:
            partition_reasons.append(
                {
                    "code": "partition_signature_mismatch",
                    "base_digest": base_signature,
                    "target_digest": target_partition["signature"],
                }
            )
        if (
            shared_changed
            or configuration_changed
            or not isinstance(target_partition, dict)
            or target_partition["signature"] != base_signature
        ):
            invalidated.append(language)
            reasons[language] = partition_reasons
        else:
            compatible.append(language)
    for language in sorted(set(target_partitions) - set(base_signatures)):
        target_signature = target_partitions[language]["signature"]
        invalidated.append(language)
        reasons[language] = [
            {
                "code": "base_partition_missing",
                "base_digest": "missing",
                "target_digest": target_signature,
            }
        ]
    return invalidated, compatible, reasons


def _validate_selected_partition_plan(
    selection: Mapping[str, Any] | None, target_identity: Mapping[str, Any]
) -> None:
    if selection is None:
        return
    target_languages = sorted(target_identity["partitions"])
    invalidated = selection.get("invalidated_partitions")
    compatible = selection.get("compatible_partitions")
    reasons = selection.get("invalidated_partition_reasons")
    if (
        not isinstance(invalidated, list)
        or invalidated != sorted(set(invalidated))
        or not isinstance(compatible, list)
        or compatible != sorted(set(compatible))
        or not isinstance(reasons, dict)
        or set(reasons) != set(invalidated)
    ):
        raise ToolchainError("structural-cache partition preflight is malformed")
    for language in invalidated:
        language_reasons = reasons.get(language)
        if not isinstance(language_reasons, list) or not language_reasons:
            raise ToolchainError(
                f"structural-cache invalidation reason is missing for {language}"
            )
        for reason in language_reasons:
            if (
                not isinstance(reason, dict)
                or set(reason) != {"code", "base_digest", "target_digest"}
                or not all(isinstance(value, str) and value for value in reason.values())
            ):
                raise ToolchainError(
                    f"structural-cache invalidation reason is malformed for {language}"
                )
    if len(target_languages) > 1 and set(target_languages).issubset(set(invalidated)):
        allowed_global_reasons = {
            "shared_influence_digest_mismatch",
            "configuration_digest_mismatch",
            "producer_identity_mismatch",
            "graph_schema_signature_mismatch",
            "toolchain_lock_digest_mismatch",
            "inventory_digest_mismatch",
            "inventory_file_sha256_mismatch",
            "inventory_policy_digest_mismatch",
            "scan_flags_mismatch",
        }
        common_reason_codes = set.intersection(
            *(
                {reason["code"] for reason in reasons[language]}
                for language in target_languages
            )
        )
        if common_reason_codes.isdisjoint(allowed_global_reasons):
            raise ToolchainError(
                "structural-cache preflight rejected unexpected all-partition "
                f"invalidation: partition_count={len(target_languages)} "
                f"common_reason_codes={sorted(common_reason_codes)}"
            )


def _preflight_reason(
    code: str, base_value: Any, target_value: Any
) -> dict[str, str]:
    def identity_digest(value: Any) -> str:
        if (
            isinstance(value, str)
            and len(value) == 64
            and all(character in "0123456789abcdef" for character in value.lower())
        ):
            return value.lower()
        if value == "missing":
            return "missing"
        return sha256_bytes(canonical_json(value))

    return {
        "code": code,
        "base_digest": identity_digest(base_value),
        "target_digest": identity_digest(target_value),
    }


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
    diagnostics: dict[str, Any] | None = None,
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
    rejection_reasons: list[dict[str, str]] = []
    for entry in catalog["entries"]:
        if (
            entry.get("status") != "ready"
            or entry.get("repository") != repository
            or type(entry.get("case_index")) is not int
            or entry["case_index"] > case_index
        ):
            continue
        if entry.get("schema_version") != STRUCTURAL_CACHE_SCHEMA_VERSION:
            rejection_reasons.append(
                _preflight_reason(
                    "cache_schema_version_mismatch",
                    entry.get("schema_version"),
                    STRUCTURAL_CACHE_SCHEMA_VERSION,
                )
            )
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
        candidate_mismatches = []
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
                code = {
                    "root_slug": "root_slug_mismatch",
                    "producer": "producer_identity_mismatch",
                    "toolchain_lock_digest": "toolchain_lock_digest_mismatch",
                    "inventory_digest": "inventory_digest_mismatch",
                    "inventory_file_sha256": "inventory_file_sha256_mismatch",
                    "inventory_policy_digest": "inventory_policy_digest_mismatch",
                    "scan_flags": "scan_flags_mismatch",
                }.get(field, f"{field}_mismatch")
                candidate_mismatches.append(
                    _preflight_reason(code, actual, expected[field])
                )
                if field == "producer" and (
                    actual.get("graph_schema_signature")
                    != expected[field].get("graph_schema_signature")
                ):
                    candidate_mismatches.append(
                        _preflight_reason(
                            "graph_schema_signature_mismatch",
                            actual.get("graph_schema_signature"),
                            expected[field].get("graph_schema_signature"),
                        )
                    )
        if not candidate_mismatches:
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
        rejection_reasons.extend(candidate_mismatches)
        # A globally incompatible candidate is a cold-rebuild choice, not an
        # extraction candidate. Tamper is still fail-closed above because the
        # sidecar/archive publication identities were fully validated.
        continue
    if not candidates:
        if diagnostics is not None:
            canonical_reasons = {
                canonical_json(reason): reason for reason in rejection_reasons
            }
            diagnostics["cold_rebuild_reasons"] = [
                canonical_reasons[key] for key in sorted(canonical_reasons)
            ] or [
                _preflight_reason(
                    "no_repository_cache_available", "missing", target_commit
                )
            ]
        return None
    _, _, _, _, entry, verified, diff = min(
        candidates, key=lambda candidate: candidate[:4]
    )
    (
        invalidated_partitions,
        compatible_partitions,
        invalidated_partition_reasons,
    ) = _partition_invalidation_plan(
        verified["core"], target_identity
    )
    return {
        "entry": entry,
        "verified": verified,
        "diff": diff,
        "invalidated_partitions": invalidated_partitions,
        "compatible_partitions": compatible_partitions,
        "invalidated_partition_reasons": invalidated_partition_reasons,
    }


def _fixed_point_lsp_impact_paths(
    files: Sequence[Mapping[str, Any]],
    *,
    root_slug: str,
    direct_paths: set[str],
    target_paths: set[str],
) -> list[str]:
    """Return the target-bounded fixed-point closure of persisted OLD LSP edges.

    Result ownership identifies which request produced an edge, not both files
    the persisted edge can invalidate. Resolve the global union of carried LSP
    result IDs to their two target-inventory endpoints, then traverse those
    edges bidirectionally from every direct seed until no target path is added.
    Endpoints outside ``root_slug`` and paths outside the target inventory can
    never enter or bridge the closure.
    """

    if not direct_paths:
        return []
    known_paths = set(target_paths) | set(direct_paths)
    path_trie: dict[str, Any] = {}
    terminal = ""
    for path in sorted(known_paths):
        node = path_trie
        for character in f"{path}:":
            node = node.setdefault(character, {})
        node[terminal] = path

    def endpoint_path(stable_id: str) -> str | None:
        prefix = f"{root_slug}:"
        if not stable_id.startswith(prefix):
            return None
        node = path_trie
        resolved: set[str] = set()
        for character in stable_id[len(prefix) :]:
            child = node.get(character)
            if not isinstance(child, dict):
                break
            node = child
            candidate = node.get(terminal)
            if isinstance(candidate, str):
                resolved.add(candidate)
        if len(resolved) > 1:
            raise ToolchainError(
                "base completeness persisted LSP endpoint is ambiguous"
            )
        return next(iter(resolved), None)

    persisted_edge_ids: set[str] = set()
    for file_record in files:
        if not isinstance(file_record, Mapping):
            raise ToolchainError("base completeness file record is malformed")
        path = file_record.get("path")
        persisted_results = file_record.get("persisted_results")
        provenance = (
            persisted_results.get("provenance")
            if isinstance(persisted_results, Mapping)
            else None
        )
        if not isinstance(path, str) or not isinstance(provenance, list):
            raise ToolchainError("base completeness persisted LSP evidence is malformed")
        for result_id in provenance:
            if not isinstance(result_id, str):
                raise ToolchainError("base completeness persisted LSP ID is malformed")
            if any(
                marker in result_id for marker in STRUCTURAL_CACHE_LSP_IMPACT_MARKERS
            ):
                persisted_edge_ids.add(result_id)

    adjacency: dict[str, set[str]] = collections.defaultdict(set)
    for result_id in sorted(persisted_edge_ids):
        markers = [
            candidate
            for candidate in STRUCTURAL_CACHE_LSP_IMPACT_MARKERS
            if candidate in result_id
        ]
        if len(markers) != 1 or result_id.count(markers[0]) != 1:
            raise ToolchainError(
                "base completeness persisted LSP edge ID is ambiguous"
            )
        endpoints = result_id.split(markers[0])
        from_path = endpoint_path(endpoints[0])
        to_path = endpoint_path(endpoints[1])
        if from_path is None or to_path is None or from_path == to_path:
            continue
        adjacency[from_path].add(to_path)
        adjacency[to_path].add(from_path)

    impacted: set[str] = set()
    visited = set(direct_paths)
    frontier = collections.deque(sorted(direct_paths))
    while frontier:
        path = frontier.popleft()
        for neighbor in sorted(adjacency.get(path, ())):
            if neighbor not in target_paths or neighbor in visited:
                continue
            visited.add(neighbor)
            impacted.add(neighbor)
            frontier.append(neighbor)

    impacted.difference_update(direct_paths)
    return sorted(impacted)


def _bounded_fixed_point_lsp_impact_paths(
    files: Sequence[Mapping[str, Any]],
    *,
    root_slug: str,
    direct_paths: set[str],
    path_partitions: Mapping[str, str],
    base_languages: Mapping[str, str],
    invalidated_partitions: set[str],
) -> list[str]:
    """Return the signed executed-minus-direct plan at graph/partition fixed point."""

    target_paths = set(path_partitions)
    executed = set(direct_paths)
    executed.update(
        path
        for path, language in path_partitions.items()
        if language in invalidated_partitions
    )
    impact_limit = min(4_096, max(1, len(path_partitions) // 2))
    while True:
        previous = set(executed)
        executed.update(
            _fixed_point_lsp_impact_paths(
                files,
                root_slug=root_slug,
                direct_paths=executed,
                target_paths=target_paths,
            )
        )
        if len(executed) > impact_limit:
            escalated_languages = {
                language
                for path in executed
                for language in (
                    path_partitions.get(path) or base_languages.get(path),
                )
                if isinstance(language, str) and language
            }
            executed.update(
                path
                for path, language in path_partitions.items()
                if language in escalated_languages
            )
        if executed == previous:
            return sorted(executed - direct_paths)


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
    validation_producers: dict[tuple[str, str], set[str]] = collections.defaultdict(set)
    jobs_path = cache_root / "enrichment_jobs.json"
    if jobs_path.is_file():
        jobs_store = load_json_object(jobs_path, "injected LSP enrichment-job ledger")
        jobs_value = jobs_store.get("jobs")
        if not isinstance(jobs_value, list):
            raise ToolchainError("injected LSP enrichment-job ledger has no jobs list")
        for job in jobs_value:
            if not isinstance(job, dict):
                raise ToolchainError("injected LSP enrichment-job record is malformed")
            if job.get("state") != "completed" or job.get("capability") != "call_references":
                continue
            job_id = job.get("job_id")
            evidence = job.get("lsp_evidence")
            if not isinstance(job_id, str) or not job_id or not isinstance(evidence, dict):
                raise ToolchainError("completed LSP enrichment job lacks durable evidence identity")
            validations = evidence.get("validations")
            if not isinstance(validations, list):
                raise ToolchainError("completed LSP enrichment job has no validations list")
            producer_id = f"enrichment-job:{job_id}"
            for validation in validations:
                if not isinstance(validation, dict):
                    raise ToolchainError("LSP validation evidence is malformed")
                if validation.get("status") != "processed":
                    continue
                symbols = validation.get("document_symbols", [])
                if not isinstance(symbols, list):
                    raise ToolchainError("document-symbol validation evidence is malformed")
                for symbol in symbols:
                    if not isinstance(symbol, dict):
                        raise ToolchainError("document-symbol response evidence is malformed")
                    path = symbol.get("file")
                    result_id = symbol.get("graph_result_id")
                    if isinstance(path, str) and path and isinstance(result_id, str) and result_id:
                        validation_producers[(path, result_id)].add(producer_id)

    def result_producer_ids(
        path: str,
        result_id: str,
        records: Sequence[tuple[str, Mapping[str, Any]]],
    ) -> list[str]:
        work_producers = [
            record_key
            for record_key, record in records
            if isinstance(record.get("produced_result_ids", []), list)
            and result_id in record.get("produced_result_ids", [])
        ]
        return sorted(
            set(work_producers) | validation_producers.get((path, result_id), set())
        )

    completed_operation_count_by_path = {
        path: sum(
            len(record.get("requested_operations", []))
            for _, record in records
            if isinstance(record.get("requested_operations", []), list)
        )
        for path, records in completed_by_path.items()
    }
    diff = selection["diff"]
    touched = set(diff["changed_paths"]) | set(diff["added_paths"]) | set(
        diff["deleted_paths"]
    )
    for old, new in diff["renamed_paths"]:
        touched.add(old)
        touched.add(new)
    target_blobs = _git_tree_blobs(git_dir, target_identity["commit"])
    invalidated = set(selection["invalidated_partitions"])
    invalidation_reasons = {
        language: [dict(reason) for reason in language_reasons]
        for language, language_reasons in selection[
            "invalidated_partition_reasons"
        ].items()
    }
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
    # invalidated. Establish every lineage-driven partition rebuild before
    # signing the impact closure so those partitions are closure roots too.
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
                if not isinstance(result_id, str) or not result_producer_ids(
                    path, result_id, records
                ):
                    invalidated.add(language)
                    reason = {
                        "code": "missing_producer_lineage",
                        "base_digest": (
                            result_id if isinstance(result_id, str) and result_id else "invalid"
                        ),
                        "target_digest": "required",
                    }
                    if reason not in invalidation_reasons.setdefault(language, []):
                        invalidation_reasons[language].append(reason)
                    break

    impact_closure_paths = _bounded_fixed_point_lsp_impact_paths(
        files,
        root_slug=target_identity["root_slug"],
        direct_paths=touched,
        path_partitions=path_partitions,
        base_languages=base_languages,
        invalidated_partitions=invalidated,
    )
    touched_for_inheritance = touched | set(impact_closure_paths)

    inherited_files = []
    inherited_work_count = 0
    for file_record in sorted(files, key=lambda record: record.get("path", "")):
        path = file_record.get("path")
        language = file_record.get("language")
        terminal = file_record.get("terminal_status")
        if (
            not isinstance(path, str)
            or not isinstance(language, str)
            or path in touched_for_inheritance
            or path not in target_blobs
            or language in invalidated
            or not isinstance(terminal, dict)
            or terminal.get("status") != "processed"
        ):
            continue
        partition = target_identity["partitions"].get(language)
        if not isinstance(partition, dict):
            invalidated.add(language)
            reason = {
                "code": "target_partition_missing",
                "base_digest": "present",
                "target_digest": "missing",
            }
            if reason not in invalidation_reasons.setdefault(language, []):
                invalidation_reasons[language].append(reason)
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
                "producer_ids": result_producer_ids(path, result_id, records),
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
        set(impact_closure_paths)
        | {
            file_record["path"]
            for file_record in files
            if isinstance(file_record, dict)
            and isinstance(file_record.get("path"), str)
            and file_record.get("language") in invalidated
            and file_record["path"] in target_blobs
        }
    )
    inherited_paths = {file_record["path"] for file_record in inherited_files}
    renamed_source_by_target = {new: old for old, new in diff["renamed_paths"]}
    operation_counts_by_language: dict[str, list[int]] = collections.defaultdict(list)
    for file_record in files:
        if not isinstance(file_record, dict):
            continue
        path = file_record.get("path")
        language = file_record.get("language")
        if isinstance(path, str) and isinstance(language, str):
            operation_counts_by_language[language].append(
                completed_operation_count_by_path.get(path, 0)
            )
    for counts in operation_counts_by_language.values():
        counts.sort()
    predicted_executed_work_count = 0
    estimated_operation_file_count = 0
    for path, language in sorted(path_partitions.items()):
        if path in inherited_paths:
            continue
        prior_path = path if path in completed_operation_count_by_path else (
            renamed_source_by_target.get(path)
        )
        if prior_path in completed_operation_count_by_path:
            predicted_executed_work_count += completed_operation_count_by_path[prior_path]
            continue
        historical_counts = operation_counts_by_language.get(language, [])
        predicted_executed_work_count += (
            historical_counts[(len(historical_counts) - 1) // 2]
            if historical_counts
            else 0
        )
        estimated_operation_file_count += 1
    authorized_operations_by_language: dict[str, list[str]] = {}
    for language in sorted(set(path_partitions.values())):
        authorized_operations_by_language[language] = sorted(
            {
                operation
                for path, records in completed_by_path.items()
                if base_languages.get(path) == language
                for _, record in records
                for operation in record.get("requested_operations", [])
                if isinstance(operation, str) and operation
            }
        )
    executed_operation_budget = {
        "max_operations": STRUCTURAL_CACHE_MAX_EXECUTED_LSP_OPERATIONS,
        "executed_estimate": predicted_executed_work_count,
        "authorized_operations_by_language": authorized_operations_by_language,
        "basis": STRUCTURAL_CACHE_OPERATION_BUDGET_BASIS,
        "estimated_file_count": estimated_operation_file_count,
    }
    authorization = {
        "schema_version": STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
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
        "executed_operation_budget": executed_operation_budget,
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
        "predicted_executed_graph_enrichment_operation_count": (
            predicted_executed_work_count
        ),
        "predicted_total_graph_enrichment_operation_count": (
            inherited_work_count + predicted_executed_work_count
        ),
        "predicted_operation_estimated_file_count": estimated_operation_file_count,
        "executed_operation_budget": executed_operation_budget,
        "impact_closure_basis": "verified_base_persisted_old_lsp_edge_fixed_point",
        "impact_closure_paths": impact_closure_paths,
        "changed_file_count": diff["distance"],
        "invalidated_partitions": sorted(invalidated),
        "compatible_partitions": sorted(
            set(selection["compatible_partitions"]) - invalidated
        ),
        "invalidated_partition_reasons": {
            language: sorted(
                language_reasons,
                key=lambda reason: (
                    reason["code"],
                    reason["base_digest"],
                    reason["target_digest"],
                ),
            )
            for language, language_reasons in sorted(invalidation_reasons.items())
            if language in invalidated
        },
        "target_file_count": len(path_partitions),
        "predicted_executed_file_count": len(path_partitions) - len(inherited_files),
    }


def build_structural_cache_preflight(
    *,
    case_index: int,
    instance_id: str,
    inventory_case: Mapping[str, Any],
    target_identity: Mapping[str, Any],
    selection: Mapping[str, Any] | None,
    injection_receipt: Mapping[str, Any] | None,
    cold_rebuild_reasons: Sequence[Mapping[str, str]] | None = None,
) -> dict[str, Any]:
    target_file_count = inventory_case.get("included_file_count")
    if type(target_file_count) is not int or target_file_count < 0:
        raise ToolchainError(f"{instance_id} inventory file count is invalid")
    target_languages = sorted(target_identity["partitions"])
    if selection is None:
        selected_base_cache = None
        compatible_partitions: list[str] = []
        inherited_partitions: list[str] = []
        reasons = [dict(reason) for reason in cold_rebuild_reasons or []] or [
            _preflight_reason(
                "no_repository_cache_available", "missing", target_identity["commit"]
            )
        ]
        invalidated_partition_reasons = {
            language: [dict(reason) for reason in reasons]
            for language in target_languages
        }
        inherited_file_count = 0
        executed_file_count = target_file_count
        expected_operation_count = {
            "inherited_exact": 0,
            "executed_estimate": None,
            "total_estimate": None,
            "max_executed": STRUCTURAL_CACHE_MAX_EXECUTED_LSP_OPERATIONS,
            "authorized_operations_by_language": None,
            "basis": "cold_no_base_work_ledger",
            "estimated_file_count": target_file_count,
        }
        impact_closure_paths: list[str] = []
        impact_closure_basis = "cold_no_base_persisted_old_lsp_edge_fixed_point"
    else:
        if injection_receipt is None:
            raise ToolchainError("selected cache preflight has no injection receipt")
        core = selection["verified"]["core"]
        entry = selection["entry"]
        selected_base_cache = {
            "instance_id": entry["instance_id"],
            "attempt_index": int(entry.get("attempt_index", 0)),
            "repository": core["repository"],
            "commit": core["commit"],
            "tree": core["tree"],
            "archive_sha256": injection_receipt["base_archive_sha256"],
            "sidecar_sha256": injection_receipt["base_sidecar_sha256"],
            "core_sha256": injection_receipt["base_core_sha256"],
        }
        compatible_partitions = list(injection_receipt["compatible_partitions"])
        invalidated_partition_reasons = {
            language: [dict(reason) for reason in language_reasons]
            for language, language_reasons in injection_receipt[
                "invalidated_partition_reasons"
            ].items()
        }
        authorization = load_json_object(
            Path(injection_receipt["authorization_path"]),
            f"{instance_id} structural-cache authorization",
        )
        if (
            authorization.get("schema_version")
            != STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION
            or authorization.get("offline_preprocessing") is not True
        ):
            raise ToolchainError(
                f"{instance_id} structural-cache authorization schema/status mismatch"
            )
        signed_budget = authorization.get("executed_operation_budget")
        if (
            not isinstance(signed_budget, dict)
            or signed_budget != injection_receipt.get("executed_operation_budget")
        ):
            raise ToolchainError(
                f"{instance_id} authorization/preflight operation budget mismatch"
            )
        inherited_partitions = sorted(
            {
                inherited["language"]
                for inherited in authorization["inherited_files"]
                if isinstance(inherited, dict)
                and isinstance(inherited.get("language"), str)
            }
        )
        inherited_file_count = injection_receipt["inherited_file_count"]
        executed_file_count = injection_receipt["predicted_executed_file_count"]
        if injection_receipt["target_file_count"] != target_file_count:
            raise ToolchainError(
                f"{instance_id} preflight target files differ from frozen inventory: "
                f"planned={injection_receipt['target_file_count']} "
                f"inventory={target_file_count}"
            )
        expected_operation_count = {
            "inherited_exact": injection_receipt[
                "inherited_graph_enrichment_operation_count"
            ],
            "executed_estimate": signed_budget["executed_estimate"],
            "total_estimate": (
                injection_receipt["inherited_graph_enrichment_operation_count"]
                + signed_budget["executed_estimate"]
            ),
            "max_executed": signed_budget["max_operations"],
            "authorized_operations_by_language": signed_budget[
                "authorized_operations_by_language"
            ],
            "basis": signed_budget["basis"],
            "estimated_file_count": signed_budget["estimated_file_count"],
        }
        impact_closure_paths = list(injection_receipt["impact_closure_paths"])
        impact_closure_basis = injection_receipt["impact_closure_basis"]
    invalidated_partitions = [
        {
            "language": language,
            "reasons": sorted(
                invalidated_partition_reasons[language],
                key=lambda reason: (
                    reason["code"],
                    reason["base_digest"],
                    reason["target_digest"],
                ),
            ),
        }
        for language in sorted(invalidated_partition_reasons)
    ]
    preflight = {
        "schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION,
        "status": "ready_to_enrich",
        "case_index": case_index,
        "instance_id": instance_id,
        "repository": target_identity["repository"],
        "target_commit": target_identity["commit"],
        "target_tree": target_identity["tree"],
        "selected_base_cache": selected_base_cache,
        "compatible_partitions": compatible_partitions,
        "inherited_partitions": inherited_partitions,
        "invalidated_partitions": invalidated_partitions,
        "predicted_file_counts": {
            "target": target_file_count,
            "inherited": inherited_file_count,
            "executed": executed_file_count,
        },
        "expected_operation_count": expected_operation_count,
        "impact_closure": {
            "basis": impact_closure_basis,
            "paths": impact_closure_paths,
            "path_count": len(impact_closure_paths),
            "paths_digest": sha256_bytes(canonical_json(impact_closure_paths)),
        },
        "digest": "",
    }
    preflight["digest"] = sha256_bytes(canonical_json(preflight))
    validate_structural_cache_preflight(preflight, target_identity)
    return preflight


def validate_structural_cache_preflight(
    preflight: Mapping[str, Any], target_identity: Mapping[str, Any]
) -> None:
    _require_exact_fields(
        preflight,
        {
            "schema_version",
            "status",
            "case_index",
            "instance_id",
            "repository",
            "target_commit",
            "target_tree",
            "selected_base_cache",
            "compatible_partitions",
            "inherited_partitions",
            "invalidated_partitions",
            "predicted_file_counts",
            "expected_operation_count",
            "impact_closure",
            "digest",
        },
        "structural-cache preflight",
    )
    if (
        preflight["schema_version"] != STRUCTURAL_CACHE_SCHEMA_VERSION
        or preflight["status"] != "ready_to_enrich"
        or type(preflight["case_index"]) is not int
        or preflight["case_index"] <= 0
        or preflight["repository"] != target_identity["repository"]
        or preflight["target_commit"] != target_identity["commit"]
        or preflight["target_tree"] != target_identity["tree"]
    ):
        raise ToolchainError("structural-cache preflight target identity is invalid")
    digest = _require_sha256(preflight["digest"], "structural-cache preflight digest")
    digest_value = dict(preflight)
    digest_value["digest"] = ""
    if sha256_bytes(canonical_json(digest_value)) != digest:
        raise ToolchainError("structural-cache preflight digest mismatch")
    compatible = preflight["compatible_partitions"]
    inherited = preflight["inherited_partitions"]
    invalidated_records = preflight["invalidated_partitions"]
    if (
        not isinstance(compatible, list)
        or compatible != sorted(set(compatible))
        or not isinstance(inherited, list)
        or inherited != sorted(set(inherited))
        or not isinstance(invalidated_records, list)
    ):
        raise ToolchainError("structural-cache preflight partitions are malformed")
    invalidated_reasons: dict[str, list[dict[str, str]]] = {}
    for record in invalidated_records:
        if (
            not isinstance(record, dict)
            or set(record) != {"language", "reasons"}
            or not isinstance(record.get("language"), str)
            or not isinstance(record.get("reasons"), list)
            or not record["reasons"]
        ):
            raise ToolchainError("structural-cache invalidated partition is malformed")
        language = record["language"]
        if language in invalidated_reasons:
            raise ToolchainError("structural-cache invalidated partition is duplicated")
        for reason in record["reasons"]:
            if (
                not isinstance(reason, dict)
                or set(reason) != {"code", "base_digest", "target_digest"}
                or not all(
                    isinstance(value, str) and value for value in reason.values()
                )
            ):
                raise ToolchainError(
                    "structural-cache invalidation reason is malformed"
                )
        invalidated_reasons[language] = record["reasons"]
    target_languages = sorted(target_identity["partitions"])
    if sorted(set(compatible) | set(invalidated_reasons)) != target_languages:
        raise ToolchainError("structural-cache preflight does not cover target partitions")
    if not set(inherited).issubset(set(compatible)):
        raise ToolchainError("inherited partitions are not cache-compatible")
    selected = preflight["selected_base_cache"]
    if selected is None:
        cold_reason_codes = {
            "no_repository_cache_available",
            "cache_schema_version_mismatch",
            "root_slug_mismatch",
            "producer_identity_mismatch",
            "graph_schema_signature_mismatch",
            "toolchain_lock_digest_mismatch",
            "inventory_digest_mismatch",
            "inventory_file_sha256_mismatch",
            "inventory_policy_digest_mismatch",
            "scan_flags_mismatch",
        }
        if compatible or inherited or any(
            reason.get("code") not in cold_reason_codes
            for reasons in invalidated_reasons.values()
            for reason in reasons
            if isinstance(reason, dict)
        ):
            raise ToolchainError("cold structural-cache preflight is inconsistent")
    else:
        _validate_selected_partition_plan(
            {
                "invalidated_partitions": sorted(invalidated_reasons),
                "compatible_partitions": compatible,
                "invalidated_partition_reasons": invalidated_reasons,
            },
            target_identity,
        )
    counts = preflight["predicted_file_counts"]
    if not isinstance(counts, dict) or set(counts) != {"target", "inherited", "executed"}:
        raise ToolchainError("structural-cache preflight file counts are malformed")
    if any(type(value) is not int or value < 0 for value in counts.values()) or (
        counts["inherited"] + counts["executed"] != counts["target"]
    ):
        raise ToolchainError("structural-cache preflight file counts are inconsistent")
    impact_closure = preflight["impact_closure"]
    if not isinstance(impact_closure, dict) or set(impact_closure) != {
        "basis",
        "paths",
        "path_count",
        "paths_digest",
    }:
        raise ToolchainError("structural-cache preflight impact closure is malformed")
    impact_paths = impact_closure["paths"]
    if (
        not isinstance(impact_closure["basis"], str)
        or not impact_closure["basis"]
        or not isinstance(impact_paths, list)
        or impact_paths != sorted(set(impact_paths))
        or type(impact_closure["path_count"]) is not int
        or impact_closure["path_count"] != len(impact_paths)
        or sha256_bytes(canonical_json(impact_paths))
        != _require_sha256(
            impact_closure["paths_digest"],
            "structural-cache preflight impact closure digest",
        )
    ):
        raise ToolchainError("structural-cache preflight impact closure is invalid")
    for path in impact_paths:
        pure = PurePosixPath(path) if isinstance(path, str) else None
        if (
            pure is None
            or pure.is_absolute()
            or not pure.parts
            or any(component in {"", ".", ".."} for component in pure.parts)
        ):
            raise ToolchainError("structural-cache preflight impact path is invalid")
    if selected is None and impact_paths:
        raise ToolchainError("cold structural-cache preflight cannot carry impact paths")
    operations = preflight["expected_operation_count"]
    if not isinstance(operations, dict) or set(operations) != {
        "inherited_exact",
        "executed_estimate",
        "total_estimate",
        "max_executed",
        "authorized_operations_by_language",
        "basis",
        "estimated_file_count",
    }:
        raise ToolchainError("structural-cache preflight operation count is malformed")
    if (
        type(operations["inherited_exact"]) is not int
        or operations["inherited_exact"] < 0
        or type(operations["max_executed"]) is not int
        or operations["max_executed"] != STRUCTURAL_CACHE_MAX_EXECUTED_LSP_OPERATIONS
        or type(operations["estimated_file_count"]) is not int
        or operations["estimated_file_count"] < 0
        or not isinstance(operations["basis"], str)
        or not operations["basis"]
    ):
        raise ToolchainError("structural-cache preflight operation count is invalid")
    if selected is None:
        if (
            operations["executed_estimate"] is not None
            or operations["total_estimate"] is not None
            or operations["authorized_operations_by_language"] is not None
        ):
            raise ToolchainError("cold operation estimate must be explicitly unknown")
    elif (
        type(operations["executed_estimate"]) is not int
        or operations["executed_estimate"] < 0
        or operations["executed_estimate"] > operations["max_executed"]
        or not isinstance(operations["authorized_operations_by_language"], dict)
        or not set(operations["authorized_operations_by_language"]).issubset(
            target_identity["partitions"]
        )
        or any(
            not isinstance(language, str)
            or not isinstance(language_operations, list)
            or language_operations != sorted(set(language_operations))
            or not all(
                isinstance(operation, str) and operation
                for operation in language_operations
            )
            or not set(language_operations).issubset(STRUCTURAL_CACHE_LSP_OPERATIONS)
            for language, language_operations in operations[
                "authorized_operations_by_language"
            ].items()
        )
        or type(operations["total_estimate"]) is not int
        or operations["total_estimate"]
        != operations["inherited_exact"] + operations["executed_estimate"]
    ):
        raise ToolchainError("incremental operation estimate is inconsistent")


def publish_structural_cache_preflight(
    preflight: Mapping[str, Any], path: Path
) -> None:
    _publish_canonical_json_exclusive(path, preflight)
    print(canonical_json(preflight).decode(), flush=True)


def require_approved_structural_cache_preflight(
    actual: Mapping[str, Any], approved_path: Path | None
) -> None:
    if approved_path is None:
        return
    approved = load_json_object(approved_path, "approved structural-cache preflight")
    if approved != actual:
        raise ToolchainError(
            "approved structural-cache preflight differs from recomputed plan"
        )


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


def run_checked(
    args: Sequence[str],
    *,
    cwd: Path | None = None,
    environment: Mapping[str, str] | None = None,
) -> str:
    completed = subprocess.run(
        list(args),
        cwd=cwd,
        check=False,
        capture_output=True,
        text=True,
        env=dict(environment) if environment is not None else None,
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


def _require_https_url(value: Any, label: str) -> str:
    url = _require_string(value, label)
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme != "https"
        or not parsed.netloc
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise ToolchainError(f"{label} must be an exact credential-free HTTPS URL")
    return url


def _repo_contract_file(
    repo_root: Path, reference: Mapping[str, Any], label: str
) -> Path:
    _require_exact_fields(reference, {"path", "sha256"}, label)
    relative = _normalized_cache_path(
        _require_string(reference["path"], f"{label}.path"), label
    )
    expected_digest = _require_sha256(reference["sha256"], f"{label}.sha256")
    root = repo_root.resolve()
    if not root.is_dir():
        raise ToolchainError(f"repository source root is not a directory: {root}")
    candidate = root
    for component in PurePosixPath(relative).parts:
        candidate = candidate / component
        if candidate.is_symlink():
            raise ToolchainError(f"{label} traverses a symlink: {relative}")
    resolved = candidate.resolve()
    if root not in resolved.parents or not candidate.is_file():
        raise ToolchainError(f"{label} is missing or escapes repository root: {relative}")
    actual_digest = sha256_file(candidate)
    if actual_digest != expected_digest:
        raise ToolchainError(
            f"{label} source digest mismatch: expected={expected_digest} "
            f"actual={actual_digest}"
        )
    return candidate


def _validate_recipe_commands(value: Any, label: str) -> list[list[str]]:
    if (
        not isinstance(value, list)
        or not value
        or any(
            not isinstance(command, list)
            or not command
            or any(not isinstance(argument, str) or not argument for argument in command)
            for command in value
        )
    ):
        raise ToolchainError(f"{label} must contain nonempty argv arrays")
    return [list(command) for command in value]


def _load_acquisition_recipes(
    lock: Mapping[str, Any], repo_root: Path
) -> tuple[dict[str, dict[str, Any]], str]:
    acquisition = lock.get("acquisition")
    if not isinstance(acquisition, dict):
        raise ToolchainError("toolchain acquisition contract must be an object")
    contract_path = _repo_contract_file(
        repo_root, acquisition, "toolchain acquisition contract"
    )
    document = load_json_object(contract_path, "toolchain acquisition contract")
    _require_exact_fields(
        document, {"schema_version", "artifacts"}, "toolchain acquisition contract"
    )
    if document["schema_version"] != SCHEMA_VERSION:
        raise ToolchainError("toolchain acquisition contract schema mismatch")
    raw_recipes = document["artifacts"]
    if not isinstance(raw_recipes, list) or not raw_recipes:
        raise ToolchainError("toolchain acquisition recipes must be nonempty")
    recipes: dict[str, dict[str, Any]] = {}
    for index, raw_recipe in enumerate(raw_recipes):
        label = f"acquisition recipe {index}"
        if not isinstance(raw_recipe, dict):
            raise ToolchainError(f"{label} must be an object")
        recipe = dict(raw_recipe)
        artifact = _require_string(recipe.get("artifact"), f"{label}.artifact")
        _normalized_cache_path(artifact, f"{label}.artifact")
        if "/" in artifact:
            raise ToolchainError(f"{label}.artifact must be one path component")
        if artifact in recipes:
            raise ToolchainError(f"duplicate acquisition recipe: {artifact}")
        _require_sha256(recipe.get("artifact_sha256"), f"{label}.artifact_sha256")
        root_name = _require_string(recipe.get("root_name"), f"{label}.root_name")
        if "/" in root_name or root_name in {".", ".."}:
            raise ToolchainError(f"{label}.root_name must be one path component")
        kind = recipe.get("kind")
        if kind == "node-npm-ci":
            _require_exact_fields(
                recipe,
                {
                    "artifact",
                    "artifact_sha256",
                    "commands",
                    "kind",
                    "node_runtime_artifact",
                    "node_runtime_root",
                    "package_json",
                    "package_lock",
                    "root_name",
                },
                label,
            )
            _repo_contract_file(repo_root, recipe["package_json"], f"{label}.package_json")
            package_lock_path = _repo_contract_file(
                repo_root, recipe["package_lock"], f"{label}.package_lock"
            )
            package_lock = load_json_object(package_lock_path, f"{label} package lock")
            if package_lock.get("lockfileVersion") != 3:
                raise ToolchainError(f"{label} package lock must use lockfileVersion 3")
            commands = _validate_recipe_commands(recipe["commands"], f"{label}.commands")
            if any(command[0] != "npm" for command in commands):
                raise ToolchainError(f"{label} commands must use the pinned npm runtime")
            _require_string(
                recipe["node_runtime_artifact"], f"{label}.node_runtime_artifact"
            )
            _require_string(recipe["node_runtime_root"], f"{label}.node_runtime_root")
        elif kind == "python-wheelhouse":
            _require_exact_fields(
                recipe,
                {
                    "artifact",
                    "artifact_sha256",
                    "kind",
                    "manifest",
                    "root_name",
                },
                label,
            )
            manifest_path = _repo_contract_file(
                repo_root, recipe["manifest"], f"{label}.manifest"
            )
            manifest = load_json_object(manifest_path, f"{label} wheel manifest")
            _require_exact_fields(
                manifest,
                {"schema_version", "root_name", "wheels"},
                f"{label} wheel manifest",
            )
            if (
                manifest["schema_version"] != SCHEMA_VERSION
                or manifest["root_name"] != root_name
            ):
                raise ToolchainError(f"{label} wheel manifest identity mismatch")
            wheels = manifest["wheels"]
            if not isinstance(wheels, list) or not wheels:
                raise ToolchainError(f"{label} wheel manifest must be nonempty")
            filenames = []
            for wheel_index, wheel in enumerate(wheels):
                wheel_label = f"{label} wheel {wheel_index}"
                if not isinstance(wheel, dict):
                    raise ToolchainError(f"{wheel_label} must be an object")
                _require_exact_fields(
                    wheel, {"filename", "sha256", "url"}, wheel_label
                )
                filename = _normalized_cache_path(
                    _require_string(wheel["filename"], f"{wheel_label}.filename"),
                    wheel_label,
                )
                if "/" in filename or not filename.endswith(".whl"):
                    raise ToolchainError(f"{wheel_label} filename is invalid")
                filenames.append(filename)
                _require_sha256(wheel["sha256"], f"{wheel_label}.sha256")
                _require_https_url(wheel["url"], f"{wheel_label}.url")
            if filenames != sorted(set(filenames)):
                raise ToolchainError(f"{label} wheel filenames must be sorted and unique")
        elif kind == "cyright-source":
            _require_exact_fields(
                recipe,
                {
                    "artifact",
                    "artifact_sha256",
                    "commands",
                    "kind",
                    "node_runtime_artifact",
                    "node_runtime_root",
                    "output_path",
                    "patches",
                    "root_name",
                    "source_root",
                    "source_sha256",
                    "source_url",
                },
                label,
            )
            _require_https_url(recipe["source_url"], f"{label}.source_url")
            _require_sha256(recipe["source_sha256"], f"{label}.source_sha256")
            _normalized_cache_path(recipe["source_root"], f"{label}.source_root")
            _normalized_cache_path(recipe["output_path"], f"{label}.output_path")
            _require_string(
                recipe["node_runtime_artifact"], f"{label}.node_runtime_artifact"
            )
            _require_string(recipe["node_runtime_root"], f"{label}.node_runtime_root")
            commands = _validate_recipe_commands(recipe["commands"], f"{label}.commands")
            if any(command[0] != "npm" for command in commands):
                raise ToolchainError(f"{label} commands must use the pinned npm runtime")
            patches = recipe["patches"]
            if not isinstance(patches, list):
                raise ToolchainError(f"{label}.patches must be an array")
            for patch_index, patch_reference in enumerate(patches):
                if not isinstance(patch_reference, dict):
                    raise ToolchainError(f"{label} patch {patch_index} must be an object")
                _repo_contract_file(
                    repo_root, patch_reference, f"{label} patch {patch_index}"
                )
        elif kind == "repo-sources":
            _require_exact_fields(
                recipe,
                {
                    "artifact",
                    "artifact_sha256",
                    "kind",
                    "root_name",
                    "sources",
                },
                label,
            )
            sources = recipe["sources"]
            if not isinstance(sources, list) or not sources:
                raise ToolchainError(f"{label}.sources must be nonempty")
            destinations = []
            for source_index, source in enumerate(sources):
                source_label = f"{label} source {source_index}"
                if not isinstance(source, dict):
                    raise ToolchainError(f"{source_label} must be an object")
                _require_exact_fields(
                    source, {"destination", "path", "sha256"}, source_label
                )
                _repo_contract_file(
                    repo_root,
                    {"path": source["path"], "sha256": source["sha256"]},
                    source_label,
                )
                destination = _normalized_cache_path(
                    _require_string(
                        source["destination"], f"{source_label}.destination"
                    ),
                    source_label,
                )
                if "/" in destination:
                    raise ToolchainError(f"{source_label}.destination must be one component")
                destinations.append(destination)
            if len(destinations) != len(set(destinations)):
                raise ToolchainError(
                    f"{label} source destinations must be unique"
                )
        else:
            raise ToolchainError(f"{label}.kind is unsupported: {kind}")
        recipes[artifact] = recipe
    return recipes, sha256_file(contract_path)


def _artifact_acquisition_groups(
    lock: Mapping[str, Any], recipes: Mapping[str, Mapping[str, Any]]
) -> dict[str, dict[str, Any]]:
    groups: dict[str, dict[str, Any]] = {}
    entries = [*lock.get("runtimes", []), *lock.get("servers", [])]
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise ToolchainError(f"toolchain artifact entry {index} must be an object")
        artifact = _require_string(entry.get("artifact"), f"artifact entry {index}.artifact")
        _normalized_cache_path(artifact, f"artifact entry {index}.artifact")
        if "/" in artifact:
            raise ToolchainError(f"artifact entry {index}.artifact must be one component")
        digest = _require_sha256(
            entry.get("artifact_sha256"), f"artifact entry {index}.artifact_sha256"
        )
        source_url = _require_https_url(
            entry.get("source_url"), f"artifact entry {index}.source_url"
        )
        group = groups.setdefault(
            artifact, {"artifact_sha256": digest, "source_urls": set()}
        )
        if group["artifact_sha256"] != digest:
            raise ToolchainError(f"artifact digest disagreement: {artifact}")
        group["source_urls"].add(source_url)
    for artifact, recipe in recipes.items():
        group = groups.get(artifact)
        if group is None:
            raise ToolchainError(f"acquisition recipe has no toolchain artifact: {artifact}")
        if recipe["artifact_sha256"] != group["artifact_sha256"]:
            raise ToolchainError(f"acquisition recipe digest mismatch: {artifact}")
    for artifact, group in groups.items():
        if artifact not in recipes and len(group["source_urls"]) != 1:
            raise ToolchainError(
                f"aggregate artifact lacks deterministic recipe: {artifact}"
            )
    for artifact, recipe in recipes.items():
        runtime_artifact = recipe.get("node_runtime_artifact")
        if runtime_artifact is not None:
            if runtime_artifact not in groups or runtime_artifact in recipes:
                raise ToolchainError(
                    f"{artifact} must reference a directly downloadable node runtime"
                )
    return groups


def verify_lock(
    lock_path: Path,
    inventory_path: Path,
    cache_root: Path | None,
    descriptor_path: Path | None,
    repo_root: Path | None = None,
) -> dict[str, Any]:
    lock = load_json_object(lock_path, "toolchain lock")
    inventory = load_json_object(inventory_path, "inventory")
    source_root = (repo_root or Path.cwd()).resolve()
    required_lock_keys = {
        "acquisition",
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
        _repo_contract_file(
            source_root, source, f"repo_parser_bundle source {index}"
        )
    acquisition_recipes, acquisition_contract_sha256 = (
        _load_acquisition_recipes(lock, source_root)
    )
    parser_recipe = acquisition_recipes.get(repo_parser_bundle["artifact"])
    if (
        parser_recipe is None
        or parser_recipe.get("kind") != "repo-sources"
        or parser_recipe.get("artifact_sha256")
        != repo_parser_bundle["artifact_sha256"]
        or [
            {"path": source["path"], "sha256": source["sha256"]}
            for source in parser_recipe["sources"]
        ]
        != sources
    ):
        raise ToolchainError(
            "repo_parser_bundle does not match its deterministic acquisition recipe"
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
    acquisition_groups = _artifact_acquisition_groups(lock, acquisition_recipes)
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
        "repository_sources_verified": True,
        "acquisition_contract_sha256": acquisition_contract_sha256,
        "acquisition_recipe_count": len(acquisition_recipes),
        "acquisition_artifact_count": len(acquisition_groups),
    }


def _download_https_artifact(url: str, digest: str, target: Path) -> None:
    _require_https_url(url, "artifact source URL")
    _require_sha256(digest, "artifact source digest")
    target.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(
        url, headers={"User-Agent": "rna-swebench-lsp-toolchain/1"}
    )
    with urllib.request.urlopen(request, timeout=120) as response, target.open("wb") as sink:  # noqa: S310
        final_url = response.geturl()
        _require_https_url(final_url, "artifact redirect URL")
        shutil.copyfileobj(response, sink)
    actual_digest = sha256_file(target)
    if actual_digest != digest:
        raise ToolchainError(
            f"download digest mismatch: expected={digest} actual={actual_digest}"
        )


def _node_recipe_environment(node_bin: Path, npm_cache: Path) -> dict[str, str]:
    environment = dict(os.environ)
    environment["PATH"] = os.pathsep.join(
        [str(node_bin), "/usr/bin", "/bin"]
    )
    environment["npm_config_cache"] = str(npm_cache)
    environment["npm_config_audit"] = "false"
    environment["npm_config_fund"] = "false"
    environment["npm_config_update_notifier"] = "false"
    return environment


def _run_node_recipe_commands(
    recipe: Mapping[str, Any],
    cache_root: Path,
    working_directory: Path,
    staging_root: Path,
) -> None:
    runtime_artifact = cache_root / recipe["node_runtime_artifact"]
    if not runtime_artifact.is_file():
        raise ToolchainError(
            f"node recipe runtime is missing: {recipe['node_runtime_artifact']}"
        )
    runtime_stage = staging_root / "node-runtime"
    _extract_tar(runtime_artifact, runtime_stage)
    node_bin = runtime_stage / recipe["node_runtime_root"] / "bin"
    npm = node_bin / "npm"
    node = node_bin / "node"
    if not npm.is_file() or not node.is_file():
        raise ToolchainError("pinned node recipe runtime lacks node/npm")
    environment = _node_recipe_environment(node_bin, staging_root / "npm-cache")
    for command in recipe["commands"]:
        completed = subprocess.run(
            [str(npm), *command[1:]],
            cwd=working_directory,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise ToolchainError(
                f"node acquisition recipe failed ({' '.join(command)}): "
                f"stdout={completed.stdout.strip()[-4000:]} "
                f"stderr={completed.stderr.strip()[-4000:]}"
            )


def _build_repo_sources_recipe(
    recipe: Mapping[str, Any], repo_root: Path, output: Path
) -> int:
    with tempfile.TemporaryDirectory(prefix="rna-lsp-repo-recipe-") as temporary:
        source_root = Path(temporary) / recipe["root_name"]
        source_root.mkdir()
        for index, source in enumerate(recipe["sources"]):
            source_path = _repo_contract_file(
                repo_root,
                {"path": source["path"], "sha256": source["sha256"]},
                f"repo acquisition source {index}",
            )
            destination = source_root / source["destination"]
            shutil.copyfile(source_path, destination)
            destination.chmod(0o755)
        seal_directory(source_root, output, recipe["root_name"])
    return len(recipe["sources"])


def _build_wheelhouse_recipe(
    recipe: Mapping[str, Any], repo_root: Path, output: Path
) -> int:
    manifest_path = _repo_contract_file(
        repo_root, recipe["manifest"], "wheelhouse acquisition manifest"
    )
    manifest = load_json_object(manifest_path, "wheelhouse acquisition manifest")
    with tempfile.TemporaryDirectory(prefix="rna-lsp-wheel-recipe-") as temporary:
        wheelhouse = Path(temporary) / recipe["root_name"]
        wheelhouse.mkdir()
        for wheel in manifest["wheels"]:
            _download_https_artifact(
                wheel["url"], wheel["sha256"], wheelhouse / wheel["filename"]
            )
        seal_directory(wheelhouse, output, recipe["root_name"])
    return len(manifest["wheels"])


def _build_node_bundle_recipe(
    recipe: Mapping[str, Any], repo_root: Path, cache_root: Path, output: Path
) -> int:
    package_json = _repo_contract_file(
        repo_root, recipe["package_json"], "node recipe package.json"
    )
    package_lock = _repo_contract_file(
        repo_root, recipe["package_lock"], "node recipe package-lock.json"
    )
    with tempfile.TemporaryDirectory(prefix="rna-lsp-node-recipe-") as temporary:
        staging_root = Path(temporary)
        bundle_root = staging_root / recipe["root_name"]
        bundle_root.mkdir()
        shutil.copyfile(package_json, bundle_root / "package.json")
        shutil.copyfile(package_lock, bundle_root / "package-lock.json")
        _run_node_recipe_commands(recipe, cache_root, bundle_root, staging_root)
        seal_directory(bundle_root, output, recipe["root_name"])
    return 1


def _build_cyright_recipe(
    recipe: Mapping[str, Any], repo_root: Path, cache_root: Path, output: Path
) -> int:
    with tempfile.TemporaryDirectory(prefix="rna-lsp-cyright-recipe-") as temporary:
        staging_root = Path(temporary)
        source_archive = staging_root / "cyright-source.tar.gz"
        _download_https_artifact(
            recipe["source_url"], recipe["source_sha256"], source_archive
        )
        extracted = staging_root / "source"
        _extract_tar(source_archive, extracted)
        source_root = extracted / recipe["source_root"]
        if not source_root.is_dir() or source_root.is_symlink():
            raise ToolchainError("Cyright source archive root is missing")
        for index, patch_reference in enumerate(recipe["patches"]):
            patch_path = _repo_contract_file(
                repo_root, patch_reference, f"Cyright acquisition patch {index}"
            )
            completed = subprocess.run(
                ["git", "apply", "--whitespace=nowarn", str(patch_path)],
                cwd=source_root,
                check=False,
                capture_output=True,
                text=True,
            )
            if completed.returncode != 0:
                detail = completed.stderr.strip() or completed.stdout.strip()
                raise ToolchainError(f"Cyright acquisition patch failed: {detail[-4000:]}")
        _run_node_recipe_commands(recipe, cache_root, source_root, staging_root)
        built_output = source_root / recipe["output_path"]
        if not built_output.is_dir() or built_output.is_symlink():
            raise ToolchainError("Cyright acquisition output is missing")
        bundle_root = staging_root / recipe["root_name"]
        bundle_root.mkdir()
        shutil.copytree(built_output, bundle_root / "dist", symlinks=True)
        seal_directory(bundle_root, output, recipe["root_name"])
    return 1


def _build_acquisition_recipe(
    recipe: Mapping[str, Any], repo_root: Path, cache_root: Path, output: Path
) -> int:
    kind = recipe["kind"]
    if kind == "repo-sources":
        return _build_repo_sources_recipe(recipe, repo_root, output)
    if kind == "python-wheelhouse":
        return _build_wheelhouse_recipe(recipe, repo_root, output)
    if kind == "node-npm-ci":
        return _build_node_bundle_recipe(recipe, repo_root, cache_root, output)
    if kind == "cyright-source":
        return _build_cyright_recipe(recipe, repo_root, cache_root, output)
    raise ToolchainError(f"unsupported acquisition recipe: {kind}")


def acquire_artifacts(
    lock_path: Path,
    cache_root: Path,
    repo_root: Path | None = None,
) -> dict[str, Any]:
    lock = load_json_object(lock_path, "toolchain lock")
    source_root = (repo_root or Path.cwd()).resolve()
    recipes, contract_sha256 = _load_acquisition_recipes(lock, source_root)
    groups = _artifact_acquisition_groups(lock, recipes)
    cache_root.mkdir(parents=True, exist_ok=True)
    downloaded = 0
    built = 0
    component_downloads = 0

    # Direct artifacts are acquired first because deterministic build recipes
    # may depend on the pinned Node runtime among them.
    for artifact, group in sorted(groups.items()):
        if artifact in recipes:
            continue
        target = cache_root / artifact
        if target.is_file() and sha256_file(target) == group["artifact_sha256"]:
            continue
        temporary = target.with_name(f".{target.name}.download-{os.getpid()}")
        try:
            source_url = next(iter(group["source_urls"]))
            _download_https_artifact(
                source_url, group["artifact_sha256"], temporary
            )
            temporary.replace(target)
            downloaded += 1
            component_downloads += 1
        finally:
            temporary.unlink(missing_ok=True)

    for artifact, recipe in sorted(recipes.items()):
        target = cache_root / artifact
        if target.is_file() and sha256_file(target) == recipe["artifact_sha256"]:
            continue
        temporary = target.with_name(f".{target.name}.build-{os.getpid()}")
        try:
            component_downloads += _build_acquisition_recipe(
                recipe, source_root, cache_root, temporary
            )
            actual_digest = sha256_file(temporary)
            if actual_digest != recipe["artifact_sha256"]:
                raise ToolchainError(
                    f"built artifact digest mismatch: {artifact} "
                    f"expected={recipe['artifact_sha256']} actual={actual_digest}"
                )
            temporary.replace(target)
            built += 1
        finally:
            temporary.unlink(missing_ok=True)
    return {
        "schema_version": SCHEMA_VERSION,
        "acquisition_contract_sha256": contract_sha256,
        "artifact_count": len(groups),
        "downloaded": downloaded,
        "built": built,
        "component_downloads": component_downloads,
    }


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


def _extract_verified_lsp_artifact_cache(
    archive_path: Path, destination: Path
) -> Path:
    """Extract the CI-bundled artifact cache without trusting tar metadata."""
    if archive_path.is_symlink() or not archive_path.is_file():
        raise ToolchainError("verified LSP artifact cache must be a regular file")
    if destination.exists() or destination.is_symlink():
        raise ToolchainError("verified LSP artifact-cache destination must be absent")
    archive_sha256 = sha256_file(archive_path)
    destination.mkdir(parents=True)
    seen: set[str] = set()
    seen_casefold: set[str] = set()
    total_size = 0
    directory_modes: list[tuple[Path, int]] = []
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            members = archive.getmembers()
            if not members or len(members) > STRUCTURAL_CACHE_MAX_MEMBERS:
                raise ToolchainError("verified LSP artifact cache entry count is invalid")
            for member in members:
                name = member.name
                pure = PurePosixPath(name)
                if (
                    "\\" in name
                    or "\0" in name
                    or pure.is_absolute()
                    or not pure.parts
                    or pure.parts[0] != "lsp-artifact-cache"
                    or any(part in {"", ".", ".."} for part in pure.parts)
                ):
                    raise ToolchainError(
                        f"verified LSP artifact cache member is unsafe: {name}"
                    )
                normalized = pure.as_posix()
                folded = normalized.casefold()
                if normalized in seen or folded in seen_casefold:
                    raise ToolchainError(
                        f"verified LSP artifact cache member is duplicated: {name}"
                    )
                seen.add(normalized)
                seen_casefold.add(folded)
                if not (member.isdir() or member.isfile()):
                    raise ToolchainError(
                        f"verified LSP artifact cache contains a link or special file: {name}"
                    )
                if getattr(member, "sparse", None):
                    raise ToolchainError(
                        f"verified LSP artifact cache contains a sparse member: {name}"
                    )
                if member.size < 0 or member.size > STRUCTURAL_CACHE_MAX_MEMBER_BYTES:
                    raise ToolchainError(
                        f"verified LSP artifact cache member size is invalid: {name}"
                    )
                total_size += member.size
                if total_size > STRUCTURAL_CACHE_MAX_TOTAL_BYTES:
                    raise ToolchainError("verified LSP artifact cache is oversized")

            for member in members:
                target = destination.joinpath(*PurePosixPath(member.name).parts)
                target.parent.mkdir(parents=True, exist_ok=True)
                if member.isdir():
                    target.mkdir(mode=0o755, parents=True, exist_ok=True)
                    directory_modes.append((target, 0o755))
                    continue
                source = archive.extractfile(member)
                if source is None:
                    raise ToolchainError(
                        f"verified LSP artifact cache member is partial: {member.name}"
                    )
                written = 0
                with target.open("xb") as output:
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        output.write(chunk)
                        written += len(chunk)
                if written != member.size:
                    raise ToolchainError(
                        f"verified LSP artifact cache member is truncated: {member.name}"
                    )
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
        for directory, mode in sorted(
            directory_modes, key=lambda item: len(item[0].parts), reverse=True
        ):
            directory.chmod(mode)
    except Exception:
        shutil.rmtree(destination, ignore_errors=True)
        raise
    if sha256_file(archive_path) != archive_sha256:
        raise ToolchainError("verified LSP artifact cache changed during extraction")
    cache_root = destination / "lsp-artifact-cache"
    if cache_root.is_symlink() or not cache_root.is_dir():
        raise ToolchainError("verified LSP artifact cache root is missing")
    return cache_root


def _materialize_verified_bundle_toolchain(
    bundle_root: Path,
    lock_path: Path,
    inventory_path: Path,
    private_root: Path,
    repo_root: Path | None,
) -> dict[str, Any]:
    """Offline-provision a private LSP root from the verifier-clean CI bundle."""
    if private_root.exists() or private_root.is_symlink():
        raise ToolchainError("private verified LSP materialization root must be absent")
    private_root.mkdir(parents=True)
    component_root = bundle_root / "components/lsp"
    artifact_cache_archive = component_root / "artifact-cache.tar.gz"
    cache_root = _extract_verified_lsp_artifact_cache(
        artifact_cache_archive, private_root / "artifact-cache"
    )
    toolchain_root = private_root / "provisioned"
    receipt = provision_toolchain(
        lock_path,
        inventory_path,
        cache_root,
        toolchain_root,
        toolchain_root / PROVISION_RECEIPT_FILE,
        offline=True,
        repo_root=repo_root,
    )
    ci_receipt_path = component_root / "provision-receipt.json"
    if ci_receipt_path.is_symlink() or not ci_receipt_path.is_file():
        raise ToolchainError("verified bundle CI provision receipt is missing")
    if load_json_object(ci_receipt_path, "verified bundle CI provision receipt") != receipt:
        raise ToolchainError(
            "private offline provision receipt differs from the CI-qualified toolchain"
        )
    identity = _validate_provisioned_toolchain(
        lock_path, inventory_path, toolchain_root
    )
    if identity["provision_receipt_digest"] != receipt["receipt_digest"]:
        raise ToolchainError("private verified LSP provision identity drifted")
    return identity


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


def toolchain_environment(
    toolchain_root: Path, isolation_root: Path
) -> dict[str, str]:
    toolchain_root = toolchain_root.resolve()
    isolation_root = isolation_root.resolve()
    home = isolation_root / "home"
    temporary = isolation_root / "tmp"
    xdg_config = isolation_root / "xdg-config"
    xdg_cache = isolation_root / "xdg-cache"
    xdg_data = isolation_root / "xdg-data"
    xdg_state = isolation_root / "xdg-state"
    pip_cache = isolation_root / "pip-cache"
    npm_cache = isolation_root / "npm-cache"
    for directory in (
        home,
        temporary,
        xdg_config,
        xdg_cache,
        xdg_data,
        xdg_state,
        pip_cache,
        npm_cache,
    ):
        directory.mkdir(parents=True, exist_ok=True)
    paths = [
        toolchain_root / "bin",
        toolchain_root / "runtimes/node-v22.12.0-darwin-arm64/bin",
        toolchain_root / "runtimes/python/bin",
        toolchain_root / "runtimes/jdk-21.0.11+10-jre/Contents/Home/bin",
    ]
    return {
        "PATH": os.pathsep.join(str(path) for path in paths),
        "HOME": str(home),
        "TMPDIR": str(temporary),
        "XDG_CONFIG_HOME": str(xdg_config),
        "XDG_CACHE_HOME": str(xdg_cache),
        "XDG_DATA_HOME": str(xdg_data),
        "XDG_STATE_HOME": str(xdg_state),
        "LANG": "C",
        "LC_ALL": "C",
        "TZ": "UTC",
        "PYTHONNOUSERSITE": "1",
        "PYTHONSAFEPATH": "1",
        "PYTHONUTF8": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PIP_NO_INDEX": "1",
        "PIP_CONFIG_FILE": os.devnull,
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PIP_CACHE_DIR": str(pip_cache),
        "npm_config_offline": "true",
        "npm_config_userconfig": os.devnull,
        "npm_config_cache": str(npm_cache),
        "JAVA_HOME": str(
            toolchain_root / "runtimes/jdk-21.0.11+10-jre/Contents/Home"
        ),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "NO_PROXY": "*",
        "no_proxy": "*",
    }


def bind_hf_default_cache(hf_home: Path, isolated_home: Path) -> dict[str, Any]:
    """Expose an HF_HOME cache at hf-hub's actual default cache path."""
    if hf_home.is_symlink() or not hf_home.is_dir():
        raise ToolchainError("HF_HOME must be a real directory")
    source_hub = hf_home / "hub"
    if source_hub.is_symlink() or not source_hub.is_dir():
        raise ToolchainError("HF_HOME hub must be a real directory")
    if isolated_home.is_symlink() or not isolated_home.is_dir():
        raise ToolchainError("isolated HOME must be a real directory")

    source_hub = source_hub.resolve(strict=True)
    isolated_home = isolated_home.resolve(strict=True)
    if (
        source_hub == isolated_home
        or source_hub in isolated_home.parents
        or isolated_home in source_hub.parents
    ):
        raise ToolchainError("HF_HOME hub and isolated HOME must be disjoint")

    cache_parent = isolated_home / ".cache"
    huggingface_parent = cache_parent / "huggingface"
    for label, directory in (
        ("isolated HOME cache", cache_parent),
        ("isolated HOME HuggingFace cache", huggingface_parent),
    ):
        if directory.is_symlink() or (directory.exists() and not directory.is_dir()):
            raise ToolchainError(f"{label} must be a real directory")
        directory.mkdir(exist_ok=True)

    default_hub = huggingface_parent / "hub"
    if default_hub.exists() or default_hub.is_symlink():
        raise ToolchainError("default HuggingFace cache destination must be absent")
    try:
        default_hub.symlink_to(source_hub, target_is_directory=True)
        if default_hub.resolve(strict=True) != source_hub:
            raise ToolchainError("default HuggingFace cache binding target drifted")
    except Exception:
        if default_hub.is_symlink():
            default_hub.unlink()
        raise
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "bound",
        "binding": "symlink",
        "default_cache_relative_path": ".cache/huggingface/hub",
        "hf_home_relative_path": "hub",
    }


def provision_toolchain(
    lock_path: Path,
    inventory_path: Path,
    cache_root: Path,
    toolchain_root: Path,
    receipt_path: Path,
    *,
    offline: bool,
    repo_root: Path | None = None,
) -> dict[str, Any]:
    if not offline:
        raise ToolchainError("provision requires --offline")
    verification = verify_lock(
        lock_path, inventory_path, cache_root, None, repo_root
    )
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
        provision_environment = toolchain_environment(
            toolchain_root, toolchain_root / ".provision-environment"
        )
        run_checked(
            [str(python_runtime), "-m", "venv", str(python_env)],
            environment=provision_environment,
        )
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
            env=provision_environment,
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
    launcher_entries = []
    for entry in lock["servers"]:
        launcher_path = _safe_destination(
            toolchain_root, f"bin/{entry['command']}"
        )
        launcher_target_path = _safe_destination(
            toolchain_root, entry["launcher"]["target"]
        )
        if not launcher_path.is_file():
            raise ToolchainError(
                f"installed launcher is missing: {entry['command']}"
            )
        if not launcher_target_path.is_file():
            raise ToolchainError(
                f"installed launcher target is missing: {entry['name']}"
            )
        try:
            launcher_path.resolve(strict=True).relative_to(toolchain_root)
            launcher_target_path.resolve(strict=True).relative_to(toolchain_root)
        except (OSError, ValueError) as error:
            raise ToolchainError(
                f"installed launcher or target escapes toolchain root: "
                f"{entry['command']}"
            ) from error
        launcher_entries.append(
            {
                "name": entry["name"],
                "command": entry["command"],
                "path": f"bin/{entry['command']}",
                "sha256": sha256_file(launcher_path),
                "target_path": entry["launcher"]["target"],
                "target_sha256": sha256_file(launcher_target_path),
            }
        )
    receipt = {
        "schema_version": SCHEMA_VERSION,
        "offline": True,
        "platform": lock["platform"],
        "lock_sha256": sha256_file(lock_path),
        "inventory_sha256": sha256_file(inventory_path),
        "installed": sorted(installed_entries, key=lambda item: item["name"]),
        "launchers": sorted(
            launcher_entries, key=lambda item: (item["name"], item["command"])
        ),
    }
    receipt["receipt_digest"] = sha256_bytes(canonical_json(receipt))
    embedded_receipt_path = toolchain_root / PROVISION_RECEIPT_FILE
    write_canonical_json(embedded_receipt_path, receipt)
    if receipt_path.resolve() == embedded_receipt_path.resolve():
        return receipt
    write_canonical_json(receipt_path, receipt)
    return receipt


def _validate_provisioned_toolchain(
    lock_path: Path,
    inventory_path: Path,
    toolchain_root: Path,
) -> dict[str, Any]:
    if toolchain_root.is_symlink() or not toolchain_root.is_dir():
        raise ToolchainError("provisioned toolchain root must be a real directory")
    toolchain_root = toolchain_root.resolve()
    receipt_path = toolchain_root / PROVISION_RECEIPT_FILE
    if receipt_path.is_symlink() or not receipt_path.is_file():
        raise ToolchainError("embedded provision receipt must be a regular file")
    receipt = load_json_object(receipt_path, "embedded provision receipt")
    _require_exact_fields(
        receipt,
        {
            "schema_version",
            "offline",
            "platform",
            "lock_sha256",
            "inventory_sha256",
            "installed",
            "launchers",
            "receipt_digest",
        },
        "embedded provision receipt",
    )
    receipt_digest = _require_sha256(
        receipt.get("receipt_digest"), "embedded provision receipt digest"
    )
    digest_payload = dict(receipt)
    digest_payload.pop("receipt_digest")
    if sha256_bytes(canonical_json(digest_payload)) != receipt_digest:
        raise ToolchainError("embedded provision receipt self-digest mismatch")

    lock = load_json_object(lock_path, "toolchain lock for provision validation")
    expected_entries = sorted(
        [*lock.get("runtimes", []), *lock.get("servers", [])],
        key=lambda entry: (
            _require_string(entry.get("name"), "locked executable name"),
            _require_string(entry.get("executable"), "locked executable path"),
        ),
    )
    installed = receipt.get("installed")
    launchers = receipt.get("launchers")
    if (
        receipt.get("schema_version") != SCHEMA_VERSION
        or receipt.get("offline") is not True
        or receipt.get("platform") != lock.get("platform")
        or receipt.get("lock_sha256") != sha256_file(lock_path)
        or receipt.get("inventory_sha256") != sha256_file(inventory_path)
        or not isinstance(installed, list)
        or len(installed) != len(expected_entries)
        or not isinstance(launchers, list)
        or len(launchers) != len(lock.get("servers", []))
    ):
        raise ToolchainError("embedded provision receipt identity mismatch")

    verified_installed = []
    ordered_installed = sorted(
        installed,
        key=lambda record: (
            _require_string(
                record.get("name") if isinstance(record, dict) else None,
                "provisioned executable name",
            ),
            _require_string(
                record.get("executable") if isinstance(record, dict) else None,
                "provisioned executable path",
            ),
        ),
    )
    for record, entry in zip(ordered_installed, expected_entries, strict=True):
        if not isinstance(record, dict):
            raise ToolchainError("provisioned executable receipt is malformed")
        _require_exact_fields(
            record,
            {"name", "executable", "sha256"},
            "provisioned executable receipt",
        )
        expected_digest = _require_sha256(
            entry.get("executable_sha256"), "locked executable digest"
        )
        if (
            record.get("name") != entry.get("name")
            or record.get("executable") != entry.get("executable")
            or record.get("sha256") != expected_digest
        ):
            raise ToolchainError(
                f"provisioned executable identity mismatch: {entry.get('name')}"
            )
        executable = _safe_destination(toolchain_root, entry["executable"])
        if not executable.is_file():
            raise ToolchainError(
                f"provisioned executable is missing: {entry.get('name')}"
            )
        actual_digest = sha256_file(executable)
        if actual_digest != expected_digest:
            raise ToolchainError(
                f"provisioned executable digest mismatch: {entry.get('name')} "
                f"expected={expected_digest} actual={actual_digest}"
            )
        verified_installed.append(
            {
                "name": entry["name"],
                "executable": entry["executable"],
                "sha256": actual_digest,
            }
        )
    expected_servers = sorted(
        lock.get("servers", []),
        key=lambda entry: (
            _require_string(entry.get("name"), "locked launcher server name"),
            _require_string(entry.get("command"), "locked launcher command"),
        ),
    )
    verified_launchers = []
    ordered_launchers = sorted(
        launchers,
        key=lambda record: (
            _require_string(
                record.get("name") if isinstance(record, dict) else None,
                "provisioned launcher server name",
            ),
            _require_string(
                record.get("command") if isinstance(record, dict) else None,
                "provisioned launcher command",
            ),
        ),
    )
    for record, entry in zip(ordered_launchers, expected_servers, strict=True):
        if not isinstance(record, dict):
            raise ToolchainError("provisioned launcher receipt is malformed")
        _require_exact_fields(
            record,
            {
                "name",
                "command",
                "path",
                "sha256",
                "target_path",
                "target_sha256",
            },
            "provisioned launcher receipt",
        )
        expected_path = f"bin/{entry['command']}"
        expected_target_path = _require_string(
            entry.get("launcher", {}).get("target")
            if isinstance(entry.get("launcher"), dict)
            else None,
            "locked launcher target",
        )
        launcher = _safe_destination(toolchain_root, expected_path)
        launcher_target = _safe_destination(toolchain_root, expected_target_path)
        if (
            record.get("name") != entry.get("name")
            or record.get("command") != entry.get("command")
            or record.get("path") != expected_path
            or record.get("target_path") != expected_target_path
            or not launcher.is_file()
            or not launcher_target.is_file()
        ):
            raise ToolchainError(
                f"provisioned launcher identity mismatch: {entry.get('name')}"
            )
        try:
            launcher.resolve(strict=True).relative_to(toolchain_root)
            launcher_target.resolve(strict=True).relative_to(toolchain_root)
        except (OSError, ValueError) as error:
            raise ToolchainError(
                f"provisioned launcher or target escapes toolchain root: "
                f"{entry.get('name')}"
            ) from error
        actual_digest = sha256_file(launcher)
        actual_target_digest = sha256_file(launcher_target)
        if record.get("sha256") != actual_digest:
            raise ToolchainError(
                f"provisioned launcher digest mismatch: {entry.get('name')}"
            )
        if record.get("target_sha256") != actual_target_digest:
            raise ToolchainError(
                f"provisioned launcher target digest mismatch: {entry.get('name')}"
            )
        verified_launchers.append(
            {
                "name": entry["name"],
                "command": entry["command"],
                "path": expected_path,
                "sha256": actual_digest,
                "target_path": expected_target_path,
                "target_sha256": actual_target_digest,
            }
        )
    return {
        "toolchain_root": str(toolchain_root),
        "inventory_sha256": sha256_file(inventory_path),
        "provision_receipt_digest": receipt_digest,
        "provision_receipt_sha256": sha256_file(receipt_path),
        "installed": verified_installed,
        "launchers": verified_launchers,
    }


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
        self.workspace_folders: list[dict[str, Any]] = []

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
        payload_parts: list[bytes] = []
        remaining_length = length
        while remaining_length:
            remaining_time = deadline - time.monotonic()
            if remaining_time <= 0 or not select.select(
                [self.process.stdout], [], [], remaining_time
            )[0]:
                raise ToolchainError("language server response timed out")
            chunk = os.read(
                self.process.stdout.fileno(), min(remaining_length, 64 * 1024)
            )
            if not chunk:
                raise ToolchainError("language server emitted truncated JSON-RPC")
            payload_parts.append(chunk)
            remaining_length -= len(chunk)
        payload = b"".join(payload_parts)
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
        if method == "initialize" and isinstance(params, Mapping):
            workspace_folders = params.get("workspaceFolders")
            self.workspace_folders = (
                [dict(folder) for folder in workspace_folders]
                if isinstance(workspace_folders, list)
                and all(isinstance(folder, dict) for folder in workspace_folders)
                else []
            )
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
            if not isinstance(items, list):
                return []
            results = []
            for item in items:
                if not isinstance(item, dict) or "section" not in item:
                    results.append(self.configuration)
                    continue
                section = item.get("section")
                if not isinstance(section, str) or not section:
                    results.append(None)
                    continue
                value: Any = self.configuration
                for component in section.split("."):
                    if not isinstance(value, Mapping) or component not in value:
                        value = None
                        break
                    value = value[component]
                results.append(value)
            return results
        if message.get("method") == "workspace/workspaceFolders":
            return self.workspace_folders
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


OPERATION_CAPABILITY_PROVIDERS = (
    ("documentSymbolProvider", ("textDocument/documentSymbol",)),
    ("definitionProvider", ("textDocument/definition",)),
    ("referencesProvider", ("textDocument/references",)),
    (
        "callHierarchyProvider",
        (
            "textDocument/prepareCallHierarchy",
            "callHierarchy/incomingCalls",
            "callHierarchy/outgoingCalls",
        ),
    ),
    ("codeActionProvider", ("textDocument/codeAction",)),
)


def _probe_client_capabilities() -> dict[str, Any]:
    return {
        "workspace": {
            "configuration": True,
            "workspaceFolders": True,
        },
        "textDocument": {
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
            "definition": {"dynamicRegistration": False},
            "references": {"dynamicRegistration": False},
            "callHierarchy": {"dynamicRegistration": False},
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {"valueSet": ["quickfix"]}
                }
            },
        },
    }


def _operation_capability_evidence(
    capabilities: Mapping[str, Any],
) -> list[dict[str, Any]]:
    evidence = []
    for provider, methods in OPERATION_CAPABILITY_PROVIDERS:
        advertised_value = capabilities.get(provider)
        evidence.append(
            {
                "provider": provider,
                "present": provider in capabilities,
                "advertised_value": advertised_value,
                "supported": advertised_value is not None
                and advertised_value is not False,
                "methods": list(methods),
            }
        )
    return evidence


def _operation_capabilities(capabilities: Mapping[str, Any]) -> list[str]:
    return [
        method
        for evidence in _operation_capability_evidence(capabilities)
        if evidence["supported"]
        for method in evidence["methods"]
    ]


def probe_server(
    entry: Mapping[str, Any], toolchain_root: Path, timeout: float
) -> dict[str, Any]:
    command = [str(toolchain_root / "bin" / entry["command"]), *entry["args"]]
    with tempfile.TemporaryDirectory(prefix="rna-lsp-probe-") as temporary:
        sandbox = Path(temporary)
        root = sandbox / "workspace"
        root.mkdir()
        probe = entry["probe"]
        document = root / probe["file_name"]
        document.parent.mkdir(parents=True, exist_ok=True)
        document.write_text("# probe\nprobe = 1\n")
        uri = document.resolve().as_uri()
        configuration = probe.get("configuration")
        rpc = JsonRpcProcess(
            command,
            root,
            toolchain_environment(toolchain_root, sandbox / "environment"),
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
                    "capabilities": _probe_client_capabilities(),
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
            operation_capability_evidence = _operation_capability_evidence(
                capabilities
            )
            operations = [
                method
                for evidence in operation_capability_evidence
                if evidence["supported"]
                for method in evidence["methods"]
            ]
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
                "executable": entry["executable"],
                "executable_sha256": entry["executable_sha256"],
                "negotiated_capabilities": operations,
                "negotiated_operation_capabilities": (
                    operation_capability_evidence
                ),
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


def _validate_toolchain_probe_evidence(
    probe_path: Path,
    lock_path: Path,
    provisioned_identity: Mapping[str, Any],
) -> dict[str, Any]:
    if probe_path.is_symlink() or not probe_path.is_file():
        raise ToolchainError("toolchain probe evidence must be a regular file")
    probe = load_json_object(probe_path, "toolchain probe evidence")
    _require_exact_fields(
        probe,
        {
            "schema_version",
            "lock_sha256",
            "toolchain_root",
            "inventory_sha256",
            "provision_receipt_digest",
            "provision_receipt_sha256",
            "installed",
            "launchers",
            "server_count",
            "servers",
            "probe_digest",
        },
        "toolchain probe evidence",
    )
    stored_digest = _require_sha256(
        probe.get("probe_digest"), "toolchain probe digest"
    )
    digest_payload = dict(probe)
    digest_payload.pop("probe_digest")
    if sha256_bytes(canonical_json(digest_payload)) != stored_digest:
        raise ToolchainError("toolchain probe self-digest mismatch")

    lock = load_json_object(lock_path, "toolchain lock for probe validation")
    lock_servers = lock.get("servers")
    probe_servers = probe.get("servers")
    if (
        not isinstance(lock_servers, list)
        or not lock_servers
        or any(not isinstance(server, dict) for server in lock_servers)
        or not isinstance(probe_servers, list)
        or type(probe.get("server_count")) is not int
        or probe["schema_version"] != SCHEMA_VERSION
        or probe["lock_sha256"] != sha256_file(lock_path)
        or probe.get("toolchain_root") != provisioned_identity.get("toolchain_root")
        or probe.get("inventory_sha256")
        != provisioned_identity.get("inventory_sha256")
        or probe.get("provision_receipt_digest")
        != provisioned_identity.get("provision_receipt_digest")
        or probe.get("provision_receipt_sha256")
        != provisioned_identity.get("provision_receipt_sha256")
        or probe.get("installed") != provisioned_identity.get("installed")
        or probe.get("launchers") != provisioned_identity.get("launchers")
        or probe["server_count"] != len(lock_servers)
        or len(probe_servers) != len(lock_servers)
    ):
        raise ToolchainError("toolchain probe lock/server identity mismatch")
    ordered_lock_servers = sorted(
        lock_servers,
        key=lambda server: _require_string(server.get("name"), "lock server name"),
    )
    lock_names = [server["name"] for server in ordered_lock_servers]
    if len(set(lock_names)) != len(lock_names):
        raise ToolchainError("toolchain lock has duplicate probe server names")

    for receipt, locked in zip(probe_servers, ordered_lock_servers, strict=True):
        if not isinstance(receipt, dict):
            raise ToolchainError("toolchain probe server receipt is malformed")
        _require_exact_fields(
            receipt,
            {
                "name",
                "version",
                "languages",
                "command",
                "args",
                "executable",
                "executable_sha256",
                "negotiated_capabilities",
                "negotiated_operation_capabilities",
                "operation",
                "result_count",
                "initialize_ms",
                "workspace_ready_ms",
                "operation_ms",
                "shutdown_ms",
                "quiescence_messages",
                "stderr_tail",
                "status",
            },
            "toolchain probe server receipt",
        )
        for field in (
            "name",
            "version",
            "languages",
            "command",
            "args",
            "executable",
            "executable_sha256",
        ):
            if receipt.get(field) != locked.get(field):
                raise ToolchainError(
                    f"toolchain probe server identity mismatch: {locked.get('name')} {field}"
                )
        operation = _require_string(
            locked.get("probe", {}).get("operation")
            if isinstance(locked.get("probe"), dict)
            else None,
            "lock probe operation",
        )
        expected_capabilities = locked.get("expected_capabilities")
        negotiated = receipt.get("negotiated_capabilities")
        evidence = receipt.get("negotiated_operation_capabilities")
        if (
            not isinstance(expected_capabilities, list)
            or any(
                not isinstance(capability, str) or not capability
                for capability in expected_capabilities
            )
            or not isinstance(negotiated, list)
            or any(
                not isinstance(capability, str) or not capability
                for capability in negotiated
            )
            or negotiated != list(dict.fromkeys(negotiated))
            or not isinstance(evidence, list)
            or len(evidence) != len(OPERATION_CAPABILITY_PROVIDERS)
        ):
            raise ToolchainError(
                f"toolchain probe capability evidence is malformed: {locked['name']}"
            )
        evidenced_operations = []
        for record, (provider, methods) in zip(
            evidence, OPERATION_CAPABILITY_PROVIDERS, strict=True
        ):
            if not isinstance(record, dict):
                raise ToolchainError("toolchain probe operation evidence is malformed")
            _require_exact_fields(
                record,
                {"provider", "present", "advertised_value", "supported", "methods"},
                "toolchain probe operation evidence",
            )
            advertised = record["advertised_value"]
            supported = advertised is not None and advertised is not False
            if (
                record.get("provider") != provider
                or record.get("methods") != list(methods)
                or type(record.get("present")) is not bool
                or type(record.get("supported")) is not bool
                or record["supported"] != supported
                or (record["supported"] and not record["present"])
            ):
                raise ToolchainError(
                    f"toolchain probe operation evidence drift: {locked['name']} {provider}"
                )
            if supported:
                evidenced_operations.extend(methods)
        if (
            negotiated != evidenced_operations
            or receipt.get("operation") != operation
            or operation not in negotiated
            or not set(expected_capabilities).issubset(negotiated)
            or receipt.get("status") != "ready"
        ):
            raise ToolchainError(
                f"toolchain probe required capability/status drift: {locked['name']}"
            )
        timing_fields = (
            "initialize_ms",
            "workspace_ready_ms",
            "operation_ms",
            "shutdown_ms",
            "quiescence_messages",
            "result_count",
        )
        if (
            any(
                type(receipt.get(field)) is not int or receipt[field] < 0
                for field in timing_fields
            )
            or receipt["workspace_ready_ms"] < receipt["initialize_ms"]
            or not isinstance(receipt.get("stderr_tail"), str)
        ):
            raise ToolchainError(
                f"toolchain probe timing/result evidence is invalid: {locked['name']}"
            )
    return probe


def probe_toolchain(
    lock_path: Path,
    inventory_path: Path,
    toolchain_root: Path,
    output_path: Path,
    timeout: float,
    repo_root: Path | None = None,
) -> dict[str, Any]:
    verification = verify_lock(lock_path, inventory_path, None, None, repo_root)
    if not verification["compatible"]:
        raise ToolchainError("cannot probe a lock with unsupported languages")
    provisioned_identity = _validate_provisioned_toolchain(
        lock_path, inventory_path, toolchain_root
    )
    toolchain_root = Path(provisioned_identity["toolchain_root"])
    lock = load_json_object(lock_path, "toolchain lock")
    receipts = []
    for entry in sorted(lock["servers"], key=lambda item: item["name"]):
        if (
            _validate_provisioned_toolchain(
                lock_path, inventory_path, toolchain_root
            )
            != provisioned_identity
        ):
            raise ToolchainError(
                f"provisioned toolchain identity changed before {entry['name']} probe"
            )
        try:
            receipts.append(probe_server(entry, toolchain_root, timeout))
        except ToolchainError as error:
            raise ToolchainError(f"{entry['name']} probe failed: {error}") from error
    if (
        _validate_provisioned_toolchain(lock_path, inventory_path, toolchain_root)
        != provisioned_identity
    ):
        raise ToolchainError("provisioned toolchain identity changed during probe")
    result = {
        "schema_version": SCHEMA_VERSION,
        "lock_sha256": sha256_file(lock_path),
        **provisioned_identity,
        "server_count": len(receipts),
        "servers": receipts,
    }
    result["probe_digest"] = sha256_bytes(canonical_json(result))
    write_canonical_json(output_path, result)
    return _validate_toolchain_probe_evidence(
        output_path, lock_path, provisioned_identity
    )


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


def _peak_memory_bytes() -> int:
    """Return the largest observed RSS for this qualifier or a completed child."""
    observed = max(
        resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss,
    )
    # Darwin reports bytes; Linux and the other supported Unix CI hosts report KiB.
    return int(observed if sys.platform == "darwin" else observed * 1024)


def _run_profiled_query(
    args: Sequence[str],
    cwd: Path,
    environment: Mapping[str, str],
    stdout_path: Path,
    stderr_path: Path,
    *,
    timeout_seconds: float = 300.0,
) -> dict[str, Any]:
    """Run one fresh-process query while measuring first stdout and total time."""
    if stdout_path.exists() or stderr_path.exists():
        raise ToolchainError("query probe output paths must be absent")
    started = time.monotonic()
    process = subprocess.Popen(
        list(args),
        cwd=cwd,
        env=dict(environment),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    if process.stdout is None or process.stderr is None:  # pragma: no cover
        raise ToolchainError("query probe pipes were not created")
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    streams: dict[str, bytearray] = {"stdout": bytearray(), "stderr": bytearray()}
    first_stdout_seconds: float | None = None
    timed_out = False
    try:
        while selector.get_map():
            remaining = timeout_seconds - (time.monotonic() - started)
            if remaining <= 0:
                timed_out = True
                break
            events = selector.select(min(remaining, 0.25))
            if not events and process.poll() is not None:
                # A final read after exit drains any bytes buffered in the pipes.
                events = [
                    (key, selectors.EVENT_READ)
                    for key in list(selector.get_map().values())
                ]
            for key, _ in events:
                chunk = os.read(key.fileobj.fileno(), 64 * 1024)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                observed = time.monotonic()
                label = str(key.data)
                streams[label].extend(chunk)
                if label == "stdout" and first_stdout_seconds is None:
                    first_stdout_seconds = observed - started
        if timed_out:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        else:
            process.wait()
    finally:
        selector.close()
    duration_seconds = time.monotonic() - started
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stdout_path.write_bytes(bytes(streams["stdout"]))
    stderr_path.write_bytes(bytes(streams["stderr"]))
    if timed_out:
        raise ToolchainError(
            f"query probe timed out after {timeout_seconds:.0f}s: {stdout_path.name}"
        )
    if process.returncode != 0:
        detail = bytes(streams["stderr"]).decode(errors="replace").strip()
        raise ToolchainError(
            f"query probe failed: {stdout_path.name}: {detail[-2000:]}"
        )
    if first_stdout_seconds is None or not streams["stdout"]:
        raise ToolchainError(f"query probe produced no stdout: {stdout_path.name}")
    return {
        "duration_ms": int(duration_seconds * 1000),
        "ttfe_ms": int(first_stdout_seconds * 1000),
        "peak_memory_bytes": _peak_memory_bytes(),
        "stdout_file": stdout_path.name,
        "stdout_sha256": sha256_file(stdout_path),
        "stderr_file": stderr_path.name,
        "stderr_sha256": sha256_file(stderr_path),
    }


def _selected_query_node(stdout: bytes) -> str:
    text = stdout.decode("utf-8", errors="strict")
    for match in re.finditer(r"(?m)^\s*`([^`\r\n]+)`\s*$", text):
        candidate = match.group(1)
        if (
            candidate.count(":") >= 2
            and candidate.rsplit(":", 1)[-1] in {"function", "method"}
            and not candidate.startswith(("http:", "https:"))
        ):
            return candidate
    raise ToolchainError("strict hybrid query returned no stable node identity")


def _run_combined_query_probes(
    *,
    combined: Any,
    rna_binary: Path,
    checkout: Path,
    environment: Mapping[str, str],
    evidence_root: Path,
    case_identity: Mapping[str, Any],
) -> dict[str, Any]:
    """Exercise every benchmark query path against a fresh-reopened READY cache."""
    if evidence_root.exists() or evidence_root.is_symlink():
        raise ToolchainError("combined query-evidence root must be absent")
    evidence_root.mkdir(parents=True)
    hybrid_command = [
        str(rna_binary),
        "--business-context",
        "disabled",
        "search",
        "--repo",
        str(checkout),
        "--search-mode",
        "hybrid",
        "--rerank",
        "--limit",
        "10",
        "--compact",
        COMBINED_QUERY,
    ]
    probes: dict[str, dict[str, Any]] = {}

    def run(name: str, command: Sequence[str]) -> bytes:
        stdout_path = evidence_root / f"{name}.stdout"
        stderr_path = evidence_root / f"{name}.stderr"
        probes[name] = _run_profiled_query(
            command,
            checkout,
            environment,
            stdout_path,
            stderr_path,
        )
        return stdout_path.read_bytes()

    first_output = run("first_hybrid_rerank", hybrid_command)
    first_text = first_output.decode("utf-8", errors="strict")
    first_stderr = (evidence_root / "first_hybrid_rerank.stderr").read_text(
        errors="replace"
    ).lower()
    if COMBINED_STRICT_SEARCH_SENTINEL not in first_text:
        raise ToolchainError("strict hybrid/RRF/rerank readiness sentinel is missing")
    if any(
        marker in first_stderr
        for marker in ("falling back", "using cpu", "original order")
    ):
        raise ToolchainError("strict hybrid/RRF/rerank query used a forbidden fallback")
    selected_node_id = _selected_query_node(first_output)

    graph_output = run(
        "graph_traversal",
        [
            str(rna_binary),
            "--business-context",
            "disabled",
            "search",
            "--repo",
            str(checkout),
            "--node",
            selected_node_id,
            "--mode",
            "neighbors",
            "--direction",
            "both",
            "--compact",
        ],
    ).decode("utf-8", errors="strict")
    if "0 result(s)" in graph_output or selected_node_id not in graph_output:
        raise ToolchainError("fresh-reopen graph traversal returned no persisted neighbors")

    full_output = run(
        "full_body",
        [
            str(rna_binary),
            "--business-context",
            "disabled",
            "search",
            "--repo",
            str(checkout),
            "--node",
            selected_node_id,
            "--include-body",
            "--compact",
        ],
    ).decode("utf-8", errors="strict")
    if selected_node_id not in full_output or "```" not in full_output:
        raise ToolchainError("fresh-reopen full-body retrieval returned no persisted body")

    minified_output = run(
        "minified_body",
        [
            str(rna_binary),
            "--business-context",
            "disabled",
            "search",
            "--repo",
            str(checkout),
            "--node",
            selected_node_id,
            "--include-body",
            "--minify-body",
            "--compact",
        ],
    ).decode("utf-8", errors="strict")
    if selected_node_id not in minified_output or "```" not in minified_output:
        raise ToolchainError("fresh-reopen minified-body retrieval returned no body")

    repeat_1 = run("repeat_hybrid_1", hybrid_command)
    repeat_2 = run("repeat_hybrid_2", hybrid_command)
    warm = run("warm_hybrid_rerank", hybrid_command)
    if repeat_1 != repeat_2 or repeat_2 != warm:
        raise ToolchainError("fresh-reopen hybrid/RRF/rerank output is not repeat-stable")
    for name in ("repeat_hybrid_1", "repeat_hybrid_2", "warm_hybrid_rerank"):
        stdout = (evidence_root / f"{name}.stdout").read_text(errors="strict")
        stderr = (evidence_root / f"{name}.stderr").read_text(
            errors="replace"
        ).lower()
        if COMBINED_STRICT_SEARCH_SENTINEL not in stdout or any(
            marker in stderr
            for marker in ("falling back", "using cpu", "original order")
        ):
            raise ToolchainError(f"{name} did not remain strict and fallback-free")

    peak_memory = max(profile["peak_memory_bytes"] for profile in probes.values())
    receipt = {
        "schema_version": combined.QUERY_EVIDENCE_SCHEMA_VERSION,
        "status": "ready",
        "case": dict(case_identity),
        "query": COMBINED_QUERY,
        "retrieval": {"mode": "hybrid", "fusion": "rrf", "rerank": True},
        "selected_node_id": selected_node_id,
        "strict_sentinel": COMBINED_STRICT_SEARCH_SENTINEL,
        "repeat_stable": True,
        "probes": probes,
        "peak_memory_bytes": peak_memory,
        "evidence_digest": "",
    }
    receipt["evidence_digest"] = sha256_bytes(canonical_json(receipt))
    receipt_path = evidence_root / combined.QUERY_EVIDENCE_RECEIPT
    _publish_canonical_json_exclusive(receipt_path, receipt)
    return {
        "root": evidence_root,
        "receipt_path": receipt_path,
        "receipt_sha256": sha256_file(receipt_path),
        "evidence_digest": receipt["evidence_digest"],
        "probes": probes,
        "peak_memory_bytes": peak_memory,
    }


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


def _raise_archive_failure(
    *,
    checkout: Path,
    instance_id: str,
    cases_root: Path,
    scan_log_path: Path,
    error: Exception,
    attempt_slug: str,
    archive_path: Path,
    sidecar_path: Path,
    additional_artifacts: Sequence[Path] = (),
) -> None:
    failure_path = _preserve_failed_case_evidence(
        checkout,
        instance_id,
        cases_root,
        scan_log_path,
        error,
        attempt_slug,
        publication_artifacts=[archive_path, sidecar_path, *additional_artifacts],
    )
    raise ToolchainError(f"{error}; failure evidence={failure_path}") from error


def _resume_ready_case(
    *,
    output_root: Path,
    case_index: int,
    instance: Mapping[str, Any],
    inventory_case: Mapping[str, Any],
    rna_binary: Path,
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
    receipt_scan_flags: Sequence[str] = QUALIFICATION_SCAN_FLAGS,
) -> tuple[dict[str, Any], dict[str, Any]] | None:
    catalog = _load_cache_catalog(output_root)
    candidates = [
        entry
        for entry in catalog["entries"]
        if entry.get("status") == "ready"
        and entry.get("case_index") == case_index
        and entry.get("instance_id") == instance["instance_id"]
    ]
    if not candidates:
        return None
    candidates.sort(key=lambda entry: int(entry.get("attempt_index", 0)))
    entry = candidates[-1]
    if (
        entry.get("population_index") != case_index
        or
        entry.get("repository") != instance["repo"]
        or entry.get("commit") != instance["base_commit"]
        or entry.get("tree") != inventory_case.get("tree")
    ):
        raise ToolchainError(
            f"resume receipt catalog identity mismatch for {instance['instance_id']}"
        )
    receipt_path = Path(_require_string(entry.get("receipt_path"), "resume receipt path"))
    if receipt_path.is_symlink() or not receipt_path.is_file():
        raise ToolchainError("resume receipt must be an existing regular file")
    if sha256_file(receipt_path) != _require_sha256(
        entry.get("receipt_sha256"), "resume receipt digest"
    ):
        raise ToolchainError("resume receipt/catalog digest mismatch")
    receipt = load_json_object(receipt_path, "resume case receipt")
    stored_receipt_digest = _require_sha256(
        receipt.get("receipt_digest"), "resume case receipt digest"
    )
    receipt_payload = dict(receipt)
    receipt_payload.pop("receipt_digest")
    if sha256_bytes(canonical_json(receipt_payload)) != stored_receipt_digest:
        raise ToolchainError("resume case receipt self-digest mismatch")
    producer = _validate_producer_identity(receipt.get("producer"))
    if (
        receipt.get("schema_version") != SCHEMA_VERSION
        or receipt.get("status") != "ready"
        or receipt.get("offline_preprocessing") is not True
        or receipt.get("population_index") != case_index
        or receipt.get("instance_id") != instance["instance_id"]
        or receipt.get("repository") != instance["repo"]
        or receipt.get("base_commit") != instance["base_commit"]
        or receipt.get("tree") != inventory_case.get("tree")
        or receipt.get("toolchain_lock_digest") != toolchain_lock_digest
        or receipt.get("inventory_digest") != inventory_digest
        or receipt.get("inventory_file_sha256") != inventory_file_sha256
        or receipt.get("case_inventory_digest")
        != inventory_case.get("per_file_digest")
        or receipt.get("scan_flags") != list(receipt_scan_flags)
    ):
        raise ToolchainError(
            f"resume receipt frozen identity mismatch for {instance['instance_id']}"
        )
    report_path = Path(_require_string(receipt.get("report_path"), "resume report path"))
    if report_path.is_symlink() or not report_path.is_file() or (
        sha256_file(report_path)
        != _require_sha256(receipt.get("report_sha256"), "resume report digest")
    ):
        raise ToolchainError("resume readiness report identity mismatch")
    report = load_json_object(report_path, "resume readiness report")
    if (
        report.get("violations") != []
        or report.get("digest") != receipt.get("report_digest")
        or report.get("graph_snapshot_digest") != receipt.get("graph_snapshot_digest")
        or report.get("digest") != entry.get("report_digest")
    ):
        raise ToolchainError("resume readiness report is not verifier-clean READY evidence")
    preflight_path = Path(
        _require_string(receipt.get("preflight_path"), "resume preflight path")
    )
    if preflight_path.is_symlink() or not preflight_path.is_file() or (
        sha256_file(preflight_path)
        != _require_sha256(receipt.get("preflight_sha256"), "resume preflight digest")
    ):
        raise ToolchainError("resume preflight evidence identity mismatch")
    preflight = load_json_object(preflight_path, "resume structural-cache preflight")
    preflight_digest = _require_sha256(
        preflight.get("digest"), "resume structural-cache preflight self-digest"
    )
    preflight_payload = dict(preflight)
    preflight_payload["digest"] = ""
    if (
        sha256_bytes(canonical_json(preflight_payload)) != preflight_digest
        or receipt.get("preflight_digest") != preflight_digest
        or preflight.get("case_index") != case_index
        or preflight.get("instance_id") != instance["instance_id"]
        or preflight.get("repository") != instance["repo"]
        or preflight.get("target_commit") != instance["base_commit"]
        or preflight.get("target_tree") != inventory_case.get("tree")
    ):
        raise ToolchainError("resume preflight evidence is not bound to frozen case")
    cache = receipt.get("cache")
    if not isinstance(cache, dict):
        raise ToolchainError("resume receipt has no structural cache identity")
    archive_path = Path(_require_string(cache.get("archive_path"), "resume archive path"))
    sidecar_path = Path(_require_string(cache.get("sidecar_path"), "resume sidecar path"))
    verified = verify_structural_cache_archive(
        archive_path,
        sidecar_path,
        expected={
            "repository": instance["repo"],
            "producer": producer,
            "toolchain_lock_digest": toolchain_lock_digest,
            "inventory_digest": inventory_digest,
            "inventory_file_sha256": inventory_file_sha256,
            "scan_flags": QUALIFICATION_SCAN_FLAGS,
        },
    )
    core = verified["core"]
    if (
        verified["archive_sha256"] != cache.get("archive_sha256")
        or verified["sidecar_sha256"] != cache.get("sidecar_sha256")
        or verified["core_sha256"] != cache.get("core_sha256")
        or verified["archive_sha256"] != entry.get("archive_sha256")
        or verified["sidecar_sha256"] != entry.get("sidecar_sha256")
        or verified["core_sha256"] != entry.get("core_sha256")
        or core["commit"] != instance["base_commit"]
        or core["tree"] != inventory_case.get("tree")
        or core["case_inventory_digest"] != inventory_case.get("per_file_digest")
        or core["configuration_digest"] != receipt.get("configuration_digest")
        or core["completeness_report_digest"] != report.get("digest")
        or core["graph_snapshot_digest"] != report.get("graph_snapshot_digest")
    ):
        raise ToolchainError("resume structural cache identities differ from receipt")
    timings = receipt.get("timings_ms")
    if not isinstance(timings, dict) or any(
        type(timings.get(field)) is not int or timings[field] < 0
        for field in (
            "scan_update",
            "full_readiness_validation",
            "cache_archive",
        )
    ):
        raise ToolchainError("resume timing evidence is malformed")
    for field in ("cache_selection", "cache_verification", "cache_injection"):
        value = timings.get(field)
        if value is not None and (type(value) is not int or value < 0):
            raise ToolchainError("resume cache-preparation timing evidence is malformed")
    if any(timings.get(field) is None for field in (
        "cache_selection",
        "cache_verification",
        "cache_injection",
    )) and not isinstance(receipt.get("timing_provenance"), dict):
        raise ToolchainError("resume recovery timing provenance is missing")
    return (
        {
            "instance_id": instance["instance_id"],
            "repository": instance["repo"],
            "base_commit": instance["base_commit"],
            "report_path": str(report_path.resolve()),
            "receipt_path": str(receipt_path.resolve()),
            "receipt_sha256": sha256_file(receipt_path),
            "archive_sha256": verified["archive_sha256"],
            "sidecar_sha256": verified["sidecar_sha256"],
            "core_sha256": verified["core_sha256"],
        },
        {
            "instance_id": instance["instance_id"],
            "population_index": case_index,
            "scan_ms": timings["scan_update"],
            "readiness_ms": timings["full_readiness_validation"],
            "cache_selection_ms": timings["cache_selection"],
            "cache_verification_ms": timings["cache_verification"],
            "cache_injection_ms": timings["cache_injection"],
            "cache_archive_ms": timings["cache_archive"],
            "report_sha256": sha256_file(report_path),
        },
    )


def _load_regular_json_with_sha(
    path: Path, expected_sha256: str, label: str
) -> dict[str, Any]:
    expected_sha256 = _require_sha256(expected_sha256, f"{label} SHA-256")
    if path.is_symlink() or not path.is_file():
        raise ToolchainError(f"{label} must be an existing regular file")
    if sha256_file(path) != expected_sha256:
        raise ToolchainError(f"{label} SHA-256 mismatch")
    return load_json_object(path, label)


def _recovered_scan_timing(
    failure_receipt: Mapping[str, Any], instance_id: str
) -> tuple[int, dict[str, Any]]:
    scan_log_path = Path(
        _require_string(failure_receipt.get("scan_log_path"), "retained scan log path")
    )
    scan_log_sha256 = _require_sha256(
        failure_receipt.get("scan_log_sha256"), "retained scan log SHA-256"
    )
    if scan_log_path.is_symlink() or not scan_log_path.is_file():
        raise ToolchainError("retained scan log must be an existing regular file")
    if sha256_file(scan_log_path) != scan_log_sha256:
        raise ToolchainError("retained scan log SHA-256 mismatch")
    totals = set()
    for line in scan_log_path.read_text(errors="replace").splitlines():
        stripped = line.strip()
        if not stripped.startswith("- total: "):
            continue
        parts = stripped.removeprefix("- total: ").split()
        if (
            len(parts) == 2
            and parts[0].endswith("m")
            and parts[1].endswith("s")
            and parts[0][:-1].isdigit()
            and parts[1][:-1].isdigit()
        ):
            totals.add((int(parts[0][:-1]) * 60 + int(parts[1][:-1])) * 1000)
    if len(totals) != 1:
        raise ToolchainError(
            f"{instance_id} retained scan log has no unique whole-second total"
        )
    recovered_ms = totals.pop()
    return recovered_ms, {
        "source": "retained_attempt_001_scan_log",
        "path": str(scan_log_path.resolve()),
        "sha256": scan_log_sha256,
        "resolution_ms": 1000,
        "value_ms": recovered_ms,
    }


def _validate_resume_replay_evidence(
    *,
    output_root: Path,
    case_index: int,
    instance: Mapping[str, Any],
    inventory_case: Mapping[str, Any],
    checkout: Path,
    replay_receipt_path: Path,
    replay_receipt_sha256: str,
    approved_preflight_path: Path,
    target_identity: Mapping[str, Any],
    rna_binary: Path,
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
) -> dict[str, Any]:
    instance_id = instance["instance_id"]
    if checkout.is_symlink() or not checkout.is_dir():
        raise ToolchainError("resume replay checkout must be an existing regular directory")
    replay = _load_regular_json_with_sha(
        replay_receipt_path,
        replay_receipt_sha256,
        "diagnostic replay receipt",
    )
    _require_exact_fields(replay, REPLAY_RECEIPT_FIELDS, "diagnostic replay receipt")
    replay["source_producer"] = _validate_producer_identity(
        replay["source_producer"]
    )
    replay["replay_producer"] = _validate_producer_identity(
        replay["replay_producer"]
    )
    count_fields = REPLAY_RECEIPT_FIELDS - {
        "schema_version",
        "diagnostic_only",
        "publishable",
        "checkout_rebuilt",
        "archive_created",
        "catalog_updated",
        "failure_receipt_sha256",
        "failure_digest",
        "authorization_sha256",
        "source_producer_commit",
        "replay_producer_commit",
        "source_producer",
        "replay_producer",
        "target_commit",
        "target_tree",
        "target_tree_source",
        "source_checkout_identity_verified",
        "source_tree_diff_replayed",
        "source_rescanned",
        "full_target_readiness_recomputed",
        "incremental_enrichment_job_id",
        "pass1_job_ids",
        "base_completeness_digest",
        "checkpoint_validation_digest",
        "diagnostic_checkpoint_validation_passed",
        "target_completeness_digest",
        "full_target_ready",
    }
    if any(type(replay[field]) is not int or replay[field] < 0 for field in count_fields):
        raise ToolchainError("diagnostic replay receipt has invalid counters")
    if (
        replay["schema_version"] != SCHEMA_VERSION
        or replay["diagnostic_only"] is not True
        or replay["publishable"] is not False
        or replay["checkout_rebuilt"] is not False
        or replay["lsp_calls"] != 0
        or replay["archive_created"] is not False
        or replay["catalog_updated"] is not False
        or replay["source_checkout_identity_verified"] is not True
        or replay["source_tree_diff_replayed"] is not False
        or replay["source_rescanned"] is not False
        or replay["full_target_readiness_recomputed"] is not True
        or replay["diagnostic_checkpoint_validation_passed"] is not True
        or replay["coverage_violation_count"] != 0
        or replay["compatibility_violation_count"] != 0
        or replay["discarded_required_result_count"] != 0
        or replay["full_target_ready"] is not True
        or replay["target_tree_source"]
        != "copied_retained_checkout_and_verified_authorization"
    ):
        raise ToolchainError("diagnostic replay receipt is not verifier-clean zero-LSP evidence")
    for field in (
        "failure_receipt_sha256",
        "failure_digest",
        "authorization_sha256",
        "base_completeness_digest",
        "checkpoint_validation_digest",
        "target_completeness_digest",
    ):
        _require_sha256(replay[field], f"diagnostic replay {field}")
    _require_git_oid(replay["source_producer_commit"], "source producer commit")
    _require_git_oid(replay["replay_producer_commit"], "replay producer commit")
    _require_git_oid(replay["target_commit"], "replay target commit")
    _require_git_oid(replay["target_tree"], "replay target tree")
    if (
        not isinstance(replay["pass1_job_ids"], list)
        or not replay["pass1_job_ids"]
        or replay["pass1_job_ids"] != sorted(set(replay["pass1_job_ids"]))
        or not all(isinstance(job_id, str) and job_id for job_id in replay["pass1_job_ids"])
        or not isinstance(replay["incremental_enrichment_job_id"], str)
        or not replay["incremental_enrichment_job_id"]
    ):
        raise ToolchainError("diagnostic replay job provenance is malformed")

    failure_path = output_root / "cases" / f"{instance_id}-attempt-001-failure.json"
    failure = _load_regular_json_with_sha(
        failure_path,
        replay["failure_receipt_sha256"],
        "attempt-001 failure receipt",
    )
    _require_exact_fields(
        failure,
        {
            "schema_version",
            "status",
            "instance_id",
            "error",
            "graph_snapshot_digest",
            "scan_log_path",
            "scan_log_sha256",
            "evidence",
            "publication_artifacts",
            "full_cache_retained",
            "full_cache_error",
            "failure_digest",
        },
        "attempt-001 failure receipt",
    )
    failure_digest = _require_sha256(
        failure.get("failure_digest"), "attempt-001 failure digest"
    )
    failure_payload = dict(failure)
    failure_payload.pop("failure_digest")
    if (
        failure["schema_version"] != SCHEMA_VERSION
        or failure["status"] != "failed"
        or failure["instance_id"] != instance_id
        or failure["full_cache_retained"] is not True
        or failure["full_cache_error"] is not None
        or failure_digest != replay["failure_digest"]
        or sha256_bytes(canonical_json(failure_payload)) != failure_digest
    ):
        raise ToolchainError("attempt-001 failure provenance is invalid")

    authorization_path = checkout / ".oh/.cache/structural-cache-inheritance.json"
    authorization = _load_regular_json_with_sha(
        authorization_path,
        replay["authorization_sha256"],
        "replay structural-cache authorization",
    )
    _require_exact_fields(
        authorization,
        {
            "schema_version",
            "offline_preprocessing",
            "repository",
            "base_commit",
            "base_tree",
            "target_commit",
            "target_tree",
            "root_slug",
            "producer",
            "toolchain_lock_digest",
            "inventory_digest",
            "inventory_file_sha256",
            "configuration_digest",
            "scan_flags",
            "base_archive_sha256",
            "base_sidecar_sha256",
            "base_core_sha256",
            "base_report_digest",
            "base_report_sha256",
            "inherited_files",
            "changed_paths",
            "added_paths",
            "deleted_paths",
            "renamed_paths",
            "invalidated_partitions",
            "invalidated_paths",
            "path_partitions",
            "executed_operation_budget",
            "digest",
        },
        "replay structural-cache authorization",
    )
    authorization["producer"] = _validate_producer_identity(authorization["producer"])
    if (
        authorization["schema_version"] != STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION
        or authorization["offline_preprocessing"] is not True
    ):
        raise ToolchainError("replay structural-cache authorization schema/status mismatch")
    authorization_digest = _require_sha256(
        authorization.get("digest"), "replay authorization digest"
    )
    authorization_payload = dict(authorization)
    authorization_payload["digest"] = ""
    if sha256_bytes(canonical_json(authorization_payload)) != authorization_digest:
        raise ToolchainError("replay authorization self-digest mismatch")
    operation_budget = authorization["executed_operation_budget"]
    _require_exact_fields(
        operation_budget,
        {
            "max_operations",
            "executed_estimate",
            "authorized_operations_by_language",
            "basis",
            "estimated_file_count",
        },
        "replay structural-cache operation budget",
    )

    approved_sha256 = sha256_file(approved_preflight_path)
    approved = _load_regular_json_with_sha(
        approved_preflight_path,
        approved_sha256,
        "approved structural-cache preflight",
    )
    validate_structural_cache_preflight(approved, target_identity)
    selected_base = approved.get("selected_base_cache")
    if not isinstance(selected_base, dict):
        raise ToolchainError("resume replay requires an approved incremental base cache")
    _require_exact_fields(
        selected_base,
        {
            "instance_id",
            "attempt_index",
            "repository",
            "commit",
            "tree",
            "archive_sha256",
            "sidecar_sha256",
            "core_sha256",
        },
        "approved replay base cache",
    )
    expected_base = {
        "repository": authorization["repository"],
        "commit": authorization["base_commit"],
        "tree": authorization["base_tree"],
        "archive_sha256": authorization["base_archive_sha256"],
        "sidecar_sha256": authorization["base_sidecar_sha256"],
        "core_sha256": authorization["base_core_sha256"],
    }
    if any(selected_base.get(field) != value for field, value in expected_base.items()):
        raise ToolchainError("approved preflight/base authorization identities differ")
    approved_operations = approved.get("expected_operation_count")
    if (
        not isinstance(approved_operations, dict)
        or approved_operations.get("max_executed")
        != operation_budget["max_operations"]
        or approved_operations.get("executed_estimate")
        != operation_budget["executed_estimate"]
        or approved_operations.get("authorized_operations_by_language")
        != operation_budget["authorized_operations_by_language"]
        or approved_operations.get("basis") != operation_budget["basis"]
        or approved_operations.get("estimated_file_count")
        != operation_budget["estimated_file_count"]
    ):
        raise ToolchainError("approved preflight/authorization operation budgets differ")

    expected_identity = {
        "repository": instance["repo"],
        "commit": instance["base_commit"],
        "tree": inventory_case.get("tree"),
        "configuration_digest": target_identity["configuration_digest"],
        "root_slug": target_identity["root_slug"],
    }
    if (
        any(target_identity.get(field) != value for field, value in expected_identity.items())
        or target_identity["producer"].get("binary_sha256") != sha256_file(rna_binary)
        or replay["target_commit"] != instance["base_commit"]
        or replay["target_tree"] != inventory_case.get("tree")
        or replay["replay_producer"] != target_identity["producer"]
        or replay["replay_producer_commit"]
        != target_identity["producer"]["producer_commit"]
        or replay["source_producer"] != authorization["producer"]
        or replay["source_producer_commit"]
        != authorization["producer"]["producer_commit"]
        or replay["base_completeness_digest"]
        != authorization["base_report_digest"]
        or authorization["repository"] != instance["repo"]
        or authorization["target_commit"] != instance["base_commit"]
        or authorization["target_tree"] != inventory_case.get("tree")
        or authorization["root_slug"] != target_identity["root_slug"]
        or authorization["toolchain_lock_digest"] != toolchain_lock_digest
        or authorization["inventory_digest"] != inventory_digest
        or authorization["inventory_file_sha256"] != inventory_file_sha256
        or authorization["configuration_digest"]
        != target_identity["configuration_digest"]
        or authorization["scan_flags"] != QUALIFICATION_SCAN_FLAGS
        or approved["case_index"] != case_index
        or approved["instance_id"] != instance_id
        or approved["repository"] != instance["repo"]
        or approved["target_commit"] != instance["base_commit"]
        or approved["target_tree"] != inventory_case.get("tree")
        or replay["target_inventory_path_count"]
        != inventory_case.get("included_file_count")
        or replay["validated_inventory_path_count"]
        != inventory_case.get("included_file_count")
    ):
        raise ToolchainError("resume replay frozen identity binding mismatch")
    scan_ms, scan_provenance = _recovered_scan_timing(failure, instance_id)
    return {
        "replay": replay,
        "failure": failure,
        "failure_path": failure_path,
        "authorization": authorization,
        "authorization_path": authorization_path,
        "approved_preflight": approved,
        "approved_preflight_sha256": approved_sha256,
        "scan_ms": scan_ms,
        "scan_provenance": scan_provenance,
    }


def _publish_resume_replay_case(
    *,
    output_root: Path,
    case_index: int,
    instance: Mapping[str, Any],
    inventory_case: Mapping[str, Any],
    checkout: Path,
    replay_receipt_path: Path,
    replay_receipt_sha256: str,
    approved_preflight_path: Path,
    rna_binary: Path,
    environment: Mapping[str, str],
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    instance_id = instance["instance_id"]
    target_identity = structural_cache_identity(rna_binary, checkout)
    recovery = _validate_resume_replay_evidence(
        output_root=output_root,
        case_index=case_index,
        instance=instance,
        inventory_case=inventory_case,
        checkout=checkout,
        replay_receipt_path=replay_receipt_path,
        replay_receipt_sha256=replay_receipt_sha256,
        approved_preflight_path=approved_preflight_path,
        target_identity=target_identity,
        rna_binary=rna_binary,
        toolchain_lock_digest=toolchain_lock_digest,
        inventory_digest=inventory_digest,
        inventory_file_sha256=inventory_file_sha256,
    )
    replay = recovery["replay"]
    authorization = recovery["authorization"]
    preflight = recovery["approved_preflight"]

    selected_base = preflight["selected_base_cache"]
    base_entries = [
        entry
        for entry in _load_cache_catalog(output_root)["entries"]
        if entry.get("status") == "ready"
        and entry.get("instance_id") == selected_base["instance_id"]
        and entry.get("attempt_index") == selected_base["attempt_index"]
        and entry.get("archive_sha256") == selected_base["archive_sha256"]
        and entry.get("sidecar_sha256") == selected_base["sidecar_sha256"]
        and entry.get("core_sha256") == selected_base["core_sha256"]
    ]
    if len(base_entries) != 1:
        raise ToolchainError("resume replay base cache has no unique catalog entry")
    base_entry = base_entries[0]
    verify_structural_cache_archive(
        Path(_require_string(base_entry.get("archive_path"), "base archive path")),
        Path(_require_string(base_entry.get("sidecar_path"), "base sidecar path")),
        expected={
            "repository": instance["repo"],
            "root_slug": target_identity["root_slug"],
            "producer": replay["source_producer"],
            "toolchain_lock_digest": toolchain_lock_digest,
            "inventory_digest": inventory_digest,
            "inventory_file_sha256": inventory_file_sha256,
            "inventory_policy_digest": target_identity["inventory_policy_digest"],
            "scan_flags": QUALIFICATION_SCAN_FLAGS,
        },
    )

    attempt_index = _next_case_attempt_index(output_root, instance_id)
    attempt_slug = f"{instance_id}-attempt-{attempt_index:03d}"
    cases_root = output_root / "cases"
    logs_root = output_root / "logs"
    archives_root = output_root / "structural-caches"
    report_path = cases_root / f"{attempt_slug}.json"
    receipt_path = cases_root / f"{attempt_slug}-receipt.json"
    readiness_log_path = logs_root / f"{attempt_slug}-readiness.log"
    archive_path = archives_root / f"{attempt_slug}.tar.gz"
    sidecar_path = archives_root / f"{attempt_slug}.manifest.json"
    for path in (
        report_path,
        receipt_path,
        readiness_log_path,
        archive_path,
        sidecar_path,
    ):
        if path.exists() or path.is_symlink():
            raise ToolchainError(f"resume replay refuses to overwrite evidence: {path}")

    case_environment = dict(environment)
    case_environment[STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV] = replay[
        "authorization_sha256"
    ]
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
    source_report_path = checkout / ".oh/.cache/lsp_completeness.json"
    report = load_json_object(source_report_path, f"{instance_id} replay readiness")
    _require_ready_case(readiness_log_path, report, instance_id)
    if report.get("digest") != replay["target_completeness_digest"]:
        raise ToolchainError("replay receipt and fresh readiness report differ")

    execution = load_json_object(
        checkout / ".oh/.cache/structural-cache-execution.json",
        f"{instance_id} replay cache execution",
    )
    executed_paths = execution.get("executed_paths")
    inherited_paths = execution.get("inherited_paths")
    if (
        not isinstance(executed_paths, list)
        or not isinstance(inherited_paths, list)
        or any(not isinstance(path, str) or not path for path in executed_paths)
        or any(not isinstance(path, str) or not path for path in inherited_paths)
        or execution.get("executed_graph_enrichment_operation_count")
        != replay["executed_operation_count"]
        or execution.get("executed_readiness_validation_request_count")
        != replay["readiness_validation_request_count"]
        or _readiness_validation_request_count(report)
        != execution.get("inherited_readiness_validation_request_count", 0)
        + execution.get("executed_readiness_validation_request_count", 0)
    ):
        raise ToolchainError("replay execution evidence differs from verified checkpoint")

    base_cache = {
        "archive_sha256": authorization["base_archive_sha256"],
        "sidecar_sha256": authorization["base_sidecar_sha256"],
        "core_sha256": authorization["base_core_sha256"],
        "report_digest": authorization["base_report_digest"],
    }
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
            inventory_case.get("per_file_digest"), f"{instance_id} inventory digest"
        ),
        base_cache=base_cache,
    )
    archive_seconds = time.monotonic() - archive_started

    _publish_canonical_json_exclusive(report_path, report)
    recovered_timings = {
        "cache_selection": None,
        "cache_verification": None,
        "cache_injection": None,
        "scan_update": recovery["scan_ms"],
        "full_readiness_validation": int(readiness_seconds * 1000),
        "cache_archive": int(archive_seconds * 1000),
    }
    case_receipt = {
        "schema_version": SCHEMA_VERSION,
        "status": "ready",
        "offline_preprocessing": True,
        "population_index": case_index,
        "instance_id": instance_id,
        "attempt_index": attempt_index,
        "repository": instance["repo"],
        "base_commit": instance["base_commit"],
        "tree": target_identity["tree"],
        "producer": target_identity["producer"],
        "toolchain_lock_digest": toolchain_lock_digest,
        "inventory_digest": inventory_digest,
        "inventory_file_sha256": inventory_file_sha256,
        "case_inventory_digest": inventory_case["per_file_digest"],
        "configuration_digest": target_identity["configuration_digest"],
        "scan_flags": QUALIFICATION_SCAN_FLAGS,
        "preflight_path": str(approved_preflight_path.resolve()),
        "preflight_sha256": recovery["approved_preflight_sha256"],
        "preflight_digest": preflight["digest"],
        "report_path": str(report_path.resolve()),
        "report_sha256": sha256_file(report_path),
        "report_digest": report["digest"],
        "graph_snapshot_digest": report["graph_snapshot_digest"],
        "cache": archive_receipt,
        "base_cache": {
            **base_cache,
            "authorization_sha256": replay["authorization_sha256"],
            "recovery": "retained_post_lsp_zero_lsp_replay",
        },
        "timings_ms": recovered_timings,
        "timing_provenance": {
            "cache_selection": "not_persisted_by_failed_attempt",
            "cache_verification": "not_persisted_by_failed_attempt",
            "cache_injection": "not_persisted_by_failed_attempt",
            "scan_update": recovery["scan_provenance"],
            "full_readiness_validation": "measured_during_zero_lsp_replay_publication",
            "cache_archive": "measured_during_zero_lsp_replay_publication",
        },
        "recovery": {
            "mode": "retained_post_lsp_zero_lsp_replay",
            "failure_receipt_path": str(recovery["failure_path"].resolve()),
            "failure_receipt_sha256": replay["failure_receipt_sha256"],
            "failure_digest": replay["failure_digest"],
            "replay_receipt_path": str(replay_receipt_path.resolve()),
            "replay_receipt_sha256": replay_receipt_sha256,
            "checkpoint_validation_digest": replay["checkpoint_validation_digest"],
            "lsp_calls": 0,
            "source_rescanned": False,
        },
        "changed_file_count": execution.get("changed_file_count", 0),
        "invalidated_file_count": execution.get("invalidated_file_count", 0),
        "graph_enrichment_operations_reused": execution.get(
            "inherited_graph_enrichment_operation_count", 0
        ),
        "graph_enrichment_operations_executed": execution[
            "executed_graph_enrichment_operation_count"
        ],
        "readiness_validation_requests_reused": execution.get(
            "inherited_readiness_validation_request_count", 0
        ),
        "readiness_validation_requests_executed": execution[
            "executed_readiness_validation_request_count"
        ],
    }
    case_receipt["receipt_digest"] = sha256_bytes(canonical_json(case_receipt))
    _publish_canonical_json_exclusive(receipt_path, case_receipt)
    _publish_cache_catalog_entry(
        output_root,
        {
            "schema_version": STRUCTURAL_CACHE_SCHEMA_VERSION,
            "status": "ready",
            "case_index": case_index,
            "attempt_index": attempt_index,
            "instance_id": instance_id,
            "population_index": case_index,
            "repository": instance["repo"],
            "commit": instance["base_commit"],
            "tree": target_identity["tree"],
            "archive_path": archive_receipt["archive_path"],
            "archive_sha256": archive_receipt["archive_sha256"],
            "core_sha256": archive_receipt["core_sha256"],
            "sidecar_path": archive_receipt["sidecar_path"],
            "sidecar_sha256": archive_receipt["sidecar_sha256"],
            "report_digest": report["digest"],
            "receipt_path": str(receipt_path.resolve()),
            "receipt_sha256": sha256_file(receipt_path),
        },
    )
    cohort_case = {
        "instance_id": instance_id,
        "repository": instance["repo"],
        "base_commit": instance["base_commit"],
        "report_path": str(report_path.resolve()),
        "receipt_path": str(receipt_path.resolve()),
        "receipt_sha256": sha256_file(receipt_path),
        "archive_sha256": archive_receipt["archive_sha256"],
        "sidecar_sha256": archive_receipt["sidecar_sha256"],
        "core_sha256": archive_receipt["core_sha256"],
    }
    timing_case = {
        "instance_id": instance_id,
        "population_index": case_index,
        "scan_ms": recovery["scan_ms"],
        "readiness_ms": recovered_timings["full_readiness_validation"],
        "cache_selection_ms": None,
        "cache_verification_ms": None,
        "cache_injection_ms": None,
        "cache_archive_ms": recovered_timings["cache_archive"],
        "report_sha256": sha256_file(report_path),
        "recovery": True,
    }
    return cohort_case, timing_case


def _publish_or_verify_canonical_json(path: Path, value: Mapping[str, Any]) -> None:
    if path.exists() or path.is_symlink():
        if path.is_symlink() or not path.is_file() or load_json_object(
            path, path.name
        ) != value:
            raise ToolchainError(f"existing immutable evidence differs: {path}")
        return
    _publish_canonical_json_exclusive(path, value)


def _qualification_checkpoint(
    *,
    output_root: Path,
    last_case_index: int,
    cohort_cases: Sequence[Mapping[str, Any]],
    timing_cases: Sequence[Mapping[str, Any]],
    isolated: bool,
) -> dict[str, Any]:
    checkpoint = {
        "schema_version": SCHEMA_VERSION,
        "status": "checkpoint",
        "isolated_micro_qualification": isolated,
        "last_case_index": last_case_index,
        "completed_case_count": len(cohort_cases),
        "cases": list(cohort_cases),
        "timings": list(timing_cases),
        "digest": "",
    }
    checkpoint["digest"] = sha256_bytes(canonical_json(checkpoint))
    path = output_root / f"checkpoint-case-{last_case_index:03d}.json"
    _publish_or_verify_canonical_json(path, checkpoint)
    return {**checkpoint, "path": str(path.resolve()), "sha256": sha256_file(path)}


def _build_frozen_cohort_manifest(
    cohort_cases: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    cases = [dict(case) for case in cohort_cases]
    schema_versions: set[int] = set()
    for case in cases:
        instance_id = _require_string(
            case.get("instance_id"), "cohort case instance ID"
        )
        report_path = Path(
            _require_string(case.get("report_path"), "cohort case report path")
        )
        if report_path.is_symlink() or not report_path.is_file():
            raise ToolchainError(
                f"cohort readiness report must be a regular file: {instance_id}"
            )
        report = load_json_object(report_path, f"{instance_id} readiness report")
        identity = report.get("identity")
        if not isinstance(identity, dict):
            raise ToolchainError(
                f"cohort readiness report has no identity: {instance_id}"
            )
        schema_version = identity.get("schema_version")
        if type(schema_version) is not int or schema_version <= 0:
            raise ToolchainError(
                f"cohort readiness report has an invalid schema: {instance_id}"
            )
        schema_versions.add(schema_version)
    if len(schema_versions) != 1:
        raise ToolchainError(
            "cohort readiness reports do not share one completeness schema"
        )
    return {"schema_version": schema_versions.pop(), "cases": cases}


def _validate_ready_aggregate(
    aggregate: Mapping[str, Any],
    cohort_manifest: Mapping[str, Any],
    *,
    recomputed_aggregate: Mapping[str, Any],
    expected_population_digest: str,
) -> None:
    if aggregate != recomputed_aggregate:
        raise ToolchainError(
            "aggregate readiness evidence differs from independently recomputed "
            "producer output"
        )
    _require_exact_fields(
        aggregate,
        {"schema_version", "cohort_digest", "checkouts", "counts", "digest"},
        "aggregate readiness report",
    )
    cases = cohort_manifest.get("cases")
    manifest_schema = cohort_manifest.get("schema_version")
    if (
        not isinstance(cases, list)
        or not cases
        or type(manifest_schema) is not int
        or manifest_schema <= 0
        or aggregate.get("schema_version") != manifest_schema
    ):
        raise ToolchainError("aggregate readiness schema or cohort is invalid")
    expected_population_digest = _require_sha256(
        expected_population_digest, "expected frozen population digest"
    )
    if aggregate.get("cohort_digest") != expected_population_digest:
        raise ToolchainError(
            "aggregate cohort digest differs from frozen population digest"
        )

    expected_identities = set()
    report_bindings: dict[tuple[str, str, str], tuple[str, int]] = {}
    report_paths: set[Path] = set()
    for case in cases:
        if not isinstance(case, dict):
            raise ToolchainError("cohort manifest case is malformed")
        identity_key = (
            _require_string(case.get("instance_id"), "cohort instance ID"),
            _require_string(case.get("repository"), "cohort repository"),
            _require_git_oid(case.get("base_commit"), "cohort base commit"),
        )
        report_path = Path(
            _require_string(case.get("report_path"), "cohort report path")
        )
        if report_path.is_symlink() or not report_path.is_file():
            raise ToolchainError("cohort report binding is not a regular file")
        resolved_report_path = report_path.resolve()
        if resolved_report_path in report_paths:
            raise ToolchainError("cohort report path is duplicated")
        report_paths.add(resolved_report_path)
        report = load_json_object(report_path, f"{identity_key[0]} cohort report")
        report_identity = report.get("identity")
        summary = report.get("summary")
        report_digest = _require_sha256(
            report.get("digest"), f"{identity_key[0]} report digest"
        )
        if (
            not isinstance(report_identity, dict)
            or report_identity.get("schema_version") != manifest_schema
            or report_identity.get("context_mode") != "disabled"
            or report_identity.get("repository") != identity_key[1]
            or report_identity.get("checkout_sha") != identity_key[2]
            or not isinstance(summary, dict)
            or type(summary.get("total_files")) is not int
            or summary["total_files"] < 0
            or report.get("violations") != []
        ):
            raise ToolchainError(
                "cohort report content identity is not verifier-clean READY"
            )
        expected_identities.add(identity_key)
        report_bindings[identity_key] = (report_digest, summary["total_files"])
    if len(expected_identities) != len(cases):
        raise ToolchainError("cohort manifest identities are missing or duplicated")

    counts = aggregate.get("counts")
    if not isinstance(counts, dict):
        raise ToolchainError("aggregate readiness counts are missing")
    _require_exact_fields(
        counts,
        {
            "checkouts",
            "unique_instances",
            "ready_checkouts",
            "files",
            "by_extension",
            "by_role",
            "by_status",
        },
        "aggregate readiness counts",
    )
    expected_count = len(cases)
    if (
        any(
            type(counts.get(field)) is not int or counts[field] < 0
            for field in ("checkouts", "unique_instances", "ready_checkouts", "files")
        )
        or counts["checkouts"] != expected_count
        or counts["unique_instances"] != expected_count
        or counts["ready_checkouts"] != expected_count
        or counts["files"]
        != sum(file_count for _, file_count in report_bindings.values())
        or any(
            not isinstance(counts[field], dict)
            or any(
                not isinstance(key, str)
                or not key
                or type(value) is not int
                or value < 0
                for key, value in counts[field].items()
            )
            for field in ("by_extension", "by_role", "by_status")
        )
    ):
        raise ToolchainError("aggregate readiness counts are not fully READY")

    checkouts = aggregate.get("checkouts")
    if not isinstance(checkouts, list) or len(checkouts) != expected_count:
        raise ToolchainError("aggregate readiness checkout count differs from cohort")
    actual_identities = set()
    for checkout in checkouts:
        if not isinstance(checkout, dict):
            raise ToolchainError("aggregate readiness checkout is malformed")
        _require_exact_fields(
            checkout,
            {
                "instance_id",
                "repository",
                "base_commit",
                "checkout_sha",
                "report_digest",
                "ready",
                "file_count",
                "violation_count",
            },
            "aggregate readiness checkout",
        )
        instance_id = _require_string(
            checkout.get("instance_id"), "aggregate instance ID"
        )
        repository = _require_string(
            checkout.get("repository"), "aggregate repository"
        )
        base_commit = _require_git_oid(
            checkout.get("base_commit"), "aggregate base commit"
        )
        checkout_sha = _require_git_oid(
            checkout.get("checkout_sha"), "aggregate checkout SHA"
        )
        report_digest = _require_sha256(
            checkout.get("report_digest"), "aggregate report digest"
        )
        identity_key = (instance_id, repository, base_commit)
        report_binding = report_bindings.get(identity_key)
        if (
            report_binding is None
            or report_digest != report_binding[0]
            or checkout_sha != base_commit
            or checkout.get("ready") is not True
            or type(checkout.get("file_count")) is not int
            or checkout["file_count"] != report_binding[1]
            or type(checkout.get("violation_count")) is not int
            or checkout["violation_count"] != 0
        ):
            raise ToolchainError(
                "aggregate readiness checkout is not verifier-clean READY"
            )
        actual_identities.add(identity_key)
    if actual_identities != expected_identities:
        raise ToolchainError("aggregate readiness identities differ from cohort")
    _require_sha256(aggregate.get("digest"), "aggregate report digest")


def _select_qualification_instances(
    instances: Sequence[Mapping[str, Any]],
    instance_ids: Sequence[str] | None,
    isolated_micro_qualification: bool,
) -> list[tuple[int, Mapping[str, Any]]]:
    indexed_instances = list(enumerate(instances, start=1))
    requested_instance_ids = list(instance_ids or [])
    if not requested_instance_ids:
        if isolated_micro_qualification:
            raise ToolchainError(
                "--isolated-micro-qualification requires explicit instance IDs"
            )
        return indexed_instances
    if not isolated_micro_qualification:
        raise ToolchainError(
            "an explicit instance subset requires --isolated-micro-qualification"
        )
    if len(requested_instance_ids) != 2 or len(set(requested_instance_ids)) != 2:
        raise ToolchainError(
            "isolated micro-qualification requires exactly two distinct instance IDs"
        )
    positions = {
        instance["instance_id"]: index for index, instance in indexed_instances
    }
    if any(instance_id not in positions for instance_id in requested_instance_ids):
        raise ToolchainError("isolated instance ID is absent from frozen population")
    selected_indexes = [positions[instance_id] for instance_id in requested_instance_ids]
    if selected_indexes != sorted(selected_indexes):
        raise ToolchainError(
            "isolated instance IDs must preserve frozen population order"
        )
    selected_id_set = set(requested_instance_ids)
    selected = [
        (positions[instance["instance_id"]], instance)
        for _, instance in indexed_instances
        if instance["instance_id"] in selected_id_set
    ]
    selected.sort(key=lambda pair: pair[0])
    if len({instance["repo"] for _, instance in selected}) != 1:
        raise ToolchainError(
            "isolated micro-qualification instances must share one repository"
        )
    return selected


def _qualification_scan_command(
    rna_binary: Path, checkout: Path, *, combined_cache: bool
) -> list[str]:
    command = [
        str(rna_binary),
        "--business-context",
        "disabled",
        "scan",
        "--repo",
        str(checkout),
        "--full",
    ]
    if not combined_cache:
        command.append("--no-embed")
    command.append("--timings")
    return command


def _scan_phase_timings(checkout: Path) -> dict[str, int | str]:
    store = load_json_object(
        checkout / ".oh/.cache/operation_reports.json",
        "combined scan operation report store",
    )
    reports = store.get("reports")
    if not isinstance(reports, list):
        raise ToolchainError("combined scan operation report store has no reports")
    candidates = [
        report
        for report in reports
        if isinstance(report, dict)
        and report.get("operation") in {"scan", "full_rebuild", "incremental_refresh"}
        and report.get("state") == "completed"
    ]
    if not candidates:
        raise ToolchainError("combined scan has no completed persisted operation report")
    report = candidates[-1]
    total_ms = report.get("duration_ms")
    phases = report.get("phases")
    if type(total_ms) is not int or total_ms < 0 or not isinstance(phases, list):
        raise ToolchainError("combined scan persisted timing evidence is malformed")
    embedding_phases = [
        phase
        for phase in phases
        if isinstance(phase, dict) and phase.get("phase") == "embeddings"
    ]
    if len(embedding_phases) != 1:
        raise ToolchainError("combined scan must persist one embeddings timing phase")
    embedding_phase = embedding_phases[0]
    semantic_ms = embedding_phase.get("duration_ms")
    if embedding_phase.get("state") != "ran" or type(semantic_ms) is not int:
        raise ToolchainError("combined scan embeddings phase did not run with timing evidence")
    if semantic_ms < 0 or semantic_ms > total_ms:
        raise ToolchainError("combined scan embeddings timing exceeds total scan time")
    persistence_phases = [
        phase
        for phase in phases
        if isinstance(phase, dict) and phase.get("phase") == "persist_graph"
    ]
    if len(persistence_phases) != 1:
        raise ToolchainError("combined scan must persist one graph-persistence timing phase")
    persistence_phase = persistence_phases[0]
    persistence_ms = persistence_phase.get("duration_ms")
    if persistence_phase.get("state") != "ran" or type(persistence_ms) is not int:
        raise ToolchainError("combined scan graph-persistence phase did not run")
    if persistence_ms < 0 or persistence_ms > total_ms:
        raise ToolchainError("combined scan persistence timing exceeds total scan time")
    return {
        "operation_id": _require_string(
            report.get("operation_id"), "combined scan operation ID"
        ),
        "total_ms": total_ms,
        "structural_ms": total_ms - semantic_ms,
        "semantic_ms": semantic_ms,
        "persistence_ms": persistence_ms,
    }


def _structural_projection_checkout(checkout: Path, destination: Path) -> Path:
    """Hardlink-copy only structural cache bytes for #785's unchanged archiver."""
    if destination.exists() or destination.is_symlink():
        raise ToolchainError("structural projection checkout destination already exists")
    source_cache = checkout / ".oh/.cache"
    if not source_cache.is_dir() or source_cache.is_symlink():
        raise ToolchainError("combined checkout cache is missing or is a symlink")
    destination_cache = destination / ".oh/.cache"
    destination_cache.parent.mkdir(parents=True)

    def ignore_semantic_root(directory: str, names: list[str]) -> set[str]:
        if Path(directory) == source_cache and "embeddings" in names:
            return {"embeddings"}
        return set()

    shutil.copytree(
        source_cache,
        destination_cache,
        symlinks=True,
        copy_function=os.link,
        ignore=ignore_semantic_root,
    )
    if (destination_cache / "embeddings").exists() or (
        destination_cache / "embeddings"
    ).is_symlink():
        raise ToolchainError("structural projection retained semantic cache bytes")
    return destination


def _require_resumed_combined_cache(
    combined: Any,
    *,
    output_root: Path,
    cohort_case: Mapping[str, Any],
    case_index: int,
    instance: Mapping[str, Any],
    runtime: Mapping[str, Any],
) -> dict[str, Any]:
    receipt_path = Path(
        _require_string(cohort_case.get("receipt_path"), "combined resume receipt path")
    )
    receipt = load_json_object(receipt_path, "combined resume case receipt")
    combined_receipt = receipt.get("combined_cache")
    if not isinstance(combined_receipt, dict):
        raise ToolchainError("combined resume receipt has no combined cache identity")
    validated = combined._validate_receipt(combined_receipt, verify_bytes=True)
    catalog = combined.load_combined_cache_catalog(output_root)
    if validated not in catalog["entries"]:
        raise ToolchainError("combined resume receipt is absent from the immutable catalog")
    verified = combined.verify_combined_cache_archive(
        Path(validated["archive_path"]),
        Path(validated["sidecar_path"]),
        expected={
            "repository": instance["repo"],
            "commit": instance["base_commit"],
            "scan_flags": COMBINED_QUALIFICATION_SCAN_FLAGS,
            "runtime": runtime,
        },
    )
    if (
        validated["case"]["case_index"] != case_index
        or validated["case"]["instance_id"] != instance["instance_id"]
        or verified["core"]["case"] != validated["case"]
    ):
        raise ToolchainError("combined resume cache is not bound to the frozen case")
    return validated


def _latest_verified_combined_case(
    combined: Any,
    *,
    output_root: Path,
    case_index: int,
    instance: Mapping[str, Any],
    runtime: Mapping[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    catalog = combined.load_combined_cache_catalog(output_root)
    matches = [
        entry
        for entry in catalog["entries"]
        if entry["case"]["case_index"] == case_index
        and entry["case"]["instance_id"] == instance["instance_id"]
        and entry["repository"] == instance["repo"]
        and entry["commit"] == instance["base_commit"]
    ]
    if not matches:
        raise ToolchainError(
            f"combined qualification case {case_index} has no verifier-clean READY cache"
        )
    matches.sort(key=lambda entry: entry["case"]["attempt_index"])
    receipt = combined._validate_receipt(matches[-1], verify_bytes=True)
    verified = combined.verify_combined_cache_archive(
        Path(receipt["archive_path"]),
        Path(receipt["sidecar_path"]),
        expected={
            "repository": instance["repo"],
            "commit": instance["base_commit"],
            "scan_flags": COMBINED_QUALIFICATION_SCAN_FLAGS,
            "runtime": runtime,
        },
    )
    if verified["core"]["case"] != receipt["case"]:
        raise ToolchainError(
            f"combined qualification case {case_index} receipt/archive identity mismatch"
        )
    return receipt, verified


def _require_actual_combined_incremental_lineage(
    combined: Any,
    first_receipt: Mapping[str, Any],
    first_verified: Mapping[str, Any],
    second_receipt: Mapping[str, Any],
    second_verified: Mapping[str, Any],
) -> None:
    first_core = first_verified["core"]
    second_core = second_verified["core"]
    if (
        first_receipt.get("base_combined_cache") is not None
        or first_core.get("base_combined_cache") is not None
    ):
        raise ToolchainError("combined qualification case 1 must be a cold cache")
    expected_base = combined.combined_base_identity(first_verified)
    second_bases = (
        second_receipt.get("base_combined_cache"),
        second_core.get("base_combined_cache"),
    )
    if any(base is None for base in second_bases):
        raise ToolchainError(
            "combined qualification case 2 has null case-1 cache lineage"
        )
    if any(base != expected_base for base in second_bases):
        raise ToolchainError(
            "combined qualification case 2 names the wrong immutable case-1 cache"
        )
    work = second_core["work"]
    if (
        work["structural_inherited_file_count"] <= 0
        or work["structural_inherited_operation_count"] <= 0
        or work["vector_inherited_count"] <= 0
    ):
        raise ToolchainError(
            "combined qualification case 2 is cold instead of incrementally inherited"
        )


def _require_combined_case2_selection(
    combined: Any,
    *,
    output_root: Path,
    first_case_index: int,
    first_instance: Mapping[str, Any],
    runtime: Mapping[str, Any],
    selection: Mapping[str, Any] | None,
) -> None:
    first_receipt, first_verified = _latest_verified_combined_case(
        combined,
        output_root=output_root,
        case_index=first_case_index,
        instance=first_instance,
        runtime=runtime,
    )
    if (
        first_receipt["base_combined_cache"] is not None
        or first_verified["core"]["base_combined_cache"] is not None
    ):
        raise ToolchainError("combined qualification case 1 must be a cold cache")
    if selection is None:
        raise ToolchainError(
            "combined qualification case 2 cannot cold-rebuild without case-1 lineage"
        )
    expected_base = combined.combined_base_identity(first_verified)
    if (
        selection.get("base_combined_cache") != expected_base
        or combined.combined_base_identity(selection["verified"]) != expected_base
        or selection["receipt"] != first_receipt
    ):
        raise ToolchainError(
            "combined qualification case 2 selected the wrong immutable case-1 cache"
        )


def _require_combined_pair_ready(
    combined: Any,
    *,
    output_root: Path,
    indexed_instances: Sequence[tuple[int, Mapping[str, Any]]],
    runtime: Mapping[str, Any],
) -> None:
    if len(indexed_instances) < 2:
        raise ToolchainError("combined qualification has no frozen case-1/case-2 pair")
    first_case = _latest_verified_combined_case(
        combined,
        output_root=output_root,
        case_index=indexed_instances[0][0],
        instance=indexed_instances[0][1],
        runtime=runtime,
    )
    second_case = _latest_verified_combined_case(
        combined,
        output_root=output_root,
        case_index=indexed_instances[1][0],
        instance=indexed_instances[1][1],
        runtime=runtime,
    )
    _require_actual_combined_incremental_lineage(
        combined, *first_case, *second_case
    )


def qualify_population(
    lock_path: Path,
    inventory_path: Path,
    population_path: Path,
    git_cache_root: Path,
    toolchain_root: Path,
    rna_binary: Path,
    output_root: Path,
    case_timeout_seconds: float = 1800.0,
    *,
    preflight_case: int | None = None,
    preflight_output: Path | None = None,
    approved_preflight: Path | None = None,
    stop_after_case: int | None = None,
    instance_ids: Sequence[str] | None = None,
    isolated_micro_qualification: bool = False,
    repo_root: Path | None = None,
    resume_replay_case: int | None = None,
    resume_replay_checkout: Path | None = None,
    resume_replay_receipt: Path | None = None,
    resume_replay_receipt_sha256: str | None = None,
    combined_runtime_manifest: Path | None = None,
    combined_bundle_archive: Path | None = None,
    combined_upload_attestation: Path | None = None,
    combined_expected_manifest_sha256: str | None = None,
    combined_expected_upload_attestation_sha256: str | None = None,
    combined_expected_github_artifact_digest: str | None = None,
    combined_expected_head_sha: str | None = None,
) -> dict[str, Any]:
    rna_binary = rna_binary.resolve()
    verification = verify_lock(lock_path, inventory_path, None, None, repo_root)
    if not verification["compatible"]:
        raise ToolchainError("cannot qualify a lock with unsupported languages")
    if combined_runtime_manifest is None and not rna_binary.is_file():
        raise ToolchainError(f"RNA binary is missing: {rna_binary}")
    if not math.isfinite(case_timeout_seconds) or case_timeout_seconds <= 0:
        raise ToolchainError("case timeout must be a positive finite number")
    git_cache_verification = verify_git_cache(population_path, git_cache_root)
    population = load_json_object(population_path, "population")
    instances = included_population(population)
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
    combined = None
    combined_runtime = None
    combined_bundle_root = None
    combined_bundle_verification_receipt = None
    combined_bundle_verification_directory = None
    combined_initialization_seconds = 0.0
    combined_options = (
        combined_runtime_manifest,
        combined_bundle_archive,
        combined_upload_attestation,
        combined_expected_manifest_sha256,
        combined_expected_upload_attestation_sha256,
        combined_expected_github_artifact_digest,
        combined_expected_head_sha,
    )
    if any(value is not None for value in combined_options) and not all(
        value is not None for value in combined_options
    ):
        raise ToolchainError("all combined CI bundle verification options are required")
    if combined_runtime_manifest is not None:
        combined_initialization_started = time.monotonic()
        if isolated_micro_qualification or instance_ids:
            raise ToolchainError(
                "combined qualification must preserve the full frozen population order"
            )
        if resume_replay_case is not None:
            raise ToolchainError(
                "combined qualification cannot publish semantic evidence from structural replay"
            )
        from scripts import swebench_combined_cache as combined_module
        from scripts import verify_swebench_semantic_bundle as bundle_verifier

        combined = combined_module
        combined_runtime_manifest = combined_runtime_manifest.resolve()
        combined_bundle_verification_directory = tempfile.TemporaryDirectory(
            prefix="rna-verified-semantic-bundle-"
        )
        verified_output = (
            Path(combined_bundle_verification_directory.name) / "verified"
        )
        try:
            combined_bundle_verification_receipt = bundle_verifier.verify_bundle(
                archive=combined_bundle_archive.resolve(),
                manifest_path=combined_runtime_manifest,
                upload_attestation_path=combined_upload_attestation.resolve(),
                output=verified_output,
                expected_manifest_sha256=combined_expected_manifest_sha256,
                expected_upload_attestation_sha256=(
                    combined_expected_upload_attestation_sha256
                ),
                expected_github_artifact_digest=(
                    combined_expected_github_artifact_digest
                ),
                expected_head_sha=combined_expected_head_sha,
            )
        except bundle_verifier.BundleVerificationError as error:
            raise ToolchainError(f"combined CI bundle verification failed: {error}") from error
        combined_bundle_root = verified_output / bundle_verifier.ARCHIVE_ROOT
        rna_binary = combined_bundle_root / "repo-native-alignment"
        combined_runtime = combined._project_runtime_manifest(
            combined_runtime_manifest
        )
        runtime_lsp = combined_runtime["projection"]["components"]["lsp"]
        if (
            runtime_lsp["toolchain_lock_sha256"] != toolchain_lock_digest
            or runtime_lsp["inventory_sha256"] != inventory_file_sha256
        ):
            raise ToolchainError(
                "combined CI bundle LSP lock/inventory differs from frozen qualification inputs"
            )
        provisioned_identity = _materialize_verified_bundle_toolchain(
            combined_bundle_root,
            lock_path,
            inventory_path,
            Path(combined_bundle_verification_directory.name)
            / "private-verified-lsp",
            repo_root,
        )
        toolchain_root = Path(provisioned_identity["toolchain_root"])
        combined_initialization_seconds = (
            time.monotonic() - combined_initialization_started
        )
    if not rna_binary.is_file():
        raise ToolchainError(f"RNA binary is missing: {rna_binary}")
    provisioned_identity = _validate_provisioned_toolchain(
        lock_path, inventory_path, toolchain_root
    )
    toolchain_root = Path(provisioned_identity["toolchain_root"])
    indexed_instances = _select_qualification_instances(
        instances,
        instance_ids,
        isolated_micro_qualification,
    )
    if combined is not None and len(indexed_instances) < 2:
        raise ToolchainError("combined qualification requires frozen cases 1 and 2")
    selected_indexes = [index for index, _ in indexed_instances]
    for label, value in (
        ("preflight case", preflight_case),
        ("stop-after case", stop_after_case),
        ("resume replay case", resume_replay_case),
    ):
        if value is not None and value not in selected_indexes:
            raise ToolchainError(f"{label} is not selected for this qualification")
    if preflight_output is not None and preflight_case is None:
        raise ToolchainError("--preflight-output requires --preflight-case")
    if preflight_case is not None and approved_preflight is not None:
        raise ToolchainError("preflight-only and approved-preflight modes are exclusive")
    replay_options = (
        resume_replay_case,
        resume_replay_checkout,
        resume_replay_receipt,
        resume_replay_receipt_sha256,
    )
    if any(value is not None for value in replay_options) and not all(
        value is not None for value in replay_options
    ):
        raise ToolchainError("all resume-replay options are required together")
    if resume_replay_case is not None:
        if approved_preflight is None:
            raise ToolchainError("resume replay requires --approved-preflight")
        if preflight_case is not None or preflight_output is not None:
            raise ToolchainError("resume replay and preflight-only modes are exclusive")
        if stop_after_case != resume_replay_case:
            raise ToolchainError("resume replay must stop after its selected case")
        _require_sha256(
            resume_replay_receipt_sha256, "resume replay receipt SHA-256"
        )
    approved_case_index = None
    if approved_preflight is not None:
        approved_plan = load_json_object(
            approved_preflight, "approved structural-cache preflight"
        )
        approved_case_index = approved_plan.get("case_index")
        if approved_case_index not in selected_indexes:
            raise ToolchainError("approved preflight case is not selected")
        if (
            resume_replay_case is not None
            and approved_case_index != resume_replay_case
        ):
            raise ToolchainError("resume replay and approved preflight cases differ")
    git_binary = shutil.which("git")
    if git_binary is None:
        raise ToolchainError("git is required to materialize frozen checkouts")
    isolation_marker_path = output_root / "isolated-micro-qualification.json"
    if isolated_micro_qualification:
        isolation_marker = {
            "schema_version": SCHEMA_VERSION,
            "status": "isolated",
            "population_sha256": sha256_file(population_path),
            "inventory_sha256": inventory_file_sha256,
            "toolchain_lock_digest": toolchain_lock_digest,
            "selected_cases": [
                {"population_index": index, "instance_id": instance["instance_id"]}
                for index, instance in indexed_instances
            ],
            "digest": "",
        }
        isolation_marker["digest"] = sha256_bytes(canonical_json(isolation_marker))
        if output_root.exists() and not isolation_marker_path.exists() and any(
            output_root.iterdir()
        ):
            raise ToolchainError(
                "isolated micro-qualification output must start empty"
            )
        for forbidden in ("cohort-manifest.json", "aggregate.json", "seal.json"):
            if (output_root / forbidden).exists() or (output_root / forbidden).is_symlink():
                raise ToolchainError(
                    "isolated micro-qualification output contains N=70 evidence"
                )
        output_root.mkdir(parents=True, exist_ok=True)
        _publish_or_verify_canonical_json(isolation_marker_path, isolation_marker)
    else:
        if isolation_marker_path.exists() or isolation_marker_path.is_symlink():
            raise ToolchainError(
                "N=70 qualification cannot use an isolated micro output root"
            )
        output_root.mkdir(parents=True, exist_ok=True)
    combined_bundle_verification_path = None
    if combined is not None:
        combined_bundle_evidence = {
            "schema_version": SCHEMA_VERSION,
            "status": "verified",
            "verification": combined_bundle_verification_receipt,
            "digest": "",
        }
        combined_bundle_evidence["digest"] = sha256_bytes(
            canonical_json(combined_bundle_evidence)
        )
        combined_bundle_verification_path = (
            output_root / "semantic-bundle-verification.json"
        )
        _publish_or_verify_canonical_json(
            combined_bundle_verification_path, combined_bundle_evidence
        )
    cases_root = output_root / "cases"
    logs_root = output_root / "logs"
    cases_root.mkdir(exist_ok=True)
    logs_root.mkdir(exist_ok=True)
    archives_root = output_root / "structural-caches"
    archives_root.mkdir(exist_ok=True)
    combined_archives_root = None
    if combined is not None:
        combined_archives_root = output_root / "combined-caches"
        combined_archives_root.mkdir(exist_ok=True)
    qualification_environment_directory = tempfile.TemporaryDirectory(
        prefix="rna-lsp-qualification-environment-"
    )
    environment = toolchain_environment(
        toolchain_root, Path(qualification_environment_directory.name)
    )
    if combined is not None:
        projection = combined_runtime["projection"]
        embedding = projection["components"]["embedding"]
        reranker = projection["components"]["reranker"]
        environment.update(
            {
                "HF_HOME": str(combined_bundle_root / "components/models/huggingface"),
                "FASTEMBED_CACHE_DIR": str(
                    combined_bundle_root / "components/models/reranker"
                ),
                "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1",
                "CANDLE_METAL_ENABLE_FAST_MATH": "1",
                "RNA_EMBEDDING_MODEL_FILES_DIGEST": embedding["files_digest"],
                "RNA_EMBEDDING_MODEL_SHA256": embedding["assets"][
                    "model.safetensors"
                ]["sha256"],
                "RNA_EMBEDDING_TOKENIZER_SHA256": embedding["assets"][
                    "tokenizer.json"
                ]["sha256"],
                "RNA_RERANKER_MODEL_FILES_DIGEST": reranker["files_digest"],
            }
        )
        bind_hf_default_cache(
            Path(environment["HF_HOME"]), Path(environment["HOME"])
        )
        environment.pop("RNA_SEMANTIC_ASSET_SEEDING", None)
    probe_path = output_root / "probe.json"
    probe_seconds = 0.0
    probe_performed = False
    cohort_cases = []
    timing_cases = []
    for index, instance in indexed_instances:
        if combined is not None and index > indexed_instances[1][0]:
            _require_combined_pair_ready(
                combined,
                output_root=output_root,
                indexed_instances=indexed_instances,
                runtime=combined_runtime,
            )
        instance_id = instance["instance_id"]
        resumed = _resume_ready_case(
            output_root=output_root,
            case_index=index,
            instance=instance,
            inventory_case=inventory_cases[instance_id],
            rna_binary=rna_binary,
            toolchain_lock_digest=toolchain_lock_digest,
            inventory_digest=inventory_digest,
            inventory_file_sha256=inventory_file_sha256,
            receipt_scan_flags=(
                COMBINED_QUALIFICATION_SCAN_FLAGS
                if combined is not None
                else QUALIFICATION_SCAN_FLAGS
            ),
        )
        if resumed is not None:
            if preflight_case == index:
                raise ToolchainError(
                    f"preflight case {index} already has verifier-clean READY evidence"
                )
            cohort_case, timing_case = resumed
            if combined is not None:
                combined_receipt = _require_resumed_combined_cache(
                    combined,
                    output_root=output_root,
                    cohort_case=cohort_case,
                    case_index=index,
                    instance=instance,
                    runtime=combined_runtime,
                )
                cohort_case["combined_archive_sha256"] = combined_receipt[
                    "archive_sha256"
                ]
                timing_case["semantic_update_ms"] = combined_receipt["timings_ms"][
                    "semantic_update_ms"
                ]
                timing_case["combined_cache_archive_ms"] = combined_receipt[
                    "publication_metrics"
                ]["combined_archive_ms"]
                if index == indexed_instances[1][0]:
                    _require_combined_pair_ready(
                        combined,
                        output_root=output_root,
                        indexed_instances=indexed_instances,
                        runtime=combined_runtime,
                    )
            cohort_cases.append(cohort_case)
            timing_cases.append(timing_case)
            if stop_after_case == index:
                return _qualification_checkpoint(
                    output_root=output_root,
                    last_case_index=index,
                    cohort_cases=cohort_cases,
                    timing_cases=timing_cases,
                    isolated=isolated_micro_qualification,
                )
            continue
        if preflight_case is not None and index < preflight_case:
            raise ToolchainError(
                f"preflight case {preflight_case} requires READY prior case {index}"
            )
        if approved_case_index is not None and index < approved_case_index:
            raise ToolchainError(
                f"approved preflight case {approved_case_index} requires READY prior case {index}"
            )
        if resume_replay_case == index:
            cohort_case, timing_case = _publish_resume_replay_case(
                output_root=output_root,
                case_index=index,
                instance=instance,
                inventory_case=inventory_cases[instance_id],
                checkout=resume_replay_checkout,
                replay_receipt_path=resume_replay_receipt,
                replay_receipt_sha256=resume_replay_receipt_sha256,
                approved_preflight_path=approved_preflight,
                rna_binary=rna_binary,
                environment=environment,
                toolchain_lock_digest=toolchain_lock_digest,
                inventory_digest=inventory_digest,
                inventory_file_sha256=inventory_file_sha256,
            )
            cohort_cases.append(cohort_case)
            timing_cases.append(timing_case)
            return _qualification_checkpoint(
                output_root=output_root,
                last_case_index=index,
                cohort_cases=cohort_cases,
                timing_cases=timing_cases,
                isolated=isolated_micro_qualification,
            )
        attempt_index = _next_case_attempt_index(output_root, instance_id)
        attempt_slug = f"{instance_id}-attempt-{attempt_index:03d}"
        case_started = time.monotonic()
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
                actual = run_checked(
                    [git_binary, "rev-parse", "HEAD"],
                    cwd=checkout,
                    environment=environment,
                )
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
                    environment=environment,
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
            combined_selection = None
            combined_injection = None
            try:
                selection_diagnostics: dict[str, Any] = {}
                if combined is None:
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
                        diagnostics=selection_diagnostics,
                    )
                else:
                    combined_catalog = combined.load_combined_cache_catalog(
                        output_root
                    )
                    combined_selection = combined.select_combined_cache(
                        combined_catalog["entries"],
                        instance["repo"],
                        instance["base_commit"],
                        target_identity,
                        git_dir,
                        index,
                        runtime_manifest_path=combined_runtime_manifest,
                        semantic_identity=None,
                        scan_flags=COMBINED_QUALIFICATION_SCAN_FLAGS,
                        toolchain_lock_digest=toolchain_lock_digest,
                        inventory_digest=inventory_digest,
                        inventory_file_sha256=inventory_file_sha256,
                        diagnostics=selection_diagnostics,
                    )
                    if index == indexed_instances[1][0]:
                        _require_combined_case2_selection(
                            combined,
                            output_root=output_root,
                            first_case_index=indexed_instances[0][0],
                            first_instance=indexed_instances[0][1],
                            runtime=combined_runtime,
                            selection=combined_selection,
                        )
                    selection = (
                        combined_selection["structural_selection"]
                        if combined_selection is not None
                        else None
                    )
                _validate_selected_partition_plan(selection, target_identity)
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
            selection_wall_seconds = time.monotonic() - selection_started
            verification_seconds = float(
                selection_diagnostics.get("verification_seconds", 0.0)
            )
            selection_seconds = max(
                0.0, selection_wall_seconds - verification_seconds
            )
            injection_seconds = 0.0
            injection_receipt = None
            if selection is not None:
                try:
                    injection_started = time.monotonic()
                    if combined is None:
                        materialized_cache = (
                            Path(temporary) / "verified-structural-cache"
                        )
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
                        verification_seconds += time.monotonic() - verification_started
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
                    else:
                        combined_injection = combined.inject_combined_cache(
                            combined_selection,
                            checkout,
                            target_identity,
                            git_dir,
                            toolchain_lock_digest=toolchain_lock_digest,
                            inventory_digest=inventory_digest,
                            inventory_file_sha256=inventory_file_sha256,
                        )
                        injection_receipt = combined_injection[
                            "structural_injection"
                        ]
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

            try:
                preflight = build_structural_cache_preflight(
                    case_index=index,
                    instance_id=instance_id,
                    inventory_case=inventory_cases[instance_id],
                    target_identity=target_identity,
                    selection=selection,
                    injection_receipt=injection_receipt,
                    cold_rebuild_reasons=selection_diagnostics.get(
                        "cold_rebuild_reasons"
                    ),
                )
                if preflight_case == index:
                    preflight_path = preflight_output or (
                        logs_root / f"{attempt_slug}-preflight-only.json"
                    )
                    publish_structural_cache_preflight(preflight, preflight_path)
                    return {
                        "schema_version": SCHEMA_VERSION,
                        "status": "preflight_ready",
                        "case_index": index,
                        "instance_id": instance_id,
                        "preflight_path": str(preflight_path.resolve()),
                        "preflight_sha256": sha256_file(preflight_path),
                        "preflight_digest": preflight["digest"],
                    }
                preflight_path = logs_root / f"{attempt_slug}-preflight.json"
                if approved_case_index == index:
                    require_approved_structural_cache_preflight(
                        preflight, approved_preflight
                    )
                publish_structural_cache_preflight(preflight, preflight_path)
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
                    f"cache preflight failed for {instance_id}; "
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
            combined_archive_path = (
                combined_archives_root / f"{attempt_slug}.tar.gz"
                if combined_archives_root is not None
                else None
            )
            combined_sidecar_path = (
                combined_archives_root / f"{attempt_slug}.manifest.json"
                if combined_archives_root is not None
                else None
            )
            combined_archive_receipt = None
            semantic_summary = None
            combined_work = None
            combined_timings = None
            combined_peak_memory = None
            query_evidence = None
            query_evidence_root = logs_root / f"{attempt_slug}-query-evidence"
            case_initialization_seconds = (
                combined_initialization_seconds
                if combined is not None and index == indexed_instances[0][0]
                else 0.0
            )
            initialization_peak_memory = _peak_memory_bytes()
            try:
                probe_pending = not probe_performed
                if probe_pending:
                    current_provisioned_identity = _validate_provisioned_toolchain(
                        lock_path, inventory_path, toolchain_root
                    )
                    if current_provisioned_identity != provisioned_identity:
                        raise ToolchainError(
                            "provisioned toolchain identity changed before probe"
                        )
                    if probe_path.is_symlink():
                        _validate_toolchain_probe_evidence(
                            probe_path, lock_path, provisioned_identity
                        )
                    elif probe_path.is_file():
                        _validate_toolchain_probe_evidence(
                            probe_path, lock_path, provisioned_identity
                        )
                    else:
                        probe_started = time.monotonic()
                        probe_toolchain(
                            lock_path,
                            inventory_path,
                            toolchain_root,
                            probe_path,
                            30.0,
                            repo_root,
                        )
                        probe_seconds = time.monotonic() - probe_started
                    probe_performed = True
                    case_initialization_seconds += probe_seconds
                initialization_peak_memory = _peak_memory_bytes()
                if (
                    _validate_provisioned_toolchain(
                        lock_path, inventory_path, toolchain_root
                    )
                    != provisioned_identity
                ):
                    raise ToolchainError(
                        "provisioned toolchain identity changed before qualification scan"
                    )
                scan_seconds = _run_logged(
                    _qualification_scan_command(
                        rna_binary,
                        checkout,
                        combined_cache=combined is not None,
                    ),
                    checkout,
                    case_environment,
                    scan_log_path,
                    timeout_seconds=case_timeout_seconds,
                    timeout_evidence_path=logs_root
                    / f"{attempt_slug}-scan-timeout.json",
                )
                scan_phase_timings = (
                    _scan_phase_timings(checkout) if combined is not None else None
                )
                scan_peak_memory = _peak_memory_bytes()
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
                if (
                    _validate_provisioned_toolchain(
                        lock_path, inventory_path, toolchain_root
                    )
                    != provisioned_identity
                ):
                    raise ToolchainError(
                        "provisioned toolchain identity changed during "
                        "qualification scan/readiness"
                    )
                execution_counts = execution or {}
                if combined is not None:
                    runtime_projection = combined_runtime["projection"]
                    if (
                        target_identity["producer"]["binary_sha256"]
                        != runtime_projection["components"]["executable_sha256"]
                        or target_identity["producer"]["producer_commit"]
                        != runtime_projection["provenance"]["head_sha"]
                    ):
                        raise ToolchainError(
                            "combined target identity differs from the exact CI producer"
                        )
                    semantic_summary = combined.verify_semantic_cache_root(
                        checkout / ".oh/.cache/embeddings"
                    )
                    if injection_receipt is None:
                        inherited_paths: list[str] = []
                        executed_file_count = preflight["predicted_file_counts"][
                            "executed"
                        ]
                    else:
                        inherited_paths = execution_counts.get("inherited_paths")
                        executed_paths = execution_counts.get("executed_paths")
                        if not isinstance(inherited_paths, list) or not isinstance(
                            executed_paths, list
                        ):
                            raise ToolchainError(
                                "combined structural execution file evidence is malformed"
                            )
                        executed_file_count = len(executed_paths)
                    if combined_injection is None:
                        vector_inherited_count = 0
                        vector_encoded_count = semantic_summary["row_count"]
                        vector_purged_count = 0
                        base_combined_cache = None
                    else:
                        base_combined_cache = combined_injection[
                            "base_combined_cache"
                        ]
                        base_row_count = combined_injection[
                            "base_semantic_row_count"
                        ]
                        if (
                            semantic_summary["generation_digest"]
                            == combined_injection[
                                "base_semantic_generation_digest"
                            ]
                        ):
                            vector_inherited_count = semantic_summary["row_count"]
                            vector_encoded_count = 0
                            vector_purged_count = 0
                        else:
                            vector_inherited_count = semantic_summary[
                                "reused_vector_count"
                            ]
                            vector_encoded_count = semantic_summary[
                                "encoded_vector_count"
                            ]
                            if vector_inherited_count > base_row_count:
                                raise ToolchainError(
                                    "combined semantic reuse exceeds the immutable base"
                                )
                            vector_purged_count = (
                                base_row_count - vector_inherited_count
                            )
                    combined_work = {
                        "structural_inherited_file_count": len(inherited_paths),
                        "structural_executed_file_count": executed_file_count,
                        "structural_invalidated_file_count": execution_counts.get(
                            "invalidated_file_count", 0
                        ),
                        "structural_inherited_operation_count": execution_counts.get(
                            "inherited_graph_enrichment_operation_count", 0
                        ),
                        "structural_executed_operation_count": execution_counts.get(
                            "executed_graph_enrichment_operation_count",
                            cold_executed_graph_enrichment_operation_count,
                        ),
                        "vector_inherited_count": vector_inherited_count,
                        "vector_encoded_count": vector_encoded_count,
                        "vector_purged_count": vector_purged_count,
                    }
                    query_evidence = _run_combined_query_probes(
                        combined=combined,
                        rna_binary=rna_binary,
                        checkout=checkout,
                        environment=case_environment,
                        evidence_root=query_evidence_root,
                        case_identity={
                            "case_index": index,
                            "attempt_index": attempt_index,
                            "instance_id": instance_id,
                        },
                    )
                archive_started = time.monotonic()
                archive_checkout = checkout
                if combined is not None:
                    archive_checkout = _structural_projection_checkout(
                        checkout, Path(temporary) / "structural-archive-checkout"
                    )
                archive_receipt = archive_structural_cache(
                    archive_checkout,
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
                if combined is not None:
                    if query_evidence is None:
                        raise ToolchainError("combined query evidence was not produced")
                    query_probes = query_evidence["probes"]
                    prepublication_total_ms = int(
                        (
                            time.monotonic()
                            - case_started
                            + (
                                combined_initialization_seconds
                                if index == indexed_instances[0][0]
                                else 0.0
                            )
                        )
                        * 1000
                    )
                    combined_timings = {
                        "cache_selection_ms": int(selection_seconds * 1000),
                        "cache_verification_ms": int(verification_seconds * 1000),
                        "cache_injection_ms": int(injection_seconds * 1000),
                        "initialization_ms": int(
                            case_initialization_seconds * 1000
                        ),
                        "structural_update_ms": scan_phase_timings["structural_ms"],
                        "semantic_update_ms": scan_phase_timings["semantic_ms"],
                        "persistence_ms": scan_phase_timings["persistence_ms"],
                        "full_readiness_validation_ms": int(
                            readiness_seconds * 1000
                        ),
                        "query_hybrid_rrf_ms": query_probes[
                            "first_hybrid_rerank"
                        ]["duration_ms"],
                        "query_graph_traversal_ms": query_probes[
                            "graph_traversal"
                        ]["duration_ms"],
                        "query_full_body_ms": query_probes["full_body"][
                            "duration_ms"
                        ],
                        "query_minified_body_ms": query_probes["minified_body"][
                            "duration_ms"
                        ],
                        "query_repeat_stability_ms": (
                            query_probes["repeat_hybrid_1"]["duration_ms"]
                            + query_probes["repeat_hybrid_2"]["duration_ms"]
                        ),
                        "first_query_ttfe_ms": query_probes[
                            "first_hybrid_rerank"
                        ]["ttfe_ms"],
                        "first_rerank_ms": query_probes["first_hybrid_rerank"][
                            "duration_ms"
                        ],
                        "warm_query_ms": query_probes["warm_hybrid_rerank"][
                            "duration_ms"
                        ],
                        "structural_cache_archive_ms": int(
                            archive_seconds * 1000
                        ),
                        "prepublication_total_ms": prepublication_total_ms,
                    }
                    combined_peak_memory = {
                        "initialization_bytes": initialization_peak_memory,
                        "scan_update_bytes": scan_peak_memory,
                        "persistence_bytes": scan_peak_memory,
                        "query_rerank_bytes": query_evidence[
                            "peak_memory_bytes"
                        ],
                    }
                    combined_archive_receipt = combined.archive_combined_cache(
                        archive_path,
                        sidecar_path,
                        checkout / ".oh/.cache/embeddings",
                        combined_runtime_manifest,
                        query_evidence_root,
                        combined_archive_path,
                        combined_sidecar_path,
                        case_identity={
                            "case_index": index,
                            "attempt_index": attempt_index,
                            "instance_id": instance_id,
                        },
                        repository=instance["repo"],
                        commit=instance["base_commit"],
                        tree=target_identity["tree"],
                        scan_flags=COMBINED_QUALIFICATION_SCAN_FLAGS,
                        work=combined_work,
                        timings_ms=combined_timings,
                        peak_memory_bytes=combined_peak_memory,
                        base_combined_cache=base_combined_cache,
                    )
            except Exception as error:
                _raise_archive_failure(
                    checkout=checkout,
                    instance_id=instance_id,
                    cases_root=cases_root,
                    scan_log_path=scan_log_path,
                    error=error,
                    attempt_slug=attempt_slug,
                    archive_path=archive_path,
                    sidecar_path=sidecar_path,
                    additional_artifacts=(
                        (
                            [combined_archive_path, combined_sidecar_path]
                            if combined_archive_path is not None
                            and combined_sidecar_path is not None
                            else []
                        )
                        + (
                            sorted(query_evidence_root.iterdir())
                            if query_evidence_root.is_dir()
                            else []
                        )
                    ),
                )
            publication_artifacts = [archive_path, sidecar_path]
            if combined_archive_path is not None and combined_sidecar_path is not None:
                publication_artifacts.extend(
                    [combined_archive_path, combined_sidecar_path]
                )
            if query_evidence_root.is_dir():
                publication_artifacts.extend(sorted(query_evidence_root.iterdir()))
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
                    "population_index": index,
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
                    "scan_flags": (
                        COMBINED_QUALIFICATION_SCAN_FLAGS
                        if combined is not None
                        else QUALIFICATION_SCAN_FLAGS
                    ),
                    "preflight_path": str(preflight_path.resolve()),
                    "preflight_sha256": sha256_file(preflight_path),
                    "preflight_digest": preflight["digest"],
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
                if combined is not None:
                    case_receipt["combined_cache"] = combined_archive_receipt
                    case_receipt["semantic"] = {
                        "generation_digest": semantic_summary["generation_digest"],
                        "semantic_identity": semantic_summary["semantic_identity"],
                        "semantic_identity_digest": semantic_summary[
                            "semantic_identity_digest"
                        ],
                        "manifest_sha256": semantic_summary["manifest_sha256"],
                        "verification_sha256": semantic_summary[
                            "verification_sha256"
                        ],
                        "row_count": semantic_summary["row_count"],
                        "reused_vector_count": semantic_summary[
                            "reused_vector_count"
                        ],
                        "encoded_vector_count": semantic_summary[
                            "encoded_vector_count"
                        ],
                        "prior_generation_digest": semantic_summary[
                            "prior_generation_digest"
                        ],
                        "work": combined_work,
                        "scan_operation": scan_phase_timings,
                        "runtime_manifest_sha256": combined_runtime[
                            "manifest_sha256"
                        ],
                        "bundle_verification_path": str(
                            combined_bundle_verification_path.resolve()
                        ),
                        "bundle_verification_sha256": sha256_file(
                            combined_bundle_verification_path
                        ),
                        "query_evidence_path": str(
                            query_evidence["receipt_path"].resolve()
                        ),
                        "query_evidence_sha256": query_evidence[
                            "receipt_sha256"
                        ],
                        "query_evidence_digest": query_evidence[
                            "evidence_digest"
                        ],
                        "query_evidence_tree_digest": combined_archive_receipt[
                            "query_evidence_tree_digest"
                        ],
                    }
                    case_receipt["timings_ms"].update(
                        {
                            "initialization": combined_timings[
                                "initialization_ms"
                            ],
                            "structural_update": combined_timings[
                                "structural_update_ms"
                            ],
                            "semantic_update": combined_timings[
                                "semantic_update_ms"
                            ],
                            "persistence": combined_timings["persistence_ms"],
                            "query_hybrid_rrf": combined_timings[
                                "query_hybrid_rrf_ms"
                            ],
                            "query_graph_traversal": combined_timings[
                                "query_graph_traversal_ms"
                            ],
                            "query_full_body": combined_timings[
                                "query_full_body_ms"
                            ],
                            "query_minified_body": combined_timings[
                                "query_minified_body_ms"
                            ],
                            "query_repeat_stability": combined_timings[
                                "query_repeat_stability_ms"
                            ],
                            "first_query_ttfe": combined_timings[
                                "first_query_ttfe_ms"
                            ],
                            "first_rerank": combined_timings[
                                "first_rerank_ms"
                            ],
                            "warm_query": combined_timings["warm_query_ms"],
                            "combined_cache_archive": combined_archive_receipt[
                                "publication_metrics"
                            ]["combined_archive_ms"],
                            "total": combined_archive_receipt[
                                "publication_metrics"
                            ]["total_ms"],
                        }
                    )
                    case_receipt["peak_memory_bytes"] = {
                        **combined_peak_memory,
                        "archive_bytes": combined_archive_receipt[
                            "publication_metrics"
                        ]["archive_peak_memory_bytes"],
                        "total_bytes": combined_archive_receipt[
                            "publication_metrics"
                        ]["total_peak_memory_bytes"],
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
                        "population_index": index,
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
                combined_catalog_path = None
                if combined is not None:
                    combined_catalog_path = combined.publish_combined_cache_receipt(
                        output_root, combined_archive_receipt
                    )
                    publication_artifacts.append(combined_catalog_path)
                    if index == indexed_instances[1][0]:
                        _require_combined_pair_ready(
                            combined,
                            output_root=output_root,
                            indexed_instances=indexed_instances,
                            runtime=combined_runtime,
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
                        **(
                            {
                                "combined_archive_sha256": combined_archive_receipt[
                                    "archive_sha256"
                                ],
                                "combined_sidecar_sha256": combined_archive_receipt[
                                    "sidecar_sha256"
                                ],
                                "semantic_generation_digest": semantic_summary[
                                    "generation_digest"
                                ],
                            }
                            if combined is not None
                            else {}
                        ),
                    }
                )
                timing_cases.append(
                    {
                        "instance_id": instance_id,
                        "population_index": index,
                        "scan_ms": int(scan_seconds * 1000),
                        "readiness_ms": int(readiness_seconds * 1000),
                        "cache_selection_ms": int(selection_seconds * 1000),
                        "cache_verification_ms": int(verification_seconds * 1000),
                        "cache_injection_ms": int(injection_seconds * 1000),
                        "cache_archive_ms": int(archive_seconds * 1000),
                        "report_sha256": sha256_file(report_path),
                        **(
                            {
                                "structural_update_ms": scan_phase_timings[
                                    "structural_ms"
                                ],
                                "semantic_update_ms": scan_phase_timings[
                                    "semantic_ms"
                                ],
                                "persistence_ms": scan_phase_timings[
                                    "persistence_ms"
                                ],
                                "first_query_ttfe_ms": combined_timings[
                                    "first_query_ttfe_ms"
                                ],
                                "first_rerank_ms": combined_timings[
                                    "first_rerank_ms"
                                ],
                                "warm_query_ms": combined_timings[
                                    "warm_query_ms"
                                ],
                                "combined_cache_archive_ms": combined_archive_receipt[
                                    "publication_metrics"
                                ]["combined_archive_ms"],
                                "total_ms": combined_archive_receipt[
                                    "publication_metrics"
                                ]["total_ms"],
                                "peak_memory_bytes": combined_archive_receipt[
                                    "publication_metrics"
                                ]["total_peak_memory_bytes"],
                                "semantic_generation_digest": semantic_summary[
                                    "generation_digest"
                                ],
                            }
                            if combined is not None
                            else {}
                        ),
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
        if stop_after_case == index:
            return _qualification_checkpoint(
                output_root=output_root,
                last_case_index=index,
                cohort_cases=cohort_cases,
                timing_cases=timing_cases,
                isolated=isolated_micro_qualification,
            )

    if isolated_micro_qualification:
        isolated_manifest = {
            "schema_version": SCHEMA_VERSION,
            "status": "isolated_ready",
            "population_sha256": sha256_file(population_path),
            "inventory_sha256": inventory_file_sha256,
            "toolchain_lock_digest": toolchain_lock_digest,
            "isolation_marker_sha256": sha256_file(isolation_marker_path),
            "selected_population_indexes": selected_indexes,
            "cases": cohort_cases,
            "timings": timing_cases,
            "digest": "",
        }
        isolated_manifest["digest"] = sha256_bytes(
            canonical_json(isolated_manifest)
        )
        isolated_manifest_path = output_root / "isolated-micro-manifest.json"
        _publish_or_verify_canonical_json(
            isolated_manifest_path, isolated_manifest
        )
        return {
            **isolated_manifest,
            "path": str(isolated_manifest_path.resolve()),
            "sha256": sha256_file(isolated_manifest_path),
        }

    if (
        _validate_provisioned_toolchain(lock_path, inventory_path, toolchain_root)
        != provisioned_identity
    ):
        raise ToolchainError("provisioned toolchain identity changed before finalization")
    _validate_toolchain_probe_evidence(
        probe_path, lock_path, provisioned_identity
    )
    cohort_manifest = _build_frozen_cohort_manifest(cohort_cases)
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
    with tempfile.TemporaryDirectory(
        prefix="rna-lsp-aggregate-verification-"
    ) as aggregate_verification_directory:
        recomputed_aggregate_path = (
            Path(aggregate_verification_directory) / "aggregate.json"
        )
        _run_logged(
            [
                str(rna_binary),
                "lsp-readiness",
                "--cohort-manifest",
                str(manifest_path),
                "--aggregate-output",
                str(recomputed_aggregate_path),
                "--json",
            ],
            output_root,
            environment,
            logs_root / "aggregate-verification.log",
        )
        recomputed_aggregate = load_json_object(
            recomputed_aggregate_path,
            "independently recomputed aggregate readiness report",
        )
    _validate_ready_aggregate(
        aggregate,
        cohort_manifest,
        recomputed_aggregate=recomputed_aggregate,
        expected_population_digest=sha256_file(population_path),
    )
    timings = {
        "schema_version": SCHEMA_VERSION,
        "cases": timing_cases,
        "server_probe": {
            "path": str(probe_path.resolve()),
            "sha256": sha256_file(probe_path),
            "duration_ms": int(probe_seconds * 1000),
        },
    }
    if combined is not None:
        timings["combined_runtime_manifest_sha256"] = combined_runtime[
            "manifest_sha256"
        ]
        timings["combined_bundle_verification_sha256"] = sha256_file(
            combined_bundle_verification_path
        )
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
    if combined is not None:
        seal["combined_cache_catalog_sha256"] = sha256_file(
            output_root / combined.COMBINED_CACHE_CATALOG
        )
        seal["combined_runtime_manifest_sha256"] = combined_runtime[
            "manifest_sha256"
        ]
        seal["combined_bundle_verification_sha256"] = sha256_file(
            combined_bundle_verification_path
        )
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
    acquire.add_argument("--repo", type=Path, default=Path("."))

    verify = subparsers.add_parser("verify-lock")
    verify.add_argument("--lock", type=Path, required=True)
    verify.add_argument("--inventory", type=Path, required=True)
    verify.add_argument("--cache", type=Path)
    verify.add_argument("--descriptors", type=Path)
    verify.add_argument("--repo", type=Path, default=Path("."))

    seal = subparsers.add_parser("seal-directory")
    seal.add_argument("--source", type=Path, required=True)
    seal.add_argument("--output", type=Path, required=True)
    seal.add_argument("--root-name", required=True)

    bind_hf = subparsers.add_parser("bind-hf-default-cache")
    bind_hf.add_argument("--hf-home", type=Path, required=True)
    bind_hf.add_argument("--home", type=Path, required=True)

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
    provision.add_argument("--repo", type=Path, default=Path("."))

    probe = subparsers.add_parser("probe")
    probe.add_argument("--lock", type=Path, required=True)
    probe.add_argument("--inventory", type=Path, required=True)
    probe.add_argument("--root", type=Path, required=True)
    probe.add_argument("--output", type=Path, required=True)
    probe.add_argument("--timeout", type=float, default=20.0)
    probe.add_argument("--repo", type=Path, default=Path("."))

    qualify = subparsers.add_parser("qualify")
    qualify.add_argument("--lock", type=Path, required=True)
    qualify.add_argument("--inventory", type=Path, required=True)
    qualify.add_argument("--population", type=Path, required=True)
    qualify.add_argument("--git-cache", type=Path, required=True)
    qualify.add_argument("--root", type=Path, required=True)
    qualify.add_argument("--rna", type=Path, required=True)
    qualify.add_argument("--output", type=Path, required=True)
    qualify.add_argument("--case-timeout-seconds", type=float, default=1800.0)
    qualify.add_argument("--preflight-case", type=int)
    qualify.add_argument("--preflight-output", type=Path)
    qualify.add_argument("--approved-preflight", type=Path)
    qualify.add_argument("--stop-after-case", type=int)
    qualify.add_argument("--instance-id", action="append", dest="instance_ids")
    qualify.add_argument("--isolated-micro-qualification", action="store_true")
    qualify.add_argument("--resume-replay-case", type=int)
    qualify.add_argument("--resume-replay-checkout", type=Path)
    qualify.add_argument("--resume-replay-receipt", type=Path)
    qualify.add_argument("--resume-replay-receipt-sha256")
    qualify.add_argument("--combined-runtime-manifest", type=Path)
    qualify.add_argument("--combined-bundle-archive", type=Path)
    qualify.add_argument("--combined-upload-attestation", type=Path)
    qualify.add_argument("--combined-expected-manifest-sha256")
    qualify.add_argument("--combined-expected-upload-attestation-sha256")
    qualify.add_argument("--combined-expected-github-artifact-digest")
    qualify.add_argument("--combined-expected-head-sha")
    qualify.add_argument("--repo", type=Path, default=Path("."))
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
            result = acquire_artifacts(args.lock, args.cache, args.repo)
        elif args.command == "verify-lock":
            result = verify_lock(
                args.lock, args.inventory, args.cache, args.descriptors, args.repo
            )
        elif args.command == "seal-directory":
            result = seal_directory(args.source, args.output, args.root_name)
        elif args.command == "bind-hf-default-cache":
            result = bind_hf_default_cache(args.hf_home, args.home)
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
                repo_root=args.repo,
            )
        elif args.command == "probe":
            result = probe_toolchain(
                args.lock,
                args.inventory,
                args.root,
                args.output,
                args.timeout,
                args.repo,
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
                preflight_case=args.preflight_case,
                preflight_output=args.preflight_output,
                approved_preflight=args.approved_preflight,
                stop_after_case=args.stop_after_case,
                instance_ids=args.instance_ids,
                isolated_micro_qualification=args.isolated_micro_qualification,
                repo_root=args.repo,
                resume_replay_case=args.resume_replay_case,
                resume_replay_checkout=args.resume_replay_checkout,
                resume_replay_receipt=args.resume_replay_receipt,
                resume_replay_receipt_sha256=args.resume_replay_receipt_sha256,
                combined_runtime_manifest=args.combined_runtime_manifest,
                combined_bundle_archive=args.combined_bundle_archive,
                combined_upload_attestation=args.combined_upload_attestation,
                combined_expected_manifest_sha256=(
                    args.combined_expected_manifest_sha256
                ),
                combined_expected_upload_attestation_sha256=(
                    args.combined_expected_upload_attestation_sha256
                ),
                combined_expected_github_artifact_digest=(
                    args.combined_expected_github_artifact_digest
                ),
                combined_expected_head_sha=args.combined_expected_head_sha,
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
