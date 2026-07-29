from __future__ import annotations

import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import verify_results  # noqa: E402


class CanonicalResultsTests(unittest.TestCase):
    def copied_package(self, destination: Path) -> tuple[Path, Path, Path]:
        for name in ("results.json", "REPORT.md", "evidence-manifest.json", "METHOD.md", "README.md"):
            shutil.copy2(ROOT / name, destination / name)
        return destination / "results.json", destination / "REPORT.md", destination / "evidence-manifest.json"

    def assert_package_fails(self, results: Path, report: Path, manifest: Path) -> None:
        with self.assertRaises(verify_results.VerificationFailure):
            verify_results.verify(results_path=results, report_path=report, manifest_path=manifest)

    def test_checked_in_results_recompute(self) -> None:
        result = verify_results.verify()
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["rows"], 20)
        self.assertEqual(result["cells"], 80)
        self.assertEqual(result["source_assertions"], 2333)
        self.assertEqual(result["registered_mechanical_decision"], "NOT_COMPUTED")
        self.assertEqual(result["registered_gate_disposition"], "NOT_FORMALLY_APPLICABLE_TO_REPAIRED_STUDY")

    def test_report_number_mutations_fail_closed(self) -> None:
        for old, new in (("| Sonnet | 17/20 |", "| Sonnet | 0/20 |"), ("in 179,511", "in 999,999")):
            with self.subTest(old=old), tempfile.TemporaryDirectory() as raw:
                results, report, manifest = self.copied_package(Path(raw))
                text = report.read_text()
                self.assertIn(old, text)
                report.write_text(text.replace(old, new, 1))
                self.assert_package_fails(results, report, manifest)

    def test_method_claim_mutation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            destination = Path(raw)
            results, report, manifest = self.copied_package(destination)
            method = destination / "METHOD.md"
            method.write_text(method.read_text() + "\nAll population effects are conclusive.\n")
            self.assert_package_fails(results, report, manifest)

    def test_manifest_digest_mutation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            results, report, manifest = self.copied_package(Path(raw))
            value = json.loads(manifest.read_text())
            value["external_artifacts"]["canonical_report_builder"]["sha256"] = "0" * 64
            manifest.write_text(json.dumps(value))
            self.assert_package_fails(results, report, manifest)

    def test_critical_ledger_mutations_fail_closed(self) -> None:
        mutations = {
            "checkout": lambda d: d["rows"][0]["conditions"]["A_sonnet"].__setitem__("base_tree", "0" * 40),
            "patch": lambda d: d["rows"][0]["conditions"]["A_sonnet"]["patch"].__setitem__("sha256", "0" * 64),
            "evaluator": lambda d: d["rows"][0]["conditions"]["A_sonnet"]["official"]["source"].__setitem__("sha256", "0" * 64),
            "prompt": lambda d: d["rows"][0]["conditions"]["A_sonnet"]["prompt"].__setitem__("sha256", "0" * 64),
            "injection": lambda d: d["rows"][0]["conditions"]["T_luna"]["treatment_context"]["injection"].__setitem__("sha256", "0" * 64),
            "status": lambda d: d["rows"][0]["conditions"]["A_sonnet"].__setitem__("status", "INVALID"),
            "transcript": lambda d: d["rows"][0]["conditions"]["A_sonnet"]["transcript_audit"].__setitem__("status", "FAIL"),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as raw:
                results, report, manifest = self.copied_package(Path(raw))
                value = json.loads(results.read_text())
                mutate(value)
                results.write_text(json.dumps(value))
                self.assert_package_fails(results, report, manifest)

    def test_path_leak_mutation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            destination = Path(raw)
            results, report, manifest = self.copied_package(destination)
            readme = destination / "README.md"
            readme.write_text(readme.read_text() + "\n/home/example/private-evidence\n")
            self.assert_package_fails(results, report, manifest)

    def test_external_mode_requires_all_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            with self.assertRaises((FileNotFoundError, verify_results.VerificationFailure)):
                verify_results.verify(evidence_root=Path(raw))


if __name__ == "__main__":
    unittest.main()
