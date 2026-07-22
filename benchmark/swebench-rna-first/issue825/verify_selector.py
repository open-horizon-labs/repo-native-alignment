#!/usr/bin/env python3
"""Offline verifier and evidence aggregator for #825 selector episodes."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shlex
import sys
from typing import Any, Mapping, Sequence

import run_selector as runner


VERIFY_SCHEMA = "issue825-episode-verification-v1"


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
        or first.get("common_decision") != "allow"
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
    if ledger.get("schema_version") != runner.TOKEN_LEDGER_SCHEMA:
        errors.append("token_ledger_schema_invalid")
    if ledger.get("valid") is not True or ledger.get("errors") != []:
        errors.append("token_usage_not_observed")

    model_invoked = ledger.get("model_invoked")
    if model_invoked is False:
        if ledger.get("source") != "model_not_invoked":
            errors.append("token_no_model_source_invalid")
        for key in TOKEN_COUNTER_FIELDS:
            if ledger.get(key) is not None:
                errors.append(f"token_{key}_must_be_null_without_model")
        for key in ("cli_turns", "provider_responses", "provider_requests"):
            if ledger.get(key) != 0:
                errors.append(f"token_{key}_must_be_zero_without_model")
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
        if type(ledger.get(key)) is not int or ledger[key] < 0:
            errors.append(f"token_{key}_invalid")
            counters_valid = False
    if type(ledger.get("cli_turns")) is not int or ledger["cli_turns"] < 0:
        errors.append("token_cli_turns_invalid")
    if ledger.get("provider_responses") is not None and (
        type(ledger["provider_responses"]) is not int or ledger["provider_responses"] < 0
    ):
        errors.append("token_provider_responses_invalid")
    if ledger.get("provider_requests") is not None:
        errors.append("token_provider_requests_must_be_unavailable")
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
    if isinstance(registration, dict) and registration.get("schema_version") != "issue825-treatment-registration-v2":
        errors.append("registration_schema_mismatch")
    if isinstance(selection, dict) and selection.get("schema_version") != "issue825-fresh-pair-selection-v2":
        errors.append("selection_schema_mismatch")
    if isinstance(manifest, dict) and isinstance(registration, dict):
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
                    if not raw_receipt.get("projected_stable_code_ids"):
                        errors.append("query_no_stable_code_ids")
                if wrapper_bytes is not None and runner.READY_SENTINEL.encode() not in wrapper_bytes:
                    errors.append("query_projection_missing_exact_readiness")
                if wrapper_bytes is not None:
                    try:
                        projected_ids = runner.stable_code_ids(wrapper_bytes.decode("utf-8", errors="strict"))
                    except UnicodeError:
                        projected_ids = []
                        errors.append("query_projection_not_utf8")
                    if not isinstance(raw_receipt, dict) or raw_receipt.get("projected_stable_code_ids") != projected_ids:
                        errors.append("query_projected_ids_not_reproducible")
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
    observed_provider_responses = runner.transcript_provider_response_count(transcripts)

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
    state: dict[str, Any] = {}
    supervisor_config: dict[str, Any] = {}
    if isinstance(supervisor, dict):
        if supervisor.get("config") is not None:
            _, loaded_config = load_ref_json(supervisor["config"], "supervisor_config", errors)
            if isinstance(loaded_config, dict):
                supervisor_config = loaded_config
                if loaded_config.get("schema_version") != "rna-supervisor-config-v3":
                    errors.append("supervisor_config_schema_mismatch")
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
        for index, ref in enumerate(supervisor.get("rna_events", [])):
            _, event = load_ref_json(ref, f"rna_event_{index}", errors)
            if isinstance(event, dict) and isinstance(identity, dict):
                if event.get("identity_sha256") != receipt.get("runtime_identity", {}).get("sha256"):
                    errors.append(f"rna_event_{index}_identity_mismatch")
                if event.get("root") != identity.get("root"):
                    errors.append(f"rna_event_{index}_root_mismatch")
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

    if arm == "T" and receipt.get("policy_compliant"):
        if not actor:
            errors.append("compliant_treatment_missing_actor_ledger")
        elif supervisor_config:
            exact_first_treatment_action(actor, supervisor_config, system, query_projection, errors)
        if state.get("fatal") is True:
            errors.append("compliant_treatment_has_fatal_state")
        if state.get("first_traversal_succeeded") is not True or state.get("first_traversal_status") not in {"OK_NONEMPTY", "OK_EMPTY"}:
            errors.append("compliant_treatment_first_traversal_invalid")
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
            expected = runner.token_ledger(
                stdout_summary,
                model_invoked=True,
                provider_responses=observed_provider_responses,
            )
            if any(ledger.get(key) != expected.get(key) for key in expected if key != "schema_version"):
                errors.append("token_ledger_not_reproducible")

    timing = receipt.get("timing_ledger")
    if not isinstance(timing, dict):
        errors.append("timing_ledger_invalid")
    else:
        rna = timing.get("rna_preprocessing_seconds")
        model = timing.get("model_wall_seconds")
        combined = timing.get("combined_pre_evaluator_wall_seconds")
        if not all(type(item) in {int, float} and item >= 0 for item in (rna, model, combined)):
            errors.append("timing_values_invalid")
        elif abs(combined - (rna + model)) > 1e-9:
            errors.append("combined_timing_mismatch")
        if arm == "A" and rna != 0:
            errors.append("control_has_rna_preprocessing_time")

    patch_ref = receipt.get("terminal_patch")
    if patch_ref is not None:
        _, patch = check_ref(patch_ref, "terminal_patch", errors)
        if patch == b"":
            errors.append("empty_patch_must_be_null")
    if receipt.get("official_evaluator_invoked") is not False:
        errors.append("official_evaluator_contamination")
    expected_authorized = (
        patch_ref is not None
        and receipt.get("policy_compliant") is True
        and receipt.get("evidence_complete") is True
        and receipt.get("returncode") == 0
        and receipt.get("timed_out") is False
        and not receipt.get("errors")
    )
    if receipt.get("evaluator_authorized") is not expected_authorized:
        errors.append("evaluator_authorization_mismatch")

    result = {
        "schema_version": VERIFY_SCHEMA,
        "episode_receipt": episode_ref,
        "case_id": receipt.get("case_id"),
        "rank": receipt.get("rank"),
        "arm": arm,
        "evidence_complete": not errors,
        "policy_compliant": receipt.get("policy_compliant") is True and not errors,
        "terminal_patch": patch_ref,
        "evaluator_authorized": receipt.get("evaluator_authorized") is True and not errors,
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
        "schema_version": "issue825-selector-evidence-aggregate-v2",
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
            if target.exists():
                print(f"FAIL CLOSED: refusing to overwrite {target}", file=sys.stderr)
                return 2
            runner.atomic_write(target, runner.canonical(result))
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
