from __future__ import annotations

import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import verify_weaker_results  # noqa: E402


class WeakerModelResultsTests(unittest.TestCase):
    def copied_package(self, destination: Path) -> tuple[Path, Path, Path, Path, Path]:
        for name in (
            "weaker-model-results.json",
            "weaker-model-evidence-manifest.json",
            "REPORT.md",
            "METHOD.md",
            "README.md",
        ):
            shutil.copy2(ROOT / name, destination / name)
        return (
            destination / "weaker-model-results.json",
            destination / "REPORT.md",
            destination / "METHOD.md",
            destination / "README.md",
            destination / "weaker-model-evidence-manifest.json",
        )

    def assert_package_fails(
        self, results: Path, report: Path, method: Path, readme: Path, manifest: Path
    ) -> None:
        with mock.patch.multiple(
            verify_weaker_results,
            RESULTS=results,
            REPORT=report,
            METHOD=method,
            README=readme,
            MANIFEST=manifest,
        ):
            with self.assertRaises(verify_weaker_results.VerificationFailure):
                verify_weaker_results.main()

    def assert_semantic_results_failure(
        self, results: Path, report: Path, method: Path, readme: Path, manifest: Path
    ) -> None:
        with mock.patch.multiple(
            verify_weaker_results,
            RESULTS=results,
            REPORT=report,
            METHOD=method,
            README=readme,
            MANIFEST=manifest,
            EXPECTED_RESULTS_SHA=verify_weaker_results.digest(results),
        ):
            with self.assertRaises(verify_weaker_results.VerificationFailure):
                verify_weaker_results.main()

    def test_checked_in_results_recompute(self) -> None:
        self.assertEqual(verify_weaker_results.main(), 0)

    def test_critical_cell_mutation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            results, report, method, readme, manifest = self.copied_package(Path(raw))
            value = json.loads(results.read_text())
            value["rows"][0]["conditions"]["T2_spark"]["patch"]["sha256"] = "0" * 64
            results.write_text(json.dumps(value))
            self.assert_package_fails(results, report, method, readme, manifest)

    def test_report_metric_mutation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            results, report, method, readme, manifest = self.copied_package(Path(raw))
            text = report.read_text()
            self.assertIn("success ", text)
            report.write_text(text.replace("success ", "outcome ", 1))
            self.assert_package_fails(results, report, method, readme, manifest)

    def test_readme_mutation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            results, report, method, readme, manifest = self.copied_package(Path(raw))
            readme.write_text(readme.read_text() + "\nunreviewed claim\n")
            self.assert_package_fails(results, report, method, readme, manifest)

    def test_frozen_prompt_parity_mutation_fails_semantically(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            results, report, method, readme, manifest = self.copied_package(Path(raw))
            value = json.loads(results.read_text())
            for backend in ("haiku", "spark"):
                value["rows"][0]["conditions"][f"A_{backend}"]["prompt"]["sha256"] = "0" * 64
            results.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
            self.assert_semantic_results_failure(results, report, method, readme, manifest)

    def test_independent_tool_recount_mutation_fails_semantically(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            results, report, method, readme, manifest = self.copied_package(Path(raw))
            value = json.loads(results.read_text())
            recount = value["rows"][0]["conditions"]["A_haiku"]["transcript_audit"]["tool_calls"]
            recount["total"] += 1
            recount["by_type"]["Bash"] += 1
            results.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
            self.assert_semantic_results_failure(results, report, method, readme, manifest)


if __name__ == "__main__":
    unittest.main()
