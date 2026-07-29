from __future__ import annotations

import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import evaluator_authorization
import evaluator_runner
import provider_usage
import registration_contract
import run_selector
import select_cases
import select_result
import verify_selector


class RegistrationContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.path = HERE / "registration.template.json"
        cls.raw = cls.path.read_bytes()
        cls.registration = json.loads(cls.raw)

    def resolved_registration(self) -> dict:
        value = json.loads(json.dumps(self.registration))
        for key, filename in registration_contract.REGISTERED_FILE_NAMES.items():
            value["registered_files"][key] = registration_contract.sha256_file(
                HERE / filename
            )
        value["rna_artifact"]["producer_commit"] = "b" * 40
        for key in registration_contract.RNA_ARTIFACT_FIELDS - {
            "producer_commit",
            "local_source_build_allowed",
        }:
            value["rna_artifact"][key] = (
                registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION[
                    "incompatible_rna_binary_sha256"
                ]
                if key == "binary_sha256"
                else "0" * 64
            )
        value["qualification_closure"]["manifest_sha256"] = "0" * 64
        value["qualification_closure"]["archive_sha256"] = "0" * 64
        return value

    def test_issue836_twenty_case_forty_episode_design_is_frozen(self) -> None:
        value = self.registration
        self.assertEqual(value["schema_version"], run_selector.REGISTRATION_SCHEMA)
        self.assertEqual(
            value["schema_version"],
            registration_contract.CURRENT_REGISTRATION_SCHEMA,
        )
        self.assertEqual(value["issue"], 836)
        self.assertTrue(value["published_before_selection"])
        self.assertTrue(value["fresh_case_claim"])
        self.assertEqual(value["prior_model_calls"], 0)
        self.assertEqual(value["prior_official_evaluator_invocations"], 0)
        self.assertEqual(value["episode_design"]["case_count"], 20)
        self.assertEqual(value["episode_design"]["episode_count"], 40)
        self.assertTrue(value["episode_design"]["fresh_session_per_episode"])
        self.assertFalse(value["episode_design"]["resume_allowed"])
        self.assertFalse(value["episode_design"]["model_retry_allowed"])
        self.assertEqual(
            value["selector"]["counterbalance"],
            {
                "odd_rank_arm_order": ["A", "T"],
                "even_rank_arm_order": ["T", "A"],
                "a_first_case_count": 10,
                "t_first_case_count": 10,
            },
        )
        self.assertEqual(
            value["selector"]["prefix_lineage"],
            registration_contract.FROZEN_V4_PREFIX_LINEAGE,
        )
        self.assertEqual(
            value["selector"]["pre_model_v3_supersession"],
            registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION,
        )

    def test_selector_population_and_exclusions_are_exact(self) -> None:
        selector = self.registration["selector"]
        exclusions_path = HERE / "exclusions.json"
        exclusions = json.loads(exclusions_path.read_bytes())
        self.assertEqual(selector["algorithm_version"], "issue836-selector-v4")
        self.assertEqual(selector["seed"], select_cases.EXPECTED_SEED)
        self.assertEqual(
            (selector["population_rows"], selector["excluded_rows"], selector["eligible_rows"]),
            (500, 81, 419),
        )
        self.assertGreaterEqual(selector["eligible_rows"], 20)
        self.assertEqual(
            selector["excluded_ids_sha256"],
            hashlib.sha256(
                select_cases.canonical(exclusions["excluded_instance_ids"])
            ).hexdigest(),
        )
        self.assertEqual(
            selector["exclusions_file_sha256"],
            hashlib.sha256(exclusions_path.read_bytes()).hexdigest(),
        )

    def test_current_and_historical_experiment_dimensions_are_authoritative(
        self,
    ) -> None:
        self.assertEqual(
            registration_contract.experiment_dimensions(self.registration),
            {
                "case_count": 20,
                "episode_count": 40,
                "max_parallel_cases": 2,
                "per_episode_budget_usd": 6.0,
                "maximum_budget_usd": 240.0,
            },
        )
        historical = json.loads(
            (HERE.parent / "issue830" / "registration.json").read_bytes()
        )
        registration_contract.validate_registration(historical)
        self.assertEqual(
            registration_contract.experiment_dimensions(historical),
            {
                "case_count": 2,
                "episode_count": 4,
                "max_parallel_cases": 2,
                "per_episode_budget_usd": 6.0,
                "maximum_budget_usd": 24.0,
            },
        )
        issue836_v2 = json.loads(
            (HERE.parent / "issue836" / "registration.json").read_bytes()
        )
        registration_contract.validate_registration(issue836_v2)
        self.assertEqual(
            registration_contract.experiment_dimensions(issue836_v2),
            {
                "case_count": 20,
                "episode_count": 40,
                "max_parallel_cases": 2,
                "per_episode_budget_usd": 6.0,
                "maximum_budget_usd": 240.0,
            },
        )
        self.assertEqual(
            select_result.registered_selection_rule(issue836_v2)[
                "schema_version"
            ],
            "issue836-selection-rule-v2",
        )
        self.assertEqual(
            select_result.result_schema(issue836_v2),
            select_result.ISSUE836_V2_RESULT_SCHEMA,
        )
        issue836_v3 = json.loads(
            (HERE.parent / "issue836-v3" / "registration.json").read_bytes()
        )
        registration_contract.validate_registration(issue836_v3)
        self.assertEqual(
            registration_contract.experiment_dimensions(issue836_v3),
            {
                "case_count": 20,
                "episode_count": 40,
                "max_parallel_cases": 2,
                "per_episode_budget_usd": 6.0,
                "maximum_budget_usd": 240.0,
            },
        )
        self.assertEqual(
            select_result.registered_selection_rule(issue836_v3)[
                "schema_version"
            ],
            "issue836-selection-rule-v3",
        )
        self.assertEqual(
            select_result.result_schema(issue836_v3),
            select_result.ISSUE836_V3_RESULT_SCHEMA,
        )

    def test_runtime_and_selection_rule_are_exact(self) -> None:
        runtime = self.registration["model_runtime"]
        self.assertEqual(runtime["cli_version"], "2.1.216")
        self.assertEqual(runtime["model"], "claude-sonnet-5")
        self.assertEqual(runtime["effort"], "high")
        self.assertEqual(runtime["wall_seconds"], 1200)
        self.assertEqual(runtime["budget_usd"], 6.0)
        self.assertEqual(runtime, registration_contract.FROZEN_MODEL_RUNTIME)
        self.assertEqual(runtime["invocations_per_episode"], 1)
        self.assertFalse(runtime["resume_allowed"])
        self.assertFalse(runtime["model_retry_allowed"])
        self.assertEqual(runtime["permission_mode"], "dontAsk")
        expected_rule = json.loads(
            json.dumps(select_result.REGISTERED_SELECTION_RULE)
        )
        expected_rule.update(
            {
                "schema_version": "issue836-selection-rule-v4",
                "episode_count": 40,
                "pair_count": 20,
            }
        )
        self.assertEqual(self.registration["selection_rule"], expected_rule)

    def test_isolation_identity_usage_and_evaluator_contracts_are_bound(self) -> None:
        isolation = self.registration["isolation"]
        self.assertTrue(isolation["same_boundary_both_arms"])
        self.assertTrue(isolation["bash_requires_single_use_gateway_request"])
        self.assertTrue(isolation["provider_parent_seatbelt_required"])
        runtime_registration = json.loads(
            json.dumps(self.registration["isolation_runtime"])
        )
        for key in (
            "gateway_python_sha256",
            "docker_binary_sha256",
            "sandbox_exec_sha256",
            "worker_entrypoint_sha256",
            "strace_artifact_sha256",
        ):
            runtime_registration[key] = "0" * 64
        registration = dict(self.registration)
        registration["isolation_runtime"] = runtime_registration
        fixed = run_selector._fixed_isolation_registration(registration)
        self.assertEqual(
            fixed["git_binary_sha256"],
            run_selector.TRUSTED_GIT_BINARY_SHA256,
        )
        self.assertIn("/opt", fixed["trace_allowed_path_prefixes"])
        self.assertIn("/var/run", fixed["trace_allowed_path_prefixes"])
        self.assertIn(
            "pip3 download", fixed["trace_forbidden_static_fragments"]
        )
        identity = self.registration["identity"]
        self.assertTrue(identity["verify_before_and_after_each_rna_entry_point"])
        self.assertTrue(identity["fatal_on_tree_cache_binary_or_repository_drift"])
        usage = self.registration["usage"]
        self.assertEqual(usage["schema_version"], provider_usage.SCHEMA_VERSION)
        self.assertTrue(usage["positive_provider_total_required_after_model_invocation"])
        self.assertTrue(
            usage[
                "positive_agent_transcript_provider_response_count_required"
            ]
        )
        self.assertEqual(
            usage["provider_responses_scope"],
            "agent_transcript_only",
        )
        self.assertTrue(usage["auxiliary_cli_usage_retained"])
        self.assertTrue(
            usage["auxiliary_cli_usage_included_in_whole_invocation_totals"]
        )
        evaluator = self.registration["evaluator"]
        self.assertEqual(evaluator["plan_schema"], evaluator_runner.PLAN_SCHEMA)
        self.assertEqual(
            evaluator["authorization_schema"],
            evaluator_authorization.SCHEMA_VERSION,
        )
        self.assertEqual(evaluator["authorization_decision"], "authorize_once")
        self.assertFalse(evaluator["runner_self_authorization_allowed"])
        self.assertTrue(evaluator["one_use_registry_claim_required"])

    def test_hash_slots_cover_all_runtime_and_evaluator_inputs(self) -> None:
        slots = self.registration["registered_files"]
        self.assertEqual(set(run_selector.REGISTERED_FILE_NAMES), set(slots))
        self.assertEqual(
            run_selector.REGISTERED_FILE_NAMES,
            registration_contract.REGISTERED_FILE_NAMES,
        )
        self.assertTrue(
            {
                "selector_sha256",
                "evaluator_runner_sha256",
                "evaluator_plan_template_sha256",
                "result_selector_sha256",
                "registration_contract_sha256",
                "offline_worker_source_sha256",
                "worker_dockerfile_sha256",
                "landlock_launcher_source_sha256",
            }.issubset(slots)
        )
        self.assertNotIn(b"issue825", self.raw)
        self.assertNotIn(b"runtime_amendment", self.raw)

    def test_shared_contract_rejects_budget_retry_and_usage_scope_drift(self) -> None:
        registration_contract.validate_registration(
            self.registration,
            require_resolved_hashes=False,
        )
        mutations = (
            ("budget", ("model_runtime", "budget_usd"), 5.99),
            ("wall", ("model_runtime", "wall_seconds"), 600),
            ("retry", ("model_runtime", "model_retry_allowed"), True),
            (
                "scope",
                ("usage", "provider_responses_scope"),
                "whole_invocation",
            ),
        )
        for label, (section, key), value in mutations:
            with self.subTest(label=label):
                changed = json.loads(json.dumps(self.registration))
                changed[section][key] = value
                with self.assertRaises(
                    registration_contract.RegistrationContractError
                ):
                    registration_contract.validate_registration(
                        changed,
                        require_resolved_hashes=False,
                    )

    def test_v4_supersession_calls_and_old_binary_identity_fail_closed(
        self,
    ) -> None:
        registration = self.resolved_registration()
        registration_contract.validate_registration(registration)
        for label, mutate in (
            (
                "rank",
                lambda value: value["selector"][
                    "pre_model_v3_supersession"
                ].update({"superseded_rank": 11}),
            ),
            (
                "replacement",
                lambda value: value["selector"][
                    "pre_model_v3_supersession"
                ].update(
                    {"replacement_instance_id": "psf__requests-9999"}
                ),
            ),
            (
                "source-rank",
                lambda value: value["selector"][
                    "pre_model_v3_supersession"
                ].update({"replacement_source_rank": 21}),
            ),
            (
                "tree",
                lambda value: value["selector"][
                    "pre_model_v3_supersession"
                ].update({"replacement_base_tree": "f" * 40}),
            ),
            (
                "problem",
                lambda value: value["selector"][
                    "pre_model_v3_supersession"
                ].update({"replacement_problem_statement_sha256": "f" * 64}),
            ),
            (
                "ranking",
                lambda value: value["selector"][
                    "pre_model_v3_supersession"
                ].update({"replacement_ranking_sha256": "f" * 64}),
            ),
            (
                "model-call",
                lambda value: value.update({"prior_model_calls": 1}),
            ),
            (
                "evaluator-call",
                lambda value: value.update(
                    {"prior_official_evaluator_invocations": 1}
                ),
            ),
            (
                "binary",
                lambda value: value["rna_artifact"].update(
                    {"binary_sha256": "f" * 64}
                ),
            ),
        ):
            with self.subTest(label=label):
                changed = json.loads(json.dumps(registration))
                mutate(changed)
                with self.assertRaises(
                    registration_contract.RegistrationContractError
                ):
                    registration_contract.validate_registration(changed)

    def test_resolved_registration_binds_every_live_source_byte(self) -> None:
        registration_contract.validate_registration(
            self.resolved_registration(),
            source_root=HERE,
        )

    def test_qualification_closure_binds_archive_and_frozen_contracts(self) -> None:
        registration = self.resolved_registration()
        archive_sha = registration_contract.sha256_bytes(b"closure archive")
        manifest = {
            "schema_version": (
                registration_contract.QUALIFICATION_MANIFEST_SCHEMA
            ),
            "qualified": True,
            "no_model_or_provider_calls": True,
            "archive_sha256": archive_sha,
            "registered_files_sha256": registration_contract.sha256_bytes(
                registration_contract.canonical(
                    registration["registered_files"]
                )
            ),
            "model_runtime_sha256": registration_contract.sha256_bytes(
                registration_contract.canonical(
                    registration["model_runtime"]
                )
            ),
            "isolation_runtime_sha256": registration_contract.sha256_bytes(
                registration_contract.canonical(
                    registration["isolation_runtime"]
                )
            ),
            "rna_artifact_sha256": registration_contract.sha256_bytes(
                registration_contract.canonical(
                    registration["rna_artifact"]
                )
            ),
            "external_inputs_sha256": "c" * 64,
            "runtime_identity_sha256": "d" * 64,
            "evidence_inventory_sha256": "a" * 64,
        }
        manifest_bytes = registration_contract.canonical(manifest)
        registration["qualification_closure"].update(
            {
                "manifest_sha256": registration_contract.sha256_bytes(
                    manifest_bytes
                ),
                "archive_sha256": archive_sha,
            }
        )
        registration_contract.validate_qualification_manifest(
            registration,
            manifest_bytes,
            archive_sha,
        )
        changed = json.loads(manifest_bytes)
        changed["model_runtime_sha256"] = "f" * 64
        with self.assertRaises(
            registration_contract.RegistrationContractError
        ):
            registration_contract.validate_qualification_manifest(
                registration,
                registration_contract.canonical(changed),
                archive_sha,
            )

    def test_rna_ci_and_runtime_trust_chain_is_cross_checked(self) -> None:
        registration = self.resolved_registration()
        artifact = registration["rna_artifact"]
        artifact["producer_commit"] = "c" * 40
        artifact["binary_sha256"] = "1" * 64
        artifact["archive_sha256"] = "2" * 64
        bundle = registration_contract.canonical(
            {
                "provenance": {"head_sha": artifact["producer_commit"]},
                "components": {
                    "executable": {"sha256": artifact["binary_sha256"]}
                },
                "artifact": {"archive_sha256": artifact["archive_sha256"]},
            }
        )
        artifact["bundle_manifest_sha256"] = (
            registration_contract.sha256_bytes(bundle)
        )
        attestation = registration_contract.canonical(
            {
                "schema": "rna-swebench-semantic-bundle-upload-v1",
                "head_sha": artifact["producer_commit"],
                "manifest_sha256": artifact["bundle_manifest_sha256"],
            }
        )
        artifact["upload_attestation_sha256"] = (
            registration_contract.sha256_bytes(attestation)
        )
        verification = registration_contract.canonical(
            {
                "schema": "rna-swebench-semantic-bundle-verification-v1",
                "head_sha": artifact["producer_commit"],
                "manifest_sha256": artifact["bundle_manifest_sha256"],
                "archive_sha256": artifact["archive_sha256"],
                "upload_attestation_sha256": artifact[
                    "upload_attestation_sha256"
                ],
            }
        )
        artifact["verification_receipt_sha256"] = (
            registration_contract.sha256_bytes(verification)
        )
        environment = registration_contract.canonical(
            {"PATH": "/usr/bin:/bin"}
        )
        artifact["canonical_environment_sha256"] = (
            registration_contract.sha256_bytes(environment)
        )
        runtime = registration_contract.canonical(
            {
                "binary_sha256": artifact["binary_sha256"],
                "environment_sha256": artifact[
                    "canonical_environment_sha256"
                ],
            }
        )
        artifact["runtime_receipt_sha256"] = (
            registration_contract.sha256_bytes(runtime)
        )
        documents = {
            "bundle_manifest": bundle,
            "upload_attestation": attestation,
            "verification_receipt": verification,
            "canonical_environment": environment,
            "runtime_receipt": runtime,
        }
        registration_contract.validate_rna_trust_documents(
            registration,
            documents,
        )
        tampered = {**documents, "runtime_receipt": b"{}\n"}
        with self.assertRaises(
            registration_contract.RegistrationContractError
        ):
            registration_contract.validate_rna_trust_documents(
                registration,
                tampered,
            )

    def test_rna_trust_json_is_materialized_but_large_artifacts_are_not(self) -> None:
        registration = json.loads(json.dumps(self.registration))
        names = {
            "launcher": "launcher_sha256",
            "binary": "binary_sha256",
            "bundle_manifest": "bundle_manifest_sha256",
            "archive": "archive_sha256",
            "upload_attestation": "upload_attestation_sha256",
            "verification_receipt": "verification_receipt_sha256",
            "canonical_environment": "canonical_environment_sha256",
            "runtime_receipt": "runtime_receipt_sha256",
        }
        manifest = {"rna_artifact": {}}
        for index, (name, key) in enumerate(names.items(), start=1):
            digest = f"{index:064x}"
            registration["rna_artifact"][key] = digest
            manifest["rna_artifact"][name] = {
                "path": f"/synthetic/{name}",
                "bytes": 2,
                "sha256": digest,
            }

        calls: list[tuple[str, bool]] = []

        def fake_check_ref(value, where, *, materialize=True):
            name = where.rsplit(".", 1)[-1]
            calls.append((name, materialize))
            return Path(value["path"]), b"{}" if materialize else b""

        with mock.patch.object(
            run_selector,
            "check_ref",
            side_effect=fake_check_ref,
        ), mock.patch.object(
            registration_contract,
            "validate_rna_trust_documents",
        ) as validate:
            run_selector.verify_rna_artifact(manifest, registration)

        materialized = {name for name, value in calls if value}
        self.assertEqual(
            materialized,
            {
                "bundle_manifest",
                "upload_attestation",
                "verification_receipt",
                "canonical_environment",
                "runtime_receipt",
            },
        )
        self.assertEqual(
            {name for name, value in calls if not value},
            {"launcher", "binary", "archive"},
        )
        documents = validate.call_args.args[1]
        self.assertTrue(all(value == b"{}" for value in documents.values()))

    def test_verifier_requires_exact_registered_claude_argv(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            receipt_path = root / "episode-receipt.json"
            receipt_path.write_bytes(b"{}\n")
            settings = root / "claude-settings.json"
            settings.write_bytes(b"{}\n")
            mcp = root / "empty-mcp.json"
            mcp.write_bytes(run_selector.EMPTY_MCP_BYTES)
            claude = root / "claude"
            claude.write_bytes(b"synthetic")
            seatbelt = root / "outer.sb"
            seatbelt.write_bytes(b"(version 1)\n")
            runtime = registration_contract.FROZEN_MODEL_RUNTIME
            session = "00000000-0000-4000-8000-000000000000"
            supervisor = {
                "sandbox_exec": "/usr/bin/sandbox-exec",
                "seatbelt_profile": str(seatbelt),
                "claude_settings_sha256": "1" * 64,
            }
            manifest = {
                "claude": {
                    "path": str(claude),
                    "sha256": runtime["cli_sha256"],
                },
                "mcp_config": run_selector.file_ref(mcp),
            }
            receipt = {"session_id": session}
            command = [
                supervisor["sandbox_exec"],
                "-f",
                str(seatbelt),
                str(claude),
                "-p",
                "--strict-mcp-config",
                "--mcp-config",
                str(mcp),
                "--model",
                runtime["model"],
                "--effort",
                runtime["effort"],
                "--permission-mode",
                runtime["permission_mode"],
                "--tools",
                ",".join(runtime["tools"]),
                "--disallowed-tools",
                ",".join(runtime["disallowed_tools"]),
                "--max-budget-usd",
                "6.0",
                "--output-format",
                "json",
                "--session-id",
                session,
                "--settings",
                str(settings),
            ]

            def fake_sha(path: Path) -> str:
                if path == claude:
                    return runtime["cli_sha256"]
                if path == settings:
                    return "1" * 64
                return run_selector.sha_bytes(path.read_bytes())

            with mock.patch.object(run_selector, "sha_file", side_effect=fake_sha):
                errors: list[str] = []
                verify_selector.validate_registered_claude_command(
                    command,
                    receipt_path=receipt_path,
                    receipt=receipt,
                    manifest=manifest,
                    registration=self.registration,
                    supervisor_config=supervisor,
                    treatment_system=None,
                    errors=errors,
                )
                self.assertEqual(errors, [])
                for flag, replacement in (
                    ("--max-budget-usd", "5.99"),
                    ("--model", "another-model"),
                    ("--tools", "Read"),
                ):
                    with self.subTest(flag=flag):
                        changed = list(command)
                        changed[changed.index(flag) + 1] = replacement
                        errors = []
                        verify_selector.validate_registered_claude_command(
                            changed,
                            receipt_path=receipt_path,
                            receipt=receipt,
                            manifest=manifest,
                            registration=self.registration,
                            supervisor_config=supervisor,
                            treatment_system=None,
                            errors=errors,
                        )
                        self.assertIn(
                            "claude_command_not_exactly_registered",
                            errors,
                        )

    def test_verifier_rejects_wall_limit_and_timeout_drift(self) -> None:
        errors: list[str] = []
        verify_selector.validate_timing_ledger(
            {
                "rna_preprocessing_seconds": 0.0,
                "model_wall_seconds": 1200.001,
                "combined_pre_evaluator_wall_seconds": 1200.001,
            },
            {"timed_out": False, "errors": []},
            "A",
            errors,
        )
        self.assertIn("model_wall_exceeds_registered_limit", errors)

        errors = []
        verify_selector.validate_timing_ledger(
            {
                "rna_preprocessing_seconds": 0.0,
                "model_wall_seconds": 1199.0,
                "combined_pre_evaluator_wall_seconds": 1199.0,
            },
            {"timed_out": True, "errors": ["model_wall_timeout"]},
            "A",
            errors,
        )
        self.assertIn("model_timeout_timing_inconsistent", errors)


if __name__ == "__main__":
    unittest.main()
