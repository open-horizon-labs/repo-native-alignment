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

sys.path.insert(0, str(Path(__file__).resolve(strict=True).parent))

import frontier_replay
from live_identity import (
    LiveIdentityError,
    LiveIdentityVerifier,
    derive_projection_authorization,
)


HERE = Path(__file__).resolve().parent.parent
CONFIG = json.loads((HERE / "config/supervisor.json").read_text())
EMPTY_RE = re.compile(
    r"^No (?:dependents|neighbors) found for `[^`]+` within [0-9]+ hops\.$"
)
READY = "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"
INDEX_RE = re.compile(r"^\*Index: [1-9][0-9]* symbols .* schema v[0-9]+.*\*$")
BENCHMARK_COMPLETENESS_PREFIX = "- **benchmark per-file LSP completeness**:"
BENCHMARK_COMPLETENESS_RE = re.compile(
    r"^- \*\*benchmark per-file LSP completeness\*\*: ready — "
    r"(?P<covered>[0-9]+)/(?P<total>[0-9]+) included files covered; "
    r"0 violation\(s\); digest=(?P<digest>[0-9a-f]{64})$"
)
FRONTIER_SCHEMA = "issue827-rna-authorization-frontier-v1"
FRONTIER_SOURCE_SCHEMA = "issue827-rna-authorization-source-v1"
AUTHORIZATION_SCHEMA = "issue827-projection-authorization-v1"
HEX_64_RE = re.compile(r"^[0-9a-f]{64}$")


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_ref(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    return {
        "path": str(path),
        "bytes": len(data),
        "sha256": sha(data),
    }


def authorization_sha256(authorization: dict[str, object]) -> str:
    return sha(canonical(authorization))


def initial_authorization_source(
    authorization: dict[str, object],
) -> dict[str, object]:
    return {
        "schema_version": FRONTIER_SOURCE_SCHEMA,
        "source_sequence": 0,
        "source_kind": "injected_query_projection",
        "classification": "INJECTED_QUERY",
        "projection_sha256": authorization["projection_sha256"],
        "model_visible_projection_sha256": authorization[
            "projection_sha256"
        ],
        "projection_authorization": authorization,
        "projection_authorization_sha256": authorization_sha256(
            authorization
        ),
    }


def emitted_projection_authorization(
    projection: bytes, classification: str
) -> dict[str, object]:
    authorization = derive_projection_authorization(projection)
    if classification == "OK_EMPTY":
        # The empty-status sentence repeats the already-authorized requested
        # node for diagnostics.  It is not a graph projection and therefore
        # cannot expand the authorization frontier.
        authorization = {
            **authorization,
            "stable_code_ids": [],
        }
    return authorization


def traversal_authorization_source(
    sequence: int,
    classification: str,
    projection: bytes,
    model_visible_projection: bytes,
) -> dict[str, object]:
    authorization = emitted_projection_authorization(
        projection, classification
    )
    return {
        "schema_version": FRONTIER_SOURCE_SCHEMA,
        "source_sequence": sequence,
        "source_kind": "rna_traversal_projection",
        "classification": classification,
        "projection_sha256": sha(projection),
        "model_visible_projection_sha256": sha(model_visible_projection),
        "projection_authorization": authorization,
        "projection_authorization_sha256": authorization_sha256(
            authorization
        ),
    }


def build_authorization_frontier(
    sources: list[dict[str, object]],
) -> dict[str, object]:
    authorized: set[str] = set()
    for source in sources:
        authorization = source["projection_authorization"]
        if not isinstance(authorization, dict):
            raise ValueError("authorization_frontier_source_authorization")
        stable_ids = authorization.get("stable_code_ids")
        if (
            not isinstance(stable_ids, list)
            or stable_ids != sorted(set(stable_ids))
            or not all(isinstance(item, str) and item for item in stable_ids)
        ):
            raise ValueError("authorization_frontier_source_ids")
        authorized.update(stable_ids)
    body: dict[str, object] = {
        "schema_version": FRONTIER_SCHEMA,
        "sources": sources,
        "authorized_stable_code_ids": sorted(authorized),
    }
    return {
        **body,
        "authorization_frontier_sha256": sha(canonical(body)),
    }


def _validate_source_shape(
    source: object,
    expected_sequence: int,
) -> dict[str, object]:
    if not isinstance(source, dict):
        raise ValueError("authorization_frontier_source_not_object")
    expected_keys = {
        "schema_version",
        "source_sequence",
        "source_kind",
        "classification",
        "projection_sha256",
        "model_visible_projection_sha256",
        "projection_authorization",
        "projection_authorization_sha256",
    }
    if set(source) != expected_keys:
        raise ValueError("authorization_frontier_source_fields")
    if (
        source.get("schema_version") != FRONTIER_SOURCE_SCHEMA
        or source.get("source_sequence") != expected_sequence
    ):
        raise ValueError("authorization_frontier_source_identity")
    for field in (
        "projection_sha256",
        "model_visible_projection_sha256",
        "projection_authorization_sha256",
    ):
        if (
            not isinstance(source.get(field), str)
            or HEX_64_RE.fullmatch(str(source[field])) is None
        ):
            raise ValueError(f"authorization_frontier_source_{field}")
    authorization = source.get("projection_authorization")
    if (
        not isinstance(authorization, dict)
        or authorization.get("schema_version") != AUTHORIZATION_SCHEMA
        or authorization.get("projection_sha256")
        != source.get("projection_sha256")
        or authorization_sha256(authorization)
        != source.get("projection_authorization_sha256")
    ):
        raise ValueError("authorization_frontier_source_authorization")
    return source


def _validate_prior_projection_source(source: dict[str, object]) -> None:
    sequence = int(source["source_sequence"])
    path = Path(CONFIG["rna_events"]) / f"{sequence:04d}.projection"
    if not path.is_file() or path.is_symlink():
        raise ValueError("authorization_frontier_projection_missing")
    model_visible = path.read_bytes()
    if sha(model_visible) != source["model_visible_projection_sha256"]:
        raise ValueError("authorization_frontier_projection_tampered")
    classification = source["classification"]
    if classification not in {"OK_NONEMPTY", "OK_EMPTY"}:
        raise ValueError("authorization_frontier_classification")
    prefix = f"RNA_STATUS={classification}\n".encode()
    if not model_visible.startswith(prefix):
        raise ValueError("authorization_frontier_projection_prefix")
    projection = model_visible[len(prefix) :]
    authorization = emitted_projection_authorization(
        projection, str(classification)
    )
    if (
        sha(projection) != source["projection_sha256"]
        or authorization != source["projection_authorization"]
        or authorization_sha256(authorization)
        != source["projection_authorization_sha256"]
    ):
        raise ValueError("authorization_frontier_projection_provenance")


def validated_authorization_frontier(
    state: dict,
    initial_authorization: dict[str, object],
) -> dict[str, object]:
    expected_initial = initial_authorization_source(initial_authorization)
    value = state.get("authorization_frontier")
    if value is None:
        if (
            state.get("first_traversal_succeeded")
            or int(state.get("rna_calls", 0)) != 0
        ):
            raise ValueError("authorization_frontier_missing")
        return build_authorization_frontier([expected_initial])
    if not isinstance(value, dict):
        raise ValueError("authorization_frontier_not_object")
    expected_keys = {
        "schema_version",
        "sources",
        "authorized_stable_code_ids",
        "authorization_frontier_sha256",
    }
    if set(value) != expected_keys or value.get("schema_version") != FRONTIER_SCHEMA:
        raise ValueError("authorization_frontier_fields")
    sources = value.get("sources")
    if not isinstance(sources, list) or not sources:
        raise ValueError("authorization_frontier_sources")
    validated_sources: list[dict[str, object]] = []
    for index, source in enumerate(sources):
        validated = _validate_source_shape(source, index)
        if index == 0:
            if validated != expected_initial:
                raise ValueError("authorization_frontier_initial_source")
        else:
            if (
                validated.get("source_kind")
                != "rna_traversal_projection"
            ):
                raise ValueError("authorization_frontier_source_kind")
            _validate_prior_projection_source(validated)
        validated_sources.append(validated)
    rebuilt = build_authorization_frontier(validated_sources)
    if rebuilt != value:
        raise ValueError("authorization_frontier_hash_or_union")
    if len(validated_sources) - 1 != int(state.get("rna_calls", 0)):
        raise ValueError("authorization_frontier_call_count")
    receipt_paths = sorted(Path(CONFIG["rna_events"]).glob("*.json"))
    receipts: list[dict[str, object]] = []
    for expected_sequence, receipt_path in enumerate(receipt_paths, start=1):
        if (
            receipt_path.name != f"{expected_sequence:04d}.json"
            or receipt_path.is_symlink()
            or not receipt_path.is_file()
        ):
            raise ValueError("authorization_frontier_receipt_sequence")
        try:
            receipt = json.loads(receipt_path.read_bytes())
        except (OSError, json.JSONDecodeError) as exc:
            raise ValueError("authorization_frontier_receipt_invalid") from exc
        if not isinstance(receipt, dict):
            raise ValueError("authorization_frontier_receipt_invalid")
        receipts.append(receipt)

    def load_projection(reference: dict, where: str) -> bytes:
        if set(reference) != {"path", "bytes", "sha256"}:
            raise frontier_replay.FrontierReplayError(
                f"{where}:projection_reference"
            )
        path = Path(str(reference["path"]))
        if (
            path != Path(CONFIG["rna_events"])
            / f"{int(where.rsplit('_', 1)[-1]):04d}.projection"
            or path.is_symlink()
            or not path.is_file()
        ):
            raise frontier_replay.FrontierReplayError(
                f"{where}:projection_path"
            )
        data = path.read_bytes()
        if (
            reference["bytes"] != len(data)
            or reference["sha256"] != sha(data)
        ):
            raise frontier_replay.FrontierReplayError(
                f"{where}:projection_reference"
            )
        return data

    try:
        initial_projection = Path(CONFIG["initial_response"]).read_bytes()
        replayed = frontier_replay.replay(
            initial_projection,
            receipts,
            load_projection,
        )
    except (OSError, frontier_replay.FrontierReplayError) as exc:
        raise ValueError(f"authorization_frontier_replay:{exc}") from exc
    if replayed != rebuilt:
        raise ValueError("authorization_frontier_replay_state_mismatch")
    return rebuilt


def node_authorizers(
    frontier: dict[str, object], node: str
) -> list[dict[str, object]]:
    result: list[dict[str, object]] = []
    sources = frontier["sources"]
    if not isinstance(sources, list):
        raise ValueError("authorization_frontier_sources")
    for source in sources:
        if not isinstance(source, dict):
            raise ValueError("authorization_frontier_source_not_object")
        authorization = source.get("projection_authorization")
        stable_ids = (
            authorization.get("stable_code_ids")
            if isinstance(authorization, dict)
            else None
        )
        if isinstance(stable_ids, list) and node in stable_ids:
            result.append(
                {
                    "source_sequence": source["source_sequence"],
                    "source_kind": source["source_kind"],
                    "classification": source["classification"],
                    "projection_sha256": source["projection_sha256"],
                    "model_visible_projection_sha256": source[
                        "model_visible_projection_sha256"
                    ],
                    "projection_authorization_sha256": source[
                        "projection_authorization_sha256"
                    ],
                }
            )
    return result


def graph_terminal_completeness(text: str, expected_digest: str) -> dict:
    lines = text.splitlines()
    index_lines = [line for line in lines if INDEX_RE.fullmatch(line)]
    if len(index_lines) != 1 or lines.count("### Capability readiness") != 1:
        raise ValueError("missing_terminal_identity")
    completeness_lines = [
        line for line in lines if line.startswith(BENCHMARK_COMPLETENESS_PREFIX)
    ]
    if len(completeness_lines) != 1:
        raise ValueError("benchmark_completeness_missing_or_ambiguous")
    match = BENCHMARK_COMPLETENESS_RE.fullmatch(completeness_lines[0])
    if match is None:
        raise ValueError("benchmark_completeness_not_ready")
    covered = int(match.group("covered"))
    total = int(match.group("total"))
    if total <= 0 or covered != total:
        raise ValueError("benchmark_completeness_coverage_mismatch")
    digest = match.group("digest")
    if digest != expected_digest:
        raise ValueError("benchmark_completeness_digest_mismatch")
    return {
        "status": "ready",
        "covered_files": covered,
        "total_files": total,
        "violations": 0,
        "report_digest": digest,
    }


def read_state() -> dict:
    path = Path(CONFIG["state"])
    if path.exists():
        try:
            value = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            return {
                "schema_version": "issue827-rna-supervisor-state-v1",
                "fatal": True,
                "fatal_reason": "state_invalid",
            }
        if (
            not isinstance(value, dict)
            or value.get("schema_version")
            != "issue827-rna-supervisor-state-v1"
        ):
            return {
                "schema_version": "issue827-rna-supervisor-state-v1",
                "fatal": True,
                "fatal_reason": "state_invalid",
            }
        return value
    return {
        "schema_version": "issue827-rna-supervisor-state-v1",
        "fatal": False,
        "first_traversal_succeeded": False,
        "model_tool_attempts": 0,
        "rna_calls": 0,
    }


def write_state(state: dict) -> None:
    path = Path(CONFIG["state"])
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        try:
            previous = json.loads(path.read_bytes())
        except (OSError, json.JSONDecodeError):
            previous = {"fatal": True, "fatal_reason": "state_invalid"}
        if isinstance(previous, dict) and previous.get("fatal") is True:
            state = previous
    temporary = path.with_name(f".{path.name}.tmp")
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
    stdout_path = directory / f"{sequence:04d}.stdout"
    stderr_path = directory / f"{sequence:04d}.stderr"
    stdout_path.write_bytes(receipt.pop("_stdout"))
    stderr_path.write_bytes(receipt.pop("_stderr"))
    receipt["raw_stdout"] = file_ref(stdout_path)
    receipt["raw_stderr"] = file_ref(stderr_path)
    model_visible = receipt.pop("_model_visible_projection", None)
    if model_visible is not None:
        if not isinstance(model_visible, bytes):
            raise ValueError("model_visible_projection_not_bytes")
        projection_path = directory / f"{sequence:04d}.projection"
        projection_path.write_bytes(model_visible)
        receipt["model_visible_projection"] = file_ref(projection_path)
    else:
        receipt["model_visible_projection"] = None
    receipt["receipt_sha256"] = sha(canonical(receipt))
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

    verifier = LiveIdentityVerifier(CONFIG, CONFIG["state"])
    try:
        live_before = verifier.verify("rna_traverse:before")
    except LiveIdentityError as exc:
        return fail(state, f"identity:{exc.reason}")

    response_path = Path(CONFIG["initial_response"])
    try:
        if not response_path.is_file() or response_path.is_symlink():
            raise ValueError("initial_response_identity")
        response_bytes = response_path.read_bytes()
        if sha(response_bytes) != CONFIG["initial_response_sha256"]:
            raise ValueError("initial_response_identity")
        response = response_bytes.decode("utf-8", errors="strict")
        if not any(line.strip("`") == READY for line in response.splitlines()):
            raise ValueError("initial_response_readiness")
        authorization = derive_projection_authorization(response_bytes)
        if authorization["stable_code_ids"] != CONFIG["initial_ids"]:
            raise ValueError("initial_response_ids")
        authorization_sha = sha(canonical(authorization))
        if authorization_sha != CONFIG.get("initial_authorization_sha256"):
            raise ValueError("initial_response_authorization")
        initial_ids = set(authorization["stable_code_ids"])
    except (OSError, UnicodeError, ValueError) as exc:
        return fail(state, f"injected_response:{exc}")

    try:
        frontier_before = validated_authorization_frontier(
            state, authorization
        )
        authorizers = node_authorizers(frontier_before, args.node)
    except (OSError, UnicodeError, ValueError) as exc:
        return fail(state, f"authorization_frontier:{exc}")
    state["authorization_frontier"] = frontier_before
    sequence = int(state.get("rna_calls", 0)) + 1
    state["rna_calls"] = sequence
    first_traversal = not state.get("first_traversal_succeeded")
    if first_traversal or not authorizers:
        denied_receipt = {
            "schema_version": "issue827-rna-traversal-receipt-v1",
            "sequence": sequence,
            "node": args.node,
            "mode": args.mode,
            "argv": [],
            "returncode": None,
            "elapsed_seconds": 0.0,
            "stdout_bytes": 0,
            "stdout_sha256": sha(b""),
            "stderr_bytes": 0,
            "stderr_sha256": sha(b""),
            "projection_authorization_sha256": authorization_sha,
            "authorization_frontier_before": frontier_before,
            "requested_node_authorized_by": authorizers,
            "live_identity_before": live_before,
            "_stdout": b"",
            "_stderr": b"",
        }
        if first_traversal and args.mode != "neighbors":
            return fail(
                state,
                "first_traversal_must_use_neighbors",
                denied_receipt,
            )
        if not authorizers:
            return fail(
                state,
                (
                    "first_node_not_in_injected_projection"
                    if first_traversal
                    else "node_not_in_authorization_frontier"
                ),
                denied_receipt,
            )
    if first_traversal and args.node not in initial_ids:
        return fail(
            state,
            "first_node_not_in_injected_projection",
            denied_receipt,
        )

    argv = [
        CONFIG["launcher"],
        "search",
        "--repo",
        CONFIG["repo"],
        "--root",
        CONFIG["root"],
        "--node",
        args.node,
        "--mode",
        args.mode,
        "--compact",
    ]
    started = time.monotonic()
    result = subprocess.run(
        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False
    )
    elapsed = time.monotonic() - started
    stdout = result.stdout
    stderr = result.stderr
    text = stdout.decode("utf-8", errors="replace")
    receipt = {
        "schema_version": "issue827-rna-traversal-receipt-v1",
        "sequence": sequence,
        "node": args.node,
        "mode": args.mode,
        "argv": argv,
        "returncode": result.returncode,
        "root": CONFIG["root"],
        "identity_sha256": live_before["identity_sha256"],
        "cache_manifest_sha256": live_before["cache_manifest_sha256"],
        "cache_archive_sha256": live_before["cache_archive_sha256"],
        "launcher_sha256": live_before["launcher_sha256"],
        "binary_sha256": live_before["binary_sha256"],
        "repository_identity": live_before["repository_identity"],
        "elapsed_seconds": elapsed,
        "stdout_bytes": len(stdout),
        "stdout_sha256": sha(stdout),
        "stderr_bytes": len(stderr),
        "stderr_sha256": sha(stderr),
        "projection_authorization_sha256": authorization_sha,
        "authorization_frontier_before": frontier_before,
        "requested_node_authorized_by": authorizers,
        "live_identity_before": live_before,
        "_stdout": stdout,
        "_stderr": stderr,
    }

    try:
        live_after = verifier.verify("rna_traverse:after")
    except LiveIdentityError as exc:
        receipt["live_identity_after"] = None
        return fail(state, f"identity_after:{exc.reason}", receipt)
    receipt["live_identity_after"] = live_after
    if live_before["live_state_sha256"] != live_after["live_state_sha256"]:
        return fail(state, "identity_changed_during_rna_call", receipt)
    if result.returncode != 0:
        return fail(state, f"launcher_exit_{result.returncode}", receipt)
    try:
        completeness = graph_terminal_completeness(
            text, live_before["readiness_report_digest"]
        )
    except ValueError as exc:
        return fail(state, str(exc), receipt)
    receipt["benchmark_completeness"] = completeness

    graph_start = text.find("## Graph")
    index_start = text.find("*Index:")
    if graph_start >= 0 and index_start > graph_start:
        projection = text[graph_start:index_start].rstrip() + "\n"
        if not re.search(r"(?m)^- \*\*[^*]+\*\*", projection):
            return fail(state, "empty_or_malformed_graph", receipt)
        classification = "OK_NONEMPTY"
    else:
        empty_lines = [
            line for line in text.splitlines() if EMPTY_RE.fullmatch(line)
        ]
        if len(empty_lines) != 1:
            return fail(state, "unrecognized_success_rendering", receipt)
        projection = empty_lines[0] + "\n"
        classification = "OK_EMPTY"

    projection_bytes = projection.encode()
    model_visible_projection = (
        f"RNA_STATUS={classification}\n".encode() + projection_bytes
    )
    source = traversal_authorization_source(
        sequence,
        classification,
        projection_bytes,
        model_visible_projection,
    )
    sources = frontier_before["sources"]
    if not isinstance(sources, list):
        return fail(state, "authorization_frontier_sources", receipt)
    try:
        frontier_after = build_authorization_frontier(
            [*sources, source]
        )
    except ValueError as exc:
        return fail(state, f"authorization_frontier_expand:{exc}", receipt)
    receipt["classification"] = classification
    receipt["projection_bytes"] = len(projection_bytes)
    receipt["projection_sha256"] = sha(projection_bytes)
    receipt["emitted_projection_authorization"] = source[
        "projection_authorization"
    ]
    receipt["emitted_projection_authorization_sha256"] = source[
        "projection_authorization_sha256"
    ]
    receipt["authorization_frontier_after"] = frontier_after
    receipt["_model_visible_projection"] = model_visible_projection
    persist(receipt)
    state["last_classification"] = classification
    state["authorization_frontier"] = frontier_after
    if sequence == 1:
        state["first_traversal_succeeded"] = True
        state["first_traversal_status"] = classification
        state["first_node"] = args.node
        state["initial_projection_authorization_sha256"] = authorization_sha
    write_state(state)
    sys.stdout.write(f"RNA_STATUS={classification}\n{projection}")
    return 0


if __name__ == "__main__":
    lock = Path(CONFIG["lock"])
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("ab") as handle_lock:
        fcntl.flock(handle_lock, fcntl.LOCK_EX)
        raise SystemExit(main())
