#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "swebench_rna_pair.py"
FIXTURES = ROOT / "scripts" / "tests" / "fixtures"
SPEC = importlib.util.spec_from_file_location("swebench_rna_pair", SCRIPT)
assert SPEC and SPEC.loader
PAIR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PAIR
SPEC.loader.exec_module(PAIR)


class SwebenchRnaPairTests(unittest.TestCase):
    def test_validate_design_requires_immutable_model_and_artifact(self) -> None:
        task_spec = {
            "dataset_revision": "abc",
            "instances": ["fixture__repo-1"],
        }
        executor = {
            "model": {
                "immutable_id": "model-20260101",
                "provider": "provider",
                "temperature": 0,
                "budget_usd": 1,
                "timeout_seconds": 10,
                "executor_version": "1.0",
            }
        }
        artifact = {
            "workflow_run_id": 1,
            "commit_sha": "a" * 40,
            "artifact_id": 2,
            "artifact_name": "rna",
            "artifact_digest": "sha256:zip",
            "binary_sha256": "binary",
        }
        self.assertEqual(
            PAIR.validate_design(task_spec, executor, artifact),
            ["fixture__repo-1"],
        )
        del executor["model"]["immutable_id"]
        with self.assertRaises(PAIR.PairError):
            PAIR.validate_design(task_spec, executor, artifact)

    def test_paired_dry_run_builds_identical_prompts_and_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            task_spec = root / "tasks.json"
            executor = root / "executor.json"
            artifact = root / "artifact.json"
            output = root / "bundle"
            task_spec.write_text(
                json.dumps(
                    {
                        "dataset_revision": "fixture-revision",
                        "instances": ["fixture__repo-1"],
                        "selection": "fixture",
                        "publication": {"score": "not applicable"},
                    }
                ),
                encoding="utf-8",
            )
            executor.write_text(
                json.dumps(
                    {
                        "command": ["exit", "99"],
                        "model": {
                            "immutable_id": "model-20260101",
                            "provider": "provider",
                            "temperature": 0,
                            "budget_usd": 1,
                            "timeout_seconds": 10,
                            "executor_version": "1.0",
                        },
                    }
                ),
                encoding="utf-8",
            )
            artifact.write_text(
                json.dumps(
                    {
                        "workflow_run_id": 1,
                        "commit_sha": "a" * 40,
                        "artifact_id": 2,
                        "artifact_name": "rna",
                        "artifact_digest": "sha256:zip",
                        "binary_sha256": "binary",
                        "binary_path": "/missing/allowed/in/dry-run",
                        "enrichment_condition": "call-references",
                    }
                ),
                encoding="utf-8",
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--task-spec",
                    str(task_spec),
                    "--executor-config",
                    str(executor),
                    "--rna-artifact",
                    str(artifact),
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
            report = json.loads(
                (output / "paired-report.json").read_text(encoding="utf-8")
            )
            self.assertEqual(report["label"], "paired pilot; not a full-suite benchmark score")
            pair = report["pairs"][0]
            self.assertTrue(pair["parity"]["task_prompt_sha256_match"])
            self.assertEqual(pair["baseline"]["outcome"], "not_evaluated")
            self.assertEqual(pair["rna"]["outcome"], "not_evaluated")
            self.assertEqual(report["arms"]["baseline"]["cost_usd"], None)


if __name__ == "__main__":
    unittest.main()
