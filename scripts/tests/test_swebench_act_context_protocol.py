#!/usr/bin/env python3

from __future__ import annotations

import ast
import contextlib
import copy
import importlib.util
import io
import json
import shutil
import sys
import tempfile
import unittest
import unittest.mock as mock
from pathlib import Path


sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "validate_swebench_act_context_protocol.py"
SPEC = importlib.util.spec_from_file_location(
    "validate_swebench_act_context_protocol", SCRIPT
)
assert SPEC and SPEC.loader
VALIDATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VALIDATOR
SPEC.loader.exec_module(VALIDATOR)

PARSER_PATH = (
    ROOT / "benchmark" / "swebench-act-context" / "upstream" / "edit_patch_v2.py"
)
PARSER_SPEC = importlib.util.spec_from_file_location(
    "frozen_edit_patch_v2", PARSER_PATH
)
assert PARSER_SPEC and PARSER_SPEC.loader
PARSER = importlib.util.module_from_spec(PARSER_SPEC)
sys.modules[PARSER_SPEC.name] = PARSER
PARSER_SPEC.loader.exec_module(PARSER)


class SwebenchActContextProtocolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.protocol = VALIDATOR.load_json(ROOT / VALIDATOR.PROTOCOL_REL)
        cls.population = VALIDATOR.load_json(ROOT / VALIDATOR.POPULATION_REL)
        cls.runtime = VALIDATOR.load_json(ROOT / VALIDATOR.RUNTIME_REL)
        cls.vector = VALIDATOR.load_json(ROOT / VALIDATOR.VECTOR_REL)

    @staticmethod
    def copy_locked_bundle(destination_root: Path) -> None:
        lock = VALIDATOR.load_json(ROOT / VALIDATOR.LOCK_REL)
        for entry in lock["files"]:
            rel = Path(entry["path"])
            destination = destination_root / rel
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / rel, destination)
        for rel in (VALIDATOR.LOCK_REL, VALIDATOR.DIGEST_REL):
            destination = destination_root / rel
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / rel, destination)

    @staticmethod
    def qualified_artifact_receipt(bundle_digest: str) -> dict:
        return {
            "schema_version": 1,
            "receipt_type": "rna-qualified-artifact-v1",
            "protocol_id": "rna-act-context-swebench-v1",
            "protocol_bundle_sha256": bundle_digest,
            "artifact_commit_sha": "7" * 40,
            "artifact_sha256": "8" * 64,
            "ci_repository": "open-horizon-labs/repo-native-alignment",
            "ci_workflow": ".github/workflows/release.yml",
            "ci_run_id": 12345,
            "ci_run_url": "https://github.com/open-horizon-labs/repo-native-alignment/actions/runs/12345",
            "ci_artifact_name": "rna-darwin-arm64-m4",
            "platform": "darwin-arm64-m4",
            "capability_evidence": {
                "release_build": {"status": "passed", "evidence_sha256": "1" * 64},
                "metal": {
                    "required": True,
                    "observed": True,
                    "fallback": False,
                    "evidence_sha256": "2" * 64,
                },
                "embeddings": {
                    "enabled": True,
                    "complete": True,
                    "fallback": False,
                    "evidence_sha256": "3" * 64,
                },
                "reranking": {
                    "enabled": True,
                    "complete": True,
                    "fallback": False,
                    "evidence_sha256": "4" * 64,
                },
                "lsp": {
                    "status": "complete",
                    "quiescent": True,
                    "languages": ["Python"],
                    "skipped_files": 0,
                    "partial_jobs": 0,
                    "degraded_jobs": 0,
                    "cancelled_jobs": 0,
                    "crashed_jobs": 0,
                    "timed_out_jobs": 0,
                    "included_file_count": 123,
                    "covered_file_count": 123,
                    "coverage_scope": "every included ordinary docs/source/test/config file",
                    "coverage_manifest_sha256": "5" * 64,
                    "evidence_sha256": "6" * 64,
                },
            },
            "qualification_issue": 786,
            "qualification_comment_url": "https://github.com/open-horizon-labs/repo-native-alignment/issues/786#issuecomment-12345",
            "qualified_at_utc": "2026-07-17T00:00:00Z",
        }

    @staticmethod
    def approved_budget_receipt(bundle_digest: str) -> dict:
        return {
            "schema_version": 1,
            "receipt_type": "approved-model-budget-v1",
            "protocol_id": "rna-act-context-swebench-v1",
            "protocol_bundle_sha256": bundle_digest,
            "authorization_scope": "n70_cohort",
            "authorization_issue": 790,
            "qualification_instance_id": None,
            "population_n": 70,
            "maximum_model_requests": 420,
            "maximum_total_usd": "100.00",
            "approval_comment_url": "https://github.com/open-horizon-labs/repo-native-alignment/issues/790#issuecomment-12345",
            "approval_evidence_sha256": "9" * 64,
            "approved_at_utc": "2026-07-17T00:00:00Z",
        }

    def authorized_runtime(self, bundle_digest: str) -> dict:
        runtime = copy.deepcopy(self.runtime)
        runtime.update(
            {
                "paid_calls_authorized": True,
                "qualified_artifact_receipt": self.qualified_artifact_receipt(
                    bundle_digest
                ),
                "approved_budget_receipt": self.approved_budget_receipt(bundle_digest),
            }
        )
        return runtime

    def authorized_runtime_errors(self, runtime: dict, bundle_digest: str) -> list[str]:
        errors: list[str] = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=bundle_digest,
            expected_artifact_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["qualified_artifact_receipt"])
            ),
            expected_budget_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
            ),
        )
        return errors

    def assert_authorized_runtime_rejected(
        self,
        runtime: dict,
        bundle_digest: str,
        anchors: dict,
        expected_error: str,
        rejected_value: object,
    ) -> None:
        errors: list[str] = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=bundle_digest,
            **anchors,
        )
        marker = json.dumps(rejected_value).lower()
        self.assertIn(expected_error, errors)
        self.assertNotIn(marker, "\n".join(errors).lower())

        with tempfile.TemporaryDirectory() as temporary:
            runtime_path = Path(temporary) / "rejected-runtime.json"
            runtime_path.write_text(json.dumps(runtime), encoding="utf-8")
            with self.assertRaises(ValueError) as context:
                VALIDATOR.validate_bundle(
                    ROOT,
                    expected_digest=bundle_digest,
                    runtime_config=runtime_path,
                    **anchors,
                )
        exception_text = str(context.exception).lower()
        self.assertIn(expected_error.lower(), exception_text)
        self.assertNotIn(marker, exception_text)

    def test_frozen_bundle_is_compatible(self) -> None:
        result = VALIDATOR.validate_bundle(ROOT)
        self.assertTrue(result["compatible"])
        self.assertTrue(result["a_binary_outcomes_comparable"])
        self.assertTrue(result["a_initial_request_tokens_comparable"])
        self.assertFalse(result["a_retry_inclusive_tokens_comparable"])
        self.assertEqual(
            result["a_retry_inclusive_tokens_reason"], "upstream_not_measured"
        )
        self.assertFalse(result["network_accessed"])
        self.assertFalse(result["model_accessed"])

    def test_validator_has_no_network_or_process_imports(self) -> None:
        tree = ast.parse(SCRIPT.read_text(encoding="utf-8"))
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".", 1)[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".", 1)[0])
        self.assertFalse(
            imported
            & {
                "anthropic",
                "http",
                "httpx",
                "requests",
                "socket",
                "subprocess",
                "urllib",
            }
        )

    def test_missing_a_initial_count_blocks_efficiency_comparison(self) -> None:
        population = copy.deepcopy(self.population)
        first = next(row for row in population["instances"] if row["included"])
        first["upstream_a"]["anthropic_initial_request_input_tokens"] = None
        errors: list[str] = []
        VALIDATOR.validate_population(population, errors)
        self.assertTrue(
            any("missing A initial-request token count" in error for error in errors)
        )

    def test_inferred_a_retry_inclusive_total_is_rejected(self) -> None:
        population = copy.deepcopy(self.population)
        first = next(row for row in population["instances"] if row["included"])
        first["upstream_a"]["retry_inclusive_input_tokens"] = 12345
        errors: list[str] = []
        VALIDATOR.validate_population(population, errors)
        self.assertTrue(
            any(
                "inferred A retry-inclusive tokens are forbidden" in error
                for error in errors
            )
        )

    def test_h1_denominator_and_noninferiority_are_frozen(self) -> None:
        protocol = copy.deepcopy(self.protocol)
        protocol["hypotheses"]["H1"]["claim"] = "B is cheaper on resolved rows"
        errors: list[str] = []
        VALIDATOR.validate_protocol(protocol, errors)
        self.assertTrue(
            any("H1 efficiency threshold drift" in error for error in errors)
        )
        self.assertTrue(any("H1 denominator drift" in error for error in errors))

    def test_schedule_and_arm_order_are_rederived(self) -> None:
        population = copy.deepcopy(self.population)
        population["run_schedule"]["episodes"][0]["arm_order"].reverse()
        errors: list[str] = []
        VALIDATOR.validate_population(population, errors)
        self.assertTrue(any("arm-order drift" in error for error in errors))

    def test_runtime_model_or_mcp_drift_is_rejected(self) -> None:
        runtime = copy.deepcopy(self.runtime)
        runtime["requested_model"] = "different-model"
        runtime["rna_access"] = "mcp"
        errors: list[str] = []
        VALIDATOR.validate_runtime(runtime, errors)
        self.assertIn("runtime requested_model drift", errors)
        self.assertIn("runtime rna_access drift", errors)

    def test_authorized_runtime_requires_both_receipts(self) -> None:
        runtime = copy.deepcopy(self.runtime)
        runtime["paid_calls_authorized"] = True
        errors: list[str] = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest="a" * 64,
            expected_artifact_receipt_digest="b" * 64,
            expected_budget_receipt_digest="c" * 64,
        )
        self.assertIn(
            "authorized runtime requires structured qualified_artifact_receipt",
            errors,
        )
        self.assertIn(
            "authorized runtime requires structured approved_budget_receipt",
            errors,
        )

    def test_separate_authorized_runtime_config_is_accepted(self) -> None:
        digest = (ROOT / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
        runtime = self.authorized_runtime(digest)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "authorized-runtime.json"
            path.write_text(json.dumps(runtime), encoding="utf-8")
            artifact_receipt_digest = VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["qualified_artifact_receipt"])
            )
            budget_receipt_digest = VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
            )
            result = VALIDATOR.validate_bundle(
                ROOT,
                expected_digest=digest,
                runtime_config=path,
                expected_artifact_receipt_digest=artifact_receipt_digest,
                expected_budget_receipt_digest=budget_receipt_digest,
            )
        self.assertTrue(result["paid_calls_authorized"])

    def test_separate_authorized_runtime_requires_external_digest(self) -> None:
        digest = (ROOT / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
        runtime = self.authorized_runtime(digest)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "unanchored-runtime.json"
            path.write_text(json.dumps(runtime), encoding="utf-8")
            with self.assertRaisesRegex(
                ValueError,
                "authorized runtime requires an externally anchored bundle digest",
            ):
                VALIDATOR.validate_bundle(ROOT, runtime_config=path)

    def test_authorized_runtime_requires_external_receipt_digests(self) -> None:
        digest = (ROOT / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
        runtime = self.authorized_runtime(digest)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "unanchored-receipts.json"
            path.write_text(json.dumps(runtime), encoding="utf-8")
            with self.assertRaisesRegex(
                ValueError,
                "authorized runtime requires an externally anchored artifact receipt digest",
            ):
                VALIDATOR.validate_bundle(
                    ROOT,
                    expected_digest=digest,
                    runtime_config=path,
                )

    def test_separate_runtime_config_cannot_drift_model(self) -> None:
        runtime = copy.deepcopy(self.runtime)
        runtime["requested_model"] = "different-model"
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "drifted-runtime.json"
            path.write_text(json.dumps(runtime), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "runtime requested_model drift"):
                VALIDATOR.validate_bundle(ROOT, runtime_config=path)

    def test_separate_runtime_config_rejects_credential_shaped_receipt(self) -> None:
        digest = (ROOT / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
        runtime = self.authorized_runtime(digest)
        runtime["qualified_artifact_receipt"]["ci_artifact_name"] = (
            "sk-" + "ant-" + "not-a-receipt"
        )
        errors: list[str] = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=digest,
            expected_artifact_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["qualified_artifact_receipt"])
            ),
            expected_budget_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
            ),
        )
        self.assertIn("runtime config contains a credential-shaped value", errors)

    def test_receipt_strings_and_capability_fallback_fail_closed(self) -> None:
        digest = "a" * 64
        runtime = copy.deepcopy(self.runtime)
        runtime.update(
            {
                "paid_calls_authorized": True,
                "qualified_artifact_receipt": "x",
                "approved_budget_receipt": "y",
            }
        )
        errors: list[str] = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=digest,
            expected_artifact_receipt_digest="b" * 64,
            expected_budget_receipt_digest="c" * 64,
        )
        self.assertIn(
            "authorized runtime requires structured qualified_artifact_receipt", errors
        )
        self.assertIn(
            "authorized runtime requires structured approved_budget_receipt", errors
        )

        runtime = self.authorized_runtime(digest)
        runtime["qualified_artifact_receipt"]["capability_evidence"]["metal"][
            "fallback"
        ] = True
        runtime["qualified_artifact_receipt"]["capability_evidence"]["lsp"][
            "degraded_jobs"
        ] = 1
        errors = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=digest,
            expected_artifact_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["qualified_artifact_receipt"])
            ),
            expected_budget_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
            ),
        )
        self.assertIn("Metal qualification must be observed with no fallback", errors)
        self.assertIn(
            "LSP qualification degraded_jobs must be JSON integer zero", errors
        )

    def test_receipts_bind_protocol_and_budget_scope(self) -> None:
        digest = "a" * 64
        runtime = self.authorized_runtime(digest)
        runtime["qualified_artifact_receipt"]["protocol_bundle_sha256"] = "b" * 64
        runtime["approved_budget_receipt"]["authorization_scope"] = "qualification_pair"
        errors: list[str] = []
        VALIDATOR.validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=digest,
            expected_artifact_receipt_digest="0" * 64,
            expected_budget_receipt_digest=VALIDATOR.sha256_bytes(
                VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
            ),
        )
        self.assertIn("qualified artifact receipt protocol digest mismatch", errors)
        self.assertIn("qualification-pair budget scope mismatch", errors)
        self.assertIn("externally anchored artifact receipt digest mismatch", errors)

    def test_receipts_reject_boolean_integers_and_impossible_timestamps(self) -> None:
        digest = "a" * 64
        runtime = self.authorized_runtime(digest)
        artifact = runtime["qualified_artifact_receipt"]
        budget = runtime["approved_budget_receipt"]
        artifact["schema_version"] = True
        artifact["qualification_issue"] = True
        artifact["qualified_at_utc"] = "2026-99-99T99:99:99Z"
        lsp = artifact["capability_evidence"]["lsp"]
        for counter in (
            "skipped_files",
            "partial_jobs",
            "degraded_jobs",
            "cancelled_jobs",
            "crashed_jobs",
            "timed_out_jobs",
        ):
            lsp[counter] = False
        lsp["included_file_count"] = True
        lsp["covered_file_count"] = True
        budget["schema_version"] = True
        budget["authorization_issue"] = True
        budget["population_n"] = True
        budget["maximum_model_requests"] = True
        budget["approved_at_utc"] = "2026-02-30T00:00:00Z"

        errors = self.authorized_runtime_errors(runtime, digest)
        self.assertTrue(
            any("schema_version must be JSON integer" in error for error in errors)
        )
        self.assertTrue(
            any("qualification_issue must be JSON integer" in error for error in errors)
        )
        self.assertTrue(any("timestamp is invalid" in error for error in errors))
        self.assertTrue(any("must be JSON integer zero" in error for error in errors))
        self.assertIn("LSP per-file coverage must be complete", errors)
        self.assertTrue(any("budget scope mismatch" in error for error in errors))

    def test_budget_authorization_issue_is_exact_and_never_compiled_as_regex(
        self,
    ) -> None:
        digest = (ROOT / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
        rejected_issues = (
            ("string", "790"),
            ("regex_metacharacter", "["),
            ("number", 790.0),
            ("boolean", True),
            ("null", None),
        )
        for value_kind, rejected_issue in rejected_issues:
            with (
                self.subTest(value_kind=value_kind),
                tempfile.TemporaryDirectory() as temporary,
            ):
                runtime = self.authorized_runtime(digest)
                runtime["approved_budget_receipt"]["authorization_issue"] = (
                    rejected_issue
                )
                artifact_receipt_digest = VALIDATOR.sha256_bytes(
                    VALIDATOR.canonical_json(runtime["qualified_artifact_receipt"])
                )
                budget_receipt_digest = VALIDATOR.sha256_bytes(
                    VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
                )
                anchors = {
                    "expected_artifact_receipt_digest": artifact_receipt_digest,
                    "expected_budget_receipt_digest": budget_receipt_digest,
                }
                errors: list[str] = []
                VALIDATOR.validate_runtime(
                    runtime,
                    errors,
                    allow_authorized=True,
                    expected_bundle_digest=digest,
                    **anchors,
                )
                rejected_marker = json.dumps(rejected_issue).lower()
                self.assertIn(
                    "approved budget receipt authorization_issue must be the declared JSON integer",
                    errors,
                )
                self.assertIn(
                    "approved budget receipt approval URL is invalid",
                    errors,
                )
                self.assertNotIn(rejected_marker, "\n".join(errors).lower())

                runtime_path = Path(temporary) / "rejected-runtime.json"
                runtime_path.write_text(json.dumps(runtime), encoding="utf-8")
                stdout = io.StringIO()
                stderr = io.StringIO()
                with (
                    contextlib.redirect_stdout(stdout),
                    contextlib.redirect_stderr(stderr),
                ):
                    result = VALIDATOR.main(
                        [
                            "--root",
                            str(ROOT),
                            "--expected-digest",
                            digest,
                            "--runtime-config",
                            str(runtime_path),
                            "--expected-artifact-receipt-digest",
                            artifact_receipt_digest,
                            "--expected-budget-receipt-digest",
                            budget_receipt_digest,
                        ]
                    )
                self.assertEqual(result, 1)
                self.assertEqual(stdout.getvalue(), "")
                self.assertNotIn(rejected_marker, stderr.getvalue().lower())
                self.assertEqual(
                    stderr.getvalue().strip(),
                    "INCOMPATIBLE: protocol validation failed",
                )

    def test_authorization_receipt_text_fields_require_exact_json_strings(
        self,
    ) -> None:
        digest = (ROOT / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
        receipt_cases = (
            (
                "artifact protocol digest",
                "qualified_artifact_receipt",
                ("protocol_bundle_sha256",),
                int("1" * 64),
                "qualified artifact receipt protocol digest is invalid",
            ),
            (
                "artifact commit",
                "qualified_artifact_receipt",
                ("artifact_commit_sha",),
                int("7" * 40),
                "qualified artifact receipt commit is invalid",
            ),
            (
                "artifact digest",
                "qualified_artifact_receipt",
                ("artifact_sha256",),
                int("8" * 64),
                "qualified artifact receipt artifact digest is invalid",
            ),
            (
                "artifact name",
                "qualified_artifact_receipt",
                ("ci_artifact_name",),
                123,
                "qualified artifact receipt artifact name is invalid",
            ),
            (
                "qualification comment URL",
                "qualified_artifact_receipt",
                ("qualification_comment_url",),
                123,
                "qualified artifact receipt qualification URL is invalid",
            ),
            (
                "release evidence digest",
                "qualified_artifact_receipt",
                ("capability_evidence", "release_build", "evidence_sha256"),
                int("1" * 64),
                "release-build evidence digest is invalid",
            ),
            (
                "Metal evidence digest",
                "qualified_artifact_receipt",
                ("capability_evidence", "metal", "evidence_sha256"),
                int("2" * 64),
                "Metal evidence digest is invalid",
            ),
            (
                "embeddings evidence digest",
                "qualified_artifact_receipt",
                ("capability_evidence", "embeddings", "evidence_sha256"),
                int("3" * 64),
                "embeddings evidence digest is invalid",
            ),
            (
                "reranking evidence digest",
                "qualified_artifact_receipt",
                ("capability_evidence", "reranking", "evidence_sha256"),
                int("4" * 64),
                "reranking evidence digest is invalid",
            ),
            (
                "LSP coverage manifest digest",
                "qualified_artifact_receipt",
                ("capability_evidence", "lsp", "coverage_manifest_sha256"),
                int("5" * 64),
                "LSP coverage_manifest_sha256 is invalid",
            ),
            (
                "LSP evidence digest",
                "qualified_artifact_receipt",
                ("capability_evidence", "lsp", "evidence_sha256"),
                int("6" * 64),
                "LSP evidence_sha256 is invalid",
            ),
            (
                "budget protocol digest",
                "approved_budget_receipt",
                ("protocol_bundle_sha256",),
                int("1" * 64),
                "approved budget receipt protocol digest is invalid",
            ),
            (
                "maximum total USD",
                "approved_budget_receipt",
                ("maximum_total_usd",),
                100,
                "approved budget receipt maximum_total_usd is invalid",
            ),
            (
                "approval comment URL",
                "approved_budget_receipt",
                ("approval_comment_url",),
                123,
                "approved budget receipt approval URL is invalid",
            ),
            (
                "approval evidence digest",
                "approved_budget_receipt",
                ("approval_evidence_sha256",),
                int("9" * 64),
                "approved budget receipt evidence digest is invalid",
            ),
        )
        for (
            label,
            receipt_name,
            field_path,
            numeric_value,
            expected_error,
        ) in receipt_cases:
            for value_kind, rejected_value in (
                ("numeric", numeric_value),
                ("boolean", True),
                ("null", None),
            ):
                with self.subTest(field=label, value_kind=value_kind):
                    runtime = self.authorized_runtime(digest)
                    target = runtime[receipt_name]
                    for field in field_path[:-1]:
                        target = target[field]
                    target[field_path[-1]] = rejected_value
                    anchors = {
                        "expected_artifact_receipt_digest": VALIDATOR.sha256_bytes(
                            VALIDATOR.canonical_json(
                                runtime["qualified_artifact_receipt"]
                            )
                        ),
                        "expected_budget_receipt_digest": VALIDATOR.sha256_bytes(
                            VALIDATOR.canonical_json(runtime["approved_budget_receipt"])
                        ),
                    }
                    self.assert_authorized_runtime_rejected(
                        runtime,
                        digest,
                        anchors,
                        expected_error,
                        rejected_value,
                    )

        valid_runtime = self.authorized_runtime(digest)
        valid_artifact_digest = VALIDATOR.sha256_bytes(
            VALIDATOR.canonical_json(valid_runtime["qualified_artifact_receipt"])
        )
        valid_budget_digest = VALIDATOR.sha256_bytes(
            VALIDATOR.canonical_json(valid_runtime["approved_budget_receipt"])
        )
        for label, anchor_name, expected_error in (
            (
                "artifact receipt anchor",
                "expected_artifact_receipt_digest",
                "authorized runtime requires an externally anchored artifact receipt digest",
            ),
            (
                "budget receipt anchor",
                "expected_budget_receipt_digest",
                "authorized runtime requires an externally anchored budget receipt digest",
            ),
        ):
            for value_kind, rejected_value in (
                ("numeric", int("1" * 64)),
                ("boolean", True),
                ("null", None),
            ):
                with self.subTest(field=label, value_kind=value_kind):
                    anchors = {
                        "expected_artifact_receipt_digest": valid_artifact_digest,
                        "expected_budget_receipt_digest": valid_budget_digest,
                    }
                    anchors[anchor_name] = rejected_value
                    self.assert_authorized_runtime_rejected(
                        valid_runtime,
                        digest,
                        anchors,
                        expected_error,
                        rejected_value,
                    )

    def test_common_credential_shapes_fail_closed_without_value_echo(self) -> None:
        digest = "a" * 64
        synthetic_values = {
            "github": "gh" + "p_" + "A" * 36,
            "aws_access_key": "AK" + "IA" + "A" * 16,
            "aws_secret": "aws_" + "secret_access_key=" + "A" * 40,
            "aws_iam_arn": (
                "arn:" + "aws:iam::" + "123456" + "789012" + ":role/example"
            ),
            "aws_account_id": "123456" + "789012",
            "slack": "xo" + "xb-" + "A" * 20,
            "bearer": "Bear" + "er " + "A" * 24,
            "private_key": "-----BEGIN " + "PRIVATE KEY-----",
        }
        for label, synthetic_value in synthetic_values.items():
            with self.subTest(label=label):
                runtime = self.authorized_runtime(digest)
                runtime["qualified_artifact_receipt"]["ci_artifact_name"] = (
                    synthetic_value
                )
                errors = self.authorized_runtime_errors(runtime, digest)
                self.assertIn(
                    "runtime config contains a credential-shaped value", errors
                )
                self.assertFalse(any(synthetic_value in error for error in errors))
        account_digits = "123456" + "789012"
        self.assertIsNotNone(VALIDATOR.SECRET_VALUE.search("id=" + account_digits))
        self.assertIsNone(VALIDATOR.SECRET_VALUE.search("0." + account_digits))
        self.assertIsNone(
            VALIDATOR.SECRET_VALUE.search("prefixA" + account_digits + "B")
        )

    def test_paired_difference_interval_and_h2_token_vectors_are_frozen(self) -> None:
        interval = self.protocol["statistics"]["paired_difference_interval"]
        for vector in interval["test_vectors"]:
            self.assertEqual(
                VALIDATOR.paired_difference_interval(**vector["cells"]),
                vector["expected"],
            )
        with self.assertRaisesRegex(ValueError, "JSON integers"):
            VALIDATOR.paired_difference_interval(True, 0, 0, 69)

        errors: list[str] = []
        protocol = copy.deepcopy(self.protocol)
        protocol["statistics"]["paired_difference_interval"]["test_vectors"][0][
            "expected"
        ]["lower"] = "-0.000000000001"
        VALIDATOR.validate_protocol(protocol, errors)
        self.assertIn("paired-difference test vectors drift", errors)

        vector = copy.deepcopy(self.vector)
        vector["h2_token_vectors"]["records"][0]["full_token_ids"][0] += 1
        errors = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertTrue(
            any(
                "H2 per-record token vectors drift" in error
                or "token ID digest mismatch" in error
                for error in errors
            )
        )

    def test_packet_vector_freezes_b_and_c_bytes(self) -> None:
        errors: list[str] = []
        VALIDATOR.validate_packet_vector(self.vector, errors)
        self.assertEqual(errors, [])
        b_packet = VALIDATOR.assemble_packet_vector(self.vector, "B")
        c_packet = VALIDATOR.assemble_packet_vector(self.vector, "C")
        self.assertNotEqual(b_packet, c_packet)
        self.assertIn(b"def target():\n    return 1\n", c_packet)
        self.assertIn(b"def helper(long_name):\n    mvb = long_name + 1", c_packet)
        self.assertIn(b"# mvb=accumulated_value", c_packet)
        self.assertIn(b'"full_body_byte_length":182', c_packet)

    def test_packet_relationship_order_is_frozen(self) -> None:
        vector = copy.deepcopy(self.vector)
        vector["records"][1]["header"]["relationships"].reverse()
        errors: list[str] = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("packet record 2 relationship order drift", errors)

    def test_packet_relationship_projection_is_exact(self) -> None:
        vector = copy.deepcopy(self.vector)
        vector["records"][1]["header"]["relationships"] = []
        errors: list[str] = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("packet candidate 1 relationship projection mismatch", errors)

        vector = copy.deepcopy(self.vector)
        omitted_only = vector["metadata"]["acquisition"]["relationships"][-1]
        vector["records"][1]["header"]["relationships"].append(omitted_only)
        vector["records"][1]["header"]["relationships"].sort(
            key=VALIDATOR._relationship_sort_key
        )
        errors = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("packet candidate 1 relationship projection mismatch", errors)

        vector = copy.deepcopy(self.vector)
        vector["records"][0]["header"]["relationships"] = [
            vector["metadata"]["acquisition"]["relationships"][0]
        ]
        errors = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("packet locus 1 relationships must be empty", errors)

        self.assertEqual(len(self.vector["records"]), 3)
        self.assertEqual(
            [record["header"]["stable_id"] for record in self.vector["records"][1:]],
            ["src/helper.py:helper:function", "src/second.py:second:function"],
        )
        self.assertFalse(
            any(
                omitted_only in record["header"]["relationships"]
                for record in self.vector["records"]
            )
        )

        vector = copy.deepcopy(self.vector)
        vector["metadata"]["acquisition"]["candidates"][1]["graph_component"] = 0
        vector["metadata"]["acquisition"]["candidates"][1]["total"] = 998000000
        vector["metadata"]["acquisition"]["candidates"][1]["graph_hops"] = None
        errors = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("acquisition candidate 2 graph score derivation drift", errors)
        self.assertIn("acquisition candidate 2 graph_hops derivation drift", errors)

    def test_acquisition_omissions_are_closed_and_consistent(self) -> None:
        vector = copy.deepcopy(self.vector)
        vector["metadata"]["acquisition"]["omissions"] = []
        errors: list[str] = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertTrue(any("requires one omission" in error for error in errors))

        vector = copy.deepcopy(self.vector)
        vector["metadata"]["acquisition"]["loci"][0]["stable_id"] = "ambiguous-locus"
        errors = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("acquisition locus 1 stable_id drift", errors)

    def test_acquisition_relationships_are_bound_to_locus_seeds(self) -> None:
        vector = copy.deepcopy(self.vector)
        acquisition = vector["metadata"]["acquisition"]
        acquisition["relationships"][0]["target"] = "src/oversize.py:oversize:function"
        for record in vector["records"]:
            if record["kind"] == "candidate":
                record["header"]["relationships"] = VALIDATOR._project_relationships(
                    record["header"]["stable_id"], acquisition["relationships"]
                )
        vector["expected_b_sha256"] = VALIDATOR.sha256_bytes(
            VALIDATOR.assemble_packet_vector(vector, "B")
        )
        vector["expected_c_sha256"] = VALIDATOR.sha256_bytes(
            VALIDATOR.assemble_packet_vector(vector, "C")
        )

        errors: list[str] = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn(
            "packet acquisition relationship 1 is not bound to its locus seed",
            errors,
        )

    def test_acquisition_admission_and_omissions_are_replayed(self) -> None:
        candidates = self.vector["metadata"]["acquisition"]["candidates"]
        decisions, omissions = VALIDATOR._replay_candidate_admission(candidates)
        self.assertEqual(decisions, [True, False, True])
        self.assertEqual(
            omissions,
            [
                {
                    "candidate_stable_id": "src/oversize.py:oversize:function",
                    "reason": "full_body_budget",
                    "required_bytes": 70000,
                    "remaining_budget_bytes": 65354,
                }
            ],
        )

        cap_candidates = [
            {
                "stable_id": f"candidate-{ordinal}",
                "eligibility": "eligible",
                "full_body_byte_length": 1,
            }
            for ordinal in range(1, 26)
        ]
        cap_candidates.append(
            {
                "stable_id": "ineligible-after-cap",
                "eligibility": "not_source_backed",
                "full_body_byte_length": 1,
            }
        )
        decisions, omissions = VALIDATOR._replay_candidate_admission(cap_candidates)
        self.assertEqual(decisions, [True] * 24 + [False, False])
        self.assertEqual(
            omissions,
            [
                {
                    "candidate_stable_id": "candidate-25",
                    "reason": "maximum_candidates",
                    "required_bytes": 1,
                    "remaining_budget_bytes": 65512,
                },
                {
                    "candidate_stable_id": "ineligible-after-cap",
                    "reason": "not_source_backed",
                    "required_bytes": None,
                    "remaining_budget_bytes": None,
                },
            ],
        )

        vector = copy.deepcopy(self.vector)
        candidates = vector["metadata"]["acquisition"]["candidates"]
        candidates[0], candidates[1] = candidates[1], candidates[0]
        for ordinal, candidate in enumerate(candidates, 1):
            candidate["acquisition_ordinal"] = ordinal
        vector["expected_b_sha256"] = VALIDATOR.sha256_bytes(
            VALIDATOR.assemble_packet_vector(vector, "B")
        )
        vector["expected_c_sha256"] = VALIDATOR.sha256_bytes(
            VALIDATOR.assemble_packet_vector(vector, "C")
        )
        errors: list[str] = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("packet acquisition candidate order drift", errors)

        vector = copy.deepcopy(self.vector)
        vector["metadata"]["acquisition"]["omissions"][0]["remaining_budget_bytes"] += 1
        vector["expected_b_sha256"] = VALIDATOR.sha256_bytes(
            VALIDATOR.assemble_packet_vector(vector, "B")
        )
        vector["expected_c_sha256"] = VALIDATOR.sha256_bytes(
            VALIDATOR.assemble_packet_vector(vector, "C")
        )
        errors = []
        VALIDATOR.validate_packet_vector(vector, errors)
        self.assertIn("packet acquisition omission replay drift", errors)

    def test_retry_prompt_vector_freezes_codepoint_slice_and_bytes(self) -> None:
        errors: list[str] = []
        VALIDATOR.validate_retry_prompt_vector(
            self.vector["retry_prompt_vector"], errors
        )
        self.assertEqual(errors, [])
        request = VALIDATOR.assemble_retry_prompt_vector(
            self.vector["retry_prompt_vector"]
        )
        self.assertEqual(len(request), 12336)
        self.assertEqual(
            VALIDATOR.sha256_bytes(request),
            "1043956d8aed9614eb3638d6fa09cde29273c382d8384a8710a0acd202fb2b09",
        )

        vector = copy.deepcopy(self.vector["retry_prompt_vector"])
        vector["previous_response_codepoint_runs"][1]["repeat"] = 6000
        errors = []
        VALIDATOR.validate_retry_prompt_vector(vector, errors)
        self.assertTrue(
            any("length drift" in error or "digest drift" in error for error in errors)
        )

    def test_vendored_parser_ignores_reasoning_and_requires_unique_search(self) -> None:
        response = "reasoning first\n*** FILE: a/example.py\n*** SEARCH\nvalue = 1\n*** REPLACE\nvalue = 2\n*** END\ntrailing prose"
        edits = PARSER.parse_edits(response)
        self.assertEqual(
            edits,
            [{"file": "example.py", "search": "value = 1", "replace": "value = 2"}],
        )
        matches, _, _ = PARSER._find_block("value = 1\nvalue = 1\n", "value = 1")
        self.assertEqual(matches, 2)

    def test_duplicate_json_key_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "duplicate.json"
            marker = "UNTRUSTED_DUPLICATE_KEY_MARKER"
            path.write_text(
                json.dumps({"safe": 1})[:-1] + f',"{marker}":1,"{marker}":2}}\n',
                encoding="utf-8",
            )
            with self.assertRaises(VALIDATOR.DuplicateKey) as context:
                VALIDATOR.load_json(path)
            self.assertNotIn(marker, str(context.exception))

            stdout = io.StringIO()
            stderr = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                result = VALIDATOR.main(
                    [
                        "--root",
                        str(ROOT),
                        "--runtime-config",
                        str(path),
                    ]
                )
            self.assertEqual(result, 1)
            self.assertNotIn(marker, stdout.getvalue())
            self.assertNotIn(marker, stderr.getvalue())
            self.assertEqual(
                stderr.getvalue().strip(),
                "INCOMPATIBLE: protocol validation failed",
            )

    def test_every_top_level_json_object_input_rejects_other_shapes(self) -> None:
        malformed_values = {
            "list": ["UNTRUSTED_LIST_MARKER"],
            "null": None,
            "string": "UNTRUSTED_STRING_MARKER",
            "number": 7,
            "boolean": True,
        }
        bundle_inputs = {
            "protocol": VALIDATOR.PROTOCOL_REL,
            "population": VALIDATOR.POPULATION_REL,
            "runtime_template": VALIDATOR.RUNTIME_REL,
            "packet_vector": VALIDATOR.VECTOR_REL,
            "lock_manifest": VALIDATOR.LOCK_REL,
        }

        for input_label, rel in bundle_inputs.items():
            for shape, malformed in malformed_values.items():
                with (
                    self.subTest(input=input_label, shape=shape),
                    tempfile.TemporaryDirectory() as temporary,
                ):
                    copied_root = Path(temporary)
                    self.copy_locked_bundle(copied_root)
                    malformed_path = copied_root / rel
                    malformed_path.write_text(json.dumps(malformed), encoding="utf-8")
                    with self.assertRaisesRegex(ValueError, "JSON object"):
                        VALIDATOR.load_json_object(malformed_path, input_label)
                    stdout = io.StringIO()
                    stderr = io.StringIO()
                    with (
                        contextlib.redirect_stdout(stdout),
                        contextlib.redirect_stderr(stderr),
                    ):
                        result = VALIDATOR.main(["--root", str(copied_root)])
                    self.assertEqual(result, 1)
                    self.assertEqual(stdout.getvalue(), "")
                    self.assertEqual(
                        stderr.getvalue().strip(),
                        "INCOMPATIBLE: protocol validation failed",
                    )

        for shape, malformed in malformed_values.items():
            with (
                self.subTest(input="runtime_config", shape=shape),
                tempfile.TemporaryDirectory() as temporary,
            ):
                runtime_path = Path(temporary) / "runtime.json"
                runtime_path.write_text(json.dumps(malformed), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "JSON object"):
                    VALIDATOR.load_json_object(runtime_path, "runtime config")
                stdout = io.StringIO()
                stderr = io.StringIO()
                with (
                    contextlib.redirect_stdout(stdout),
                    contextlib.redirect_stderr(stderr),
                ):
                    result = VALIDATOR.main(
                        ["--root", str(ROOT), "--runtime-config", str(runtime_path)]
                    )
                self.assertEqual(result, 1)
                self.assertEqual(stdout.getvalue(), "")
                self.assertEqual(
                    stderr.getvalue().strip(),
                    "INCOMPATIBLE: protocol validation failed",
                )

    def test_locked_file_tamper_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            copied_root = Path(temporary)
            self.copy_locked_bundle(copied_root)
            protocol_path = copied_root / VALIDATOR.PROTOCOL_REL
            protocol_path.write_text(
                protocol_path.read_text(encoding="utf-8") + " ", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, "digest drift"):
                VALIDATOR.validate_bundle(copied_root)

    def test_lock_manifest_credential_metadata_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            copied_root = Path(temporary)
            self.copy_locked_bundle(copied_root)
            lock_path = copied_root / VALIDATOR.LOCK_REL
            lock = VALIDATOR.load_json(lock_path)
            lock["note"] = "sk-" + "ant-" + "not-lock-metadata"
            lock_path.write_text(json.dumps(lock), encoding="utf-8")
            digest = (
                (copied_root / VALIDATOR.DIGEST_REL).read_text(encoding="ascii").strip()
            )
            with self.assertRaisesRegex(
                ValueError, "credential-shaped value in lock manifest"
            ):
                VALIDATOR.validate_bundle(copied_root, expected_digest=digest)

    def test_lock_manifest_rejects_boolean_schema_version_without_echo(self) -> None:
        for schema_version in (True, False):
            with (
                self.subTest(schema_version=schema_version),
                tempfile.TemporaryDirectory() as temporary,
            ):
                copied_root = Path(temporary)
                self.copy_locked_bundle(copied_root)
                lock_path = copied_root / VALIDATOR.LOCK_REL
                lock = VALIDATOR.load_json(lock_path)
                lock["schema_version"] = schema_version
                lock_path.write_text(json.dumps(lock), encoding="utf-8")
                digest = (
                    (copied_root / VALIDATOR.DIGEST_REL)
                    .read_text(encoding="ascii")
                    .strip()
                )
                rejected_value = json.dumps(schema_version)

                with self.assertRaisesRegex(
                    ValueError, "lock schema_version must be JSON integer 1"
                ) as context:
                    VALIDATOR.validate_bundle(copied_root, expected_digest=digest)
                self.assertNotIn(rejected_value, str(context.exception).lower())

                stdout = io.StringIO()
                stderr = io.StringIO()
                with (
                    contextlib.redirect_stdout(stdout),
                    contextlib.redirect_stderr(stderr),
                ):
                    result = VALIDATOR.main(
                        [
                            "--root",
                            str(copied_root),
                            "--expected-digest",
                            digest,
                        ]
                    )
                self.assertEqual(result, 1)
                self.assertNotIn(rejected_value, stdout.getvalue().lower())
                self.assertNotIn(rejected_value, stderr.getvalue().lower())
                self.assertEqual(
                    stderr.getvalue().strip(),
                    "INCOMPATIBLE: protocol validation failed",
                )

    def test_unsafe_lock_paths_are_rejected_before_target_read(self) -> None:
        cases = (
            "absolute",
            "parent",
            "non_normalized",
            "symlink_escape",
            "symlink_in_repo",
            "symlink_parent",
        )
        for case in cases:
            with (
                self.subTest(case=case),
                tempfile.TemporaryDirectory() as temporary,
            ):
                temporary_root = Path(temporary)
                copied_root = temporary_root / "repo"
                self.copy_locked_bundle(copied_root)
                marker = f"UNTRUSTED_{case.upper()}_TARGET_MARKER"
                outside = temporary_root / f"{marker}.txt"
                outside.write_text(
                    f"{marker}: outside target must never be opened", encoding="utf-8"
                )
                lock_path = copied_root / VALIDATOR.LOCK_REL
                lock = VALIDATOR.load_json(lock_path)
                locked_rel = (
                    "benchmark/swebench-act-context/upstream/LICENSE"
                    if case == "symlink_parent"
                    else VALIDATOR.EXPECTED_LOCK_PATHS[0]
                )
                entry_ordinal, entry = next(
                    (ordinal, item)
                    for ordinal, item in enumerate(lock["files"], 1)
                    if item["path"] == locked_rel
                )
                locked_path = copied_root / locked_rel

                if case == "absolute":
                    entry["path"] = str(outside)
                    forbidden = outside
                elif case == "parent":
                    entry["path"] = "../outside-target.txt"
                    forbidden = temporary_root / "outside-target.txt"
                    forbidden.write_text(
                        f"{marker}: exact parent target must never be opened",
                        encoding="utf-8",
                    )
                elif case == "non_normalized":
                    entry["path"] = locked_rel.replace("/", "//", 1)
                    forbidden = locked_path
                elif case == "symlink_escape":
                    locked_path.unlink()
                    locked_path.symlink_to(outside)
                    forbidden = outside
                elif case == "symlink_in_repo":
                    forbidden = copied_root / f"{marker}.md"
                    shutil.copy2(locked_path, forbidden)
                    locked_path.unlink()
                    locked_path.symlink_to(forbidden)
                else:
                    forbidden_parent = copied_root / marker
                    shutil.copytree(locked_path.parent, forbidden_parent)
                    shutil.rmtree(locked_path.parent)
                    locked_path.parent.symlink_to(
                        forbidden_parent, target_is_directory=True
                    )
                    forbidden = forbidden_parent / locked_path.name

                lock_path.write_text(json.dumps(lock), encoding="utf-8")
                readable_without_guard = forbidden.read_bytes()
                self.assertTrue(forbidden.is_file())
                self.assertGreater(len(readable_without_guard), 0)
                forbidden_resolved = forbidden.resolve(strict=True)
                opened_forbidden: list[Path] = []
                original_open = Path.open

                def guarded_open(
                    path: Path,
                    mode: str = "r",
                    buffering: int = -1,
                    encoding: str | None = None,
                    errors: str | None = None,
                    newline: str | None = None,
                ) -> object:
                    if path.resolve(strict=True) == forbidden_resolved:
                        opened_forbidden.append(path)
                        raise AssertionError("unsafe lock target was opened")
                    return original_open(
                        path, mode, buffering, encoding, errors, newline
                    )

                errors: list[str] = []
                stdout = io.StringIO()
                stderr = io.StringIO()
                with (
                    mock.patch.object(Path, "open", guarded_open),
                    contextlib.redirect_stdout(stdout),
                    contextlib.redirect_stderr(stderr),
                ):
                    VALIDATOR.validate_lock(copied_root, errors)
                    result = VALIDATOR.main(["--root", str(copied_root)])
                self.assertIn(
                    f"lock entry {entry_ordinal} path is unsafe",
                    errors,
                )
                self.assertEqual(opened_forbidden, [])
                self.assertEqual(result, 1)
                self.assertEqual(stdout.getvalue(), "")
                self.assertNotIn(marker, stderr.getvalue())
                self.assertEqual(
                    stderr.getvalue().strip(),
                    "INCOMPATIBLE: protocol validation failed",
                )
                if case in {"symlink_escape", "symlink_in_repo", "symlink_parent"}:
                    self.assertIn("bundle file inventory contains a symlink", errors)

    def test_resealed_locked_file_with_common_credential_shape_fails_closed(
        self,
    ) -> None:
        synthetic_values = {
            "github": "gh" + "p_" + "A" * 36,
            "aws_iam_arn": (
                "arn:" + "aws:iam::" + "123456" + "789012" + ":role/example"
            ),
            "aws_account_id": "123456" + "789012",
        }
        for label, synthetic_value in synthetic_values.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as temporary:
                copied_root = Path(temporary)
                self.copy_locked_bundle(copied_root)
                readme_path = copied_root / "benchmark/swebench-act-context/README.md"
                readme_path.write_text(
                    readme_path.read_text(encoding="utf-8")
                    + "\n"
                    + synthetic_value
                    + "\n",
                    encoding="utf-8",
                )
                lock_path = copied_root / VALIDATOR.LOCK_REL
                lock = VALIDATOR.load_json(lock_path)
                for entry in lock["files"]:
                    file_path = copied_root / entry["path"]
                    data = file_path.read_bytes()
                    entry["bytes"] = len(data)
                    entry["sha256"] = VALIDATOR.sha256_bytes(data)
                digest = VALIDATOR.sha256_bytes(VALIDATOR._lock_material(lock["files"]))
                lock["bundle_sha256"] = digest
                lock_path.write_text(
                    json.dumps(lock, indent=2, ensure_ascii=False) + "\n",
                    encoding="utf-8",
                )
                (copied_root / VALIDATOR.DIGEST_REL).write_text(
                    digest + "\n", encoding="ascii"
                )

                with self.assertRaisesRegex(
                    ValueError,
                    "credential-shaped value in locked file",
                ) as context:
                    VALIDATOR.validate_bundle(copied_root, expected_digest=digest)
                self.assertNotIn(synthetic_value, str(context.exception))

    def test_unlisted_bundle_file_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            copied_root = Path(temporary)
            self.copy_locked_bundle(copied_root)
            extra = copied_root / "benchmark/swebench-act-context/unlisted.txt"
            extra.write_text("unexpected", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "bundle file inventory drift"):
                VALIDATOR.validate_bundle(copied_root)


if __name__ == "__main__":
    unittest.main()
