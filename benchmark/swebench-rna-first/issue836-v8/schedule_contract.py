#!/usr/bin/env python3
"""Pure validation helpers for the issue836-v8 staged execution schedule."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
from typing import Any, Mapping, Sequence
import uuid


SCHEDULE_SCHEMA = "issue836-rolling-execution-schedule-v8"
ENVELOPE_SCHEMA = "issue836-v4-frozen-episode-envelope-v1"
WAVE_MANIFEST_SCHEMA = "issue836-rolling-wave-manifest-v8"
WAVE_RECEIPT_SCHEMA = "issue836-rolling-wave-receipt-v8"
FINAL_LEDGER_SCHEMA = "issue836-rolling-final-ledger-v8"
SELECTION_BINDING_SCHEMA = "issue836-rolling-selection-binding-v8"
ENVELOPE_BINDING_SCHEMA = "issue836-rolling-envelope-binding-v8"
SCHEDULE_FILENAME = "execution-schedule-v17.json"
SELECTION_BINDING_FILENAME = "selection-binding-v17.json"
COMPATIBILITY_FILENAME = "v8-compatibility-manifest.json"
WAVE_MANIFEST_FILENAME = "rolling-wave-manifest-v17.json"
INVOCATION_FILENAME = "v8-invocation-start.json"
ENVELOPE_BINDING_FILENAME = "v8-envelope-binding.json"
PREDECESSOR_ACTIVITY_FILENAME = "predecessor-activity.json"
QUALIFIED_SOURCE_ROOT = Path(
    "/Users/muness/swebench-evidence/issue829-readiness-fix.IYiMyx/source"
)
QUALIFIED_HARNESS_ROOT = (
    QUALIFIED_SOURCE_ROOT / "benchmark/swebench-rna-first/issue827"
)
SUCCESSOR_HARNESS_ROOT = Path(__file__).resolve().parent.parent / "issue827"
REGISTERED_SOURCE_DELTA = {
    "bash_gateway_sha256": {
        "qualified": (
            "42b0484a6d63e2cae59550003ecfa0c494778dc925cd641abe3b21e8a2b4c58c"
        ),
        "successor": (
            "cded193127f4e35f0d72481bdbf56a437f3c9e58c4ec1c0819a2a1b19270fbb9"
        ),
    },
    "common_supervisor_sha256": {
        "qualified": (
            "8aea0bc62eb36d5e249cb50a1aebce6c2545d7083cc91b5bcc933de945d9a597"
        ),
        "successor": (
            "d1730dbea66c80cf17b94a8a1df28a623958d9b67784148cddbd004e4a44ccbe"
        ),
    },
    "isolation_sha256": {
        "qualified": (
            "63b556e094f0553701118fbea7db9729a327f30496388778fb2ff48b8d28284e"
        ),
        "successor": (
            "8473f135eefddc87723ffcfd016d85196637f6573d9ba16b052cdd7949a98754"
        ),
    },
    "runner_sha256": {
        "qualified": (
            "cc250022e01b5c708d5d4d0c5a9ba8caad97f7a7d08006c231f7eb17cb58d658"
        ),
        "successor": (
            "83b963bb65b8af3162159be5043577acbd5908093ecdb61f999bc350a18e3a7a"
        ),
    },
}
SUCCESSOR_REGISTERED_FILES_SHA256 = (
    "6e6adf4e78d42a54385a3f8c900d73adac790374a394fc5619cf784bef00c45b"
)
PROTOCOL_CHANGE = (
    "deterministic_issue_query_preconditioning_and_gateway_recovery_"
    "plus_qualified_registration_source_bridge_"
    "plus_native_tool_parallelism_"
    "plus_trace_namespace_classification_repair_"
    "plus_internal_tracked_symlink_support_"
    "plus_noncontaminating_observation_policy_"
    "plus_workspace_alias_mapping_"
    "plus_three_case_parallelism_"
    "no_cohort_or_arm_order_change"
)
QUALIFIED_REGISTRATION_RELATIVE_PATH = (
    "benchmark/swebench-rna-first/issue830/registration.json"
)
QUALIFIED_REGISTRATION_SHA256 = (
    "ee3c602de28696a3dee4a0c9c6107c8d184c22b20763987c2dcf7f2e496cfd1a"
)
QUALIFICATION_MANIFEST_SHA256 = (
    "a9e3d460fb9d0daf2c4e4bc93c08781d327ba729f7082498679a8ac5823a43a9"
)
QUALIFICATION_ARCHIVE_SHA256 = (
    "97fe60aac2107925061fb13a94ea61a07ffdba38460e426633729d942bcad960"
)
QUALIFIED_REGISTERED_FILES_SHA256 = (
    "14b9b57f5b9db2d1d1835a7e861a622317ed25f37505f4540982d217c20a1cc8"
)
CURRENT_REGISTERED_FILES_SHA256 = (
    "d867d3a9da1ffb63b13fdc0a68dd84717043f2203a0e0168f477ca9c65b59fef"
)
REGISTERED_FILE_DELTA_KEYS = [
    "evaluator_plan_template_sha256",
    "evaluator_runner_sha256",
    "registration_contract_sha256",
    "result_selector_sha256",
    "runner_sha256",
    "selector_sha256",
    "verifier_sha256",
]
QUALIFICATION_COMPATIBILITY = {
    "schema_version": "issue836-v4-qualification-compatibility-v1",
    "reason_code": (
        "v4_reused_issue830_closure_after_registered_harness_successor"
    ),
    "qualified_registration_relative_path": QUALIFIED_REGISTRATION_RELATIVE_PATH,
    "qualified_registration_sha256": QUALIFIED_REGISTRATION_SHA256,
    "qualification_manifest_sha256": QUALIFICATION_MANIFEST_SHA256,
    "qualification_archive_sha256": QUALIFICATION_ARCHIVE_SHA256,
    "qualified_registered_files_sha256": QUALIFIED_REGISTERED_FILES_SHA256,
    "current_registered_files_sha256": CURRENT_REGISTERED_FILES_SHA256,
    "registered_file_delta_keys": REGISTERED_FILE_DELTA_KEYS,
    "model_runtime_unchanged": True,
    "isolation_runtime_unchanged": True,
    "rna_artifact_unchanged": True,
    "applies_equally_to_arms": ["A", "T"],
    "cohort_or_arm_change": False,
    "gold_or_outcomes_inspected": False,
    "prior_model_calls": 0,
    "prior_provider_requests": 0,
    "prior_official_evaluator_invocations": 0,
}
BASE_REGISTRATION_SHA256 = (
    "2b070bb61ea2c5de6fe6b1d8cf840d6d4e53732b4e524d3f26aefc3504a6523b"
)
BASE_SELECTION_SHA256 = (
    "8d198247774ee6793c46ab61ba1b5005af90a8f6cc6abe2e54f2c613c2b26cc8"
)
BASE_SOURCE_COMMIT = "6bb1bf6200cb9f380007d033f717a2825fe75934"
BASE_SOURCE_TREE = "70af087a30f93ad204af1196fad30a0da04c3e66"
APPROVED_ASSEMBLER_SHA256 = (
    "ab0bed124258bfd473a5be3332bccadb428bd5412f598dd5f5d3035002efb05e"
)
NO_SPEND = {
    "credentials_accessed": False,
    "model_calls": 0,
    "provider_requests": 0,
    "official_evaluator_invocations": 0,
}
PREDECESSOR_ACTIVITY = {
    "infrastructure_invalidated_provider_episodes": 10,
    "local_cli_process_starts": 14,
    "model_invoked_receipts": 14,
    "official_evaluator_invocations": 0,
    "provider_cost_usd": 3.3581463,
    "provider_exposed_episodes": 12,
    "provider_usage_tokens": 4510145,
    "stale_auth_episode_starts": 2,
    "terminal_patches": 5,
}


class ContractError(RuntimeError):
    """A staged-execution identity or budget invariant did not hold."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def exact_keys(value: Mapping[str, Any], keys: set[str], where: str) -> None:
    require(isinstance(value, Mapping), f"{where} must be an object")
    missing = keys - set(value)
    extra = set(value) - keys
    require(not missing, f"{where}: missing fields {sorted(missing)}")
    require(not extra, f"{where}: unexpected fields {sorted(extra)}")


def canonical(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")


def sha_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha_canonical(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()


def file_ref(path: Path) -> dict[str, Any]:
    require(path.is_file() and not path.is_symlink(), f"invalid file: {path}")
    resolved = path.resolve(strict=True)
    return {
        "path": str(resolved),
        "bytes": resolved.stat().st_size,
        "sha256": sha_file(resolved),
    }


def validate_qualified_registered_sources(
    registration_contract_module: Any,
    registration: Mapping[str, Any],
) -> None:
    """Validate the frozen registration against its byte-qualified sources."""

    require(
        QUALIFIED_HARNESS_ROOT.is_dir()
        and not QUALIFIED_HARNESS_ROOT.is_symlink(),
        "qualified registration source root unavailable",
    )
    try:
        registration_contract_module.validate_registration(
            registration,
            source_root=QUALIFIED_HARNESS_ROOT,
        )
    except registration_contract_module.RegistrationContractError as exc:
        raise ContractError(
            f"qualified registration source validation failed: {exc}"
        ) from exc
    registered = registration.get("registered_files")
    require(
        isinstance(registered, Mapping)
        and sha_canonical(registered) == CURRENT_REGISTERED_FILES_SHA256,
        "frozen registered source digest drift",
    )
    filenames = registration_contract_module.REGISTERED_FILE_NAMES
    require(
        SUCCESSOR_HARNESS_ROOT.is_dir()
        and not SUCCESSOR_HARNESS_ROOT.is_symlink()
        and isinstance(filenames, Mapping)
        and set(filenames) == set(registered),
        "successor registered source root or interface drift",
    )
    successor = {
        key: sha_file(SUCCESSOR_HARNESS_ROOT / str(filename))
        for key, filename in filenames.items()
    }
    changed = sorted(
        key for key in registered if registered[key] != successor[key]
    )
    require(
        changed == sorted(REGISTERED_SOURCE_DELTA),
        "successor registered source delta drift",
    )
    for key, identity in REGISTERED_SOURCE_DELTA.items():
        require(
            registered[key] == identity["qualified"]
            and successor[key] == identity["successor"],
            f"successor registered source identity drift: {key}",
        )
    require(
        sha_canonical(successor) == SUCCESSOR_REGISTERED_FILES_SHA256,
        "successor registered source digest drift",
    )


def check_ref(value: Any, where: str) -> tuple[Path, bytes]:
    require(isinstance(value, dict), f"{where} must be a file reference")
    exact_keys(value, {"path", "bytes", "sha256"}, where)
    path = Path(value["path"])
    require(
        path.is_absolute()
        and path.is_file()
        and not path.is_symlink()
        and path.resolve(strict=True) == path,
        f"{where}.path invalid",
    )
    data = path.read_bytes()
    require(len(data) == value["bytes"], f"{where}.bytes mismatch")
    require(hashlib.sha256(data).hexdigest() == value["sha256"], f"{where}.sha256 mismatch")
    return path, data


def validate_predecessor_activity(
    value: Mapping[str, Any],
    root: Path,
) -> dict[str, Any]:
    """Validate all superseded v8-lineage attempts without erasing spend."""

    path, data = check_ref(value, "predecessor activity")
    require(
        path == (root / PREDECESSOR_ACTIVITY_FILENAME).resolve(strict=True),
        "predecessor activity binds another file",
    )
    try:
        document = json.loads(data)
    except (json.JSONDecodeError, UnicodeError) as exc:
        raise ContractError("predecessor activity is not JSON") from exc
    exact_keys(
        document,
        {
            "schema_version",
            "activity",
            "adaptations",
            "cohort_reused_unchanged",
            "earlier_predecessor_ledger",
            "evaluator_or_outcomes_inspected",
            "new_session_ids_required",
            "predecessor_attempts",
            "predecessor_superseded",
            "protocol_change",
        },
        "predecessor activity",
    )
    require(
        document["schema_version"] == "issue836-predecessor-activity-v3"
        and document["activity"] == PREDECESSOR_ACTIVITY
        and document["cohort_reused_unchanged"] is True
        and document["evaluator_or_outcomes_inspected"] is False
        and document["new_session_ids_required"] is True
        and document["predecessor_superseded"] is True
        and document["protocol_change"]
        == (
            "deterministic_issue_query_preconditioning_and_gateway_recovery_"
            "plus_trace_namespace_classification_repair_"
            "plus_internal_tracked_symlink_support_"
            "plus_noncontaminating_observation_policy_"
            "plus_workspace_alias_mapping_"
            "plus_three_case_parallelism"
        ),
        "predecessor activity semantics drift",
    )
    adaptations = document["adaptations"]
    require(
        isinstance(adaptations, list)
        and [item.get("type") for item in adaptations]
        == [
            "preconditioned_treatment_restoration",
            "trusted_gateway_access_restoration",
            "ordinary_gateway_failure_delivery",
            "qualified_registration_source_validation_bridge",
            "native_tool_parallelism_restoration",
            "authoritative_model_usage_reporting",
            "trace_namespace_classification_repair",
            "internal_tracked_symlink_support",
            "noncontaminating_observation_policy",
            "workspace_alias_and_native_hook_telemetry",
            "three_case_trial_parallelism",
        ]
        and adaptations[0].get("applies_to_arms") == ["T"]
        and all(
            item.get("applies_to_arms") == ["A", "T"]
            for item in adaptations[1:]
        ),
        "successor adaptations drift",
    )
    check_ref(
        document["earlier_predecessor_ledger"],
        "predecessor activity.earlier_predecessor_ledger",
    )
    attempts = document["predecessor_attempts"]
    require(
        isinstance(attempts, list)
        and [attempt.get("classification") for attempt in attempts]
        == [
            "stale_auth_zero_provider",
            "provider_exposed_supervisor_termination",
            "provider_exposed_native_tool_overlap",
            "provider_exposed_outer_seatbelt_dev_null_denial",
            "provider_exposed_trace_namespace_classification",
            "provider_exposed_relaxed_policy_admissible_pair",
            "provider_exposed_censored_monitor_false_positive_pair",
        ],
        "predecessor attempt classifications drift",
    )
    for index, attempt in enumerate(attempts):
        exact_keys(
            attempt,
            {
                "classification",
                "invocation_start",
                "wave_receipt",
                "a_episode_receipt",
                "t_episode_receipt",
            },
            f"predecessor activity.predecessor_attempts[{index}]",
        )
        for label, reference in attempt.items():
            if label != "classification":
                check_ref(
                    reference,
                    "predecessor activity."
                    f"predecessor_attempts[{index}].{label}",
                )
    return document


def explicit_wave_ranks(value: Any) -> tuple[int, ...]:
    require(
        isinstance(value, list) and 1 <= len(value) <= 3,
        "a wave must explicitly select one to three ranks",
    )
    require(
        all(type(rank) is int and 1 <= rank <= 20 for rank in value),
        "wave ranks must be integers from 1 through 20",
    )
    require(len(set(value)) == len(value), "wave ranks must be unique")
    require(value == sorted(value), "wave ranks must be in frozen selection order")
    return tuple(value)


def _uuid4(value: Any, where: str) -> str:
    require(isinstance(value, str), f"{where} must be a UUID string")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as exc:
        raise ContractError(f"{where} invalid") from exc
    require(
        parsed.version == 4 and str(parsed) == value,
        f"{where} is not canonical UUID4",
    )
    return value


def validate_episode_envelope(
    envelope: Mapping[str, Any],
    selection: Mapping[str, Any],
    *,
    registration_ref: Mapping[str, Any],
    selection_ref: Mapping[str, Any],
) -> dict[int, dict[str, str]]:
    """Validate all v4 identities and the fixed 40-session/$240 envelope."""

    exact_keys(
        envelope,
        {
            "schema_version",
            "assembler",
            "verified",
            "source_commit",
            "source_tree",
            "registration",
            "selection",
            "case_count",
            "episode_count",
            "per_episode_budget_usd",
            "maximum_budget_usd",
            "cases",
            "execution_episode_keys",
            "same_case_serialized",
            "max_parallel_cases",
            "selection_policy",
            "model_outputs_inspected",
            "evaluator_or_outcome_accessed",
            "no_spend_assertion",
        },
        "episode envelope",
    )
    require(
        envelope["schema_version"] == ENVELOPE_SCHEMA
        and isinstance(envelope["assembler"], dict)
        and set(envelope["assembler"]) == {"path", "bytes", "sha256"}
        and envelope["assembler"]["sha256"] == APPROVED_ASSEMBLER_SHA256
        and envelope["verified"] is True
        and isinstance(envelope["source_commit"], str)
        and re.fullmatch(r"[0-9a-f]{40}", envelope["source_commit"]) is not None
        and isinstance(envelope["source_tree"], str)
        and re.fullmatch(r"[0-9a-f]{40}", envelope["source_tree"]) is not None
        and envelope["registration"] == registration_ref
        and envelope["selection"] == selection_ref,
        "episode envelope frozen identity drift",
    )
    require(
        envelope["case_count"] == 20
        and envelope["episode_count"] == 40
        and envelope["per_episode_budget_usd"] == 6.0
        and envelope["maximum_budget_usd"] == 240.0
        and envelope["same_case_serialized"] is True
        and envelope["max_parallel_cases"] == 2
        and envelope["selection_policy"]
        == "full_frozen_v4_identity_before_first_batch"
        and envelope["model_outputs_inspected"] is False
        and envelope["evaluator_or_outcome_accessed"] is False
        and envelope["no_spend_assertion"] == NO_SPEND,
        "episode envelope schedule/budget drift",
    )
    selected = selection.get("cases")
    cases = envelope["cases"]
    require(
        isinstance(selected, list)
        and len(selected) == 20
        and isinstance(cases, list)
        and len(cases) == 20,
        "episode envelope must contain the full 20-case cohort",
    )
    sessions_seen: set[str] = set()
    sessions_by_rank: dict[int, dict[str, str]] = {}
    episode_keys: list[dict[str, Any]] = []
    for index, (case, chosen) in enumerate(zip(cases, selected, strict=True)):
        where = f"episode envelope.cases[{index}]"
        exact_keys(
            case,
            {
                "rank",
                "instance_id",
                "base_commit",
                "base_tree",
                "arm_order",
                "sessions",
            },
            where,
        )
        require(
            case["rank"] == index + 1 == chosen.get("rank")
            and case["instance_id"] == chosen.get("instance_id")
            and case["base_commit"] == chosen.get("base_commit")
            and case["base_tree"] == chosen.get("base_tree")
            and case["arm_order"] == chosen.get("arm_order")
            and case["arm_order"]
            == (["A", "T"] if case["rank"] % 2 else ["T", "A"]),
            f"{where} differs from the frozen selection",
        )
        sessions = case["sessions"]
        exact_keys(sessions, {"A", "T"}, f"{where}.sessions")
        normalized: dict[str, str] = {}
        for arm in ("A", "T"):
            session = _uuid4(sessions[arm], f"{where}.sessions.{arm}")
            require(session not in sessions_seen, f"session reused: {session}")
            sessions_seen.add(session)
            normalized[arm] = session
        sessions_by_rank[case["rank"]] = normalized
        episode_keys.extend(
            {
                "rank": case["rank"],
                "case_id": case["instance_id"],
                "arm": arm,
                "session_id": normalized[arm],
            }
            for arm in case["arm_order"]
        )
    require(
        len(sessions_seen) == 40
        and envelope["execution_episode_keys"] == episode_keys,
        "episode envelope 40-episode identity/order drift",
    )
    return sessions_by_rank


def next_cumulative_state(
    *,
    prior_ranks: set[int],
    prior_sessions: set[str],
    requested_ranks: Sequence[int],
    requested_sessions: set[str],
) -> dict[str, Any]:
    """Derive an append-only ledger state and reject any possible re-spend."""

    ranks = explicit_wave_ranks(list(requested_ranks))
    require(
        not prior_ranks.intersection(ranks),
        "a requested rank already appears in the cumulative ledger",
    )
    require(
        len(requested_sessions) == 2 * len(ranks),
        "a wave must bind exactly two sessions per selected case",
    )
    require(
        not prior_sessions.intersection(requested_sessions),
        "a requested session already appears in the cumulative ledger",
    )
    cumulative_ranks = sorted(prior_ranks.union(ranks))
    cumulative_sessions = sorted(prior_sessions.union(requested_sessions))
    require(
        len(cumulative_sessions) == 2 * len(cumulative_ranks),
        "cumulative rank/session ledger is inconsistent",
    )
    return {
        "cumulative_ranks": cumulative_ranks,
        "cumulative_sessions": cumulative_sessions,
        "cumulative_case_count": len(cumulative_ranks),
        "cumulative_episode_count": len(cumulative_sessions),
        "cumulative_maximum_budget_usd": 6.0 * len(cumulative_sessions),
        "pending_ranks": [
            rank for rank in range(1, 21) if rank not in set(cumulative_ranks)
        ],
    }


def validate_qualification_compatibility(
    record: Mapping[str, Any],
    qualification_manifest: Mapping[str, Any],
    current_registration: Mapping[str, Any],
    qualified_registration: Mapping[str, Any],
) -> None:
    """Validate the exact no-spend bridge for the frozen v4 registration."""

    require(
        record == QUALIFICATION_COMPATIBILITY,
        "qualification compatibility record drift",
    )
    exact_keys(
        qualification_manifest,
        {
            "schema_version",
            "qualified",
            "no_model_or_provider_calls",
            "archive_sha256",
            "registered_files_sha256",
            "model_runtime_sha256",
            "isolation_runtime_sha256",
            "rna_artifact_sha256",
            "external_inputs_sha256",
            "runtime_identity_sha256",
            "evidence_inventory_sha256",
        },
        "qualification compatibility manifest",
    )
    require(
        qualification_manifest["schema_version"]
        == "issue827-qualification-closure-v1"
        and qualification_manifest["qualified"] is True
        and qualification_manifest["no_model_or_provider_calls"] is True
        and qualification_manifest["archive_sha256"]
        == QUALIFICATION_ARCHIVE_SHA256
        and qualification_manifest["registered_files_sha256"]
        == QUALIFIED_REGISTERED_FILES_SHA256,
        "qualified predecessor closure identity drift",
    )
    for key in (
        "external_inputs_sha256",
        "runtime_identity_sha256",
        "evidence_inventory_sha256",
    ):
        require(
            isinstance(qualification_manifest[key], str)
            and re.fullmatch(
                r"[0-9a-f]{64}",
                qualification_manifest[key],
            )
            is not None,
            f"qualification compatibility digest invalid: {key}",
        )
    require(
        current_registration["qualification_closure"]["manifest_sha256"]
        == qualified_registration["qualification_closure"]["manifest_sha256"]
        == QUALIFICATION_MANIFEST_SHA256
        and current_registration["qualification_closure"]["archive_sha256"]
        == qualified_registration["qualification_closure"]["archive_sha256"]
        == QUALIFICATION_ARCHIVE_SHA256,
        "qualification closure registration lineage drift",
    )
    current_files = current_registration["registered_files"]
    qualified_files = qualified_registration["registered_files"]
    require(
        sha_canonical(current_files) == CURRENT_REGISTERED_FILES_SHA256
        and sha_canonical(qualified_files)
        == QUALIFIED_REGISTERED_FILES_SHA256,
        "qualification registered-file digest drift",
    )
    changed = sorted(
        key
        for key in set(current_files) | set(qualified_files)
        if current_files.get(key) != qualified_files.get(key)
    )
    require(
        changed == REGISTERED_FILE_DELTA_KEYS,
        "qualification registered-file delta drift",
    )
    for field, manifest_key in (
        ("model_runtime", "model_runtime_sha256"),
        ("isolation_runtime", "isolation_runtime_sha256"),
        ("rna_artifact", "rna_artifact_sha256"),
    ):
        require(
            current_registration[field] == qualified_registration[field]
            and sha_canonical(current_registration[field])
            == qualification_manifest[manifest_key],
            f"qualification compatibility changed {field}",
        )


def validate_schedule(schedule: Mapping[str, Any], root: Path) -> None:
    """Validate the formal successor schedule and every registered source."""

    exact_keys(
        schedule,
        {
            "schema_version",
            "authoritative",
            "protocol_change",
            "base_source_commit",
            "base_source_tree",
            "implementation_commit",
            "implementation_tree",
            "base_registration_sha256",
            "base_selection_sha256",
            "approved_assembler_sha256",
            "qualification_compatibility",
            "registered_files",
            "case_count",
            "episode_count",
            "per_episode_budget_usd",
            "maximum_budget_usd",
            "max_cases_per_wave",
            "max_episodes_per_wave",
            "same_case_serialized",
            "different_cases_max_parallel",
            "one_shot_per_rank",
            "append_only_cumulative_ledger",
            "evaluation_before_full_cohort_allowed",
            "predecessor_activity",
        },
        "execution schedule",
    )
    require(
        schedule["schema_version"] == SCHEDULE_SCHEMA
        and schedule["authoritative"] is True
        and schedule["protocol_change"] == PROTOCOL_CHANGE
        and schedule["base_source_commit"] == BASE_SOURCE_COMMIT
        and schedule["base_source_tree"] == BASE_SOURCE_TREE
        and isinstance(schedule["implementation_commit"], str)
        and re.fullmatch(
            r"[0-9a-f]{40}",
            schedule["implementation_commit"],
        )
        is not None
        and isinstance(schedule["implementation_tree"], str)
        and re.fullmatch(
            r"[0-9a-f]{40}",
            schedule["implementation_tree"],
        )
        is not None
        and schedule["base_registration_sha256"] == BASE_REGISTRATION_SHA256
        and schedule["base_selection_sha256"] == BASE_SELECTION_SHA256,
        "execution schedule frozen source/cohort drift",
    )
    require(
        schedule["case_count"] == 20
        and schedule["episode_count"] == 40
        and schedule["per_episode_budget_usd"] == 6.0
        and schedule["maximum_budget_usd"] == 240.0
        and schedule["max_cases_per_wave"] == 3
        and schedule["max_episodes_per_wave"] == 6
        and schedule["same_case_serialized"] is True
        and schedule["different_cases_max_parallel"] == 3
        and schedule["one_shot_per_rank"] is True
        and schedule["append_only_cumulative_ledger"] is True
        and schedule["evaluation_before_full_cohort_allowed"] is False,
        "execution schedule dimensions/budget drift",
    )
    validate_predecessor_activity(schedule["predecessor_activity"], root)
    require(
        schedule["approved_assembler_sha256"]
        == APPROVED_ASSEMBLER_SHA256,
        "execution schedule approved assembler drift",
    )
    require(
        schedule["qualification_compatibility"]
        == QUALIFICATION_COMPATIBILITY,
        "execution schedule qualification compatibility drift",
    )
    qualified_registration_path = (
        root.parents[2] / QUALIFIED_REGISTRATION_RELATIVE_PATH
    )
    require(
        qualified_registration_path.is_file()
        and not qualified_registration_path.is_symlink()
        and sha_file(qualified_registration_path)
        == QUALIFIED_REGISTRATION_SHA256,
        "qualified predecessor registration drift",
    )
    registered = schedule["registered_files"]
    exact_keys(
        registered,
        {
            "schedule_contract.py",
            "run_wave.py",
            "observational_tool_supervisor.py",
            "verify_preconditioned.py",
            "precondition-prefix.txt",
            "precondition-suffix.txt",
            "verify_waves.py",
            "assemble_wave.py",
            "assemble_successor.py",
            "generate_schedule.py",
            "generate_selection_binding.py",
            PREDECESSOR_ACTIVITY_FILENAME,
        },
        "execution schedule.registered_files",
    )
    for filename, expected in registered.items():
        require(
            isinstance(expected, str)
            and re.fullmatch(r"[0-9a-f]{64}", expected) is not None,
            f"registered hash invalid: {filename}",
        )
        path = root / filename
        require(
            path.is_file()
            and not path.is_symlink()
            and sha_file(path) == expected,
            f"registered source hash mismatch: {filename}",
        )
    require(
        registered["assemble_successor.py"] == APPROVED_ASSEMBLER_SHA256,
        "approved assembler is not the registered successor assembler",
    )


def validate_selection_binding(
    binding: Mapping[str, Any],
    *,
    schedule_sha256: str,
    selection: Mapping[str, Any],
) -> None:
    """Validate the v8 schedule binding without changing the v4 cohort."""

    exact_keys(
        binding,
        {
            "schema_version",
            "authoritative",
            "protocol_change",
            "schedule_sha256",
            "schedule_commit",
            "schedule_tree",
            "base_registration_sha256",
            "base_selection_sha256",
            "base_selection_digest",
            "case_count",
            "episode_count",
            "cases",
            "episode_identities",
            "problem_statements_inspected_for_schedule_change",
            "gold_or_outcomes_inspected_for_schedule_change",
            "predecessor_activity",
        },
        "selection binding",
    )
    require(
        binding["schema_version"] == SELECTION_BINDING_SCHEMA
        and binding["authoritative"] is True
        and binding["protocol_change"] == PROTOCOL_CHANGE
        and binding["schedule_sha256"] == schedule_sha256
        and isinstance(binding["schedule_commit"], str)
        and re.fullmatch(r"[0-9a-f]{40}", binding["schedule_commit"])
        is not None
        and isinstance(binding["schedule_tree"], str)
        and re.fullmatch(r"[0-9a-f]{40}", binding["schedule_tree"])
        is not None
        and binding["base_registration_sha256"]
        == BASE_REGISTRATION_SHA256
        and binding["base_selection_sha256"] == BASE_SELECTION_SHA256
        and binding["base_selection_digest"] == selection.get("digest")
        and binding["case_count"] == 20
        and binding["episode_count"] == 40
        and binding["problem_statements_inspected_for_schedule_change"]
        is False
        and binding["gold_or_outcomes_inspected_for_schedule_change"] is False
        and binding["predecessor_activity"]
        == file_ref(
            Path(__file__).resolve().parent / PREDECESSOR_ACTIVITY_FILENAME
        ),
        "selection binding source/no-spend identity drift",
    )
    selected = selection.get("cases")
    require(
        isinstance(selected, list)
        and len(selected) == 20
        and isinstance(binding["cases"], list)
        and len(binding["cases"]) == 20,
        "selection binding case list invalid",
    )
    expected_cases = [
        {
            key: case[key]
            for key in (
                "rank",
                "instance_id",
                "repo",
                "base_commit",
                "base_tree",
                "problem_statement_sha256",
                "arm_order",
            )
        }
        for case in selected
    ]
    expected_identities = [
        {
            "rank": case["rank"],
            "case_id": case["instance_id"],
            "arm": arm,
        }
        for case in selected
        for arm in case["arm_order"]
    ]
    require(
        binding["cases"] == expected_cases
        and binding["episode_identities"] == expected_identities,
        "selection binding changes frozen case/arm identities",
    )


def validate_envelope_binding(
    binding: Mapping[str, Any],
    *,
    schedule: Mapping[str, Any],
    schedule_ref: Mapping[str, Any],
    selection_binding_ref: Mapping[str, Any],
    envelope_ref: Mapping[str, Any],
) -> None:
    exact_keys(
        binding,
        {
            "schema_version",
            "verified",
            "schedule",
            "selection_binding",
            "episode_envelope",
            "predecessor_activity",
            "approved_assembler_sha256",
            "assembly_source_commit",
            "assembly_source_tree",
            "case_count",
            "episode_count",
            "per_episode_budget_usd",
            "maximum_budget_usd",
            "model_outputs_inspected",
            "evaluator_or_outcome_accessed",
            "no_spend_assertion",
        },
        "envelope binding",
    )
    require(
        binding["schema_version"] == ENVELOPE_BINDING_SCHEMA
        and binding["verified"] is True
        and binding["schedule"] == schedule_ref
        and binding["selection_binding"] == selection_binding_ref
        and binding["episode_envelope"] == envelope_ref
        and binding["predecessor_activity"]
        == schedule["predecessor_activity"]
        and binding["approved_assembler_sha256"]
        == APPROVED_ASSEMBLER_SHA256
        and binding["assembly_source_commit"]
        == schedule["implementation_commit"]
        and binding["assembly_source_tree"]
        == schedule["implementation_tree"]
        and binding["case_count"] == 20
        and binding["episode_count"] == 40
        and binding["per_episode_budget_usd"] == 6.0
        and binding["maximum_budget_usd"] == 240.0
        and binding["model_outputs_inspected"] is False
        and binding["evaluator_or_outcome_accessed"] is False
        and binding["no_spend_assertion"] == NO_SPEND,
        "envelope binding drift",
    )
