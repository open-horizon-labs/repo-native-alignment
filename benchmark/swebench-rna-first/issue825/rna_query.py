#!/usr/bin/env python3
"""Opaque exact-title acquisition wrapper; never exposes the immutable index path."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys


HERE = Path(__file__).resolve().parent.parent
CONFIG = json.loads((HERE / "config/supervisor.json").read_text())
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
    argv = [
        CONFIG["launcher"], "search", "--repo", CONFIG["repo"], "--root", CONFIG["root"],
        "--compact", "--include-artifacts=false", f"--limit={CONFIG['result_limit']}", query.decode("utf-8"),
    ]
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode != 0:
        sys.stderr.buffer.write(result.stderr)
        return 42
    text = result.stdout.decode("utf-8", errors="replace")
    marker = "### Strict semantic qualification"
    projection = text.split(marker, 1)[0]
    ready = next((line for line in text.splitlines() if line.startswith("`status=READY ")), None)
    if ready is None or "embeddings=true" not in ready or "retrieval=hybrid" not in ready or "rerank=true" not in ready or "fallback=false" not in ready:
        print("RNA_QUERY_STATUS=ERROR readiness", file=sys.stderr)
        return 42
    projection = projection.rstrip() + "\n\n" + ready + "\n"
    sys.stdout.write(projection)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
