#!/usr/bin/env python3
"""Shared, fail-closed registration contracts for issues #827 and #836.

The model runner, offline verifier, and official evaluator all import this
module.  Keeping the immutable contract in one place prevents a permissive
consumer from silently accepting a registration that another consumer would
reject.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
from typing import Any, Mapping, TypedDict


LEGACY_REGISTRATION_SCHEMA = "issue827-treatment-registration-v1"
ISSUE836_V2_REGISTRATION_SCHEMA = "issue836-treatment-registration-v2"
ISSUE836_V3_REGISTRATION_SCHEMA = "issue836-treatment-registration-v3"
CURRENT_REGISTRATION_SCHEMA = "issue836-treatment-registration-v4"
# New registrations use the current schema. Historical issue #827/#830 and
# issue #836 v2/v3 registrations remain valid through versioned schemas.
REGISTRATION_SCHEMA = CURRENT_REGISTRATION_SCHEMA
LEGACY_EPISODE_DESIGN_SCHEMA = "issue827-episode-design-v1"
ISSUE836_V2_EPISODE_DESIGN_SCHEMA = "issue836-episode-design-v2"
ISSUE836_V3_EPISODE_DESIGN_SCHEMA = "issue836-episode-design-v3"
CURRENT_EPISODE_DESIGN_SCHEMA = "issue836-episode-design-v4"
QUALIFICATION_REGISTRATION_SCHEMA = (
    "issue827-qualification-closure-registration-v1"
)
QUALIFICATION_MANIFEST_SCHEMA = "issue827-qualification-closure-v1"
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")

FROZEN_MODEL_RUNTIME: dict[str, Any] = {
    "cli": "Claude Code",
    "cli_version": "2.1.216",
    "cli_sha256": (
        "d01b49210d72ecbe277a2665d104bacccddf2d22185be99446d2929e0edfc48d"
    ),
    "model": "claude-sonnet-5",
    "effort": "high",
    "wall_seconds": 1200,
    "budget_usd": 6.0,
    "invocations_per_episode": 1,
    "resume_allowed": False,
    "model_retry_allowed": False,
    "permission_mode": "dontAsk",
    "safe_mode": False,
    "tools": ["Bash", "Edit", "Read", "Write", "Glob", "Grep"],
    "disallowed_tools": ["WebSearch", "WebFetch"],
    "strict_empty_mcp_sha256": (
        "e93fc8db2b1bd77107fe6c758bca9545fa864cf7cce8ab93a7b2b93a1d566a7b"
    ),
}

# Every executable/template input to selection, execution, verification, and
# evaluation is registered.  Generated registration/selection/evidence files
# cannot hash themselves and are bound by ordinary file references instead.
REGISTERED_FILE_NAMES: dict[str, str] = {
    "system_prefix_sha256": "system-prefix.txt",
    "system_suffix_sha256": "system-suffix.txt",
    "rna_query_sha256": "rna_query.py",
    "rna_traverse_sha256": "rna_traverse.py",
    "frontier_replay_sha256": "frontier_replay.py",
    "tool_supervisor_sha256": "tool_supervisor.py",
    "supervisor_template_sha256": "supervisor.template.json",
    "claude_settings_template_sha256": "claude-settings.template.json",
    "validator_sha256": "validate_episode.py",
    "common_supervisor_sha256": "common_supervisor.py",
    "hook_guard_sha256": "hook_guard.py",
    "live_identity_sha256": "live_identity.py",
    "provider_usage_sha256": "provider_usage.py",
    "isolation_sha256": "isolation.py",
    "bash_gateway_sha256": "bash_gateway.py",
    "trusted_rna_broker_sha256": "trusted_rna_broker.py",
    "evaluator_authorization_sha256": "evaluator_authorization.py",
    "runner_sha256": "run_selector.py",
    "verifier_sha256": "verify_selector.py",
    "selector_sha256": "select_cases.py",
    "evaluator_runner_sha256": "evaluator_runner.py",
    "evaluator_plan_template_sha256": "evaluator-plan.template.json",
    "result_selector_sha256": "select_result.py",
    "registration_contract_sha256": "registration_contract.py",
    "offline_worker_source_sha256": "worker/offline_worker.py",
    "worker_dockerfile_sha256": "worker/Dockerfile",
    "landlock_launcher_source_sha256": "worker/landlock_launcher.c",
}

RNA_ARTIFACT_FIELDS = {
    "producer_commit",
    "launcher_sha256",
    "binary_sha256",
    "bundle_manifest_sha256",
    "archive_sha256",
    "upload_attestation_sha256",
    "verification_receipt_sha256",
    "canonical_environment_sha256",
    "runtime_receipt_sha256",
    "local_source_build_allowed",
}

FROZEN_V3_PREFIX_LINEAGE: dict[str, Any] = {
    "ranks_1_through_2": "pre_model_carry_forward_prefix",
    "ranks_3_through_7_and_9_through_20": "deterministic_extension",
    "rank_8": "pre_model_replacement_from_s2_rank_21",
    "outcomes_inspected_for_extension": False,
}

FROZEN_V3_PRE_MODEL_SUPERSESSION: dict[str, Any] = {
    "schema_version": "issue836-pre-model-cohort-supersession-v1",
    "superseded_registration_schema": ISSUE836_V2_REGISTRATION_SCHEMA,
    "superseded_registration_sha256": (
        "10345f1ba1b1638f04b6b671a3aa64f5847e17944d955cd08494f91f003275b0"
    ),
    "superseded_registration_commit": (
        "a6c768ba633ab0301df1ea6a94ceb036a401e45e"
    ),
    "superseded_selection_schema": "issue836-fresh-cohort-selection-v2",
    "superseded_selection_sha256": (
        "8b1c0dbfac7a540668f526a656f1a230af497b963b56793f44039686d147b73b"
    ),
    "superseded_selection_digest": (
        "d9438a49ff01e9e86519c0f9871c319a5c90be368464ec84466f1e899c53ebd5"
    ),
    "dataset_arrow_sha256": (
        "0d119efe73413554335bd410a04d82fd4a586bfd312cee677ee40af5de2ac46e"
    ),
    "exclusions_sha256": (
        "0f9e9fb4e46303624670c79a3cd4b127188c6a166e674a5759248e308a9c9c79"
    ),
    "excluded_ids_sha256": (
        "6823c44cc1d7f4f1485690f12de9e8d08060a2e66b394ce2c28469893791886b"
    ),
    "superseded_rank": 8,
    "excluded_instance_id": "sympy__sympy-24661",
    "replacement_instance_id": "django__django-11163",
    "replacement_source_rank": 21,
    "replacement_method": "next_deterministic_s2_ranked_candidate",
    "preserved_arm_order": ["T", "A"],
    "reason_code": "old_shipped_rna_binary_mjs_descriptor_incompatibility",
    "reason": (
        "the registered old shipped RNA binary cannot prepare the exact "
        "selected tree because its JavaScript descriptor routing does not "
        "support .mjs"
    ),
    "incompatible_rna_binary_sha256": (
        "d4d264da1a012b38814f0f2e9ee92f77c5aab3ed558a0f23abcd830d4b78ca94"
    ),
    "replacement_repo": "django/django",
    "replacement_base_commit": "e6588aa4e793b7f56f4cadbfa155b581e0efc59a",
    "replacement_base_tree": "4c221e3aba9030ab459871106740934193ce1118",
    "replacement_ranking_sha256": (
        "0bbab2a1aa0c0547bc1fea70b6fa97ae226b9c7c6e178dd1f69b2279900c5beb"
    ),
    "replacement_problem_statement_sha256": (
        "8dbd8cae38c3a82b681a79bdfe8ffaa78aa6f1fad9744a1ca197f90265e5c80b"
    ),
    "detected_during_pre_model_cache_preparation": True,
    "prior_model_calls": 0,
    "prior_official_evaluator_invocations": 0,
    "case_replacement_after_model_start": False,
}

FROZEN_V4_PREFIX_LINEAGE: dict[str, Any] = {
    "ranks_1_through_7_9_through_11_and_13_through_20": (
        "exact_v3_carry_forward"
    ),
    "rank_8": "exact_v3_pre_model_rank_21_replacement_carry_forward",
    "rank_12": "pre_model_replacement_from_s2_rank_22",
    "outcomes_inspected_for_replacement": False,
}

FROZEN_V4_PRE_MODEL_SUPERSESSION: dict[str, Any] = {
    "schema_version": "issue836-pre-model-cohort-supersession-v2",
    "superseded_registration_schema": ISSUE836_V3_REGISTRATION_SCHEMA,
    "superseded_registration_path": (
        "benchmark/swebench-rna-first/issue836-v3/registration.json"
    ),
    "superseded_registration_sha256": (
        "6f319f138336aef194cd91962edb75f3db172816cab7e19344d20d39217c5e92"
    ),
    "superseded_registration_commit": (
        "a82f0b67e251f2cec6ae968e3b78f74e902ee16d"
    ),
    "superseded_selection_schema": "issue836-fresh-cohort-selection-v3",
    "superseded_selection_path": (
        "benchmark/swebench-rna-first/issue836-v3/selection.json"
    ),
    "superseded_selection_sha256": (
        "dfd7ce6f4fcdd6e9b7baf81eb3faca76f32cde86485f3876d6d139154cedac80"
    ),
    "superseded_selection_commit": (
        "654890f249ebf7fcdc08a1f79bc1f507a65a5e07"
    ),
    "superseded_selection_digest": (
        "6df1b24382d28624bcda6e4fe07d39126ba4f7f6064af5ef89bc9cde9f5549f4"
    ),
    "dataset_arrow_sha256": (
        "0d119efe73413554335bd410a04d82fd4a586bfd312cee677ee40af5de2ac46e"
    ),
    "exclusions_sha256": (
        "0f9e9fb4e46303624670c79a3cd4b127188c6a166e674a5759248e308a9c9c79"
    ),
    "excluded_ids_sha256": (
        "6823c44cc1d7f4f1485690f12de9e8d08060a2e66b394ce2c28469893791886b"
    ),
    "superseded_rank": 12,
    "excluded_instance_id": "sympy__sympy-24539",
    "excluded_repo": "sympy/sympy",
    "excluded_base_commit": "193e3825645d93c73e31cdceb6d742cc6919624d",
    "excluded_base_tree": "8ffa903a7635c0fc04fae14e0a22cb4104cb4682",
    "excluded_ranking_sha256": (
        "048fbc0780ad22b1233b61da02326ea68265cb2fb914a76fa6b2cad0964aef7a"
    ),
    "excluded_problem_statement_sha256": (
        "c45c55bcffb2b526d21ed6b0c8d19976455280d6ce9343f7f2d62d96c3d3e037"
    ),
    "replacement_instance_id": "psf__requests-1724",
    "replacement_source_rank": 22,
    "replacement_method": "next_deterministic_eligible_source_rank",
    "preserved_arm_order": ["T", "A"],
    "reason_code": "old_shipped_rna_binary_exact_rank12_mjs_incompatibility",
    "reason": (
        "the registered old shipped RNA binary rejects the exact v3 rank-12 "
        "tree solely because bin/test_pyodide.mjs is unsupported; all other "
        "19 v3 trees were audited mjs-free"
    ),
    "incompatible_rna_binary_sha256": (
        "d4d264da1a012b38814f0f2e9ee92f77c5aab3ed558a0f23abcd830d4b78ca94"
    ),
    "rejected_tree_mjs_paths": ["bin/test_pyodide.mjs"],
    "other_v3_tree_count": 19,
    "other_v3_tree_mjs_path_count": 0,
    "replacement_repo": "psf/requests",
    "replacement_base_commit": "1ba83c47ce7b177efe90d5f51f7760680f72eda0",
    "replacement_base_tree": "c8e845adc2051eac27d5998697d3e83e920ef2c8",
    "replacement_ranking_sha256": (
        "0bf45a8589083de3e3dea6a230493a8279e01e379418593d1bfee686469d4761"
    ),
    "replacement_problem_statement_sha256": (
        "eacde5d201658474274cce4558b5d2b8ae74d9fcc2ad6ddc76aaf003b2160b8a"
    ),
    "replacement_source_verification_manifest_path": (
        "/Users/muness/swebench-evidence/issue836-selector-20case-20260724/"
        "replacement-v4-rank12/cache-setup/source-cache-verification.json"
    ),
    "replacement_source_verification_manifest_sha256": (
        "3c85713bf402204ffccc6e55f0b4edbf8a3aee8e2b1b93a3b177f0ed0c755207"
    ),
    "detected_during_pre_model_cache_preparation": True,
    "prior_model_calls": 0,
    "prior_provider_requests": 0,
    "prior_official_evaluator_invocations": 0,
    "case_replacement_after_model_start": False,
}


class RegistrationContractError(RuntimeError):
    """A frozen preregistration identity or semantic contract did not hold."""


class ExperimentDimensions(TypedDict):
    """Authoritative count, concurrency, and budget dimensions."""

    case_count: int
    episode_count: int
    max_parallel_cases: int
    per_episode_budget_usd: float
    maximum_budget_usd: float


def canonical(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RegistrationContractError(message)


def exact_keys(value: Mapping[str, Any], keys: set[str], where: str) -> None:
    missing = keys - set(value)
    extra = set(value) - keys
    require(not missing, f"{where}: missing fields {sorted(missing)}")
    require(not extra, f"{where}: unexpected fields {sorted(extra)}")


def require_sha256(value: Any, where: str) -> str:
    require(
        isinstance(value, str) and HEX64.fullmatch(value) is not None,
        f"{where} is not SHA-256",
    )
    return value


def experiment_dimensions(
    registration: Mapping[str, Any],
) -> ExperimentDimensions:
    """Return versioned experiment dimensions, rejecting cross-field drift."""

    schema = registration.get("schema_version")
    if schema == LEGACY_REGISTRATION_SCHEMA:
        expected = {
            "issue": 827,
            "episode_schema": LEGACY_EPISODE_DESIGN_SCHEMA,
            "selector_schema": "issue827-selector-v1",
            "selection_rule_schema": "issue827-selection-rule-v1",
            "case_count": 2,
            "episode_count": 4,
        }
    elif schema == ISSUE836_V2_REGISTRATION_SCHEMA:
        expected = {
            "issue": 836,
            "episode_schema": ISSUE836_V2_EPISODE_DESIGN_SCHEMA,
            "selector_schema": "issue836-selector-v2",
            "selection_rule_schema": "issue836-selection-rule-v2",
            "case_count": 20,
            "episode_count": 40,
        }
    elif schema == ISSUE836_V3_REGISTRATION_SCHEMA:
        expected = {
            "issue": 836,
            "episode_schema": ISSUE836_V3_EPISODE_DESIGN_SCHEMA,
            "selector_schema": "issue836-selector-v3",
            "selection_rule_schema": "issue836-selection-rule-v3",
            "case_count": 20,
            "episode_count": 40,
        }
    elif schema == CURRENT_REGISTRATION_SCHEMA:
        expected = {
            "issue": 836,
            "episode_schema": CURRENT_EPISODE_DESIGN_SCHEMA,
            "selector_schema": "issue836-selector-v4",
            "selection_rule_schema": "issue836-selection-rule-v4",
            "case_count": 20,
            "episode_count": 40,
        }
    else:
        raise RegistrationContractError("registration schema mismatch")

    require(
        registration.get("issue") == expected["issue"],
        "registration issue mismatch",
    )
    episode = registration.get("episode_design")
    selector = registration.get("selector")
    selection_rule = registration.get("selection_rule")
    evaluator = registration.get("evaluator")
    runtime = registration.get("model_runtime")
    require(isinstance(episode, dict), "registration episode design missing")
    require(isinstance(selector, dict), "registration selector missing")
    require(
        isinstance(selection_rule, dict),
        "registration selection rule missing",
    )
    require(isinstance(evaluator, dict), "registration evaluator missing")
    require(isinstance(runtime, dict), "registration model runtime missing")

    case_count = expected["case_count"]
    episode_count = expected["episode_count"]
    max_parallel_cases = 2
    per_episode_budget_usd = 6.0
    for actual, frozen, where in (
        (
            episode.get("schema_version"),
            expected["episode_schema"],
            "registration episode design schema",
        ),
        (
            episode.get("case_count"),
            case_count,
            "registration episode case count",
        ),
        (
            episode.get("episode_count"),
            episode_count,
            "registration episode count",
        ),
        (
            episode.get("different_cases_max_parallel"),
            max_parallel_cases,
            "registration maximum parallel cases",
        ),
        (
            selector.get("algorithm_version"),
            expected["selector_schema"],
            "registration selector schema",
        ),
        (
            selector.get("selected_case_count"),
            case_count,
            "registration selector case count",
        ),
        (
            selector.get("episode_count"),
            episode_count,
            "registration selector episode count",
        ),
        (
            selection_rule.get("schema_version"),
            expected["selection_rule_schema"],
            "registration selection rule schema",
        ),
        (
            selection_rule.get("pair_count"),
            case_count,
            "registration selection rule pair count",
        ),
        (
            selection_rule.get("episode_count"),
            episode_count,
            "registration selection rule episode count",
        ),
        (
            evaluator.get("max_parallel"),
            max_parallel_cases,
            "registration evaluator maximum parallel cases",
        ),
        (
            runtime.get("budget_usd"),
            per_episode_budget_usd,
            "registration per-episode budget",
        ),
        (
            runtime.get("wall_seconds"),
            1200,
            "registration episode wall time",
        ),
    ):
        require(actual == frozen, f"{where} drift")

    return {
        "case_count": case_count,
        "episode_count": episode_count,
        "max_parallel_cases": max_parallel_cases,
        "per_episode_budget_usd": per_episode_budget_usd,
        "maximum_budget_usd": episode_count * per_episode_budget_usd,
    }


def is_issue836_registration_schema(value: Any) -> bool:
    """Return whether a schema is a supported issue #836 registration."""

    return value in {
        ISSUE836_V2_REGISTRATION_SCHEMA,
        ISSUE836_V3_REGISTRATION_SCHEMA,
        CURRENT_REGISTRATION_SCHEMA,
    }


def validate_registration(
    registration: Mapping[str, Any],
    *,
    source_root: Path | None = None,
    require_resolved_hashes: bool = True,
) -> None:
    """Validate shared frozen semantics and, optionally, live source bytes."""

    dimensions = experiment_dimensions(registration)
    schema = registration.get("schema_version")

    runtime = registration.get("model_runtime")
    require(
        runtime == FROZEN_MODEL_RUNTIME,
        "registration model runtime is not the frozen #827 runtime",
    )
    episode = registration.get("episode_design")
    require(isinstance(episode, dict), "registration episode design missing")
    for key, expected in {
        "schema_version": (
            LEGACY_EPISODE_DESIGN_SCHEMA
            if schema == LEGACY_REGISTRATION_SCHEMA
            else (
                ISSUE836_V2_EPISODE_DESIGN_SCHEMA
                if schema == ISSUE836_V2_REGISTRATION_SCHEMA
                else (
                    ISSUE836_V3_EPISODE_DESIGN_SCHEMA
                    if schema == ISSUE836_V3_REGISTRATION_SCHEMA
                    else CURRENT_EPISODE_DESIGN_SCHEMA
                )
            )
        ),
        "case_count": dimensions["case_count"],
        "episode_count": dimensions["episode_count"],
        "same_case_serialized": True,
        "different_cases_max_parallel": dimensions["max_parallel_cases"],
        "fresh_session_per_episode": True,
        "resume_allowed": False,
        "model_retry_allowed": False,
        "official_evaluator_feedback_to_model": False,
    }.items():
        require(
            episode.get(key) == expected,
            f"registration episode design drift: {key}",
        )

    selector = registration.get("selector")
    require(isinstance(selector, dict), "registration selector missing")
    if schema == ISSUE836_V3_REGISTRATION_SCHEMA:
        require(
            selector.get("pre_model_v2_supersession")
            == FROZEN_V3_PRE_MODEL_SUPERSESSION,
            "registration pre-model v2 supersession drift",
        )
        require(
            selector.get("prefix_lineage") == FROZEN_V3_PREFIX_LINEAGE,
            "registration v3 prefix lineage drift",
        )
        if require_resolved_hashes:
            require(
                selector["pre_model_v2_supersession"][
                    "incompatible_rna_binary_sha256"
                ]
                == registration.get("rna_artifact", {}).get("binary_sha256"),
                "superseded binary identity differs from registered RNA binary",
            )
        require(
            "pre_model_v3_supersession" not in selector,
            "v3 registration unexpectedly declares v4 supersession",
        )
    elif schema == CURRENT_REGISTRATION_SCHEMA:
        require(
            selector.get("pre_model_v3_supersession")
            == FROZEN_V4_PRE_MODEL_SUPERSESSION,
            "registration pre-model v3 supersession drift",
        )
        require(
            selector.get("prefix_lineage") == FROZEN_V4_PREFIX_LINEAGE,
            "registration v4 prefix lineage drift",
        )
        require(
            "pre_model_v2_supersession" not in selector,
            "v4 registration unexpectedly embeds v2 supersession",
        )
        require(
            registration.get("prior_model_calls") == 0
            and registration.get("prior_official_evaluator_invocations") == 0
            and FROZEN_V4_PRE_MODEL_SUPERSESSION["prior_model_calls"] == 0
            and FROZEN_V4_PRE_MODEL_SUPERSESSION[
                "prior_official_evaluator_invocations"
            ]
            == 0,
            "v4 registration is not a zero-call pre-model successor",
        )
        require(
            selector.get("problem_statements_inspected_by_human_before_selection")
            is False
            and selector.get("gold_or_outcomes_inspected_before_selection")
            is False
            and selector.get("case_replacement_after_model_start") is False,
            "v4 registration pre-model inspection/replacement contract drift",
        )
        if require_resolved_hashes:
            require(
                selector["pre_model_v3_supersession"][
                    "incompatible_rna_binary_sha256"
                ]
                == registration.get("rna_artifact", {}).get("binary_sha256"),
                "superseded binary identity differs from registered RNA binary",
            )
    else:
        require(
            "pre_model_v2_supersession" not in selector,
            "historical registration unexpectedly declares v3 supersession",
        )
        require(
            "pre_model_v3_supersession" not in selector,
            "historical registration unexpectedly declares v4 supersession",
        )

    usage = registration.get("usage")
    require(isinstance(usage, dict), "registration usage contract missing")
    for key, expected in {
        "schema_version": "issue827-provider-usage-v1",
        "whole_invocation_model_usage_authoritative": True,
        "top_level_usage_retained_separately": True,
        "provider_responses_scope": "agent_transcript_only",
        "auxiliary_cli_usage_retained": True,
        "auxiliary_cli_usage_included_in_whole_invocation_totals": True,
        "positive_provider_total_required_after_model_invocation": True,
        "positive_agent_transcript_provider_response_count_required": True,
        "provider_requests_never_inferred_from_cli_turns": True,
        "missing_partial_negative_boolean_or_inconsistent_usage_invalid": True,
    }.items():
        require(
            usage.get(key) == expected,
            f"registration provider usage contract drift: {key}",
        )

    registered = registration.get("registered_files")
    require(
        isinstance(registered, dict),
        "registration registered_files missing",
    )
    exact_keys(
        registered,
        set(REGISTERED_FILE_NAMES),
        "registration.registered_files",
    )
    if require_resolved_hashes:
        for key in REGISTERED_FILE_NAMES:
            require_sha256(registered[key], f"registration.registered_files.{key}")
    if source_root is not None:
        require(source_root.is_dir(), "registered source root missing")
        for key, filename in REGISTERED_FILE_NAMES.items():
            path = source_root / filename
            require(
                path.is_file() and not path.is_symlink(),
                f"registered source missing or symlinked: {filename}",
            )
            require(
                registered[key] == sha256_file(path),
                f"registered source hash mismatch: {filename}",
            )

    artifact = registration.get("rna_artifact")
    require(isinstance(artifact, dict), "registration RNA artifact missing")
    exact_keys(artifact, RNA_ARTIFACT_FIELDS, "registration.rna_artifact")
    if require_resolved_hashes:
        require(
            isinstance(artifact["producer_commit"], str)
            and HEX40.fullmatch(artifact["producer_commit"]) is not None,
            "registration RNA producer commit invalid",
        )
        for key in RNA_ARTIFACT_FIELDS - {
            "producer_commit",
            "local_source_build_allowed",
        }:
            require_sha256(artifact[key], f"registration.rna_artifact.{key}")
    require(
        artifact["local_source_build_allowed"] is False,
        "registration permits local source artifact",
    )

    closure = registration.get("qualification_closure")
    require(
        isinstance(closure, dict),
        "registration qualification closure missing",
    )
    exact_keys(
        closure,
        {
            "schema_version",
            "manifest_schema",
            "manifest_sha256",
            "archive_sha256",
            "qualified",
            "no_model_or_provider_calls",
        },
        "registration.qualification_closure",
    )
    require(
        closure["schema_version"] == QUALIFICATION_REGISTRATION_SCHEMA,
        "qualification registration schema mismatch",
    )
    require(
        closure["manifest_schema"] == QUALIFICATION_MANIFEST_SCHEMA,
        "qualification manifest schema mismatch",
    )
    if require_resolved_hashes:
        require_sha256(
            closure["manifest_sha256"],
            "registration.qualification_closure.manifest_sha256",
        )
        require_sha256(
            closure["archive_sha256"],
            "registration.qualification_closure.archive_sha256",
        )
    require(closure["qualified"] is True, "qualification not successful")
    require(
        closure["no_model_or_provider_calls"] is True,
        "qualification was not no-spend",
    )


def validate_qualification_manifest(
    registration: Mapping[str, Any],
    manifest_bytes: bytes,
    archive_sha256: str,
) -> Mapping[str, Any]:
    """Validate the immutable no-spend closure against preregistered identities."""

    try:
        manifest = json.loads(manifest_bytes)
    except json.JSONDecodeError as exc:
        raise RegistrationContractError(
            f"qualification closure manifest invalid JSON: {exc}"
        ) from exc
    require(isinstance(manifest, dict), "qualification manifest is not an object")
    exact_keys(
        manifest,
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
        "qualification manifest",
    )
    closure = registration["qualification_closure"]
    require(
        sha256_bytes(manifest_bytes) == closure["manifest_sha256"],
        "qualification manifest differs from registration",
    )
    require(
        manifest["schema_version"] == closure["manifest_schema"],
        "qualification manifest schema differs from registration",
    )
    require(
        manifest["qualified"] is True
        and manifest["no_model_or_provider_calls"] is True,
        "qualification closure is not successful no-spend evidence",
    )
    require_sha256(archive_sha256, "qualification archive SHA-256")
    require(
        archive_sha256
        == closure["archive_sha256"]
        == manifest["archive_sha256"],
        "qualification archive differs from registration or manifest",
    )
    bindings = {
        "registered_files_sha256": registration["registered_files"],
        "model_runtime_sha256": registration["model_runtime"],
        "isolation_runtime_sha256": registration["isolation_runtime"],
        "rna_artifact_sha256": registration["rna_artifact"],
    }
    for key, value in bindings.items():
        require(
            manifest[key] == sha256_bytes(canonical(value)),
            f"qualification manifest binding mismatch: {key}",
        )
    require_sha256(
        manifest["external_inputs_sha256"],
        "qualification external inputs SHA-256",
    )
    require_sha256(
        manifest["runtime_identity_sha256"],
        "qualification runtime identity SHA-256",
    )
    require_sha256(
        manifest["evidence_inventory_sha256"],
        "qualification evidence inventory SHA-256",
    )
    return manifest


def validate_rna_trust_documents(
    registration: Mapping[str, Any],
    documents: Mapping[str, bytes],
) -> None:
    """Cross-check CI provenance and selector-runtime trust documents."""

    expected = registration["rna_artifact"]
    required = {
        "bundle_manifest",
        "upload_attestation",
        "verification_receipt",
        "canonical_environment",
        "runtime_receipt",
    }
    exact_keys(documents, required, "RNA trust documents")
    try:
        parsed = {
            name: json.loads(documents[name])
            for name in required
        }
    except json.JSONDecodeError as exc:
        raise RegistrationContractError(
            f"RNA artifact trust anchor JSON invalid: {exc}"
        ) from exc
    for name, value in parsed.items():
        require(isinstance(value, dict), f"RNA trust document not object: {name}")

    bundle = parsed["bundle_manifest"]
    attestation = parsed["upload_attestation"]
    verification = parsed["verification_receipt"]
    environment = parsed["canonical_environment"]
    runtime = parsed["runtime_receipt"]
    require(
        all(isinstance(key, str) and isinstance(value, str)
            for key, value in environment.items()),
        "RNA canonical environment invalid",
    )
    require(
        attestation.get("schema") == "rna-swebench-semantic-bundle-upload-v1",
        "RNA upload attestation schema mismatch",
    )
    require(
        verification.get("schema")
        == "rna-swebench-semantic-bundle-verification-v1",
        "RNA verification receipt schema mismatch",
    )
    require(
        bundle.get("provenance", {}).get("head_sha")
        == attestation.get("head_sha")
        == verification.get("head_sha")
        == expected["producer_commit"],
        "RNA producer provenance mismatch",
    )
    require(
        sha256_bytes(documents["bundle_manifest"])
        == attestation.get("manifest_sha256")
        == verification.get("manifest_sha256")
        == expected["bundle_manifest_sha256"],
        "RNA bundle manifest provenance mismatch",
    )
    require(
        bundle.get("components", {}).get("executable", {}).get("sha256")
        == runtime.get("binary_sha256")
        == expected["binary_sha256"],
        "RNA binary provenance mismatch",
    )
    require(
        bundle.get("artifact", {}).get("archive_sha256")
        == verification.get("archive_sha256")
        == expected["archive_sha256"],
        "RNA archive provenance mismatch",
    )
    require(
        sha256_bytes(documents["upload_attestation"])
        == verification.get("upload_attestation_sha256")
        == expected["upload_attestation_sha256"],
        "RNA upload attestation provenance mismatch",
    )
    require(
        sha256_bytes(documents["verification_receipt"])
        == expected["verification_receipt_sha256"],
        "RNA verification receipt differs from registration",
    )
    require(
        sha256_bytes(documents["canonical_environment"])
        == runtime.get("environment_sha256")
        == expected["canonical_environment_sha256"],
        "RNA canonical environment provenance mismatch",
    )
    require(
        sha256_bytes(documents["runtime_receipt"])
        == expected["runtime_receipt_sha256"],
        "RNA runtime receipt differs from registration",
    )
