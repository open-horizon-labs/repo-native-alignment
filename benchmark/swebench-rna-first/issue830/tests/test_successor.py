from __future__ import annotations

import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import verify_successor  # type: ignore  # noqa: E402


class SuccessorRegistrationTests(unittest.TestCase):
    def test_published_registration_is_verifier_clean(self) -> None:
        receipt = verify_successor.verify_registration(
            HERE / "registration.json",
            HERE / "successor-lineage.json",
            HERE / "qualification-closure.manifest.json",
            None,
        )
        self.assertTrue(receipt["verified"])
        self.assertEqual(receipt["model_calls"], 0)
        self.assertFalse(receipt["credentials_accessed"])

    def test_artifact_or_lineage_tamper_fails_closed(self) -> None:
        registration = json.loads((HERE / "registration.json").read_bytes())
        lineage = json.loads((HERE / "successor-lineage.json").read_bytes())
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            registration_path = root / "registration.json"
            lineage_path = root / "lineage.json"
            changed = copy.deepcopy(registration)
            changed["rna_artifact"]["producer_commit"] = "0" * 40
            registration_path.write_text(json.dumps(changed))
            lineage_path.write_text(json.dumps(lineage))
            with self.assertRaises(verify_successor.SuccessorVerificationError):
                verify_successor.verify_registration(
                    registration_path,
                    lineage_path,
                    HERE / "qualification-closure.manifest.json",
                    None,
                )

            changed = copy.deepcopy(lineage)
            changed["retained_pre_model_attempts"][0]["episode_receipts"] = 1
            registration_path.write_bytes((HERE / "registration.json").read_bytes())
            lineage_path.write_text(json.dumps(changed))
            with self.assertRaises(verify_successor.SuccessorVerificationError):
                verify_successor.verify_registration(
                    registration_path,
                    lineage_path,
                    HERE / "qualification-closure.manifest.json",
                    None,
                )

            changed = copy.deepcopy(lineage)
            changed["retained_pre_model_attempts"][1][
                "model_processes_started"
            ] = 1
            registration_path.write_bytes((HERE / "registration.json").read_bytes())
            lineage_path.write_text(json.dumps(changed))
            with self.assertRaises(verify_successor.SuccessorVerificationError):
                verify_successor.verify_registration(
                    registration_path,
                    lineage_path,
                    HERE / "qualification-closure.manifest.json",
                    None,
                )

            changed = copy.deepcopy(lineage)
            changed["prior_activity"]["model_calls"] = 1
            registration_path.write_bytes((HERE / "registration.json").read_bytes())
            lineage_path.write_text(json.dumps(changed))
            with self.assertRaises(verify_successor.SuccessorVerificationError):
                verify_successor.verify_registration(
                    registration_path,
                    lineage_path,
                    HERE / "qualification-closure.manifest.json",
                    None,
                )

    def test_successor_selection_if_published(self) -> None:
        selection = HERE / "selection.json"
        if not selection.exists():
            self.skipTest("selection is intentionally published after registration")
        receipt = verify_successor.verify_selection(
            selection,
            HERE / "registration.json",
            HERE / "successor-lineage.json",
        )
        self.assertTrue(receipt["verified"])
        self.assertEqual(
            [row["arm_order"] for row in receipt["case_order"]],
            [["A", "T"], ["T", "A"]],
        )


if __name__ == "__main__":
    unittest.main()
