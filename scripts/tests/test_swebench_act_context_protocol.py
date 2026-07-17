#!/usr/bin/env python3

from __future__ import annotations

import ast
import copy
import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "validate_swebench_act_context_protocol.py"
SPEC = importlib.util.spec_from_file_location("validate_swebench_act_context_protocol", SCRIPT)
assert SPEC and SPEC.loader
VALIDATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VALIDATOR
SPEC.loader.exec_module(VALIDATOR)

PARSER_PATH = ROOT / "benchmark" / "swebench-act-context" / "upstream" / "edit_patch_v2.py"
PARSER_SPEC = importlib.util.spec_from_file_location("frozen_edit_patch_v2", PARSER_PATH)
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

    def test_frozen_bundle_is_compatible(self) -> None:
        result = VALIDATOR.validate_bundle(ROOT)
        self.assertTrue(result["compatible"])
        self.assertTrue(result["a_binary_outcomes_comparable"])
        self.assertTrue(result["a_initial_request_tokens_comparable"])
        self.assertFalse(result["a_retry_inclusive_tokens_comparable"])
        self.assertEqual(result["a_retry_inclusive_tokens_reason"], "upstream_not_measured")
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
        self.assertTrue(any("missing A initial-request token count" in error for error in errors))

    def test_inferred_a_retry_inclusive_total_is_rejected(self) -> None:
        population = copy.deepcopy(self.population)
        first = next(row for row in population["instances"] if row["included"])
        first["upstream_a"]["retry_inclusive_input_tokens"] = 12345
        errors: list[str] = []
        VALIDATOR.validate_population(population, errors)
        self.assertTrue(any("inferred A retry-inclusive tokens are forbidden" in error for error in errors))

    def test_h1_denominator_and_noninferiority_are_frozen(self) -> None:
        protocol = copy.deepcopy(self.protocol)
        protocol["hypotheses"]["H1"]["claim"] = "B is cheaper on resolved rows"
        errors: list[str] = []
        VALIDATOR.validate_protocol(protocol, errors)
        self.assertTrue(any("H1 efficiency threshold drift" in error for error in errors))
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
        VALIDATOR.validate_runtime(runtime, errors, allow_authorized=True)
        self.assertIn(
            "authorized runtime requires non-empty qualified_artifact_receipt",
            errors,
        )
        self.assertIn(
            "authorized runtime requires non-empty approved_budget_receipt",
            errors,
        )

    def test_separate_authorized_runtime_config_is_accepted(self) -> None:
        runtime = copy.deepcopy(self.runtime)
        runtime.update(
            {
                "paid_calls_authorized": True,
                "qualified_artifact_receipt": "qualification:sha256:abc123",
                "approved_budget_receipt": "budget:approval:782",
            }
        )
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "authorized-runtime.json"
            path.write_text(json.dumps(runtime), encoding="utf-8")
            result = VALIDATOR.validate_bundle(ROOT, runtime_config=path)
        self.assertTrue(result["paid_calls_authorized"])

    def test_separate_runtime_config_cannot_drift_model(self) -> None:
        runtime = copy.deepcopy(self.runtime)
        runtime["requested_model"] = "different-model"
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "drifted-runtime.json"
            path.write_text(json.dumps(runtime), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "runtime requested_model drift"):
                VALIDATOR.validate_bundle(ROOT, runtime_config=path)

    def test_separate_runtime_config_rejects_credential_shaped_receipt(self) -> None:
        runtime = copy.deepcopy(self.runtime)
        runtime.update(
            {
                "paid_calls_authorized": True,
                "qualified_artifact_receipt": "sk-" + "ant-" + "not-a-receipt",
                "approved_budget_receipt": "budget:approval:782",
            }
        )
        errors: list[str] = []
        VALIDATOR.validate_runtime(runtime, errors, allow_authorized=True)
        self.assertIn("runtime config contains a credential-shaped value", errors)

    def test_packet_vector_freezes_b_and_c_bytes(self) -> None:
        errors: list[str] = []
        VALIDATOR.validate_packet_vector(self.vector, errors)
        self.assertEqual(errors, [])
        b_packet = VALIDATOR.assemble_packet_vector(self.vector, "B")
        c_packet = VALIDATOR.assemble_packet_vector(self.vector, "C")
        self.assertNotEqual(b_packet, c_packet)
        self.assertIn(b"def target():\n    return 1\n", c_packet)
        self.assertNotIn(b"long_name", c_packet)

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
            path.write_text('{"a":1,"a":2}\n', encoding="utf-8")
            with self.assertRaises(VALIDATOR.DuplicateKey):
                VALIDATOR.load_json(path)

    def test_locked_file_tamper_fails_closed(self) -> None:
        lock = VALIDATOR.load_json(ROOT / VALIDATOR.LOCK_REL)
        with tempfile.TemporaryDirectory() as temporary:
            copied_root = Path(temporary)
            for entry in lock["files"]:
                rel = Path(entry["path"])
                destination = copied_root / rel
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / rel, destination)
            for rel in (VALIDATOR.LOCK_REL, VALIDATOR.DIGEST_REL):
                destination = copied_root / rel
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / rel, destination)
            protocol_path = copied_root / VALIDATOR.PROTOCOL_REL
            protocol_path.write_text(protocol_path.read_text(encoding="utf-8") + " ", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "digest drift"):
                VALIDATOR.validate_bundle(copied_root)


if __name__ == "__main__":
    unittest.main()
