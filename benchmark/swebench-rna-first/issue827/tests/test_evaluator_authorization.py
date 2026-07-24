from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import evaluator_authorization as authorization
import provider_usage


class EvaluatorAuthorizationTests(unittest.TestCase):
    def fixture(self, root: Path):
        patch = root / "terminal.patch"
        patch.write_bytes(b"diff --git a/x b/x\n")
        actor = root / "actor.json"
        actor.write_bytes(authorization.canonical({
            "schema_version": authorization.ACTOR_SCHEMA,
            "arm": "A",
            "actions": [{
                "sequence": 1,
                "actor": "harness",
                "action": "official_evaluator_authorization_request",
                "requested": True,
                "authorized": False,
                "invoked": False,
            }],
        }))
        ledger = {
            "schema_version": provider_usage.SCHEMA_VERSION,
            "valid": True,
            "errors": [],
            "source": "top_level_usage+model_events",
            "model_invoked": True,
            "input_tokens": 3,
            "cache_creation_input_tokens": 1,
            "cache_read_input_tokens": 2,
            "output_tokens": 4,
            "reasoning_tokens": 2,
            "reasoning_tokens_observed": True,
            "unobserved_fields": [],
            "provider_total_tokens": 10,
            "cli_turns": 1,
            "provider_responses": 1,
            "provider_responses_scope": "agent_transcript_only",
            "provider_requests": None,
        }
        receipt = {
            "case_id": "repo__repo-1", "rank": 1, "arm": "A",
            "authorization_requested": True,
            "evaluator_authorized": False,
            "official_evaluator_invoked": False,
            "returncode": 0, "timed_out": False, "errors": [],
            "evidence_complete": True, "policy_compliant": True,
            "terminal_patch": authorization.file_ref(patch),
            "actor_tool_ledger": authorization.file_ref(actor),
            "token_ledger": ledger,
        }
        receipt_path = root / "episode-receipt.json"
        receipt_path.write_bytes(authorization.canonical(receipt))
        verification = {
            "episode_receipt": authorization.file_ref(receipt_path),
            "case_id": receipt["case_id"], "rank": 1, "arm": "A",
            "errors": [], "evidence_complete": True, "policy_compliant": True,
            "evaluator_authorized": True, "official_evaluator_invoked": False,
            "terminal_patch": receipt["terminal_patch"], "token_ledger": ledger,
        }
        verification_path = root / "episode-verification.json"
        return receipt_path, receipt, verification_path, verification

    def test_authorization_is_reproducible_and_one_use(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = self.fixture(Path(temporary))
            value = authorization.build(*args)
            self.assertTrue(value["one_use"])
            self.assertEqual(value["decision"], "authorize_once")
            authorization.validate(value, *args)
            tampered = {**value, "authorization_id": "0" * 64}
            with self.assertRaisesRegex(authorization.AuthorizationError, "not_reproducible"):
                authorization.validate(tampered, *args)

    def test_unobserved_optional_reasoning_still_authorizes(self):
        with tempfile.TemporaryDirectory() as temporary:
            receipt_path, receipt, verification_path, verification = self.fixture(Path(temporary))
            receipt["token_ledger"] = {
                **receipt["token_ledger"],
                "reasoning_tokens": None,
                "reasoning_tokens_observed": False,
                "unobserved_fields": ["reasoning_tokens"],
            }
            receipt_path.write_bytes(authorization.canonical(receipt))
            verification["episode_receipt"] = authorization.file_ref(receipt_path)
            verification["token_ledger"] = receipt["token_ledger"]
            value = authorization.build(
                receipt_path,
                receipt,
                verification_path,
                verification,
            )
            self.assertEqual(value["decision"], "authorize_once")

    def test_reasoning_observation_inconsistency_never_authorizes(self):
        with tempfile.TemporaryDirectory() as temporary:
            receipt_path, receipt, verification_path, verification = self.fixture(Path(temporary))
            receipt["token_ledger"] = {
                **receipt["token_ledger"],
                "reasoning_tokens": None,
                "reasoning_tokens_observed": True,
            }
            receipt_path.write_bytes(authorization.canonical(receipt))
            verification["episode_receipt"] = authorization.file_ref(receipt_path)
            verification["token_ledger"] = receipt["token_ledger"]
            with self.assertRaisesRegex(
                authorization.AuthorizationError,
                "reasoning_tokens",
            ):
                authorization.build(
                    receipt_path,
                    receipt,
                    verification_path,
                    verification,
                )


if __name__ == "__main__":
    unittest.main()
