#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "swebench_rna_one.py"
FIXTURES = ROOT / "scripts" / "tests" / "fixtures"
SPEC = importlib.util.spec_from_file_location("swebench_rna_one", SCRIPT)
assert SPEC and SPEC.loader
HARNESS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HARNESS
SPEC.loader.exec_module(HARNESS)


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
            self.assertEqual(usage["before"]["cache_creation_input_tokens"], 30)
            self.assertEqual(usage["before"]["cache_read_input_tokens"], 400)
            self.assertEqual(usage["totals"]["cache_creation_input_tokens"], 80)
            self.assertEqual(usage["totals"]["cache_read_input_tokens"], 900)
            self.assertEqual(usage["totals"]["cost_usd"], 0.25)

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
                command, scan_stdout, scan_stderr, embedding, command
            )
            self.assertEqual(state["observed"], "degraded")
            self.assertIn(
                "Error: embeddings support is not compiled in",
                state["degraded_evidence"],
            )
            self.assertIn("embedding_stderr", state["raw_logs"])

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


if __name__ == "__main__":
    unittest.main()
