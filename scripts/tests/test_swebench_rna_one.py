#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "swebench_rna_one.py"
PROXY_SCRIPT = ROOT / "scripts" / "swebench_rna_mcp_proxy.py"
FIXTURES = ROOT / "scripts" / "tests" / "fixtures"
SPEC = importlib.util.spec_from_file_location("swebench_rna_one", SCRIPT)
assert SPEC and SPEC.loader
HARNESS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HARNESS
SPEC.loader.exec_module(HARNESS)
PROXY_SPEC = importlib.util.spec_from_file_location(
    "swebench_rna_mcp_proxy", PROXY_SCRIPT
)
assert PROXY_SPEC and PROXY_SPEC.loader
PROXY = importlib.util.module_from_spec(PROXY_SPEC)
sys.modules[PROXY_SPEC.name] = PROXY
PROXY_SPEC.loader.exec_module(PROXY)


class SwebenchRnaOneTests(unittest.TestCase):
    def test_isolated_checkout_has_one_local_commit_and_no_remote(self) -> None:
        instance = json.loads(
            (FIXTURES / "swebench_instance.json").read_text(encoding="utf-8")
        )
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary) / "checkout"
            proof = HARNESS.materialize_checkout(
                instance,
                checkout,
                fixture_source=FIXTURES / "repo",
            )
            self.assertEqual(proof["history_commit_count"], 1)
            self.assertEqual(proof["remotes"], [])
            self.assertEqual(
                subprocess.check_output(
                    ["git", "remote", "-v"], cwd=checkout, text=True
                ).strip(),
                "",
            )

    def test_meaningful_changes_preserves_both_rename_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            subprocess.run(["git", "init", "--quiet"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "config", "user.name", "Test"], cwd=checkout, check=True
            )
            subprocess.run(
                ["git", "config", "user.email", "test@example.invalid"],
                cwd=checkout,
                check=True,
            )
            (checkout / "old.py").write_text("value = 1\n", encoding="utf-8")
            subprocess.run(["git", "add", "old.py"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "commit", "--quiet", "-m", "base"], cwd=checkout, check=True
            )
            subprocess.run(
                ["git", "mv", "old.py", "new.py"], cwd=checkout, check=True
            )
            self.assertEqual(
                HARNESS.meaningful_checkout_changes(checkout),
                ["new.py", "old.py"],
            )

    def test_evaluator_command_uses_official_entrypoint_and_single_instance(self) -> None:
        command = HARNESS.build_evaluator_command(
            python="/python",
            dataset_name="/run/dataset-instance.json",
            predictions_path=Path("/run/prediction.jsonl"),
            instance_id="django__django-13279",
            run_id="rna-test",
        )
        self.assertEqual(command[:3], ["/python", "-m", "swebench.harness.run_evaluation"])
        self.assertIn("/run/dataset-instance.json", command)
        self.assertEqual(command[command.index("--max_workers") + 1], "1")
        self.assertEqual(
            command[command.index("--instance_ids") + 1],
            "django__django-13279",
        )

    def test_stage_ledger_keeps_every_category_explicitly_unknown(self) -> None:
        ledger = HARNESS.stage_ledger_skeleton()
        self.assertEqual(set(ledger["stages"]), set(HARNESS.STAGE_NAMES))
        for stage in HARNESS.STAGE_NAMES:
            for field in HARNESS.TOKEN_FIELDS:
                self.assertEqual(ledger["stages"][stage][field]["status"], "unknown")
                self.assertIsNone(ledger["stages"][stage][field]["value"])

    def test_baseline_stage_ledger_marks_rna_metrics_not_applicable(self) -> None:
        ledger = HARNESS.stage_ledger_skeleton(arm="baseline")
        stage = ledger["stages"]["rna_tool_results_orientation_and_planning"]
        self.assertEqual(stage["mcp_calls"]["status"], "not_applicable")
        self.assertEqual(stage["delivered_bytes"]["status"], "not_applicable")

    def test_task_prompt_is_identical_without_rna_specific_instruction(self) -> None:
        instance = json.loads(
            (FIXTURES / "swebench_instance.json").read_text(encoding="utf-8")
        )
        with tempfile.TemporaryDirectory() as temporary:
            prompt = Path(temporary) / "task.md"
            HARNESS.make_task_prompt(instance, prompt)
            text = prompt.read_text(encoding="utf-8")
            self.assertIn("repository tools available in this run", text)
            self.assertIn("complete at least one successful call before", text)
            self.assertNotIn("rna-server", text)

    def test_mcp_summary_counts_only_tool_results_before_first_edit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary) / "mcp.jsonl"
            rows = [
                {
                    "observed_at": "2026-01-01T00:00:00+00:00",
                    "direction": "client_to_server",
                    "method": "initialize",
                    "message_bytes": 10,
                },
                {
                    "observed_at": "2026-01-01T00:00:01+00:00",
                    "direction": "client_to_server",
                    "method": "tools/call",
                    "tool_name": "search",
                    "message_bytes": 20,
                },
                {
                    "observed_at": "2026-01-01T00:00:02+00:00",
                    "direction": "server_to_client",
                    "response_to_method": "tools/call",
                    "response_to_tool": "search",
                    "message_bytes": 300,
                },
                {
                    "observed_at": "2026-01-01T00:00:04+00:00",
                    "direction": "client_to_server",
                    "method": "tools/call",
                    "tool_name": "search",
                    "message_bytes": 20,
                },
                {
                    "observed_at": "2026-01-01T00:00:05+00:00",
                    "direction": "server_to_client",
                    "response_to_method": "tools/call",
                    "response_to_tool": "search",
                    "message_bytes": 500,
                },
            ]
            HARNESS.write_jsonl(trace, rows)
            summary = HARNESS.summarize_mcp_trace(
                trace, first_edit_at="2026-01-01T00:00:03+00:00"
            )
            self.assertEqual(summary["tool_calls"], 2)
            self.assertEqual(summary["orientation_tool_calls"], 1)
            self.assertEqual(
                summary["orientation_delivered_tool_result_bytes"], 300
            )
            self.assertEqual(summary["successful_orientation_tool_responses"], 1)
            self.assertTrue(summary["observed_real_mcp_use"])

    def test_mcp_summary_rejects_handshake_only_and_error_only_traffic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary) / "mcp.jsonl"
            HARNESS.write_jsonl(
                trace,
                [
                    {
                        "direction": "client_to_server",
                        "method": "initialize",
                        "observed_at": "2026-01-01T00:00:00+00:00",
                    },
                    {
                        "direction": "client_to_server",
                        "method": "tools/call",
                        "observed_at": "2026-01-01T00:00:01+00:00",
                    },
                    {
                        "direction": "server_to_client",
                        "response_to_method": "tools/call",
                        "is_error": True,
                        "observed_at": "2026-01-01T00:00:02+00:00",
                    },
                ],
            )
            summary = HARNESS.summarize_mcp_trace(
                trace, first_edit_at="2026-01-01T00:00:03+00:00"
            )
            self.assertFalse(summary["observed_real_mcp_use"])
            self.assertEqual(summary["successful_tool_responses"], 0)

            pending = {}
            PROXY.trace_row(
                "client_to_server",
                b'{"jsonrpc":"2.0","id":8,"method":"tools/call","params":'
                b'{"name":"search","arguments":{"query":"missing"}}}\n',
                pending,
            )
            mcp_error = PROXY.trace_row(
                "server_to_client",
                b'{"jsonrpc":"2.0","id":8,"result":{"isError":true,'
                b'"content":[{"type":"text","text":"failed"}]}}\n',
                pending,
            )
            self.assertTrue(mcp_error["is_error"])

            pending = {}
            PROXY.trace_row(
                "client_to_server",
                b'{"jsonrpc":"2.0","id":9,"method":"tools/call","params":'
                b'{"name":"search","arguments":{"query":"pending"}}}\n',
                pending,
            )
            nonterminal = PROXY.trace_row(
                "server_to_client",
                b'{"jsonrpc":"2.0","id":9,"method":"notifications/progress"}\n',
                pending,
            )
            self.assertFalse(nonterminal["is_response"])
            self.assertIsNone(nonterminal["response_to_method"])
            self.assertIn(9, pending)

    def test_proxy_trace_correlates_arguments_and_response_hash(self) -> None:
        pending = {}
        request = (
            b'{"jsonrpc":"2.0","id":7,"method":"tools/call","params":'
            b'{"name":"search","arguments":{"query":"SessionBase"}}}\n'
        )
        response = b'{"jsonrpc":"2.0","id":7,"result":{"content":[]}}\n'
        request_row = PROXY.trace_row("client_to_server", request, pending)
        response_row = PROXY.trace_row("server_to_client", response, pending)
        self.assertEqual(
            request_row["request_params"]["arguments"]["query"], "SessionBase"
        )
        self.assertEqual(response_row["response_to_tool"], "search")
        self.assertEqual(
            response_row["response_to_params"]["arguments"]["query"], "SessionBase"
        )
        self.assertEqual(len(response_row["message_sha256"]), 64)

    def test_provider_cache_tokens_are_recorded_without_inference(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary) / "executor.jsonl"
            rows = [
                {
                    "observed_at": "2026-01-01T00:00:01+00:00",
                    "observed_monotonic": 1.0,
                    "line": json.dumps(
                        {
                            "type": "assistant",
                            "message": {
                                "id": "message-1",
                                "usage": {
                                    "input_tokens": 2,
                                    "cache_creation_input_tokens": 30,
                                    "cache_read_input_tokens": 400,
                                    "output_tokens": 5,
                                }
                            },
                        }
                    ),
                },
                {
                    "observed_at": "2026-01-01T00:00:01.5+00:00",
                    "observed_monotonic": 1.5,
                    "line": json.dumps(
                        {
                            "type": "assistant",
                            "message": {
                                "id": "message-1",
                                "usage": {
                                    "input_tokens": 3,
                                    "cache_creation_input_tokens": 40,
                                    "cache_read_input_tokens": 500,
                                    "output_tokens": 6,
                                },
                            },
                        }
                    ),
                },
                {
                    "observed_at": "2026-01-01T00:00:03+00:00",
                    "observed_monotonic": 3.0,
                    "line": json.dumps(
                        {
                            "type": "result",
                            "total_cost_usd": 0.25,
                            "usage": {
                                "input_tokens": 7,
                                "cache_creation_input_tokens": 80,
                                "cache_read_input_tokens": 900,
                                "output_tokens": 11,
                            },
                        }
                    ),
                },
            ]
            HARNESS.write_jsonl(trace, rows)
            usage, _ = HARNESS.collect_usage_and_fallbacks(
                trace, first_edit_monotonic=2.0
            )
            self.assertEqual(usage["before"]["cache_creation_input_tokens"], 40)
            self.assertEqual(usage["before"]["cache_read_input_tokens"], 500)
            self.assertEqual(usage["totals"]["cache_creation_input_tokens"], 80)
            self.assertEqual(usage["totals"]["cache_read_input_tokens"], 900)
            self.assertEqual(usage["totals"]["cost_usd"], 0.25)

    def test_stage_report_wins_and_unobserved_handoff_stays_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "executor-report.json"
            report.write_text(
                json.dumps(
                    {
                        "stages": {
                            "frontier_before_first_edit": {"input_tokens": 99}
                        }
                    }
                ),
                encoding="utf-8",
            )
            ledger = HARNESS.stage_ledger_skeleton()
            HARNESS.merge_stage_ledger(
                ledger,
                executor_report=report,
                usage={
                    "before": {"input_tokens": 5},
                    "after": {"input_tokens": 7},
                    "totals": {},
                },
                mcp_summary={
                    "orientation_delivered_tool_result_bytes": 1,
                    "orientation_tool_calls": 1,
                },
            )
            self.assertEqual(
                ledger["stages"]["frontier_before_first_edit"]["input_tokens"][
                    "value"
                ],
                99,
            )
            self.assertEqual(
                ledger["stages"]["first_edit_through_handoff"]["input_tokens"][
                    "status"
                ],
                "unknown",
            )
            self.assertEqual(
                ledger["observed_intervals"][
                    "post_first_edit_until_executor_exit"
                ]["input_tokens"]["value"],
                7,
            )

    def test_collect_patch_includes_untracked_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            subprocess.run(["git", "init", "--quiet"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "config", "user.name", "Test"], cwd=checkout, check=True
            )
            subprocess.run(
                ["git", "config", "user.email", "test@example.invalid"],
                cwd=checkout,
                check=True,
            )
            (checkout / "tracked.txt").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "add", "tracked.txt"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "commit", "--quiet", "-m", "base"], cwd=checkout, check=True
            )
            (checkout / "created.txt").write_text("new content\n", encoding="utf-8")
            patch = HARNESS.collect_patch(checkout)
            self.assertIn("diff --git a/created.txt b/created.txt", patch)
            self.assertIn("+new content", patch)

    def test_collect_patch_excludes_generated_binary_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            subprocess.run(["git", "init", "--quiet"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "config", "user.name", "Test"], cwd=checkout, check=True
            )
            subprocess.run(
                ["git", "config", "user.email", "test@example.invalid"],
                cwd=checkout,
                check=True,
            )
            (checkout / "tracked.txt").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "add", "tracked.txt"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "commit", "--quiet", "-m", "base"], cwd=checkout, check=True
            )
            cache = checkout / "__pycache__"
            cache.mkdir()
            (cache / "bad.pyc").write_bytes(b"\xb2\x00\xff")
            (checkout / "solution.py").write_text("fixed = True\n", encoding="utf-8")
            patch = HARNESS.collect_patch(checkout)
            self.assertIn("solution.py", patch)
            self.assertNotIn("bad.pyc", patch)

    def test_executor_timeout_terminates_descendant_processes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            marker = checkout / "descendant-survived"
            child = (
                "import pathlib,time; time.sleep(0.8); "
                f"pathlib.Path({str(marker)!r}).write_text('survived')"
            )
            parent = (
                "import subprocess,sys,time; "
                f"subprocess.Popen([sys.executable, '-c', {child!r}]); "
                "time.sleep(60)"
            )
            monitor = HARNESS.FirstEditMonitor(checkout)
            result = HARNESS.run_executor(
                [sys.executable, "-c", parent],
                cwd=checkout,
                env={**dict(os.environ)},
                stdout_path=checkout / "stdout.log",
                stderr_path=checkout / "stderr.log",
                timed_trace_path=checkout / "trace.jsonl",
                timeout=0.1,
                monitor=monitor,
            )
            monitor.stop()
            self.assertEqual(result.exit_code, 124)
            time.sleep(1)
            self.assertFalse(marker.exists())

    def test_enrichment_state_includes_embedding_capability_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            logs = Path(temporary)
            scan_stdout = logs / "scan.stdout.log"
            scan_stderr = logs / "scan.stderr.log"
            scan_stdout.write_text("scan complete\n", encoding="utf-8")
            scan_stderr.write_text("", encoding="utf-8")
            (logs / "embeddings.stdout.log").write_text("", encoding="utf-8")
            (logs / "embeddings.stderr.log").write_text(
                "Error: embeddings support is not compiled in\n", encoding="utf-8"
            )
            command = HARNESS.CommandResult(
                command=["rna"],
                exit_code=0,
                started_at="start",
                finished_at="finish",
                duration_seconds=1.0,
            )
            embedding = HARNESS.dataclasses.replace(command, exit_code=1)
            state = HARNESS.parse_enrichment_state(
                command,
                scan_stdout,
                scan_stderr,
                embedding,
                command,
                condition="full",
            )
            self.assertEqual(state["observed"], "degraded")
            self.assertIn(
                "Error: embeddings support is not compiled in",
                state["degraded_evidence"],
            )
            self.assertIn("embedding_stderr", state["raw_logs"])

    def test_call_references_is_ready_when_embeddings_are_intentionally_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            logs = Path(temporary)
            scan_stdout = logs / "scan.stdout.log"
            scan_stderr = logs / "scan.stderr.log"
            scan_stdout.write_text(
                "call_references: completed\nDegraded queries: semantic search\n",
                encoding="utf-8",
            )
            scan_stderr.write_text("", encoding="utf-8")
            command = HARNESS.CommandResult(
                command=["rna"],
                exit_code=0,
                started_at="start",
                finished_at="finish",
                duration_seconds=1.0,
            )
            state = HARNESS.parse_enrichment_state(
                command,
                scan_stdout,
                scan_stderr,
                None,
                command,
                condition="call-references",
            )
            self.assertEqual(state["observed"], "ready")

    def test_dry_run_builds_auditable_bundle_without_executor_or_evaluator(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "bundle"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "fixture__repo-1",
                    "--executor-command",
                    "exit 99",
                    "--output-dir",
                    str(output),
                    "--instance-json",
                    str(FIXTURES / "swebench_instance.json"),
                    "--fixture-source",
                    str(FIXTURES / "repo"),
                    "--rna-binary",
                    str(Path(shutil.which("repo-native-alignment") or "")),
                    "--dry-run",
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            manifest = json.loads(
                (output / "manifest.json").read_text(encoding="utf-8")
            )
            self.assertEqual(manifest["status"], "dry_run_complete")
            self.assertEqual(manifest["dry_run"]["tokens_spent"], 0)
            self.assertEqual(
                manifest["checkout_isolation"]["history_commit_count"], 1
            )
            self.assertEqual(manifest["checkout_isolation"]["remotes"], [])
            evaluator = json.loads(
                (output / "evaluation" / "command.json").read_text(encoding="utf-8")
            )
            self.assertIn("swebench.harness.run_evaluation", evaluator["argv"])
            self.assertFalse((output / "executor.stdout.log").exists())
            self.assertFalse((output / "evaluation" / "stdout.log").exists())

    def test_baseline_dry_run_records_rna_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "baseline"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "fixture__repo-1",
                    "--arm",
                    "baseline",
                    "--executor-command",
                    "exit 99",
                    "--output-dir",
                    str(output),
                    "--instance-json",
                    str(FIXTURES / "swebench_instance.json"),
                    "--fixture-source",
                    str(FIXTURES / "repo"),
                    "--dry-run",
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            manifest = json.loads(
                (output / "manifest.json").read_text(encoding="utf-8")
            )
            self.assertEqual(manifest["arm"], "baseline")
            self.assertEqual(manifest["rna"]["availability"], "unavailable")
            mcp_config = json.loads(
                (output / "mcp-config.json").read_text(encoding="utf-8")
            )
            self.assertEqual(mcp_config, {"mcpServers": {}})


if __name__ == "__main__":
    unittest.main()
