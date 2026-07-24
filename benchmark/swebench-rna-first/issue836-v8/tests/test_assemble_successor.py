from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "issue836_v8_assemble_successor",
    ROOT / "assemble_successor.py",
)
assert SPEC is not None and SPEC.loader is not None
assemble_successor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(assemble_successor)


class SuccessorExplicitRanksTests(unittest.TestCase):
    def test_accepts_three_increasing_frozen_ranks(self) -> None:
        self.assertEqual(
            assemble_successor.successor_explicit_ranks([1, 12, 20]),
            (1, 12, 20),
        )

    def test_rejects_four_ranks(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "at most three"):
            assemble_successor.successor_explicit_ranks([1, 2, 3, 4])

    def test_preserves_existing_scope_guards(self) -> None:
        invalid_values = (
            [],
            [1, 1],
            [2, 1],
            [0],
            [21],
            [True],
        )
        for values in invalid_values:
            with self.subTest(values=values), self.assertRaises(RuntimeError):
                assemble_successor.successor_explicit_ranks(values)


if __name__ == "__main__":
    unittest.main()
