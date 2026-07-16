#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "swebench_claude_executor.py"
SPEC = importlib.util.spec_from_file_location("swebench_claude_executor", SCRIPT)
assert SPEC and SPEC.loader
EXECUTOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EXECUTOR
SPEC.loader.exec_module(EXECUTOR)


class SwebenchClaudeExecutorTests(unittest.TestCase):
    def test_query_prefers_code_identifiers(self) -> None:
        prompt = """## Problem statement
kernS raises UnboundLocalError for an expression.
## Hints supplied by the benchmark
(none)
"""
        self.assertEqual(
            EXECUTOR.query_from_prompt(prompt),
            "kernS",
        )

    def test_baseline_empty_config_skips_preflight(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = Path(temporary) / "mcp.json"
            config.write_text('{"mcpServers": {}}\n', encoding="utf-8")
            context, report = EXECUTOR.acquire_rna_context(config, "task")
            self.assertIsNone(context)
            self.assertEqual(report["status"], "not_applicable")

    def test_rna_preflight_makes_real_search_call(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            server = root / "server.py"
            server.write_text(
                """import json,sys
for line in sys.stdin:
    request=json.loads(line)
    if request.get("id")==1:
        result={"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}
        print(json.dumps({"jsonrpc":"2.0","id":1,"result":result}),flush=True)
    elif request.get("id")==2:
        assert request["method"]=="tools/call"
        assert request["params"]["name"]=="search"
        result={"content":[{"type":"text","text":"RNA FOUND CONTEXT"}],"isError":False}
        print(json.dumps({"jsonrpc":"2.0","id":2,"result":result}),flush=True)
""",
                encoding="utf-8",
            )
            config = root / "mcp.json"
            config.write_text(
                json.dumps(
                    {
                        "mcpServers": {
                            "rna-server": {
                                "command": sys.executable,
                                "args": [str(server)],
                            }
                        }
                    }
                ),
                encoding="utf-8",
            )
            context, report = EXECUTOR.acquire_rna_context(
                config,
                "## Problem statement\ncycle_key is wrong\n"
                "## Hints supplied by the benchmark\n(none)\n",
            )
            self.assertEqual(context, "RNA FOUND CONTEXT")
            self.assertEqual(report["status"], "success")
            self.assertEqual(report["tool"], "search")
            self.assertIn("cycle_key", report["query"])


if __name__ == "__main__":
    unittest.main()
