#!/usr/bin/env python3
"""Offline verifier and evidence aggregator for #827 selector episodes."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shlex
import sys
from typing import Any, Mapping, Sequence

import run_selector as runner
import evaluator_authorization
import isolation
import provider_usage
import registration_contract
import frontier_replay


VERIFY_SCHEMA = "issue827-episode-verification-v1"


def check_ref(ref: Any, label: str, errors: list[str]) -> tuple[Path | None, bytes | None]:
    try:
        if not isinstance(ref, dict):
            raise runner.FailClosed(f"{label} is not a file ref")
        return runner.check_ref(ref, label)
    except (runner.FailClosed, OSError) as exc:
        errors.append(f"{label}:{exc}")
        return None, None


def load_ref_json(ref: Any, label: str, errors: list[str]) -> tuple[Path | None, Any]:
    path, data = check_ref(ref, label, errors)
    if data is None:
        return path, None
    try:
        return path, json.loads(data)
    except json.JSONDecodeError as exc:
        errors.append(f"{label}:invalid_json:{exc}")
        return path, None


def selected_case(manifest: Mapping[str, Any], case_id: str, rank: int, errors: list[str]) -> Mapping[str, Any] | None:
    matches = [case for case in manifest.get("cases", []) if case.get("instance_id") == case_id and case.get("rank") == rank]
    if len(matches) != 1:
        errors.append("episode_not_unique_in_run_manifest")
        return None
    return matches[0]


def exact_first_treatment_action(
    actor: Mapping[str, Any],
    config: Mapping[str, Any],
    system: bytes,
    projection: bytes,
    errors: list[str],
) -> None:
    model = [action for action in actor.get("actions", []) if action.get("actor") == "model"]
    if not model:
        errors.append("missing_first_model_tool")
        return
    first = model[0]
    command = first.get("bash_command")
    try:
        argv = shlex.split(command) if isinstance(command, str) else []
    except ValueError:
        argv = []
    forbidden = isinstance(command, str) and any(token in command for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n"))
    expected_wrapper = config.get("wrapper")
    marker = b"Your FIRST actual tool call must be Bash with exactly this command shape"
    if marker not in system:
        errors.append("treatment_system_missing_first_action_directive")
    if isinstance(expected_wrapper, str) and expected_wrapper.encode() not in system:
        errors.append("treatment_system_wrapper_mismatch")
    if not projection or projection not in system:
        errors.append("exact_query_projection_not_injected")
    try:
        projected_ids = runner.stable_code_ids(projection.decode("utf-8", errors="strict"))
    except UnicodeError:
        projected_ids = []
        errors.append("query_projection_not_utf8")
    else:
        expected_authorization = frontier_replay.source(
            0,
            "injected_query_projection",
            "INJECTED_QUERY",
            projection,
            projection,
        )
        if (
            config.get("initial_authorization_sha256")
            != expected_authorization[
                "projection_authorization_sha256"
            ]
        ):
            errors.append(
                "configured_projection_authorization_sha_mismatch"
            )
    initial_ids = config.get("initial_ids")
    if initial_ids != projected_ids:
        errors.append("configured_ids_not_exactly_projected")
    if runner.sha_bytes(projection) != config.get("initial_response_sha256"):
        errors.append("configured_projection_sha_mismatch")
    if (
        first.get("tool") != "Bash"
        or forbidden
        or len(argv) != 5
        or argv[0] != expected_wrapper
        or argv[1] != "--node"
        or argv[2] not in projected_ids
        or f"`{argv[2]}`".encode() not in projection
        or argv[3:] != ["--mode", "neighbors"]
        or first.get("common_decision") != "replace_allow"
        or first.get("treatment_decision") != "allow"
    ):
        errors.append("first_tool_not_exact_injected_rna_neighbors")


TOKEN_COUNTER_FIELDS = (
    "input_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "output_tokens",
    "provider_total_tokens",
    "reasoning_tokens",
)


def validate_token_ledger(
    ledger: Any,
    receipt: Mapping[str, Any],
    errors: list[str],
) -> None:
    if not isinstance(ledger, dict):
        errors.append("token_ledger_invalid")
        return
    if ledger.get("schema_version") != provider_usage.SCHEMA_VERSION:
        errors.append("token_ledger_schema_invalid")
    if ledger.get("valid") is not True or ledger.get("errors") != []:
        errors.append("token_usage_not_observed")
    if ledger.get("provider_responses_scope") != "agent_transcript_only":
        errors.append("token_provider_responses_scope_invalid")

    model_invoked = ledger.get("model_invoked")
    if model_invoked is False:
        if ledger.get("source") != "model_not_invoked":
            errors.append("token_no_model_source_invalid")
        for key in TOKEN_COUNTER_FIELDS:
            if ledger.get(key) is not None:
                errors.append(f"token_{key}_must_be_null_without_model")
        for key in ("cli_turns", "provider_responses", "provider_requests"):
            if ledger.get(key) not in (None, 0):
                errors.append(f"token_{key}_must_be_null_or_zero_without_model")
        timing = receipt.get("timing_ledger")
        no_model_context = (
            receipt.get("returncode") is None
            and isinstance(timing, dict)
            and timing.get("model_wall_seconds") == 0
            and receipt.get("policy_compliant") is False
            and receipt.get("evaluator_authorized") is False
        )
        if not no_model_context:
            errors.append("token_no_model_context_invalid")
        return
    if model_invoked is not True:
        errors.append("token_model_invoked_invalid")
        return

    counters_valid = True
    for key in TOKEN_COUNTER_FIELDS:
        if key == "reasoning_tokens":
            continue
        if type(ledger.get(key)) is not int or ledger[key] < 0:
            errors.append(f"token_{key}_invalid")
            counters_valid = False
    reasoning_observed = ledger.get("reasoning_tokens_observed")
    unobserved = ledger.get("unobserved_fields")
    if type(reasoning_observed) is not bool:
        errors.append("token_reasoning_tokens_observed_invalid")
    elif reasoning_observed:
        if (
            type(ledger.get("reasoning_tokens")) is not int
            or ledger["reasoning_tokens"] < 0
            or not isinstance(unobserved, list)
            or "reasoning_tokens" in unobserved
        ):
            errors.append("token_reasoning_tokens_observation_inconsistent")
    elif (
        ledger.get("reasoning_tokens") is not None
        or unobserved != ["reasoning_tokens"]
    ):
        errors.append("token_reasoning_tokens_observation_inconsistent")
    if ledger.get("cli_turns") is not None and (
        type(ledger["cli_turns"]) is not int or ledger["cli_turns"] < 0
    ):
        errors.append("token_cli_turns_invalid")
    if type(ledger.get("provider_responses")) is not int or ledger["provider_responses"] <= 0:
        errors.append("token_provider_responses_invalid")
    if ledger.get("provider_requests") is not None and (
        type(ledger["provider_requests"]) is not int or ledger["provider_requests"] < 0
    ):
        errors.append("token_provider_requests_invalid")
    if counters_valid:
        expected_total = sum(
            ledger[key]
            for key in (
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "output_tokens",
            )
        )
        if ledger.get("provider_total_tokens") != expected_total:
            errors.append("token_provider_total_mismatch")
    model_events = ledger.get("model_events_usage")
    auxiliary = ledger.get("auxiliary_cli_usage")
    if not isinstance(model_events, dict):
        errors.append("token_model_events_usage_invalid")
    elif auxiliary is not None and not isinstance(auxiliary, dict):
        errors.append("token_auxiliary_cli_usage_invalid")
    elif isinstance(auxiliary, dict):
        for key in (
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "output_tokens",
            "provider_total_tokens",
        ):
            if type(auxiliary.get(key)) is not int or auxiliary[key] < 0:
                errors.append(f"token_auxiliary_{key}_invalid")
        if counters_valid:
            for key in (
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "output_tokens",
            ):
                if (
                    type(model_events.get(key)) is not int
                    or ledger[key] != model_events[key] + auxiliary[key]
                ):
                    errors.append(f"token_auxiliary_{key}_mismatch")


def validate_registered_claude_command(
    command: Any,
    *,
    receipt_path: Path,
    receipt: Mapping[str, Any],
    manifest: Mapping[str, Any],
    registration: Mapping[str, Any],
    supervisor_config: Mapping[str, Any],
    treatment_system: Any,
    errors: list[str],
) -> None:
    if not isinstance(command, list) or not all(
        isinstance(item, str) for item in command
    ):
        errors.append("command_invalid")
        return
    runtime = registration.get("model_runtime")
    if runtime != registration_contract.FROZEN_MODEL_RUNTIME:
        errors.append("command_registration_runtime_invalid")
        return
    claude = manifest.get("claude")
    mcp_ref = manifest.get("mcp_config")
    if not isinstance(claude, dict):
        errors.append("command_claude_identity_missing")
        return
    claude_path = Path(str(claude.get("path")))
    try:
        if (
            not claude_path.is_file()
            or claude_path.is_symlink()
            or runner.sha_file(claude_path) != runtime["cli_sha256"]
            or claude.get("sha256") != runtime["cli_sha256"]
        ):
            errors.append("command_claude_identity_mismatch")
    except OSError as exc:
        errors.append(f"command_claude_identity:{exc}")
    mcp_path, mcp_bytes = check_ref(mcp_ref, "command_mcp_config", errors)
    if (
        mcp_bytes != runner.EMPTY_MCP_BYTES
        or not isinstance(mcp_ref, dict)
        or mcp_ref.get("sha256") != runtime["strict_empty_mcp_sha256"]
    ):
        errors.append("command_mcp_config_mismatch")

    settings_path = receipt_path.parent / "claude-settings.json"
    try:
        if (
            not settings_path.is_file()
            or settings_path.is_symlink()
            or runner.sha_file(settings_path)
            != supervisor_config.get("claude_settings_sha256")
        ):
            errors.append("command_settings_identity_mismatch")
    except OSError as exc:
        errors.append(f"command_settings_identity:{exc}")

    system_path: str | None = None
    if treatment_system is not None:
        if not isinstance(treatment_system, dict):
            errors.append("command_treatment_system_ref_invalid")
        else:
            system_path = str(treatment_system.get("path"))
    expected = [
        str(supervisor_config.get("sandbox_exec")),
        "-f",
        str(supervisor_config.get("seatbelt_profile")),
        str(claude_path),
        "-p",
        "--strict-mcp-config",
        "--mcp-config",
        str(mcp_path),
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
        str(runtime["budget_usd"]),
        "--output-format",
        "json",
        "--session-id",
        str(receipt.get("session_id")),
        "--settings",
        str(settings_path),
    ]
    if system_path is not None:
        expected.extend(["--append-system-prompt-file", system_path])
    if command != expected:
        errors.append("claude_command_not_exactly_registered")


def validate_timing_ledger(
    timing: Any,
    receipt: Mapping[str, Any],
    arm: Any,
    errors: list[str],
) -> None:
    if not isinstance(timing, dict):
        errors.append("timing_ledger_invalid")
        return
    rna = timing.get("rna_preprocessing_seconds")
    model = timing.get("model_wall_seconds")
    combined = timing.get("combined_pre_evaluator_wall_seconds")
    if not all(
        type(item) in {int, float} and item >= 0
        for item in (rna, model, combined)
    ):
        errors.append("timing_values_invalid")
        return
    if abs(combined - (rna + model)) > 1e-9:
        errors.append("combined_timing_mismatch")
    wall_limit = registration_contract.FROZEN_MODEL_RUNTIME["wall_seconds"]
    if model > wall_limit:
        errors.append("model_wall_exceeds_registered_limit")
    timed_out = receipt.get("timed_out")
    receipt_errors = receipt.get("errors")
    if not isinstance(receipt_errors, list):
        receipt_errors = []
    if timed_out is True:
        if model != wall_limit or "model_wall_timeout" not in receipt_errors:
            errors.append("model_timeout_timing_inconsistent")
    elif timed_out is False and "model_wall_timeout" in receipt_errors:
        errors.append("model_timeout_flag_inconsistent")
    if arm == "A" and rna != 0:
        errors.append("control_has_rna_preprocessing_time")


def verify_episode(receipt_path: Path) -> dict[str, Any]:
    receipt_path = receipt_path.resolve(strict=True)
    episode_ref = runner.file_ref(receipt_path)
    errors: list[str] = []
    try:
        receipt = json.loads(receipt_path.read_bytes())
    except json.JSONDecodeError as exc:
        receipt = {}
        errors.append(f"episode_receipt_invalid_json:{exc}")
    if receipt.get("schema_version") != runner.RECEIPT_SCHEMA:
        errors.append("episode_receipt_schema_mismatch")

    _, manifest = load_ref_json(receipt.get("run_manifest"), "run_manifest", errors)
    _, registration = load_ref_json(receipt.get("registration"), "registration", errors)
    _, selection = load_ref_json(receipt.get("selection"), "selection", errors)
    if isinstance(manifest, dict) and manifest.get("schema_version") != runner.RUN_SCHEMA:
        errors.append("run_manifest_schema_mismatch")
    if isinstance(registration, dict) and registration.get("schema_version") != runner.REGISTRATION_SCHEMA:
        errors.append("registration_schema_mismatch")
    if isinstance(selection, dict) and selection.get("schema_version") != runner.SELECTION_SCHEMA:
        errors.append("selection_schema_mismatch")
    if isinstance(registration, dict):
        try:
            registration_contract.validate_registration(
                registration,
                source_root=runner.SOURCE,
            )
        except registration_contract.RegistrationContractError as exc:
            errors.append(f"registration_contract:{exc}")
    if isinstance(manifest, dict) and isinstance(registration, dict):
        try:
            runner.verify_rna_artifact(manifest, registration)
        except (runner.FailClosed, OSError) as exc:
            errors.append(f"rna_artifact:{exc}")
        try:
            runner.verify_qualification_closure(manifest, registration)
        except (runner.FailClosed, OSError) as exc:
            errors.append(f"qualification_closure:{exc}")
        runner_ref = manifest.get("runner")
        _, _ = check_ref(runner_ref, "registered_runner", errors)
        if isinstance(runner_ref, dict) and runner_ref.get("sha256") != registration.get("registered_files", {}).get("runner_sha256"):
            errors.append("runner_not_registration_bound")
        common_ref = manifest.get("common_supervisor")
        _, _ = check_ref(common_ref, "registered_common_supervisor", errors)
        if isinstance(common_ref, dict) and common_ref.get("sha256") != registration.get("registered_files", {}).get("common_supervisor_sha256"):
            errors.append("common_supervisor_not_registration_bound")
    case = selected_case(manifest, receipt.get("case_id"), receipt.get("rank"), errors) if isinstance(manifest, dict) else None
    arm = receipt.get("arm")
    if arm not in {"A", "T"}:
        errors.append("arm_invalid")
    expected_policy = {"A": "control", "T": "treatment"}.get(arm)
    if receipt.get("policy") != expected_policy:
        errors.append("policy_arm_mismatch")
    if case is not None:
        if receipt.get("base_commit") != case.get("base_commit") or receipt.get("base_tree") != case.get("base_tree"):
            errors.append("base_identity_mismatch")
        selected_match = [item for item in (selection or {}).get("cases", []) if item.get("instance_id") == receipt.get("case_id")]
        if len(selected_match) != 1 or arm not in selected_match[0].get("arm_order", []):
            errors.append("selection_arm_mismatch")

    _, identity = load_ref_json(receipt.get("runtime_identity"), "runtime_identity", errors)
    if isinstance(identity, dict):
        required_identity = {
            "schema_version": runner.IDENTITY_SCHEMA,
            "case_id": receipt.get("case_id"),
            "base_commit": receipt.get("base_commit"),
            "base_tree": receipt.get("base_tree"),
            "cache_bindings_verified": True,
            "fresh_reopen_ready": True,
            "readiness_sentinel": runner.READY_SENTINEL,
        }
        for key, value in required_identity.items():
            if identity.get(key) != value:
                errors.append(f"runtime_identity_{key}_mismatch")
        expected_repository = identity.get("expected_repository_identity")
        live_repository = identity.get("live_repository_identity")
        try:
            expected_repository = runner.canonical_repository_slug(
                expected_repository, "runtime_identity.expected_repository_identity"
            )
            live_repository = runner.canonical_repository_slug(
                live_repository, "runtime_identity.live_repository_identity"
            )
        except runner.FailClosed as exc:
            errors.append(f"runtime_identity_repository:{exc}")
        if expected_repository != live_repository:
            errors.append("runtime_identity_repository_mismatch")
        bindings = ((case or {}).get("cache") or {}).get("bindings", [])
        inventory = [
            item.get("equals") for item in bindings
            if isinstance(item, dict) and item.get("label") == "operational_cache_inventory_sha256"
        ]
        if len(inventory) != 1 or identity.get("operational_cache_inventory_sha256") != inventory[0]:
            errors.append("runtime_identity_operational_cache_inventory_mismatch")

    prompt_ref = receipt.get("prompt")
    prompt_path: Path | None = None
    prompt_bytes: bytes | None = None
    if prompt_ref is not None:
        prompt_path, prompt_bytes = check_ref(prompt_ref, "prompt", errors)
        del prompt_path
        if case is not None:
            _, frozen_prompt = check_ref(case.get("user_prompt"), "manifest_user_prompt", errors)
            if prompt_bytes != frozen_prompt:
                errors.append("user_prompt_not_byte_equal")

    command = receipt.get("command")
    if command is not None:
        if not isinstance(command, list) or not all(isinstance(item, str) for item in command):
            errors.append("command_invalid")
        else:
            if "--safe-mode" in command or "--resume" in command:
                errors.append("forbidden_cli_mode")
            if command.count("--session-id") != 1 or receipt.get("session_id") not in command:
                errors.append("session_command_mismatch")
            treatment_flag = "--append-system-prompt-file" in command
            if treatment_flag != (arm == "T"):
                errors.append("treatment_system_flag_arm_mismatch")

    system_ref = receipt.get("treatment_system")
    query = receipt.get("query_evidence")
    token_context = receipt.get("token_ledger")
    pre_model_failure = (
        isinstance(token_context, dict)
        and token_context.get("model_invoked") is False
        and command is None
        and receipt.get("returncode") is None
    )
    system = b""
    if arm == "A":
        if system_ref is not None or query is not None:
            errors.append("control_contains_treatment_material")
    elif system_ref is not None:
        _, maybe_system = check_ref(system_ref, "treatment_system", errors)
        system = maybe_system or b""
    elif command is not None:
        errors.append("treatment_missing_system")

    query_projection = b""
    if arm == "T" and query is None:
        errors.append("treatment_missing_query_evidence")
    if query is not None:
        if not isinstance(query, dict):
            errors.append("query_evidence_invalid")
        else:
            if query.get("schema_version") != runner.QUERY_EVIDENCE_SCHEMA:
                errors.append("query_evidence_schema_invalid")
            if query.get("acquisition_started") is not True:
                errors.append("query_acquisition_not_recorded")
            wrapper_returncode = query.get("wrapper_returncode")
            if type(wrapper_returncode) is not int:
                errors.append("query_wrapper_returncode_invalid")
            check_ref(query.get("wrapper_stdout"), "query_wrapper_stdout", errors)
            check_ref(query.get("wrapper_stderr"), "query_wrapper_stderr", errors)
            raw_receipt = None
            if query.get("raw_receipt") is not None:
                _, raw_receipt = load_ref_json(query.get("raw_receipt"), "query_raw_receipt", errors)
            if isinstance(raw_receipt, dict):
                check_ref(raw_receipt.get("stdout"), "query_raw_stdout", errors)
                check_ref(raw_receipt.get("stderr"), "query_raw_stderr", errors)
                if isinstance(identity, dict):
                    if raw_receipt.get("root") != identity.get("root"):
                        errors.append("query_root_mismatch")
                    if raw_receipt.get("cache_manifest_sha256") != identity.get("cache_manifest_sha256"):
                        errors.append("query_cache_identity_mismatch")
                    if raw_receipt.get("repository_identity") != identity.get("live_repository_identity"):
                        errors.append("query_repository_identity_mismatch")
            wrapper_path, wrapper_bytes = check_ref(query.get("wrapper_stdout"), "query_wrapper_stdout_repeat", errors)
            del wrapper_path
            query_projection = wrapper_bytes or b""
            if query.get("succeeded") is True:
                if pre_model_failure or wrapper_returncode != 0 or query.get("failure") is not None:
                    errors.append("query_success_state_invalid")
                if not isinstance(raw_receipt, dict):
                    errors.append("query_success_missing_raw_receipt")
                else:
                    if raw_receipt.get("returncode") != 0:
                        errors.append("query_raw_returncode_nonzero")
                if wrapper_bytes is not None and runner.READY_SENTINEL.encode() not in wrapper_bytes:
                    errors.append("query_projection_missing_exact_readiness")
                if wrapper_bytes is not None:
                    try:
                        projected_ids = runner.stable_code_ids(wrapper_bytes.decode("utf-8", errors="strict"))
                    except UnicodeError:
                        projected_ids = []
                        errors.append("query_projection_not_utf8")
                        projection_is_utf8 = False
                    else:
                        projection_is_utf8 = True
                    if not projected_ids:
                        errors.append("query_no_stable_code_ids")
                    if projection_is_utf8:
                        expected_authorization = frontier_replay.source(
                            0,
                            "injected_query_projection",
                            "INJECTED_QUERY",
                            wrapper_bytes,
                            wrapper_bytes,
                        )
                        if not isinstance(raw_receipt, dict):
                            errors.append(
                                "query_projection_authorization_missing"
                            )
                        else:
                            if (
                                raw_receipt.get(
                                    "projection_authorization"
                                )
                                != expected_authorization[
                                    "projection_authorization"
                                ]
                            ):
                                errors.append(
                                    "query_projection_authorization_mismatch"
                                )
                            if (
                                raw_receipt.get(
                                    "projection_authorization_sha256"
                                )
                                != expected_authorization[
                                    "projection_authorization_sha256"
                                ]
                            ):
                                errors.append(
                                    "query_projection_authorization_hash_mismatch"
                                )
                            if (
                                query.get(
                                    "projection_authorization_sha256"
                                )
                                != raw_receipt.get(
                                    "projection_authorization_sha256"
                                )
                            ):
                                errors.append(
                                    "query_evidence_authorization_hash_mismatch"
                                )
                    if query.get("projected_stable_code_ids") != projected_ids:
                        errors.append("query_evidence_projected_ids_mismatch")
                if isinstance(raw_receipt, dict):
                    raw_path, raw_bytes = check_ref(raw_receipt.get("stdout"), "query_raw_stdout_repeat", errors)
                    del raw_path
                    if raw_bytes is not None and runner.READY_SENTINEL.encode() not in raw_bytes:
                        errors.append("query_raw_missing_exact_readiness")
            else:
                if not pre_model_failure:
                    errors.append("query_failure_outside_pre_model_episode")
                if not isinstance(query.get("failure"), str) or not query["failure"]:
                    errors.append("query_failure_reason_missing")
                receipt_errors = receipt.get("errors")
                if not isinstance(receipt_errors, list) or not any(
                    isinstance(item, str) and item.startswith("rna_preprocessing_failed:")
                    for item in receipt_errors
                ):
                    errors.append("query_failure_not_bound_to_episode_error")

    stdout_summary: dict[str, Any] = {}
    if receipt.get("stdout") is not None:
        stdout_path, _ = check_ref(receipt.get("stdout"), "stdout", errors)
        if stdout_path is not None:
            stdout_summary = runner.safe_summary(stdout_path)
    if receipt.get("stderr") is not None:
        check_ref(receipt.get("stderr"), "stderr", errors)
    transcripts = receipt.get("transcripts", [])
    if not isinstance(transcripts, list):
        errors.append("transcripts_invalid")
        transcripts = []
    for index, transcript in enumerate(transcripts):
        if not isinstance(transcript, dict):
            errors.append(f"transcript_{index}_invalid")
        else:
            check_ref(transcript.get("retained"), f"transcript_{index}", errors)
    try:
        model_events = runner.transcript_model_events(transcripts)
    except (runner.FailClosed, OSError, json.JSONDecodeError) as exc:
        model_events = None
        errors.append(f"transcript_model_events:{exc}")
    observed_provider_responses = len(model_events) if model_events is not None else None

    actor: dict[str, Any] = {}
    if receipt.get("actor_tool_ledger") is not None:
        _, loaded_actor = load_ref_json(receipt.get("actor_tool_ledger"), "actor_tool_ledger", errors)
        if isinstance(loaded_actor, dict):
            actor = loaded_actor
            sequences = [action.get("sequence") for action in actor.get("actions", [])]
            if sequences != list(range(1, len(sequences) + 1)):
                errors.append("actor_sequence_not_contiguous")
            if actor.get("arm") != arm:
                errors.append("actor_arm_mismatch")

    supervisor = receipt.get("supervisor")
    common_hooks: list[dict[str, Any]] = []
    treatment_hooks: list[dict[str, Any]] = []
    rna_events: list[dict[str, Any]] = []
    state: dict[str, Any] = {}
    supervisor_config: dict[str, Any] = {}
    if isinstance(supervisor, dict):
        if supervisor.get("config") is not None:
            _, loaded_config = load_ref_json(supervisor["config"], "supervisor_config", errors)
            if isinstance(loaded_config, dict):
                supervisor_config = loaded_config
                if loaded_config.get("schema_version") != "issue827-supervisor-config-v4":
                    errors.append("supervisor_config_schema_mismatch")
                try:
                    runner.require_fully_rendered(
                        loaded_config, "verified supervisor config"
                    )
                    isolation.validate_worker_config(loaded_config)
                except (runner.FailClosed, isolation.IsolationViolation) as exc:
                    errors.append(f"supervisor_isolation_config:{exc}")
                if receipt.get("runtime_identity", {}).get("sha256") != loaded_config.get("expected_identity_sha256"):
                    errors.append("supervisor_identity_mismatch")
                if isinstance(identity, dict) and loaded_config.get("expected_repository_identity") != identity.get("expected_repository_identity"):
                    errors.append("supervisor_repository_identity_mismatch")
                if isinstance(query, dict):
                    expected_query_command = [
                        loaded_config.get("query_wrapper"),
                        "--query-sha256",
                        loaded_config.get("expected_query_sha256"),
                    ]
                    if query.get("wrapper_command") != expected_query_command:
                        errors.append("query_wrapper_command_mismatch")
                harness_root = Path(str(loaded_config.get("harness_root", "")))
                pinned_paths = {
                    "bash_gateway_sha256": Path(str(loaded_config.get("bash_gateway", ""))),
                    "isolation_module_sha256": harness_root / "isolation.py",
                    "common_supervisor_sha256": harness_root / "common_supervisor.py",
                    "tool_supervisor_sha256": harness_root / "tool_supervisor.py",
                    "hook_guard_sha256": harness_root / "hook_guard.py",
                }
                for key, path in pinned_paths.items():
                    try:
                        if runner.sha_file(path) != loaded_config.get(key):
                            errors.append(f"{key}_mismatch")
                    except OSError as exc:
                        errors.append(f"{key}:{exc}")
                if isinstance(manifest, dict):
                    host = manifest.get("isolation")
                    if not isinstance(host, dict):
                        errors.append("manifest_isolation_invalid")
                    else:
                        for manifest_key, config_path, config_hash in (
                            ("gateway_python", "gateway_python", "gateway_python_sha256"),
                            ("docker_binary", "docker_binary", "docker_binary_sha256"),
                            ("sandbox_exec", "sandbox_exec", "sandbox_exec_sha256"),
                        ):
                            ref = host.get(manifest_key)
                            if (
                                not isinstance(ref, dict)
                                or ref.get("path") != loaded_config.get(config_path)
                                or ref.get("sha256") != loaded_config.get(config_hash)
                            ):
                                errors.append(f"manifest_{manifest_key}_not_config_bound")
                        if host.get("docker_host") != loaded_config.get("docker_host_env", {}).get("DOCKER_HOST"):
                            errors.append("manifest_docker_host_not_config_bound")
                        if host.get("docker_server") != loaded_config.get("docker_server"):
                            errors.append("manifest_docker_server_not_config_bound")
                if isinstance(case, dict):
                    worker = case.get("isolation_worker")
                    if not isinstance(worker, dict):
                        errors.append("case_isolation_worker_invalid")
                    else:
                        if worker.get("image") != loaded_config.get("worker_image"):
                            errors.append("case_worker_image_not_config_bound")
                        for manifest_key, config_key in (
                            ("image_manifest", "worker_image_manifest_sha256"),
                            ("preflight_receipt", "worker_preflight_receipt"),
                        ):
                            ref = worker.get(manifest_key)
                            if not isinstance(ref, dict):
                                errors.append(f"case_worker_{manifest_key}_invalid")
                            elif manifest_key == "image_manifest":
                                if ref.get("sha256") != loaded_config.get(config_key):
                                    errors.append("case_worker_manifest_not_config_bound")
                            elif ref.get("path") != loaded_config.get(config_key):
                                errors.append("case_worker_preflight_not_config_bound")
        if supervisor.get("common_hook_ledger") is not None:
            common_path, _ = check_ref(supervisor["common_hook_ledger"], "common_hook_ledger", errors)
            if common_path:
                try:
                    common_hooks = runner.load_jsonl(common_path)
                except runner.FailClosed as exc:
                    errors.append(f"common_hook_ledger:{exc}")
        if supervisor.get("treatment_hook_ledger") is not None:
            treatment_path, _ = check_ref(supervisor["treatment_hook_ledger"], "treatment_hook_ledger", errors)
            if treatment_path:
                try:
                    treatment_hooks = runner.load_jsonl(treatment_path)
                except runner.FailClosed as exc:
                    errors.append(f"treatment_hook_ledger:{exc}")
        if supervisor.get("state") is not None:
            _, loaded_state = load_ref_json(supervisor["state"], "supervisor_state", errors)
            if isinstance(loaded_state, dict):
                state = loaded_state
        if supervisor.get("common_state") is not None:
            _, common_state = load_ref_json(supervisor["common_state"], "common_state", errors)
            if isinstance(common_state, dict) and common_state.get("fatal") and receipt.get("policy_compliant"):
                errors.append("common_fatal_but_policy_compliant")
        if supervisor.get("hook_guard_state") is not None:
            _, guard_state = load_ref_json(
                supervisor["hook_guard_state"], "hook_guard_state", errors
            )
            if isinstance(guard_state, dict) and receipt.get("policy_compliant"):
                errors.append("hook_guard_fatal_but_policy_compliant")
        if supervisor.get("hook_guard_ledger") is not None:
            check_ref(supervisor["hook_guard_ledger"], "hook_guard_ledger", errors)
        _, native_tool_state = load_ref_json(
            supervisor.get("native_tool_state"),
            "native_tool_state",
            errors,
        )
        if native_tool_state != {
            "schema_version": "issue827-native-tool-state-v1",
            "active": {},
        }:
            errors.append("native_tool_state_not_quiescent")
        for index, ref in enumerate(supervisor.get("rna_events", [])):
            _, event = load_ref_json(ref, f"rna_event_{index}", errors)
            if isinstance(event, dict):
                rna_events.append(event)
            if isinstance(event, dict) and isinstance(identity, dict):
                if event.get("identity_sha256") != receipt.get("runtime_identity", {}).get("sha256"):
                    errors.append(f"rna_event_{index}_identity_mismatch")
                if event.get("root") != identity.get("root"):
                    errors.append(f"rna_event_{index}_root_mismatch")
        isolation_block = supervisor.get("isolation")
        if (
            not isinstance(isolation_block, dict)
            or isolation_block.get("schema_version")
            != "issue827-isolation-evidence-v1"
        ):
            errors.append("isolation_evidence_invalid")
        else:
            for key in (
                "harness_materialization",
                "seatbelt_profile",
                "trusted_rna_seatbelt_profile",
                "private_tree_audits",
                "worker_config_preflight",
            ):
                check_ref(isolation_block.get(key), f"isolation_{key}", errors)
            if not pre_model_failure:
                for key in (
                    "trusted_rna_broker_ready",
                    "trusted_rna_broker_stop",
                    "trusted_rna_broker_teardown",
                ):
                    check_ref(
                        isolation_block.get(key),
                        f"isolation_{key}",
                        errors,
                    )
            for key in (
                "hook_guard_state",
                "hook_guard_ledger",
            ):
                if isolation_block.get(key) is not None:
                    check_ref(isolation_block.get(key), f"isolation_{key}", errors)
            for key in (
                "gateway_requests",
                "gateway_claimed",
                "gateway_revoked",
                "gateway_receipts",
                "gateway_traces",
                "gateway_teardowns",
                "trusted_rna_broker_requests",
                "trusted_rna_broker_claimed",
                "trusted_rna_broker_outputs",
            ):
                refs = isolation_block.get(key)
                if not isinstance(refs, list):
                    errors.append(f"isolation_{key}_invalid")
                    continue
                for ref_index, ref in enumerate(refs):
                    check_ref(
                        ref, f"isolation_{key}_{ref_index}", errors
                    )
        broker_block = supervisor.get("trusted_rna_broker")
        if not pre_model_failure and not isinstance(broker_block, dict):
            errors.append("trusted_rna_broker_evidence_missing")
        elif isinstance(broker_block, dict):
            for key in ("ready", "teardown", "stdout", "stderr"):
                check_ref(
                    broker_block.get(key),
                    f"trusted_rna_broker_{key}",
                    errors,
                )
            if broker_block.get("returncode") != 0:
                errors.append("trusted_rna_broker_returncode")
    else:
        errors.append("supervisor_evidence_invalid")

    common_pre = [item for item in common_hooks if item.get("event", {}).get("hook_event_name") == "PreToolUse"]
    treatment_pre = [item for item in treatment_hooks if item.get("event", {}).get("hook_event_name") == "PreToolUse"]
    if len(common_pre) != len(treatment_pre):
        errors.append("common_treatment_hook_count_mismatch")
    for index, pair in enumerate(zip(common_pre, treatment_pre), 1):
        if pair[0].get("event") != pair[1].get("event"):
            errors.append(f"hook_event_mismatch_{index}")
    if any(item.get("decision") == "deny" for item in common_hooks) and receipt.get("policy_compliant"):
        errors.append("common_denial_but_policy_compliant")

    if supervisor_config and not pre_model_failure:
        isolated, isolation_errors = runner.isolation_compliance(
            supervisor_config, common_hooks, treatment_hooks
        )
        errors.extend(f"isolation:{item}" for item in isolation_errors)
        if not isolated and receipt.get("policy_compliant"):
            errors.append("isolation_failure_but_policy_compliant")
        post_audit = (
            supervisor.get("post_private_tree_audit")
            if isinstance(supervisor, dict)
            else None
        )
        if post_audit is None:
            errors.append("post_private_tree_audit_missing")
        else:
            _, audit = load_ref_json(
                post_audit, "post_private_tree_audit", errors
            )
            if not isinstance(audit, dict):
                errors.append("post_private_tree_audit_invalid")

    if arm == "T" and isinstance(query, dict) and supervisor_config:
        confinement = query.get("trusted_rna_confinement")
        if not isinstance(confinement, dict):
            errors.append("query_trusted_rna_confinement_missing")
        else:
            expected_refs = {
                "sandbox_exec": (
                    supervisor_config.get("sandbox_exec"),
                    supervisor_config.get("sandbox_exec_sha256"),
                ),
                "seatbelt_profile": (
                    supervisor_config.get("trusted_rna_seatbelt_profile"),
                    supervisor_config.get(
                        "trusted_rna_seatbelt_profile_sha256"
                    ),
                ),
                "canonical_environment": (
                    supervisor_config.get("trusted_rna_environment"),
                    supervisor_config.get(
                        "trusted_rna_environment_sha256"
                    ),
                ),
            }
            for key, (expected_path, expected_sha) in expected_refs.items():
                ref = confinement.get(key)
                check_ref(ref, f"query_confinement_{key}", errors)
                if (
                    not isinstance(ref, dict)
                    or ref.get("path") != expected_path
                    or ref.get("sha256") != expected_sha
                ):
                    errors.append(
                        f"query_confinement_{key}_not_config_bound"
                    )
            if (
                confinement.get("execution_plane")
                != "top_level_trusted_rna_seatbelt"
                or confinement.get("network_inbound") != "denied"
                or confinement.get("network_outbound") != "denied"
                or confinement.get("read_roots")
                != supervisor_config.get("trusted_rna_read_roots")
                or confinement.get("write_roots")
                != supervisor_config.get("trusted_rna_write_roots")
                or confinement.get("process_environment_sha256")
                != runner.sha_bytes(
                    runner.canonical(
                        supervisor_config.get("trusted_rna_env")
                    )
                )
            ):
                errors.append("query_confinement_contract_mismatch")
            requested = query.get("requested_wrapper_command")
            query_command = query.get("wrapper_command")
            expected_prefix = [
                str(supervisor_config.get("sandbox_exec")),
                "-f",
                str(
                    supervisor_config.get(
                        "trusted_rna_seatbelt_profile"
                    )
                ),
                str(supervisor_config.get("gateway_python")),
            ]
            if (
                not isinstance(requested, list)
                or not isinstance(query_command, list)
                or query_command != [*expected_prefix, *requested]
            ):
                errors.append("query_confinement_command_mismatch")

    if command is not None and supervisor_config:
        expected_prefix = [
            supervisor_config.get("sandbox_exec"),
            "-f",
            supervisor_config.get("seatbelt_profile"),
        ]
        if command[:3] != expected_prefix:
            errors.append("outer_seatbelt_command_prefix_mismatch")
        try:
            if runner.sha_file(
                Path(str(supervisor_config["seatbelt_profile"]))
            ) != supervisor_config.get("seatbelt_profile_sha256"):
                errors.append("seatbelt_profile_sha256_mismatch")
            if runner.sha_file(
                Path(str(supervisor_config["sandbox_exec"]))
            ) != supervisor_config.get("sandbox_exec_sha256"):
                errors.append("sandbox_exec_sha256_mismatch")
        except (KeyError, OSError) as exc:
            errors.append(f"outer_seatbelt_identity:{exc}")
        if isinstance(manifest, dict) and isinstance(registration, dict):
            validate_registered_claude_command(
                command,
                receipt_path=receipt_path,
                receipt=receipt,
                manifest=manifest,
                registration=registration,
                supervisor_config=supervisor_config,
                treatment_system=system_ref,
                errors=errors,
            )

    if arm == "T" and receipt.get("policy_compliant"):
        if not actor:
            errors.append("compliant_treatment_missing_actor_ledger")
        elif supervisor_config:
            exact_first_treatment_action(actor, supervisor_config, system, query_projection, errors)
        if state.get("fatal") is True:
            errors.append("compliant_treatment_has_fatal_state")
        if state.get("first_traversal_succeeded") is not True or state.get("first_traversal_status") not in {"OK_NONEMPTY", "OK_EMPTY"}:
            errors.append("compliant_treatment_first_traversal_invalid")
        if supervisor_config:
            try:
                replayed = runner.replay_treatment_frontier(
                    query_projection,
                    rna_events,
                    Path(str(supervisor_config["rna_events"])),
                )
                if (
                    state.get("authorization_frontier") != replayed
                    or state.get("rna_calls") != len(rna_events)
                ):
                    errors.append(
                        "compliant_treatment_frontier_state_mismatch"
                    )
            except (
                KeyError,
                OSError,
                runner.FailClosed,
                frontier_replay.FrontierReplayError,
            ) as exc:
                errors.append(f"treatment_frontier_replay:{exc}")
    if state.get("fatal"):
        if receipt.get("evaluator_authorized"):
            errors.append("fatal_treatment_authorized_evaluator")
        model_actions = [action for action in actor.get("actions", []) if action.get("actor") == "model"]
        fatal_seen = False
        for action in model_actions:
            if fatal_seen and action.get("treatment_decision") == "allow":
                errors.append("allowed_model_tool_after_fatal")
            if action.get("treatment_decision") == "deny":
                fatal_seen = True

    ledger = receipt.get("token_ledger")
    validate_token_ledger(ledger, receipt, errors)
    if isinstance(ledger, dict):
        if stdout_summary.get("valid_json") is True and receipt.get("stdout") is not None:
            _, stdout_bytes = check_ref(receipt.get("stdout"), "stdout_usage_replay", errors)
            try:
                raw_result = json.loads(stdout_bytes) if stdout_bytes is not None else {}
            except json.JSONDecodeError as exc:
                raw_result = {}
                errors.append(f"stdout_usage_replay_invalid_json:{exc}")
            expected = runner.token_ledger(
                raw_result if isinstance(raw_result, dict) else {},
                model_invoked=True,
                model_events=model_events,
                provider_responses=observed_provider_responses,
            )
            if any(ledger.get(key) != expected.get(key) for key in expected if key != "schema_version"):
                errors.append("token_ledger_not_reproducible")

    timing = receipt.get("timing_ledger")
    validate_timing_ledger(timing, receipt, arm, errors)

    patch_ref = receipt.get("terminal_patch")
    if patch_ref is not None:
        _, patch = check_ref(patch_ref, "terminal_patch", errors)
        if patch == b"":
            errors.append("empty_patch_must_be_null")
    if receipt.get("official_evaluator_invoked") is not False:
        errors.append("official_evaluator_contamination")
    if receipt.get("evaluator_authorized") is not False:
        errors.append("runner_self_authorized_evaluator")
    expected_authorized = (
        patch_ref is not None
        and receipt.get("authorization_requested") is True
        and receipt.get("policy_compliant") is True
        and receipt.get("evidence_complete") is True
        and receipt.get("returncode") == 0
        and receipt.get("timed_out") is False
        and not receipt.get("errors")
        and not errors
    )
    if receipt.get("authorization_requested") is not expected_authorized:
        errors.append("evaluator_authorization_request_mismatch")
        expected_authorized = False

    result = {
        "schema_version": VERIFY_SCHEMA,
        "episode_receipt": episode_ref,
        "case_id": receipt.get("case_id"),
        "rank": receipt.get("rank"),
        "arm": arm,
        "evidence_complete": not errors,
        "policy_compliant": receipt.get("policy_compliant") is True and not errors,
        "terminal_patch": patch_ref,
        "terminal_patch_sha256": patch_ref.get("sha256") if isinstance(patch_ref, dict) else None,
        "evaluator_authorized": expected_authorized and not errors,
        "official_evaluator_invoked": False,
        "token_ledger": ledger,
        "timing_ledger": timing,
        "errors": errors,
    }
    return result


def verify_run(output_root: Path) -> dict[str, Any]:
    receipt_paths = sorted(output_root.glob("rank-*/*/episode-receipt.json"))
    results = [verify_episode(path) for path in receipt_paths]
    errors: list[str] = []
    if len(results) != 4:
        errors.append(f"expected_four_episode_receipts_found_{len(results)}")
    identities = [(item.get("case_id"), item.get("arm")) for item in results]
    if len(set(identities)) != len(identities):
        errors.append("duplicate_case_arm_receipt")
    by_arm: dict[str, dict[str, Any]] = {}
    for arm in ("A", "T"):
        selected = [item for item in results if item.get("arm") == arm]
        token_totals = [
            (item.get("token_ledger") or {}).get("provider_total_tokens")
            for item in selected
        ]
        by_arm[arm] = {
            "episodes": len(selected),
            "verifier_clean": sum(item.get("evidence_complete") is True for item in selected),
            "policy_compliant": sum(item.get("policy_compliant") is True for item in selected),
            "evaluator_authorized": sum(item.get("evaluator_authorized") is True for item in selected),
            "provider_total_tokens": (
                sum(token_totals)
                if token_totals and all(type(value) is int for value in token_totals)
                else None
            ),
            "combined_pre_evaluator_wall_seconds": sum((item.get("timing_ledger") or {}).get("combined_pre_evaluator_wall_seconds", 0) for item in selected),
        }
    return {
        "schema_version": "issue827-selector-evidence-aggregate-v1",
        "output_root": str(output_root.resolve()),
        "episodes": results,
        "by_arm": by_arm,
        "all_four_verifier_clean": len(results) == 4 and all(item.get("evidence_complete") is True for item in results),
        "official_evaluator_invoked": False,
        "errors": errors,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="command", required=True)
    episode = sub.add_parser("episode")
    episode.add_argument("episode_receipt", type=Path)
    episode.add_argument("--write", action="store_true")
    run = sub.add_parser("run")
    run.add_argument("output_root", type=Path)
    run.add_argument("--write", action="store_true")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.command == "episode":
        result = verify_episode(args.episode_receipt)
        if args.write:
            target = args.episode_receipt.resolve().parent / "episode-verification.json"
            authorization_target = args.episode_receipt.resolve().parent / "evaluator-authorization.json"
            if target.exists():
                print(f"FAIL CLOSED: refusing to overwrite {target}", file=sys.stderr)
                return 2
            if result["evaluator_authorized"] and authorization_target.exists():
                print(f"FAIL CLOSED: refusing to overwrite {authorization_target}", file=sys.stderr)
                return 2
            authorization = None
            if result["evaluator_authorized"]:
                receipt = json.loads(args.episode_receipt.resolve(strict=True).read_bytes())
                try:
                    authorization = evaluator_authorization.build(
                        args.episode_receipt.resolve(strict=True), receipt, target, result
                    )
                except evaluator_authorization.AuthorizationError as exc:
                    print(f"FAIL CLOSED: authorization audit failed: {exc}", file=sys.stderr)
                    return 2
            runner.atomic_write(target, runner.canonical(result))
            if authorization is not None:
                evaluator_authorization.write_exclusive(authorization_target, authorization)
        print(json.dumps(result, sort_keys=True, indent=2))
        return 0 if result["evidence_complete"] else 2
    result = verify_run(args.output_root)
    if args.write:
        target = args.output_root.resolve() / "selector-evidence-aggregate.json"
        if target.exists():
            print(f"FAIL CLOSED: refusing to overwrite {target}", file=sys.stderr)
            return 2
        runner.atomic_write(target, runner.canonical(result))
    print(json.dumps(result, sort_keys=True, indent=2))
    return 0 if result["all_four_verifier_clean"] and not result["errors"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
