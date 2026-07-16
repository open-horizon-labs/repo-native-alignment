#!/usr/bin/env python3
"""Verify semantic artifact provenance and fail-closed CLI evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import shlex
import subprocess


def sha256(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def run_json(command: list[str]) -> dict:
    completed = subprocess.run(command, text=True, capture_output=True)
    if completed.returncode != 0:
        raise RuntimeError(
            "command failed closed\n"
            f"command: {shlex.join(command)}\n"
            f"exit: {completed.returncode}\n"
            f"stdout:\n{completed.stdout}\n"
            f"stderr:\n{completed.stderr}"
        )
    return json.loads(completed.stdout)

def validate_search_report(report: dict) -> None:
    required = {
        "requested_mode": "hybrid",
        "effective_mode": "hybrid",
        "fusion": "reciprocal_rank_fusion",
        "rerank_applied": True,
        "acceleration": "metal",
        "index_hash_scope": "active_lance_manifests",
    }
    for key, expected in required.items():
        if report.get(key) != expected:
            raise RuntimeError(f"{key}={report.get(key)!r}, expected {expected!r}")
    if report.get("degradations"):
        raise RuntimeError(f"qualification degraded: {report.get('degradations')}")
    for count in (
        "keyword_candidates",
        "vector_candidates",
        "fusion_candidates",
        "rerank_candidates",
    ):
        if not isinstance(report.get(count), int) or report[count] < 1:
            raise RuntimeError(f"{count} must be a positive integer")
    if (
        not isinstance(report.get("index_generation_unix_ms"), int)
        or report["index_generation_unix_ms"] <= 0
    ):
        raise RuntimeError("index generation must be a positive Unix timestamp")
    if not re.fullmatch(r"[0-9a-f]{64}", str(report.get("index_blake3", ""))):
        raise RuntimeError("index_blake3 must be a 64-character lowercase hex digest")

    results = report.get("results")
    if not isinstance(results, list) or not results:
        raise RuntimeError("qualified search returned no stable node IDs")
    for expected_rank, result in enumerate(results, start=1):
        if not isinstance(result.get("id"), str) or not result["id"]:
            raise RuntimeError("qualified result is missing a stable ID")
        if result.get("final_rank") != expected_rank:
            raise RuntimeError("final result ranks must be contiguous and one-based")
        if not isinstance(result.get("retrieval_rank"), int) or result["retrieval_rank"] < 1:
            raise RuntimeError("retrieval rank must be a positive integer")
        for score in ("retrieval_score", "rerank_score"):
            if not isinstance(result.get(score), (int, float)) or not math.isfinite(
                result[score]
            ):
                raise RuntimeError(f"{score} must be finite")


def compare_reopened_report(first: dict, reopened: dict, tolerance: float = 1e-6) -> None:
    validate_search_report(reopened)
    stable_fields = (
        "requested_mode",
        "effective_mode",
        "keyword_candidates",
        "vector_candidates",
        "fusion_candidates",
        "fusion",
        "rerank_candidates",
        "rerank_applied",
        "degradations",
        "embedding_model",
        "rerank_model",
        "acceleration",
        "index_generation_unix_ms",
        "index_blake3",
        "index_hash_scope",
    )
    for field in stable_fields:
        if reopened.get(field) != first.get(field):
            raise RuntimeError(f"fresh-process reopen changed {field}")
    if len(reopened["results"]) != len(first["results"]):
        raise RuntimeError("fresh-process reopen changed result count")
    for before, after in zip(first["results"], reopened["results"], strict=True):
        for field in ("id", "kind", "title", "retrieval_rank", "final_rank"):
            if after.get(field) != before.get(field):
                raise RuntimeError(f"fresh-process reopen changed result {field}")
        for score in ("retrieval_score", "rerank_score"):
            if not math.isclose(
                float(after[score]), float(before[score]), rel_tol=tolerance, abs_tol=tolerance
            ):
                raise RuntimeError(f"fresh-process reopen changed result {score}")


def traversal_result_count(output: str, node: str, direction: str) -> int:
    header = f"## Graph neighbors ({direction}) of `{node}`"
    if header not in output:
        raise RuntimeError(f"traversal did not resolve selected stable ID `{node}`")
    match = re.search(r"(?m)^(\d+) result\(s\)$", output)
    if not match or int(match.group(1)) < 1:
        raise RuntimeError(f"traversal returned no graph context for `{node}`")
    return int(match.group(1))

def traversal_command(binary: Path, repo: Path, node: str) -> list[str]:
    return [
        str(binary),
        "search",
        "--repo",
        str(repo),
        "--root",
        "all",
        "--node",
        node,
        "--mode",
        "neighbors",
        "--direction",
        "incoming",
        "--compact",
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--expected-sha", required=True)
    args = parser.parse_args()

    manifest = json.loads((args.artifact / "artifact-manifest.json").read_text())
    if manifest.get("schema_version") != 1:
        raise RuntimeError(f"unsupported artifact schema: {manifest.get('schema_version')}")
    if manifest["git_sha"] != args.expected_sha:
        raise RuntimeError(
            f"artifact SHA {manifest['git_sha']} != checkout SHA {args.expected_sha}"
        )
    expected_metadata = {
        "target": "aarch64-apple-darwin",
        "cpu": "apple-m4",
        "job": "semantic-artifact",
        "features": ["metal"],
        "acceleration": "metal-required",
        "candle_metal_force_release": "1",
        "rustflags": "-C target-cpu=apple-m4 -C link-arg=-Wl,-dead_strip",
    }
    for key, expected in expected_metadata.items():
        if manifest.get(key) != expected:
            raise RuntimeError(f"manifest {key}={manifest.get(key)!r}, expected {expected!r}")
    actual_hashes = {}
    for name, expected in manifest["files"].items():
        actual = sha256(args.artifact / name)
        if actual != expected:
            raise RuntimeError(f"artifact hash mismatch for {name}: {actual} != {expected}")
        actual_hashes[name] = actual
    qualification_digest = hashlib.sha256(
        json.dumps(actual_hashes, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    if qualification_digest != manifest["qualification_digest"]:
        raise RuntimeError("artifact qualification digest does not match its payload")

    binary = args.artifact / "repo-native-alignment"
    capabilities = run_json(
        [
            str(binary),
            "capabilities",
            "--json",
            "--require",
            "embeddings,rerank,metal",
        ]
    )
    if (
        capabilities.get("acceleration") != "metal"
        or capabilities.get("embeddings") is not True
        or capabilities.get("rerank") is not True
        or capabilities.get("metal") is not True
    ):
        raise RuntimeError(f"Metal attestation failed: {capabilities}")
    model_lock = json.loads((args.artifact / "model-lock.json").read_text())
    if model_lock.get("schema_version") != 1:
        raise RuntimeError(f"unsupported model lock schema: {model_lock.get('schema_version')}")
    locked_models = {
        model["purpose"]: model["repository"] for model in model_lock["models"]
    }
    if set(locked_models) != {"embedding", "rerank"}:
        raise RuntimeError(f"model lock purposes are not exact: {sorted(locked_models)}")
    if manifest["embedding_model"] != locked_models.get("embedding"):
        raise RuntimeError("manifest embedding model does not match model lock")
    if manifest["rerank_model"] != locked_models.get("rerank"):
        raise RuntimeError("manifest rerank model does not match model lock")
    if capabilities["embedding_model"] != locked_models.get("embedding"):
        raise RuntimeError("runtime embedding model does not match model lock")
    if capabilities["rerank_model"] != locked_models.get("rerank"):
        raise RuntimeError("runtime rerank model does not match model lock")

    query = "find greeting function behavior and related helper call relationships"
    command = [
        str(binary),
        "search",
        query,
        "--repo",
        str(args.repo),
        "--search-mode",
        "hybrid",
        "--rerank",
        "--compact",
        "--limit",
        "5",
        "--diagnostics-json",
    ]
    report = run_json(command)
    validate_search_report(report)

    selected_node = None
    traversal_count = 0
    for result in report["results"]:
        node = result["id"]
        completed = subprocess.run(
            traversal_command(binary, args.repo, node),
            check=True,
            text=True,
            capture_output=True,
        )
        try:
            traversal_count = traversal_result_count(completed.stdout, node, "incoming")
            selected_node = node
            break
        except RuntimeError:
            continue
    if selected_node is None:
        raise RuntimeError("no qualified stable ID resolved to non-empty graph context")

    # A second process must reopen the persisted index while all model/network
    # clients are already forced offline by the workflow environment.
    reopened = run_json(command)
    compare_reopened_report(report, reopened)
    print(
        json.dumps(
            {
                "capabilities": capabilities,
                "search": report,
                "traversal": {
                    "node": selected_node,
                    "mode": "neighbors",
                    "direction": "incoming",
                    "result_count": traversal_count,
                },
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
