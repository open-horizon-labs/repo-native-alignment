#!/usr/bin/env python3
"""Focused synthetic tests for #825 evaluator sealing and result selection."""

from __future__ import annotations

import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest import mock

import evaluator_runner as runner
import select_result


DATASET_SHA = "0d119efe73413554335bd410a04d82fd4a586bfd312cee677ee40af5de2ac46e"


def write_json(path: Path, value: object) -> dict[str, object]:
    path.parent.mkdir(parents=True, exist_ok=True)
    data = runner.canonical_json_bytes(value)
    path.write_bytes(data)
    return {"path": str(path), "bytes": len(data), "sha256": runner.sha256_bytes(data)}


def write_bytes(path: Path, data: bytes) -> dict[str, object]:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return {"path": str(path), "bytes": len(data), "sha256": runner.sha256_bytes(data)}


class Fixture:
    def __init__(
        self,
        root: Path,
        *,
        patch_keys: set[tuple[int, str]],
        noncompliant_keys: set[tuple[int, str]] = frozenset(),
        incomplete_keys: set[tuple[int, str]] = frozenset(),
        pre_model_keys: set[tuple[int, str]] = frozenset(),
        arm_tokens: dict[str, int] | None = None,
    ) -> None:
        self.root = root
        self.evidence = root / "evidence"
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
        registration = {
            "schema_version": "issue825-treatment-registration-v2",
            "issue": 825,
            "dataset": {"arrow_sha256": DATASET_SHA},
        }
        selection = {
            "schema_version": "issue825-fresh-pair-selection-v2",
            "cases": self.cases,
        }
        registration_ref = write_json(self.evidence / "registration.json", registration)
        selection_ref = write_json(self.evidence / "selection.json", selection)

        episodes = []
        for case in self.cases:
            for arm in ("A", "T"):
                key = (case["rank"], arm)
                directory = self.model / f"rank-{case['rank']}" / arm
                patch_ref = None
                if key in patch_keys:
                    patch_ref = write_bytes(
                        directory / "terminal.patch",
                        f"diff --git a/{arm} b/{arm}\n+synthetic {key}\n".encode(),
                    )
                pre_model = key in pre_model_keys
                compliant = key not in noncompliant_keys and not pre_model
                complete = key not in incomplete_keys
                receipt_authorized = compliant and patch_ref is not None
                authorized = receipt_authorized and complete
                token_total = self.arm_tokens[arm]
                token_ledger = ({
                    "schema_version": runner.TOKEN_LEDGER_SCHEMA,
                    "valid": True,
                    "errors": [],
                    "source": "model_not_invoked",
                    "model_invoked": False,
                    "input_tokens": None,
                    "output_tokens": None,
                    "cache_creation_input_tokens": None,
                    "cache_read_input_tokens": None,
                    "provider_total_tokens": None,
                    "reasoning_tokens": None,
                    "cli_turns": 0,
                    "provider_responses": 0,
                    "provider_requests": 0,
                } if pre_model else {
                    "schema_version": runner.TOKEN_LEDGER_SCHEMA,
                    "valid": True,
                    "errors": [],
                    "source": "whole_invocation_model_usage",
                    "model_invoked": True,
                    "input_tokens": token_total - 100,
                    "output_tokens": 100,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "provider_total_tokens": token_total,
                    "reasoning_tokens": 0,
                    "cli_turns": 1,
                    "provider_responses": None,
                    "provider_requests": None,
                })
                timing_ledger = {
                    "model_wall_seconds": 0.0 if pre_model else 100.0,
                    "rna_preprocessing_seconds": 1.0 if pre_model else 0.0,
                    "combined_pre_evaluator_wall_seconds": 1.0 if pre_model else 100.0,
                }
                receipt = {
                    "schema_version": "issue825-episode-receipt-v1",
                    "case_id": case["instance_id"],
                    "rank": case["rank"],
                    "arm": arm,
                    "policy": "control" if arm == "A" else "treatment",
                    "session_id": f"synthetic-rank-{case['rank']}-{arm}",
                    "base_commit": case["base_commit"],
                    "base_tree": case["base_tree"],
                    "terminal_patch": patch_ref,
                    "policy_compliant": compliant,
                    "evidence_complete": True,
                    "errors": [] if compliant else ["synthetic_policy_failure"],
                    "evaluator_authorized": receipt_authorized,
                    "returncode": None if pre_model else 0,
                    "official_evaluator_invoked": False,
                    "token_ledger": token_ledger,
                    "timing_ledger": timing_ledger,
                }
                receipt_ref = write_json(directory / "episode-receipt.json", receipt)
                verification = {
                    "schema_version": "issue825-episode-verification-v1",
                    "case_id": case["instance_id"],
                    "rank": case["rank"],
                    "arm": arm,
                    "episode_receipt": receipt_ref,
                    "terminal_patch": patch_ref,
                    "policy_compliant": compliant,
                    "evidence_complete": complete,
                    "errors": [] if complete else ["synthetic_verifier_failure"],
                    "evaluator_authorized": authorized,
                    "official_evaluator_invoked": False,
                    "token_ledger": token_ledger,
                    "timing_ledger": timing_ledger,
                }
                verification_ref = write_json(
                    directory / "episode-verification.json", verification
                )
                image_tag = f"issue825-rank{case['rank']}"
                image_name = f"swebench/sweb.eval.synthetic:{image_tag}"
                episodes.append(
                    {
                        "case_id": case["instance_id"],
                        "rank": case["rank"],
                        "arm": arm,
                        "base_commit": case["base_commit"],
                        "base_tree": case["base_tree"],
                        "episode_receipt": receipt_ref,
                        "episode_verification": verification_ref,
                        "model_name_or_path": f"claude-sonnet-5-{arm}",
                        "run_id": f"issue825-rank{case['rank']}-{arm}",
                        "official_image": image_name,
                        "official_image_source": "docker.io/swebench/sweb.eval.synthetic:latest",
                        "official_image_manifest_digest": "sha256:" + "3" * 64,
                        "official_image_config_id": "sha256:" + "4" * 64,
                        "official_image_local_id": "sha256:" + "4" * 64,
                        "official_image_tag": image_tag,
                    }
                )
        zero = "0" * 64
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
            "evaluator": {
                "python": "/synthetic/python",
                "python_realpath": "/synthetic/python-real",
                "python_sha256": zero,
                "swebench_version": "4.1.0",
                "swebench_record": "/synthetic/RECORD",
                "swebench_record_sha256": zero,
                "run_evaluation": "/synthetic/run_evaluation.py",
                "run_evaluation_sha256": zero,
                "distribution_lock_sha256": zero,
                "dataset_name": "princeton-nlp/SWE-bench_Verified",
                "dataset_split": "test",
                "dataset_cache_root": "/synthetic/dataset-cache",
                "dataset_arrow": "/synthetic/swe-bench_verified-test.arrow",
                "dataset_arrow_sha256": DATASET_SHA,
                "dataset_info": "/synthetic/dataset_info.json",
                "dataset_info_sha256": zero,
                "docker_server": "synthetic|server|linux|amd64",
            },
            "episodes": episodes,
        }
        self.plan_path = self.evidence / "evaluator-plan.json"
        write_json(self.plan_path, plan)

    def validated_plan(self) -> dict[str, object]:
        return runner.validate_plan(self.plan_path)


class EvaluatorToolsTests(unittest.TestCase):
    def test_plan_accepts_pre_model_t_failures_with_null_tokens(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(
                Path(temporary),
                patch_keys={(1, "A"), (2, "A")},
                pre_model_keys={(1, "T"), (2, "T")},
            )
            plan = fixture.validated_plan()
            dispositions = {
                (item["episode"]["rank"], item["episode"]["arm"]): item["disposition"]
                for item in plan["_validated_episodes"]
            }
            self.assertEqual(dispositions[(1, "A")], "evaluate")
            self.assertEqual(dispositions[(2, "A")], "evaluate")
            self.assertEqual(dispositions[(1, "T")], "noncompliant")
            self.assertEqual(dispositions[(2, "T")], "noncompliant")
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            self.assertEqual(
                sum(item["seal"]["disposition"] == "evaluate" for item in seal_set["seals"]),
                2,
            )

    def test_seal_routes_only_compliant_nonempty_patches_and_claim_is_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(
                Path(temporary),
                patch_keys={(1, "A"), (1, "T"), (2, "T")},
                noncompliant_keys={(1, "T")},
            )
            plan = fixture.validated_plan()
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            dispositions = {
                (item["seal"]["rank"], item["seal"]["arm"]): item["seal"]["disposition"]
                for item in seal_set["seals"]
            }
            self.assertEqual(
                dispositions,
                {
                    (1, "A"): "evaluate",
                    (1, "T"): "noncompliant",
                    (2, "A"): "no_patch",
                    (2, "T"): "evaluate",
                },
            )
            skipped = next(
                item
                for item in seal_set["seals"]
                if item["seal"]["disposition"] == "noncompliant"
            )
            skip_result = runner.write_skip_receipt(plan, skipped)
            self.assertFalse(skip_result["official_evaluator_invocation_confirmed"])
            self.assertEqual(
                json.loads(Path(skip_result["receipt"]["path"]).read_bytes())[
                    "official_evaluator_invocations"
                ],
                0,
            )
            eligible = next(
                item
                for item in seal_set["seals"]
                if item["seal"]["disposition"] == "evaluate"
            )
            runner.claim_evaluation(plan, eligible)
            with self.assertRaises(runner.FailClosed):
                runner.claim_evaluation(plan, eligible)

    def test_popen_failure_is_authorized_but_records_zero_invocations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary), patch_keys={(1, "A"), (1, "T")})
            plan = fixture.validated_plan()
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            seal_info = next(
                item
                for item in seal_set["seals"]
                if item["seal"]["rank"] == 1 and item["seal"]["arm"] == "A"
            )
            episode = next(
                item
                for item in plan["episodes"]
                if item["rank"] == 1 and item["arm"] == "A"
            )
            def container_absence(
                _container_name: str, *, remove: bool
            ) -> dict[str, object]:
                if remove:
                    raise runner.FailClosed("synthetic cleanup failure")
                return {"absent": True}

            case_state = {"poisoned": None}
            with (
                mock.patch.object(
                    runner,
                    "ensure_container_absent",
                    side_effect=container_absence,
                ),
                mock.patch.object(
                    runner,
                    "inspect_pinned_image",
                    return_value={"id": episode["official_image_local_id"]},
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
                    seal_info,
                    threading.Lock(),
                    case_state,
                )
            receipt = json.loads(Path(result["receipt"]["path"]).read_bytes())
            self.assertTrue(receipt["official_evaluator_invocation_authorized"])
            self.assertFalse(receipt["official_evaluator_invocation_confirmed"])
            self.assertEqual(receipt["official_evaluator_invocations"], 0)
            self.assertIsInstance(case_state["poisoned"], dict)

            sibling_seal = next(
                item
                for item in seal_set["seals"]
                if item["seal"]["rank"] == 1 and item["seal"]["arm"] == "T"
            )
            sibling_episode = next(
                item
                for item in plan["episodes"]
                if item["rank"] == 1 and item["arm"] == "T"
            )
            sibling = runner.evaluate_one(
                plan,
                sibling_episode,
                sibling_seal,
                threading.Lock(),
                case_state,
            )
            sibling_receipt = json.loads(
                Path(sibling["receipt"]["path"]).read_bytes()
            )
            self.assertTrue(sibling_receipt["case_poisoned"])
            self.assertEqual(sibling_receipt["official_evaluator_invocations"], 0)

    def test_container_absence_requires_explicit_not_found(self) -> None:
        not_found = runner.subprocess.CompletedProcess(
            ["docker"], 1, b"", b"Error: No such container: synthetic"
        )
        with mock.patch.object(runner, "run_bytes", return_value=not_found):
            proof = runner.ensure_container_absent("synthetic", remove=False)
        self.assertTrue(proof["absent"])

        daemon_error = runner.subprocess.CompletedProcess(
            ["docker"], 1, b"", b"permission denied connecting to daemon"
        )
        with mock.patch.object(runner, "run_bytes", return_value=daemon_error):
            with self.assertRaises(runner.FailClosed):
                runner.ensure_container_absent("synthetic", remove=False)

    def test_official_output_projection_binds_patch_and_test_counts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            run_dir = Path(temporary).resolve()
            episode = {
                "case_id": "project__project-100",
                "arm": "A",
                "run_id": "issue825-rank1-A",
                "model_name_or_path": "claude-sonnet-5-A",
            }
            patch = b"diff --git a/a b/a\n+synthetic\n"
            log_root = (
                run_dir
                / "logs/run_evaluation"
                / episode["run_id"]
                / episode["model_name_or_path"]
                / episode["case_id"]
            )
            log_root.mkdir(parents=True)
            (log_root / "eval.sh").write_text("true\n")
            (log_root / "patch.diff").write_bytes(patch)
            (log_root / "run_instance.log").write_text("ok\n")
            (log_root / "test_output.txt").write_text("ok\n")
            write_json(
                log_root / "report.json",
                {
                    episode["case_id"]: {
                        "resolved": True,
                        "tests_status": {
                            "FAIL_TO_PASS": {"success": ["required"], "failure": []},
                            "PASS_TO_PASS": {"success": ["kept"], "failure": []},
                        },
                    }
                },
            )
            write_json(
                run_dir / f"{episode['model_name_or_path']}.{episode['run_id']}.json",
                {
                    "total_instances": 1,
                    "submitted_instances": 1,
                    "submitted_ids": [episode["case_id"]],
                },
            )
            refs, tests = runner.evaluator_outputs(run_dir, episode, patch)
            self.assertIn("report", refs)
            self.assertTrue(tests["resolved"])
            self.assertEqual(tests["counts"]["FAIL_TO_PASS"]["success"], 1)

    def test_offline_aggregation_selects_registered_token_efficiency(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(
                Path(temporary),
                patch_keys={(1, "A"), (1, "T"), (2, "A"), (2, "T")},
                arm_tokens={"A": 1000, "T": 800},
            )
            plan = fixture.validated_plan()
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            results = []
            for seal_info in seal_set["seals"]:
                seal = seal_info["seal"]
                episode = next(
                    item
                    for item in plan["episodes"]
                    if item["case_id"] == seal["case_id"]
                    and item["arm"] == seal["arm"]
                )
                run_dir = fixture.output / "evaluations" / seal["case_id"] / seal["arm"]
                run_dir.mkdir(parents=True)
                sealed_dir = seal_info["path"].parent
                patch = (sealed_dir / seal["terminal_patch"]["path"]).read_bytes()
                prediction = (sealed_dir / seal["prediction"]["path"]).read_bytes()
                prediction_path = run_dir / "predictions.jsonl"
                prediction_path.write_bytes(prediction)
                log_root = (
                    run_dir
                    / "logs/run_evaluation"
                    / seal["run_id"]
                    / seal["model_name_or_path"]
                    / seal["case_id"]
                )
                log_root.mkdir(parents=True)
                (log_root / "eval.sh").write_text("true\n")
                (log_root / "patch.diff").write_bytes(patch)
                (log_root / "run_instance.log").write_text("ok\n")
                (log_root / "test_output.txt").write_text("ok\n")
                write_json(
                    log_root / "report.json",
                    {
                        seal["case_id"]: {
                            "resolved": True,
                            "tests_status": {
                                "FAIL_TO_PASS": {
                                    "success": ["required"],
                                    "failure": [],
                                },
                                "PASS_TO_PASS": {
                                    "success": ["kept"],
                                    "failure": [],
                                },
                            },
                        }
                    },
                )
                write_json(
                    run_dir / f"{seal['model_name_or_path']}.{seal['run_id']}.json",
                    {
                        "total_instances": 1,
                        "submitted_instances": 1,
                        "submitted_ids": [seal["case_id"]],
                    },
                )
                outputs, tests = runner.evaluator_outputs(
                    run_dir, episode, patch
                )
                test_data = runner.canonical_json_bytes(tests)
                test_path = run_dir / "test-lists.json"
                test_path.write_bytes(test_data)
                claim = runner.claim_evaluation(plan, seal_info)
                command = runner.evaluator_command(plan, episode, prediction_path)
                preexisting = {"absent": True}
                container = {
                    "image": {
                        "id": episode["official_image_local_id"],
                        "repo_tags": [episode["official_image"]],
                    },
                    "peak_memory_bytes": 1,
                    "samples": [],
                }
                write_json(
                    run_dir / "evaluation.started.json",
                    {
                        "schema_version": "issue825-official-evaluation-start-v1",
                        "started_at": "2026-07-22T00:00:00Z",
                        "case_id": seal["case_id"],
                        "arm": seal["arm"],
                        "run_id": seal["run_id"],
                        "terminal_set_digest": seal["terminal_set_digest"],
                        "seal_sha256": seal_info["sha256"],
                        "irrevocable_registry_start": claim,
                        "command": command,
                        "preexisting_container_check": preexisting,
                        "official_evaluator_invocation_authorized": True,
                    },
                )
                (run_dir / "evaluator.stdout").write_bytes(b"")
                (run_dir / "evaluator.stderr").write_bytes(b"")
                write_json(run_dir / "container-monitor.json", container)
                receipt_path = run_dir / "evaluation.receipt.json"
                receipt = {
                    "schema_version": runner.EVALUATION_SCHEMA,
                    "case_id": seal["case_id"],
                    "rank": seal["rank"],
                    "arm": seal["arm"],
                    "run_id": seal["run_id"],
                    "terminal_set_digest": seal["terminal_set_digest"],
                    "seal": {
                        "path": str(seal_info["path"]),
                        "bytes": len(seal_info["bytes"]),
                        "sha256": seal_info["sha256"],
                    },
                    "irrevocable_registry_start": claim,
                    "prediction": runner.file_reference(prediction_path),
                    "command": command,
                    "official_evaluator_invocation_authorized": True,
                    "official_evaluator_invocation_confirmed": True,
                    "official_evaluator_invocations": 1,
                    "valid_official_outputs": True,
                    "returncode": 0,
                    "timed_out": False,
                    "preexisting_container_check": preexisting,
                    "container_cleanup": {"absent": True},
                    "container": container,
                    "pinned_image": {
                        "id": episode["official_image_local_id"],
                        "repo_tags": [episode["official_image"]],
                    },
                    "official_outputs": outputs,
                    "model_output_delivery": "none",
                    "test_lists": {
                        "path": "test-lists.json",
                        "bytes": len(test_data),
                        "sha256": runner.sha256_bytes(test_data),
                    },
                    "inventory": runner.recursive_inventory(
                        run_dir, {receipt_path}
                    ),
                }
                receipt_ref = write_json(receipt_path, receipt)
                results.append(
                    {
                        "case_id": seal["case_id"],
                        "rank": seal["rank"],
                        "arm": seal["arm"],
                        "disposition": "evaluate",
                        "receipt": receipt_ref,
                        "official_evaluator_invocation_authorized": True,
                        "official_evaluator_invocation_confirmed": True,
                        "valid": True,
                    }
                )
            results.sort(key=lambda value: (value["rank"], value["arm"]))
            batch = {
                "schema_version": runner.BATCH_SCHEMA,
                "plan_sha256": plan["_sha256"],
                "script_sha256": runner.sha256_file(runner.SCRIPT_PATH),
                "terminal_set_digest": seal_set["terminal_set_digest"],
                "seal_set": {
                    "path": str(seal_set["path"]),
                    "bytes": len(seal_set["bytes"]),
                    "sha256": seal_set["sha256"],
                },
                "official_evaluations_authorized": 4,
                "official_evaluations_started": 4,
                "official_evaluations_recorded": 4,
                "zero_invocation_receipts": 0,
                "max_parallel": 2,
                "same_case_serialized": True,
                "failures": [],
                "environment": {
                    "dataset_arrow_sha256": DATASET_SHA,
                    "official_evaluator_invocations": 0,
                    "model_session_isolation": {
                        "checked_session_count": 4,
                        "all_absent": True,
                    },
                },
                "results": results,
                "model_output_delivery": "none; synthetic out-of-band evidence",
            }
            batch_ref = write_json(fixture.output / "evaluation-batch.receipt.json", batch)
            result = select_result.aggregate(
                fixture.plan_path, Path(batch_ref["path"])
            )
            self.assertEqual(result["decision"], "selected_T")
            self.assertEqual(result["classification"], "material_efficiency_selection")
            self.assertEqual(result["official_evaluations_started"], 4)
            first_receipt = json.loads(
                Path(results[0]["receipt"]["path"]).read_bytes()
            )
            tampered_tests = (
                Path(results[0]["receipt"]["path"]).parent
                / first_receipt["test_lists"]["path"]
            )
            original_tests = tampered_tests.read_bytes()
            tampered_tests.write_bytes(b"{}\n")
            with self.assertRaises(runner.FailClosed):
                select_result.aggregate(fixture.plan_path, Path(batch_ref["path"]))
            tampered_tests.write_bytes(original_tests)

            first_receipt["valid_official_outputs"] = False
            first_receipt["returncode"] = 1
            first_receipt_path = Path(results[0]["receipt"]["path"])
            first_receipt_data = runner.canonical_json_bytes(first_receipt)
            first_receipt_path.write_bytes(first_receipt_data)
            results[0]["receipt"] = {
                "path": str(first_receipt_path),
                "bytes": len(first_receipt_data),
                "sha256": runner.sha256_bytes(first_receipt_data),
            }
            results[0]["valid"] = False
            batch["results"] = results
            Path(batch_ref["path"]).write_bytes(runner.canonical_json_bytes(batch))
            failed_result = select_result.aggregate(
                fixture.plan_path, Path(batch_ref["path"])
            )
            self.assertEqual(failed_result["classification"], "inconclusive")

    def test_registered_decision_rejects_fewer_treatment_resolutions(self) -> None:
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
                        "resolved": arm == "A",
                        "pass_to_pass_regressions": 0,
                        "provider_total_tokens": 100,
                        "combined_pre_evaluator_wall_seconds": 100.0,
                    }
                )
        decision = select_result.decide_registered(metrics)
        self.assertEqual(decision["decision"], "no_RNA_treatment")
        self.assertEqual(decision["classification"], "efficacy_or_regression_rejection")

    def test_no_patch_aggregate_rejects_substituted_skip_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary), patch_keys=set())
            plan = fixture.validated_plan()
            runner.seal_all(plan)
            seal_set = runner.validate_seal_set(plan)
            results = [
                runner.write_skip_receipt(plan, seal_info)
                for seal_info in seal_set["seals"]
            ]
            results.sort(key=lambda value: (value["rank"], value["arm"]))
            batch = {
                "schema_version": runner.BATCH_SCHEMA,
                "plan_sha256": plan["_sha256"],
                "script_sha256": runner.sha256_file(runner.SCRIPT_PATH),
                "terminal_set_digest": seal_set["terminal_set_digest"],
                "seal_set": {
                    "path": str(seal_set["path"]),
                    "bytes": len(seal_set["bytes"]),
                    "sha256": seal_set["sha256"],
                },
                "official_evaluations_authorized": 0,
                "official_evaluations_started": 0,
                "official_evaluations_recorded": 0,
                "zero_invocation_receipts": 4,
                "max_parallel": 2,
                "same_case_serialized": True,
                "failures": [],
                "environment": {
                    "dataset_arrow_sha256": DATASET_SHA,
                    "official_evaluator_invocations": 0,
                    "model_session_isolation": {
                        "checked_session_count": 4,
                        "all_absent": True,
                    },
                },
                "results": results,
                "model_output_delivery": "none; synthetic",
            }
            batch_path = fixture.output / "evaluation-batch.receipt.json"
            batch_path.write_bytes(runner.canonical_json_bytes(batch))
            aggregate = select_result.aggregate(fixture.plan_path, batch_path)
            self.assertEqual(aggregate["decision"], "selected_T")

            source = Path(results[0]["receipt"]["path"])
            substitute = fixture.output / "substituted-skip.json"
            results[0]["receipt"] = write_bytes(substitute, source.read_bytes())
            batch_path.write_bytes(runner.canonical_json_bytes(batch))
            with self.assertRaises(runner.FailClosed):
                select_result.aggregate(fixture.plan_path, batch_path)

    def test_zero_efficiency_baseline_is_not_a_material_reduction(self) -> None:
        metrics = [
            {
                "rank": rank,
                "arm": arm,
                "evidence_complete": True,
                "policy_compliant": True,
                "outcome_valid": True,
                "resolved": False,
                "pass_to_pass_regressions": 0,
                "provider_total_tokens": 0,
                "combined_pre_evaluator_wall_seconds": 0.0,
            }
            for rank in (1, 2)
            for arm in ("A", "T")
        ]
        decision = select_result.decide_registered(metrics)
        self.assertEqual(decision["classification"], "no_registered_advantage")

    def test_missing_token_evidence_is_inconclusive_not_efficiency(self) -> None:
        metrics = [
            {
                "rank": rank,
                "arm": arm,
                "evidence_complete": arm == "A",
                "policy_compliant": True,
                "outcome_valid": True,
                "resolved": False,
                "pass_to_pass_regressions": 0,
                "provider_total_tokens": 100 if arm == "A" else None,
                "combined_pre_evaluator_wall_seconds": 100.0,
            }
            for rank in (1, 2)
            for arm in ("A", "T")
        ]
        decision = select_result.decide_registered(metrics)
        self.assertEqual(decision["classification"], "inconclusive")


if __name__ == "__main__":
    unittest.main()
