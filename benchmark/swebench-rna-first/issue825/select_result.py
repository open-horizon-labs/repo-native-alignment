#!/usr/bin/env python3
"""Offline, deterministic result aggregation for the registered #825 selector."""

from __future__ import annotations

import argparse
from decimal import Decimal
import json
from pathlib import Path
import sys
from typing import Any, Mapping

import evaluator_runner as evaluator


RESULT_SCHEMA = "issue825-selector-result-v2"


def _as_decimal(value: int | float) -> Decimal:
    return Decimal(str(value))


def _status_bucket(statuses: Mapping[str, Any], category: str) -> Mapping[str, Any]:
    matches = [value for key, value in statuses.items() if key.upper() == category]
    evaluator.require(len(matches) == 1 and isinstance(matches[0], dict), f"missing {category} status")
    return matches[0]


def _outcome_list(bucket: Mapping[str, Any], outcome: str) -> list[Any]:
    matches = [value for key, value in bucket.items() if key.lower() == outcome]
    evaluator.require(len(matches) == 1 and isinstance(matches[0], list), f"missing {outcome} list")
    return matches[0]


def _load_result_receipt(reference: Any) -> tuple[Path, bytes, dict[str, Any]]:
    path, data = evaluator.validate_file_reference(reference, "batch result receipt")
    value = json.loads(data)
    evaluator.require(isinstance(value, dict), "batch result receipt must be an object")
    return path, data, value


def _verified_official_outcome(
    plan: Mapping[str, Any],
    result: Mapping[str, Any],
    seal_info: Mapping[str, Any],
) -> dict[str, Any]:
    seal = seal_info["seal"]
    path, data, receipt = _load_result_receipt(result["receipt"])
    run_dir = Path(plan["output_root"]) / "evaluations" / seal["case_id"] / seal["arm"]
    evaluator.require(path == run_dir / "evaluation.receipt.json", "official receipt path drift")
    evaluator.require(receipt.get("schema_version") == evaluator.EVALUATION_SCHEMA, "official evaluation receipt schema mismatch")
    for key in ("case_id", "rank", "arm", "run_id", "terminal_set_digest"):
        evaluator.require(receipt.get(key) == seal[key], f"official receipt {key} mismatch")
    evaluator.require(receipt.get("official_evaluator_invocation_authorized") is True, "official evaluation was not authorized")
    evaluator.require(receipt.get("official_evaluator_invocation_confirmed") is True, "official evaluation did not start")
    evaluator.require(receipt.get("official_evaluator_invocations") == 1, "official evaluation count is not exactly one")
    evaluator.require(receipt.get("valid_official_outputs") is True, "official outputs are invalid")
    evaluator.require(receipt.get("returncode") == 0 and receipt.get("timed_out") is False, "official evaluator did not finish cleanly")
    evaluator.require(receipt.get("model_output_delivery") == "none", "evaluator output was delivered to a model")
    evaluator.require(
        result.get("official_evaluator_invocation_authorized") is True
        and result.get("official_evaluator_invocation_confirmed") is True,
        "batch result invocation flags differ from official receipt",
    )
    expected_seal_ref = {
        "path": str(seal_info["path"]),
        "bytes": len(seal_info["bytes"]),
        "sha256": seal_info["sha256"],
    }
    evaluator.require(receipt.get("seal") == expected_seal_ref, "official receipt seal binding mismatch")
    claim_path, claim_data = evaluator.validate_file_reference(
        receipt.get("irrevocable_registry_start"),
        "official evaluator start claim",
        Path(plan["registry_root"]),
    )
    evaluator.require(
        claim_path
        == Path(plan["registry_root"])
        / f"{seal['case_id']}--{seal['arm']}--{seal_info['sha256']}.evaluation-started.json",
        "official evaluator start claim path drift",
    )
    claim = json.loads(claim_data)
    evaluator.require(
        claim.get("plan_sha256") == plan["_sha256"]
        and claim.get("script_sha256") == evaluator.sha256_file(evaluator.SCRIPT_PATH)
        and claim.get("terminal_set_digest") == seal["terminal_set_digest"]
        and claim.get("seal_sha256") == seal_info["sha256"]
        and claim.get("official_evaluations_authorized") == 1,
        "official evaluator start claim contents drift",
    )
    prediction_path, prediction_data = evaluator.validate_file_reference(
        receipt.get("prediction"), "official evaluator prediction", run_dir
    )
    evaluator.require(prediction_path == run_dir / "predictions.jsonl", "prediction path drift")
    sealed_prediction = evaluator.strict_regular(
        seal_info["path"].parent / seal["prediction"]["path"], "sealed prediction"
    )
    evaluator.require(prediction_data == sealed_prediction, "evaluated prediction differs from seal")
    episode_matches = [
        item
        for item in plan["episodes"]
        if item["case_id"] == seal["case_id"] and item["arm"] == seal["arm"]
    ]
    evaluator.require(len(episode_matches) == 1, "official episode identity is ambiguous")
    episode = episode_matches[0]
    evaluator.require(
        receipt.get("command") == evaluator.evaluator_command(plan, episode, prediction_path),
        "official evaluator command drift",
    )
    started = json.loads(
        evaluator.strict_regular(run_dir / "evaluation.started.json", "evaluation start record")
    )
    evaluator.require(
        started.get("schema_version") == "issue825-official-evaluation-start-v1"
        and started.get("case_id") == seal["case_id"]
        and started.get("arm") == seal["arm"]
        and started.get("seal_sha256") == seal_info["sha256"]
        and started.get("irrevocable_registry_start")
        == receipt["irrevocable_registry_start"]
        and started.get("command") == receipt["command"]
        and started.get("official_evaluator_invocation_authorized") is True,
        "evaluation start record drift",
    )
    evaluator.require(
        receipt.get("preexisting_container_check", {}).get("absent") is True
        and receipt.get("container_cleanup", {}).get("absent") is True,
        "container isolation proof is incomplete",
    )
    container = receipt.get("container")
    evaluator.require(
        isinstance(container, dict)
        and not container.get("error")
        and container.get("image", {}).get("id") == episode["official_image_local_id"]
        and episode["official_image"] in container.get("image", {}).get("repo_tags", [])
        and type(container.get("peak_memory_bytes")) is int
        and container["peak_memory_bytes"] > 0,
        "official container image proof mismatch",
    )
    monitor = json.loads(
        evaluator.strict_regular(run_dir / "container-monitor.json", "container monitor")
    )
    evaluator.require(monitor == container, "container monitor differs from receipt")
    evaluator.strict_regular(run_dir / "evaluator.stdout", "evaluator stdout")
    evaluator.strict_regular(run_dir / "evaluator.stderr", "evaluator stderr")
    pinned = receipt.get("pinned_image")
    evaluator.require(
        isinstance(pinned, dict)
        and pinned.get("id") == episode["official_image_local_id"]
        and episode["official_image"] in pinned.get("repo_tags", []),
        "pinned image proof mismatch",
    )
    sealed_patch = evaluator.strict_regular(
        seal_info["path"].parent / seal["terminal_patch"]["path"], "sealed patch"
    )
    projected_outputs, projected_tests = evaluator.evaluator_outputs(
        run_dir, episode, sealed_patch
    )
    evaluator.require(
        receipt.get("official_outputs") == projected_outputs,
        "official output references do not match raw evaluator artifacts",
    )
    test_ref = receipt.get("test_lists")
    evaluator.require(isinstance(test_ref, dict), "official test-list reference missing")
    test_path = path.parent / test_ref["path"]
    test_data = evaluator.strict_regular(test_path, "official test lists")
    evaluator.require(len(test_data) == test_ref["bytes"], "official test-list byte count mismatch")
    evaluator.require(evaluator.sha256_bytes(test_data) == test_ref["sha256"], "official test-list digest mismatch")
    tests = json.loads(test_data)
    evaluator.require(
        test_data == evaluator.canonical_json_bytes(projected_tests),
        "test-list projection differs from official report",
    )
    evaluator.require(tests.get("schema_version") == "issue825-official-test-lists-v1", "official test-list schema mismatch")
    evaluator.require(tests.get("case_id") == seal["case_id"] and tests.get("arm") == seal["arm"], "official test-list identity mismatch")
    evaluator.require(type(tests.get("resolved")) is bool, "official resolution flag missing")
    evaluator.require(
        receipt.get("inventory") == evaluator.recursive_inventory(run_dir, {path}),
        "official evaluator inventory drift",
    )
    statuses = tests.get("tests_status")
    evaluator.require(isinstance(statuses, dict), "official test status object missing")
    required = _status_bucket(statuses, "FAIL_TO_PASS")
    pass_to_pass = _status_bucket(statuses, "PASS_TO_PASS")
    required_passed = len(_outcome_list(required, "success"))
    required_failed = len(_outcome_list(required, "failure"))
    regressions = len(_outcome_list(pass_to_pass, "failure"))
    pass_to_pass_passed = len(_outcome_list(pass_to_pass, "success"))
    return {
        "outcome_valid": True,
        "resolved": tests["resolved"],
        "required_passed": required_passed,
        "required_total": required_passed + required_failed,
        "pass_to_pass_passed": pass_to_pass_passed,
        "pass_to_pass_total": pass_to_pass_passed + regressions,
        "pass_to_pass_regressions": regressions,
        "official_evaluator_invocations": 1,
        "evaluation_receipt": {
            "path": str(path),
            "bytes": len(data),
            "sha256": evaluator.sha256_bytes(data),
        },
        "test_lists": {
            "path": str(test_path),
            "bytes": len(test_data),
            "sha256": evaluator.sha256_bytes(test_data),
        },
    }


def _verified_skip(
    plan: Mapping[str, Any],
    result: Mapping[str, Any],
    seal_info: Mapping[str, Any],
) -> dict[str, Any]:
    seal = seal_info["seal"]
    path, data, receipt = _load_result_receipt(result["receipt"])
    run_dir = Path(plan["output_root"]) / "evaluations" / seal["case_id"] / seal["arm"]
    evaluator.require(
        path == run_dir / "evaluation.skip.receipt.json",
        "skip receipt path drift",
    )
    evaluator.require(receipt.get("schema_version") == evaluator.SKIP_SCHEMA, "skip receipt schema mismatch")
    for key in ("case_id", "rank", "arm", "run_id", "terminal_set_digest"):
        evaluator.require(receipt.get(key) == seal[key], f"skip receipt {key} mismatch")
    evaluator.require(
        receipt.get("seal")
        == {
            "path": str(seal_info["path"]),
            "bytes": len(seal_info["bytes"]),
            "sha256": seal_info["sha256"],
        },
        "skip receipt seal binding mismatch",
    )
    evaluator.require(receipt.get("disposition") == seal["disposition"], "skip disposition mismatch")
    evaluator.require(receipt.get("official_evaluator_invocation_authorized") is False, "skipped episode authorized evaluator")
    evaluator.require(receipt.get("official_evaluator_invocation_confirmed") is False, "skipped episode started evaluator")
    evaluator.require(receipt.get("official_evaluator_invocations") == 0, "skipped episode has evaluator calls")
    evaluator.require(receipt.get("model_output_delivery") == "none", "skip receipt reports model feedback")
    evaluator.require(
        result.get("valid") is True
        and result.get("official_evaluator_invocation_authorized") is False
        and result.get("official_evaluator_invocation_confirmed") is False,
        "batch skip flags differ from skip receipt",
    )
    evaluator.require(
        {item for item in run_dir.iterdir()} == {path},
        "skip directory contains evaluator start/output artifacts",
    )
    claim = (
        Path(plan["registry_root"])
        / f"{seal['case_id']}--{seal['arm']}--{seal_info['sha256']}.evaluation-started.json"
    )
    evaluator.require(not claim.exists(), "skipped episode has an evaluator start claim")
    outcome_valid = seal["disposition"] == "no_patch"
    return {
        "outcome_valid": outcome_valid,
        "resolved": False if outcome_valid else None,
        "required_passed": None,
        "required_total": None,
        "pass_to_pass_passed": None,
        "pass_to_pass_total": None,
        "pass_to_pass_regressions": 0 if outcome_valid else None,
        "official_evaluator_invocations": 0,
        "evaluation_receipt": {
            "path": str(path),
            "bytes": len(data),
            "sha256": evaluator.sha256_bytes(data),
        },
        "test_lists": None,
    }


def _failed_evaluation(
    plan: Mapping[str, Any],
    result: Mapping[str, Any],
    seal_info: Mapping[str, Any],
) -> dict[str, Any]:
    seal = seal_info["seal"]
    path, data, receipt = _load_result_receipt(result["receipt"])
    schema = receipt.get("schema_version")
    evaluator.require(
        schema in (evaluator.FAILURE_SCHEMA, evaluator.EVALUATION_SCHEMA),
        "failure receipt schema mismatch",
    )
    expected_name = (
        "evaluation.failure.receipt.json"
        if schema == evaluator.FAILURE_SCHEMA
        else "evaluation.receipt.json"
    )
    evaluator.require(
        path
        == Path(plan["output_root"])
        / "evaluations"
        / seal["case_id"]
        / seal["arm"]
        / expected_name,
        "failed evaluation receipt path drift",
    )
    for key in ("case_id", "rank", "arm", "run_id", "terminal_set_digest"):
        evaluator.require(receipt.get(key) == seal[key], f"failure receipt {key} mismatch")
    evaluator.require(
        receipt.get("seal")
        == {
            "path": str(seal_info["path"]),
            "bytes": len(seal_info["bytes"]),
            "sha256": seal_info["sha256"],
        },
        "failure receipt seal binding mismatch",
    )
    confirmed = receipt.get("official_evaluator_invocation_confirmed") is True
    evaluator.require(
        receipt.get("official_evaluator_invocations") == int(confirmed),
        "failed evaluation invocation count mismatch",
    )
    evaluator.require(
        receipt.get("valid_official_outputs") is False,
        "failed evaluation claims valid official outputs",
    )
    evaluator.require(
        result.get("official_evaluator_invocation_confirmed") is confirmed
        and result.get("official_evaluator_invocation_authorized")
        is (receipt.get("official_evaluator_invocation_authorized") is True),
        "batch failure invocation flags differ from receipt",
    )
    if schema == evaluator.EVALUATION_SCHEMA:
        evaluator.require(confirmed, "terminal evaluation receipt lacks an invocation")
        evaluator.require(
            receipt.get("container_cleanup", {}).get("absent") is True,
            "invalid terminal evaluation lacks cleanup proof",
        )
    evaluator.require(receipt.get("model_output_delivery") == "none", "failure receipt reports model feedback")
    return {
        "outcome_valid": False,
        "resolved": None,
        "required_passed": None,
        "required_total": None,
        "pass_to_pass_passed": None,
        "pass_to_pass_total": None,
        "pass_to_pass_regressions": None,
        "official_evaluator_invocations": int(confirmed),
        "evaluation_receipt": {
            "path": str(path),
            "bytes": len(data),
            "sha256": evaluator.sha256_bytes(data),
        },
        "test_lists": None,
    }


def episode_metrics(
    plan: Mapping[str, Any],
    seal_info: Mapping[str, Any],
    result: Mapping[str, Any],
) -> dict[str, Any]:
    seal = seal_info["seal"]
    evaluator.require(result.get("case_id") == seal["case_id"], "batch case mismatch")
    evaluator.require(result.get("rank") == seal["rank"], "batch rank mismatch")
    evaluator.require(result.get("arm") == seal["arm"], "batch arm mismatch")
    evaluator.require(result.get("disposition") == seal["disposition"], "batch disposition mismatch")
    if seal["disposition"] == "evaluate":
        if result.get("valid") is True:
            outcome = _verified_official_outcome(plan, result, seal_info)
        else:
            outcome = _failed_evaluation(plan, result, seal_info)
    else:
        outcome = _verified_skip(plan, result, seal_info)

    receipt_path = seal_info["path"].parent / seal["sealed_episode_receipt"]["path"]
    receipt_data = evaluator.strict_regular(receipt_path, "sealed episode receipt")
    evaluator.require(
        len(receipt_data) == seal["sealed_episode_receipt"]["bytes"]
        and evaluator.sha256_bytes(receipt_data) == seal["sealed_episode_receipt"]["sha256"],
        "sealed episode receipt changed",
    )
    episode = json.loads(receipt_data)
    verification_path = seal_info["path"].parent / seal["sealed_episode_verification"]["path"]
    verification_data = evaluator.strict_regular(verification_path, "sealed episode verification")
    evaluator.require(
        len(verification_data) == seal["sealed_episode_verification"]["bytes"]
        and evaluator.sha256_bytes(verification_data) == seal["sealed_episode_verification"]["sha256"],
        "sealed episode verification changed",
    )
    verification = json.loads(verification_data)
    token_ledger = episode["token_ledger"]
    timing = episode["timing_ledger"]
    return {
        "case_id": seal["case_id"],
        "rank": seal["rank"],
        "arm": seal["arm"],
        "disposition": seal["disposition"],
        "evidence_complete": verification["evidence_complete"],
        "policy_compliant": episode.get("policy_compliant") is True,
        "policy_verifier_clean": verification["policy_compliant"],
        "evaluator_authorized": verification["evaluator_authorized"],
        "provider_input_tokens": token_ledger["input_tokens"],
        "provider_output_tokens": token_ledger["output_tokens"],
        "provider_total_tokens": token_ledger["provider_total_tokens"],
        "model_wall_seconds": timing["model_wall_seconds"],
        "rna_preprocessing_seconds": timing["rna_preprocessing_seconds"],
        "combined_pre_evaluator_wall_seconds": timing["combined_pre_evaluator_wall_seconds"],
        **outcome,
    }


def decide_registered(metrics: list[Mapping[str, Any]]) -> dict[str, Any]:
    evaluator.require(len(metrics) == 4, "registered decision requires four episodes")
    treatment = [item for item in metrics if item["arm"] == "T"]
    control = [item for item in metrics if item["arm"] == "A"]
    evaluator.require(len(treatment) == len(control) == 2, "registered decision requires two A/T pairs")
    if any(item["policy_compliant"] is not True for item in treatment):
        return {
            "decision": "no_RNA_treatment",
            "classification": "treatment_noncompliance",
            "reason": "at least one T episode failed the mandatory RNA-first manipulation contract",
        }
    if any(item.get("evidence_complete") is not True for item in metrics):
        return {
            "decision": "no_RNA_treatment",
            "classification": "inconclusive",
            "reason": "at least one episode lacks verifier-complete model, token, or harness evidence",
        }
    if any(item["outcome_valid"] is not True for item in metrics):
        return {
            "decision": "no_RNA_treatment",
            "classification": "inconclusive",
            "reason": "at least one episode lacks a verifier-clean resolution/regression outcome",
        }

    def totals(items: list[Mapping[str, Any]]) -> dict[str, Any]:
        return {
            "resolved": sum(item["resolved"] is True for item in items),
            "pass_to_pass_regressions": sum(item["pass_to_pass_regressions"] for item in items),
            "provider_total_tokens": sum(item["provider_total_tokens"] for item in items),
            "combined_pre_evaluator_wall_seconds": sum(
                (_as_decimal(item["combined_pre_evaluator_wall_seconds"]) for item in items),
                Decimal("0"),
            ),
        }

    a = totals(control)
    t = totals(treatment)
    aggregate = {
        "A": {**a, "combined_pre_evaluator_wall_seconds": str(a["combined_pre_evaluator_wall_seconds"])},
        "T": {**t, "combined_pre_evaluator_wall_seconds": str(t["combined_pre_evaluator_wall_seconds"])},
    }
    common = {
        "aggregates": aggregate,
        "thresholds": {
            "token_reduction_percent": 15,
            "maximum_token_increase_percent_for_time_path": 5,
            "time_reduction_percent": 20,
        },
    }
    if t["resolved"] < a["resolved"] or t["pass_to_pass_regressions"] > a["pass_to_pass_regressions"]:
        return {
            "decision": "no_RNA_treatment",
            "classification": "efficacy_or_regression_rejection",
            "reason": "T resolved fewer cases or caused more total PASS_TO_PASS regressions",
            **common,
        }
    if t["resolved"] > a["resolved"]:
        return {
            "decision": "selected_T",
            "classification": "efficacy_selection",
            "reason": "T resolved more cases without more regressions",
            **common,
        }
    if t["pass_to_pass_regressions"] < a["pass_to_pass_regressions"]:
        return {
            "decision": "selected_T",
            "classification": "regression_safety_selection",
            "reason": "resolution tied and T caused fewer regressions",
            **common,
        }
    token_reduction = (
        a["provider_total_tokens"] > 0
        and 100 * t["provider_total_tokens"]
        <= 85 * a["provider_total_tokens"]
    )
    token_within_five = (
        100 * t["provider_total_tokens"]
        <= 105 * a["provider_total_tokens"]
    )
    time_reduction = (
        a["combined_pre_evaluator_wall_seconds"] > 0
        and Decimal("100") * t["combined_pre_evaluator_wall_seconds"]
        <= Decimal("80") * a["combined_pre_evaluator_wall_seconds"]
    )
    common["efficiency_checks"] = {
        "tokens_at_least_15_percent_lower": token_reduction,
        "tokens_no_more_than_5_percent_higher": token_within_five,
        "combined_wall_at_least_20_percent_lower": time_reduction,
    }
    if token_reduction or (token_within_five and time_reduction):
        return {
            "decision": "selected_T",
            "classification": "material_efficiency_selection",
            "reason": "efficacy and regressions tied and T crossed a registered efficiency threshold",
            **common,
        }
    return {
        "decision": "no_RNA_treatment",
        "classification": "no_registered_advantage",
        "reason": "T crossed no efficacy, regression-safety, or material-efficiency threshold",
        **common,
    }


def decide_for_selection(
    selection: Mapping[str, Any],
    metrics: list[Mapping[str, Any]],
) -> dict[str, Any]:
    authoritative = selection.get("authoritative")
    evaluator.require(
        type(authoritative) is bool,
        "selection authoritative flag is missing or malformed",
    )
    if not authoritative:
        return {
            "decision": "no_selection",
            "classification": "non_authoritative_qualification",
            "reason": (
                "the bound selection is explicitly non-authoritative; "
                "qualification evidence cannot select a treatment"
            ),
            "selection_authoritative": False,
            "selection_state": selection.get("state"),
        }
    evaluator.require(
        selection.get("state") == "selected_pre_model",
        "authoritative selection state is not selected_pre_model",
    )
    evaluator.require(
        selection.get("problem_statements_inspected_by_human_before_selection") is False,
        "authoritative selection permits prior human problem-statement inspection",
    )
    evaluator.require(
        selection.get("gold_or_outcomes_inspected_before_selection") is False,
        "authoritative selection permits prior gold/outcome inspection",
    )
    return {
        "selection_authoritative": True,
        "selection_state": selection.get("state"),
        **decide_registered(metrics),
    }


def aggregate(plan_path: Path, batch_path: Path) -> dict[str, Any]:
    plan = evaluator.validate_plan(plan_path.resolve(strict=True))
    seal_set = evaluator.validate_seal_set(plan)
    batch_data = evaluator.strict_regular(batch_path.resolve(strict=True), "evaluation batch receipt")
    evaluator.require(
        batch_path.resolve(strict=True)
        == Path(plan["output_root"]).resolve(strict=True)
        / "evaluation-batch.receipt.json",
        "evaluation batch path drift",
    )
    batch = json.loads(batch_data)
    evaluator.require(batch.get("schema_version") == evaluator.BATCH_SCHEMA, "batch schema mismatch")
    evaluator.require(batch.get("plan_sha256") == plan["_sha256"], "batch plan mismatch")
    evaluator.require(batch.get("script_sha256") == evaluator.sha256_file(evaluator.SCRIPT_PATH), "batch evaluator script mismatch")
    evaluator.require(batch.get("terminal_set_digest") == seal_set["terminal_set_digest"], "batch terminal set mismatch")
    evaluator.require(
        batch.get("seal_set")
        == {
            "path": str(seal_set["path"]),
            "bytes": len(seal_set["bytes"]),
            "sha256": seal_set["sha256"],
        },
        "batch seal-set binding mismatch",
    )
    evaluator.require(batch.get("model_output_delivery", "").startswith("none"), "batch reports model feedback")
    results = batch.get("results")
    evaluator.require(isinstance(results, list) and len(results) == 4, "batch must record four episode dispositions")
    evaluator.require(all(isinstance(item, dict) for item in results), "batch result shape mismatch")
    evaluator.require(batch.get("failures") == [], "batch contains unrecorded worker failures")
    evaluator.require(
        batch.get("max_parallel") == 2
        and batch.get("same_case_serialized") is True,
        "batch concurrency contract drift",
    )
    environment = batch.get("environment")
    evaluator.require(
        isinstance(environment, dict)
        and environment.get("dataset_arrow_sha256")
        == plan["evaluator"]["dataset_arrow_sha256"]
        and environment.get("official_evaluator_invocations") == 0
        and environment.get("model_session_isolation", {}).get("all_absent") is True
        and environment.get("model_session_isolation", {}).get("checked_session_count")
        == 4,
        "batch environment/no-feedback proof mismatch",
    )
    result_map = {(item["case_id"], item["arm"]): item for item in results}
    evaluator.require(len(result_map) == 4, "batch contains duplicate episode results")
    metrics: list[dict[str, Any]] = []
    for seal_info in seal_set["seals"]:
        key = (seal_info["seal"]["case_id"], seal_info["seal"]["arm"])
        evaluator.require(key in result_map, f"batch result missing: {key}")
        metrics.append(episode_metrics(plan, seal_info, result_map[key]))
    metrics.sort(key=lambda value: (value["rank"], value["arm"]))
    expected_authorized = sum(item["disposition"] == "evaluate" for item in metrics)
    expected_started = sum(item["official_evaluator_invocations"] for item in metrics)
    evaluator.require(batch.get("official_evaluations_authorized") == expected_authorized, "batch authorization count mismatch")
    evaluator.require(batch.get("official_evaluations_started") == expected_started, "batch invocation count mismatch")
    evaluator.require(
        batch.get("official_evaluations_recorded") == expected_authorized,
        "batch recorded-evaluation count mismatch",
    )
    evaluator.require(batch.get("zero_invocation_receipts") == 4 - expected_authorized, "batch zero-call count mismatch")
    decision = decide_for_selection(plan["_selection"], metrics)
    payload = {
        "schema_version": RESULT_SCHEMA,
        "computed_at": evaluator.utc_now(),
        "plan": {"path": str(plan_path), "bytes": len(plan["_bytes"]), "sha256": plan["_sha256"]},
        "registration_sha256": evaluator.sha256_bytes(plan["_registration_bytes"]),
        "selection_sha256": evaluator.sha256_bytes(plan["_selection_bytes"]),
        "evaluator_script_sha256": evaluator.sha256_file(evaluator.SCRIPT_PATH),
        "aggregator_script_sha256": evaluator.sha256_file(Path(__file__).resolve()),
        "terminal_set_digest": seal_set["terminal_set_digest"],
        "evaluation_batch": {
            "path": str(batch_path),
            "bytes": len(batch_data),
            "sha256": evaluator.sha256_bytes(batch_data),
        },
        "episodes": metrics,
        "official_evaluations_authorized": expected_authorized,
        "official_evaluations_started": expected_started,
        "zero_invocation_episodes": 4 - expected_authorized,
        "no_model_feedback_verified": True,
        **decision,
    }
    payload["result_payload_sha256"] = evaluator.sha256_bytes(evaluator.canonical_json_bytes(payload))
    return payload


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("--plan", type=Path, required=True)
    value.add_argument("--batch-receipt", type=Path, required=True)
    value.add_argument("--output", type=Path, required=True)
    return value


def main() -> int:
    args = parser().parse_args()
    try:
        evaluator.require(args.output.is_absolute(), "result output path must be absolute")
        result = aggregate(args.plan, args.batch_receipt)
        data = evaluator.canonical_json_bytes(result)
        evaluator.write_exclusive(args.output, data)
        print(
            json.dumps(
                {
                    "status": "selection-complete",
                    "decision": result["decision"],
                    "output": str(args.output),
                    "sha256": evaluator.sha256_bytes(data),
                },
                sort_keys=True,
            )
        )
        return 0
    except (evaluator.FailClosed, FileExistsError) as exc:
        print(f"FAIL-CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
