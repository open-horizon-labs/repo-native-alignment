#!/usr/bin/env python3
"""Seal and evaluate #825 A/T terminal patches exactly once.

The adapter has no model integration.  It consumes immutable episode and
verification receipts only after the model process is terminal, seals all four
episode outcomes out-of-band, and invokes the official SWE-bench evaluator once
for each verifier-authorized non-empty patch.  Noncompliant, incomplete, and
no-patch episodes receive immutable zero-invocation receipts.

Evaluator stdout, stderr, reports, and receipts are written only beneath the
registered evaluator output root and are never returned to a model process.
An exclusive registry marker is written before every authorized invocation;
failure or interruption therefore cannot be retried under the same seal.
"""

from __future__ import annotations

import argparse
import concurrent.futures
from datetime import datetime, timezone
import hashlib
import importlib.metadata
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any, Mapping


PLAN_SCHEMA = "issue825-official-evaluator-plan-v1"
SEAL_SCHEMA = "issue825-terminal-episode-seal-v1"
SEAL_SET_SCHEMA = "issue825-terminal-episode-seal-set-v1"
EVALUATION_SCHEMA = "issue825-official-evaluation-receipt-v1"
SKIP_SCHEMA = "issue825-official-evaluation-skip-v1"
FAILURE_SCHEMA = "issue825-official-evaluation-failure-v1"
BATCH_SCHEMA = "issue825-official-evaluation-batch-v1"
TOKEN_LEDGER_SCHEMA = "issue825-token-ledger-v3"
ALLOWED_ARMS = {"A", "T"}
ARM_POLICIES = {"A": "control", "T": "treatment"}
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
SCRIPT_PATH = Path(__file__).resolve()


class FailClosed(RuntimeError):
    """Raised when an immutable evaluator contract cannot be proved."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FailClosed(message)


def canonical_json_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("utf-8")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def validate_authoritative_selection(
    selection: Mapping[str, Any], registration_bytes: bytes
) -> None:
    require(selection.get("authoritative") is True, "selection is not authoritative")
    require(selection.get("state") == "selected_pre_model", "selection state is not selected_pre_model")
    require(
        selection.get("problem_statements_inspected_by_human_before_selection") is False,
        "selection permits prior human problem-statement inspection",
    )
    require(
        selection.get("gold_or_outcomes_inspected_before_selection") is False,
        "selection permits prior gold/outcome inspection",
    )
    require(
        selection.get("registration_sha256") == sha256_bytes(registration_bytes),
        "selection binds another registration",
    )


def write_exclusive(path: Path, data: bytes, mode: int = 0o444) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise
    path.chmod(mode)


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as exc:
        raise FailClosed(f"cannot read JSON {path}: {exc}") from exc


def strict_regular(path: Path, label: str) -> bytes:
    require(path.is_absolute(), f"{label} path must be absolute: {path}")
    require(path.is_file(), f"{label} is missing or not a file: {path}")
    require(not path.is_symlink(), f"{label} must not be a symlink: {path}")
    return path.read_bytes()


def validate_file_reference(
    reference: Any, label: str, expected_root: Path | None = None
) -> tuple[Path, bytes]:
    require(
        isinstance(reference, dict)
        and set(reference) == {"path", "bytes", "sha256"},
        f"{label} reference schema mismatch",
    )
    path = Path(reference["path"])
    data = strict_regular(path, label)
    if expected_root is not None:
        require(
            is_relative_to(path.resolve(strict=True), expected_root.resolve(strict=True)),
            f"{label} is outside its registered root: {path}",
        )
    require(type(reference["bytes"]) is int, f"{label} byte count type mismatch")
    require(reference["bytes"] == len(data), f"{label} byte count mismatch")
    require(
        isinstance(reference["sha256"], str)
        and HEX64.fullmatch(reference["sha256"]) is not None,
        f"{label} digest shape mismatch",
    )
    require(reference["sha256"] == sha256_bytes(data), f"{label} digest mismatch")
    return path, data


def file_reference(path: Path) -> dict[str, Any]:
    data = strict_regular(path, "referenced file")
    return {"path": str(path), "bytes": len(data), "sha256": sha256_bytes(data)}


def is_relative_to(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def run_bytes(
    command: list[str],
    *,
    cwd: Path | None = None,
    environment: Mapping[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        raise FailClosed(
            f"command failed ({result.returncode}): {command!r}: "
            f"{result.stderr.decode(errors='replace').strip()}"
        )
    return result


def distribution_lock() -> bytes:
    values = sorted(
        (
            distribution.metadata.get("Name", "").lower(),
            distribution.version,
        )
        for distribution in importlib.metadata.distributions()
    )
    return canonical_json_bytes(values)


def _require_exact_keys(value: Any, expected: set[str], label: str) -> Mapping[str, Any]:
    require(isinstance(value, dict), f"{label} must be a JSON object")
    require(set(value) == expected, f"{label} keys differ: {sorted(set(value) ^ expected)}")
    return value


def _normalize_policy(value: Any, arm: str) -> str:
    require(isinstance(value, str), "episode policy must be a string")
    normalized = {"A": "control", "T": "treatment"}.get(value, value)
    require(normalized == ARM_POLICIES[arm], f"episode policy/arm mismatch: {arm}/{value}")
    return normalized


def _same_ref(left: Any, right: Any, label: str) -> None:
    require(left == right, f"{label} references differ")


def validate_episode_input(
    plan: Mapping[str, Any], episode: Mapping[str, Any]
) -> dict[str, Any]:
    model_root = Path(plan["model_output_root"])
    receipt_path, receipt_bytes = validate_file_reference(
        episode["episode_receipt"], "episode receipt", model_root
    )
    verification_path, verification_bytes = validate_file_reference(
        episode["episode_verification"], "episode verification", model_root
    )
    receipt = json.loads(receipt_bytes)
    verification = json.loads(verification_bytes)
    require(
        receipt.get("schema_version") == "issue825-episode-receipt-v1",
        "episode receipt schema mismatch",
    )
    require(
        verification.get("schema_version") == "issue825-episode-verification-v1",
        "episode verification schema mismatch",
    )
    receipt_identity = {
        "case_id": episode["case_id"],
        "rank": episode["rank"],
        "arm": episode["arm"],
        "base_commit": episode["base_commit"],
        "base_tree": episode["base_tree"],
    }
    for key, expected in receipt_identity.items():
        require(receipt.get(key) == expected, f"episode receipt {key} mismatch")
    for key in ("case_id", "rank", "arm"):
        expected = receipt_identity[key]
        require(verification.get(key) == expected, f"episode verification {key} mismatch")
    policy = _normalize_policy(receipt.get("policy"), episode["arm"])
    require(policy == ARM_POLICIES[episode["arm"]], "episode policy mismatch")
    require(
        isinstance(receipt.get("session_id"), str) and receipt["session_id"],
        "episode session identity missing",
    )
    expected_receipt_ref = {
        "path": str(receipt_path),
        "bytes": len(receipt_bytes),
        "sha256": sha256_bytes(receipt_bytes),
    }
    require(
        verification.get("episode_receipt") == expected_receipt_ref,
        "verification does not bind the complete episode receipt",
    )
    for key in (
        "evidence_complete",
        "policy_compliant",
        "evaluator_authorized",
        "official_evaluator_invoked",
    ):
        require(type(receipt.get(key)) is bool, f"episode receipt {key} must be boolean")
        require(type(verification.get(key)) is bool, f"episode verification {key} must be boolean")
    for key in ("evidence_complete", "policy_compliant", "evaluator_authorized"):
        require(
            verification[key] is not True or receipt[key] is True,
            f"episode verifier improperly promotes {key}",
        )
    require(
        receipt["official_evaluator_invoked"] is False
        and verification["official_evaluator_invoked"] is False,
        "episode reports evaluator feedback before terminal sealing",
    )

    receipt_patch = receipt.get("terminal_patch")
    verification_patch = verification.get("terminal_patch")
    _same_ref(receipt_patch, verification_patch, "terminal patch")
    patch_path: Path | None = None
    patch_bytes = b""
    patch_ref: dict[str, Any] | None = None
    if receipt_patch is not None:
        patch_path, patch_bytes = validate_file_reference(
            receipt_patch, "terminal patch", model_root
        )
        patch_ref = {
            "path": str(patch_path),
            "bytes": len(patch_bytes),
            "sha256": sha256_bytes(patch_bytes),
        }
    terminal_sha = verification.get("terminal_patch_sha256")
    require(
        terminal_sha in (None, sha256_bytes(patch_bytes)),
        "verification terminal patch digest mismatch",
    )

    evidence_complete = verification["evidence_complete"]
    policy_compliant = verification["policy_compliant"]
    evaluator_authorized = verification["evaluator_authorized"]
    if not evidence_complete:
        disposition = "incomplete_evidence"
        require(not evaluator_authorized, "incomplete episode authorized an evaluator")
    elif not policy_compliant:
        disposition = "noncompliant"
        require(not evaluator_authorized, "noncompliant episode authorized an evaluator")
    elif not patch_bytes:
        disposition = "no_patch"
        require(not evaluator_authorized, "no-patch episode authorized an evaluator")
    else:
        disposition = "evaluate"
        require(evaluator_authorized, "compliant terminal patch lacks evaluator authorization")

    token_ledger = receipt.get("token_ledger")
    require(isinstance(token_ledger, dict), "episode token ledger missing")
    require(
        token_ledger.get("schema_version") == TOKEN_LEDGER_SCHEMA
        and token_ledger.get("valid") is True
        and token_ledger.get("errors") == [],
        "episode token ledger v3 invalid",
    )
    timing = receipt.get("timing_ledger")
    require(isinstance(timing, dict), "episode timing ledger missing")
    if token_ledger.get("model_invoked") is False:
        require(
            disposition == "noncompliant"
            and receipt.get("returncode") is None
            and timing.get("model_wall_seconds") == 0
            and receipt.get("policy_compliant") is False
            and receipt.get("evaluator_authorized") is False,
            "no-model token ledger is not confined to a pre-model noncompliant episode",
        )
        require(
            token_ledger.get("source") == "model_not_invoked"
            and token_ledger.get("cli_turns") == 0
            and token_ledger.get("provider_responses") == 0
            and token_ledger.get("provider_requests") == 0
            and all(
                token_ledger.get(key) is None
                for key in (
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "output_tokens",
                    "provider_total_tokens",
                    "reasoning_tokens",
                )
            ),
            "no-model token ledger must record null provider counters",
        )
    else:
        require(
            token_ledger.get("model_invoked") is True,
            "episode token ledger model invocation state invalid",
        )
        for key in (
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "output_tokens",
            "provider_total_tokens",
            "reasoning_tokens",
            "cli_turns",
        ):
            require(
                type(token_ledger.get(key)) is int and token_ledger[key] >= 0,
                f"episode token ledger {key} invalid",
            )
        require(
            token_ledger.get("provider_responses") is None
            or (
                type(token_ledger["provider_responses"]) is int
                and token_ledger["provider_responses"] >= 0
            ),
            "episode token ledger provider_responses invalid",
        )
        require(
            token_ledger.get("provider_requests") is None,
            "episode token ledger must not conflate CLI turns with provider requests",
        )
        require(
            token_ledger["provider_total_tokens"]
            == token_ledger["input_tokens"]
            + token_ledger["cache_creation_input_tokens"]
            + token_ledger["cache_read_input_tokens"]
            + token_ledger["output_tokens"],
            "episode token total double-counts or omits provider tokens",
        )
    if evaluator_authorized:
        require(
            token_ledger.get("model_invoked") is True,
            "authorized patch lacks full model token evidence",
        )
    require(verification.get("token_ledger") == token_ledger, "verifier token ledger mismatch")
    require(verification.get("timing_ledger") == timing, "verifier timing ledger mismatch")
    for key in (
        "model_wall_seconds",
        "rna_preprocessing_seconds",
        "combined_pre_evaluator_wall_seconds",
    ):
        require(
            type(timing.get(key)) in (int, float) and timing[key] >= 0,
            f"episode timing ledger {key} invalid",
        )
    require(
        abs(
            timing["combined_pre_evaluator_wall_seconds"]
            - timing["model_wall_seconds"]
            - timing["rna_preprocessing_seconds"]
        )
        <= 1e-6,
        "combined pre-evaluator wall time is not RNA plus model wall",
    )
    if episode["arm"] == "A":
        require(
            timing["rna_preprocessing_seconds"] == 0,
            "control arm reports RNA preprocessing time",
        )

    return {
        "episode": dict(episode),
        "receipt_path": receipt_path,
        "receipt_bytes": receipt_bytes,
        "receipt": receipt,
        "verification_path": verification_path,
        "verification_bytes": verification_bytes,
        "verification": verification,
        "patch_path": patch_path,
        "patch_bytes": patch_bytes,
        "patch_ref": patch_ref,
        "disposition": disposition,
    }


def validate_output_isolation(plan: Mapping[str, Any]) -> None:
    roots = {
        "evidence": Path(plan["evidence_root"]),
        "model": Path(plan["model_output_root"]),
        "output": Path(plan["output_root"]),
        "registry": Path(plan["registry_root"]),
    }
    for label, path in roots.items():
        require(path.is_absolute(), f"{label} root must be absolute")
    for label in ("model", "output", "registry"):
        require(
            is_relative_to(roots[label].resolve(strict=False), roots["evidence"].resolve(strict=False)),
            f"{label} root must be inside the registered evidence root",
        )
    pairs = (("model", "output"), ("model", "registry"), ("output", "registry"))
    for left, right in pairs:
        left_path = roots[left].resolve(strict=False)
        right_path = roots[right].resolve(strict=False)
        require(
            not is_relative_to(left_path, right_path)
            and not is_relative_to(right_path, left_path),
            f"isolated roots overlap: {left}/{right}",
        )


def validate_plan(path: Path) -> dict[str, Any]:
    require(path.is_absolute(), "plan path must be absolute")
    plan_bytes = strict_regular(path, "evaluator plan")
    plan = read_json(path)
    _require_exact_keys(
        plan,
        {
            "schema_version",
            "registration",
            "selection",
            "evidence_root",
            "model_output_root",
            "output_root",
            "registry_root",
            "max_parallel",
            "evaluator_wall_seconds",
            "evaluator",
            "episodes",
        },
        "plan",
    )
    require(plan["schema_version"] == PLAN_SCHEMA, "plan schema mismatch")
    require(plan["max_parallel"] == 2, "outer evaluator parallelism must be 2")
    require(plan["evaluator_wall_seconds"] == 3600, "evaluator wall limit drift")
    validate_output_isolation(plan)

    registration_path, registration_bytes = validate_file_reference(
        plan["registration"], "registration"
    )
    selection_path, selection_bytes = validate_file_reference(plan["selection"], "selection")
    registration = json.loads(registration_bytes)
    selection = json.loads(selection_bytes)
    require(registration.get("issue") == 825, "registration issue mismatch")
    require(
        registration.get("schema_version") == "issue825-treatment-registration-v3",
        "registration schema mismatch",
    )
    require(
        selection.get("schema_version") == "issue825-fresh-pair-selection-v2",
        "selection schema mismatch",
    )
    validate_authoritative_selection(selection, registration_bytes)

    evaluator = _require_exact_keys(
        plan["evaluator"],
        {
            "python",
            "python_realpath",
            "python_sha256",
            "swebench_version",
            "swebench_record",
            "swebench_record_sha256",
            "run_evaluation",
            "run_evaluation_sha256",
            "distribution_lock_sha256",
            "dataset_name",
            "dataset_split",
            "dataset_cache_root",
            "dataset_arrow",
            "dataset_arrow_sha256",
            "dataset_info",
            "dataset_info_sha256",
            "docker_server",
        },
        "evaluator",
    )
    for key in (
        "python_sha256",
        "swebench_record_sha256",
        "run_evaluation_sha256",
        "distribution_lock_sha256",
        "dataset_arrow_sha256",
        "dataset_info_sha256",
    ):
        require(
            isinstance(evaluator[key], str) and HEX64.fullmatch(evaluator[key]) is not None,
            f"evaluator {key} is not SHA-256",
        )
    require(
        evaluator["dataset_arrow_sha256"]
        == registration.get("dataset", {}).get("arrow_sha256"),
        "evaluator dataset differs from registration",
    )
    require(evaluator["dataset_split"] == "test", "dataset split drift")

    episodes = plan["episodes"]
    require(isinstance(episodes, list) and len(episodes) == 4, "exactly four episodes required")
    selection_cases = selection.get("cases")
    require(isinstance(selection_cases, list) and len(selection_cases) == 2, "selection must contain two cases")
    selected = {case["instance_id"]: case for case in selection_cases}
    require(len(selected) == 2, "selection contains duplicate cases")
    seen: set[tuple[str, str]] = set()
    run_ids: set[str] = set()
    validated: list[dict[str, Any]] = []
    episode_keys = {
        "case_id",
        "rank",
        "arm",
        "base_commit",
        "base_tree",
        "episode_receipt",
        "episode_verification",
        "model_name_or_path",
        "run_id",
        "official_image",
        "official_image_source",
        "official_image_manifest_digest",
        "official_image_config_id",
        "official_image_local_id",
        "official_image_tag",
    }
    for index, episode in enumerate(episodes):
        episode = _require_exact_keys(episode, episode_keys, f"episodes[{index}]")
        arm = episode["arm"]
        require(arm in ALLOWED_ARMS, f"unknown episode arm: {arm}")
        require(type(episode["rank"]) is int and episode["rank"] in (1, 2), "bad rank")
        require(HEX40.fullmatch(episode["base_commit"]) is not None, "bad base commit")
        require(HEX40.fullmatch(episode["base_tree"]) is not None, "bad base tree")
        key = (episode["case_id"], arm)
        require(key not in seen, f"duplicate episode: {key}")
        seen.add(key)
        require(episode["run_id"] not in run_ids, "duplicate evaluator run_id")
        run_ids.add(episode["run_id"])
        selected_case = selected.get(episode["case_id"])
        require(selected_case is not None, "episode is outside frozen selection")
        require(selected_case.get("rank") == episode["rank"], "episode rank mismatch")
        require(selected_case.get("base_commit") == episode["base_commit"], "episode base mismatch")
        require(selected_case.get("base_tree") == episode["base_tree"], "episode tree mismatch")
        require(episode["official_image"].endswith(":" + episode["official_image_tag"]), "image tag mismatch")
        for digest_key in (
            "official_image_manifest_digest",
            "official_image_config_id",
            "official_image_local_id",
        ):
            require(
                re.fullmatch(r"sha256:[0-9a-f]{64}", episode[digest_key]) is not None,
                f"bad {digest_key}",
            )
        require(
            episode["official_image_local_id"]
            == episode["official_image_config_id"],
            "local Docker image ID differs from registered manifest config ID",
        )
        validated.append(validate_episode_input(plan, episode))
    expected = {(case_id, arm) for case_id in selected for arm in ALLOWED_ARMS}
    require(seen == expected, "plan does not contain both A/T episodes for both cases")

    plan["_path"] = str(path)
    plan["_bytes"] = plan_bytes
    plan["_sha256"] = sha256_bytes(plan_bytes)
    plan["_registration_path"] = registration_path
    plan["_registration_bytes"] = registration_bytes
    plan["_registration"] = registration
    plan["_selection_path"] = selection_path
    plan["_selection_bytes"] = selection_bytes
    plan["_selection"] = selection
    plan["_validated_episodes"] = validated
    return plan


def terminal_set_digest(validated: list[Mapping[str, Any]]) -> str:
    members = []
    for item in validated:
        episode = item["episode"]
        members.append(
            {
                "case_id": episode["case_id"],
                "rank": episode["rank"],
                "arm": episode["arm"],
                "base_commit": episode["base_commit"],
                "base_tree": episode["base_tree"],
                "episode_receipt_sha256": sha256_bytes(item["receipt_bytes"]),
                "episode_verification_sha256": sha256_bytes(item["verification_bytes"]),
                "terminal_patch_sha256": sha256_bytes(item["patch_bytes"]),
                "disposition": item["disposition"],
                "evaluator_authorized": item["verification"]["evaluator_authorized"],
            }
        )
    members.sort(key=lambda value: (value["rank"], value["arm"]))
    return sha256_bytes(canonical_json_bytes(members))


def arm_slug(episode: Mapping[str, Any]) -> str:
    return episode["arm"]


def evidence_identity(plan: Mapping[str, Any]) -> str:
    return sha256_bytes(
        canonical_json_bytes(
            {
                "evidence_root": plan["evidence_root"],
                "model_output_root": plan["model_output_root"],
                "output_root": plan["output_root"],
                "registry_root": plan["registry_root"],
                "registration_sha256": sha256_bytes(plan["_registration_bytes"]),
                "selection_sha256": sha256_bytes(plan["_selection_bytes"]),
            }
        )
    )


def _copy_sealed(directory: Path, name: str, data: bytes) -> dict[str, Any]:
    path = directory / name
    write_exclusive(path, data)
    return {"path": name, "bytes": len(data), "sha256": sha256_bytes(data)}


def seal_all(plan: Mapping[str, Any]) -> Path:
    output_root = Path(plan["output_root"])
    registry_root = Path(plan["registry_root"])
    require(not output_root.exists(), "evaluator output root already exists")
    output_root.parent.mkdir(parents=True, exist_ok=True)
    registry_root.mkdir(parents=True, exist_ok=True)
    validated = list(plan["_validated_episodes"])
    set_digest = terminal_set_digest(validated)
    claim_path = registry_root / f"terminal-set-{set_digest}.sealed.json"
    claim = {
        "schema_version": "issue825-irrevocable-seal-claim-v1",
        "claimed_at": utc_now(),
        "terminal_set_digest": set_digest,
        "plan_sha256": plan["_sha256"],
        "script_sha256": sha256_file(SCRIPT_PATH),
        "evidence_identity": evidence_identity(plan),
        "official_evaluator_invocations": 0,
    }
    write_exclusive(claim_path, canonical_json_bytes(claim))

    temporary = Path(tempfile.mkdtemp(prefix=output_root.name + ".sealing-", dir=output_root.parent))
    seal_refs: list[dict[str, Any]] = []
    try:
        sealed_root = temporary / "sealed"
        for item in validated:
            episode = item["episode"]
            directory = sealed_root / episode["case_id"] / arm_slug(episode)
            directory.mkdir(parents=True, exist_ok=False)
            receipt_ref = _copy_sealed(directory, "episode-receipt.json", item["receipt_bytes"])
            verification_ref = _copy_sealed(
                directory, "episode-verification.json", item["verification_bytes"]
            )
            patch_ref = None
            prediction_ref = None
            if item["patch_path"] is not None:
                patch_ref = _copy_sealed(directory, "terminal.patch", item["patch_bytes"])
            if item["disposition"] == "evaluate":
                prediction = canonical_json_bytes(
                    {
                        "instance_id": episode["case_id"],
                        "model_patch": item["patch_bytes"].decode("utf-8"),
                        "model_name_or_path": episode["model_name_or_path"],
                    }
                )
                prediction_ref = _copy_sealed(directory, "prediction.jsonl", prediction)
            seal = {
                "schema_version": SEAL_SCHEMA,
                "sealed_at": utc_now(),
                "case_id": episode["case_id"],
                "rank": episode["rank"],
                "arm": episode["arm"],
                "base_commit": episode["base_commit"],
                "base_tree": episode["base_tree"],
                "model_name_or_path": episode["model_name_or_path"],
                "run_id": episode["run_id"],
                "plan": {
                    "path": plan["_path"],
                    "bytes": len(plan["_bytes"]),
                    "sha256": plan["_sha256"],
                },
                "script_sha256": sha256_file(SCRIPT_PATH),
                "registration_sha256": sha256_bytes(plan["_registration_bytes"]),
                "selection_sha256": sha256_bytes(plan["_selection_bytes"]),
                "evidence_identity": evidence_identity(plan),
                "terminal_set_digest": set_digest,
                "source_episode_receipt": dict(episode["episode_receipt"]),
                "source_episode_verification": dict(episode["episode_verification"]),
                "sealed_episode_receipt": receipt_ref,
                "sealed_episode_verification": verification_ref,
                "terminal_patch": patch_ref,
                "prediction": prediction_ref,
                "disposition": item["disposition"],
                "evaluator_authorized": item["verification"]["evaluator_authorized"],
                "official_evaluator_invocations": 0,
            }
            seal_bytes = canonical_json_bytes(seal)
            write_exclusive(directory / "seal.json", seal_bytes)
            seal_refs.append(
                {
                    "case_id": episode["case_id"],
                    "rank": episode["rank"],
                    "arm": episode["arm"],
                    "disposition": item["disposition"],
                    "sha256": sha256_bytes(seal_bytes),
                }
            )
        seal_refs.sort(key=lambda value: (value["rank"], value["arm"]))
        seal_set = {
            "schema_version": SEAL_SET_SCHEMA,
            "sealed_at": utc_now(),
            "plan": {
                "path": plan["_path"],
                "bytes": len(plan["_bytes"]),
                "sha256": plan["_sha256"],
            },
            "script_sha256": sha256_file(SCRIPT_PATH),
            "registration_sha256": sha256_bytes(plan["_registration_bytes"]),
            "selection_sha256": sha256_bytes(plan["_selection_bytes"]),
            "evidence_identity": evidence_identity(plan),
            "terminal_set_digest": set_digest,
            "registry_claim": file_reference(claim_path),
            "seals": seal_refs,
            "official_evaluations_authorized": sum(
                ref["disposition"] == "evaluate" for ref in seal_refs
            ),
            "official_evaluator_invocations": 0,
        }
        write_exclusive(temporary / "seal-set.json", canonical_json_bytes(seal_set))
        os.rename(temporary, output_root)
        for directory in sorted((output_root / "sealed").rglob("*"), reverse=True):
            if directory.is_dir():
                directory.chmod(0o555)
        (output_root / "sealed").chmod(0o555)
        return output_root
    except BaseException:
        # An interrupted seal is retained for diagnosis and is never silently reused.
        raise


def validate_seal(plan: Mapping[str, Any], episode: Mapping[str, Any], set_digest: str) -> dict[str, Any]:
    root = Path(plan["output_root"])
    directory = root / "sealed" / episode["case_id"] / arm_slug(episode)
    seal_path = directory / "seal.json"
    seal_bytes = strict_regular(seal_path, "episode seal")
    seal = json.loads(seal_bytes)
    require(seal.get("schema_version") == SEAL_SCHEMA, "seal schema mismatch")
    for key in ("case_id", "rank", "arm", "base_commit", "base_tree", "model_name_or_path", "run_id"):
        require(seal.get(key) == episode[key], f"seal {key} mismatch")
    require(seal.get("terminal_set_digest") == set_digest, "seal set digest mismatch")
    require(seal.get("plan", {}).get("sha256") == plan["_sha256"], "seal plan mismatch")
    require(seal.get("script_sha256") == sha256_file(SCRIPT_PATH), "seal script mismatch")
    require(
        seal.get("registration_sha256") == sha256_bytes(plan["_registration_bytes"])
        and seal.get("selection_sha256") == sha256_bytes(plan["_selection_bytes"])
        and seal.get("evidence_identity") == evidence_identity(plan),
        "seal frozen identity mismatch",
    )
    require(seal.get("official_evaluator_invocations") == 0, "seal reports evaluation")
    validated_matches = [
        item
        for item in plan["_validated_episodes"]
        if item["episode"]["case_id"] == episode["case_id"]
        and item["episode"]["arm"] == episode["arm"]
    ]
    require(len(validated_matches) == 1, "validated episode identity is ambiguous")
    validated = validated_matches[0]
    require(seal.get("disposition") == validated["disposition"], "seal disposition drift")
    require(
        seal.get("evaluator_authorized")
        == validated["verification"]["evaluator_authorized"],
        "seal evaluator authorization drift",
    )
    require(
        seal.get("source_episode_receipt") == episode["episode_receipt"]
        and seal.get("source_episode_verification") == episode["episode_verification"],
        "seal source references drift",
    )
    for key in ("sealed_episode_receipt", "sealed_episode_verification"):
        reference = seal[key]
        path = directory / reference["path"]
        data = strict_regular(path, f"sealed {key}")
        require(len(data) == reference["bytes"], f"sealed {key} size changed")
        require(sha256_bytes(data) == reference["sha256"], f"sealed {key} changed")
    require(
        seal["sealed_episode_receipt"]["bytes"] == len(validated["receipt_bytes"])
        and seal["sealed_episode_receipt"]["sha256"]
        == sha256_bytes(validated["receipt_bytes"]),
        "sealed episode receipt differs from source",
    )
    require(
        seal["sealed_episode_verification"]["bytes"]
        == len(validated["verification_bytes"])
        and seal["sealed_episode_verification"]["sha256"]
        == sha256_bytes(validated["verification_bytes"]),
        "sealed episode verification differs from source",
    )
    patch_ref = seal.get("terminal_patch")
    prediction_ref = seal.get("prediction")
    if patch_ref is None:
        require(not validated["patch_bytes"], "seal omitted a source terminal patch")
        require(prediction_ref is None, "prediction exists without a patch")
    else:
        patch = strict_regular(directory / patch_ref["path"], "sealed patch")
        require(len(patch) == patch_ref["bytes"] and sha256_bytes(patch) == patch_ref["sha256"], "sealed patch changed")
        require(patch == validated["patch_bytes"], "sealed patch differs from source")
        if seal["disposition"] == "evaluate":
            prediction = strict_regular(directory / prediction_ref["path"], "sealed prediction")
            require(len(prediction) == prediction_ref["bytes"] and sha256_bytes(prediction) == prediction_ref["sha256"], "sealed prediction changed")
            expected_prediction = canonical_json_bytes(
                {
                    "instance_id": episode["case_id"],
                    "model_patch": validated["patch_bytes"].decode("utf-8"),
                    "model_name_or_path": episode["model_name_or_path"],
                }
            )
            require(prediction == expected_prediction, "sealed prediction differs from source")
        else:
            require(prediction_ref is None, "skipped disposition contains a prediction")
    return {"path": seal_path, "bytes": seal_bytes, "sha256": sha256_bytes(seal_bytes), "seal": seal}


def validate_seal_set(plan: Mapping[str, Any]) -> dict[str, Any]:
    root = Path(plan["output_root"])
    path = root / "seal-set.json"
    data = strict_regular(path, "seal set")
    value = json.loads(data)
    require(value.get("schema_version") == SEAL_SET_SCHEMA, "seal-set schema mismatch")
    require(value.get("plan", {}).get("sha256") == plan["_sha256"], "seal-set plan mismatch")
    require(value.get("script_sha256") == sha256_file(SCRIPT_PATH), "seal-set script mismatch")
    set_digest = value.get("terminal_set_digest")
    require(isinstance(set_digest, str) and HEX64.fullmatch(set_digest) is not None, "bad terminal-set digest")
    require(
        set_digest == terminal_set_digest(plan["_validated_episodes"]),
        "terminal-set digest is not reconstructible from frozen inputs",
    )
    seals = [validate_seal(plan, episode, set_digest) for episode in plan["episodes"]]
    refs = sorted(
        (
            {
                "case_id": item["seal"]["case_id"],
                "rank": item["seal"]["rank"],
                "arm": item["seal"]["arm"],
                "disposition": item["seal"]["disposition"],
                "sha256": item["sha256"],
            }
            for item in seals
        ),
        key=lambda item: (item["rank"], item["arm"]),
    )
    require(value.get("seals") == refs, "seal-set member references changed")
    require(
        value.get("official_evaluations_authorized")
        == sum(item["disposition"] == "evaluate" for item in refs),
        "seal-set evaluator authorization count mismatch",
    )
    claim_path, claim_bytes = validate_file_reference(value["registry_claim"], "seal registry claim")
    claim = json.loads(claim_bytes)
    require(claim_path.parent == Path(plan["registry_root"]), "seal claim registry drift")
    require(claim.get("terminal_set_digest") == set_digest, "seal claim digest mismatch")
    return {
        "path": path,
        "bytes": data,
        "sha256": sha256_bytes(data),
        "terminal_set_digest": set_digest,
        "seals": seals,
        "value": value,
    }


def validate_dataset_rows(plan: Mapping[str, Any], dataset: Path) -> dict[str, str]:
    """Read only instance/base identity; never expose problem, tests, or gold columns."""
    try:
        import pyarrow as pa  # type: ignore
        import pyarrow.ipc as ipc  # type: ignore
    except ImportError as exc:
        raise FailClosed("pyarrow is required to verify the frozen evaluator dataset") from exc

    expected = {episode["case_id"]: episode["base_commit"] for episode in plan["episodes"]}
    observed: dict[str, list[str]] = {case_id: [] for case_id in expected}
    with pa.memory_map(str(dataset), "r") as source:
        reader = ipc.open_stream(source)
        names = reader.schema.names
        require("instance_id" in names and "base_commit" in names, "dataset identity columns missing")
        instance_index = names.index("instance_id")
        base_index = names.index("base_commit")
        for batch in reader:
            instance_ids = batch.column(instance_index).to_pylist()
            bases = batch.column(base_index).to_pylist()
            for instance_id, base_commit in zip(instance_ids, bases, strict=True):
                if instance_id in observed:
                    observed[instance_id].append(base_commit)
    for case_id, base_commit in expected.items():
        require(observed[case_id] == [base_commit], f"dataset row/base mismatch: {case_id}")
    return {case_id: values[0] for case_id, values in observed.items()}


def inspect_pinned_image(episode: Mapping[str, Any]) -> dict[str, Any]:
    result = run_bytes(["docker", "image", "inspect", episode["official_image"]])
    images = json.loads(result.stdout)
    require(isinstance(images, list) and len(images) == 1, "Docker image inspect shape mismatch")
    image = images[0]
    require(image.get("Id") == episode["official_image_local_id"], "local image ID drift")
    require(image.get("Id") == episode["official_image_config_id"], "image config ID drift")
    require(episode["official_image"] in (image.get("RepoTags") or []), "official image tag absent")
    return {
        "image": episode["official_image"],
        "id": image["Id"],
        "repo_tags": sorted(image.get("RepoTags") or []),
        "repo_digests": sorted(image.get("RepoDigests") or []),
    }


def validate_static_environment(plan: Mapping[str, Any], *, include_docker: bool) -> dict[str, Any]:
    evaluator = plan["evaluator"]
    python = Path(evaluator["python"])
    real_python = Path(evaluator["python_realpath"])
    strict_regular(real_python, "evaluator Python")
    require(python.resolve(strict=True) == real_python, "evaluator Python realpath drift")
    require(sha256_file(real_python) == evaluator["python_sha256"], "evaluator Python digest drift")
    require(sys.executable == str(python), f"invoke with registered evaluator Python: {python}")
    require(importlib.metadata.version("swebench") == evaluator["swebench_version"], "swebench version drift")
    entrypoint_spec = importlib.util.find_spec("swebench.harness.run_evaluation")
    require(
        entrypoint_spec is not None
        and entrypoint_spec.origin is not None
        and Path(entrypoint_spec.origin).resolve(strict=True)
        == Path(evaluator["run_evaluation"]).resolve(strict=True),
        "import-resolved evaluator entrypoint differs from registration",
    )
    for path_key, digest_key, label in (
        ("swebench_record", "swebench_record_sha256", "swebench RECORD"),
        ("run_evaluation", "run_evaluation_sha256", "official evaluator entrypoint"),
        ("dataset_arrow", "dataset_arrow_sha256", "dataset Arrow"),
        ("dataset_info", "dataset_info_sha256", "dataset info"),
    ):
        path = Path(evaluator[path_key])
        strict_regular(path, label)
        require(sha256_file(path) == evaluator[digest_key], f"{label} digest drift")
    dataset_cache = Path(evaluator["dataset_cache_root"])
    require(
        dataset_cache.is_absolute()
        and dataset_cache.is_dir()
        and not dataset_cache.is_symlink(),
        "registered dataset cache root is missing or unsafe",
    )
    require(
        is_relative_to(
            Path(evaluator["dataset_arrow"]).resolve(strict=True),
            dataset_cache.resolve(strict=True),
        ),
        "frozen dataset Arrow is outside the registered dataset cache",
    )
    require(
        sha256_bytes(distribution_lock()) == evaluator["distribution_lock_sha256"],
        "evaluator distribution lock drift",
    )
    dataset_rows = validate_dataset_rows(plan, Path(evaluator["dataset_arrow"]))
    result: dict[str, Any] = {
        "python_sha256": evaluator["python_sha256"],
        "swebench_version": evaluator["swebench_version"],
        "run_evaluation_sha256": evaluator["run_evaluation_sha256"],
        "dataset_arrow_sha256": evaluator["dataset_arrow_sha256"],
        "dataset_rows": dataset_rows,
        "official_evaluator_invocations": 0,
    }
    if include_docker:
        server = run_bytes(
            [
                "docker",
                "version",
                "--format",
                "{{.Server.Version}}|{{.Server.GitCommit}}|{{.Server.Os}}|{{.Server.Arch}}",
            ]
        ).stdout.decode().strip()
        require(server == evaluator["docker_server"], "Docker server identity drift")
        images: dict[str, Any] = {}
        for episode in plan["episodes"]:
            images[f"{episode['case_id']}:{episode['arm']}"] = inspect_pinned_image(episode)
        result["docker_server"] = server
        result["official_images"] = images
    return result


def evaluator_command(
    plan: Mapping[str, Any], episode: Mapping[str, Any], predictions: Path
) -> list[str]:
    evaluator = plan["evaluator"]
    return [
        evaluator["python"],
        "-I",
        "-m",
        "swebench.harness.run_evaluation",
        "--dataset_name",
        evaluator["dataset_name"],
        "--split",
        evaluator["dataset_split"],
        "--predictions_path",
        str(predictions),
        "--max_workers",
        "1",
        "--instance_image_tag",
        episode["official_image_tag"],
        "--run_id",
        episode["run_id"],
        "--instance_ids",
        episode["case_id"],
    ]


def no_live_model_sessions(plan: Mapping[str, Any]) -> dict[str, Any]:
    process_list = run_bytes(["ps", "-axo", "pid=,command="]).stdout.decode(
        errors="replace"
    )
    session_ids = sorted(
        item["receipt"]["session_id"] for item in plan["_validated_episodes"]
    )
    require(len(set(session_ids)) == 4, "episode session identities are not unique")
    for session_id in session_ids:
        require(session_id not in process_list, "a frozen episode model session is still live")
    return {"checked_session_count": len(session_ids), "all_absent": True}


def sanitized_environment(dataset_cache_root: str) -> tuple[dict[str, str], list[str]]:
    prefixes = ("ANTHROPIC", "CLAUDE", "OPENAI", "CODEX", "PYTHON")
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.upper().startswith(prefixes)
    }
    removed = sorted(key for key in os.environ if key not in environment)
    environment["HF_DATASETS_OFFLINE"] = "1"
    environment["HF_HUB_OFFLINE"] = "1"
    environment["TRANSFORMERS_OFFLINE"] = "1"
    environment["HF_DATASETS_CACHE"] = dataset_cache_root
    environment["PYTHONNOUSERSITE"] = "1"
    return environment, removed


def evaluator_container_name(episode: Mapping[str, Any]) -> str:
    return f"sweb.eval.{episode['case_id']}.{episode['run_id']}"


def ensure_container_absent(
    container_name: str, *, remove: bool, timeout_seconds: float = 30.0
) -> dict[str, Any]:
    started = time.monotonic()
    removal_attempted = False
    removal_returncode: int | None = None
    while True:
        inspect = run_bytes(["docker", "container", "inspect", container_name], check=False)
        if inspect.returncode != 0:
            diagnostic = (inspect.stderr + inspect.stdout).decode(errors="replace").lower()
            require(
                "no such container" in diagnostic or "no such object" in diagnostic,
                f"Docker inspect failed without proving absence: {container_name}",
            )
            return {
                "container": container_name,
                "absent": True,
                "removal_attempted": removal_attempted,
                "removal_returncode": removal_returncode,
                "wait_seconds": time.monotonic() - started,
            }
        if remove and not removal_attempted:
            removal = run_bytes(["docker", "rm", "-f", container_name], check=False)
            removal_attempted = True
            removal_returncode = removal.returncode
        if time.monotonic() - started >= timeout_seconds:
            raise FailClosed(f"Docker container remained after cleanup: {container_name}")
        time.sleep(0.25)


def monitor_container(
    container_name: str,
    expected_image: str,
    expected_image_id: str,
    stop: threading.Event,
    result: dict[str, Any],
) -> None:
    samples: list[dict[str, Any]] = []
    try:
        import docker  # type: ignore

        client = docker.from_env()
        container = None
        while not stop.is_set():
            try:
                container = client.containers.get(container_name)
                break
            except docker.errors.NotFound:
                time.sleep(0.25)
        if container is None:
            result.update({"samples": [], "error": "container not observed"})
            return
        image = container.image
        require(image.id == expected_image_id, "evaluator container image ID drift")
        require(expected_image in image.tags, "evaluator container image tag drift")
        result["image"] = {
            "id": image.id,
            "repo_tags": sorted(image.tags),
            "repo_digests": sorted(image.attrs.get("RepoDigests") or []),
        }
        peak = 0
        for sample in container.stats(stream=True, decode=True):
            usage = int(sample.get("memory_stats", {}).get("usage", 0) or 0)
            peak = max(peak, usage)
            samples.append(
                {
                    "read": sample.get("read"),
                    "memory_usage_bytes": usage,
                    "pids_current": sample.get("pids_stats", {}).get("current"),
                }
            )
            if stop.is_set():
                break
        result["peak_memory_bytes"] = peak
        result["samples"] = samples
    except BaseException as exc:
        result.update({"samples": samples, "error": f"{type(exc).__name__}: {exc}"})


def process_tree_rss_kib(root_pid: int) -> int:
    result = run_bytes(["ps", "-axo", "pid=,ppid=,rss="], check=False)
    if result.returncode != 0:
        return 0
    children: dict[int, list[int]] = {}
    rss: dict[int, int] = {}
    for line in result.stdout.decode(errors="replace").splitlines():
        fields = line.split()
        if len(fields) != 3:
            continue
        try:
            pid, parent, value = (int(field) for field in fields)
        except ValueError:
            continue
        children.setdefault(parent, []).append(pid)
        rss[pid] = value
    stack = [root_pid]
    seen: set[int] = set()
    total = 0
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        total += rss.get(pid, 0)
        stack.extend(children.get(pid, []))
    return total


def terminate_process_bounded(
    process: subprocess.Popen[bytes], timeout_seconds: float = 10.0
) -> int:
    if process.poll() is not None:
        return process.returncode
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        return process.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    try:
        return process.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired as exc:
        raise FailClosed(f"evaluator process survived SIGKILL: {process.pid}") from exc


def recursive_inventory(root: Path, excluded: set[Path]) -> list[dict[str, Any]]:
    inventory: list[dict[str, Any]] = []
    for path in sorted(root.rglob("*")):
        if not path.is_file() or path in excluded:
            continue
        require(not path.is_symlink(), f"evaluator emitted symlink: {path}")
        inventory.append(
            {
                "path": str(path.relative_to(root)),
                "bytes": path.stat().st_size,
                "sha256": sha256_file(path),
            }
        )
    return inventory


def evaluator_outputs(
    run_dir: Path, episode: Mapping[str, Any], sealed_patch: bytes
) -> tuple[dict[str, Any], dict[str, Any]]:
    log_root = (
        run_dir
        / "logs/run_evaluation"
        / episode["run_id"]
        / episode["model_name_or_path"]
        / episode["case_id"]
    )
    expected = {
        "eval_script": log_root / "eval.sh",
        "patch": log_root / "patch.diff",
        "report": log_root / "report.json",
        "run_log": log_root / "run_instance.log",
        "test_output": log_root / "test_output.txt",
    }
    refs: dict[str, Any] = {}
    for key, path in expected.items():
        data = strict_regular(path, f"official evaluator {key}")
        refs[key] = {
            "path": str(path.relative_to(run_dir)),
            "bytes": len(data),
            "sha256": sha256_bytes(data),
        }
    require(expected["patch"].read_bytes() == sealed_patch, "official patch differs from seal")
    summary_path = run_dir / f"{episode['model_name_or_path']}.{episode['run_id']}.json"
    summary_bytes = strict_regular(summary_path, "official evaluator summary")
    summary = json.loads(summary_bytes)
    require(summary.get("total_instances") == 1, "official total_instances mismatch")
    require(summary.get("submitted_instances") == 1, "official submitted_instances mismatch")
    require(summary.get("submitted_ids") == [episode["case_id"]], "official case mismatch")
    refs["summary"] = {
        "path": str(summary_path.relative_to(run_dir)),
        "bytes": len(summary_bytes),
        "sha256": sha256_bytes(summary_bytes),
    }
    report = read_json(expected["report"])
    require(isinstance(report, dict) and set(report) == {episode["case_id"]}, "report case mismatch")
    instance = report[episode["case_id"]]
    require(type(instance.get("resolved")) is bool, "official resolved flag missing")
    statuses = instance.get("tests_status")
    require(isinstance(statuses, dict), "official test statuses missing")
    test_lists = {
        "schema_version": "issue825-official-test-lists-v1",
        "case_id": episode["case_id"],
        "arm": episode["arm"],
        "resolved": instance["resolved"],
        "tests_status": statuses,
        "counts": {
            category: {outcome: len(tests) for outcome, tests in outcomes.items()}
            for category, outcomes in statuses.items()
        },
    }
    return refs, test_lists


def claim_evaluation(
    plan: Mapping[str, Any], seal_info: Mapping[str, Any]
) -> dict[str, Any]:
    seal = seal_info["seal"]
    require(seal["disposition"] == "evaluate", "cannot claim a skipped episode")
    registry = Path(plan["registry_root"])
    marker = registry / (
        f"{seal['case_id']}--{seal['arm']}--{seal_info['sha256']}.evaluation-started.json"
    )
    value = {
        "schema_version": "issue825-irrevocable-evaluation-start-v1",
        "started_at": utc_now(),
        "case_id": seal["case_id"],
        "arm": seal["arm"],
        "run_id": seal["run_id"],
        "plan_sha256": plan["_sha256"],
        "script_sha256": sha256_file(SCRIPT_PATH),
        "terminal_set_digest": seal["terminal_set_digest"],
        "seal_sha256": seal_info["sha256"],
        "official_evaluations_authorized": 1,
    }
    try:
        write_exclusive(marker, canonical_json_bytes(value))
    except FileExistsError as exc:
        raise FailClosed(
            f"official evaluator already claimed for {seal['case_id']} {seal['arm']}"
        ) from exc
    return file_reference(marker)


def write_skip_receipt(
    plan: Mapping[str, Any], seal_info: Mapping[str, Any]
) -> dict[str, Any]:
    seal = seal_info["seal"]
    require(seal["disposition"] != "evaluate", "evaluate disposition cannot be skipped")
    run_dir = Path(plan["output_root"]) / "evaluations" / seal["case_id"] / seal["arm"]
    run_dir.mkdir(parents=True, exist_ok=False)
    path = run_dir / "evaluation.skip.receipt.json"
    value = {
        "schema_version": SKIP_SCHEMA,
        "recorded_at": utc_now(),
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
        "disposition": seal["disposition"],
        "official_evaluator_invocation_authorized": False,
        "official_evaluator_invocation_confirmed": False,
        "official_evaluator_invocations": 0,
        "model_output_delivery": "none",
    }
    data = canonical_json_bytes(value)
    write_exclusive(path, data)
    return {
        "case_id": seal["case_id"],
        "rank": seal["rank"],
        "arm": seal["arm"],
        "disposition": seal["disposition"],
        "receipt": {"path": str(path), "bytes": len(data), "sha256": sha256_bytes(data)},
        "official_evaluator_invocation_authorized": False,
        "official_evaluator_invocation_confirmed": False,
        "valid": True,
    }


def write_case_poison_failure(
    plan: Mapping[str, Any],
    seal_info: Mapping[str, Any],
    poison: Mapping[str, Any],
) -> dict[str, Any]:
    seal = seal_info["seal"]
    run_dir = Path(plan["output_root"]) / "evaluations" / seal["case_id"] / seal["arm"]
    run_dir.mkdir(parents=True, exist_ok=False)
    path = run_dir / "evaluation.failure.receipt.json"
    value = {
        "schema_version": FAILURE_SCHEMA,
        "recorded_at": utc_now(),
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
        "error": "FailClosed: sibling evaluator isolation is poisoned",
        "case_poisoned": True,
        "blocked_by_case_poison": dict(poison),
        "official_evaluator_invocation_authorized": False,
        "official_evaluator_invocation_confirmed": False,
        "official_evaluator_invocations": 0,
        "valid_official_outputs": False,
        "inventory": [],
        "model_output_delivery": "none",
    }
    data = canonical_json_bytes(value)
    write_exclusive(path, data)
    return {
        "case_id": seal["case_id"],
        "rank": seal["rank"],
        "arm": seal["arm"],
        "disposition": "evaluate",
        "receipt": {"path": str(path), "bytes": len(data), "sha256": sha256_bytes(data)},
        "official_evaluator_invocation_authorized": False,
        "official_evaluator_invocation_confirmed": False,
        "valid": False,
    }


def evaluate_one_inner(
    plan: Mapping[str, Any],
    episode: Mapping[str, Any],
    seal_info: Mapping[str, Any],
    state: dict[str, Any],
) -> dict[str, Any]:
    seal = seal_info["seal"]
    run_dir = Path(plan["output_root"]) / "evaluations" / seal["case_id"] / seal["arm"]
    state["run_dir"] = run_dir
    run_dir.mkdir(parents=True, exist_ok=False)
    sealed_dir = seal_info["path"].parent
    prediction_source = sealed_dir / seal["prediction"]["path"]
    prediction_bytes = strict_regular(prediction_source, "sealed prediction")
    predictions = run_dir / "predictions.jsonl"
    write_exclusive(predictions, prediction_bytes)
    command = evaluator_command(plan, episode, predictions)
    container_name = evaluator_container_name(episode)
    preexisting = ensure_container_absent(container_name, remove=False)
    require(preexisting.get("absent") is True, "preexisting evaluator container")
    image = inspect_pinned_image(episode)
    claim = claim_evaluation(plan, seal_info)
    state["invocation_authorized"] = True
    state["claim"] = claim
    started_at = utc_now()
    started_monotonic = time.monotonic()
    state.update(
        {
            "command": command,
            "started_at": started_at,
            "started_monotonic": started_monotonic,
            "container_name": container_name,
        }
    )
    start_value = {
        "schema_version": "issue825-official-evaluation-start-v1",
        "started_at": started_at,
        "case_id": seal["case_id"],
        "arm": seal["arm"],
        "run_id": seal["run_id"],
        "terminal_set_digest": seal["terminal_set_digest"],
        "seal_sha256": seal_info["sha256"],
        "irrevocable_registry_start": claim,
        "command": command,
        "preexisting_container_check": preexisting,
        "official_evaluator_invocation_authorized": True,
    }
    write_exclusive(run_dir / "evaluation.started.json", canonical_json_bytes(start_value))
    environment, removed_keys = sanitized_environment(
        plan["evaluator"]["dataset_cache_root"]
    )
    stdout_path = run_dir / "evaluator.stdout"
    stderr_path = run_dir / "evaluator.stderr"
    stop = threading.Event()
    container_result: dict[str, Any] = {}
    state.update(
        {
            "monitor_stop": stop,
            "container_result": container_result,
            "peak_rss_kib": 0,
        }
    )
    monitor = threading.Thread(
        target=monitor_container,
        args=(
            container_name,
            episode["official_image"],
            episode["official_image_local_id"],
            stop,
            container_result,
        ),
        daemon=True,
    )
    state["monitor"] = monitor
    timed_out = False
    peak_rss = 0
    try:
        with stdout_path.open("xb") as stdout_handle, stderr_path.open("xb") as stderr_handle:
            process = subprocess.Popen(
                command,
                cwd=run_dir,
                env=environment,
                stdout=stdout_handle,
                stderr=stderr_handle,
                start_new_session=True,
            )
            state["process"] = process
            state["invocation_confirmed"] = True
            monitor.start()
            while process.poll() is None:
                peak_rss = max(peak_rss, process_tree_rss_kib(process.pid))
                state["peak_rss_kib"] = peak_rss
                if time.monotonic() - started_monotonic > plan["evaluator_wall_seconds"]:
                    timed_out = True
                    terminate_process_bounded(process)
                    break
                time.sleep(0.25)
            require(process.poll() is not None, "evaluator did not reach a terminal state")
            returncode = process.returncode
    except BaseException:
        process = state.get("process")
        if process is not None and callable(getattr(process, "poll", None)) and process.poll() is None:
            try:
                terminate_process_bounded(process)
            except BaseException as termination_exc:
                state["host_termination_error"] = (
                    f"{type(termination_exc).__name__}: {termination_exc}"
                )
        raise
    finally:
        stop.set()
        if monitor.is_alive():
            monitor.join(timeout=3)
    cleanup = ensure_container_absent(container_name, remove=True)
    require(cleanup.get("absent") is True, "evaluator container absence not proved")
    stdout_path.chmod(0o444)
    stderr_path.chmod(0o444)
    write_exclusive(run_dir / "container-monitor.json", canonical_json_bytes(container_result))

    patch = strict_regular(sealed_dir / seal["terminal_patch"]["path"], "sealed patch")
    outputs: dict[str, Any] = {}
    test_lists_ref: dict[str, Any] | None = None
    output_error: str | None = None
    valid_outputs = False
    try:
        outputs, test_lists = evaluator_outputs(run_dir, episode, patch)
        test_lists_data = canonical_json_bytes(test_lists)
        test_lists_path = run_dir / "test-lists.json"
        write_exclusive(test_lists_path, test_lists_data)
        test_lists_ref = {
            "path": str(test_lists_path.relative_to(run_dir)),
            "bytes": len(test_lists_data),
            "sha256": sha256_bytes(test_lists_data),
        }
        require(not container_result.get("error"), f"container monitor failed: {container_result.get('error')}")
        require(container_result.get("image", {}).get("id") == episode["official_image_local_id"], "container image proof mismatch")
        require(
            episode["official_image"]
            in container_result.get("image", {}).get("repo_tags", []),
            "container image tag proof mismatch",
        )
        require(
            type(container_result.get("peak_memory_bytes")) is int
            and container_result["peak_memory_bytes"] > 0,
            "container memory proof missing",
        )
        valid_outputs = returncode == 0 and not timed_out
    except BaseException as exc:
        output_error = f"{type(exc).__name__}: {exc}"

    receipt_path = run_dir / "evaluation.receipt.json"
    receipt = {
        "schema_version": EVALUATION_SCHEMA,
        "case_id": seal["case_id"],
        "rank": seal["rank"],
        "arm": seal["arm"],
        "run_id": seal["run_id"],
        "model_name_or_path": seal["model_name_or_path"],
        "terminal_set_digest": seal["terminal_set_digest"],
        "seal": {
            "path": str(seal_info["path"]),
            "bytes": len(seal_info["bytes"]),
            "sha256": seal_info["sha256"],
        },
        "irrevocable_registry_start": claim,
        "prediction": file_reference(predictions),
        "command": command,
        "started_at": started_at,
        "ended_at": utc_now(),
        "wall_seconds": time.monotonic() - started_monotonic,
        "wall_limit_seconds": plan["evaluator_wall_seconds"],
        "returncode": returncode,
        "timed_out": timed_out,
        "peak_process_tree_rss_kib": peak_rss,
        "container": container_result,
        "pinned_image": image,
        "preexisting_container_check": preexisting,
        "container_cleanup": cleanup,
        "credential_environment_keys_removed": removed_keys,
        "official_evaluator_invocation_authorized": True,
        "official_evaluator_invocation_confirmed": True,
        "official_evaluator_invocations": 1,
        "valid_official_outputs": valid_outputs,
        "output_validation_error": output_error,
        "official_outputs": outputs,
        "test_lists": test_lists_ref,
        "inventory": recursive_inventory(run_dir, {receipt_path}),
        "model_output_delivery": "none",
    }
    receipt_data = canonical_json_bytes(receipt)
    write_exclusive(receipt_path, receipt_data)
    return {
        "case_id": seal["case_id"],
        "rank": seal["rank"],
        "arm": seal["arm"],
        "disposition": "evaluate",
        "receipt": {
            "path": str(receipt_path),
            "bytes": len(receipt_data),
            "sha256": sha256_bytes(receipt_data),
        },
        "official_evaluator_invocation_authorized": True,
        "official_evaluator_invocation_confirmed": True,
        "valid": valid_outputs,
    }


def evaluate_one(
    plan: Mapping[str, Any],
    episode: Mapping[str, Any],
    seal_info: Mapping[str, Any],
    case_lock: threading.Lock,
    case_state: dict[str, Any],
) -> dict[str, Any]:
    with case_lock:
        if seal_info["seal"]["disposition"] != "evaluate":
            return write_skip_receipt(plan, seal_info)
        poison = case_state.get("poisoned")
        if isinstance(poison, dict):
            return write_case_poison_failure(plan, seal_info, poison)
        state: dict[str, Any] = {
            "invocation_authorized": False,
            "invocation_confirmed": False,
            "peak_rss_kib": 0,
        }
        try:
            return evaluate_one_inner(plan, episode, seal_info, state)
        except BaseException as exc:
            seal = seal_info["seal"]
            run_dir = Path(plan["output_root"]) / "evaluations" / seal["case_id"] / seal["arm"]
            run_dir.mkdir(parents=True, exist_ok=True)
            host_cleanup_error: str | None = None
            process = state.get("process")
            if process is not None and callable(getattr(process, "poll", None)) and process.poll() is None:
                try:
                    terminate_process_bounded(process)
                except BaseException as process_cleanup_exc:
                    host_cleanup_error = (
                        f"{type(process_cleanup_exc).__name__}: {process_cleanup_exc}"
                    )
            container_name = evaluator_container_name(episode)
            cleanup: dict[str, Any] | None = None
            cleanup_error: str | None = None
            try:
                cleanup = ensure_container_absent(container_name, remove=True)
            except BaseException as cleanup_exc:
                cleanup_error = f"{type(cleanup_exc).__name__}: {cleanup_exc}"
            unsafe_reasons = {
                key: value
                for key, value in {
                    "host_termination_error": state.get("host_termination_error"),
                    "host_cleanup_error": host_cleanup_error,
                    "container_cleanup_error": cleanup_error,
                    "container_absence_unproven": (
                        container_name
                        if not isinstance(cleanup, dict)
                        or cleanup.get("absent") is not True
                        else None
                    ),
                }.items()
                if value is not None
            }
            path = run_dir / "evaluation.failure.receipt.json"
            value = {
                "schema_version": FAILURE_SCHEMA,
                "recorded_at": utc_now(),
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
                "error": f"{type(exc).__name__}: {exc}",
                "container_cleanup": cleanup,
                "container_cleanup_error": cleanup_error,
                "host_termination_error": state.get("host_termination_error"),
                "host_cleanup_error": host_cleanup_error,
                "case_poisoned": bool(unsafe_reasons),
                "blocked_by_case_poison": None,
                "official_evaluator_invocation_authorized": state["invocation_authorized"],
                "official_evaluator_invocation_confirmed": state["invocation_confirmed"],
                "official_evaluator_invocations": int(state["invocation_confirmed"]),
                "valid_official_outputs": False,
                "inventory": recursive_inventory(run_dir, {path}),
                "model_output_delivery": "none",
            }
            data = canonical_json_bytes(value)
            write_exclusive(path, data)
            if unsafe_reasons:
                case_state["poisoned"] = {
                    "case_id": seal["case_id"],
                    "source_arm": seal["arm"],
                    "unsafe_reasons": unsafe_reasons,
                    "failure_receipt": {
                        "path": str(path),
                        "bytes": len(data),
                        "sha256": sha256_bytes(data),
                    },
                }
            return {
                "case_id": seal["case_id"],
                "rank": seal["rank"],
                "arm": seal["arm"],
                "disposition": "evaluate",
                "receipt": {"path": str(path), "bytes": len(data), "sha256": sha256_bytes(data)},
                "official_evaluator_invocation_authorized": state["invocation_authorized"],
                "official_evaluator_invocation_confirmed": value["official_evaluator_invocation_confirmed"],
                "valid": False,
            }


def evaluate_all(plan: Mapping[str, Any]) -> int:
    model_isolation = no_live_model_sessions(plan)
    environment = validate_static_environment(plan, include_docker=True)
    environment["model_session_isolation"] = model_isolation
    seal_set = validate_seal_set(plan)
    output_root = Path(plan["output_root"])
    started_path = output_root / "evaluation-batch.started.json"
    receipt_path = output_root / "evaluation-batch.receipt.json"
    require(not started_path.exists(), "evaluation batch already started; retry forbidden")
    require(not receipt_path.exists(), "evaluation batch already recorded")
    authorized = seal_set["value"]["official_evaluations_authorized"]
    write_exclusive(
        started_path,
        canonical_json_bytes(
            {
                "schema_version": "issue825-official-evaluation-batch-start-v1",
                "started_at": utc_now(),
                "plan_sha256": plan["_sha256"],
                "script_sha256": sha256_file(SCRIPT_PATH),
                "terminal_set_digest": seal_set["terminal_set_digest"],
                "seal_set_sha256": seal_set["sha256"],
                "official_evaluations_authorized": authorized,
                "max_parallel": plan["max_parallel"],
                "model_output_delivery": "none",
            }
        ),
    )
    seals = {
        (item["seal"]["case_id"], item["seal"]["arm"]): item
        for item in seal_set["seals"]
    }
    locks = {episode["case_id"]: threading.Lock() for episode in plan["episodes"]}
    case_states = {case_id: {"poisoned": None} for case_id in locks}
    results: list[dict[str, Any]] = []
    failures: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=plan["max_parallel"]) as pool:
        future_map = {
            pool.submit(
                evaluate_one,
                plan,
                episode,
                seals[(episode["case_id"], episode["arm"])],
                locks[episode["case_id"]],
                case_states[episode["case_id"]],
            ): episode
            for episode in plan["episodes"]
        }
        for future in concurrent.futures.as_completed(future_map):
            episode = future_map[future]
            try:
                results.append(future.result())
            except BaseException as exc:
                failures.append(
                    f"{episode['case_id']} {episode['arm']}: {type(exc).__name__}: {exc}"
                )
    results.sort(key=lambda value: (value["rank"], value["arm"]))
    receipt = {
        "schema_version": BATCH_SCHEMA,
        "ended_at": utc_now(),
        "plan_sha256": plan["_sha256"],
        "script_sha256": sha256_file(SCRIPT_PATH),
        "terminal_set_digest": seal_set["terminal_set_digest"],
        "seal_set": {
            "path": str(seal_set["path"]),
            "bytes": len(seal_set["bytes"]),
            "sha256": seal_set["sha256"],
        },
        "environment": environment,
        "official_evaluations_authorized": authorized,
        "official_evaluations_started": sum(
            item["official_evaluator_invocation_confirmed"] for item in results
        ),
        "official_evaluations_recorded": sum(
            item["disposition"] == "evaluate" for item in results
        ),
        "zero_invocation_receipts": sum(
            item["disposition"] != "evaluate" for item in results
        ),
        "max_parallel": plan["max_parallel"],
        "same_case_serialized": True,
        "results": results,
        "failures": failures,
        "valid": len(results) == 4 and not failures and all(item["valid"] for item in results),
        "model_output_delivery": "none; evaluator outputs remain out-of-band",
    }
    write_exclusive(receipt_path, canonical_json_bytes(receipt))
    return 0 if receipt["valid"] else 1


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("command", choices=("static-preflight", "preflight", "seal", "evaluate"))
    value.add_argument("--plan", type=Path, required=True)
    value.add_argument("--write-receipt", type=Path)
    return value


def main() -> int:
    args = parser().parse_args()
    try:
        plan = validate_plan(args.plan.resolve(strict=True))
        if args.command == "static-preflight":
            value = {
                "schema_version": "issue825-evaluator-static-preflight-v1",
                "plan_sha256": plan["_sha256"],
                "script_sha256": sha256_file(SCRIPT_PATH),
                "terminal_set_digest": terminal_set_digest(plan["_validated_episodes"]),
                "dispositions": [
                    {
                        "case_id": item["episode"]["case_id"],
                        "arm": item["episode"]["arm"],
                        "disposition": item["disposition"],
                    }
                    for item in plan["_validated_episodes"]
                ],
                "official_evaluator_invocations": 0,
            }
            if args.write_receipt is not None:
                require(args.write_receipt.is_absolute(), "receipt path must be absolute")
                write_exclusive(args.write_receipt, canonical_json_bytes(value))
            print(
                json.dumps(
                    {
                        "status": "static-preflight-complete",
                        "official_evaluator_invocations": 0,
                    },
                    sort_keys=True,
                )
            )
            return 0
        require(args.write_receipt is None, "--write-receipt is static-preflight only")
        if args.command == "preflight":
            validate_static_environment(plan, include_docker=True)
            print(json.dumps({"status": "preflight-complete", "official_evaluator_invocations": 0}, sort_keys=True))
            return 0
        if args.command == "seal":
            output = seal_all(plan)
            print(json.dumps({"status": "sealed", "output_root": str(output), "official_evaluator_invocations": 0}, sort_keys=True))
            return 0
        return evaluate_all(plan)
    except (FailClosed, FileExistsError) as exc:
        print(f"FAIL-CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
