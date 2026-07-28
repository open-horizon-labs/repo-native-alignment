from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import verify_working_set_diagnostic  # noqa: E402


class WorkingSetDiagnosticVerificationTests(unittest.TestCase):
    def test_checked_in_package_passes(self) -> None:
        with mock.patch.dict("os.environ", {}, clear=True):
            self.assertEqual(verify_working_set_diagnostic.main(), 0)

    def test_scale_decision_mutation_fails_closed(self) -> None:
        value = json.loads(verify_working_set_diagnostic.RESULTS.read_text())
        value["design_diagnosis"]["scale_decision"] = "SCALE"
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "mutated-results.json"
            path.write_text(json.dumps(value))
            with mock.patch.object(verify_working_set_diagnostic, "RESULTS", path):
                with self.assertRaises(verify_working_set_diagnostic.VerificationFailure):
                    verify_working_set_diagnostic.main()


if __name__ == "__main__":
    unittest.main()
