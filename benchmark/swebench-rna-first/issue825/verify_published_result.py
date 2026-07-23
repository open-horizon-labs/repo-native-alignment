#!/usr/bin/env python3
"""Offline verifier for the published #825 amended selector evidence.

This file is deliberately outside the registered model/evaluator runtime.  It
audits post-outcome evidence without changing or reinterpreting that runtime.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shlex
from pathlib import Path
from typing import Any, Mapping, Sequence


ORIGINAL_REGISTRATION = "dbfcc2553cd5cda945e53295f11ea21100265aff3c8131e45f6543fc7cf56fbc"
ORIGINAL_SELECTION = "2898db4e0a083fd3facd4799534a24b8e1344af47a6142c4ba668f0a08216a66"
AMENDED_REGISTRATION = "700aab84dfe4df7e2cf2ff1af206be4bd4e336f1e3c74dc93d28e3b38e8a8141"
AMENDED_SELECTION = "794aa76a7d9bcbfb5148ba59e88ff060ef8052514884f46b7cf92dcd36c6d0c4"
EVALUATION_BATCH = "794996b8023dbe3fb2e0164bebd9d9b7c990bf0307026cdf9427cd6e5be2fc5c"
SELECTION_RESULT = "350dd532c90c05d8b8d66883576afa4608ece9dca6ebeb58afd38aa7941af127"
SELECTION_CORRECTION = "970116a6420614edae3416515eda9ae7bb7cc39a51e498da8cb65f02fefdfa4a"
RUN_MANIFEST = "9a11ce063ef6f08ff50546a2aa34a14fd77d3bb5b57cc10e946b9a17f26471e1"
ORIGINAL_RUN_MANIFEST = "5148945367c90e10c7952b84c78d81ee382af424baa03f044e2164f3ca657a93"
PREPARATION = "71602d955239f0f1749b3c30fad60867a38b0041b09498fe06bc5d9a7ece2005"
PREFLIGHT = "1a87daa79ffa76beb09a1a2be2a18b68286dfa7128bcf9b3c453b10efbfc22c8"
TRUST_ANCHOR = "fa9aa50650d65c82b553b56e1a1e8f0f96267079b334f0f708bbf3ad40507952"
SOURCE_COMMIT = "c90e58889d41cfd48089ee5659e9fa512317a7cb"
RUNNER = "196d58e051cdc52ff5f19c77f371cda3955c4d4fdfb14c69f0fc5ed15751879e"
EVALUATOR_SCRIPT = "987a3aed681fb4feb2771a69ee994b21320e2b9d02a91687c6ccf138d89843a2"
EVALUATOR_PLAN = "d0e1139a816dffadefcd886707a131b06a2b443f2c3cb12798e1a178f2ea611b"
TERMINAL_SET = "8fa95fb7dddc5c74518895920cf5eafa3a9e1ea23512ac62e3f9e6996c02bc1d"
EXPECTED_EVALUATIONS = 4
XARRAY_T_LEDGER = "d1d43c196a4532b02818facd0bcbeed8452d3217929c7357b412e913b5c8a18b"
FORBIDDEN_ATTEMPT_POLICY = (
    b"Do not use network access, hidden tests, evaluator output, gold patches, "
    b"or evidence from another arm."
)
XARRAY_T_POLICY_VIOLATIONS = {
    27: {
        "sequence": 28,
        "command_sha256": "7fa666b253549de3e2496a56fcd1ef07314432fea39258c1c90a3040f8b236d8",
        "classification": "forbidden_network_access_attempt",
    },
    37: {
        "sequence": 38,
        "command_sha256": "02ac12064a2cfcb44e7d588b45a46b006b1bd85640423e314652d79fd4c7e870",
        "classification": "forbidden_other_arm_evidence_access_attempt",
    },
}
XARRAY = "pydata__xarray-4687"
DJANGO = "django__django-15503"
CASE_IDENTITIES = {
    XARRAY: {
        "base_commit": "d3b6aa6d8b997df115a53c001d00222a0f92f63a",
        "base_tree": "8b555e27a5779a9cc342acca2011fd59ebeac97a",
        "runtime_identity": "bdc193d2f5091fdc6a18869d31ac03ba1cc48e5dba4c2009174dfe63e7d31e13",
    },
    DJANGO: {
        "base_commit": "859a87d873ce7152af73ab851653b4e1c3ffea4c",
        "base_tree": "72cda8bf81eb3bbd3e104850f72e6a145c9008fe",
        "runtime_identity": "5e6a5c0659889ad1621ea04a607c7a30bffad677d4c3b2ac139cc143774dc395",
    },
}

REGISTERED_SOURCE_FILES = {
    "system_prefix_sha256": "system-prefix.txt",
    "system_suffix_sha256": "system-suffix.txt",
    "rna_query_sha256": "rna_query.py",
    "rna_traverse_sha256": "rna_traverse.py",
    "tool_supervisor_sha256": "tool_supervisor.py",
    "supervisor_template_sha256": "supervisor.template.json",
    "claude_settings_template_sha256": "claude-settings.template.json",
    "validator_sha256": "validate_episode.py",
    "common_supervisor_sha256": "common_supervisor.py",
    "case_selector_sha256": "select_cases.py",
    "runner_sha256": "run_selector.py",
    "verifier_sha256": "verify_selector.py",
    "exclusions_sha256": "exclusions.json",
    "evaluator_runner_sha256": "evaluator_runner.py",
    "evaluator_plan_template_sha256": "evaluator-plan.template.json",
    "result_selector_sha256": "select_result.py",
    "selector_harness_tests_sha256": "tests/test_selector_harness.py",
    "evaluator_tests_sha256": "test_evaluator_tools.py",
}

T_EVIDENCE_PATHS = {
    XARRAY: {
        "ledger": "final/model-evidence/xarray/T/actor-tool-ledger.json",
        "system": "final/model-evidence/xarray/T/treatment-system.bin",
    },
    DJANGO: {
        "ledger": "final/model-evidence/django/T/actor-tool-ledger.json",
        "system": "final/model-evidence/django/T/treatment-system.bin",
    },
}

CREDENTIAL_PATTERNS = (
    re.compile(rb"ANTHROPIC_API_KEY", re.IGNORECASE),
    re.compile(rb"sk-ant-", re.IGNORECASE),
    re.compile(rb"\.claude-swe", re.IGNORECASE),
    re.compile(rb"(?:api_key|api-key)\s*[:=]\s*[\"']?[^\s\"',}]+", re.IGNORECASE),
)

EXPECTED_FILES = {
    "README.md",
    "SHA256SUMS",
    "verification-receipt.json",
    "original-xarray/A/episode-receipt.json",
    "original-xarray/A/episode-verification.json",
    "original-xarray/T/episode-receipt.json",
    "original-xarray/T/episode-verification.json",
    "final/django/A/episode-receipt.json",
    "final/django/A/episode-verification.json",
    "final/django/T/episode-receipt.json",
    "final/django/T/episode-verification.json",
    "final/xarray/A/episode-receipt.json",
    "final/xarray/A/episode-verification.json",
    "final/xarray/T/episode-receipt.json",
    "final/xarray/T/episode-verification.json",
    "final/evidence-trust-anchor.json",
    "final/evaluation-batch.receipt.json",
    "final/evaluator-static-preflight.receipt.json",
    "final/preparation-receipt.json",
    "final/run-manifest.json",
    "final/selection-result.json",
    "final/superseding-selection-correction.json",
    "final/evaluations/django/A/evaluation.receipt.json",
    "final/evaluations/django/T/evaluation.receipt.json",
    "final/evaluations/xarray/A/evaluation.receipt.json",
    "final/evaluations/xarray/T/evaluation.receipt.json",
    "final/model-evidence/xarray/T/actor-tool-ledger.json",
    "final/model-evidence/xarray/T/treatment-system.bin",
    "final/model-evidence/django/T/actor-tool-ledger.json",
    "final/model-evidence/django/T/treatment-system.bin",
}


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


def flag_value(command: Any, flag: str) -> str:
    require(isinstance(command, list), "episode command missing")
    require(command.count(flag) == 1, f"{flag} missing or duplicated")
    index = command.index(flag)
    require(index + 1 < len(command), f"{flag} value missing")
    value = command[index + 1]
    require(isinstance(value, str) and value, f"{flag} value invalid")
    return value


def verify_inventory(root: Path) -> None:
    observed: set[str] = set()
    pending = [root]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                relative = Path(entry.path).relative_to(root).as_posix()
                require(not entry.is_symlink(), f"symlink in evidence tree: {relative}")
                if entry.is_dir(follow_symlinks=False):
                    pending.append(Path(entry.path))
                elif entry.is_file(follow_symlinks=False):
                    observed.add(relative)
                else:
                    raise EvidenceError(f"special file in evidence tree: {relative}")
    missing = sorted(EXPECTED_FILES - observed)
    extra = sorted(observed - EXPECTED_FILES)
    require(not missing and not extra, f"evidence inventory drift; missing={missing}, extra={extra}")
    for relative in sorted(EXPECTED_FILES):
        data = strict_bytes(root / relative)
        require(
            not any(pattern.search(data) for pattern in CREDENTIAL_PATTERNS),
            f"credential marker in published evidence: {relative}",
        )


def verify_registered_sources(issue_dir: Path, registered_files: Any) -> None:
    require(isinstance(registered_files, dict), "registered source file map missing")
    require(set(registered_files) == set(REGISTERED_SOURCE_FILES), "registered source file set drift")
    for key, relative in REGISTERED_SOURCE_FILES.items():
        require(registered_files.get(key) == digest(issue_dir / relative), f"registered source drift: {relative}")


def report_outcome(report: Mapping[str, Any], case_id: str) -> dict[str, int | bool]:
    require(set(report) == {case_id}, f"official report case set drift: {case_id}")
    case = report[case_id]
    require(isinstance(case, dict), f"official report case invalid: {case_id}")
    statuses = case.get("tests_status")
    require(isinstance(statuses, dict), f"official report test status missing: {case_id}")

    def counts(category: str) -> tuple[int, int]:
        value = statuses.get(category)
        require(isinstance(value, dict), f"official report category missing: {case_id}/{category}")
        success = value.get("success")
        failure = value.get("failure")
        require(isinstance(success, list) and isinstance(failure, list), f"official report category invalid: {case_id}/{category}")
        require(len(set(success)) == len(success) and len(set(failure)) == len(failure), f"official report duplicate tests: {case_id}/{category}")
        require(not set(success).intersection(failure), f"official report conflicting tests: {case_id}/{category}")
        return len(success), len(success) + len(failure)

    required_passed, required_total = counts("FAIL_TO_PASS")
    pass_to_pass_passed, pass_to_pass_total = counts("PASS_TO_PASS")
    regressions = pass_to_pass_total - pass_to_pass_passed
    resolved = required_total > 0 and required_passed == required_total and regressions == 0
    require(case.get("resolved") is resolved, f"official report resolution claim drift: {case_id}")
    return {
        "resolved": resolved,
        "required_passed": required_passed,
        "required_total": required_total,
        "pass_to_pass_regressions": regressions,
        "pass_to_pass_total": pass_to_pass_total,
    }


def verify_session(receipt: Mapping[str, Any], label: str) -> str:
    session = receipt.get("session_id")
    require(isinstance(session, str) and session, f"{label} session missing")
    command = receipt.get("command")
    require(flag_value(command, "--session-id") == session, f"{label} command/session mismatch")
    require(
        not any(isinstance(item, str) and (item == "--continue" or item.startswith("--resume")) for item in command),
        f"{label} resume command forbidden",
    )
    require(
        not any("resume" in str(key).lower() for key in receipt),
        f"{label} resume lineage forbidden",
    )
    transcripts = receipt.get("transcripts")
    require(isinstance(transcripts, list) and len(transcripts) == 1, f"{label} transcript lineage is not fresh")
    return session


def verify_usage(receipt: Mapping[str, Any], verification: Mapping[str, Any], label: str) -> tuple[int, float]:
    ledger = receipt.get("token_ledger")
    require(isinstance(ledger, dict) and ledger == verification.get("token_ledger"), f"{label} usage ledger mismatch")
    require(ledger.get("valid") is True and ledger.get("errors") == [], f"{label} usage ledger invalid")
    require(ledger.get("model_invoked") is True, f"{label} model usage absent")
    fields = ("input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")
    values = [ledger.get(field) for field in fields]
    require(all(isinstance(value, int) and value >= 0 for value in values), f"{label} usage components invalid")
    total = ledger.get("provider_total_tokens")
    require(isinstance(total, int) and total > 0 and total == sum(values), f"{label} token total invalid")
    require(isinstance(ledger.get("provider_responses"), int) and ledger["provider_responses"] > 0, f"{label} provider usage absent")
    require(isinstance(ledger.get("cli_turns"), int) and ledger["cli_turns"] > 0, f"{label} turn usage absent")
    timing = receipt.get("timing_ledger")
    require(isinstance(timing, dict) and timing == verification.get("timing_ledger"), f"{label} timing ledger mismatch")
    wall = timing.get("combined_pre_evaluator_wall_seconds")
    require(isinstance(wall, (int, float)) and wall > 0, f"{label} pre-evaluator wall time invalid")
    return total, float(wall)


def verify_ref(reference: Any, expected_path: Path) -> None:
    require(isinstance(reference, dict), "file reference missing")
    data = strict_bytes(expected_path)
    require(reference.get("bytes") == len(data), f"reference byte count mismatch: {expected_path}")
    require(reference.get("sha256") == sha_bytes(data), f"reference digest mismatch: {expected_path}")


def prove_xarray_t_policy_violations(issue_dir: Path, ledger_path: Path) -> list[dict[str, Any]]:
    suffix = strict_bytes(issue_dir / "system-suffix.txt")
    require(
        sha_bytes(suffix) == "4e5855ad0dbd582c56a997cba07f3de962427760c97c0e9b980140f5e43b1ffd",
        "registered treatment policy source drift",
    )
    require(FORBIDDEN_ATTEMPT_POLICY in suffix, "registered forbidden-attempt policy missing")
    ledger, ledger_bytes = load(ledger_path)
    require(sha_bytes(ledger_bytes) == XARRAY_T_LEDGER, "xarray T actor ledger drift")
    require(ledger.get("arm") == "T", "xarray treatment ledger arm drift")
    actions = ledger.get("actions")
    require(isinstance(actions, list), "xarray T actor tool ledger invalid")

    proven: list[dict[str, Any]] = []
    for model_action_index, expected in XARRAY_T_POLICY_VIOLATIONS.items():
        matches = [
            item
            for item in actions
            if isinstance(item, dict) and item.get("model_action_index") == model_action_index
        ]
        require(len(matches) == 1, f"xarray T action {model_action_index} missing or duplicated")
        action = matches[0]
        require(
            action.get("actor") == "model"
            and action.get("action") == "tool_attempt"
            and action.get("tool") == "Bash",
            f"xarray T action {model_action_index} is not a model Bash attempt",
        )
        require(action.get("sequence") == expected["sequence"], f"xarray T action {model_action_index} sequence drift")
        require(
            action.get("common_decision") == "allow"
            and action.get("treatment_decision") == "allow",
            f"xarray T action {model_action_index} was not allowed to execute",
        )
        command = action.get("bash_command")
        require(
            isinstance(command, str) and command == action.get("tool_input", {}).get("command"),
            f"xarray T action {model_action_index} command drift",
        )
        require(
            sha_bytes(command.encode()) == expected["command_sha256"],
            f"xarray T action {model_action_index} command digest drift",
        )
        if model_action_index == 27:
            require(
                "pip3 download --no-deps" in command,
                "xarray T action 27 does not prove the forbidden network attempt",
            )
        elif model_action_index == 37:
            require(
                "/checkouts/rank-01-pydata__xarray-4687/A" in command,
                "xarray T action 37 does not prove the forbidden other-arm access attempt",
            )
        proven.append(
            {
                "allow_decisions": {"common": "allow", "treatment": "allow"},
                "classification": expected["classification"],
                "command_sha256": expected["command_sha256"],
                "model_action_index": model_action_index,
                "sequence": expected["sequence"],
            }
        )
    return proven


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
    for field in ("policy_compliant", "evaluator_authorized", "official_evaluator_invoked"):
        require(
            receipt.get(field) == verification.get(field),
            f"{field} mismatch: {relative}/{arm}",
        )
    require(receipt.get("official_evaluator_invoked") is False, f"pre-evaluator invocation present: {relative}/{arm}")
    require(verification.get("official_evaluator_invoked") is False, f"verification reports evaluator feedback: {relative}/{arm}")
    require(receipt.get("terminal_patch") == verification.get("terminal_patch"), f"terminal patch mismatch: {relative}/{arm}")
    verify_ref(verification.get("episode_receipt"), receipt_path)
    verify_session(receipt, f"{relative}/{arm}")
    return receipt, verification, sha_bytes(receipt_bytes), sha_bytes(verification_bytes)


def verify(root: Path, issue_dir: Path) -> dict[str, Any]:
    root = root.resolve(strict=True)
    issue_dir = issue_dir.resolve(strict=True)
    verify_inventory(root)
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
    verify_registered_sources(issue_dir, amended_registration.get("registered_files"))
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
    episode_evidence: dict[tuple[str, str], dict[str, Any]] = {}
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
        require(verification.get("evidence_complete") is True and verification.get("policy_compliant") is True and verification.get("evaluator_authorized") is True and verification.get("official_evaluator_invoked") is False, f"final xarray {arm} verification incomplete")
        identity = CASE_IDENTITIES[XARRAY]
        require(receipt.get("base_commit") == identity["base_commit"] and receipt.get("base_tree") == identity["base_tree"], f"final xarray {arm} source tree drift")
        require(receipt.get("runtime_identity", {}).get("sha256") == identity["runtime_identity"], f"final xarray {arm} runtime identity drift")
        require(receipt.get("run_manifest", {}).get("sha256") == RUN_MANIFEST, f"final xarray {arm} run manifest drift")
        session = receipt.get("session_id")
        require(isinstance(session, str) and session not in original_sessions and session not in final_sessions, f"final xarray {arm} session was reused")
        final_sessions.add(session)
        tokens, wall = verify_usage(receipt, verification, f"final xarray {arm}")
        episode_evidence[(XARRAY, arm)] = {"tokens": tokens, "wall": wall, "patch": receipt["terminal_patch"]}
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
        require(verification.get("evidence_complete") is True and verification.get("policy_compliant") is True and verification.get("evaluator_authorized") is True and verification.get("official_evaluator_invoked") is False, f"retained django {arm} verification incomplete")
        identity = CASE_IDENTITIES[DJANGO]
        require(receipt.get("base_commit") == identity["base_commit"] and receipt.get("base_tree") == identity["base_tree"], f"retained django {arm} source tree drift")
        require(receipt.get("runtime_identity", {}).get("sha256") == identity["runtime_identity"], f"retained django {arm} runtime identity drift")
        require(receipt.get("run_manifest", {}).get("sha256") == ORIGINAL_RUN_MANIFEST, f"retained django {arm} run manifest drift")
        tokens, wall = verify_usage(receipt, verification, f"retained django {arm}")
        episode_evidence[(DJANGO, arm)] = {"tokens": tokens, "wall": wall, "patch": receipt["terminal_patch"]}
        episode_digests[f"final_django_{arm}"] = {"receipt": receipt_sha, "verification": verification_sha}

    run_manifest, run_manifest_bytes = load(root / "final/run-manifest.json")
    require(sha_bytes(run_manifest_bytes) == RUN_MANIFEST, "run manifest drift")
    require(run_manifest.get("registration", {}).get("sha256") == AMENDED_REGISTRATION, "run manifest registration drift")
    require(run_manifest.get("selection", {}).get("sha256") == AMENDED_SELECTION, "run manifest selection drift")
    require(run_manifest.get("runner", {}).get("sha256") == RUNNER, "run manifest runner drift")
    run_cases = {case["instance_id"]: case for case in run_manifest.get("cases", [])}
    require(set(run_cases) == {XARRAY, DJANGO}, "run manifest case set drift")
    for case_id, case in run_cases.items():
        identity = CASE_IDENTITIES[case_id]
        require(case.get("base_commit") == identity["base_commit"] and case.get("base_tree") == identity["base_tree"], f"run manifest source tree drift: {case_id}")
        cache = case.get("cache")
        require(isinstance(cache, dict), f"run manifest cache identity missing: {case_id}")
        bindings = cache.get("bindings")
        require(isinstance(bindings, list), f"run manifest cache bindings missing: {case_id}")
        binding_map = {item.get("label"): item.get("equals") for item in bindings if isinstance(item, dict)}
        require(len(binding_map) == len(bindings), f"run manifest cache bindings duplicated: {case_id}")
        require(binding_map.get("repository_commit") == identity["base_commit"], f"cache commit drift: {case_id}")
        require(binding_map.get("repository_tree") == identity["base_tree"], f"cache tree drift: {case_id}")
        require(binding_map.get("cache_archive_sha256") == cache.get("archive", {}).get("sha256"), f"cache archive binding drift: {case_id}")
        require(binding_map.get("cache_manifest_sha256") == cache.get("manifest", {}).get("sha256"), f"cache manifest binding drift: {case_id}")
        require(binding_map.get("root") == cache.get("root"), f"cache root binding drift: {case_id}")
        require(binding_map.get("fresh_reopen_status") == "READY", f"cache readiness drift: {case_id}")
        require(binding_map.get("rna_binary_sha256") == run_manifest.get("rna_artifact", {}).get("binary", {}).get("sha256"), f"RNA binary binding drift: {case_id}")
        require(binding_map.get("rna_launcher_sha256") == run_manifest.get("rna_artifact", {}).get("launcher", {}).get("sha256"), f"RNA launcher binding drift: {case_id}")
    manifest_xarray_sessions = {run_cases[XARRAY]["arms"][arm]["session_id"] for arm in ("A", "T")}
    require(manifest_xarray_sessions == final_sessions, "run manifest does not prebind final xarray sessions")

    preparation, preparation_bytes = load(root / "final/preparation-receipt.json")
    require(sha_bytes(preparation_bytes) == PREPARATION, "preparation receipt drift")
    require(preparation.get("models_launched") == 0 and preparation.get("official_evaluator_invocations") == 0, "amended preparation was not zero-invocation")
    require(preparation.get("run_manifest", {}).get("sha256") == sha_bytes(run_manifest_bytes), "preparation does not bind run manifest")
    require(preparation.get("source_commit") == SOURCE_COMMIT, "preparation source commit drift")
    require(preparation.get("runner", {}).get("sha256") == RUNNER, "preparation runner drift")
    require(preparation.get("registration", {}).get("sha256") == AMENDED_REGISTRATION, "preparation registration drift")
    require(preparation.get("selection", {}).get("sha256") == AMENDED_SELECTION, "preparation selection drift")
    preflight, preflight_bytes = load(root / "final/evaluator-static-preflight.receipt.json")
    require(sha_bytes(preflight_bytes) == PREFLIGHT, "evaluator preflight drift")
    require(preflight.get("official_evaluator_invocations") == 0, "evaluator preflight was not zero-invocation")
    require(preflight.get("script_sha256") == EVALUATOR_SCRIPT, "preflight evaluator source drift")
    require(preflight.get("plan_sha256") == EVALUATOR_PLAN, "preflight evaluator plan drift")
    require(preflight.get("terminal_set_digest") == TERMINAL_SET, "preflight terminal set drift")

    batch_path = root / "final/evaluation-batch.receipt.json"
    batch, batch_bytes = load(batch_path)
    require(sha_bytes(batch_bytes) == EVALUATION_BATCH, "evaluation batch drift")
    require(batch.get("valid") is True and batch.get("failures") == [], "evaluation batch invalid")
    require(batch.get("script_sha256") == EVALUATOR_SCRIPT and batch.get("plan_sha256") == EVALUATOR_PLAN, "evaluation batch source drift")
    require(batch.get("terminal_set_digest") == TERMINAL_SET, "evaluation batch terminal set drift")
    require(batch.get("official_evaluations_authorized") == EXPECTED_EVALUATIONS, "evaluation authorization count drift")
    require(batch.get("official_evaluations_started") == EXPECTED_EVALUATIONS and batch.get("official_evaluations_recorded") == EXPECTED_EVALUATIONS, "evaluation count drift")
    require(batch.get("zero_invocation_receipts") == 0, "evaluation batch retained zero-invocation receipt")
    require(batch.get("model_output_delivery") == "none; evaluator outputs remain out-of-band", "evaluator feedback boundary drift")
    isolation = batch.get("environment", {}).get("model_session_isolation", {})
    require(isolation.get("all_absent") is True and isolation.get("checked_session_count") == EXPECTED_EVALUATIONS, "model/evaluator session isolation drift")
    require(batch.get("environment", {}).get("official_evaluator_invocations") == 0, "batch preparation invoked evaluator")

    anchor_path = root / "final/evidence-trust-anchor.json"
    anchor, anchor_bytes = load(anchor_path)
    require(sha_bytes(anchor_bytes) == TRUST_ANCHOR, "evidence trust anchor drift")
    require(anchor.get("schema_version") == "issue825-published-evidence-trust-anchor-v1", "trust anchor schema drift")
    anchored_reports = {item.get("case_id"): item for item in anchor.get("reports", [])}
    require(set(anchored_reports) == {XARRAY, DJANGO}, "anchored official report set drift")
    report_bytes: dict[str, bytes] = {}
    report_outcomes: dict[str, Mapping[str, Any]] = {}
    for case_id, report_anchor in anchored_reports.items():
        try:
            exact_report = base64.b64decode(report_anchor.get("base64"), validate=True)
            report = json.loads(exact_report)
        except Exception as exc:
            raise EvidenceError(f"invalid anchored official report: {case_id}") from exc
        require(sha_bytes(exact_report) == report_anchor.get("sha256"), f"anchored official report digest drift: {case_id}")
        require(not any(pattern.search(exact_report) for pattern in CREDENTIAL_PATTERNS), f"credential marker in official report: {case_id}")
        require(isinstance(report, dict), f"anchored official report invalid: {case_id}")
        report_bytes[case_id] = exact_report
        report_outcomes[case_id] = report_outcome(report, case_id)
    anchored_projections = {(item.get("case_id"), item.get("arm")): item for item in anchor.get("projections", [])}
    require(set(anchored_projections) == {(XARRAY, "T"), (DJANGO, "T")}, "anchored projection set drift")
    for case_id in (XARRAY, DJANGO):
        receipt = load(root / ("final/xarray/T/episode-receipt.json" if case_id == XARRAY else "final/django/T/episode-receipt.json"))[0]
        query = receipt.get("query_evidence")
        require(isinstance(query, dict) and query.get("succeeded") is True, f"T query evidence invalid: {case_id}")
        projected_ids = query.get("projected_stable_code_ids")
        require(isinstance(projected_ids, list) and projected_ids, f"T projected IDs missing: {case_id}")
        require(projected_ids == query.get("raw_stable_code_ids"), f"T raw/projected ID set drift: {case_id}")
        projection_anchor = anchored_projections[(case_id, "T")]
        try:
            projection = base64.b64decode(projection_anchor.get("base64"), validate=True)
        except Exception as exc:
            raise EvidenceError(f"invalid anchored projection: {case_id}") from exc
        require(sha_bytes(projection) == projection_anchor.get("sha256"), f"anchored projection digest drift: {case_id}")
        require(not any(pattern.search(projection) for pattern in CREDENTIAL_PATTERNS), f"credential marker in injected projection: {case_id}")
        require(query.get("wrapper_stdout", {}).get("sha256") == projection_anchor.get("sha256"), f"projection receipt digest drift: {case_id}")
        require(all(stable_id.encode() in projection for stable_id in projected_ids), f"selected stable ID absent from injected projection: {case_id}")
        require(flag_value(receipt.get("command"), "--append-system-prompt-file") == receipt.get("treatment_system", {}).get("path"), f"T projection was not injected: {case_id}")
        paths = T_EVIDENCE_PATHS[case_id]
        ledger_path = root / paths["ledger"]
        system_path = root / paths["system"]
        verify_ref(receipt.get("actor_tool_ledger"), ledger_path)
        verify_ref(receipt.get("treatment_system"), system_path)
        ledger, _ = load(ledger_path)
        actions = ledger.get("actions")
        require(isinstance(actions, list), f"T actor tool ledger invalid: {case_id}")
        model_actions = [item for item in actions if isinstance(item, dict) and item.get("actor") == "model"]
        require(model_actions and model_actions[0].get("model_action_index") == 1, f"T first model action missing: {case_id}")
        first = model_actions[0]
        require(first.get("action") == "tool_attempt" and first.get("tool") == "Bash", f"T first model action was not a traversal: {case_id}")
        command = first.get("bash_command")
        require(isinstance(command, str) and command == first.get("tool_input", {}).get("command"), f"T first model command drift: {case_id}")
        argv = shlex.split(command)
        require(
            len(argv) == 5
            and Path(argv[0]).name == "rna_traverse.py"
            and argv[1] == "--node"
            and argv[3:] == ["--mode", "neighbors"],
            f"T first model tool was not an unchained neighbors traversal: {case_id}",
        )
        traversed_id = argv[2]
        system = strict_bytes(system_path)
        require(traversed_id in projected_ids, f"T traversal ID was not injected: {case_id}")
        require(traversed_id.encode() in projection, f"T traversal ID absent from exact projection: {case_id}")
        require(traversed_id.encode() in system, f"T traversal ID absent from exact treatment system: {case_id}")
        require(projection in system, f"exact injected projection absent from treatment system: {case_id}")

    proven_violations = prove_xarray_t_policy_violations(
        issue_dir,
        root / T_EVIDENCE_PATHS[XARRAY]["ledger"],
    )

    evaluation_paths = {
        (XARRAY, "A"): root / "final/evaluations/xarray/A/evaluation.receipt.json",
        (XARRAY, "T"): root / "final/evaluations/xarray/T/evaluation.receipt.json",
        (DJANGO, "A"): root / "final/evaluations/django/A/evaluation.receipt.json",
        (DJANGO, "T"): root / "final/evaluations/django/T/evaluation.receipt.json",
    }
    evaluation_digests: dict[str, str] = {}
    evaluation_outcomes: dict[tuple[str, str], Mapping[str, Any]] = {}
    results = batch.get("results")
    require(isinstance(results, list) and len(results) == EXPECTED_EVALUATIONS, "evaluation result count drift")
    for item in results:
        key = (item.get("case_id"), item.get("arm"))
        require(key in evaluation_paths, f"unexpected evaluation result: {key}")
        path = evaluation_paths[key]
        observed = digest(path)
        require(item.get("receipt", {}).get("sha256") == observed, f"batch evaluation digest mismatch: {key}")
        evaluation, _ = load(path)
        require(evaluation.get("official_evaluator_invocations") == 1, f"evaluation invocation count drift: {key}")
        require(evaluation.get("official_evaluator_invocation_authorized") is True, f"evaluation not authorized: {key}")
        require(evaluation.get("official_evaluator_invocation_confirmed") is True, f"evaluation not confirmed: {key}")
        require(evaluation.get("valid_official_outputs") is True, f"official outputs invalid: {key}")
        require(evaluation.get("model_output_delivery") == "none", f"evaluation feedback delivered: {key}")
        require(evaluation.get("terminal_set_digest") == TERMINAL_SET, f"evaluation terminal set drift: {key}")
        require(evaluation.get("returncode") == 0 and evaluation.get("timed_out") is False, f"evaluation did not complete: {key}")
        report_ref = evaluation.get("official_outputs", {}).get("report")
        require(isinstance(report_ref, dict), f"official report reference missing: {key}")
        require(report_ref.get("bytes") == len(report_bytes[key[0]]), f"official report byte count mismatch: {key}")
        require(report_ref.get("sha256") == sha_bytes(report_bytes[key[0]]), f"official report digest mismatch: {key}")
        patch = evaluation.get("official_outputs", {}).get("patch")
        require(
            isinstance(patch, dict)
            and patch.get("sha256") == episode_evidence[key]["patch"].get("sha256")
            and patch.get("bytes") == episode_evidence[key]["patch"].get("bytes"),
            f"terminal patch/evaluator linkage drift: {key}",
        )
        outcome = report_outcomes[key[0]]
        evaluation_outcomes[key] = outcome
        evaluation_digests[f"{key[0]}:{key[1]}"] = observed
    require(len(evaluation_digests) == EXPECTED_EVALUATIONS, "evaluation result set incomplete")

    result_path = root / "final/selection-result.json"
    result, result_bytes = load(result_path)
    require(sha_bytes(result_bytes) == SELECTION_RESULT, "selection result drift")
    require(result.get("decision") == "selected_T", "published decision is not selected_T")
    require(result.get("selection_authoritative") is True, "selection is not authoritative")
    require(result.get("protocol_classification") == "amended_development_selector", "result protocol classification drift")
    require(result.get("no_model_feedback_verified") is True, "result does not verify feedback isolation")
    require(result.get("evaluation_batch", {}).get("sha256") == EVALUATION_BATCH, "result does not bind evaluation batch")
    require(result.get("registration_sha256") == AMENDED_REGISTRATION and result.get("selection_sha256") == AMENDED_SELECTION, "result protocol binding drift")
    require(result.get("evaluator_script_sha256") == EVALUATOR_SCRIPT and result.get("plan", {}).get("sha256") == EVALUATOR_PLAN, "result evaluator source drift")
    require(result.get("terminal_set_digest") == TERMINAL_SET, "result terminal set drift")
    require(result.get("official_evaluations_authorized") == EXPECTED_EVALUATIONS and result.get("official_evaluations_started") == EXPECTED_EVALUATIONS, "result evaluator count drift")
    episodes = result.get("episodes")
    require(isinstance(episodes, list) and len(episodes) == EXPECTED_EVALUATIONS, "selection result episode set incomplete")
    result_episodes = {(episode.get("case_id"), episode.get("arm")): episode for episode in episodes}
    require(set(result_episodes) == set(evaluation_outcomes), "selection result episode identities drift")
    totals: dict[str, dict[str, float | int]] = {
        "A": {"resolved": 0, "regressions": 0, "tokens": 0, "wall": 0.0},
        "T": {"resolved": 0, "regressions": 0, "tokens": 0, "wall": 0.0},
    }
    for key, outcome in evaluation_outcomes.items():
        case_id, arm = key
        episode = result_episodes[key]
        require(episode.get("outcome_valid") is True and episode.get("policy_verifier_clean") is True, "invalid episode entered selection")
        require(episode.get("official_evaluator_invocations") == 1, "selection episode evaluator count drift")
        require(episode.get("evaluation_receipt", {}).get("sha256") == evaluation_digests[f"{case_id}:{arm}"], "selection episode evaluation digest drift")
        require(episode.get("resolved") == outcome.get("resolved"), f"published resolution drift: {key}")
        require(episode.get("required_passed") == outcome.get("required_passed"), f"published required-pass count drift: {key}")
        require(episode.get("required_total") == outcome.get("required_total"), f"published required-total count drift: {key}")
        require(episode.get("pass_to_pass_regressions") == outcome.get("pass_to_pass_regressions"), f"published regressions drift: {key}")
        require(episode.get("pass_to_pass_total") == outcome.get("pass_to_pass_total"), f"published pass-to-pass total drift: {key}")
        require(episode.get("provider_total_tokens") == episode_evidence[key]["tokens"], f"published token usage drift: {key}")
        require(episode.get("combined_pre_evaluator_wall_seconds") == episode_evidence[key]["wall"], f"published wall usage drift: {key}")
        totals[arm]["resolved"] += int(outcome.get("resolved") is True)
        totals[arm]["regressions"] += int(outcome["pass_to_pass_regressions"])
        totals[arm]["tokens"] += int(episode_evidence[key]["tokens"])
        totals[arm]["wall"] += float(episode_evidence[key]["wall"])
    require(totals["A"]["resolved"] == totals["T"]["resolved"] == 2, "resolution aggregate drift")
    require(totals["A"]["regressions"] == totals["T"]["regressions"] == 0, "regression aggregate drift")
    require(totals["A"]["tokens"] == 9_394_654 and totals["T"]["tokens"] == 4_367_033, "token aggregate drift")
    require(100 * totals["T"]["tokens"] <= 85 * totals["A"]["tokens"], "registered token threshold no longer passes")
    require(100 * totals["T"]["wall"] <= 80 * totals["A"]["wall"], "registered wall threshold no longer passes")

    correction_path = root / "final/superseding-selection-correction.json"
    correction, correction_bytes = load(correction_path)
    require(sha_bytes(correction_bytes) == SELECTION_CORRECTION, "superseding selection correction drift")
    require(
        correction.get("schema_version") == "issue825-superseding-selection-correction-v1",
        "selection correction schema drift",
    )
    require(
        correction.get("supersedes") == {"selection_result_sha256": SELECTION_RESULT},
        "selection correction does not bind the historical selected-T result",
    )
    require(
        correction.get("amended_registration_sha256") == AMENDED_REGISTRATION
        and correction.get("amended_selection_sha256") == AMENDED_SELECTION,
        "selection correction protocol binding drift",
    )
    require(
        correction.get("policy_source")
        == {
            "forbidden_attempt_policy_utf8": FORBIDDEN_ATTEMPT_POLICY.decode(),
            "system_suffix_sha256": "4e5855ad0dbd582c56a997cba07f3de962427760c97c0e9b980140f5e43b1ffd",
        },
        "selection correction policy source drift",
    )
    require(
        correction.get("treatment_noncompliance")
        == {
            "actor_tool_ledger_sha256": XARRAY_T_LEDGER,
            "arm": "T",
            "case_id": XARRAY,
            "violations": proven_violations,
        },
        "selection correction violation evidence drift",
    )

    treatment_policy_compliance = {
        XARRAY: len(proven_violations) == 0,
        DJANGO: True,
    }
    registered_selection_prerequisite_satisfied = all(treatment_policy_compliance.values())
    corrected_decision = (
        "selected_T" if registered_selection_prerequisite_satisfied else "no_RNA_treatment"
    )
    corrected_classification = "treatment_noncompliance"
    corrected_reason = "at least one T episode failed the mandatory RNA-first manipulation contract"
    require(
        correction.get("corrected_result")
        == {
            "classification": corrected_classification,
            "decision": corrected_decision,
            "reason": corrected_reason,
            "registered_selection_prerequisite_satisfied": registered_selection_prerequisite_satisfied,
            "selection_authoritative": True,
            "xarray_T_policy_compliant": False,
        },
        "corrected registered decision drift",
    )
    require(
        correction.get("historical_outcomes")
        == {
            "aggregate_verification_required": True,
            "erroneous_post_nonadherence_evaluator": {
                "arm": "T",
                "case_id": XARRAY,
                "official_evaluator_invocations": 1,
                "registered_required_invocations": 0,
            },
            "official_evaluations_recorded": EXPECTED_EVALUATIONS,
            "selection_use": "none_after_treatment_noncompliance",
        },
        "selection correction historical-outcome classification drift",
    )
    require(
        evaluation_outcomes[(XARRAY, "T")].get("resolved") is True,
        "retained post-nonadherence xarray T evaluator outcome was not verified",
    )

    return {
        "schema_version": "issue825-published-result-verification-v2",
        "valid": True,
        "protocol_classification": "amended_development_selector",
        "decision": corrected_decision,
        "decision_classification": corrected_classification,
        "decision_reason": corrected_reason,
        "checks": {
            "original_xarray_symmetric_wall_timeout": True,
            "retained_harness_evaluator_invocations_before_amendment": 0,
            "fresh_amended_xarray_sessions": True,
            "retained_django_pair_unchanged": True,
            "official_evaluations_once": 4,
            "model_feedback_delivered": False,
            "selection_recomputed": True,
            "historical_selected_T_superseded": True,
            "xarray_T_policy_compliant": False,
            "xarray_T_forbidden_attempts_proven": len(proven_violations),
            "erroneous_post_nonadherence_evaluator_invocations": 1,
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
            "superseding_selection_correction_sha256": SELECTION_CORRECTION,
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
        print(json.dumps({"schema_version": "issue825-published-result-verification-v2", "valid": False, "error": str(exc)}, sort_keys=True))
        return 1
    print(canonical(result).decode(), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
