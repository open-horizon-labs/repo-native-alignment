from __future__ import annotations

import json
from types import SimpleNamespace
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import run_wave  # noqa: E402
import assemble_wave  # noqa: E402


class PriorWaveSealTests(unittest.TestCase):
    def test_successor_dns_aliases_are_literal_and_docker_is_denied(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            read = root / "read"
            write = root / "write"
            read.mkdir()
            write.mkdir()
            profile = run_wave.generate_successor_outer_seatbelt_profile(
                read_roots=[read],
                write_roots=[write],
            )
        self.assertIn('(literal "/etc")', profile)
        self.assertIn('(literal "/var")', profile)
        self.assertNotIn('(subpath "/etc")', profile)
        self.assertNotIn('(subpath "/var")', profile)
        for kind, path in run_wave.DOCKER_NETWORK_SURFACES:
            self.assertIn(
                f'(deny network-outbound ({kind} "{path}"))',
                profile,
            )

    def test_adapter_and_runner_manifest_interfaces_are_exact(self) -> None:
        self.assertEqual(
            assemble_wave.WAVE_MANIFEST_KEYS,
            run_wave.MANIFEST_KEYS,
        )
        self.assertEqual(
            assemble_wave.COMPATIBILITY_KEYS,
            run_wave.COMPATIBILITY_KEYS,
        )
        self.assertEqual(
            run_wave.contract.WAVE_MANIFEST_SCHEMA,
            assemble_wave.contract.WAVE_MANIFEST_SCHEMA,
        )

    def test_v4_episode_keys_must_be_projected_from_frozen_cases(self) -> None:
        expected = [
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "A",
                "session_id": "session-a",
            },
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "T",
                "session_id": "session-t",
            },
        ]
        manifest = {
            "cases": [
                {
                    "rank": 1,
                    "instance_id": "owner__repo-1",
                    "arm_order": ["A", "T"],
                    "arms": {
                        "A": {"session_id": "session-a"},
                        "T": {"session_id": "session-t"},
                    },
                }
            ],
            "execution_episode_keys": expected,
        }
        receipt = {"execution_episode_keys": expected}
        handoff = {"exact_requested_episode_order": expected}
        assemble_wave.validate_execution_episode_keys(
            manifest,
            receipt,
            handoff,
        )

        forged = [dict(item) for item in expected]
        forged[0]["case_id"] = "owner__repo-forged"
        manifest["execution_episode_keys"] = forged
        receipt["execution_episode_keys"] = forged
        handoff["exact_requested_episode_order"] = forged
        with self.assertRaises(run_wave.base.FailClosed):
            assemble_wave.validate_execution_episode_keys(
                manifest,
                receipt,
                handoff,
            )

    def test_paid_wave_input_must_be_canonical_adapter_output(self) -> None:
        compatibility_path = (
            Path("/evidence/wave") / run_wave.contract.COMPATIBILITY_FILENAME
        )
        canonical = (
            compatibility_path.parent
            / run_wave.contract.WAVE_MANIFEST_FILENAME
        )
        run_wave.require_canonical_wave_manifest_path(
            canonical,
            compatibility_path,
        )
        with self.assertRaises(run_wave.base.FailClosed):
            run_wave.require_canonical_wave_manifest_path(
                Path("/evidence/copied-wave-manifest.json"),
                compatibility_path,
            )

    def test_successor_envelope_binding_coexists_with_preserved_v5(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_root = Path(directory).resolve()
            old_binding = evidence_root / "v5-envelope-binding.json"
            old_binding.write_text("{}\n")
            successor = (
                evidence_root
                / run_wave.contract.ENVELOPE_BINDING_FILENAME
            )
            assemble_wave.validate_envelope_binding_path(
                successor,
                evidence_root,
            )
            self.assertTrue(old_binding.is_file())
            self.assertFalse(successor.exists())
            with self.assertRaises(run_wave.base.FailClosed):
                assemble_wave.validate_envelope_binding_path(
                    old_binding,
                    evidence_root,
                )

    def test_qualification_bridge_accepts_only_exact_registered_delta(
        self,
    ) -> None:
        current = json.loads(
            (ROOT.parent / "issue836-v4" / "registration.json").read_bytes()
        )
        qualified = json.loads(
            (ROOT.parent / "issue830" / "registration.json").read_bytes()
        )
        qualification = json.loads(
            (
                ROOT.parent
                / "issue830"
                / "qualification-closure.manifest.json"
            ).read_bytes()
        )
        run_wave.contract.validate_qualification_compatibility(
            run_wave.contract.QUALIFICATION_COMPATIBILITY,
            qualification,
            current,
            qualified,
        )
        compatibility = {
            "qualification_closure": {
                "manifest": run_wave.contract.file_ref(
                    ROOT.parent
                    / "issue830"
                    / "qualification-closure.manifest.json"
                )
            }
        }
        schedule = {
            "qualification_compatibility": (
                run_wave.contract.QUALIFICATION_COMPATIBILITY
            )
        }

        def exact_registered_files_mismatch(*_args: object) -> None:
            raise run_wave.base.FailClosed(
                "qualification manifest binding mismatch: "
                "registered_files_sha256"
            )

        run_wave.verify_v4_qualification_compatibility(
            compatibility,
            current,
            schedule,
            qualification_verifier=exact_registered_files_mismatch,
        )

        def another_failure(*_args: object) -> None:
            raise run_wave.base.FailClosed("another qualification failure")

        with self.assertRaises(run_wave.base.FailClosed):
            run_wave.verify_v4_qualification_compatibility(
                compatibility,
                current,
                schedule,
                qualification_verifier=another_failure,
            )

        tampered = json.loads(json.dumps(current))
        tampered["registered_files"]["runner_sha256"] = "0" * 64
        with self.assertRaises(run_wave.contract.ContractError):
            run_wave.contract.validate_qualification_compatibility(
                run_wave.contract.QUALIFICATION_COMPATIBILITY,
                qualification,
                tampered,
                qualified,
            )

    def test_exact_complete_prior_wave_is_accepted(self) -> None:
        run_wave.require_sealed_prior_wave(
            {
                "all_authorized_episodes_recorded": True,
                "worker_errors": [],
                "episode_receipts": [{}, {}, {}, {}],
            },
            (1, 2),
        )

    def test_partial_or_failed_prior_wave_blocks_more_spend(self) -> None:
        invalid = (
            {
                "all_authorized_episodes_recorded": False,
                "worker_errors": [],
                "episode_receipts": [{}, {}, {}, {}],
            },
            {
                "all_authorized_episodes_recorded": True,
                "worker_errors": ["failed"],
                "episode_receipts": [{}, {}, {}, {}],
            },
            {
                "all_authorized_episodes_recorded": True,
                "worker_errors": [],
                "episode_receipts": [{}, {}, {}],
            },
        )
        for receipt in invalid:
            with self.subTest(receipt=receipt), self.assertRaises(
                run_wave.base.FailClosed
            ):
                run_wave.require_sealed_prior_wave(receipt, (1, 2))

    def test_arbitrary_or_zero_model_episode_receipts_do_not_consume(self) -> None:
        compatibility = {"path": "/compat", "bytes": 1, "sha256": "a" * 64}
        registration = {"path": "/registration", "bytes": 1, "sha256": "b" * 64}
        selection = {"path": "/selection", "bytes": 1, "sha256": "c" * 64}
        authorized = [
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "A",
                "session_id": "session-a",
            }
        ]
        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory).resolve() / "output"
            receipt_path = (
                output_root
                / "rank-01-owner__repo-1"
                / "A"
                / "episode-receipt.json"
            )
            receipt_path.parent.mkdir(parents=True)
            for document in (
                {"not": "an episode receipt"},
                {
                    "schema_version": run_wave.base.RECEIPT_SCHEMA,
                    **authorized[0],
                    "run_manifest": compatibility,
                    "registration": registration,
                    "selection": selection,
                    "official_evaluator_invoked": False,
                    "token_ledger": {"model_invoked": False},
                },
            ):
                receipt_path.write_text(
                    json.dumps(document, sort_keys=True) + "\n"
                )
                with self.subTest(document=document), self.assertRaises(
                    run_wave.base.FailClosed
                ):
                    run_wave.validate_consumed_episode_refs(
                        [run_wave.contract.file_ref(receipt_path)],
                        authorized_episode_keys=authorized,
                        compatibility_ref=compatibility,
                        registration_ref=registration,
                        selection_ref=selection,
                        output_root=output_root,
                        where="synthetic wave",
                    )

    def test_consumed_failure_requires_canonical_path_and_exact_identity(self) -> None:
        compatibility = {"path": "/compat", "bytes": 1, "sha256": "a" * 64}
        registration = {"path": "/registration", "bytes": 1, "sha256": "b" * 64}
        selection = {"path": "/selection", "bytes": 1, "sha256": "c" * 64}
        authorized = [
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "A",
                "session_id": "session-a",
            }
        ]
        receipt = {
            "schema_version": run_wave.base.RECEIPT_SCHEMA,
            **authorized[0],
            "run_manifest": compatibility,
            "registration": registration,
            "selection": selection,
            "official_evaluator_invoked": False,
            "token_ledger": {"model_invoked": True},
            "errors": ["model_exit_1"],
            "policy_compliant": False,
        }
        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory).resolve() / "output"
            canonical = (
                output_root
                / "rank-01-owner__repo-1"
                / "A"
                / "episode-receipt.json"
            )
            canonical.parent.mkdir(parents=True)
            canonical.write_text(json.dumps(receipt, sort_keys=True) + "\n")
            accepted = run_wave.validate_consumed_episode_refs(
                [run_wave.contract.file_ref(canonical)],
                authorized_episode_keys=authorized,
                compatibility_ref=compatibility,
                registration_ref=registration,
                selection_ref=selection,
                output_root=output_root,
                where="synthetic wave",
            )
            self.assertEqual(
                accepted,
                [(1, "owner__repo-1", "A", "session-a")],
            )
            escaped = output_root.parent / "episode-receipt.json"
            escaped.write_text(json.dumps(receipt, sort_keys=True) + "\n")
            with self.assertRaises(run_wave.base.FailClosed):
                run_wave.validate_consumed_episode_refs(
                    [run_wave.contract.file_ref(escaped)],
                    authorized_episode_keys=authorized,
                    compatibility_ref=compatibility,
                    registration_ref=registration,
                    selection_ref=selection,
                    output_root=output_root,
                    where="synthetic wave",
                )

    def test_zero_model_first_arm_stops_same_case(self) -> None:
        prepared = SimpleNamespace(
            output_root=None,
            manifest_ref={"manifest": 1},
            registration_ref={"registration": 1},
            selection_ref={"selection": 1},
        )
        case = SimpleNamespace(
            rank=1,
            case_id="owner__repo-1",
            arm_order=("A", "T"),
            sessions={"A": "session-a", "T": "session-t"},
        )
        with tempfile.TemporaryDirectory() as directory:
            prepared.output_root = Path(directory).resolve() / "output"
            receipt = {
                "episode_receipt": {"path": "/unused"},
                "token_ledger": {"model_invoked": False},
            }
            with (
                mock.patch.object(
                    run_wave.base,
                    "materialize_harness",
                    return_value={},
                ),
                mock.patch.object(
                    run_wave.base,
                    "launch_episode",
                    return_value=receipt,
                ) as launch,
            ):
                receipts, errors = run_wave.execute_case_once(prepared, case)
            self.assertEqual(launch.call_count, 1)
            self.assertEqual(receipts, [receipt])
            self.assertEqual(
                errors,
                [
                    "owner__repo-1/A: "
                    "retryable_pre_model_failure_not_consumed"
                ],
            )

    def test_execution_claim_is_cross_process_exclusive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory).resolve() / "output"
            script = (
                "import pathlib,sys;"
                "sys.path.insert(0,sys.argv[2]);"
                "import run_wave;"
                "\ntry:\n"
                "  with run_wave.execution_claim(pathlib.Path(sys.argv[1])):"
                "\n    pass\n"
                "except run_wave.base.FailClosed:\n"
                "  raise SystemExit(3)\n"
            )
            with run_wave.execution_claim(output_root):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-c",
                        script,
                        str(output_root),
                        str(ROOT),
                    ],
                    check=False,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
            self.assertEqual(result.returncode, 3, result.stderr.decode())


if __name__ == "__main__":
    unittest.main()
