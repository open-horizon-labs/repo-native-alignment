#!/usr/bin/env python3
"""Opaque exact-title acquisition with projection-bound authorization."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve(strict=True).parent))

from live_identity import (
    LiveIdentityError,
    LiveIdentityVerifier,
    derive_projection_authorization,
)


HERE = Path(__file__).resolve().parent.parent
CONFIG = json.loads((HERE / "config/supervisor.json").read_text())

READY = "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def persist_raw(
    argv: list[str],
    result: subprocess.CompletedProcess[bytes],
    elapsed: float,
    live_before: dict[str, object],
    live_after: dict[str, object] | None,
    raw_ids: list[str],
    authorization: dict[str, object] | None,
    post_identity_error: str | None,
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
        "schema_version": "issue827-title-query-receipt-v1",
        "argv": argv,
        "root": CONFIG["root"],
        "identity_sha256": live_before["identity_sha256"],
        "cache_manifest_sha256": live_before["cache_manifest_sha256"],
        "cache_archive_sha256": live_before["cache_archive_sha256"],
        "launcher_sha256": live_before["launcher_sha256"],
        "binary_sha256": live_before["binary_sha256"],
        "repository_identity": live_before["repository_identity"],
        "elapsed_seconds": elapsed,
        "returncode": result.returncode,
        "stdout": {
            "path": str(stdout_path),
            "bytes": len(result.stdout),
            "sha256": sha(result.stdout),
        },
        "stderr": {
            "path": str(stderr_path),
            "bytes": len(result.stderr),
            "sha256": sha(result.stderr),
        },
        # Raw IDs are retained only to audit redaction.  They are never an
        # authorization source.
        "raw_stable_code_ids_observational_only": raw_ids,
        "projection_authorization": authorization,
        "projection_authorization_sha256": (
            sha(canonical(authorization)) if authorization is not None else None
        ),
        "live_identity_before": live_before,
        "live_identity_after": live_after,
        "post_identity_error": post_identity_error,
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
    if (
        args.query_sha256 != expected_query_sha
        or hashlib.sha256(query).hexdigest() != expected_query_sha
    ):
        print("RNA_QUERY_STATUS=ERROR query_identity", file=sys.stderr)
        return 42

    verifier = LiveIdentityVerifier(CONFIG, CONFIG["state"])
    try:
        live_before = verifier.verify("rna_query:before")
    except LiveIdentityError as exc:
        print(f"RNA_QUERY_STATUS=ERROR identity:{exc.reason}", file=sys.stderr)
        return 42

    argv = [
        CONFIG["launcher"],
        "search",
        "--repo",
        CONFIG["repo"],
        "--root",
        CONFIG["root"],
        "--compact",
        "--include-artifacts=false",
        f"--limit={CONFIG['result_limit']}",
        query.decode("utf-8"),
    ]
    started = time.monotonic()
    result = subprocess.run(
        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False
    )
    elapsed = time.monotonic() - started

    text: str | None
    projection: bytes | None = None
    authorization: dict[str, object] | None = None
    ready: str | None = None
    try:
        text = result.stdout.decode("utf-8", errors="strict")
        marker = "### Strict semantic qualification"
        projection_body = text.split(marker, 1)[0]
        ready = next(
            (line for line in text.splitlines() if line.strip("`") == READY), None
        )
        if result.returncode == 0 and ready is not None:
            projection = (
                projection_body.rstrip() + "\n\n" + ready + "\n"
            ).encode("utf-8")
            authorization = derive_projection_authorization(projection)
    except UnicodeError:
        text = None

    live_after: dict[str, object] | None = None
    post_identity_error: str | None = None
    try:
        live_after = verifier.verify("rna_query:after")
    except LiveIdentityError as exc:
        post_identity_error = exc.reason

    try:
        persist_raw(
            argv,
            result,
            elapsed,
            live_before,
            live_after,
            (
                derive_projection_authorization(result.stdout)["stable_code_ids"]
                if text is not None
                else []
            ),
            authorization,
            post_identity_error,
        )
    except (OSError, ValueError) as exc:
        print(f"RNA_QUERY_STATUS=ERROR evidence:{exc}", file=sys.stderr)
        return 42

    if post_identity_error is not None:
        print(
            f"RNA_QUERY_STATUS=ERROR identity_after:{post_identity_error}",
            file=sys.stderr,
        )
        return 42
    if result.returncode != 0:
        sys.stderr.buffer.write(result.stderr)
        return 42
    if text is None:
        print("RNA_QUERY_STATUS=ERROR stdout_utf8", file=sys.stderr)
        return 42
    if ready is None:
        print("RNA_QUERY_STATUS=ERROR readiness", file=sys.stderr)
        return 42
    if (
        authorization is None
        or not authorization["stable_code_ids"]
        or projection is None
    ):
        print("RNA_QUERY_STATUS=ERROR no_stable_code_id", file=sys.stderr)
        return 42
    sys.stdout.buffer.write(projection)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
