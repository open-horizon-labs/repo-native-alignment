#!/usr/bin/env python3
"""Pure replay validator for model-visible RNA traversal authorization."""

from __future__ import annotations

import hashlib
import json
from typing import Any, Callable, Mapping, Sequence

from live_identity import derive_projection_authorization


FRONTIER_SCHEMA = "issue827-rna-authorization-frontier-v1"
SOURCE_SCHEMA = "issue827-rna-authorization-source-v1"
RECEIPT_SCHEMA = "issue827-rna-traversal-receipt-v1"


class FrontierReplayError(ValueError):
    pass


def canonical(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode()


def sha(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def authorization_sha(value: Mapping[str, Any]) -> str:
    return sha(canonical(value))


def emitted_authorization(
    projection: bytes,
    classification: str,
) -> dict[str, Any]:
    value = derive_projection_authorization(projection)
    if classification == "OK_EMPTY":
        value = {**value, "stable_code_ids": []}
    return value


def source(
    sequence: int,
    kind: str,
    classification: str,
    projection: bytes,
    model_visible: bytes,
) -> dict[str, Any]:
    authorization = (
        derive_projection_authorization(projection)
        if sequence == 0
        else emitted_authorization(projection, classification)
    )
    return {
        "schema_version": SOURCE_SCHEMA,
        "source_sequence": sequence,
        "source_kind": kind,
        "classification": classification,
        "projection_sha256": sha(projection),
        "model_visible_projection_sha256": sha(model_visible),
        "projection_authorization": authorization,
        "projection_authorization_sha256": authorization_sha(authorization),
    }


def build_frontier(sources: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    authorized: set[str] = set()
    normalized: list[dict[str, Any]] = []
    for expected_sequence, item in enumerate(sources):
        if (
            not isinstance(item, Mapping)
            or item.get("schema_version") != SOURCE_SCHEMA
            or item.get("source_sequence") != expected_sequence
        ):
            raise FrontierReplayError("source_identity")
        authorization = item.get("projection_authorization")
        ids = (
            authorization.get("stable_code_ids")
            if isinstance(authorization, Mapping)
            else None
        )
        if (
            not isinstance(ids, list)
            or ids != sorted(set(ids))
            or not all(isinstance(node, str) and node for node in ids)
            or authorization_sha(authorization)
            != item.get("projection_authorization_sha256")
        ):
            raise FrontierReplayError("source_authorization")
        authorized.update(ids)
        normalized.append(dict(item))
    body = {
        "schema_version": FRONTIER_SCHEMA,
        "sources": normalized,
        "authorized_stable_code_ids": sorted(authorized),
    }
    return {
        **body,
        "authorization_frontier_sha256": sha(canonical(body)),
    }


def authorizers(
    frontier: Mapping[str, Any],
    node: str,
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for item in frontier.get("sources", []):
        authorization = item.get("projection_authorization")
        ids = (
            authorization.get("stable_code_ids")
            if isinstance(authorization, Mapping)
            else []
        )
        if node in ids:
            result.append(
                {
                    "source_sequence": item["source_sequence"],
                    "source_kind": item["source_kind"],
                    "classification": item["classification"],
                    "projection_sha256": item["projection_sha256"],
                    "model_visible_projection_sha256": item[
                        "model_visible_projection_sha256"
                    ],
                    "projection_authorization_sha256": item[
                        "projection_authorization_sha256"
                    ],
                }
            )
    return result


def replay(
    initial_projection: bytes,
    receipts: Sequence[Mapping[str, Any]],
    load_projection: Callable[[Mapping[str, Any], str], bytes],
) -> dict[str, Any]:
    """Recompute the complete authorization frontier from immutable bytes."""

    initial = source(
        0,
        "injected_query_projection",
        "INJECTED_QUERY",
        initial_projection,
        initial_projection,
    )
    sources: list[Mapping[str, Any]] = [initial]
    frontier = build_frontier(sources)
    initial_authorization_sha = initial["projection_authorization_sha256"]
    for sequence, receipt in enumerate(receipts, start=1):
        where = f"rna_event_{sequence}"
        if (
            not isinstance(receipt, Mapping)
            or receipt.get("schema_version") != RECEIPT_SCHEMA
            or receipt.get("sequence") != sequence
        ):
            raise FrontierReplayError(f"{where}:receipt_identity")
        receipt_body = {
            key: value
            for key, value in receipt.items()
            if key != "receipt_sha256"
        }
        if receipt.get("receipt_sha256") != sha(canonical(receipt_body)):
            raise FrontierReplayError(f"{where}:receipt_hash")
        classification = receipt.get("classification")
        if classification not in {"OK_NONEMPTY", "OK_EMPTY"}:
            raise FrontierReplayError(f"{where}:classification")
        node = receipt.get("node")
        if not isinstance(node, str) or not node:
            raise FrontierReplayError(f"{where}:node")
        expected_authorizers = authorizers(frontier, node)
        if not expected_authorizers:
            raise FrontierReplayError(f"{where}:guessed_node")
        if sequence == 1 and receipt.get("mode") != "neighbors":
            raise FrontierReplayError(f"{where}:first_mode")
        if receipt.get("authorization_frontier_before") != frontier:
            raise FrontierReplayError(f"{where}:frontier_before")
        if receipt.get("requested_node_authorized_by") != expected_authorizers:
            raise FrontierReplayError(f"{where}:authorizer_provenance")
        if (
            receipt.get("projection_authorization_sha256")
            != initial_authorization_sha
        ):
            raise FrontierReplayError(f"{where}:initial_authorization")

        visible_ref = receipt.get("model_visible_projection")
        if not isinstance(visible_ref, Mapping):
            raise FrontierReplayError(f"{where}:projection_ref")
        visible = load_projection(visible_ref, where)
        prefix = f"RNA_STATUS={classification}\n".encode()
        if not visible.startswith(prefix):
            raise FrontierReplayError(f"{where}:projection_prefix")
        projection = visible[len(prefix) :]
        if (
            receipt.get("projection_bytes") != len(projection)
            or receipt.get("projection_sha256") != sha(projection)
        ):
            raise FrontierReplayError(f"{where}:projection_identity")
        emitted = emitted_authorization(projection, classification)
        if (
            receipt.get("emitted_projection_authorization") != emitted
            or receipt.get("emitted_projection_authorization_sha256")
            != authorization_sha(emitted)
        ):
            raise FrontierReplayError(f"{where}:emitted_authorization")
        next_source = source(
            sequence,
            "rna_traversal_projection",
            classification,
            projection,
            visible,
        )
        sources.append(next_source)
        frontier = build_frontier(sources)
        if receipt.get("authorization_frontier_after") != frontier:
            raise FrontierReplayError(f"{where}:frontier_after")
    return frontier
