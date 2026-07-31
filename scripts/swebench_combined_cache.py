#!/usr/bin/env python3
"""Deterministic composition of verifier-clean structural and semantic caches.

This module deliberately does not qualify LSP, embeddings, or the CI artifact.
It binds their already-verified immutable outputs into one archive and delegates
all structural verification and target authorization to #785's verifier.
"""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import os
import resource
import shutil
import sys
import tarfile
import tempfile
import time
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence

from scripts import swebench_lsp_toolchain as STRUCTURAL


ToolchainError = STRUCTURAL.ToolchainError


def _apple_silicon_generation(chip: object) -> int | None:
    if not isinstance(chip, str):
        return None
    fields = chip.split()
    if len(fields) < 2 or fields[0] != "Apple":
        return None
    generation = fields[1]
    digits = generation[1:]
    if not generation.startswith("M") or not digits.isascii() or not digits.isdigit():
        return None
    return int(digits)


COMBINED_CACHE_SCHEMA_VERSION = 1
COMBINED_CACHE_ROOT = "combined"
COMBINED_CACHE_CORE = ".rna-combined-cache-core.json"
COMBINED_CACHE_CATALOG = "combined-cache-catalog.json"
SEMANTIC_ROOT = "embeddings"
SEMANTIC_SCHEMA_VERSION = 2
SEMANTIC_SCHEMA_SIGNATURE = (
    "rna.embedding-generation.v2:"
    "id-kind-title-body-text_hash-file_path-language-subsystem-"
    "cyclomatic-vector-f32:value-addressed-vector-input"
)
SEMANTIC_BUNDLE_SCHEMA = "rna-swebench-semantic-bundle-manifest-v1"
EMBEDDING_MODEL_ID = "sentence-transformers/all-MiniLM-L6-v2"
EMBEDDING_TOKENIZER_IDENTITY = (
    "sentence-transformers/all-MiniLM-L6-v2:metal-candle-tokenizer-v1"
)
EMBEDDING_PREPROCESSING_VERSION = (
    "rna-minilm-preprocessing-v2-stable-semantic-metadata-char-budget-650"
)
EMBEDDING_DIMENSION = 384
RERANKER_MODEL_ID = "jinaai/jina-reranker-v1-turbo-en"
FIXED_MTIME = 1_577_836_800
MAX_MEMBERS = STRUCTURAL.STRUCTURAL_CACHE_MAX_MEMBERS + 100_000
MAX_MEMBER_BYTES = STRUCTURAL.STRUCTURAL_CACHE_MAX_MEMBER_BYTES
MAX_TOTAL_BYTES = STRUCTURAL.STRUCTURAL_CACHE_MAX_TOTAL_BYTES * 2

RUNTIME_MANIFEST_MEMBER = "components/runtime/semantic-bundle-manifest.json"
SEMANTIC_MEMBER_ROOT = "components/semantic/embeddings"
QUERY_EVIDENCE_SCHEMA_VERSION = 1
QUERY_EVIDENCE_RECEIPT = "query-probes.json"
QUERY_EVIDENCE_MEMBER_ROOT = "components/evidence/query-probes"
QUERY_PROBE_NAMES = (
    "first_hybrid_rerank",
    "graph_traversal",
    "full_body",
    "minified_body",
    "repeat_hybrid_1",
    "repeat_hybrid_2",
    "warm_hybrid_rerank",
)

WORK_FIELDS = {
    "structural_inherited_file_count",
    "structural_executed_file_count",
    "structural_invalidated_file_count",
    "structural_inherited_operation_count",
    "structural_executed_operation_count",
    "vector_inherited_count",
    "vector_encoded_count",
    "vector_purged_count",
}
TIMING_FIELDS = {
    "cache_selection_ms",
    "cache_verification_ms",
    "cache_injection_ms",
    "initialization_ms",
    "structural_update_ms",
    "semantic_update_ms",
    "persistence_ms",
    "full_readiness_validation_ms",
    "query_hybrid_rrf_ms",
    "query_graph_traversal_ms",
    "query_full_body_ms",
    "query_minified_body_ms",
    "query_repeat_stability_ms",
    "first_query_ttfe_ms",
    "first_rerank_ms",
    "warm_query_ms",
    "structural_cache_archive_ms",
    "prepublication_total_ms",
}
PEAK_MEMORY_FIELDS = {
    "initialization_bytes",
    "scan_update_bytes",
    "persistence_bytes",
    "query_rerank_bytes",
}
PUBLICATION_METRIC_FIELDS = {
    "combined_archive_ms",
    "total_ms",
    "archive_peak_memory_bytes",
    "total_peak_memory_bytes",
}


def semantic_canonical_json(value: Any) -> bytes:
    """Canonical bytes used by the immutable semantic-generation contract."""
    try:
        encoded = json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise ToolchainError("semantic JSON is not canonically serializable") from error
    return encoded + b"\n"


def _require_exact_fields(value: Mapping[str, Any], fields: set[str], label: str) -> None:
    actual = set(value)
    if actual != fields:
        raise ToolchainError(
            f"{label} fields mismatch: missing={sorted(fields - actual)} "
            f"extra={sorted(actual - fields)}"
        )


def _require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ToolchainError(f"{label} must be a non-empty string")
    return value


def _require_sha256(value: Any, label: str) -> str:
    value = _require_string(value, label)
    if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
        raise ToolchainError(f"{label} must be a lowercase SHA-256")
    return value


def _require_git_oid(value: Any, label: str) -> str:
    value = _require_string(value, label)
    if len(value) != 40 or any(character not in "0123456789abcdef" for character in value):
        raise ToolchainError(f"{label} must be a lowercase Git object id")
    return value


def _require_count(value: Any, label: str) -> int:
    if type(value) is not int or value < 0:
        raise ToolchainError(f"{label} must be a non-negative integer")
    return value


def _normalized_path(value: Any, label: str) -> str:
    value = _require_string(value, label)
    if "\0" in value or "\\" in value or value.startswith("/"):
        raise ToolchainError(f"{label} is not a safe relative path")
    pure = PurePosixPath(value)
    if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
        raise ToolchainError(f"{label} is not normalized")
    if pure.parts and len(pure.parts[0]) >= 2 and pure.parts[0][1] == ":":
        raise ToolchainError(f"{label} has a drive prefix")
    if pure.as_posix() != value:
        raise ToolchainError(f"{label} is not canonical")
    return value


def _load_canonical_json(path: Path, label: str, *, semantic: bool) -> dict[str, Any]:
    if not path.is_file() or path.is_symlink():
        raise ToolchainError(f"{label} is missing or is a symlink")
    raw = path.read_bytes()
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ToolchainError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ToolchainError(f"{label} must be a JSON object")
    expected = semantic_canonical_json(value) if semantic else STRUCTURAL.canonical_json(value)
    if raw != expected:
        raise ToolchainError(f"{label} bytes are not canonical")
    return value


def _sha256_semantic(value: Any) -> str:
    return STRUCTURAL.sha256_bytes(semantic_canonical_json(value))


def _regular_tree(root: Path, label: str) -> list[dict[str, Any]]:
    if not root.is_dir() or root.is_symlink():
        raise ToolchainError(f"{label} root must be a real directory")
    members: list[dict[str, Any]] = []
    total = 0
    for path in sorted(root.rglob("*"), key=lambda candidate: candidate.as_posix()):
        relative = _normalized_path(path.relative_to(root).as_posix(), f"{label} member")
        stat_result = path.lstat()
        if path.is_symlink():
            raise ToolchainError(f"{label} contains a symlink: {relative}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ToolchainError(f"{label} contains a special file: {relative}")
        size = stat_result.st_size
        if size > MAX_MEMBER_BYTES:
            raise ToolchainError(f"{label} member is oversized: {relative}")
        total += size
        if total > MAX_TOTAL_BYTES or len(members) >= MAX_MEMBERS:
            raise ToolchainError(f"{label} exceeds archive safety limits")
        members.append(
            {
                "path": relative,
                "size_bytes": size,
                "sha256": STRUCTURAL.sha256_file(path),
                "mode": 0o755 if stat_result.st_mode & 0o111 else 0o644,
            }
        )
    paths = [member["path"] for member in members]
    if len(paths) != len(set(paths)) or len(paths) != len({path.casefold() for path in paths}):
        raise ToolchainError(f"{label} paths collide")
    return members


def _tree_digest(members: Sequence[Mapping[str, Any]]) -> str:
    return STRUCTURAL.sha256_bytes(STRUCTURAL.canonical_json(list(members)))


def _peak_memory_bytes() -> int:
    observed = max(
        resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss,
    )
    return int(observed if sys.platform == "darwin" else observed * 1024)


def _validate_query_evidence_root(root: Path) -> dict[str, Any]:
    members = _regular_tree(root, "combined query evidence")
    by_path = {member["path"]: member for member in members}
    receipt_member = by_path.get(QUERY_EVIDENCE_RECEIPT)
    if receipt_member is None:
        raise ToolchainError("combined query evidence receipt is missing")
    receipt = _load_canonical_json(
        root / QUERY_EVIDENCE_RECEIPT,
        "combined query evidence receipt",
        semantic=False,
    )
    _require_exact_fields(
        receipt,
        {
            "schema_version",
            "status",
            "case",
            "query",
            "retrieval",
            "selected_node_id",
            "strict_sentinel",
            "repeat_stable",
            "probes",
            "peak_memory_bytes",
            "evidence_digest",
        },
        "combined query evidence receipt",
    )
    if (
        receipt["schema_version"] != QUERY_EVIDENCE_SCHEMA_VERSION
        or receipt["status"] != "ready"
        or receipt["query"] != STRUCTURAL.COMBINED_QUERY
        or receipt["retrieval"]
        != {"mode": "hybrid", "fusion": "rrf", "rerank": True}
        or receipt["strict_sentinel"] != STRUCTURAL.COMBINED_STRICT_SEARCH_SENTINEL
        or receipt["repeat_stable"] is not True
    ):
        raise ToolchainError("combined query evidence is not strict READY evidence")
    case = _validate_case_identity(receipt["case"])
    _require_string(receipt["selected_node_id"], "query evidence selected node")
    peak_memory = _require_count(
        receipt["peak_memory_bytes"], "query evidence peak memory"
    )
    if peak_memory <= 0:
        raise ToolchainError("query evidence peak memory must be positive")
    probes = receipt["probes"]
    if not isinstance(probes, dict) or set(probes) != set(QUERY_PROBE_NAMES):
        raise ToolchainError("combined query evidence probe set/order is invalid")
    observed_peak = 0
    for name in QUERY_PROBE_NAMES:
        probe = probes[name]
        if not isinstance(probe, dict):
            raise ToolchainError(f"combined query probe is malformed: {name}")
        _require_exact_fields(
            probe,
            {
                "duration_ms",
                "ttfe_ms",
                "peak_memory_bytes",
                "stdout_file",
                "stdout_sha256",
                "stderr_file",
                "stderr_sha256",
            },
            f"combined query probe {name}",
        )
        duration = _require_count(probe["duration_ms"], f"{name} duration")
        ttfe = _require_count(probe["ttfe_ms"], f"{name} TTFE")
        memory = _require_count(probe["peak_memory_bytes"], f"{name} peak memory")
        if ttfe > duration or memory <= 0:
            raise ToolchainError(f"combined query probe timing/memory is invalid: {name}")
        observed_peak = max(observed_peak, memory)
        for stream in ("stdout", "stderr"):
            filename = _normalized_path(
                probe[f"{stream}_file"], f"{name} {stream} file"
            )
            if PurePosixPath(filename).parent != PurePosixPath("."):
                raise ToolchainError(f"combined query probe output is not flat: {filename}")
            member = by_path.get(filename)
            if member is None or member["sha256"] != _require_sha256(
                probe[f"{stream}_sha256"], f"{name} {stream} digest"
            ):
                raise ToolchainError(f"combined query probe output mismatch: {filename}")
            if stream == "stdout" and member["size_bytes"] <= 0:
                raise ToolchainError(f"combined query probe stdout is empty: {name}")
    if observed_peak != peak_memory:
        raise ToolchainError("combined query evidence peak memory is inconsistent")
    evidence_digest = _require_sha256(
        receipt["evidence_digest"], "combined query evidence digest"
    )
    digest_payload = dict(receipt)
    digest_payload["evidence_digest"] = ""
    if STRUCTURAL.sha256_bytes(STRUCTURAL.canonical_json(digest_payload)) != evidence_digest:
        raise ToolchainError("combined query evidence self-digest mismatch")
    return {
        "schema_version": QUERY_EVIDENCE_SCHEMA_VERSION,
        "status": "ready",
        "case": case,
        "receipt_sha256": receipt_member["sha256"],
        "evidence_digest": evidence_digest,
        "tree_digest": _tree_digest(members),
        "peak_memory_bytes": peak_memory,
        "members": members,
    }


def _lance_tree_digest(members: Sequence[Mapping[str, Any]]) -> str:
    portable = [
        {"path": member["path"], "size": member["size_bytes"], "sha256": member["sha256"]}
        for member in members
    ]
    return _sha256_semantic(portable)


def _validate_semantic_identity(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolchainError("semantic identity must be an object")
    _require_exact_fields(
        value,
        {
            "model",
            "tokenizer",
            "model_files_digest",
            "model_sha256",
            "tokenizer_sha256",
            "reranker_files_digest",
            "preprocessing_version",
            "artifact_sha256",
            "schema_signature",
            "dimension",
            "flags",
        },
        "semantic identity",
    )
    if value["model"] != EMBEDDING_MODEL_ID:
        raise ToolchainError("semantic model is not the frozen encoder")
    if value["tokenizer"] != EMBEDDING_TOKENIZER_IDENTITY:
        raise ToolchainError("semantic tokenizer identity is not frozen")
    if value["preprocessing_version"] != EMBEDDING_PREPROCESSING_VERSION:
        raise ToolchainError("semantic preprocessing version is not frozen")
    _require_sha256(value["artifact_sha256"], "semantic artifact digest")
    for field in (
        "model_files_digest",
        "model_sha256",
        "tokenizer_sha256",
        "reranker_files_digest",
    ):
        _require_sha256(value[field], f"semantic {field}")
    if value["schema_signature"] != SEMANTIC_SCHEMA_SIGNATURE:
        raise ToolchainError("semantic schema signature mismatch")
    if value["dimension"] != EMBEDDING_DIMENSION:
        raise ToolchainError("semantic vector dimension is not frozen")
    if not isinstance(value["flags"], dict) or any(
        not isinstance(key, str)
        or not key
        or not isinstance(flag, str)
        or not flag
        for key, flag in value["flags"].items()
    ):
        raise ToolchainError("semantic flags must be a string map")
    return dict(value)


def verify_semantic_cache_root(root: Path) -> dict[str, Any]:
    """Verify the one active immutable semantic generation independently."""
    all_members = _regular_tree(root, "semantic cache")
    current_path = root / "current.json"
    current = _load_canonical_json(current_path, "semantic current pointer", semantic=True)
    _require_exact_fields(
        current,
        {"schema_version", "generation_digest", "manifest_sha256", "verification_sha256"},
        "semantic current pointer",
    )
    if current["schema_version"] != SEMANTIC_SCHEMA_VERSION:
        raise ToolchainError("semantic current pointer schema mismatch")
    generation_digest = _require_sha256(current["generation_digest"], "generation digest")
    generation_root = root / "generations" / generation_digest
    if not generation_root.is_dir() or generation_root.is_symlink():
        raise ToolchainError("active semantic generation is missing or is a symlink")
    manifest_path = generation_root / "manifest.json"
    coverage_path = generation_root / "coverage.json"
    verification_path = generation_root / "verification.json"
    manifest = _load_canonical_json(manifest_path, "semantic manifest", semantic=True)
    coverage = _load_canonical_json(coverage_path, "semantic coverage", semantic=True)
    verification = _load_canonical_json(
        verification_path, "semantic verification receipt", semantic=True
    )

    manifest_fields = {
        "schema_version",
        "generation_digest",
        "semantic_identity",
        "semantic_identity_digest",
        "canonical_input_digest",
        "target_graph_digest",
        "structural_graph_snapshot_digest",
        "row_count",
        "coverage_digest",
        "lance_tree_digest",
        "reused_vector_count",
        "encoded_vector_count",
        "created_by_artifact_sha256",
        "device_attestation",
    }
    if "prior_generation_digest" in manifest:
        manifest_fields.add("prior_generation_digest")
    _require_exact_fields(manifest, manifest_fields, "semantic manifest")
    if manifest["schema_version"] != SEMANTIC_SCHEMA_VERSION:
        raise ToolchainError("semantic manifest schema mismatch")
    if manifest["generation_digest"] != generation_digest:
        raise ToolchainError("semantic generation directory/manifest mismatch")
    semantic_identity = _validate_semantic_identity(manifest["semantic_identity"])
    semantic_identity_digest = _require_sha256(
        manifest["semantic_identity_digest"], "semantic identity digest"
    )
    if semantic_identity_digest != _sha256_semantic(semantic_identity):
        raise ToolchainError("semantic identity digest mismatch")
    canonical_input_digest = _require_sha256(
        manifest["canonical_input_digest"], "aggregate canonical input digest"
    )
    target_graph_digest = _require_sha256(
        manifest["target_graph_digest"], "semantic target graph digest"
    )
    structural_graph_snapshot_digest = _require_sha256(
        manifest["structural_graph_snapshot_digest"], "structural graph snapshot digest"
    )
    row_count = _require_count(manifest["row_count"], "semantic row count")
    reused_count = _require_count(
        manifest["reused_vector_count"], "reused vector count"
    )
    encoded_count = _require_count(
        manifest["encoded_vector_count"], "encoded vector count"
    )
    if reused_count + encoded_count != row_count:
        raise ToolchainError("semantic reused/encoded counts do not cover the generation")
    _require_sha256(manifest["created_by_artifact_sha256"], "semantic producer artifact")
    device = manifest["device_attestation"]
    if not isinstance(device, dict):
        raise ToolchainError("semantic device attestation is missing")
    _require_exact_fields(
        device,
        {"required_device", "observed_device", "backend", "device_index", "artifact_sha256"},
        "semantic device attestation",
    )
    if (
        device["required_device"] != "metal"
        or device["observed_device"] != "metal"
        or device["backend"] != "candle-metal"
        or device["device_index"] != 0
    ):
        raise ToolchainError("semantic generation did not attest strict Metal execution")
    if _require_sha256(device["artifact_sha256"], "device artifact digest") != semantic_identity[
        "artifact_sha256"
    ]:
        raise ToolchainError("semantic device artifact identity mismatch")
    prior = manifest.get("prior_generation_digest")
    if prior is not None:
        _require_sha256(prior, "prior semantic generation digest")

    expected_generation_digest = _sha256_semantic(
        {
            "schema_version": SEMANTIC_SCHEMA_VERSION,
            "semantic_identity": semantic_identity,
            "canonical_input_digest": canonical_input_digest,
            "target_graph_digest": target_graph_digest,
            "structural_graph_snapshot_digest": structural_graph_snapshot_digest,
        }
    )
    if generation_digest != expected_generation_digest:
        raise ToolchainError("semantic generation digest mismatch")

    _require_exact_fields(
        coverage,
        {
            "schema_version",
            "generation_digest",
            "semantic_identity_digest",
            "canonical_input_digest",
            "target_graph_digest",
            "structural_graph_snapshot_digest",
            "row_count",
            "rows",
        },
        "semantic coverage",
    )
    if coverage["schema_version"] != SEMANTIC_SCHEMA_VERSION:
        raise ToolchainError("semantic coverage schema mismatch")
    for field, expected in {
        "generation_digest": generation_digest,
        "semantic_identity_digest": semantic_identity_digest,
        "canonical_input_digest": canonical_input_digest,
        "target_graph_digest": target_graph_digest,
        "structural_graph_snapshot_digest": structural_graph_snapshot_digest,
        "row_count": row_count,
    }.items():
        if coverage[field] != expected:
            raise ToolchainError(f"semantic coverage {field} mismatch")
    rows = coverage["rows"]
    if not isinstance(rows, list) or len(rows) != row_count:
        raise ToolchainError("semantic coverage row count mismatch")
    ids: list[str] = []
    canonical_inputs: list[dict[str, str]] = []
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise ToolchainError(f"semantic coverage row {index} is not an object")
        _require_exact_fields(
            row, {"id", "canonical_input_digest", "vector_sha256"}, f"coverage row {index}"
        )
        stable_id = _require_string(row["id"], f"coverage row {index} id")
        input_digest = _require_sha256(
            row["canonical_input_digest"], f"coverage row {index} input digest"
        )
        _require_sha256(row["vector_sha256"], f"coverage row {index} vector digest")
        ids.append(stable_id)
        canonical_inputs.append({"id": stable_id, "canonical_input_digest": input_digest})
    if ids != sorted(ids) or len(ids) != len(set(ids)):
        raise ToolchainError("semantic coverage rows are not uniquely sorted by stable id")
    if _sha256_semantic(canonical_inputs) != canonical_input_digest:
        raise ToolchainError("semantic aggregate canonical input digest mismatch")
    coverage_digest = STRUCTURAL.sha256_file(coverage_path)
    if manifest["coverage_digest"] != coverage_digest:
        raise ToolchainError("semantic manifest coverage digest mismatch")

    lance_root = generation_root / "lance"
    lance_members = _regular_tree(lance_root, "semantic Lance generation")
    if not lance_members:
        raise ToolchainError("semantic Lance generation is empty")
    lance_tree_digest = _lance_tree_digest(lance_members)
    if manifest["lance_tree_digest"] != lance_tree_digest:
        raise ToolchainError("semantic Lance tree digest mismatch")

    _require_exact_fields(
        verification,
        {
            "schema_version",
            "generation_digest",
            "manifest_sha256",
            "coverage_digest",
            "lance_tree_digest",
            "structural_graph_snapshot_digest",
            "target_graph_digest",
            "row_count",
            "one_to_one_coverage",
            "fresh_reopen_ready",
        },
        "semantic verification receipt",
    )
    if verification["schema_version"] != SEMANTIC_SCHEMA_VERSION:
        raise ToolchainError("semantic verification receipt schema mismatch")
    manifest_sha256 = STRUCTURAL.sha256_file(manifest_path)
    verification_sha256 = STRUCTURAL.sha256_file(verification_path)
    comparisons = {
        "generation_digest": generation_digest,
        "manifest_sha256": manifest_sha256,
        "coverage_digest": coverage_digest,
        "lance_tree_digest": lance_tree_digest,
        "structural_graph_snapshot_digest": structural_graph_snapshot_digest,
        "target_graph_digest": target_graph_digest,
        "row_count": row_count,
        "one_to_one_coverage": True,
        "fresh_reopen_ready": True,
    }
    for field, expected in comparisons.items():
        if verification[field] != expected:
            raise ToolchainError(f"semantic verification {field} mismatch")
    if current["manifest_sha256"] != manifest_sha256:
        raise ToolchainError("semantic current pointer manifest digest mismatch")
    if current["verification_sha256"] != verification_sha256:
        raise ToolchainError("semantic current pointer verification digest mismatch")

    expected_paths = {
        "current.json",
        f"generations/{generation_digest}/manifest.json",
        f"generations/{generation_digest}/coverage.json",
        f"generations/{generation_digest}/verification.json",
    } | {
        f"generations/{generation_digest}/lance/{member['path']}"
        for member in lance_members
    }
    members = [member for member in all_members if member["path"] in expected_paths]
    if {member["path"] for member in members} != expected_paths:
        raise ToolchainError("semantic cache active generation is partial")
    # Prior immutable generations are allowed in the live source root. They are
    # intentionally not copied into this archive: the immutable base combined
    # archive retains them, while this publication carries one independently
    # verified active projection. Every extra source member still passed the
    # regular-file/symlink/special-file checks above.
    for member in all_members:
        path = member["path"]
        if path in expected_paths:
            continue
        parts = PurePosixPath(path).parts
        if (
            len(parts) < 3
            or parts[0] != "generations"
            or len(parts[1]) != 64
            or any(character not in "0123456789abcdef" for character in parts[1])
            or parts[1] == generation_digest
        ):
            raise ToolchainError("semantic cache contains an undeclared non-generation member")
    return {
        "generation_digest": generation_digest,
        "semantic_identity": semantic_identity,
        "semantic_identity_digest": semantic_identity_digest,
        "manifest_sha256": manifest_sha256,
        "coverage_digest": coverage_digest,
        "verification_sha256": verification_sha256,
        "lance_tree_digest": lance_tree_digest,
        "structural_graph_snapshot_digest": structural_graph_snapshot_digest,
        "target_graph_digest": target_graph_digest,
        "row_count": row_count,
        "reused_vector_count": reused_count,
        "encoded_vector_count": encoded_count,
        "prior_generation_digest": prior,
        "created_by_artifact_sha256": manifest["created_by_artifact_sha256"],
        "members": members,
        "semantic_cache_tree_digest": _tree_digest(members),
    }


def _project_runtime_manifest(path: Path) -> dict[str, Any]:
    manifest = _load_canonical_json(path, "qualified semantic bundle manifest", semantic=False)
    _require_exact_fields(
        manifest,
        {
            "schema",
            "artifact",
            "provenance",
            "build",
            "host",
            "components",
            "qualification",
        },
        "qualified semantic bundle manifest",
    )
    if manifest.get("schema") != SEMANTIC_BUNDLE_SCHEMA:
        raise ToolchainError("qualified semantic bundle manifest schema mismatch")

    def object_at(value: Mapping[str, Any], key: str, label: str) -> Mapping[str, Any]:
        nested = value.get(key)
        if not isinstance(nested, dict):
            raise ToolchainError(f"qualified bundle {label} is missing")
        return nested

    artifact = object_at(manifest, "artifact", "artifact")
    provenance = object_at(manifest, "provenance", "provenance")
    host = object_at(manifest, "host", "host attestation")
    build = object_at(manifest, "build", "build")
    components = object_at(manifest, "components", "components")
    _require_exact_fields(
        artifact, {"name", "archive_file", "archive_sha256"}, "qualified artifact"
    )
    _require_exact_fields(
        provenance,
        {"repository", "head_sha", "workflow", "job", "run_id", "run_attempt"},
        "qualified provenance",
    )
    _require_exact_fields(
        host,
        {"architecture", "chip", "metal_device_observed", "system_profiler_sha256"},
        "qualified host",
    )
    _require_exact_fields(
        build,
        {
            "target",
            "target_cpu",
            "features",
            "profile",
            "rustc",
            "cargo",
            "rustflags",
            "link_flags",
            "metal_kernel_profile",
            "candle_metal_enable_fast_math",
            "metal_kernel_compilation",
        },
        "qualified build",
    )
    _require_exact_fields(
        components,
        {"executable", "embedding", "reranker", "lsp"},
        "qualified components",
    )
    executable = object_at(components, "executable", "executable")
    _require_exact_fields(executable, {"path", "sha256"}, "qualified executable")
    embedding = object_at(components, "embedding", "embedding")
    _require_exact_fields(
        embedding,
        {"model_id", "assets", "files", "files_digest"},
        "qualified embedding",
    )
    embedding_assets = embedding.get("assets")
    if not isinstance(embedding_assets, dict):
        raise ToolchainError("qualified embedding named assets are missing")
    _require_exact_fields(
        embedding_assets,
        {"config.json", "tokenizer.json", "model.safetensors"},
        "qualified embedding assets",
    )

    def project_files(value: Any, label: str) -> tuple[list[dict[str, Any]], str]:
        if not isinstance(value, list) or not value:
            raise ToolchainError(f"qualified {label} file inventory is empty")
        records: list[dict[str, Any]] = []
        for index, record in enumerate(value):
            if not isinstance(record, dict):
                raise ToolchainError(f"qualified {label} file {index} is not an object")
            _require_exact_fields(
                record,
                {"path", "size", "sha256"},
                f"qualified {label} file {index}",
            )
            records.append(
                {
                    "path": _normalized_path(
                        record["path"], f"qualified {label} file {index} path"
                    ),
                    "size": _require_count(
                        record["size"], f"qualified {label} file {index} size"
                    ),
                    "sha256": _require_sha256(
                        record["sha256"], f"qualified {label} file {index} digest"
                    ),
                }
            )
        paths = [record["path"] for record in records]
        if paths != sorted(paths, key=lambda candidate: candidate.encode("utf-8")):
            raise ToolchainError(f"qualified {label} file inventory is not sorted")
        if len(paths) != len(set(paths)) or len(paths) != len(
            {candidate.casefold() for candidate in paths}
        ):
            raise ToolchainError(f"qualified {label} file inventory has path collisions")
        return records, STRUCTURAL.sha256_bytes(STRUCTURAL.canonical_json(records))

    embedding_files, embedding_files_digest = project_files(
        embedding.get("files"), "embedding"
    )
    if _require_sha256(
        embedding.get("files_digest"), "embedding files digest"
    ) != embedding_files_digest:
        raise ToolchainError("qualified embedding file inventory digest mismatch")
    projected_assets: dict[str, dict[str, Any]] = {}
    for name in ("config.json", "tokenizer.json", "model.safetensors"):
        asset = embedding_assets.get(name)
        if not isinstance(asset, dict):
            raise ToolchainError(f"qualified embedding asset is missing: {name}")
        _require_exact_fields(asset, {"path", "size", "sha256"}, f"embedding asset {name}")
        asset_path = _normalized_path(asset["path"], f"embedding asset {name} path")
        if PurePosixPath(asset_path).name != name:
            raise ToolchainError(f"qualified embedding asset name mismatch: {name}")
        projected_assets[name] = {
            "path": asset_path,
            "size": _require_count(asset["size"], f"embedding asset {name} size"),
            "sha256": _require_sha256(asset["sha256"], f"embedding asset {name} digest"),
        }
        if projected_assets[name] not in embedding_files:
            raise ToolchainError(
                f"qualified embedding asset is absent from its file inventory: {name}"
            )
    reranker = object_at(components, "reranker", "reranker")
    _require_exact_fields(
        reranker, {"model_id", "files", "files_digest"}, "qualified reranker"
    )
    reranker_files, reranker_files_digest = project_files(
        reranker.get("files"), "reranker"
    )
    if _require_sha256(
        reranker.get("files_digest"), "reranker files digest"
    ) != reranker_files_digest:
        raise ToolchainError("qualified reranker file inventory digest mismatch")
    lsp = object_at(components, "lsp", "LSP")
    _require_exact_fields(
        lsp,
        {
            "toolchain_lock_sha256",
            "inventory_sha256",
            "descriptor_inventory_sha256",
            "provision_receipt_sha256",
            "probe_receipt_sha256",
            "files",
            "files_digest",
        },
        "qualified LSP",
    )
    lsp_files, lsp_files_digest = project_files(lsp.get("files"), "LSP")
    if _require_sha256(lsp.get("files_digest"), "LSP files digest") != lsp_files_digest:
        raise ToolchainError("qualified LSP file inventory digest mismatch")
    lsp_by_path = {record["path"]: record for record in lsp_files}
    for manifest_field, member_path in (
        ("toolchain_lock_sha256", "toolchain-lock.json"),
        ("inventory_sha256", "inventory.json"),
        ("descriptor_inventory_sha256", "descriptor-inventory.json"),
        ("provision_receipt_sha256", "provision-receipt.json"),
        ("probe_receipt_sha256", "probe-receipt.json"),
    ):
        if member_path not in lsp_by_path:
            raise ToolchainError(f"qualified LSP inventory omits {member_path}")
        if _require_sha256(lsp.get(manifest_field), f"LSP {manifest_field}") != lsp_by_path[
            member_path
        ]["sha256"]:
            raise ToolchainError(f"qualified LSP identity differs from {member_path}")
    qualification = object_at(manifest, "qualification", "qualification")
    _require_exact_fields(
        qualification,
        {
            "strict_mode",
            "offline",
            "embeddings",
            "rerank",
            "metal",
            "lsp",
            "fallbacks",
            "evidence_sha256",
            "lsp_readiness_sha256",
        },
        "qualified bundle qualification",
    )
    projection = {
        "artifact": {
            "name": _require_string(artifact.get("name"), "CI artifact name"),
            "archive_file": _require_string(
                artifact.get("archive_file"), "CI artifact archive file"
            ),
            "archive_sha256": _require_sha256(
                artifact.get("archive_sha256"), "CI artifact archive digest"
            ),
        },
        "provenance": {
            "repository": _require_string(
                provenance.get("repository"), "CI repository"
            ),
            "head_sha": _require_git_oid(provenance.get("head_sha"), "CI head SHA"),
            "workflow": _require_string(provenance.get("workflow"), "CI workflow"),
            "job": _require_string(provenance.get("job"), "CI job"),
            "run_id": _require_count(provenance.get("run_id"), "CI run id"),
            "run_attempt": _require_count(
                provenance.get("run_attempt"), "CI run attempt"
            ),
        },
        "host": {
            "architecture": host.get("architecture"),
            "chip": host.get("chip"),
            "metal_device_observed": host.get("metal_device_observed"),
            "system_profiler_sha256": host.get("system_profiler_sha256"),
        },
        "build": {field: build.get(field) for field in (
            "target", "target_cpu", "features", "profile", "rustc", "cargo", "rustflags",
            "link_flags", "metal_kernel_profile", "candle_metal_enable_fast_math",
            "metal_kernel_compilation",
        )},
        "components": {
            "executable_path": _require_string(
                executable.get("path"), "qualified executable path"
            ),
            "executable_sha256": _require_sha256(
                executable.get("sha256"), "qualified executable digest"
            ),
            "embedding": {
                "model_id": _require_string(embedding.get("model_id"), "embedding model id"),
                "files": embedding_files,
                "files_digest": embedding_files_digest,
                "assets": projected_assets,
            },
            "reranker": {
                "model_id": _require_string(reranker.get("model_id"), "reranker model id"),
                "files": reranker_files,
                "files_digest": reranker_files_digest,
            },
            "lsp": {
                field: _require_sha256(lsp.get(field), f"LSP {field}")
                for field in (
                    "toolchain_lock_sha256", "inventory_sha256",
                    "descriptor_inventory_sha256", "provision_receipt_sha256",
                    "probe_receipt_sha256", "files_digest",
                )
            }
            | {"files": lsp_files},
        },
        "qualification": {
            field: qualification.get(field)
            for field in (
                "strict_mode", "offline", "embeddings", "rerank", "metal", "lsp",
                "fallbacks", "evidence_sha256", "lsp_readiness_sha256",
            )
        },
    }
    for field in (
        "target", "target_cpu", "profile", "rustc", "cargo", "rustflags",
        "metal_kernel_profile", "metal_kernel_compilation",
    ):
        _require_string(projection["build"][field], f"build {field}")
    expected_artifact_name = (
        "repo-native-alignment-swebench-semantic-darwin-arm64-apple-m4-"
        + projection["provenance"]["head_sha"]
    )
    if (
        projection["artifact"]["name"] != expected_artifact_name
        or projection["artifact"]["archive_file"]
        != f"{expected_artifact_name}.tar.gz"
    ):
        raise ToolchainError("qualified semantic artifact name is not frozen")
    if (
        projection["provenance"]["repository"]
        != "open-horizon-labs/repo-native-alignment"
        or projection["provenance"]["workflow"]
        != ".github/workflows/swebench-semantic-bundle.yml"
        or projection["provenance"]["job"] != "build-semantic-bundle"
        or projection["provenance"]["run_id"] <= 0
        or projection["provenance"]["run_attempt"] <= 0
    ):
        raise ToolchainError("qualified semantic CI provenance is not frozen")
    if projection["build"]["link_flags"] != ["-Wl,-dead_strip"]:
        raise ToolchainError("qualified link flags are invalid")
    features = projection["build"]["features"]
    if projection["build"]["target"] != "aarch64-apple-darwin" or projection["build"][
        "target_cpu"
    ] != "apple-m4":
        raise ToolchainError("qualified bundle is not the exact macOS arm64 M4 target")
    if features != ["embeddings", "metal", "swebench-semantic-bundle"]:
        raise ToolchainError("qualified build feature set is invalid")
    if projection["components"]["embedding"]["model_id"] != EMBEDDING_MODEL_ID:
        raise ToolchainError("qualified bundle encoder model is not frozen")
    if projection["components"]["reranker"]["model_id"] != RERANKER_MODEL_ID:
        raise ToolchainError("qualified bundle reranker model is not frozen")
    if (
        projection["build"]["profile"] != "release"
        or projection["build"]["metal_kernel_profile"] != "release-fast-math"
        or projection["build"]["candle_metal_enable_fast_math"] is not True
        or projection["build"]["metal_kernel_compilation"]
        != "embedded-source-runtime"
    ):
        raise ToolchainError("qualified Metal build settings are invalid")
    host_generation = _apple_silicon_generation(projection["host"]["chip"])
    if (
        projection["host"]["architecture"] != "arm64"
        or host_generation is None
        or host_generation < 4
        or projection["host"]["metal_device_observed"] is not True
    ):
        raise ToolchainError(
            "qualified host is not an observed Apple M4-or-newer Metal device"
        )
    if projection["components"]["executable_path"] != "repo-native-alignment":
        raise ToolchainError("qualified executable path is invalid")
    _require_sha256(
        projection["host"]["system_profiler_sha256"], "host system profiler digest"
    )
    for field in ("strict_mode", "offline", "embeddings", "rerank", "metal", "lsp"):
        if projection["qualification"][field] is not True:
            raise ToolchainError(f"qualified bundle did not prove {field}")
    if projection["qualification"]["fallbacks"] != []:
        raise ToolchainError("qualified bundle used a fallback")
    _require_sha256(
        projection["qualification"]["evidence_sha256"], "qualification evidence digest"
    )
    _require_sha256(
        projection["qualification"]["lsp_readiness_sha256"],
        "qualification LSP readiness digest",
    )
    return {
        "manifest_sha256": STRUCTURAL.sha256_file(path),
        "projection": projection,
    }


def verify_runtime_bundle_directory(
    runtime_manifest_path: Path, bundle_root: Path
) -> dict[str, Any]:
    """Verify the extracted executable/model/LSP bytes named by the CI sidecar."""
    runtime = _project_runtime_manifest(runtime_manifest_path)
    if not bundle_root.is_dir() or bundle_root.is_symlink():
        raise ToolchainError("qualified semantic bundle root must be a real directory")
    projection = runtime["projection"]
    executable_path = bundle_root / _normalized_path(
        projection["components"]["executable_path"], "qualified executable path"
    )
    if executable_path.is_symlink() or not executable_path.is_file():
        raise ToolchainError("qualified semantic bundle executable is missing")
    if STRUCTURAL.sha256_file(executable_path) != projection["components"][
        "executable_sha256"
    ]:
        raise ToolchainError("qualified semantic bundle executable digest mismatch")

    component_roots = {
        "embedding": bundle_root / "components/models/huggingface",
        "reranker": bundle_root / "components/models/reranker",
        "lsp": bundle_root / "components/lsp",
    }
    for component, component_root in component_roots.items():
        actual = [
            {
                "path": member["path"],
                "size": member["size_bytes"],
                "sha256": member["sha256"],
            }
            for member in _regular_tree(component_root, f"qualified {component} runtime")
        ]
        if actual != projection["components"][component]["files"]:
            raise ToolchainError(
                f"qualified {component} runtime bytes differ from the CI manifest"
            )
    return runtime


def _validate_case_identity(value: Mapping[str, Any]) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolchainError("combined cache case identity must be an object")
    _require_exact_fields(
        value, {"case_index", "attempt_index", "instance_id"}, "combined cache case identity"
    )
    if type(value["case_index"]) is not int or value["case_index"] <= 0:
        raise ToolchainError("combined cache case index is invalid")
    if type(value["attempt_index"]) is not int or value["attempt_index"] <= 0:
        raise ToolchainError("combined cache attempt index is invalid")
    _require_string(value["instance_id"], "combined cache instance id")
    return dict(value)


def _validate_scan_flags(value: Sequence[str]) -> list[str]:
    if (
        not isinstance(value, (list, tuple))
        or not value
        or any(not isinstance(flag, str) or not flag for flag in value)
    ):
        raise ToolchainError("combined cache scan flags are invalid")
    flags = list(value)
    if flags != list(dict.fromkeys(flags)):
        raise ToolchainError("combined cache scan flags contain duplicates")
    if "--no-embed" in flags or not {"scan", "--full", "--timings"}.issubset(flags):
        raise ToolchainError("combined cache scan flags do not enable the full semantic scan")
    return flags


def _validate_counts(value: Mapping[str, Any], fields: set[str], label: str) -> dict[str, int]:
    if not isinstance(value, dict):
        raise ToolchainError(f"{label} must be an object")
    _require_exact_fields(value, fields, label)
    return {field: _require_count(value[field], f"{label} {field}") for field in sorted(fields)}


def _validate_base_identity(value: Mapping[str, Any] | None) -> dict[str, Any] | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise ToolchainError("base combined cache identity must be null or an object")
    fields = {
        "archive_sha256",
        "sidecar_sha256",
        "core_sha256",
        "repository",
        "commit",
        "tree",
        "structural_archive_sha256",
        "semantic_generation_digest",
        "semantic_row_count",
    }
    _require_exact_fields(value, fields, "base combined cache identity")
    for field in (
        "archive_sha256",
        "sidecar_sha256",
        "core_sha256",
        "structural_archive_sha256",
        "semantic_generation_digest",
    ):
        _require_sha256(value[field], f"base combined cache {field}")
    _require_string(value["repository"], "base combined cache repository")
    _require_git_oid(value["commit"], "base combined cache commit")
    _require_git_oid(value["tree"], "base combined cache tree")
    _require_count(value["semantic_row_count"], "base combined cache semantic row count")
    return dict(value)


def _member(path: str, source: Path, *, mode: int | None = None) -> tuple[dict[str, Any], Path]:
    path = _normalized_path(path, "combined cache member")
    if not source.is_file() or source.is_symlink():
        raise ToolchainError(f"combined cache source is missing or is a symlink: {source}")
    stat_result = source.stat()
    size = stat_result.st_size
    if size > MAX_MEMBER_BYTES:
        raise ToolchainError(f"combined cache member is oversized: {path}")
    if mode is None:
        mode = 0o755 if stat_result.st_mode & 0o111 else 0o644
    return (
        {
            "path": path,
            "sha256": STRUCTURAL.sha256_file(source),
            "size_bytes": size,
            "mode": mode,
        },
        source,
    )


def _structural_summary(
    verified: Mapping[str, Any], *, archive_member: str, sidecar_member: str
) -> dict[str, Any]:
    core = verified["core"]
    return {
        "archive_member": archive_member,
        "sidecar_member": sidecar_member,
        "archive_sha256": verified["archive_sha256"],
        "sidecar_sha256": verified["sidecar_sha256"],
        "core_sha256": verified["core_sha256"],
        "structural_cache_tree_digest": verified["structural_cache_tree_digest"],
        "completeness_report_digest": core["completeness_report_digest"],
        "completeness_report_sha256": core["completeness_report_sha256"],
        "graph_snapshot_digest": core["graph_snapshot_digest"],
        "producer": core["producer"],
        "toolchain_lock_digest": core["toolchain_lock_digest"],
        "inventory_digest": core["inventory_digest"],
        "inventory_file_sha256": core["inventory_file_sha256"],
        "case_inventory_digest": core["case_inventory_digest"],
        "configuration_digest": core["configuration_digest"],
        "inventory_policy_digest": core["inventory_policy_digest"],
        "root_slug": core["root_slug"],
        "shared_influence_digest": core["shared_influence_digest"],
        "scan_flags": core["scan_flags"],
        "partition_signatures": core["partition_signatures"],
        "base_cache": core["base_cache"],
    }


def _validate_cross_binding(
    *,
    repository: str,
    commit: str,
    tree: str,
    structural_verified: Mapping[str, Any],
    semantic: Mapping[str, Any],
    runtime: Mapping[str, Any],
    work: Mapping[str, int],
    base: Mapping[str, Any] | None,
) -> None:
    structural_core = structural_verified["core"]
    if (
        structural_core["repository"] != repository
        or structural_core["commit"] != commit
        or structural_core["tree"] != tree
    ):
        raise ToolchainError("combined target differs from verified structural cache")
    if semantic["structural_graph_snapshot_digest"] != structural_core["graph_snapshot_digest"]:
        raise ToolchainError("semantic generation is not bound to the structural graph snapshot")
    projection = runtime["projection"]
    executable_sha256 = projection["components"]["executable_sha256"]
    if not (
        structural_core["producer"]["binary_sha256"]
        == semantic["created_by_artifact_sha256"]
        == semantic["semantic_identity"]["artifact_sha256"]
        == executable_sha256
    ):
        raise ToolchainError("structural, semantic, and qualified executable identities differ")
    if structural_core["producer"]["producer_commit"] != projection["provenance"]["head_sha"]:
        raise ToolchainError("structural producer commit differs from qualified CI head")
    if semantic["semantic_identity"]["model"] != projection["components"]["embedding"]["model_id"]:
        raise ToolchainError("semantic model differs from qualified embedding asset")
    runtime_embedding = projection["components"]["embedding"]
    semantic_identity = semantic["semantic_identity"]
    if semantic_identity["model_files_digest"] != runtime_embedding["files_digest"]:
        raise ToolchainError("semantic encoder tree differs from qualified embedding assets")
    if semantic_identity["model_sha256"] != runtime_embedding["assets"]["model.safetensors"][
        "sha256"
    ]:
        raise ToolchainError("semantic encoder weights differ from qualified asset")
    if semantic_identity["tokenizer_sha256"] != runtime_embedding["assets"]["tokenizer.json"][
        "sha256"
    ]:
        raise ToolchainError("semantic tokenizer differs from qualified asset")
    if semantic_identity["reranker_files_digest"] != projection["components"]["reranker"][
        "files_digest"
    ]:
        raise ToolchainError("semantic reranker tree differs from qualified assets")
    lsp = projection["components"]["lsp"]
    if structural_core["toolchain_lock_digest"] != lsp["toolchain_lock_sha256"]:
        raise ToolchainError("structural LSP lock differs from qualified LSP asset")
    if structural_core["inventory_file_sha256"] != lsp["inventory_sha256"]:
        raise ToolchainError("structural inventory differs from qualified LSP asset")
    prior = semantic["prior_generation_digest"]
    if base is None:
        if (
            prior is not None
            or work["vector_inherited_count"] != 0
            or work["vector_encoded_count"] != semantic["row_count"]
            or work["vector_purged_count"] != 0
            or semantic["reused_vector_count"] != 0
            or semantic["encoded_vector_count"] != semantic["row_count"]
        ):
            raise ToolchainError("cold combined cache unexpectedly inherited semantic work")
    else:
        if base["repository"] != repository:
            raise ToolchainError("base combined cache repository mismatch")
        if semantic["generation_digest"] == base["semantic_generation_digest"]:
            if (
                base["semantic_row_count"] != semantic["row_count"]
                or work["vector_inherited_count"] != semantic["row_count"]
                or work["vector_encoded_count"] != 0
                or work["vector_purged_count"] != 0
            ):
                raise ToolchainError(
                    "unchanged semantic generation did not reuse all vector work"
                )
        else:
            if prior != base["semantic_generation_digest"]:
                raise ToolchainError("semantic prior generation differs from base combined cache")
            if work["vector_inherited_count"] != semantic["reused_vector_count"]:
                raise ToolchainError(
                    "combined receipt inherited-vector count differs from generation"
                )
            if work["vector_encoded_count"] != semantic["encoded_vector_count"]:
                raise ToolchainError(
                    "combined receipt encoded-vector count differs from generation"
                )
            if work["vector_inherited_count"] > base["semantic_row_count"] or work[
                "vector_purged_count"
            ] != base["semantic_row_count"] - work["vector_inherited_count"]:
                raise ToolchainError(
                    "combined receipt purged-vector count differs from immutable base"
                )
        structural_base = structural_core["base_cache"]
        if not isinstance(structural_base, dict) or (
            structural_base["archive_sha256"] != base["structural_archive_sha256"]
        ):
            raise ToolchainError("structural lineage differs from base combined cache")


def _write_combined_archive(
    sources: Sequence[tuple[Mapping[str, Any], Path]],
    core: Mapping[str, Any],
    output: Path,
) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists() or output.is_symlink():
        raise ToolchainError(f"refusing to overwrite combined cache archive: {output}")
    temporary = output.with_name(f".{output.name}.tmp-{os.getpid()}")
    if temporary.exists() or temporary.is_symlink():
        raise ToolchainError("combined cache archive staging path already exists")
    core_bytes = STRUCTURAL.canonical_json(core)
    try:
        with temporary.open("xb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as archive:
                    directories = {COMBINED_CACHE_ROOT}
                    for member, _ in sources:
                        parts = PurePosixPath(member["path"]).parts
                        for length in range(1, len(parts)):
                            directories.add(
                                f"{COMBINED_CACHE_ROOT}/" + "/".join(parts[:length])
                            )
                    for directory in sorted(directories):
                        info = tarfile.TarInfo(directory)
                        info.type = tarfile.DIRTYPE
                        info.mode = 0o755
                        info.uid = info.gid = 0
                        info.mtime = FIXED_MTIME
                        archive.addfile(info)
                    for member, source in sources:
                        info = tarfile.TarInfo(f"{COMBINED_CACHE_ROOT}/{member['path']}")
                        info.size = member["size_bytes"]
                        info.mode = member["mode"]
                        info.uid = info.gid = 0
                        info.mtime = FIXED_MTIME
                        with source.open("rb") as handle:
                            archive.addfile(info, handle)
                    info = tarfile.TarInfo(f"{COMBINED_CACHE_ROOT}/{COMBINED_CACHE_CORE}")
                    info.size = len(core_bytes)
                    info.mode = 0o644
                    info.uid = info.gid = 0
                    info.mtime = FIXED_MTIME
                    archive.addfile(info, io.BytesIO(core_bytes))
        try:
            os.link(temporary, output)
        except FileExistsError as error:
            raise ToolchainError(f"refusing to overwrite combined cache archive: {output}") from error
    finally:
        temporary.unlink(missing_ok=True)


def archive_combined_cache(
    structural_archive_path: Path,
    structural_sidecar_path: Path,
    semantic_root: Path,
    runtime_manifest_path: Path,
    query_evidence_root: Path,
    archive_path: Path,
    sidecar_path: Path,
    *,
    case_identity: Mapping[str, Any],
    repository: str,
    commit: str,
    tree: str,
    scan_flags: Sequence[str],
    work: Mapping[str, Any],
    timings_ms: Mapping[str, Any],
    peak_memory_bytes: Mapping[str, Any],
    base_combined_cache: Mapping[str, Any] | None,
    expected_structural: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Publish a self-contained combined cache without mutating either input."""
    if sidecar_path.exists() or sidecar_path.is_symlink():
        raise ToolchainError(f"refusing to overwrite combined cache sidecar: {sidecar_path}")
    case = _validate_case_identity(case_identity)
    repository = _require_string(repository, "combined cache repository")
    commit = _require_git_oid(commit, "combined cache commit")
    tree = _require_git_oid(tree, "combined cache tree")
    flags = _validate_scan_flags(scan_flags)
    validated_work = _validate_counts(work, WORK_FIELDS, "combined cache work")
    validated_timings = _validate_counts(timings_ms, TIMING_FIELDS, "combined cache timings")
    validated_peak_memory = _validate_counts(
        peak_memory_bytes, PEAK_MEMORY_FIELDS, "combined cache peak memory"
    )
    if any(value <= 0 for value in validated_peak_memory.values()):
        raise ToolchainError("combined cache peak-memory evidence must be positive")
    base = _validate_base_identity(base_combined_cache)

    structural_before = STRUCTURAL.sha256_file(structural_archive_path)
    structural_sidecar_before = STRUCTURAL.sha256_file(structural_sidecar_path)
    structural_verified = STRUCTURAL.verify_structural_cache_archive(
        structural_archive_path, structural_sidecar_path, expected=expected_structural
    )
    semantic = verify_semantic_cache_root(semantic_root)
    runtime = _project_runtime_manifest(runtime_manifest_path)
    query_evidence = _validate_query_evidence_root(query_evidence_root)
    if query_evidence["case"] != case:
        raise ToolchainError("combined query evidence belongs to a different case")
    _validate_cross_binding(
        repository=repository,
        commit=commit,
        tree=tree,
        structural_verified=structural_verified,
        semantic=semantic,
        runtime=runtime,
        work=validated_work,
        base=base,
    )

    archive_name = _normalized_path(structural_archive_path.name, "structural archive name")
    sidecar_name = _normalized_path(structural_sidecar_path.name, "structural sidecar name")
    structural_archive_member = f"components/structural/{archive_name}"
    structural_sidecar_member = f"components/structural/{sidecar_name}"
    if structural_archive_member == structural_sidecar_member:
        raise ToolchainError("structural archive and sidecar names collide")
    sources = [
        _member(structural_archive_member, structural_archive_path, mode=0o644),
        _member(structural_sidecar_member, structural_sidecar_path, mode=0o644),
        _member(RUNTIME_MANIFEST_MEMBER, runtime_manifest_path, mode=0o644),
    ]
    for semantic_member in semantic["members"]:
        sources.append(
            _member(
                f"{SEMANTIC_MEMBER_ROOT}/{semantic_member['path']}",
                semantic_root / semantic_member["path"],
                mode=semantic_member["mode"],
            )
        )
    for evidence_member in query_evidence["members"]:
        sources.append(
            _member(
                f"{QUERY_EVIDENCE_MEMBER_ROOT}/{evidence_member['path']}",
                query_evidence_root / evidence_member["path"],
                mode=evidence_member["mode"],
            )
        )
    sources.sort(key=lambda pair: pair[0]["path"])
    members = [dict(member) for member, _ in sources]
    if len(members) > MAX_MEMBERS or sum(member["size_bytes"] for member in members) > MAX_TOTAL_BYTES:
        raise ToolchainError("combined cache exceeds archive safety limits")
    if len({member["path"].casefold() for member in members}) != len(members):
        raise ToolchainError("combined cache member paths collide")
    semantic_summary = {key: value for key, value in semantic.items() if key != "members"}
    query_evidence_summary = {
        key: value for key, value in query_evidence.items() if key != "members"
    }
    core = {
        "schema_version": COMBINED_CACHE_SCHEMA_VERSION,
        "status": "ready",
        "offline_preprocessing": True,
        "case": case,
        "repository": repository,
        "commit": commit,
        "tree": tree,
        "scan_flags": flags,
        "structural": _structural_summary(
            structural_verified,
            archive_member=structural_archive_member,
            sidecar_member=structural_sidecar_member,
        ),
        "semantic": semantic_summary,
        "runtime": runtime,
        "query_evidence": query_evidence_summary,
        "base_combined_cache": base,
        "work": validated_work,
        "timings_ms": validated_timings,
        "peak_memory_bytes": validated_peak_memory,
        "members": members,
        "combined_cache_tree_digest": _tree_digest(members),
    }
    archive_started = time.monotonic()
    _write_combined_archive(sources, core, archive_path)
    combined_archive_ms = int((time.monotonic() - archive_started) * 1000)
    archive_peak_memory_bytes = _peak_memory_bytes()
    publication_metrics = {
        "combined_archive_ms": combined_archive_ms,
        "total_ms": validated_timings["prepublication_total_ms"]
        + combined_archive_ms,
        "archive_peak_memory_bytes": archive_peak_memory_bytes,
        "total_peak_memory_bytes": max(
            archive_peak_memory_bytes, *validated_peak_memory.values()
        ),
    }
    sidecar = {
        "schema_version": COMBINED_CACHE_SCHEMA_VERSION,
        "publication_status": "ready",
        "archive_name": archive_path.name,
        "archive_size_bytes": archive_path.stat().st_size,
        "archive_sha256": STRUCTURAL.sha256_file(archive_path),
        "core_sha256": STRUCTURAL.sha256_bytes(STRUCTURAL.canonical_json(core)),
        "publication_metrics": publication_metrics,
        "core": core,
    }
    staged_sidecar = sidecar_path.with_name(f".{sidecar_path.name}.verify-{os.getpid()}")
    if staged_sidecar.exists() or staged_sidecar.is_symlink():
        raise ToolchainError("combined cache sidecar verification path already exists")
    try:
        STRUCTURAL._publish_canonical_json_exclusive(staged_sidecar, sidecar)
        verified = verify_combined_cache_archive(archive_path, staged_sidecar)
        try:
            os.link(staged_sidecar, sidecar_path)
        except FileExistsError as error:
            raise ToolchainError(f"refusing to overwrite combined cache sidecar: {sidecar_path}") from error
    finally:
        staged_sidecar.unlink(missing_ok=True)
    if (
        STRUCTURAL.sha256_file(structural_archive_path) != structural_before
        or STRUCTURAL.sha256_file(structural_sidecar_path) != structural_sidecar_before
    ):
        raise ToolchainError("combined publication mutated its immutable structural base")
    return {
        "schema_version": COMBINED_CACHE_SCHEMA_VERSION,
        "status": "ready",
        "case": case,
        "repository": repository,
        "commit": commit,
        "tree": tree,
        "archive_path": str(archive_path.resolve()),
        "archive_sha256": verified["archive_sha256"],
        "archive_size_bytes": verified["archive_size_bytes"],
        "sidecar_path": str(sidecar_path.resolve()),
        "sidecar_sha256": STRUCTURAL.sha256_file(sidecar_path),
        "core_sha256": verified["core_sha256"],
        "structural_archive_sha256": structural_verified["archive_sha256"],
        "semantic_generation_digest": semantic["generation_digest"],
        "semantic_manifest_sha256": semantic["manifest_sha256"],
        "semantic_verification_sha256": semantic["verification_sha256"],
        "runtime_manifest_sha256": runtime["manifest_sha256"],
        "query_evidence_receipt_sha256": query_evidence["receipt_sha256"],
        "query_evidence_digest": query_evidence["evidence_digest"],
        "query_evidence_tree_digest": query_evidence["tree_digest"],
        "base_combined_cache": base,
        "work": validated_work,
        "timings_ms": validated_timings,
        "peak_memory_bytes": validated_peak_memory,
        "publication_metrics": publication_metrics,
    }


def _validate_core(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolchainError("combined cache core must be an object")
    fields = {
        "schema_version",
        "status",
        "offline_preprocessing",
        "case",
        "repository",
        "commit",
        "tree",
        "scan_flags",
        "structural",
        "semantic",
        "runtime",
        "query_evidence",
        "base_combined_cache",
        "work",
        "timings_ms",
        "peak_memory_bytes",
        "members",
        "combined_cache_tree_digest",
    }
    _require_exact_fields(value, fields, "combined cache core")
    if value["schema_version"] != COMBINED_CACHE_SCHEMA_VERSION:
        raise ToolchainError("combined cache core schema mismatch")
    if value["status"] != "ready" or value["offline_preprocessing"] is not True:
        raise ToolchainError("combined cache core is not reusable READY evidence")
    value["case"] = _validate_case_identity(value["case"])
    _require_string(value["repository"], "combined cache repository")
    _require_git_oid(value["commit"], "combined cache commit")
    _require_git_oid(value["tree"], "combined cache tree")
    value["scan_flags"] = _validate_scan_flags(value["scan_flags"])
    if not isinstance(value["structural"], dict):
        raise ToolchainError("combined structural identity must be an object")
    if not isinstance(value["semantic"], dict):
        raise ToolchainError("combined semantic identity must be an object")
    if not isinstance(value["runtime"], dict):
        raise ToolchainError("combined runtime identity must be an object")
    if not isinstance(value["query_evidence"], dict):
        raise ToolchainError("combined query evidence identity must be an object")
    value["base_combined_cache"] = _validate_base_identity(value["base_combined_cache"])
    value["work"] = _validate_counts(value["work"], WORK_FIELDS, "combined cache work")
    value["timings_ms"] = _validate_counts(
        value["timings_ms"], TIMING_FIELDS, "combined cache timings"
    )
    value["peak_memory_bytes"] = _validate_counts(
        value["peak_memory_bytes"], PEAK_MEMORY_FIELDS, "combined cache peak memory"
    )
    if any(count <= 0 for count in value["peak_memory_bytes"].values()):
        raise ToolchainError("combined cache peak-memory evidence must be positive")
    members = value["members"]
    if not isinstance(members, list) or not members:
        raise ToolchainError("combined cache core has no members")
    normalized = []
    seen: set[str] = set()
    seen_folded: set[str] = set()
    total = 0
    for index, member in enumerate(members):
        if not isinstance(member, dict):
            raise ToolchainError(f"combined cache member {index} is not an object")
        _require_exact_fields(
            member, {"path", "sha256", "size_bytes", "mode"}, f"combined member {index}"
        )
        path = _normalized_path(member["path"], f"combined member {index} path")
        if path in seen or path.casefold() in seen_folded:
            raise ToolchainError(f"duplicate/casefold combined member: {path}")
        seen.add(path)
        seen_folded.add(path.casefold())
        _require_sha256(member["sha256"], f"combined member {path} digest")
        size = _require_count(member["size_bytes"], f"combined member {path} size")
        if size > MAX_MEMBER_BYTES or member["mode"] not in {0o644, 0o755}:
            raise ToolchainError(f"combined member metadata is invalid: {path}")
        total += size
        normalized.append(dict(member))
    if normalized != sorted(normalized, key=lambda member: member["path"]):
        raise ToolchainError("combined cache members are not sorted")
    if len(normalized) > MAX_MEMBERS or total > MAX_TOTAL_BYTES:
        raise ToolchainError("combined cache exceeds safety limits")
    structural_archive_member = _normalized_path(
        value["structural"].get("archive_member"), "structural archive member"
    )
    structural_sidecar_member = _normalized_path(
        value["structural"].get("sidecar_member"), "structural sidecar member"
    )
    if (
        not structural_archive_member.startswith("components/structural/")
        or not structural_sidecar_member.startswith("components/structural/")
        or structural_archive_member == structural_sidecar_member
    ):
        raise ToolchainError("combined structural component paths are invalid")
    for required in (
        structural_archive_member,
        structural_sidecar_member,
        RUNTIME_MANIFEST_MEMBER,
        f"{SEMANTIC_MEMBER_ROOT}/current.json",
        f"{QUERY_EVIDENCE_MEMBER_ROOT}/{QUERY_EVIDENCE_RECEIPT}",
    ):
        if required not in seen:
            raise ToolchainError(f"combined cache is missing required member: {required}")
    if _tree_digest(normalized) != _require_sha256(
        value["combined_cache_tree_digest"], "combined cache tree digest"
    ):
        raise ToolchainError("combined cache tree digest mismatch")
    value["members"] = normalized
    return dict(value)


def _validate_sidecar(sidecar_path: Path, archive_path: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    if not sidecar_path.is_file() or sidecar_path.is_symlink():
        raise ToolchainError("combined cache sidecar is missing or is a symlink")
    sidecar = _load_canonical_json(sidecar_path, "combined cache sidecar", semantic=False)
    _require_exact_fields(
        sidecar,
        {
            "schema_version",
            "publication_status",
            "archive_name",
            "archive_size_bytes",
            "archive_sha256",
            "core_sha256",
            "publication_metrics",
            "core",
        },
        "combined cache sidecar",
    )
    if sidecar["schema_version"] != COMBINED_CACHE_SCHEMA_VERSION:
        raise ToolchainError("combined cache sidecar schema mismatch")
    if sidecar["publication_status"] != "ready":
        raise ToolchainError("combined cache sidecar is not a completed publication")
    if sidecar["archive_name"] != archive_path.name:
        raise ToolchainError("combined cache archive name mismatch")
    if not archive_path.is_file() or archive_path.is_symlink():
        raise ToolchainError("combined cache archive is missing or is a symlink")
    size = sidecar["archive_size_bytes"]
    if type(size) is not int or size <= 0 or archive_path.stat().st_size != size:
        raise ToolchainError("combined cache archive is partial or changed size")
    if STRUCTURAL.sha256_file(archive_path) != _require_sha256(
        sidecar["archive_sha256"], "combined archive digest"
    ):
        raise ToolchainError("combined cache archive digest mismatch")
    core = _validate_core(sidecar["core"])
    publication_metrics = _validate_counts(
        sidecar["publication_metrics"],
        PUBLICATION_METRIC_FIELDS,
        "combined cache publication metrics",
    )
    if (
        publication_metrics["total_ms"]
        != core["timings_ms"]["prepublication_total_ms"]
        + publication_metrics["combined_archive_ms"]
        or publication_metrics["archive_peak_memory_bytes"] <= 0
        or publication_metrics["total_peak_memory_bytes"]
        != max(
            publication_metrics["archive_peak_memory_bytes"],
            *core["peak_memory_bytes"].values(),
        )
    ):
        raise ToolchainError("combined cache publication timing/memory is inconsistent")
    if STRUCTURAL.sha256_bytes(STRUCTURAL.canonical_json(core)) != _require_sha256(
        sidecar["core_sha256"], "combined core digest"
    ):
        raise ToolchainError("combined cache core digest mismatch")
    return sidecar, core


def _validated_archive_name(name: str) -> tuple[str, bool]:
    if name == COMBINED_CACHE_ROOT:
        return "", True
    prefix = f"{COMBINED_CACHE_ROOT}/"
    if not name.startswith(prefix):
        raise ToolchainError(f"archive member is outside {COMBINED_CACHE_ROOT}/: {name}")
    return _normalized_path(name[len(prefix) :], "combined archive member"), False


def _extract_verified_archive(
    archive_path: Path,
    sidecar_path: Path,
    staging: Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    sidecar, core = _validate_sidecar(sidecar_path, archive_path)
    declared = {member["path"]: member for member in core["members"]}
    expected_files = set(declared) | {COMBINED_CACHE_CORE}
    expected_directories = {""}
    for path in expected_files:
        parts = PurePosixPath(path).parts
        for length in range(1, len(parts)):
            expected_directories.add("/".join(parts[:length]))
    staging.mkdir(parents=True)
    try:
        archive = tarfile.open(archive_path, mode="r:gz")
    except (tarfile.TarError, OSError) as error:
        raise ToolchainError("combined cache archive is unreadable or partial") from error
    with archive:
        try:
            infos = archive.getmembers()
        except (tarfile.TarError, OSError) as error:
            raise ToolchainError("combined cache archive ended prematurely") from error
        if len(infos) > MAX_MEMBERS + len(expected_directories) + 1:
            raise ToolchainError("combined cache archive contains too many headers")
        seen: set[str] = set()
        seen_folded: set[str] = set()
        actual_files: set[str] = set()
        actual_directories: set[str] = set()
        total = 0
        file_names: set[str] = set()
        for info in infos:
            relative, is_root = _validated_archive_name(info.name)
            if info.name in seen or info.name.casefold() in seen_folded:
                raise ToolchainError(f"duplicate/casefold archive member: {info.name}")
            seen.add(info.name)
            seen_folded.add(info.name.casefold())
            if info.pax_headers or getattr(info, "sparse", None):
                raise ToolchainError(f"archive member uses extended metadata: {info.name}")
            if info.issym() or info.islnk() or info.isdev() or info.isfifo():
                raise ToolchainError(f"archive member is a link or special file: {info.name}")
            if info.isdir():
                if info.mode & 0o777 != 0o755:
                    raise ToolchainError(f"archive directory mode mismatch: {info.name}")
                actual_directories.add("" if is_root else relative)
                continue
            if not info.isfile() or is_root:
                raise ToolchainError(f"archive member is not a regular scoped file: {info.name}")
            file_names.add(relative)
            actual_files.add(relative)
            if relative not in expected_files:
                raise ToolchainError(f"undeclared combined cache member: {relative}")
            declared_member = declared.get(relative)
            expected_size = (
                len(STRUCTURAL.canonical_json(core))
                if relative == COMBINED_CACHE_CORE
                else declared_member["size_bytes"]
            )
            expected_mode = 0o644 if relative == COMBINED_CACHE_CORE else declared_member["mode"]
            if info.size != expected_size or info.mode & 0o777 != expected_mode:
                raise ToolchainError(f"archive member metadata mismatch: {relative}")
            total += info.size
            if info.size > MAX_MEMBER_BYTES or total > MAX_TOTAL_BYTES:
                raise ToolchainError("combined cache archive exceeds safety limits")
        for file_name in file_names:
            prefix = file_name + "/"
            if any(other.startswith(prefix) for other in file_names if other != file_name):
                raise ToolchainError(f"archive file/directory prefix conflict: {file_name}")
        if actual_files != expected_files:
            raise ToolchainError("combined cache archive is partial")
        if actual_directories != expected_directories:
            raise ToolchainError("combined cache archive directory set is incomplete or undeclared")
        for info in infos:
            if not info.isfile():
                continue
            relative, _ = _validated_archive_name(info.name)
            stream = archive.extractfile(info)
            if stream is None:
                raise ToolchainError(f"unable to read archive member: {relative}")
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
                if relative == COMBINED_CACHE_CORE
                else declared[relative]["sha256"]
            )
            if digest.hexdigest() != expected_digest:
                raise ToolchainError(f"archive member digest mismatch: {relative}")
            destination.chmod(0o644 if relative == COMBINED_CACHE_CORE else declared[relative]["mode"])
    embedded = _load_canonical_json(
        staging / COMBINED_CACHE_CORE, "embedded combined cache core", semantic=False
    )
    if embedded != core:
        raise ToolchainError("embedded combined cache core differs from sidecar")
    (staging / COMBINED_CACHE_CORE).unlink()
    actual_members = _regular_tree(staging, "extracted combined cache")
    if actual_members != core["members"] or _tree_digest(actual_members) != core[
        "combined_cache_tree_digest"
    ]:
        raise ToolchainError("extracted combined cache tree differs from core")
    return sidecar, core


def _verify_components(staging: Path, core: Mapping[str, Any]) -> dict[str, Any]:
    archive_member = core["structural"]["archive_member"]
    sidecar_member = core["structural"]["sidecar_member"]
    structural_verified = STRUCTURAL.verify_structural_cache_archive(
        staging / archive_member,
        staging / sidecar_member,
    )
    semantic = verify_semantic_cache_root(staging / SEMANTIC_MEMBER_ROOT)
    runtime = _project_runtime_manifest(staging / RUNTIME_MANIFEST_MEMBER)
    query_evidence = _validate_query_evidence_root(
        staging / QUERY_EVIDENCE_MEMBER_ROOT
    )
    if _structural_summary(
        structural_verified, archive_member=archive_member, sidecar_member=sidecar_member
    ) != core["structural"]:
        raise ToolchainError("embedded structural identity differs from combined core")
    semantic_summary = {key: value for key, value in semantic.items() if key != "members"}
    if semantic_summary != core["semantic"]:
        raise ToolchainError("embedded semantic identity differs from combined core")
    if runtime != core["runtime"]:
        raise ToolchainError("embedded runtime identity differs from combined core")
    query_evidence_summary = {
        key: value for key, value in query_evidence.items() if key != "members"
    }
    if query_evidence_summary != core["query_evidence"]:
        raise ToolchainError("embedded query evidence differs from combined core")
    if query_evidence["case"] != core["case"]:
        raise ToolchainError("embedded query evidence belongs to another case")
    _validate_cross_binding(
        repository=core["repository"],
        commit=core["commit"],
        tree=core["tree"],
        structural_verified=structural_verified,
        semantic=semantic,
        runtime=runtime,
        work=core["work"],
        base=core["base_combined_cache"],
    )
    return {
        "structural": structural_verified,
        "semantic": semantic,
        "runtime": runtime,
        "query_evidence": query_evidence,
    }


def _compose_cache(staging: Path, destination: Path, core: Mapping[str, Any]) -> None:
    if destination.exists() or destination.is_symlink():
        raise ToolchainError("combined cache materialization destination already exists")
    structural_cache = destination.with_name(f".{destination.name}.structural-{os.getpid()}")
    if structural_cache.exists() or structural_cache.is_symlink():
        raise ToolchainError("combined structural materialization path already exists")
    try:
        STRUCTURAL.verify_structural_cache_archive(
            staging / core["structural"]["archive_member"],
            staging / core["structural"]["sidecar_member"],
            materialize_cache=structural_cache,
        )
        semantic_destination = structural_cache / SEMANTIC_ROOT
        if semantic_destination.exists() or semantic_destination.is_symlink():
            raise ToolchainError("structural cache improperly contains semantic payload")
        shutil.copytree(staging / SEMANTIC_MEMBER_ROOT, semantic_destination, symlinks=False)
        structural_cache.replace(destination)
    finally:
        if structural_cache.exists():
            shutil.rmtree(structural_cache)


def verify_combined_cache_archive(
    archive_path: Path,
    sidecar_path: Path,
    *,
    expected: Mapping[str, Any] | None = None,
    inject_checkout: Path | None = None,
    materialize_cache: Path | None = None,
) -> dict[str, Any]:
    """Verify every component and optionally inject a composed isolated copy."""
    archive_before = STRUCTURAL.sha256_file(archive_path) if archive_path.is_file() else None
    sidecar_before = STRUCTURAL.sha256_file(sidecar_path) if sidecar_path.is_file() else None
    with tempfile.TemporaryDirectory(prefix="rna-combined-cache-verify-") as temporary:
        staging = Path(temporary) / "components"
        sidecar, core = _extract_verified_archive(archive_path, sidecar_path, staging)
        components = _verify_components(staging, core)
        if expected is not None:
            comparisons = {
                "repository": core["repository"],
                "commit": core["commit"],
                "tree": core["tree"],
                "scan_flags": core["scan_flags"],
                "runtime": core["runtime"],
                "semantic_identity": core["semantic"]["semantic_identity"],
                "structural": core["structural"],
            }
            for field, actual in comparisons.items():
                if field in expected and expected[field] != actual:
                    raise ToolchainError(f"combined cache {field} mismatch")
        if inject_checkout is not None and materialize_cache is not None:
            raise ToolchainError("choose checkout injection or cache materialization, not both")
        if inject_checkout is not None:
            destination = STRUCTURAL._safe_checkout_cache_destination(inject_checkout)
            temporary_destination = destination.with_name(f".cache.inject-{os.getpid()}")
            _compose_cache(staging, temporary_destination, core)
            temporary_destination.replace(destination)
        if materialize_cache is not None:
            materialize_cache.parent.mkdir(parents=True, exist_ok=True)
            _compose_cache(staging, materialize_cache, core)
    if (
        STRUCTURAL.sha256_file(archive_path) != archive_before
        or STRUCTURAL.sha256_file(sidecar_path) != sidecar_before
    ):
        raise ToolchainError("combined cache verification mutated its immutable base")
    return {
        "core": core,
        "archive_sha256": sidecar["archive_sha256"],
        "archive_size_bytes": sidecar["archive_size_bytes"],
        "sidecar_sha256": sidecar_before,
        "core_sha256": sidecar["core_sha256"],
        "publication_metrics": sidecar["publication_metrics"],
        "combined_cache_tree_digest": core["combined_cache_tree_digest"],
        "structural_archive_sha256": components["structural"]["archive_sha256"],
        "structural_core": components["structural"]["core"],
        "semantic_generation_digest": components["semantic"]["generation_digest"],
        "query_evidence_digest": components["query_evidence"]["evidence_digest"],
    }


def combined_base_identity(verified: Mapping[str, Any]) -> dict[str, Any]:
    core = verified["core"]
    return {
        "archive_sha256": verified["archive_sha256"],
        "sidecar_sha256": verified["sidecar_sha256"],
        "core_sha256": verified["core_sha256"],
        "repository": core["repository"],
        "commit": core["commit"],
        "tree": core["tree"],
        "structural_archive_sha256": verified["structural_archive_sha256"],
        "semantic_generation_digest": verified["semantic_generation_digest"],
        "semantic_row_count": core["semantic"]["row_count"],
    }


def _validate_receipt(value: Mapping[str, Any], *, verify_bytes: bool) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolchainError("combined cache receipt must be an object")
    fields = {
        "schema_version",
        "status",
        "case",
        "repository",
        "commit",
        "tree",
        "archive_path",
        "archive_sha256",
        "archive_size_bytes",
        "sidecar_path",
        "sidecar_sha256",
        "core_sha256",
        "structural_archive_sha256",
        "semantic_generation_digest",
        "semantic_manifest_sha256",
        "semantic_verification_sha256",
        "runtime_manifest_sha256",
        "query_evidence_receipt_sha256",
        "query_evidence_digest",
        "query_evidence_tree_digest",
        "base_combined_cache",
        "work",
        "timings_ms",
        "peak_memory_bytes",
        "publication_metrics",
    }
    _require_exact_fields(value, fields, "combined cache receipt")
    if value["schema_version"] != COMBINED_CACHE_SCHEMA_VERSION or value["status"] != "ready":
        raise ToolchainError("combined cache receipt is not READY schema v1")
    value = dict(value)
    value["case"] = _validate_case_identity(value["case"])
    _require_string(value["repository"], "receipt repository")
    _require_git_oid(value["commit"], "receipt commit")
    _require_git_oid(value["tree"], "receipt tree")
    archive_path = Path(_require_string(value["archive_path"], "receipt archive path"))
    sidecar_path = Path(_require_string(value["sidecar_path"], "receipt sidecar path"))
    for field in (
        "archive_sha256",
        "sidecar_sha256",
        "core_sha256",
        "structural_archive_sha256",
        "semantic_generation_digest",
        "semantic_manifest_sha256",
        "semantic_verification_sha256",
        "runtime_manifest_sha256",
        "query_evidence_receipt_sha256",
        "query_evidence_digest",
        "query_evidence_tree_digest",
    ):
        _require_sha256(value[field], f"receipt {field}")
    if type(value["archive_size_bytes"]) is not int or value["archive_size_bytes"] <= 0:
        raise ToolchainError("receipt archive size is invalid")
    value["base_combined_cache"] = _validate_base_identity(value["base_combined_cache"])
    value["work"] = _validate_counts(value["work"], WORK_FIELDS, "receipt work")
    value["timings_ms"] = _validate_counts(value["timings_ms"], TIMING_FIELDS, "receipt timings")
    value["peak_memory_bytes"] = _validate_counts(
        value["peak_memory_bytes"], PEAK_MEMORY_FIELDS, "receipt peak memory"
    )
    value["publication_metrics"] = _validate_counts(
        value["publication_metrics"],
        PUBLICATION_METRIC_FIELDS,
        "receipt publication metrics",
    )
    if (
        any(count <= 0 for count in value["peak_memory_bytes"].values())
        or value["publication_metrics"]["archive_peak_memory_bytes"] <= 0
        or value["publication_metrics"]["total_ms"]
        != value["timings_ms"]["prepublication_total_ms"]
        + value["publication_metrics"]["combined_archive_ms"]
        or value["publication_metrics"]["total_peak_memory_bytes"]
        != max(
            value["publication_metrics"]["archive_peak_memory_bytes"],
            *value["peak_memory_bytes"].values(),
        )
    ):
        raise ToolchainError("receipt timing/peak-memory evidence is inconsistent")
    if verify_bytes:
        if STRUCTURAL.sha256_file(archive_path) != value["archive_sha256"]:
            raise ToolchainError("receipt/archive digest mismatch")
        if STRUCTURAL.sha256_file(sidecar_path) != value["sidecar_sha256"]:
            raise ToolchainError("receipt/sidecar digest mismatch")
        verified = verify_combined_cache_archive(archive_path, sidecar_path)
        core = verified["core"]
        comparisons = {
            "core_sha256": verified["core_sha256"],
            "structural_archive_sha256": verified["structural_archive_sha256"],
            "semantic_generation_digest": verified["semantic_generation_digest"],
            "semantic_manifest_sha256": core["semantic"]["manifest_sha256"],
            "semantic_verification_sha256": core["semantic"]["verification_sha256"],
            "runtime_manifest_sha256": core["runtime"]["manifest_sha256"],
            "query_evidence_receipt_sha256": core["query_evidence"][
                "receipt_sha256"
            ],
            "query_evidence_digest": core["query_evidence"]["evidence_digest"],
            "query_evidence_tree_digest": core["query_evidence"]["tree_digest"],
            "repository": core["repository"],
            "commit": core["commit"],
            "tree": core["tree"],
            "base_combined_cache": core["base_combined_cache"],
            "work": core["work"],
            "timings_ms": core["timings_ms"],
            "peak_memory_bytes": core["peak_memory_bytes"],
            "publication_metrics": verified["publication_metrics"],
        }
        for field, actual in comparisons.items():
            if value[field] != actual:
                raise ToolchainError(f"combined receipt {field} differs from archive")
    return value


def load_combined_cache_catalog(output_root: Path) -> dict[str, Any]:
    path = output_root / COMBINED_CACHE_CATALOG
    if not path.exists():
        return {"schema_version": COMBINED_CACHE_SCHEMA_VERSION, "entries": []}
    catalog = _load_canonical_json(path, "combined cache catalog", semantic=False)
    _require_exact_fields(catalog, {"schema_version", "entries"}, "combined cache catalog")
    if catalog["schema_version"] != COMBINED_CACHE_SCHEMA_VERSION:
        raise ToolchainError("combined cache catalog schema mismatch")
    if not isinstance(catalog["entries"], list):
        raise ToolchainError("combined cache catalog entries must be a list")
    entries = [_validate_receipt(entry, verify_bytes=False) for entry in catalog["entries"]]
    expected = sorted(
        entries,
        key=lambda entry: (
            entry["case"]["case_index"],
            entry["case"]["attempt_index"],
            entry["case"]["instance_id"],
            entry["archive_sha256"],
        ),
    )
    if entries != expected:
        raise ToolchainError("combined cache catalog entries are not deterministic")
    attempts = [
        (entry["case"]["case_index"], entry["case"]["attempt_index"], entry["case"]["instance_id"])
        for entry in entries
    ]
    if len(attempts) != len(set(attempts)):
        raise ToolchainError("combined cache catalog has duplicate case attempts")
    return {"schema_version": COMBINED_CACHE_SCHEMA_VERSION, "entries": entries}


def publish_combined_cache_receipt(output_root: Path, receipt: Mapping[str, Any]) -> Path:
    if not output_root.is_dir() or output_root.is_symlink():
        raise ToolchainError("combined cache catalog root must be a real directory")
    receipt = _validate_receipt(receipt, verify_bytes=True)
    catalog = load_combined_cache_catalog(output_root)
    attempt = (
        receipt["case"]["case_index"],
        receipt["case"]["attempt_index"],
        receipt["case"]["instance_id"],
    )
    if any(
        (
            entry["case"]["case_index"],
            entry["case"]["attempt_index"],
            entry["case"]["instance_id"],
        )
        == attempt
        for entry in catalog["entries"]
    ):
        raise ToolchainError("refusing to overwrite a combined cache catalog attempt")
    catalog["entries"].append(receipt)
    catalog["entries"].sort(
        key=lambda entry: (
            entry["case"]["case_index"],
            entry["case"]["attempt_index"],
            entry["case"]["instance_id"],
            entry["archive_sha256"],
        )
    )
    path = output_root / COMBINED_CACHE_CATALOG
    STRUCTURAL.write_canonical_json(path, catalog)
    return path


def select_combined_cache(
    receipts: Sequence[Mapping[str, Any]],
    repository: str,
    target_commit: str,
    target_identity: Mapping[str, Any],
    git_dir: Path,
    case_index: int,
    *,
    runtime_manifest_path: Path,
    semantic_identity: Mapping[str, Any] | None,
    scan_flags: Sequence[str],
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
    diagnostics: dict[str, Any] | None = None,
) -> dict[str, Any] | None:
    """Select the nearest fully verified compatible prior combined receipt."""
    repository = _require_string(repository, "target repository")
    target_commit = _require_git_oid(target_commit, "target commit")
    if target_identity.get("repository") != repository or target_identity.get("commit") != target_commit:
        raise ToolchainError("target structural identity does not match selection target")
    target_tree = _require_git_oid(target_identity.get("tree"), "target tree")
    if STRUCTURAL._git_commit_tree(git_dir, target_commit) != target_tree:
        raise ToolchainError("target commit/tree binding mismatch")
    runtime = _project_runtime_manifest(runtime_manifest_path)
    semantic_identity = (
        _validate_semantic_identity(semantic_identity)
        if semantic_identity is not None
        else None
    )
    flags = _validate_scan_flags(scan_flags)
    candidates = []
    verification_seconds = 0.0
    verified_candidate_count = 0
    for receipt in receipts:
        if not isinstance(receipt, dict) or receipt.get("status") != "ready":
            continue
        case = receipt.get("case")
        if (
            not isinstance(case, dict)
            or type(case.get("case_index")) is not int
            or case["case_index"] >= case_index
            or receipt.get("repository") != repository
        ):
            continue
        archive_path = Path(_require_string(receipt.get("archive_path"), "receipt archive path"))
        sidecar_path = Path(_require_string(receipt.get("sidecar_path"), "receipt sidecar path"))
        if STRUCTURAL.sha256_file(archive_path) != _require_sha256(
            receipt.get("archive_sha256"), "receipt archive digest"
        ) or STRUCTURAL.sha256_file(sidecar_path) != _require_sha256(
            receipt.get("sidecar_sha256"), "receipt sidecar digest"
        ):
            raise ToolchainError("combined receipt bytes differ from recorded identity")
        verification_started = time.monotonic()
        verified = verify_combined_cache_archive(archive_path, sidecar_path)
        verification_seconds += time.monotonic() - verification_started
        verified_candidate_count += 1
        core = verified["core"]
        if STRUCTURAL._git_commit_tree(git_dir, core["commit"]) != core["tree"]:
            raise ToolchainError("combined cache commit/tree binding mismatch")
        structural = core["structural"]
        compatible = (
            core["scan_flags"] == flags
            and core["runtime"] == runtime
            and (
                semantic_identity is None
                or core["semantic"]["semantic_identity"] == semantic_identity
            )
            and structural["root_slug"] == target_identity["root_slug"]
            and structural["producer"] == target_identity["producer"]
            and structural["toolchain_lock_digest"] == toolchain_lock_digest
            and structural["inventory_digest"] == inventory_digest
            and structural["inventory_file_sha256"] == inventory_file_sha256
            and structural["inventory_policy_digest"]
            == target_identity["inventory_policy_digest"]
        )
        if not compatible:
            continue
        diff = STRUCTURAL._git_diff_paths(git_dir, core["commit"], target_commit)
        candidates.append(
            (
                diff["distance"],
                -case["case_index"],
                -case["attempt_index"],
                case["instance_id"],
                receipt,
                verified,
                diff,
            )
        )
    if not candidates:
        if diagnostics is not None:
            diagnostics.update(
                {
                    "verification_seconds": verification_seconds,
                    "verified_candidate_count": verified_candidate_count,
                }
            )
        return None
    _, _, _, _, receipt, verified, diff = min(candidates, key=lambda item: item[:4])
    (
        invalidated_partitions,
        compatible_partitions,
        invalidated_partition_reasons,
    ) = STRUCTURAL._partition_invalidation_plan(
        verified["structural_core"], target_identity
    )
    structural_summary = verified["core"]["structural"]
    structural_archive_size = next(
        member["size_bytes"]
        for member in verified["core"]["members"]
        if member["path"] == structural_summary["archive_member"]
    )
    structural_verified = {
        "core": verified["structural_core"],
        "archive_sha256": verified["structural_archive_sha256"],
        "archive_size_bytes": structural_archive_size,
        "core_sha256": structural_summary["core_sha256"],
        "sidecar_sha256": structural_summary["sidecar_sha256"],
        "structural_cache_tree_digest": structural_summary[
            "structural_cache_tree_digest"
        ],
    }
    structural_selection = {
        "entry": {
            "instance_id": receipt["case"]["instance_id"],
            "attempt_index": receipt["case"]["attempt_index"],
            "repository": receipt["repository"],
            "commit": receipt["commit"],
            "tree": receipt["tree"],
            "archive_sha256": verified["structural_archive_sha256"],
            "sidecar_sha256": structural_summary["sidecar_sha256"],
            "core_sha256": structural_summary["core_sha256"],
        },
        "verified": structural_verified,
        "diff": diff,
        "invalidated_partitions": invalidated_partitions,
        "compatible_partitions": compatible_partitions,
        "invalidated_partition_reasons": invalidated_partition_reasons,
    }
    if diagnostics is not None:
        diagnostics.update(
            {
                "verification_seconds": verification_seconds,
                "verified_candidate_count": verified_candidate_count,
            }
        )
    return {
        "receipt": dict(receipt),
        "verified": verified,
        "diff": diff,
        "invalidated_partitions": invalidated_partitions,
        "compatible_partitions": compatible_partitions,
        "invalidated_partition_reasons": invalidated_partition_reasons,
        "base_combined_cache": combined_base_identity(verified),
        "structural_selection": structural_selection,
    }


def inject_combined_cache(
    selection: Mapping[str, Any],
    checkout: Path,
    target_identity: Mapping[str, Any],
    git_dir: Path,
    *,
    toolchain_lock_digest: str,
    inventory_digest: str,
    inventory_file_sha256: str,
) -> dict[str, Any]:
    """Inject semantic bytes after #785 authorizes the structural base copy."""
    receipt = selection["receipt"]
    archive_path = Path(receipt["archive_path"])
    sidecar_path = Path(receipt["sidecar_path"])
    archive_before = STRUCTURAL.sha256_file(archive_path)
    sidecar_before = STRUCTURAL.sha256_file(sidecar_path)
    with tempfile.TemporaryDirectory(prefix="rna-combined-cache-inject-") as temporary:
        temporary_root = Path(temporary)
        components_root = temporary_root / "components"
        _, core = _extract_verified_archive(archive_path, sidecar_path, components_root)
        components = _verify_components(components_root, core)
        materialized_structural = temporary_root / "structural-cache"
        STRUCTURAL.verify_structural_cache_archive(
            components_root / core["structural"]["archive_member"],
            components_root / core["structural"]["sidecar_member"],
            materialize_cache=materialized_structural,
        )
        structural_selection = {
            "entry": {
                "archive_path": str(
                    components_root / core["structural"]["archive_member"]
                ),
                "sidecar_path": str(
                    components_root / core["structural"]["sidecar_member"]
                ),
            },
            "verified": components["structural"],
            "diff": selection["diff"],
            "invalidated_partitions": selection["invalidated_partitions"],
            "compatible_partitions": selection["compatible_partitions"],
            "invalidated_partition_reasons": selection[
                "invalidated_partition_reasons"
            ],
        }
        structural_receipt = STRUCTURAL.inject_structural_cache(
            structural_selection,
            checkout,
            target_identity,
            git_dir,
            toolchain_lock_digest=toolchain_lock_digest,
            inventory_digest=inventory_digest,
            inventory_file_sha256=inventory_file_sha256,
            verified=components["structural"],
            materialized_cache=materialized_structural,
        )
        cache_root = checkout / ".oh" / ".cache"
        semantic_destination = cache_root / SEMANTIC_ROOT
        if semantic_destination.exists() or semantic_destination.is_symlink():
            raise ToolchainError("structural injection unexpectedly produced semantic cache bytes")
        semantic_staging = cache_root / f".{SEMANTIC_ROOT}.inject-{os.getpid()}"
        if semantic_staging.exists() or semantic_staging.is_symlink():
            raise ToolchainError("semantic injection staging path already exists")
        try:
            shutil.copytree(
                components_root / SEMANTIC_MEMBER_ROOT,
                semantic_staging,
                symlinks=False,
            )
            verify_semantic_cache_root(semantic_staging)
            semantic_staging.replace(semantic_destination)
        finally:
            if semantic_staging.exists():
                shutil.rmtree(semantic_staging)
    if (
        STRUCTURAL.sha256_file(archive_path) != archive_before
        or STRUCTURAL.sha256_file(sidecar_path) != sidecar_before
    ):
        raise ToolchainError("combined injection mutated its immutable base archive")
    semantic = verify_semantic_cache_root(checkout / ".oh" / ".cache" / SEMANTIC_ROOT)
    return {
        "base_combined_cache": combined_base_identity(selection["verified"]),
        "base_archive_sha256": archive_before,
        "base_sidecar_sha256": sidecar_before,
        "base_structural_archive_sha256": selection["verified"]["structural_archive_sha256"],
        "base_semantic_generation_digest": semantic["generation_digest"],
        "base_semantic_row_count": semantic["row_count"],
        "changed_file_count": selection["diff"]["distance"],
        "invalidated_partitions": list(selection["invalidated_partitions"]),
        "structural_injection": structural_receipt,
    }
