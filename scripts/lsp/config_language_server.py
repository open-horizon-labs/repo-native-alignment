#!/usr/bin/env python3
"""Small deterministic LSP for line-oriented project configuration files."""

from __future__ import annotations

import json
import re
import sys
from typing import Any, BinaryIO


VERSION = "1.0.0"
SECTION = re.compile(r"^\s*\[([^]]+)]\s*$")
KEY_VALUE = re.compile(r"^\s*([A-Za-z0-9_.-]+)\s*[:=]")
MAKE_TARGET = re.compile(r"^([A-Za-z0-9_.%/+-]+)\s*:(?!=)")


def read_message(stream: BinaryIO) -> dict[str, Any] | None:
    headers: dict[str, str] = {}
    while True:
        line = stream.readline()
        if not line:
            return None
        if line in {b"\r\n", b"\n"}:
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    payload = stream.read(length)
    if len(payload) != length:
        raise EOFError("truncated LSP payload")
    value = json.loads(payload)
    if not isinstance(value, dict):
        raise ValueError("LSP payload must be an object")
    return value


def write_message(stream: BinaryIO, value: dict[str, Any]) -> None:
    payload = json.dumps(value, separators=(",", ":"), sort_keys=True).encode()
    stream.write(f"Content-Length: {len(payload)}\r\n\r\n".encode())
    stream.write(payload)
    stream.flush()


def symbol(name: str, kind: int, line: int, end: int) -> dict[str, Any]:
    coordinate = {
        "start": {"line": line, "character": 0},
        "end": {"line": line, "character": max(end, 1)},
    }
    return {
        "name": name,
        "kind": kind,
        "range": coordinate,
        "selectionRange": coordinate,
    }


def document_symbols(text: str) -> list[dict[str, Any]]:
    symbols: list[dict[str, Any]] = []
    for line_number, line in enumerate(text.splitlines()):
        if not line.strip() or line.lstrip().startswith(("#", ";")):
            continue
        section = SECTION.match(line)
        if section:
            symbols.append(symbol(section.group(1), 3, line_number, len(line)))
            continue
        key = KEY_VALUE.match(line)
        if key:
            symbols.append(symbol(key.group(1), 7, line_number, len(line)))
            continue
        target = MAKE_TARGET.match(line)
        if target:
            symbols.append(symbol(target.group(1), 12, line_number, len(line)))
    return symbols


def serve(stdin: BinaryIO, stdout: BinaryIO) -> int:
    documents: dict[str, str] = {}
    shutdown = False
    while True:
        message = read_message(stdin)
        if message is None:
            return 0
        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params")
        params = params if isinstance(params, dict) else {}
        if method == "initialize":
            result: Any = {
                "capabilities": {
                    "documentSymbolProvider": True,
                    "textDocumentSync": {"openClose": True, "change": 1},
                },
                "serverInfo": {
                    "name": "rna-config-language-server",
                    "version": VERSION,
                },
            }
        elif method == "shutdown":
            shutdown = True
            result = None
        elif method == "exit":
            return 0 if shutdown else 1
        elif method == "textDocument/didOpen":
            document = params.get("textDocument", {})
            if isinstance(document, dict):
                uri, text = document.get("uri"), document.get("text")
                if isinstance(uri, str) and isinstance(text, str):
                    documents[uri] = text
            continue
        elif method == "textDocument/didChange":
            document = params.get("textDocument", {})
            changes = params.get("contentChanges", [])
            uri = document.get("uri") if isinstance(document, dict) else None
            if isinstance(uri, str) and isinstance(changes, list) and changes:
                change = changes[-1]
                if isinstance(change, dict) and isinstance(change.get("text"), str):
                    documents[uri] = change["text"]
            continue
        elif method == "textDocument/didClose":
            document = params.get("textDocument", {})
            uri = document.get("uri") if isinstance(document, dict) else None
            if isinstance(uri, str):
                documents.pop(uri, None)
            continue
        elif method == "textDocument/documentSymbol":
            document = params.get("textDocument", {})
            uri = document.get("uri") if isinstance(document, dict) else None
            result = document_symbols(documents.get(uri, "")) if isinstance(uri, str) else []
        elif request_id is None:
            continue
        else:
            write_message(
                stdout,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": f"unsupported method: {method}"},
                },
            )
            continue
        if request_id is not None:
            write_message(
                stdout,
                {"jsonrpc": "2.0", "id": request_id, "result": result},
            )


def main(argv: list[str]) -> int:
    if argv == ["--version"]:
        print(f"rna-config-language-server {VERSION}")
        return 0
    if argv:
        print("usage: rna-config-language-server [--version]", file=sys.stderr)
        return 2
    return serve(sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
