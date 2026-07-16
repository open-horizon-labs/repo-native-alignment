#!/usr/bin/env python3
"""Claude Code executor with deterministic optional RNA MCP context acquisition."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Mapping, Sequence


class ExecutorError(RuntimeError):
    """A preflight or executor failure."""


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def query_from_prompt(prompt: str) -> str:
    problem = prompt.split("## Problem statement", 1)[-1].split(
        "## Hints supplied by the benchmark", 1
    )[0]
    candidates = re.findall(r"[A-Za-z_][A-Za-z0-9_]{3,}", problem)
    stop = {
        "from",
        "with",
        "that",
        "this",
        "when",
        "where",
        "should",
        "error",
        "issue",
        "using",
        "does",
        "have",
        "into",
    }
    prioritized = [
        token
        for token in candidates
        if token.lower() not in stop
        and ("_" in token or any(char.isupper() for char in token))
    ]
    remaining = [
        token for token in candidates if token.lower() not in stop and token not in prioritized
    ]
    for token in [*prioritized, *remaining]:
        return token
    return "repository entry point"


def receive_response(process: subprocess.Popen[bytes], request_id: int) -> Mapping[str, Any]:
    assert process.stdout is not None
    while True:
        line = process.stdout.readline()
        if not line:
            raise ExecutorError(f"MCP server exited before response {request_id}")
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and value.get("id") == request_id:
            return value


def send(process: subprocess.Popen[bytes], message: Mapping[str, Any]) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message, separators=(",", ":")) + "\n").encode())
    process.stdin.flush()


def acquire_rna_context(config_path: Path, prompt: str) -> tuple[str | None, dict[str, Any]]:
    config = read_json(config_path)
    servers = config.get("mcpServers", {}) if isinstance(config, dict) else {}
    if not isinstance(servers, dict) or not servers:
        return None, {
            "status": "not_applicable",
            "reason": "MCP configuration contains no repository server",
        }
    if set(servers) != {"rna-server"}:
        raise ExecutorError("paired executor accepts only the `rna-server` MCP endpoint")
    server = servers["rna-server"]
    if not isinstance(server, dict) or not isinstance(server.get("command"), str):
        raise ExecutorError("invalid rna-server MCP configuration")
    command = [server["command"], *server.get("args", [])]
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "swebench-paired-preflight",
                        "version": "1",
                    },
                },
            },
        )
        initialized = receive_response(process, 1)
        if "error" in initialized:
            raise ExecutorError(f"MCP initialize failed: {initialized['error']}")
        send(
            process,
            {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        )
        query = query_from_prompt(prompt)
        text = ""
        attempts = 0
        for request_id in range(2, 17):
            attempts += 1
            send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "search",
                        "arguments": {"query": query, "compact": True},
                    },
                },
            )
            response = receive_response(process, request_id)
            if "error" in response:
                raise ExecutorError(f"RNA search failed: {response['error']}")
            result = response.get("result")
            if not isinstance(result, dict) or result.get("isError") is True:
                raise ExecutorError(f"RNA search returned an error result: {result}")
            content = result.get("content", [])
            text = "\n".join(
                str(item.get("text", ""))
                for item in content
                if isinstance(item, dict) and item.get("type") == "text"
            ).strip()
            if text and "index building" not in text.lower():
                break
            time.sleep(2)
        if not text or "index building" in text.lower():
            raise ExecutorError("RNA search did not become ready within 30 seconds")
        return text[:24000], {
            "status": "success",
            "tool": "search",
            "query": query,
            "attempts": attempts,
            "delivered_characters": min(len(text), 24000),
        }
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        if process.stdin is not None:
            process.stdin.close()
        if process.stdout is not None:
            process.stdout.close()


def arguments(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", default="high")
    parser.add_argument("--max-budget-usd", type=float, required=True)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = arguments(argv)
    task_path = Path(os.environ["SWEBENCH_TASK_PROMPT"])
    config_path = Path(os.environ["SWEBENCH_MCP_CONFIG"])
    run_dir = Path(os.environ["SWEBENCH_RUN_DIR"])
    prompt = task_path.read_text(encoding="utf-8")
    try:
        context, preflight = acquire_rna_context(config_path, prompt)
        (run_dir / "executor-preflight.json").write_text(
            json.dumps(preflight, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        runtime_prompt = prompt
        if context is not None:
            (run_dir / "rna-preflight-context.txt").write_text(
                context + "\n", encoding="utf-8"
            )
            runtime_prompt += (
                "\n## RNA orientation context acquired before editing\n\n"
                + context
                + "\n"
            )
        command = [
            "claude",
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--strict-mcp-config",
            "--mcp-config",
            str(config_path),
            "--permission-mode",
            "bypassPermissions",
            "--model",
            args.model,
            "--effort",
            args.effort,
            "--max-budget-usd",
            str(args.max_budget_usd),
            "--no-session-persistence",
            runtime_prompt,
        ]
        return subprocess.run(command, check=False).returncode
    except Exception as error:
        (run_dir / "executor-preflight.json").write_text(
            json.dumps(
                {
                    "status": "failed",
                    "error": f"{type(error).__name__}: {error}",
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        print(f"executor preflight failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
