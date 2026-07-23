#!/usr/bin/env python3
"""Offline verifier for the published #825 amended selector evidence.

This file is deliberately outside the registered model/evaluator runtime.  It
audits post-outcome evidence without changing or reinterpreting that runtime.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any, Mapping, Sequence


ORIGINAL_REGISTRATION = "dbfcc2553cd5cda945e53295f11ea21100265aff3c8131e45f6543fc7cf56fbc"
ORIGINAL_SELECTION = "2898db4e0a083fd3facd4799534a24b8e1344af47a6142c4ba668f0a08216a66"
AMENDED_REGISTRATION = "700aab84dfe4df7e2cf2ff1af206be4bd4e336f1e3c74dc93d28e3b38e8a8141"
AMENDED_SELECTION = "794aa76a7d9bcbfb5148ba59e88ff060ef8052514884f46b7cf92dcd36c6d0c4"
EVALUATION_BATCH = "794996b8023dbe3fb2e0164bebd9d9b7c990bf0307026cdf9427cd6e5be2fc5c"
SELECTION_RESULT = "350dd532c90c05d8b8d66883576afa4608ece9dca6ebeb58afd38aa7941af127"
XARRAY = "pydata__xarray-4687"
DJANGO = "django__django-15503"


class EvidenceError(ValueError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise EvidenceError(message)


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def sha_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def strict_bytes(path: Path) -> bytes:
    require(path.is_file() and not path.is_symlink(), f"missing or symlinked evidence: {path}")
    before = path.stat()
    data = path.read_bytes()
    after = path.stat()
    require(
        (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns),
        f"evidence changed while reading: {path}",
    )
    return data


def load(path: Path) -> tuple[dict[str, Any], bytes]:
    data = strict_bytes(path)
    value = json.loads(data)
    require(isinstance(value, dict), f"evidence is not an object: {path}")
    return value, data


def digest(path: Path, expected: str | None = None) -> str:
    observed = sha_bytes(strict_bytes(path))
    if expected is not None:
        require(observed == expected, f"digest mismatch: {path}")
    return observed


def budget(command: Any) -> float:
    require(isinstance(command, list), "episode command missing")
    require(command.count("--max-budget-usd") == 1, "episode budget flag missing or duplicated")
    index = command.index("--max-budget-usd")
    require(index + 1 < len(command), "episode budget value missing")
    return float(command[index + 1])


def verify_ref(reference: Any, expected_path: Path) -> None:
    require(isinstance(reference, dict), "file reference missing")
    data = strict_bytes(expected_path)
    require(reference.get("bytes") == len(data), f"reference byte count mismatch: {expected_path}")
    require(reference.get("sha256") == sha_bytes(data), f"reference digest mismatch: {expected_path}")


def verify_pair_record(
    root: Path,
    relative: str,
    *,
    case_id: str,
    rank: int,
    arm: str,
) -> tuple[dict[str, Any], dict[str, Any], str, str]:
    directory = root / relative / arm
    receipt_path = directory / "episode-receipt.json"
    verification_path = directory / "episode-verification.json"
    receipt, receipt_bytes = load(receipt_path)
    verification, verification_bytes = load(verification_path)
    for document in (receipt, verification):
        require(document.get("case_id") == case_id, f"case mismatch: {relative}/{arm}")
        require(document.get("rank") == rank, f"rank mismatch: {relative}/{arm}")
        require(document.get("arm") == arm, f"arm mismatch: {relative}/{arm}")
    verify_ref(verification.get("episode_receipt"), receipt_path)
    return receipt, verification, sha_bytes(receipt_bytes), sha_bytes(verification_bytes)


def verify(root: Path, issue_dir: Path) -> dict[str, Any]:
    root = root.resolve(strict=True)
    issue_dir = issue_dir.resolve(strict=True)
    digest(issue_dir / "registration.json", ORIGINAL_REGISTRATION)
    digest(issue_dir / "selection.json", ORIGINAL_SELECTION)
    amended_registration, amended_registration_bytes = load(
        issue_dir / "registration-timeout-1200-budget-6.json"
    )
    amended_selection, amended_selection_bytes = load(
        issue_dir / "selection-timeout-1200-budget-6.json"
    )
    require(sha_bytes(amended_registration_bytes) == AMENDED_REGISTRATION, "amended registration drift")
    require(sha_bytes(amended_selection_bytes) == AMENDED_SELECTION, "amended selection drift")
    amendment = amended_registration.get("runtime_amendment")
    require(isinstance(amendment, dict), "runtime amendment missing")
    require(amendment.get("original_registration_sha256") == ORIGINAL_REGISTRATION, "original registration binding drift")
    require(amendment.get("original_selection_sha256") == ORIGINAL_SELECTION, "original selection binding drift")
    require(amendment.get("original_wall_seconds") == 600, "original wall limit drift")
    require(amendment.get("amended_wall_seconds") == 1200, "amended wall limit drift")
    require(amendment.get("original_budget_usd") == 3.0, "original budget drift")
    require(amendment.get("amended_budget_usd") == 6.0, "amended budget drift")
    require(amendment.get("result_classification") == "amended_development_selector", "classification drift")
    require(amendment.get("fresh_sessions_required") is True, "fresh-session requirement missing")
    require(amendment.get("resume_allowed") is False, "resume unexpectedly allowed")

    rerun = {(item["case_id"], item["arm"]): item for item in amendment.get("rerun_episodes", [])}
    retained = {(item["case_id"], item["arm"]): item for item in amendment.get("retained_episodes", [])}
    require(set(rerun) == {(XARRAY, "A"), (XARRAY, "T")}, "rerun scope drift")
    require(set(retained) == {(DJANGO, "A"), (DJANGO, "T")}, "retained scope drift")

    original_sessions: set[str] = set()
    final_sessions: set[str] = set()
    episode_digests: dict[str, dict[str, str]] = {}
    for arm in ("A", "T"):
        receipt, verification, receipt_sha, verification_sha = verify_pair_record(
            root, "original-xarray", case_id=XARRAY, rank=1, arm=arm
        )
        expected = rerun[(XARRAY, arm)]
        require(receipt_sha == expected["receipt_sha256"], f"original xarray {arm} receipt drift")
        require(verification_sha == expected["verification_sha256"], f"original xarray {arm} verification drift")
        require(receipt.get("timed_out") is True and receipt.get("returncode") == 143, f"original xarray {arm} was not retained as timeout")
        timing = receipt.get("timing_ledger")
        require(isinstance(timing, dict) and timing.get("model_wall_seconds", 0) >= 600, f"original xarray {arm} did not reach wall ceiling")
        require(budget(receipt.get("command")) == 3.0, f"original xarray {arm} budget drift")
        require(receipt.get("evaluator_authorized") is False, f"original xarray {arm} evaluator authorization drift")
        require(receipt.get("official_evaluator_invoked") is False, f"original xarray {arm} evaluator invocation drift")
        require(verification.get("evaluator_authorized") is False, f"original xarray {arm} verification authorization drift")
        require(verification.get("official_evaluator_invoked") is False, f"original xarray {arm} verification evaluator drift")
        require(receipt.get("registration", {}).get("sha256") == ORIGINAL_REGISTRATION, f"original xarray {arm} registration drift")
        require(receipt.get("selection", {}).get("sha256") == ORIGINAL_SELECTION, f"original xarray {arm} selection drift")
        session = receipt.get("session_id")
        require(isinstance(session, str) and session, f"original xarray {arm} session missing")
        require(session not in original_sessions, "original xarray sessions overlap")
        original_sessions.add(session)
        episode_digests[f"original_xarray_{arm}"] = {"receipt": receipt_sha, "verification": verification_sha}

    for arm in ("A", "T"):
        receipt, verification, receipt_sha, verification_sha = verify_pair_record(
            root, "final/xarray", case_id=XARRAY, rank=1, arm=arm
        )
        require(receipt.get("timed_out") is False and receipt.get("returncode") == 0, f"final xarray {arm} did not complete")
        require(budget(receipt.get("command")) == 6.0, f"final xarray {arm} budget drift")
        require(receipt.get("registration", {}).get("sha256") == AMENDED_REGISTRATION, f"final xarray {arm} registration drift")
        require(receipt.get("selection", {}).get("sha256") == AMENDED_SELECTION, f"final xarray {arm} selection drift")
        require(receipt.get("policy_compliant") is True and receipt.get("evaluator_authorized") is True, f"final xarray {arm} not evaluator-authorized")
        require(receipt.get("official_evaluator_invoked") is False, f"final xarray {arm} reports upstream evaluator feedback")
        require(verification.get("evidence_complete") is True and verification.get("policy_compliant") is True and verification.get("evaluator_authorized") is True, f"final xarray {arm} verification incomplete")
        session = receipt.get("session_id")
        require(isinstance(session, str) and session not in original_sessions and session not in final_sessions, f"final xarray {arm} session was reused")
        final_sessions.add(session)
        episode_digests[f"final_xarray_{arm}"] = {"receipt": receipt_sha, "verification": verification_sha}

    for arm in ("A", "T"):
        receipt, verification, receipt_sha, verification_sha = verify_pair_record(
            root, "final/django", case_id=DJANGO, rank=2, arm=arm
        )
        expected = retained[(DJANGO, arm)]
        require(receipt_sha == expected["receipt_sha256"], f"retained django {arm} receipt drift")
        require(verification_sha == expected["verification_sha256"], f"retained django {arm} verification drift")
        require(budget(receipt.get("command")) == 3.0, f"retained django {arm} budget drift")
        require(receipt.get("registration", {}).get("sha256") == ORIGINAL_REGISTRATION, f"retained django {arm} registration drift")
        require(receipt.get("selection", {}).get("sha256") == ORIGINAL_SELECTION, f"retained django {arm} selection drift")
        require(receipt.get("policy_compliant") is True and receipt.get("evaluator_authorized") is True, f"retained django {arm} not evaluator-authorized")
        require(receipt.get("official_evaluator_invoked") is False, f"retained django {arm} reports upstream evaluator feedback")
        require(verification.get("evidence_complete") is True and verification.get("policy_compliant") is True and verification.get("evaluator_authorized") is True, f"retained django {arm} verification incomplete")
        episode_digests[f"final_django_{arm}"] = {"receipt": receipt_sha, "verification": verification_sha}

    run_manifest, run_manifest_bytes = load(root / "final/run-manifest.json")
    require(run_manifest.get("registration", {}).get("sha256") == AMENDED_REGISTRATION, "run manifest registration drift")
    require(run_manifest.get("selection", {}).get("sha256") == AMENDED_SELECTION, "run manifest selection drift")
    run_cases = {case["instance_id"]: case for case in run_manifest.get("cases", [])}
    require(set(run_cases) == {XARRAY, DJANGO}, "run manifest case set drift")
    manifest_xarray_sessions = {run_cases[XARRAY]["arms"][arm]["session_id"] for arm in ("A", "T")}
    require(manifest_xarray_sessions == final_sessions, "run manifest does not prebind final xarray sessions")

    preparation, preparation_bytes = load(root / "final/preparation-receipt.json")
    require(preparation.get("models_launched") == 0 and preparation.get("official_evaluator_invocations") == 0, "amended preparation was not zero-invocation")
    require(preparation.get("run_manifest", {}).get("sha256") == sha_bytes(run_manifest_bytes), "preparation does not bind run manifest")
    preflight, preflight_bytes = load(root / "final/evaluator-static-preflight.receipt.json")
    require(preflight.get("official_evaluator_invocations") == 0, "evaluator preflight was not zero-invocation")

    batch_path = root / "final/evaluation-batch.receipt.json"
    batch, batch_bytes = load(batch_path)
    require(sha_bytes(batch_bytes) == EVALUATION_BATCH, "evaluation batch drift")
    require(batch.get("valid") is True and batch.get("failures") == [], "evaluation batch invalid")
    require(batch.get("official_evaluations_authorized") == 4, "evaluation authorization count drift")
    require(batch.get("official_evaluations_started") == 4 and batch.get("official_evaluations_recorded") == 4, "evaluation count drift")
    require(batch.get("model_output_delivery") == "none; evaluator outputs remain out-of-band", "evaluator feedback boundary drift")
    isolation = batch.get("environment", {}).get("model_session_isolation", {})
    require(isolation.get("all_absent") is True and isolation.get("checked_session_count") == 4, "model/evaluator session isolation drift")

    evaluation_paths = {
        (XARRAY, "A"): root / "final/evaluations/xarray/A/evaluation.receipt.json",
        (XARRAY, "T"): root / "final/evaluations/xarray/T/evaluation.receipt.json",
        (DJANGO, "A"): root / "final/evaluations/django/A/evaluation.receipt.json",
        (DJANGO, "T"): root / "final/evaluations/django/T/evaluation.receipt.json",
    }
    evaluation_digests: dict[str, str] = {}
    for item in batch.get("results", []):
        key = (item.get("case_id"), item.get("arm"))
        require(key in evaluation_paths, f"unexpected evaluation result: {key}")
        path = evaluation_paths[key]
        observed = digest(path)
        require(item.get("receipt", {}).get("sha256") == observed, f"batch evaluation digest mismatch: {key}")
        evaluation, _ = load(path)
        require(evaluation.get("official_evaluator_invocations") == 1, f"evaluation invocation count drift: {key}")
        require(evaluation.get("official_evaluator_invocation_confirmed") is True, f"evaluation not confirmed: {key}")
        require(evaluation.get("valid_official_outputs") is True, f"official outputs invalid: {key}")
        require(evaluation.get("model_output_delivery") == "none", f"evaluation feedback delivered: {key}")
        evaluation_digests[f"{key[0]}:{key[1]}"] = observed
    require(len(evaluation_digests) == 4, "evaluation result set incomplete")

    result_path = root / "final/selection-result.json"
    result, result_bytes = load(result_path)
    require(sha_bytes(result_bytes) == SELECTION_RESULT, "selection result drift")
    require(result.get("decision") == "selected_T", "published decision is not selected_T")
    require(result.get("selection_authoritative") is True, "selection is not authoritative")
    require(result.get("protocol_classification") == "amended_development_selector", "result protocol classification drift")
    require(result.get("no_model_feedback_verified") is True, "result does not verify feedback isolation")
    require(result.get("evaluation_batch", {}).get("sha256") == EVALUATION_BATCH, "result does not bind evaluation batch")
    episodes = result.get("episodes")
    require(isinstance(episodes, list) and len(episodes) == 4, "selection result episode set incomplete")
    totals: dict[str, dict[str, float | int]] = {
        "A": {"resolved": 0, "regressions": 0, "tokens": 0, "wall": 0.0},
        "T": {"resolved": 0, "regressions": 0, "tokens": 0, "wall": 0.0},
    }
    for episode in episodes:
        arm = episode["arm"]
        require(episode.get("outcome_valid") is True and episode.get("policy_verifier_clean") is True, "invalid episode entered selection")
        require(episode.get("official_evaluator_invocations") == 1, "selection episode evaluator count drift")
        require(episode.get("evaluation_receipt", {}).get("sha256") == evaluation_digests[f"{episode['case_id']}:{arm}"], "selection episode evaluation digest drift")
        totals[arm]["resolved"] += int(episode.get("resolved") is True)
        totals[arm]["regressions"] += episode["pass_to_pass_regressions"]
        totals[arm]["tokens"] += episode["provider_total_tokens"]
        totals[arm]["wall"] += episode["combined_pre_evaluator_wall_seconds"]
    require(totals["A"]["resolved"] == totals["T"]["resolved"] == 2, "resolution aggregate drift")
    require(totals["A"]["regressions"] == totals["T"]["regressions"] == 0, "regression aggregate drift")
    require(totals["A"]["tokens"] == 9_394_654 and totals["T"]["tokens"] == 4_367_033, "token aggregate drift")
    require(100 * totals["T"]["tokens"] <= 85 * totals["A"]["tokens"], "registered token threshold no longer passes")
    require(100 * totals["T"]["wall"] <= 80 * totals["A"]["wall"], "registered wall threshold no longer passes")

    return {
        "schema_version": "issue825-published-result-verification-v1",
        "valid": True,
        "classification": "amended_development_selector",
        "decision": "selected_T",
        "checks": {
            "original_xarray_symmetric_wall_timeout": True,
            "retained_harness_evaluator_invocations_before_amendment": 0,
            "fresh_amended_xarray_sessions": True,
            "retained_django_pair_unchanged": True,
            "official_evaluations_once": 4,
            "model_feedback_delivered": False,
            "selection_recomputed": True,
        },
        "protocol": {
            "original_registration_sha256": ORIGINAL_REGISTRATION,
            "original_selection_sha256": ORIGINAL_SELECTION,
            "amended_registration_sha256": AMENDED_REGISTRATION,
            "amended_selection_sha256": AMENDED_SELECTION,
        },
        "evidence": {
            "episode_digests": episode_digests,
            "run_manifest_sha256": sha_bytes(run_manifest_bytes),
            "preparation_receipt_sha256": sha_bytes(preparation_bytes),
            "evaluator_static_preflight_sha256": sha_bytes(preflight_bytes),
            "evaluation_digests": evaluation_digests,
            "evaluation_batch_sha256": EVALUATION_BATCH,
            "selection_result_sha256": SELECTION_RESULT,
        },
        "aggregates": {
            "A": {"resolved": 2, "regressions": 0, "provider_total_tokens": 9_394_654, "combined_wall_seconds": totals["A"]["wall"]},
            "T": {"resolved": 2, "regressions": 0, "provider_total_tokens": 4_367_033, "combined_wall_seconds": totals["T"]["wall"]},
        },
    }


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    default_root = Path(__file__).with_name("evidence") / "amended-selector"
    value.add_argument("--evidence-root", type=Path, default=default_root)
    value.add_argument("--issue-dir", type=Path, default=Path(__file__).parent)
    return value


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        result = verify(args.evidence_root, args.issue_dir)
    except (EvidenceError, OSError, json.JSONDecodeError, KeyError, TypeError, ValueError) as exc:
        print(json.dumps({"schema_version": "issue825-published-result-verification-v1", "valid": False, "error": str(exc)}, sort_keys=True))
        return 1
    print(canonical(result).decode(), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
