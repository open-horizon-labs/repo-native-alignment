from __future__ import annotations

import io
import json
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
CONFIG_SERVER = ROOT / "scripts" / "lsp" / "config_language_server.py"
COHORT_SERVER = ROOT / "scripts" / "lsp" / "cohort_language_server.py"


def encode_message(message: dict[str, Any]) -> bytes:
    payload = json.dumps(message, separators=(",", ":"), sort_keys=True).encode()
    return f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload


def decode_messages(output: bytes) -> list[dict[str, Any]]:
    stream = io.BytesIO(output)
    messages: list[dict[str, Any]] = []
    while stream.tell() < len(output):
        headers: dict[str, str] = {}
        while True:
            line = stream.readline()
            if not line:
                raise AssertionError("truncated LSP headers")
            if line in {b"\r\n", b"\n"}:
                break
            name, value = line.decode("ascii").split(":", 1)
            headers[name.lower()] = value.strip()
        length = int(headers["content-length"])
        payload = stream.read(length)
        if len(payload) != length:
            raise AssertionError("truncated LSP payload")
        message = json.loads(payload)
        if not isinstance(message, dict):
            raise AssertionError("LSP response must be a JSON object")
        messages.append(message)
    return messages


class SwebenchLspShimTests(unittest.TestCase):
    def exercise_server(
        self,
        command: list[str],
        *,
        language_id: str,
        uri: str,
        text: str,
        expected_names: list[str],
    ) -> None:
        messages = [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": None,
                    "rootUri": None,
                    "capabilities": {"textDocument": {"documentSymbol": {}}},
                },
            },
            {"jsonrpc": "2.0", "method": "initialized", "params": {}},
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id,
                        "version": 1,
                        "text": text,
                    }
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "textDocument/documentSymbol",
                "params": {"textDocument": {"uri": uri}},
            },
            {"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}},
            {"jsonrpc": "2.0", "method": "exit", "params": {}},
        ]

        completed = subprocess.run(
            command,
            input=b"".join(encode_message(message) for message in messages),
            capture_output=True,
            timeout=5,
            check=False,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        self.assertEqual(completed.stderr, b"")
        responses = decode_messages(completed.stdout)

        # Notifications, including initialized, must remain quiescent.
        self.assertEqual([response.get("id") for response in responses], [1, 2, 3])
        initialize = responses[0]
        self.assertEqual(initialize.get("jsonrpc"), "2.0")
        self.assertIs(
            initialize["result"]["capabilities"]["documentSymbolProvider"],
            True,
        )

        symbols = responses[1].get("result")
        self.assertIsInstance(symbols, list)
        self.assertTrue(symbols)
        self.assertEqual([symbol["name"] for symbol in symbols], expected_names)
        for symbol in symbols:
            self.assertIsInstance(symbol, dict)
            self.assertIsInstance(symbol["name"], str)
            self.assertTrue(symbol["name"])
            self.assertIs(type(symbol["kind"]), int)
            self.assertGreater(symbol["kind"], 0)
            self.assertEqual(symbol["range"], symbol["selectionRange"])
            start = symbol["range"]["start"]
            end = symbol["range"]["end"]
            self.assertGreaterEqual(start["line"], 0)
            self.assertEqual(start["character"], 0)
            self.assertEqual(end["line"], start["line"])
            self.assertGreater(end["character"], start["character"])

        self.assertEqual(responses[2], {"jsonrpc": "2.0", "id": 3, "result": None})

    def test_config_server_lsp_lifecycle_and_document_symbols(self) -> None:
        self.exercise_server(
            [sys.executable, str(CONFIG_SERVER)],
            language_id="ini",
            uri="file:///fixture/settings.ini",
            text="[database]\nhost = localhost\n",
            expected_names=["database", "host"],
        )

    def test_cohort_server_lsp_lifecycle_and_document_symbols(self) -> None:
        self.exercise_server(
            [sys.executable, str(COHORT_SERVER), "--language", "starlark"],
            language_id="starlark",
            uri="file:///fixture/defs.bzl",
            text="def build_target(name):\n    rule_name = name\n",
            expected_names=["build_target", "rule_name"],
        )


if __name__ == "__main__":
    unittest.main()
