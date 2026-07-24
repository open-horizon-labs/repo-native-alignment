#!/usr/bin/env python3
"""Independent, one-use authorization receipts for the issue #827 evaluator."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
from typing import Any, Mapping

import provider_usage


SCHEMA_VERSION = "issue827-evaluator-authorization-v1"
ACTOR_SCHEMA = "issue827-actor-tool-ledger-v1"
HEX64 = re.compile(r"^[0-9a-f]{64}$")


class AuthorizationError(ValueError):
    pass


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def file_ref(path: Path, data: bytes | None = None) -> dict[str, Any]:
    path = path.resolve(strict=True) if data is None else path.resolve(strict=False)
    payload = path.read_bytes() if data is None else data
    return {"path": str(path), "bytes": len(payload), "sha256": sha256_bytes(payload)}


def _load_ref(reference: Any, label: str) -> tuple[Path, bytes]:
    if not isinstance(reference, dict) or set(reference) != {"path", "bytes", "sha256"}:
        raise AuthorizationError(f"{label}_reference_invalid")
    path = Path(reference["path"])
    if not path.is_absolute() or not path.is_file() or path.is_symlink():
        raise AuthorizationError(f"{label}_file_invalid")
    data = path.read_bytes()
    if type(reference["bytes"]) is not int or reference["bytes"] != len(data):
        raise AuthorizationError(f"{label}_byte_count_mismatch")
    if not isinstance(reference["sha256"], str) or not HEX64.fullmatch(reference["sha256"]):
        raise AuthorizationError(f"{label}_digest_invalid")
    if reference["sha256"] != sha256_bytes(data):
        raise AuthorizationError(f"{label}_digest_mismatch")
    return path.resolve(strict=True), data


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise AuthorizationError(message)


def _validate_token_ledger(ledger: Any) -> None:
    _require(isinstance(ledger, dict), "token_ledger_missing")
    _require(ledger.get("schema_version") == provider_usage.SCHEMA_VERSION, "token_ledger_schema_invalid")
    _require(ledger.get("valid") is True and ledger.get("errors") == [], "token_ledger_invalid")
    _require(ledger.get("model_invoked") is True, "model_invocation_not_proved")
    for key in (*provider_usage.REQUIRED_TOKEN_FIELDS, "provider_total_tokens"):
        _require(type(ledger.get(key)) is int and ledger[key] >= 0, f"token_{key}_invalid")
    reasoning_observed = ledger.get("reasoning_tokens_observed")
    unobserved = ledger.get("unobserved_fields")
    _require(type(reasoning_observed) is bool, "reasoning_tokens_observed_invalid")
    _require(
        isinstance(unobserved, list)
        and all(isinstance(item, str) for item in unobserved),
        "unobserved_fields_invalid",
    )
    if reasoning_observed:
        _require(
            type(ledger.get("reasoning_tokens")) is int
            and ledger["reasoning_tokens"] >= 0,
            "token_reasoning_tokens_invalid",
        )
        _require(
            "reasoning_tokens" not in unobserved,
            "reasoning_tokens_observation_inconsistent",
        )
    else:
        _require(
            ledger.get("reasoning_tokens") is None,
            "reasoning_tokens_observation_inconsistent",
        )
        _require(
            unobserved == ["reasoning_tokens"],
            "reasoning_tokens_unobserved_fields_invalid",
        )
    _require(
        ledger["provider_total_tokens"]
        == ledger["input_tokens"]
        + ledger["cache_creation_input_tokens"]
        + ledger["cache_read_input_tokens"]
        + ledger["output_tokens"],
        "provider_token_total_mismatch",
    )
    _require(ledger["provider_total_tokens"] > 0, "provider_token_total_zero")
    _require(
        type(ledger.get("provider_responses")) is int
        and ledger["provider_responses"] > 0,
        "provider_response_count_missing",
    )
    _require(
        ledger.get("provider_responses_scope") == "agent_transcript_only",
        "provider_response_scope_invalid",
    )
    _require(
        ledger.get("provider_requests") is None
        or (type(ledger["provider_requests"]) is int and ledger["provider_requests"] >= 0),
        "provider_request_count_invalid",
    )


def _validate_actor_ledger(receipt: Mapping[str, Any]) -> dict[str, Any]:
    _, data = _load_ref(receipt.get("actor_tool_ledger"), "actor_tool_ledger")
    try:
        actor = json.loads(data)
    except json.JSONDecodeError as exc:
        raise AuthorizationError("actor_tool_ledger_invalid_json") from exc
    _require(isinstance(actor, dict) and actor.get("schema_version") == ACTOR_SCHEMA, "actor_tool_ledger_schema_invalid")
    _require(actor.get("arm") == receipt.get("arm"), "actor_tool_ledger_arm_mismatch")
    actions = actor.get("actions")
    _require(isinstance(actions, list) and bool(actions), "actor_tool_actions_missing")
    _require(
        [action.get("sequence") for action in actions if isinstance(action, dict)]
        == list(range(1, len(actions) + 1)),
        "actor_tool_sequence_invalid",
    )
    request = actions[-1]
    _require(
        isinstance(request, dict)
        and request.get("actor") == "harness"
        and request.get("action") == "official_evaluator_authorization_request"
        and request.get("requested") is True
        and request.get("authorized") is False
        and request.get("invoked") is False,
        "actor_tool_authorization_request_invalid",
    )
    _require(
        not any(
            action.get("authorized") is True or action.get("invoked") is True
            for action in actions
            if isinstance(action, dict)
        ),
        "actor_or_harness_self_authorized_evaluator",
    )
    return actor


def build(
    receipt_path: Path,
    receipt: Mapping[str, Any],
    verification_path: Path,
    verification: Mapping[str, Any],
) -> dict[str, Any]:
    """Recompute authorization solely from frozen receipt/verifier evidence."""
    receipt_bytes = receipt_path.resolve(strict=True).read_bytes()
    verification_bytes = canonical(verification)
    _require(receipt.get("authorization_requested") is True, "authorization_not_requested")
    _require(receipt.get("evaluator_authorized") is False, "runner_self_authorized_evaluator")
    _require(receipt.get("official_evaluator_invoked") is False, "runner_invoked_evaluator")
    _require(receipt.get("returncode") == 0 and receipt.get("timed_out") is False, "model_not_terminal_success")
    _require(receipt.get("errors") == [], "episode_receipt_has_errors")
    _require(receipt.get("evidence_complete") is True and receipt.get("policy_compliant") is True, "episode_receipt_not_eligible")
    _require(verification.get("errors") == [], "episode_verification_has_errors")
    _require(verification.get("evidence_complete") is True and verification.get("policy_compliant") is True, "episode_verification_not_eligible")
    _require(verification.get("evaluator_authorized") is True, "verifier_did_not_authorize")
    _require(verification.get("official_evaluator_invoked") is False, "verifier_reports_evaluator_invocation")
    _require(verification.get("episode_receipt") == file_ref(receipt_path), "verification_receipt_binding_mismatch")
    patch = receipt.get("terminal_patch")
    _require(patch is not None and verification.get("terminal_patch") == patch, "terminal_patch_missing_or_unbound")
    _load_ref(patch, "terminal_patch")
    _validate_token_ledger(receipt.get("token_ledger"))
    _require(verification.get("token_ledger") == receipt.get("token_ledger"), "verification_token_ledger_mismatch")
    _validate_actor_ledger(receipt)
    core = {
        "case_id": receipt.get("case_id"),
        "rank": receipt.get("rank"),
        "arm": receipt.get("arm"),
        "episode_receipt": file_ref(receipt_path),
        "episode_verification": file_ref(verification_path, verification_bytes),
        "actor_tool_ledger": dict(receipt["actor_tool_ledger"]),
        "terminal_patch": dict(patch),
        "token_ledger_sha256": sha256_bytes(canonical(receipt["token_ledger"])),
        "decision": "authorize_once",
        "one_use": True,
        "official_evaluator_invocations_before_authorization": 0,
    }
    return {
        "schema_version": SCHEMA_VERSION,
        "authorization_id": sha256_bytes(canonical(core)),
        **core,
    }


def validate(
    authorization: Mapping[str, Any],
    receipt_path: Path,
    receipt: Mapping[str, Any],
    verification_path: Path,
    verification: Mapping[str, Any],
) -> None:
    expected = build(receipt_path, receipt, verification_path, verification)
    _require(dict(authorization) == expected, "evaluator_authorization_not_reproducible")


def write_exclusive(path: Path, value: Mapping[str, Any]) -> None:
    payload = canonical(value)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise
