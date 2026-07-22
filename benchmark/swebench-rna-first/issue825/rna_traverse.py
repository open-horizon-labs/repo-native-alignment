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


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


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

    sequence = int(state.get("rna_calls", 0)) + 1
    state["rna_calls"] = sequence
    initial_ids = set(CONFIG["initial_ids"])
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
