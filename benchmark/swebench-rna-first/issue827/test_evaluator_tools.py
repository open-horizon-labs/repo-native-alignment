#!/usr/bin/env python3
"""Focused synthetic tests for the fresh issue #827 evaluator gate."""

from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import evaluator_authorization
import evaluator_runner as runner
import provider_usage
import registration_contract
import run_selector as selector_runner
import select_result


DATASET_SHA = "0d119efe73413554335bd410a04d82fd4a586bfd312cee677ee40af5de2ac46e"
ZERO = "0" * 64


def write_json(path: Path, value: object) -> dict[str, object]:
    path.parent.mkdir(parents=True, exist_ok=True)
    data = runner.canonical_json_bytes(value)
    path.write_bytes(data)
    return {
        "path": str(path.resolve()),
        "bytes": len(data),
        "sha256": runner.sha256_bytes(data),
    }


def write_bytes(path: Path, data: bytes) -> dict[str, object]:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return {
        "path": str(path.resolve()),
        "bytes": len(data),
        "sha256": runner.sha256_bytes(data),
    }


def evaluator_config() -> dict[str, object]:
    return {
        "python": "/synthetic/python",
        "python_realpath": "/synthetic/python-real",
        "python_sha256": ZERO,
        "swebench_version": "4.1.0",
        "swebench_record": "/synthetic/RECORD",
        "swebench_record_sha256": ZERO,
        "run_evaluation": "/synthetic/run_evaluation.py",
        "run_evaluation_sha256": ZERO,
        "distribution_lock_sha256": ZERO,
        "dataset_name": "princeton-nlp/SWE-bench_Verified",
        "dataset_split": "test",
        "dataset_cache_root": "/synthetic/dataset-cache",
        "dataset_arrow": "/synthetic/swe-bench_verified-test.arrow",
        "dataset_arrow_sha256": DATASET_SHA,
        "dataset_info": "/synthetic/dataset_info.json",
        "dataset_info_sha256": ZERO,
        "docker_server": "synthetic|server|linux|amd64",
    }


def registration(evaluator: dict[str, object]) -> dict[str, object]:
    value = json.loads((HERE / "registration.template.json").read_bytes())
    value["dataset"]["arrow_sha256"] = DATASET_SHA
    value["evaluator"].update(
        {
            "python_sha256": evaluator["python_sha256"],
            "distribution_lock_sha256": evaluator[
                "distribution_lock_sha256"
            ],
            "swebench_record_sha256": evaluator["swebench_record_sha256"],
            "run_evaluation_sha256": evaluator["run_evaluation_sha256"],
            "dataset_info_sha256": evaluator["dataset_info_sha256"],
            "docker_server": evaluator["docker_server"],
        }
    )
    for key, filename in registration_contract.REGISTERED_FILE_NAMES.items():
        value["registered_files"][key] = registration_contract.sha256_file(
            HERE / filename
        )
    value["rna_artifact"]["producer_commit"] = "b" * 40
    for key in registration_contract.RNA_ARTIFACT_FIELDS - {
        "producer_commit",
        "local_source_build_allowed",
    }:
        value["rna_artifact"][key] = ZERO
    value["qualification_closure"]["manifest_sha256"] = ZERO
    value["qualification_closure"]["archive_sha256"] = ZERO
    return value


def valid_token_ledger(total: int) -> dict[str, object]:
    return {
        "schema_version": provider_usage.SCHEMA_VERSION,
        "valid": True,
        "errors": [],
        "source": "top_level_usage+model_events",
        "model_invoked": True,
        "input_tokens": total - 100,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "output_tokens": 100,
        "reasoning_tokens": 10,
        "reasoning_tokens_observed": True,
        "unobserved_fields": [],
        "provider_total_tokens": total,
        "cli_turns": 1,
        "provider_responses": 1,
        "provider_responses_scope": "agent_transcript_only",
        "provider_requests": None,
    }


def no_model_token_ledger() -> dict[str, object]:
    return {
        "schema_version": provider_usage.SCHEMA_VERSION,
        "valid": True,
        "errors": [],
        "source": "model_not_invoked",
        "model_invoked": False,
        "input_tokens": None,
        "cache_creation_input_tokens": None,
        "cache_read_input_tokens": None,
        "output_tokens": None,
        "reasoning_tokens": None,
        "reasoning_tokens_observed": False,
        "unobserved_fields": ["reasoning_tokens"],
        "provider_total_tokens": None,
        "cli_turns": 0,
        "provider_responses": 0,
        "provider_responses_scope": "agent_transcript_only",
        "provider_requests": 0,
    }


class Fixture:
    def __init__(
        self,
        root: Path,
        *,
        patch_keys: set[tuple[int, str]],
        noncompliant_keys: set[tuple[int, str]] = frozenset(),
        incomplete_keys: set[tuple[int, str]] = frozenset(),
        pre_model_keys: set[tuple[int, str]] = frozenset(),
        invalid_token_keys: set[tuple[int, str]] = frozenset(),
        arm_tokens: dict[str, int] | None = None,
    ) -> None:
        self.root = root.resolve()
        self.evidence = self.root / "evidence"
        self.model = self.evidence / "episodes"
        self.output = self.evidence / "terminal-evaluations"
        self.registry = self.evidence / "irrevocable-registry"
        self.evidence.mkdir(parents=True)
        self.arm_tokens = arm_tokens or {"A": 1000, "T": 800}
        self.cases = [
            {
                "rank": 1,
                "instance_id": "project__project-100",
                "base_commit": "1" * 40,
                "base_tree": "a" * 40,
                "arm_order": ["A", "T"],
            },
            {
                "rank": 2,
                "instance_id": "project__project-200",
                "base_commit": "2" * 40,
                "base_tree": "b" * 40,
                "arm_order": ["T", "A"],
            },
        ]
        evaluator = evaluator_config()
        registration_ref = write_json(
            self.evidence / "registration.json", registration(evaluator)
        )
        selection = {
            "schema_version": "issue827-fresh-pair-selection-v1",
            "state": "selected_pre_model",
            "authoritative": True,
            "registration_sha256": registration_ref["sha256"],
            "problem_statements_inspected_by_human_before_selection": False,
            "gold_or_outcomes_inspected_before_selection": False,
            "fresh_case_claim": True,
            "prior_model_calls": 0,
            "cases": self.cases,
        }
        selection_ref = write_json(self.evidence / "selection.json", selection)

        episodes: list[dict[str, object]] = []
        for case in self.cases:
            for arm in ("A", "T"):
                key = (case["rank"], arm)
                directory = self.model / f"rank-{case['rank']}" / arm
                patch_ref = (
                    write_bytes(
                        directory / "terminal.patch",
                        (
                            f"diff --git a/{arm} b/{arm}\n"
                            f"+synthetic {key}\n"
                        ).encode(),
                    )
                    if key in patch_keys
                    else None
                )
                pre_model = key in pre_model_keys
                noncompliant = key in noncompliant_keys or pre_model
                incomplete = key in incomplete_keys or key in invalid_token_keys
                token_ledger = (
                    no_model_token_ledger()
                    if pre_model
                    else valid_token_ledger(self.arm_tokens[arm])
                )
                if key in invalid_token_keys:
                    token_ledger = {
                        **token_ledger,
                        "valid": False,
                        "errors": ["missing_complete_provider_usage"],
                        "input_tokens": None,
                        "cache_creation_input_tokens": None,
                        "cache_read_input_tokens": None,
                        "output_tokens": None,
                        "reasoning_tokens": None,
                        "provider_total_tokens": None,
                        "cli_turns": None,
                    }
                eligible = (
                    patch_ref is not None
                    and not noncompliant
                    and not incomplete
                )
                actor = {
                    "schema_version": evaluator_authorization.ACTOR_SCHEMA,
                    "arm": arm,
                    "actions": [
                        {
                            "sequence": 1,
                            "actor": "harness",
                            "action": (
                                "official_evaluator_authorization_request"
                            ),
                            "requested": eligible,
                            "authorized": False,
                            "invoked": False,
                        }
                    ],
                }
                actor_ref = write_json(directory / "actor-tool-ledger.json", actor)
                timing = {
                    "model_wall_seconds": 0.0 if pre_model else 100.0,
                    "rna_preprocessing_seconds": (
                        1.0 if pre_model and arm == "T" else 0.0
                    ),
                    "combined_pre_evaluator_wall_seconds": (
                        1.0 if pre_model and arm == "T" else 100.0
                    ),
                }
                receipt = {
                    "schema_version": "issue827-episode-receipt-v1",
                    "case_id": case["instance_id"],
                    "rank": case["rank"],
                    "arm": arm,
                    "policy": "control" if arm == "A" else "treatment",
                    "session_id": f"fresh-rank-{case['rank']}-{arm}",
                    "base_commit": case["base_commit"],
                    "base_tree": case["base_tree"],
                    "registration": registration_ref,
                    "selection": selection_ref,
                    "terminal_patch": patch_ref,
                    "actor_tool_ledger": actor_ref,
                    "policy_compliant": not noncompliant,
                    "evidence_complete": not incomplete,
                    "errors": (
                        ["synthetic_policy_failure"] if noncompliant else []
                    ),
                    "authorization_requested": eligible,
                    "evaluator_authorized": False,
                    "official_evaluator_invoked": False,
                    "returncode": None if pre_model else 0,
                    "timed_out": False,
                    "token_ledger": token_ledger,
                    "timing_ledger": timing,
                }
                receipt_path = directory / "episode-receipt.json"
                receipt_ref = write_json(receipt_path, receipt)
                verification = {
                    "schema_version": "issue827-episode-verification-v1",
                    "case_id": case["instance_id"],
                    "rank": case["rank"],
                    "arm": arm,
                    "episode_receipt": receipt_ref,
                    "terminal_patch": patch_ref,
                    "terminal_patch_sha256": (
                        patch_ref["sha256"] if patch_ref is not None else None
                    ),
                    "policy_compliant": not noncompliant,
                    "evidence_complete": not incomplete,
                    "errors": (
                        ["synthetic_verifier_failure"] if incomplete else []
                    ),
                    "evaluator_authorized": eligible,
                    "official_evaluator_invoked": False,
                    "token_ledger": token_ledger,
                    "timing_ledger": timing,
                }
                verification_path = directory / "episode-verification.json"
                authorization_ref = None
                if eligible:
                    authorization = evaluator_authorization.build(
                        receipt_path, receipt, verification_path, verification
                    )
                    authorization_ref = write_json(
                        directory / "evaluator-authorization.json",
                        authorization,
                    )
                verification_ref = write_json(
                    verification_path, verification
                )
                image_tag = f"issue827-rank{case['rank']}"
                episodes.append(
                    {
                        "case_id": case["instance_id"],
                        "rank": case["rank"],
                        "arm": arm,
                        "base_commit": case["base_commit"],
                        "base_tree": case["base_tree"],
                        "episode_receipt": receipt_ref,
                        "episode_verification": verification_ref,
                        "evaluator_authorization": authorization_ref,
                        "model_name_or_path": f"claude-sonnet-5-{arm}",
                        "run_id": f"issue827-rank{case['rank']}-{arm}",
                        "official_image": (
                            f"swebench/sweb.eval.synthetic:{image_tag}"
                        ),
                        "official_image_source": (
                            "docker.io/swebench/sweb.eval.synthetic:latest"
                        ),
                        "official_image_manifest_digest": "sha256:" + "3" * 64,
                        "official_image_config_id": "sha256:" + "4" * 64,
                        "official_image_local_id": "sha256:" + "4" * 64,
                        "official_image_tag": image_tag,
                    }
                )
        plan = {
            "schema_version": runner.PLAN_SCHEMA,
            "registration": registration_ref,
            "selection": selection_ref,
            "evidence_root": str(self.evidence),
            "model_output_root": str(self.model),
            "output_root": str(self.output),
            "registry_root": str(self.registry),
            "max_parallel": 2,
            "evaluator_wall_seconds": 3600,
            "evaluator": evaluator,
            "episodes": episodes,
        }
        self.plan_path = self.evidence / "evaluator-plan.json"
        write_json(self.plan_path, plan)

    def validated_plan(self) -> dict[str, object]:
        return runner.validate_plan(self.plan_path)


class AuthoritativeSelectionInputTests(unittest.TestCase):
    def setUp(self) -> None:
        self.registration_bytes = runner.canonical_json_bytes(
            {
                "schema_version": "issue827-treatment-registration-v1",
                "issue": 827,
            }
        )
        self.valid_selection = {
            "authoritative": True,
            "state": "selected_pre_model",
            "problem_statements_inspected_by_human_before_selection": False,
            "gold_or_outcomes_inspected_before_selection": False,
            "fresh_case_claim": True,
            "prior_model_calls": 0,
            "registration_sha256": runner.sha256_bytes(
                self.registration_bytes
            ),
        }

    def validators(self):
        return (
            (
                selector_runner.validate_authoritative_selection,
                selector_runner.FailClosed,
            ),
            (runner.validate_authoritative_selection, runner.FailClosed),
        )

    def test_exact_fresh_selection_is_accepted(self) -> None:
        for validator, _ in self.validators():
            with self.subTest(validator=validator.__module__):
                validator(dict(self.valid_selection), self.registration_bytes)

    def test_amendment_and_prior_exposure_fields_are_rejected(self) -> None:
        invalid_updates = (
            {"state": "selected_pre_amended_rerun"},
            {"fresh_case_claim": False},
            {"prior_model_calls": 1},
            {"runtime_amendment": {"schema_version": "forbidden"}},
        )
        for validator, failure in self.validators():
            for update in invalid_updates:
                with self.subTest(
                    validator=validator.__module__, update=update
                ):
                    selection = {**self.valid_selection, **update}
                    if "runtime_amendment" in update:
                        selection["state"] = "selected_pre_amended_rerun"
                    with self.assertRaises(failure):
                        validator(selection, self.registration_bytes)

    def test_setup_qualification_and_inspected_inputs_are_rejected(self) -> None:
        invalid_updates = (
            {"authoritative": False},
            {"authoritative": 1},
            {"state": "postfix_setup_qualification_not_selection_evidence"},
            {"problem_statements_inspected_by_human_before_selection": True},
            {"gold_or_outcomes_inspected_before_selection": True},
            {"registration_sha256": ZERO},
        )
        for validator, failure in self.validators():
            for update in invalid_updates:
                with self.subTest(
                    validator=validator.__module__, update=update
                ):
                    with self.assertRaises(failure):
                        validator(
                            {**self.valid_selection, **update},
                            self.registration_bytes,
                        )


class EvaluatorToolsTests(unittest.TestCase):
    def test_plan_consumes_reproducible_independent_authorization(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary), patch_keys={(1, "A")})
            plan = fixture.validated_plan()
            episode = next(
                item
                for item in plan["_validated_episodes"]
                if item["episode"]["rank"] == 1
                and item["episode"]["arm"] == "A"
            )
            self.assertEqual(episode["disposition"], "evaluate")
            self.assertEqual(
                episode["authorization"]["decision"], "authorize_once"
            )
            self.assertTrue(episode["authorization"]["one_use"])
            self.assertFalse(episode["receipt"]["evaluator_authorized"])

            source = episode["authorization_path"]
            value = json.loads(source.read_bytes())
            value["authorization_id"] = ZERO
            source.write_bytes(runner.canonical_json_bytes(value))
            plan_value = runner.read_json(fixture.plan_path)
            for planned in plan_value["episodes"]:
                if planned["rank"] == 1 and planned["arm"] == "A":
                    planned["evaluator_authorization"] = (
                        runner.file_reference(source)
                    )
            fixture.plan_path.write_bytes(
                runner.canonical_json_bytes(plan_value)
            )
            with self.assertRaisesRegex(
                runner.FailClosed, "authorization.*not_reproducible"
            ):
                fixture.validated_plan()

    def test_invalid_tokens_only_allow_unauthorized_zero_invocation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(
                Path(temporary),
                patch_keys={(1, "A")},
                invalid_token_keys={(1, "A")},
            )
            plan = fixture.validated_plan()
            episode = next(
                item
                for item in plan["_validated_episodes"]
                if item["episode"]["rank"] == 1
                and item["episode"]["arm"] == "A"
            )
            self.assertEqual(episode["disposition"], "incomplete_evidence")
            self.assertIsNone(
                episode["episode"]["evaluator_authorization"]
            )
            runner.seal_all(plan)
            seal = next(
                item
                for item in runner.validate_seal_set(plan)["seals"]
                if item["seal"]["rank"] == 1
                and item["seal"]["arm"] == "A"
            )
            result = runner.write_skip_receipt(plan, seal)
            self.assertFalse(
                result["official_evaluator_invocation_authorized"]
            )
            self.assertFalse(
                result["official_evaluator_invocation_confirmed"]
            )

    def test_pre_model_t_failure_has_null_usage_and_no_authorization(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(
                Path(temporary),
                patch_keys=set(),
                pre_model_keys={(1, "T")},
            )
            plan = fixture.validated_plan()
            episode = next(
                item
                for item in plan["_validated_episodes"]
                if item["episode"]["rank"] == 1
                and item["episode"]["arm"] == "T"
            )
            self.assertEqual(episode["disposition"], "noncompliant")
            self.assertFalse(
                episode["receipt"]["token_ledger"]["model_invoked"]
            )
            self.assertIsNone(
                episode["receipt"]["token_ledger"][
                    "provider_total_tokens"
                ]
            )

    def test_seal_and_registry_claim_are_authorization_id_bound_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(
                Path(temporary), patch_keys={(1, "A"), (1, "T")}
            )
            plan = fixture.validated_plan()
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            seal = next(
                item
                for item in seal_set["seals"]
                if item["seal"]["rank"] == 1
                and item["seal"]["arm"] == "A"
            )
            claim = runner.claim_evaluation(plan, seal)
            expected = (
                fixture.registry
                / (
                    "authorization-"
                    f"{seal['seal']['evaluator_authorization_id']}"
                    ".evaluation-started.json"
                )
            )
            self.assertEqual(Path(claim["path"]), expected)
            value = json.loads(expected.read_bytes())
            self.assertTrue(value["one_use"])
            self.assertEqual(
                value["evaluator_authorization_id"],
                seal["seal"]["evaluator_authorization_id"],
            )
            with self.assertRaisesRegex(
                runner.FailClosed, "already claimed"
            ):
                runner.claim_evaluation(plan, seal)

    def test_popen_failure_records_claim_but_zero_invocations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary), patch_keys={(1, "A")})
            plan = fixture.validated_plan()
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            seal = next(
                item
                for item in seal_set["seals"]
                if item["seal"]["rank"] == 1
                and item["seal"]["arm"] == "A"
            )
            episode = next(
                item
                for item in plan["episodes"]
                if item["rank"] == 1 and item["arm"] == "A"
            )
            case_state = {"poisoned": None}
            with (
                mock.patch.object(
                    runner,
                    "ensure_container_absent",
                    return_value={"absent": True},
                ),
                mock.patch.object(
                    runner,
                    "inspect_pinned_image",
                    return_value={
                        "id": episode["official_image_local_id"],
                        "repo_tags": [episode["official_image"]],
                    },
                ),
                mock.patch.object(
                    runner.subprocess,
                    "Popen",
                    side_effect=OSError("synthetic Popen failure"),
                ),
            ):
                result = runner.evaluate_one(
                    plan,
                    episode,
                    seal,
                    threading.Lock(),
                    case_state,
                )
            receipt = json.loads(
                Path(result["receipt"]["path"]).read_bytes()
            )
            self.assertTrue(
                receipt["official_evaluator_invocation_authorized"]
            )
            self.assertFalse(
                receipt["official_evaluator_invocation_confirmed"]
            )
            self.assertEqual(receipt["official_evaluator_invocations"], 0)
            self.assertEqual(
                receipt["evaluator_authorization_id"],
                seal["seal"]["evaluator_authorization_id"],
            )
            self.assertIsNotNone(receipt["irrevocable_registry_start"])

    def test_container_absence_requires_explicit_not_found(self) -> None:
        not_found = runner.subprocess.CompletedProcess(
            ["docker"], 1, b"", b"Error: No such container: synthetic"
        )
        with mock.patch.object(runner, "run_bytes", return_value=not_found):
            proof = runner.ensure_container_absent(
                "synthetic", remove=False
            )
        self.assertTrue(proof["absent"])

        daemon_error = runner.subprocess.CompletedProcess(
            ["docker"],
            1,
            b"",
            b"permission denied connecting to daemon",
        )
        with mock.patch.object(
            runner, "run_bytes", return_value=daemon_error
        ):
            with self.assertRaises(runner.FailClosed):
                runner.ensure_container_absent(
                    "synthetic", remove=False
                )

    def test_registered_decision_rule_is_unchanged(self) -> None:
        metrics = []
        for rank in (1, 2):
            for arm in ("A", "T"):
                metrics.append(
                    {
                        "rank": rank,
                        "arm": arm,
                        "evidence_complete": True,
                        "policy_compliant": True,
                        "outcome_valid": True,
                        "resolved": False,
                        "pass_to_pass_regressions": 0,
                        "provider_total_tokens": (
                            1000 if arm == "A" else 800
                        ),
                        "combined_pre_evaluator_wall_seconds": 100.0,
                    }
                )
        decision = select_result.decide_registered(metrics)
        self.assertEqual(decision["decision"], "selected_T")
        self.assertEqual(
            decision["classification"], "material_efficiency_selection"
        )
        self.assertEqual(
            decision["thresholds"],
            {
                "token_reduction_percent": 15,
                "maximum_token_increase_percent_for_time_path": 5,
                "time_reduction_percent": 20,
            },
        )

    def test_registration_contract_drift_fails_plan(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary), patch_keys=set())
            plan = runner.read_json(fixture.plan_path)
            registration_path = Path(plan["registration"]["path"])
            value = runner.read_json(registration_path)
            value["evaluator"]["one_use_registry_claim_required"] = False
            plan["registration"] = write_json(registration_path, value)
            selection_path = Path(plan["selection"]["path"])
            selection = runner.read_json(selection_path)
            selection["registration_sha256"] = plan["registration"][
                "sha256"
            ]
            plan["selection"] = write_json(selection_path, selection)
            for episode in plan["episodes"]:
                receipt_path = Path(episode["episode_receipt"]["path"])
                receipt = runner.read_json(receipt_path)
                receipt["registration"] = plan["registration"]
                receipt["selection"] = plan["selection"]
                episode["episode_receipt"] = write_json(
                    receipt_path, receipt
                )
                verification_path = Path(
                    episode["episode_verification"]["path"]
                )
                verification = runner.read_json(verification_path)
                verification["episode_receipt"] = episode[
                    "episode_receipt"
                ]
                episode["episode_verification"] = write_json(
                    verification_path, verification
                )
            fixture.plan_path.write_bytes(
                runner.canonical_json_bytes(plan)
            )
            with self.assertRaisesRegex(
                runner.FailClosed, "registration evaluator contract drift"
            ):
                fixture.validated_plan()


if __name__ == "__main__":
    unittest.main()
