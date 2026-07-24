from __future__ import annotations

import copy
import sys
from pathlib import Path
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import schedule_contract as contract  # noqa: E402
import verify_waves  # noqa: E402
import run_selector as base  # type: ignore  # noqa: E402


def sessions() -> dict[int, dict[str, str]]:
    return {
        rank: {
            "A": f"00000000-0000-4000-8000-{2 * rank - 1:012d}",
            "T": f"00000000-0000-4000-8000-{2 * rank:012d}",
        }
        for rank in range(1, 21)
    }


def identities() -> list[tuple[int, str, str]]:
    return [
        (rank, f"owner__repo-{rank}", arm)
        for rank in range(1, 21)
        for arm in (("A", "T") if rank % 2 else ("T", "A"))
    ]


def sealed_waves() -> list[dict]:
    fixed = sessions()
    expected = identities()
    prior_ranks: set[int] = set()
    prior_sessions: set[str] = set()
    prior_refs: list[dict] = []
    waves = []
    for wave_number, first_rank in enumerate(range(1, 21, 2), 1):
        requested = [first_rank, first_rank + 1]
        requested_sessions = {
            fixed[rank][arm]
            for rank in requested
            for arm in ("A", "T")
        }
        state = contract.next_cumulative_state(
            prior_ranks=prior_ranks,
            prior_sessions=prior_sessions,
            requested_ranks=requested,
            requested_sessions=requested_sessions,
        )
        self_ref = {
            "path": f"/wave-{wave_number:02d}/wave-receipt.json",
            "bytes": wave_number,
            "sha256": f"{wave_number:064x}",
        }
        wave = {
            "self_ref": self_ref,
            "prior_wave_receipts": list(prior_refs),
            "requested_ranks": requested,
            "requested_sessions": sorted(requested_sessions),
            "authorized_episode_keys": [
                {
                    "rank": rank,
                    "case_id": case_id,
                    "arm": arm,
                    "session_id": fixed[rank][arm],
                }
                for rank, case_id, arm in expected
                if rank in set(requested)
            ],
            "case_count": 2,
            "episode_count": 4,
            "per_episode_budget_usd": 6.0,
            "maximum_budget_usd": 24.0,
            **state,
            "episode_receipts": [
                {
                    "path": f"/wave-{wave_number:02d}/episode-{index}.json",
                    "bytes": index,
                    "sha256": f"{100 + 4 * wave_number + index:064x}",
                }
                for index in range(4)
            ],
            "worker_errors": [],
            "all_authorized_episodes_recorded": True,
            "official_evaluator_invoked": False,
        }
        waves.append(wave)
        prior_ranks.update(requested)
        prior_sessions.update(requested_sessions)
        prior_refs.append(self_ref)
    return waves


class SealedWaveVerifierTests(unittest.TestCase):
    def test_final_ledger_uses_registered_schema(self) -> None:
        self.assertEqual(
            verify_waves.AGGREGATE_SCHEMA,
            contract.FINAL_LEDGER_SCHEMA,
        )

    def test_episode_verifier_receives_bridge_and_rejects_other_errors(
        self,
    ) -> None:
        original = base.verify_qualification_closure

        def invoke_qualification(_episode_path: Path) -> dict:
            base.verify_qualification_closure({}, {})
            return {
                "evidence_complete": True,
                "official_evaluator_invoked": False,
                "errors": [],
            }

        with (
            mock.patch.object(
                verify_waves.run_wave,
                "verify_v4_qualification_compatibility",
            ) as bridge,
            mock.patch.object(
                verify_waves.base_verifier,
                "verify_episode",
                side_effect=invoke_qualification,
            ),
        ):
            result = (
                verify_waves.verify_episode_with_qualification_compatibility(
                    Path("/episode.json"),
                    compatibility={"compatibility": 1},
                    registration={"registration": 1},
                    schedule={"schedule": 1},
                    where="synthetic episode",
                )
            )
        self.assertTrue(result["evidence_complete"])
        bridge.assert_called_once()
        self.assertIs(base.verify_qualification_closure, original)

        with (
            mock.patch.object(
                verify_waves.run_wave,
                "verify_v4_qualification_compatibility",
            ),
            mock.patch.object(
                verify_waves.base_verifier,
                "verify_episode",
                return_value={
                    "evidence_complete": False,
                    "official_evaluator_invoked": False,
                    "errors": ["another verifier error"],
                },
            ),
            self.assertRaises(base.FailClosed),
        ):
            verify_waves.verify_episode_with_qualification_compatibility(
                Path("/episode.json"),
                compatibility={"compatibility": 1},
                registration={"registration": 1},
                schedule={"schedule": 1},
                where="synthetic episode",
            )
        self.assertIs(base.verify_qualification_closure, original)

    def test_complete_ten_wave_chain_is_accepted(self) -> None:
        verify_waves.validate_sealed_wave_documents(
            sealed_waves(),
            expected_identities=identities(),
            envelope_sessions=sessions(),
        )

    def test_missing_wave_fails_closed(self) -> None:
        with self.assertRaises(base.FailClosed):
            verify_waves.validate_sealed_wave_documents(
                sealed_waves()[:-1],
                expected_identities=identities(),
                envelope_sessions=sessions(),
            )

    def test_budget_and_identity_tampering_fail_closed(self) -> None:
        for field in ("budget", "identity", "duplicate"):
            waves = copy.deepcopy(sealed_waves())
            if field == "budget":
                waves[4]["maximum_budget_usd"] = 25.0
            elif field == "identity":
                waves[4]["authorized_episode_keys"][0]["case_id"] = "wrong__case-9"
            else:
                waves[4]["requested_ranks"] = [8, 9]
            with self.subTest(field=field), self.assertRaises(base.FailClosed):
                verify_waves.validate_sealed_wave_documents(
                    waves,
                    expected_identities=identities(),
                    envelope_sessions=sessions(),
                )

    def test_compatibility_manifest_must_bind_cumulative_root(self) -> None:
        schedule = {"schedule": 1}
        envelope = {"envelope": 1}
        registration = {"registration": 1}
        selection = {"selection": 1}
        runner = {
            "sha256": contract.sha_file(
                verify_waves.BASE / "run_selector.py"
            )
        }
        wave_runner = {"wave_runner": 1}
        wave_assembler = {"wave_assembler": 1}
        selection_binding = {"selection_binding": 1}
        envelope_binding = {"envelope_binding": 1}
        cases = [{"rank": 1}]
        compatibility = {
            key: None for key in verify_waves.run_wave.COMPATIBILITY_KEYS
        }
        compatibility.update(
            {
                "schema_version": base.RUN_SCHEMA,
                "evidence_root": "/",
                "registration": registration,
                "selection": selection,
                "runner": runner,
                "wave_schedule": schedule,
                "wave_selection_binding": selection_binding,
                "wave_runner": wave_runner,
                "wave_assembler": wave_assembler,
                "episode_envelope": envelope,
                "wave_envelope_binding": envelope_binding,
                "batch_id": "wave-001",
                "explicit_requested_ranks": [1],
                "output_root": "/wrong-root",
                "cases": cases,
            }
        )
        with (
            mock.patch.object(
                verify_waves,
                "load_ref_json",
                return_value=(Path("/compatibility.json"), compatibility),
            ),
            self.assertRaises(base.FailClosed),
        ):
            verify_waves._verify_compatibility_manifest(  # noqa: SLF001
                {"compatibility": 1},
                output_root=Path("/expected-root"),
                wave_manifest={"cases": cases, "evidence_root": "/"},
                schedule_ref=schedule,
                envelope_ref=envelope,
                registration_ref=registration,
                selection_ref=selection,
                wave_runner_ref=wave_runner,
                wave_assembler_ref=wave_assembler,
                selection_binding_ref=selection_binding,
                envelope_binding_ref=envelope_binding,
                batch_id="wave-001",
                requested_ranks=[1],
            )


if __name__ == "__main__":
    unittest.main()
