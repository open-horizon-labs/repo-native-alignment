#!/usr/bin/env python3
"""Deterministic stdio LSP fixture for capability/readiness CLI checks.

Set RNA_LSP_FIXTURE_SCENARIO (or the first argument) to document_zero
(default), document_features, document_definition_error, python_features,
workspace, method_not_found, crash, or timeout. The deterministic feature
scenarios exercise Markdown and Python evidence without provisioning a server.
"""

import json
import os
from pathlib import Path
import re
import sys
import time
from urllib.parse import unquote, urlparse


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


def lsp_character(text):
    return len(text.encode("utf-16-le")) // 2


def repository_file_uri(message, relative_path):
    uri = message.get("params", {}).get("textDocument", {}).get("uri", "")
    path = Path(unquote(urlparse(uri).path))
    root = path.parent.parent if path.parent.name in ("docs", "src", "tests") else path.parent
    return (root / relative_path).as_uri()


def document_link_result(uri):
    """Return the src/app.py link at its exact range in the requested document."""
    if not uri:
        return []
    path = Path(unquote(urlparse(uri).path))
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return []
    pattern = re.compile(r"\[[^\]]+\]\((?:\.\./)?src/app\.py\)")
    for line_number, line in enumerate(lines):
        match = pattern.search(line)
        if match is None:
            continue
        return [
            {
                "range": {
                    "start": {
                        "line": line_number,
                        "character": lsp_character(line[: match.start()]),
                    },
                    "end": {
                        "line": line_number,
                        "character": lsp_character(line[: match.end()]),
                    },
                },
                "target": (
                    (path.parent.parent if path.parent.name == "docs" else path.parent)
                    / "src/app.py"
                ).as_uri(),
            }
        ]
    return []


def python_document_symbols(uri):
    path = Path(unquote(urlparse(uri).path))
    if path.as_posix().endswith("/src/app.py"):
        return [
            {
                "name": "greet",
                "kind": 12,
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 1, "character": 27},
                },
            }
        ]
    if path.as_posix().endswith("/tests/test_app.py"):
        # Valid processed-zero response: test functions are represented through
        # the reference edge returned for the source declaration below.
        return []
    return []


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
            elif scenario in ("document_features", "document_definition_error"):
                capabilities = {
                    "referencesProvider": True,
                    "definitionProvider": True,
                    "documentLinkProvider": {"resolveProvider": False},
                    "documentSymbolProvider": True,
                }
            elif scenario == "python_features":
                capabilities = {
                    "referencesProvider": True,
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
            elif scenario in ("document_features", "document_definition_error"):
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
            elif scenario == "python_features":
                uri = message.get("params", {}).get("textDocument", {}).get("uri", "")
                result(request_id, python_document_symbols(uri))
            else:
                result(request_id, [])
        elif method == "textDocument/documentLink" and scenario in (
            "document_features",
            "document_definition_error",
        ):
            uri = message.get("params", {}).get("textDocument", {}).get("uri", "")
            result(request_id, document_link_result(uri))
        elif method == "textDocument/definition" and scenario in (
            "document_features",
            "document_definition_error",
        ):
            if scenario == "document_definition_error":
                rpc_error(request_id, -32603, "fixture definition failure")
            else:
                result(
                    request_id,
                    [{"uri": repository_file_uri(message, "src/app.py"), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}}],
                )
        elif method == "textDocument/references" and scenario in (
            "document_features",
            "document_definition_error",
        ):
            result(
                request_id,
                [{"uri": repository_file_uri(message, "tests/test_app.py"), "range": {"start": {"line": 3, "character": 0}, "end": {"line": 3, "character": 10}}}],
            )
        elif method == "textDocument/references" and scenario == "python_features":
            result(
                request_id,
                [{"uri": repository_file_uri(message, "tests/test_app.py"), "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 14}}}],
            )
        elif method == "shutdown":
            result(request_id, None)
        elif method == "exit":
            return 0
        elif request_id is not None:
            rpc_error(request_id, -32601, "fixture method not found")


if __name__ == "__main__":
    raise SystemExit(main())
