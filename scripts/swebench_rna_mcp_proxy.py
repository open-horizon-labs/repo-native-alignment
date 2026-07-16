#!/usr/bin/env python3
"""Transparent newline-delimited stdio MCP proxy with an auditable JSONL trace."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, BinaryIO

from swebench_rna_one import utc_now


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rna-binary", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--trace", type=Path, required=True)
    parser.add_argument("--server-stderr", type=Path, required=True)
    return parser.parse_args()


def parse_message(line: bytes) -> dict[str, Any]:
    try:
        value = json.loads(line)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def trace_row(
    direction: str,
    line: bytes,
    pending: dict[Any, dict[str, Any]],
) -> dict[str, Any]:
    message = parse_message(line)
    message_id = message.get("id")
    method = message.get("method")
    response_to_method = None
    response_to_tool = None
    request_params = None
    response_to_params = None
    if direction == "client_to_server" and method and message_id is not None:
        params = message.get("params")
        tool_name = (
            params.get("name")
            if method == "tools/call" and isinstance(params, dict)
            else None
        )
        request_params = params if method == "tools/call" else None
        pending[message_id] = {
            "method": str(method),
            "tool_name": tool_name,
            "request_params": request_params,
        }
    elif direction == "server_to_client" and message_id is not None:
        request = pending.pop(message_id, None)
        if request:
            response_to_method = request["method"]
            response_to_tool = request["tool_name"]
            response_to_params = request["request_params"]
    params = message.get("params")
    tool_name = None
    if method == "tools/call" and isinstance(params, dict):
        tool_name = params.get("name")
    return {
        "observed_at": utc_now(),
        "direction": direction,
        "message_bytes": len(line),
        "message_sha256": hashlib.sha256(line).hexdigest(),
        "id": message_id,
        "method": method,
        "request_params": request_params,
        "response_to_method": response_to_method,
        "response_to_tool": response_to_tool,
        "response_to_params": response_to_params,
        "tool_name": tool_name,
        "is_error": "error" in message,
    }


def main() -> int:
    args = arguments()
    args.trace.parent.mkdir(parents=True, exist_ok=True)
    args.server_stderr.parent.mkdir(parents=True, exist_ok=True)
    pending: dict[Any, dict[str, Any]] = {}
    lock = threading.Lock()
    with args.server_stderr.open("wb") as server_stderr:
        server = subprocess.Popen(
            [str(args.rna_binary), "--repo", str(args.repo)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=server_stderr,
            bufsize=0,
        )

        def record(direction: str, line: bytes) -> None:
            row = trace_row(direction, line, pending)
            with lock, args.trace.open("a", encoding="utf-8") as trace:
                trace.write(json.dumps(row, sort_keys=True) + "\n")

        def forward(
            source: BinaryIO, destination: BinaryIO, direction: str
        ) -> None:
            try:
                while True:
                    line = source.readline()
                    if not line:
                        break
                    record(direction, line)
                    destination.write(line)
                    destination.flush()
            finally:
                if direction == "client_to_server":
                    with contextlib.suppress(BrokenPipeError):
                        destination.close()

        assert server.stdin is not None
        assert server.stdout is not None
        upstream = threading.Thread(
            target=forward,
            args=(sys.stdin.buffer, server.stdin, "client_to_server"),
            daemon=True,
        )
        downstream = threading.Thread(
            target=forward,
            args=(server.stdout, sys.stdout.buffer, "server_to_client"),
            daemon=True,
        )
        upstream.start()
        downstream.start()
        downstream.join()
        upstream.join(timeout=2)
        return server.wait()


if __name__ == "__main__":
    raise SystemExit(main())
