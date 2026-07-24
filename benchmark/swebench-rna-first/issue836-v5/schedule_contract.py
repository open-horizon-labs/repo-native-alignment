#!/usr/bin/env python3
"""Pure validation helpers for the issue836-v5 staged execution schedule."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
from typing import Any, Mapping, Sequence
import uuid


SCHEDULE_SCHEMA = "issue836-rolling-execution-schedule-v5"
ENVELOPE_SCHEMA = "issue836-v4-frozen-episode-envelope-v1"
WAVE_MANIFEST_SCHEMA = "issue836-rolling-wave-manifest-v5"
WAVE_RECEIPT_SCHEMA = "issue836-rolling-wave-receipt-v5"
FINAL_LEDGER_SCHEMA = "issue836-rolling-final-ledger-v5"
SELECTION_BINDING_SCHEMA = "issue836-rolling-selection-binding-v5"
ENVELOPE_BINDING_SCHEMA = "issue836-rolling-envelope-binding-v5"
BASE_REGISTRATION_SHA256 = (
    "2b070bb61ea2c5de6fe6b1d8cf840d6d4e53732b4e524d3f26aefc3504a6523b"
)
BASE_SELECTION_SHA256 = (
    "8d198247774ee6793c46ab61ba1b5005af90a8f6cc6abe2e54f2c613c2b26cc8"
)
BASE_SOURCE_COMMIT = "6bb1bf6200cb9f380007d033f717a2825fe75934"
BASE_SOURCE_TREE = "70af087a30f93ad204af1196fad30a0da04c3e66"
APPROVED_ASSEMBLER_SHA256 = (
    "72744c08c98467d15e171bcb5f1de95ddfad809e177559240b73c95b4b0666ee"
)
NO_SPEND = {
    "credentials_accessed": False,
    "model_calls": 0,
    "provider_requests": 0,
    "official_evaluator_invocations": 0,
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


def file_ref(path: Path) -> dict[str, Any]:
    require(path.is_file() and not path.is_symlink(), f"invalid file: {path}")
    resolved = path.resolve(strict=True)
    return {
        "path": str(resolved),
        "bytes": resolved.stat().st_size,
        "sha256": sha_file(resolved),
    }


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


def explicit_wave_ranks(value: Any) -> tuple[int, ...]:
    require(
        isinstance(value, list) and 1 <= len(value) <= 2,
        "a wave must explicitly select one or two ranks",
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
        and envelope["source_commit"] == BASE_SOURCE_COMMIT
        and isinstance(envelope["source_tree"], str)
        and re.fullmatch(r"[0-9a-f]{40}", envelope["source_tree"]) is not None
        and envelope["source_tree"] == BASE_SOURCE_TREE
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


def validate_schedule(schedule: Mapping[str, Any], root: Path) -> None:
    """Validate the formal v5 schedule and every newly registered source."""

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
            "prior_model_calls",
            "prior_provider_requests",
            "prior_official_evaluator_invocations",
        },
        "execution schedule",
    )
    require(
        schedule["schema_version"] == SCHEDULE_SCHEMA
        and schedule["authoritative"] is True
        and schedule["protocol_change"]
        == "execution_schedule_only_staged_one_or_two_case_waves"
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
        and schedule["max_cases_per_wave"] == 2
        and schedule["max_episodes_per_wave"] == 4
        and schedule["same_case_serialized"] is True
        and schedule["different_cases_max_parallel"] == 2
        and schedule["one_shot_per_rank"] is True
        and schedule["append_only_cumulative_ledger"] is True
        and schedule["evaluation_before_full_cohort_allowed"] is False
        and schedule["prior_model_calls"] == 0
        and schedule["prior_provider_requests"] == 0
        and schedule["prior_official_evaluator_invocations"] == 0,
        "execution schedule dimensions/budget drift",
    )
    require(
        schedule["approved_assembler_sha256"]
        == APPROVED_ASSEMBLER_SHA256,
        "execution schedule approved assembler drift",
    )
    registered = schedule["registered_files"]
    exact_keys(
        registered,
        {
            "schedule_contract.py",
            "run_wave.py",
            "verify_waves.py",
            "assemble_wave.py",
            "generate_schedule.py",
            "generate_selection_binding.py",
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


def validate_selection_binding(
    binding: Mapping[str, Any],
    *,
    schedule_sha256: str,
    selection: Mapping[str, Any],
) -> None:
    """Validate the v5 schedule binding without changing the v4 cohort."""

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
            "prior_model_calls",
            "prior_provider_requests",
            "prior_official_evaluator_invocations",
        },
        "selection binding",
    )
    require(
        binding["schema_version"] == SELECTION_BINDING_SCHEMA
        and binding["authoritative"] is True
        and binding["protocol_change"]
        == "execution_schedule_only_no_cohort_or_arm_change"
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
        and binding["prior_model_calls"] == 0
        and binding["prior_provider_requests"] == 0
        and binding["prior_official_evaluator_invocations"] == 0,
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
            "approved_assembler_sha256",
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
        and binding["approved_assembler_sha256"]
        == APPROVED_ASSEMBLER_SHA256
        and binding["case_count"] == 20
        and binding["episode_count"] == 40
        and binding["per_episode_budget_usd"] == 6.0
        and binding["maximum_budget_usd"] == 240.0
        and binding["model_outputs_inspected"] is False
        and binding["evaluator_or_outcome_accessed"] is False
        and binding["no_spend_assertion"] == NO_SPEND,
        "envelope binding drift",
    )
