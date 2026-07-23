#!/usr/bin/env python3
"""Fail-closed verifier for the issue #830 successor registration."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
import sys
from typing import Any, Mapping


HERE = Path(__file__).resolve().parent
HARNESS = HERE.parent / "issue827"
HEX40 = re.compile(r"[0-9a-f]{40}")
EXPECTED_ARTIFACT = {
    "producer_commit": "13c74539441adeef1ffd7d68b413ff148203f21c",
    "launcher_sha256": "5feb634b639d1a4650d45e0e04a72e007fbf05aaf2ed172391dcb9f806a76530",
    "binary_sha256": "d4d264da1a012b38814f0f2e9ee92f77c5aab3ed558a0f23abcd830d4b78ca94",
    "bundle_manifest_sha256": "c7d1d594fef2fb9103f52b3f358b6e11d8368cc7fdcfe4f96b997b57ce5723bd",
    "archive_sha256": "f2e40a59a849f57a792a97ddcb575cbd7629e6b24e9cf017898b2925dd886e33",
    "upload_attestation_sha256": "28ddea0b7fd3bb0bb295f7a368ccf3dbbd81a9e158c5a6d8fea2fd5512eade5c",
    "verification_receipt_sha256": "215509e3c99b1033469f3ed2db7f498746b291a20c44fa2f93c6dad897971885",
    "canonical_environment_sha256": "39baf2645a6d1bf502ea19cb95ef059df4192fdcadb7c6e7cfea60f54a938868",
    "runtime_receipt_sha256": "d9f2c5849c00a8f145c86522ddf2dbea6e1ed33f1f8637c29e837ba112e50198",
    "local_source_build_allowed": False,
}
ORIGINAL_SELECTION_SHA256 = (
    "9d2b75d9af8787882fadabb792cd3018686caade7af004f6aae80aa4dee82fbd"
)
ORIGINAL_SELECTION_DIGEST = (
    "991fda825b69d7dc023df3c8605c7b5064abb7146b486ca57a7a0f35dc8769b2"
)


class SuccessorVerificationError(RuntimeError):
    pass


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SuccessorVerificationError(message)


def regular_bytes(path: Path, label: str) -> bytes:
    path = path.resolve(strict=True)
    metadata = path.stat()
    require(stat.S_ISREG(metadata.st_mode), f"{label} is not a regular file")
    require(not path.is_symlink(), f"{label} is a symlink")
    return path.read_bytes()


def load_object(path: Path, label: str) -> tuple[dict[str, Any], bytes]:
    payload = regular_bytes(path, label)
    try:
        value = json.loads(payload)
    except json.JSONDecodeError as exc:
        raise SuccessorVerificationError(f"{label} is invalid JSON: {exc}") from exc
    require(isinstance(value, dict), f"{label} is not an object")
    return value, payload


def sha_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_registration(
    registration_path: Path,
    lineage_path: Path,
    closure_path: Path,
    qualification_archive: Path | None,
) -> dict[str, Any]:
    sys.path.insert(0, str(HARNESS))
    import registration_contract  # type: ignore

    registration, registration_bytes = load_object(
        registration_path, "successor registration"
    )
    lineage, lineage_bytes = load_object(lineage_path, "successor lineage")
    closure, closure_bytes = load_object(closure_path, "qualification closure")

    try:
        registration_contract.validate_registration(
            registration,
            source_root=HARNESS,
        )
    except registration_contract.RegistrationContractError as exc:
        raise SuccessorVerificationError(str(exc)) from exc

    require(registration.get("issue") == 827, "runner compatibility issue drift")
    require(
        registration.get("classification")
        == "post_selection_pre_outcome_artifact_successor",
        "successor classification drift",
    )
    require(
        registration.get("fresh_case_claim") is False,
        "successor incorrectly claims a new fresh selection",
    )
    require(
        registration.get("published_before_selection") is False,
        "successor incorrectly claims publication before original selection",
    )
    require(registration.get("successor") == lineage, "embedded lineage differs")
    require(lineage.get("issue") == 830, "lineage issue drift")
    require(lineage.get("supersedes_issue") == 827, "lineage predecessor drift")
    require(
        lineage.get("cases_carried_forward_without_rerandomization") is True,
        "lineage permits rerandomization",
    )
    require(
        lineage.get("prior_activity")
        == {
            "credentials_accessed": False,
            "model_calls": 0,
            "official_evaluator_invocations": 0,
            "provider_requests": 0,
        },
        "lineage imports prior activity",
    )
    require(
        lineage.get("runtime_authorization")
        == {
            "episode_budget_usd": 6.0,
            "episode_count": 4,
            "maximum_budget_usd": 24.0,
            "model_retry_allowed": False,
            "resume_allowed": False,
            "wall_seconds": 1200,
        },
        "successor runtime authorization drift",
    )
    require(registration.get("rna_artifact") == EXPECTED_ARTIFACT, "artifact drift")
    require(registration.get("prior_model_calls") == 0, "prior model activity")
    require(
        registration.get("prior_official_evaluator_invocations") == 0,
        "prior evaluator activity",
    )
    require(
        registration["model_runtime"]["budget_usd"] == 6.0
        and registration["model_runtime"]["wall_seconds"] == 1200
        and registration["model_runtime"]["invocations_per_episode"] == 1
        and registration["model_runtime"]["model_retry_allowed"] is False
        and registration["model_runtime"]["resume_allowed"] is False,
        "model runtime drift",
    )

    registered_closure = registration["qualification_closure"]
    require(
        sha_bytes(closure_bytes) == registered_closure["manifest_sha256"],
        "qualification manifest hash drift",
    )
    archive_sha = registered_closure["archive_sha256"]
    if qualification_archive is not None:
        archive_path = qualification_archive.resolve(strict=True)
        require(
            sha_file(archive_path) == archive_sha,
            "qualification archive hash drift",
        )
    try:
        registration_contract.validate_qualification_manifest(
            registration,
            closure_bytes,
            archive_sha,
        )
    except registration_contract.RegistrationContractError as exc:
        raise SuccessorVerificationError(str(exc)) from exc

    return {
        "schema_version": "issue830-successor-registration-verification-v1",
        "verified": True,
        "registration_sha256": sha_bytes(registration_bytes),
        "successor_lineage_sha256": sha_bytes(lineage_bytes),
        "qualification_manifest_sha256": sha_bytes(closure_bytes),
        "qualification_archive_sha256": archive_sha,
        "model_calls": 0,
        "provider_requests": 0,
        "official_evaluator_invocations": 0,
        "credentials_accessed": False,
    }


def verify_selection(
    selection_path: Path,
    registration_path: Path,
    lineage_path: Path,
) -> dict[str, Any]:
    selection, selection_bytes = load_object(selection_path, "successor selection")
    original, original_bytes = load_object(
        HARNESS / "selection.json", "original selection"
    )
    registration_bytes = regular_bytes(registration_path, "successor registration")
    lineage, _ = load_object(lineage_path, "successor lineage")

    require(
        sha_bytes(original_bytes) == ORIGINAL_SELECTION_SHA256,
        "original selection identity drift",
    )
    require(
        original.get("digest") == ORIGINAL_SELECTION_DIGEST,
        "original selection digest drift",
    )
    require(selection.get("authoritative") is True, "selection not authoritative")
    require(
        selection.get("state") == "selected_pre_model",
        "selection is not pre-model",
    )
    require(selection.get("prior_model_calls") == 0, "selection imports model calls")
    require(
        selection.get("registration_sha256") == sha_bytes(registration_bytes),
        "selection registration hash drift",
    )
    registration_commit = selection.get("registration_commit")
    require(
        isinstance(registration_commit, str)
        and HEX40.fullmatch(registration_commit) is not None,
        "selection registration commit invalid",
    )
    require(
        selection.get("successor")
        == {
            "issue": 830,
            "registration_commit": registration_commit,
            "original_selection_sha256": ORIGINAL_SELECTION_SHA256,
            "original_selection_digest": ORIGINAL_SELECTION_DIGEST,
            "cases_carried_forward_without_rerandomization": True,
            "fresh_case_claim_scope": "original_issue827_selection_event_only",
        },
        "selection successor lineage drift",
    )

    normalized = json.loads(canonical(selection))
    normalized_original = json.loads(canonical(original))
    for key in ("digest", "registration_commit", "registration_sha256"):
        normalized.pop(key, None)
        normalized_original.pop(key, None)
    normalized.pop("successor", None)
    for case in normalized["cases"]:
        case.pop("cache_preparation", None)
    for case in normalized_original["cases"]:
        case.pop("cache_preparation", None)
    require(
        normalized == normalized_original,
        "selection changed fields beyond successor bindings/cache wording",
    )

    expected_cases = [
        {
            key: case[key]
            for key in ("instance_id", "base_commit", "base_tree", "arm_order")
        }
        for case in lineage["cases"]
    ]
    observed_cases = [
        {
            key: case[key]
            for key in ("instance_id", "base_commit", "base_tree", "arm_order")
        }
        for case in selection["cases"]
    ]
    require(observed_cases == expected_cases, "case identities or order drift")
    require(
        selection["cases"][0]["cache_preparation"]
        == "reuse verifier-clean exact-producer issue #829 combined cache; verify, inject into a fresh exact-tree checkout, and fresh-reopen READY without scan, LSP, or encoding",
        "SymPy cache preparation drift",
    )
    require(
        selection["cases"][1]["cache_preparation"]
        == "cold exact-tree preparation with the registered exact-CI artifact; fresh-process readiness and strict hybrid/Metal query verification",
        "Django cache preparation drift",
    )

    digest_body = dict(selection)
    observed_digest = digest_body.pop("digest", None)
    require(
        observed_digest == sha_bytes(canonical(digest_body)),
        "selection canonical digest drift",
    )
    return {
        "schema_version": "issue830-successor-selection-verification-v1",
        "verified": True,
        "selection_sha256": sha_bytes(selection_bytes),
        "selection_digest": observed_digest,
        "registration_commit": registration_commit,
        "case_order": [
            {
                "instance_id": case["instance_id"],
                "arm_order": case["arm_order"],
            }
            for case in selection["cases"]
        ],
        "model_calls": 0,
        "provider_requests": 0,
        "official_evaluator_invocations": 0,
        "credentials_accessed": False,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    result.add_argument(
        "--registration",
        type=Path,
        default=HERE / "registration.json",
    )
    result.add_argument(
        "--lineage",
        type=Path,
        default=HERE / "successor-lineage.json",
    )
    result.add_argument(
        "--qualification-manifest",
        type=Path,
        default=HERE / "qualification-closure.manifest.json",
    )
    result.add_argument("--qualification-archive", type=Path)
    result.add_argument("--selection", type=Path)
    return result


def main() -> int:
    args = parser().parse_args()
    receipt = {
        "registration": verify_registration(
            args.registration,
            args.lineage,
            args.qualification_manifest,
            args.qualification_archive,
        )
    }
    if args.selection is not None:
        receipt["selection"] = verify_selection(
            args.selection,
            args.registration,
            args.lineage,
        )
    print(json.dumps(receipt, sort_keys=True, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (SuccessorVerificationError, OSError) as exc:
        print(f"FAIL CLOSED: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
