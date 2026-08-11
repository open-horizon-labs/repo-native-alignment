from __future__ import annotations

import json
from pathlib import Path
import sys
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import verify_native_default_control_repair as verifier  # noqa: E402


class NativeDefaultControlRepairVerificationTests(unittest.TestCase):
    def test_checked_in_package_passes(self) -> None:
        with mock.patch.dict("os.environ", {}, clear=True):
            self.assertEqual(verifier.main(), 0)

    def test_rank_inventory_mutation_fails_closed(self) -> None:
        value = json.loads(verifier.RESULTS.read_text())
        value["cases"][0]["rank"] = 99
        with self.assertRaisesRegex(verifier.VerificationFailure, "case inventory"):
            verifier.verify_offline(value, json.loads(verifier.MANIFEST.read_text()))

    def test_treatment_mutation_fails_closed(self) -> None:
        value = json.loads(verifier.RESULTS.read_text())
        value["cases"][0]["T_PD"]["total_input_tokens"] += 1
        with self.assertRaisesRegex(verifier.VerificationFailure, "immutable T_PD reuse"):
            verifier.verify_offline(value, json.loads(verifier.MANIFEST.read_text()))

    def test_evidence_path_traversal_fails_closed(self) -> None:
        manifest = json.loads(verifier.MANIFEST.read_text())
        manifest["artifacts"][0]["path"] = "../outside"
        with self.assertRaisesRegex(verifier.VerificationFailure, "unsafe evidence path"):
            verifier.verify_offline(json.loads(verifier.RESULTS.read_text()), manifest)


if __name__ == "__main__":
    unittest.main()
