#!/usr/bin/env python3
"""Deterministic LSP for the frozen cohort's small, otherwise-unserved languages.

Each parser emits symbols only for concrete declarations present in the file.
Zero symbols is a valid result; the server never fabricates placeholder nodes.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from typing import Any, BinaryIO, Callable


VERSION = "1.0.0"
PARSERS: dict[str, list[tuple[re.Pattern[str], int]]] = {
    "autotools": [
        (re.compile(r"^\s*(?:AC|AM|AS|AX)_[A-Z0-9_]+\s*\(\s*\[?([^],]+)", re.I), 12),
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?::=|\?=|\+=|=)"), 13),
        (re.compile(r"^([A-Za-z0-9_.%/+-]+)\s*:(?!=)"), 12),
    ],
    "batch": [
        (re.compile(r"^\s*:([A-Za-z0-9_.-]+)\s*$"), 12),
        (re.compile(r"^\s*(?:set\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=" , re.I), 13),
    ],
    "plantuml": [
        (re.compile(r"^\s*(?:abstract\s+)?(?:class|interface|enum|actor|component|package|node|database)\s+[\"']?([^\s\"'{]+)", re.I), 5),
    ],
    "roff": [
        (re.compile(r'^\.(?:TH|SH|SS)\s+\"?([^\"]+?)\"?\s*$'), 3),
    ],
    "autolev": [
        (re.compile(r"^\s*(CONSTANTS|MOTIONVARIABLES'?|NEWTONIAN|BODIES|POINTS?|PARTICLES?|FRAMES?)\s+(.+)$", re.I), 13),
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*="), 13),
    ],
    "antlr": [
        (re.compile(r"^\s*(?:lexer\s+|parser\s+)?grammar\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"), 2),
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:\[[^]]*])?\s*(?:returns\s*\[[^]]*])?\s*:"), 12),
    ],
    "lex": [
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s+[^\s].*$"), 13),
        (re.compile(r"^\s*(?:static\s+)?(?:int|void|char|double|float)\s+([A-Za-z_][A-Za-z0-9_]*)\s*\("), 12),
    ],
    "emacs-lisp": [
        (re.compile(r"^\s*\((?:defun|defmacro|defsubst|defvar|defconst|defcustom|define-minor-mode)\s+([^\s()]+)"), 12),
    ],
    "scheme": [
        (re.compile(r"^\s*\((?:define|define-public|define-module)\s+\(?([^\s()]+)"), 12),
        (re.compile(r"^\s*\(plugin-configure\s+([^\s()]+)"), 2),
    ],
    "powershell": [
        (re.compile(r"^\s*function\s+([A-Za-z_][A-Za-z0-9_-]*)", re.I), 12),
        (re.compile(r"^\s*\$([A-Za-z_][A-Za-z0-9_]*)\s*="), 13),
    ],
    "starlark": [
        (re.compile(r"^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\("), 12),
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*="), 13),
    ],
    "cohort-text": [
        (re.compile(r"^\s*(?:#+|={2,})\s*([^#=].+?)\s*$"), 3),
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_.-]+)\s*[:=]"), 7),
    ],
    "plaintext": [
        (re.compile(r"^\s*(?:#+|={2,})\s*([^#=].+?)\s*$"), 3),
        (re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_.-]+)\s*[:=]"), 7),
    ],
}


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


def symbol(name: str, kind: int, line: int, width: int) -> dict[str, Any]:
    coordinate = {
        "start": {"line": line, "character": 0},
        "end": {"line": line, "character": max(width, 1)},
    }
    return {"name": name, "kind": kind, "range": coordinate, "selectionRange": coordinate}


def document_symbols(language: str, text: str) -> list[dict[str, Any]]:
    output: list[dict[str, Any]] = []
    for line_number, line in enumerate(text.splitlines()):
        for pattern, kind in PARSERS[language]:
            match = pattern.match(line)
            if not match:
                continue
            name = match.group(match.lastindex or 1).strip().strip('"\'')
            if name:
                output.append(symbol(name, kind, line_number, len(line)))
            break
    return output


def serve(language: str, stdin: BinaryIO, stdout: BinaryIO) -> int:
    documents: dict[str, str] = {}
    shutdown = False
    while True:
        message = read_message(stdin)
        if message is None:
            return 0
        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params") if isinstance(message.get("params"), dict) else {}
        if method == "initialize":
            result: Any = {
                "capabilities": {
                    "documentSymbolProvider": True,
                    "textDocumentSync": {"openClose": True, "change": 1},
                },
                "serverInfo": {"name": "rna-cohort-language-server", "version": VERSION},
            }
        elif method == "shutdown":
            shutdown = True
            result = None
        elif method == "exit":
            return 0 if shutdown else 1
        elif method == "textDocument/didOpen":
            document = params.get("textDocument", {})
            if isinstance(document, dict) and isinstance(document.get("uri"), str) and isinstance(document.get("text"), str):
                documents[document["uri"]] = document["text"]
            continue
        elif method == "textDocument/didChange":
            document = params.get("textDocument", {})
            changes = params.get("contentChanges", [])
            uri = document.get("uri") if isinstance(document, dict) else None
            if isinstance(uri, str) and isinstance(changes, list) and changes and isinstance(changes[-1], dict) and isinstance(changes[-1].get("text"), str):
                documents[uri] = changes[-1]["text"]
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
            result = document_symbols(language, documents.get(uri, "")) if isinstance(uri, str) else []
        elif request_id is None:
            continue
        else:
            write_message(stdout, {"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": f"unsupported method: {method}"}})
            continue
        if request_id is not None:
            write_message(stdout, {"jsonrpc": "2.0", "id": request_id, "result": result})


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", action="store_true")
    parser.add_argument("--language", choices=sorted(PARSERS))
    args = parser.parse_args(argv)
    if args.version:
        print(f"rna-cohort-language-server {VERSION}")
        return 0
    if not args.language:
        parser.error("--language is required")
    return serve(args.language, sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
