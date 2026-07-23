from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import frontier_replay
import run_selector


class FrontierReplayTests(unittest.TestCase):
    initial = b"`pkg.py:target:function`\n"

    def receipt(
        self,
        root: Path,
        *,
        sequence: int,
        node: str,
        classification: str,
        projection: bytes,
        frontier_before: dict,
    ) -> dict:
        visible = f"RNA_STATUS={classification}\n".encode() + projection
        visible_path = root / f"{sequence:04d}.projection"
        visible_path.write_bytes(visible)
        emitted = frontier_replay.emitted_authorization(
            projection,
            classification,
        )
        next_source = frontier_replay.source(
            sequence,
            "rna_traversal_projection",
            classification,
            projection,
            visible,
        )
        value = {
            "schema_version": frontier_replay.RECEIPT_SCHEMA,
            "sequence": sequence,
            "node": node,
            "mode": "neighbors",
            "classification": classification,
            "projection_bytes": len(projection),
            "projection_sha256": frontier_replay.sha(projection),
            "projection_authorization_sha256": (
                frontier_before["sources"][0][
                    "projection_authorization_sha256"
                ]
            ),
            "authorization_frontier_before": frontier_before,
            "requested_node_authorized_by": frontier_replay.authorizers(
                frontier_before,
                node,
            ),
            "emitted_projection_authorization": emitted,
            "emitted_projection_authorization_sha256": (
                frontier_replay.authorization_sha(emitted)
            ),
            "authorization_frontier_after": (
                frontier_replay.build_frontier(
                    [*frontier_before["sources"], next_source]
                )
            ),
            "model_visible_projection": run_selector.file_ref(visible_path),
        }
        value["receipt_sha256"] = frontier_replay.sha(
            frontier_replay.canonical(value)
        )
        return value

    def replay(self, root: Path, receipts: list[dict]) -> dict:
        return run_selector.replay_treatment_frontier(
            self.initial,
            receipts,
            root,
        )

    def test_replay_accepts_visible_expansion_and_rejects_guessed_node(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            initial_source = frontier_replay.source(
                0,
                "injected_query_projection",
                "INJECTED_QUERY",
                self.initial,
                self.initial,
            )
            initial_frontier = frontier_replay.build_frontier(
                [initial_source]
            )
            first = self.receipt(
                root,
                sequence=1,
                node="pkg.py:target:function",
                classification="OK_NONEMPTY",
                projection=b"`pkg.py:helper:function`\n",
                frontier_before=initial_frontier,
            )
            replayed = self.replay(root, [first])
            self.assertIn(
                "pkg.py:helper:function",
                replayed["authorized_stable_code_ids"],
            )

            guessed = self.receipt(
                root,
                sequence=2,
                node="pkg.py:guessed:function",
                classification="OK_EMPTY",
                projection=(
                    b"No neighbors found for "
                    b"`pkg.py:guessed:function` within 1 hops.\n"
                ),
                frontier_before=replayed,
            )
            with self.assertRaisesRegex(
                frontier_replay.FrontierReplayError,
                "guessed_node",
            ):
                self.replay(root, [first, guessed])

    def test_projection_or_receipt_tamper_fails_replay(self):
        for tamper in ("projection", "receipt"):
            with self.subTest(tamper=tamper), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                initial_source = frontier_replay.source(
                    0,
                    "injected_query_projection",
                    "INJECTED_QUERY",
                    self.initial,
                    self.initial,
                )
                first = self.receipt(
                    root,
                    sequence=1,
                    node="pkg.py:target:function",
                    classification="OK_NONEMPTY",
                    projection=b"`pkg.py:helper:function`\n",
                    frontier_before=frontier_replay.build_frontier(
                        [initial_source]
                    ),
                )
                if tamper == "projection":
                    (root / "0001.projection").write_bytes(b"tampered")
                else:
                    first["node"] = "pkg.py:guessed:function"
                with self.assertRaises(
                    (
                        run_selector.FailClosed,
                        frontier_replay.FrontierReplayError,
                    )
                ):
                    self.replay(root, [first])

    def test_empty_response_does_not_expand_frontier(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            initial_source = frontier_replay.source(
                0,
                "injected_query_projection",
                "INJECTED_QUERY",
                self.initial,
                self.initial,
            )
            initial_frontier = frontier_replay.build_frontier(
                [initial_source]
            )
            receipt = self.receipt(
                root,
                sequence=1,
                node="pkg.py:target:function",
                classification="OK_EMPTY",
                projection=(
                    b"No neighbors found for "
                    b"`pkg.py:target:function` within 1 hops.\n"
                ),
                frontier_before=initial_frontier,
            )
            replayed = self.replay(root, [receipt])
            self.assertEqual(
                replayed["authorized_stable_code_ids"],
                ["pkg.py:target:function"],
            )


if __name__ == "__main__":
    unittest.main()
