from __future__ import annotations

import copy
import sys
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import schedule_contract as contract  # noqa: E402


def synthetic_selection() -> dict:
    return {
        "cases": [
            {
                "rank": rank,
                "instance_id": f"owner__repo-{rank}",
                "base_commit": f"{rank:040x}",
                "base_tree": f"{rank + 20:040x}",
                "arm_order": ["A", "T"] if rank % 2 else ["T", "A"],
            }
            for rank in range(1, 21)
        ]
    }


def synthetic_envelope() -> dict:
    cases = []
    episodes = []
    for chosen in synthetic_selection()["cases"]:
        rank = chosen["rank"]
        sessions = {
            "A": f"00000000-0000-4000-8000-{2 * rank - 1:012d}",
            "T": f"00000000-0000-4000-8000-{2 * rank:012d}",
        }
        cases.append(
            {
                **chosen,
                "sessions": sessions,
            }
        )
        episodes.extend(
            {
                "rank": rank,
                "case_id": chosen["instance_id"],
                "arm": arm,
                "session_id": sessions[arm],
            }
            for arm in chosen["arm_order"]
        )
    return {
        "schema_version": contract.ENVELOPE_SCHEMA,
        "assembler": {
            "path": "/approved/assemble_run.py",
            "bytes": 1,
            "sha256": contract.APPROVED_ASSEMBLER_SHA256,
        },
        "verified": True,
        "source_commit": contract.BASE_SOURCE_COMMIT,
        "source_tree": contract.BASE_SOURCE_TREE,
        "registration": {"path": "/frozen/registration.json", "bytes": 1, "sha256": "3" * 64},
        "selection": {"path": "/frozen/selection.json", "bytes": 1, "sha256": "4" * 64},
        "case_count": 20,
        "episode_count": 40,
        "per_episode_budget_usd": 6.0,
        "maximum_budget_usd": 240.0,
        "cases": cases,
        "execution_episode_keys": episodes,
        "same_case_serialized": True,
        "max_parallel_cases": 2,
        "selection_policy": "full_frozen_v4_identity_before_first_batch",
        "model_outputs_inspected": False,
        "evaluator_or_outcome_accessed": False,
        "no_spend_assertion": contract.NO_SPEND,
    }


class ScheduleContractTests(unittest.TestCase):
    def test_wave_scope_is_explicit_ordered_and_capped(self) -> None:
        self.assertEqual(contract.explicit_wave_ranks([1]), (1,))
        self.assertEqual(contract.explicit_wave_ranks([1, 12]), (1, 12))
        self.assertEqual(
            contract.explicit_wave_ranks([1, 12, 20]), (1, 12, 20)
        )
        for invalid in ([1, 1], [1, 2, 3, 4], [12, 1], [0], [21]):
            with self.subTest(invalid=invalid), self.assertRaises(contract.ContractError):
                contract.explicit_wave_ranks(invalid)

    def test_full_envelope_freezes_all_identities_sessions_and_budget(self) -> None:
        sessions = contract.validate_episode_envelope(
            synthetic_envelope(),
            synthetic_selection(),
            registration_ref={"path": "/frozen/registration.json", "bytes": 1, "sha256": "3" * 64},
            selection_ref={"path": "/frozen/selection.json", "bytes": 1, "sha256": "4" * 64},
        )
        self.assertEqual(len(sessions), 20)
        self.assertEqual(len({value for arms in sessions.values() for value in arms.values()}), 40)

    def test_envelope_tampering_fails_closed(self) -> None:
        for mutate in ("budget", "session", "order"):
            envelope = copy.deepcopy(synthetic_envelope())
            if mutate == "budget":
                envelope["maximum_budget_usd"] = 239.0
            elif mutate == "session":
                envelope["cases"][1]["sessions"]["A"] = envelope["cases"][0]["sessions"]["A"]
            else:
                envelope["cases"][0]["arm_order"] = ["T", "A"]
            with self.subTest(mutate=mutate), self.assertRaises(contract.ContractError):
                contract.validate_episode_envelope(
                    envelope,
                    synthetic_selection(),
                    registration_ref={"path": "/frozen/registration.json", "bytes": 1, "sha256": "3" * 64},
                    selection_ref={"path": "/frozen/selection.json", "bytes": 1, "sha256": "4" * 64},
                )

    def test_cumulative_chain_rejects_duplicate_spend(self) -> None:
        first = contract.next_cumulative_state(
            prior_ranks=set(),
            prior_sessions=set(),
            requested_ranks=[1, 2],
            requested_sessions={
                value
                for rank in (1, 2)
                for value in synthetic_envelope()["cases"][rank - 1]["sessions"].values()
            },
        )
        self.assertEqual(first["cumulative_ranks"], [1, 2])
        with self.assertRaises(contract.ContractError):
            contract.next_cumulative_state(
                prior_ranks={1, 2},
                prior_sessions=set(first["cumulative_sessions"]),
                requested_ranks=[2],
                requested_sessions=set(
                    synthetic_envelope()["cases"][1]["sessions"].values()
                ),
            )


if __name__ == "__main__":
    unittest.main()
