from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import verify_compact_progressive_disclosure as verifier  # noqa: E402


class CompactProgressiveDisclosureVerificationTests(unittest.TestCase):
    def test_checked_in_package_passes(self) -> None:
        with mock.patch.dict("os.environ", {}, clear=True):
            self.assertEqual(verifier.main(), 0)

    def test_scale_mutation_fails_closed(self) -> None:
        value = json.loads(verifier.RESULTS.read_text())
        value["scope"]["scale_authorized"] = True
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "mutated-results.json"
            path.write_text(json.dumps(value))
            with mock.patch.object(verifier, "RESULTS", path):
                with self.assertRaises(verifier.VerificationFailure):
                    verifier.verify_offline(value, json.loads(verifier.MANIFEST.read_text()))

    def test_prompt_identity_mutation_fails_closed(self) -> None:
        value = json.loads(verifier.RESULTS.read_text())
        value["cases"][0]["E"]["prompt"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(verifier.VerificationFailure, "prompt identity"):
            verifier.verify_offline(value, json.loads(verifier.MANIFEST.read_text()))

    def test_package_digest_mutations_fail_closed(self) -> None:
        for name in ("RESULTS", "MANIFEST", "REPORT", "METHOD"):
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
