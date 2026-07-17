#!/usr/bin/env python3
"""Deterministic stdio LSP fixture for capability/readiness CLI checks.

Set RNA_LSP_FIXTURE_SCENARIO (or the first argument) to document_zero
(default), document_features, workspace, method_not_found, crash, or timeout.
The document_features scenario exercises Markdown symbol/link/definition/
reference evidence without provisioning a real server.
"""

import json
import os
from pathlib import Path
import sys
import time


def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    payload = sys.stdin.buffer.read(length)
    if len(payload) != length:
        return None
    return json.loads(payload)


def send_message(payload):
    encoded = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(encoded)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(encoded)
    sys.stdout.buffer.flush()


def result(request_id, value):
    send_message({"jsonrpc": "2.0", "id": request_id, "result": value})


def rpc_error(request_id, code, message):
    send_message(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


def main():
    scenario = (
        sys.argv[1]
        if len(sys.argv) > 1
        else os.environ.get("RNA_LSP_FIXTURE_SCENARIO", "document_zero")
    )
    while True:
        message = read_message()
        if message is None:
            return 0
        method = message.get("method")
        request_id = message.get("id")

        if method == "initialize":
            if scenario == "workspace":
                capabilities = {"workspaceSymbolProvider": True}
            elif scenario == "document_features":
                capabilities = {
                    "referencesProvider": True,
                    "definitionProvider": True,
                    "documentLinkProvider": {"resolveProvider": False},
                    "documentSymbolProvider": True,
                }
            else:
                capabilities = {"documentSymbolProvider": True}
            result(request_id, {"capabilities": capabilities})
        elif method in ("workspace/symbol", "textDocument/documentSymbol"):
            if scenario == "method_not_found":
                rpc_error(request_id, -32601, "fixture method not found")
            elif scenario == "crash":
                return 17
            elif scenario == "timeout":
                time.sleep(60)
            elif scenario == "workspace":
                result(request_id, [{"name": "fixture-symbol"}])
            elif scenario == "document_features":
                result(
                    request_id,
                    [
                        {
                            "name": "Fixture guide",
                            "kind": 3,
                            "range": {
                                "start": {"line": 0, "character": 0},
                                "end": {"line": 2, "character": 68},
                            },
                        }
                    ],
                )
            else:
                result(request_id, [])
        elif method == "textDocument/documentLink" and scenario == "document_features":
            result(
                request_id,
                [{"range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 16}}, "target": (Path.cwd() / "src/app.py").as_uri()}],
            )
        elif method == "textDocument/definition" and scenario == "document_features":
            result(
                request_id,
                [{"uri": (Path.cwd() / "src/app.py").as_uri(), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}}],
            )
        elif method == "textDocument/references" and scenario == "document_features":
            result(
                request_id,
                [{"uri": (Path.cwd() / "tests/test_app.py").as_uri(), "range": {"start": {"line": 3, "character": 0}, "end": {"line": 3, "character": 10}}}],
            )
        elif method == "shutdown":
            result(request_id, None)
        elif method == "exit":
            return 0
        elif request_id is not None:
            rpc_error(request_id, -32601, "fixture method not found")


if __name__ == "__main__":
    raise SystemExit(main())
