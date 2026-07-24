from __future__ import annotations

import hashlib
import json
from pathlib import Path
import sys
import unittest


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import registration_contract
import select_cases


class Issue836CaseSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.registration = json.loads(
            (HERE / "registration.template.json").read_bytes()
        )

    def current_selection(self) -> dict:
        instance_ids = [f"case-{index:02d}" for index in range(30)]
        excluded = {"case-03", "case-17"}
        ranked = select_cases.deterministic_ranked_prefix(
            instance_ids,
            excluded,
            select_cases.EXPECTED_SEED,
            20,
        )
        return {
            "schema_version": select_cases.CURRENT_SCHEMA,
            "cases": [
                {
                    "rank": rank,
                    "instance_id": instance_id,
                    "ranking_sha256": ranking_sha256,
                    "arm_order": (
                        ["A", "T"] if rank % 2 == 1 else ["T", "A"]
                    ),
                }
                for rank, (ranking_sha256, instance_id) in enumerate(
                    ranked,
                    start=1,
                )
            ],
        }

    def test_deterministic_prefix_is_exactly_twenty_eligible_ranks(self) -> None:
        instance_ids = [f"case-{index:02d}" for index in range(30)]
        excluded = {"case-03", "case-17"}
        expected = sorted(
            (
                hashlib.sha256(
                    select_cases.EXPECTED_SEED.encode("utf-8")
                    + b"\0"
                    + instance_id.encode("utf-8")
                ).hexdigest(),
                instance_id,
            )
            for instance_id in instance_ids
            if instance_id not in excluded
        )[:20]
        selected = select_cases.deterministic_ranked_prefix(
            instance_ids,
            excluded,
            select_cases.EXPECTED_SEED,
            20,
        )
        self.assertEqual(selected, expected)
        self.assertEqual(len(selected), 20)

    def test_current_selection_has_exact_ranks_parity_and_forty_episodes(
        self,
    ) -> None:
        selection = self.current_selection()
        identities = select_cases.expected_episode_identities(
            self.registration,
            selection,
        )
        self.assertEqual(
            [case["rank"] for case in selection["cases"]],
            list(range(1, 21)),
        )
        self.assertEqual(len(identities), 40)
        self.assertEqual(
            [identity.rank for identity in identities],
            [rank for rank in range(1, 21) for _ in range(2)],
        )
        self.assertEqual(
            sum(case["arm_order"][0] == "A" for case in selection["cases"]),
            10,
        )
        self.assertEqual(
            sum(case["arm_order"][0] == "T" for case in selection["cases"]),
            10,
        )
        self.assertTrue(
            all(
                case["arm_order"]
                == (["A", "T"] if case["rank"] % 2 == 1 else ["T", "A"])
                for case in selection["cases"]
            )
        )

    def test_current_registration_records_truthful_prefix_lineage(self) -> None:
        selector = self.registration["selector"]
        self.assertEqual(
            selector["prefix_lineage"],
            {
                "ranks_1_through_2": "pre_model_carry_forward_prefix",
                "ranks_3_through_20": "deterministic_extension",
                "outcomes_inspected_for_extension": False,
            },
        )
        self.assertFalse(
            selector["gold_or_outcomes_inspected_before_selection"]
        )
        self.assertFalse(
            selector["problem_statements_inspected_by_human_before_selection"]
        )

    def test_historical_issue830_pair_remains_a_valid_verifier_path(
        self,
    ) -> None:
        registration = json.loads(
            (HERE.parent / "issue830" / "registration.json").read_bytes()
        )
        selection = json.loads(
            (HERE.parent / "issue830" / "selection.json").read_bytes()
        )
        registration_contract.validate_registration(registration)
        identities = select_cases.expected_episode_identities(
            registration,
            selection,
        )
        self.assertEqual(
            identities,
            (
                select_cases.ExpectedEpisodeIdentity(
                    1,
                    "sympy__sympy-23534",
                    "A",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    1,
                    "sympy__sympy-23534",
                    "T",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    2,
                    "django__django-11179",
                    "T",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    2,
                    "django__django-11179",
                    "A",
                ),
            ),
        )

    def test_selection_schema_rank_and_parity_drift_fail_closed(self) -> None:
        for label, mutate in (
            (
                "schema",
                lambda selection: selection.update(
                    {"schema_version": select_cases.LEGACY_SCHEMA}
                ),
            ),
            (
                "rank",
                lambda selection: selection["cases"][9].update({"rank": 9}),
            ),
            (
                "parity",
                lambda selection: selection["cases"][9].update(
                    {"arm_order": ["A", "T"]}
                ),
            ),
        ):
            with self.subTest(label=label):
                selection = self.current_selection()
                mutate(selection)
                with self.assertRaises(select_cases.SelectionError):
                    select_cases.expected_episode_identities(
                        self.registration,
                        selection,
                    )


if __name__ == "__main__":
    unittest.main()
