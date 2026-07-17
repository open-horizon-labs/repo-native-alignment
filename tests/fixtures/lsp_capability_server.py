#!/usr/bin/env python3
"""Deterministic stdio LSP fixture for capability/readiness CLI checks.

Set RNA_LSP_FIXTURE_SCENARIO to document_zero (default), workspace,
method_not_found, crash, or timeout. The fixture deliberately advertises only
the readiness capability exercised by the scenario.
"""

import json
import os
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
    scenario = os.environ.get("RNA_LSP_FIXTURE_SCENARIO", "document_zero")
    while True:
        message = read_message()
        if message is None:
            return 0
        method = message.get("method")
        request_id = message.get("id")

        if method == "initialize":
            capabilities = (
                {"workspaceSymbolProvider": True}
                if scenario == "workspace"
                else {"documentSymbolProvider": True}
            )
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
            else:
                result(request_id, [])
        elif method == "shutdown":
            result(request_id, None)
        elif method == "exit":
            return 0
        elif request_id is not None:
            rpc_error(request_id, -32601, "fixture method not found")


if __name__ == "__main__":
    raise SystemExit(main())
