from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import verify_bounded_progressive_disclosure as verifier  # noqa: E402


class BoundedProgressiveDisclosureVerificationTests(unittest.TestCase):
    def test_checked_in_package_passes(self) -> None:
        with mock.patch.dict("os.environ", {}, clear=True):
            self.assertEqual(verifier.main(), 0)

    def test_rank_inventory_mutation_fails_closed(self) -> None:
        value = json.loads(verifier.RESULTS.read_text())
        value["cases"][0]["rank"] = 99
        with self.assertRaisesRegex(verifier.VerificationFailure, "case inventory"):
            verifier.verify_offline(value, json.loads(verifier.MANIFEST.read_text()))

    def test_input_accounting_mutation_fails_closed(self) -> None:
        value = json.loads(verifier.RESULTS.read_text())
        value["cases"][0]["T_PD"]["total_input_tokens"] += 1
        with self.assertRaisesRegex(verifier.VerificationFailure, "input accounting"):
            verifier.verify_offline(value, json.loads(verifier.MANIFEST.read_text()))

    def test_evidence_path_traversal_fails_closed(self) -> None:
        manifest = json.loads(verifier.MANIFEST.read_text())
        manifest["artifacts"][0]["path"] = "../outside"
        with self.assertRaisesRegex(verifier.VerificationFailure, "unsafe evidence path"):
            verifier.verify_offline(json.loads(verifier.RESULTS.read_text()), manifest)

    def test_package_digest_mutations_fail_closed(self) -> None:
        for name in ("RESULTS", "MANIFEST", "CANONICAL_RESULTS", "REPORT", "METHOD"):
            with self.subTest(artifact=name):
                original = getattr(verifier, name)
                with tempfile.TemporaryDirectory() as raw:
                    mutated = Path(raw) / original.name
                    mutated.write_bytes(original.read_bytes() + b"\n")
                    with mock.patch.object(verifier, name, mutated):
                        with self.assertRaisesRegex(verifier.VerificationFailure, "package digest drift"):
                            verifier.verify_package_digests()


if __name__ == "__main__":
    unittest.main()
