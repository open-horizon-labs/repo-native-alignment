#!/usr/bin/env python3
"""Deterministically select the registered case prefix without exposing text.

The selector can first emit a non-authoritative rank receipt so the selected
repositories can be acquired. A final receipt additionally requires an exact
commit-to-tree binding for every selected case. Historical issue #827 pair
registrations and current issue #836 cohort registrations share this path.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
from typing import Any, Mapping, NamedTuple

import registration_contract

LEGACY_SCHEMA = "issue827-fresh-pair-selection-v1"
ISSUE836_V2_SCHEMA = "issue836-fresh-cohort-selection-v2"
ISSUE836_V3_SCHEMA = "issue836-fresh-cohort-selection-v3"
CURRENT_SCHEMA = "issue836-fresh-cohort-selection-v4"
SCHEMA = CURRENT_SCHEMA
LEGACY_ALGORITHM_VERSION = "issue827-selector-v1"
ISSUE836_V2_ALGORITHM_VERSION = "issue836-selector-v2"
ISSUE836_V3_ALGORITHM_VERSION = "issue836-selector-v3"
CURRENT_ALGORITHM_VERSION = "issue836-selector-v4"
EXPECTED_ROWS = 500
EXPECTED_SEED = "rna-first-sonnet-hermetic-selector-v1"
EXPECTED_RANKING = "ascending SHA256(seed_utf8 || 0x00 || instance_id_utf8)"
REGISTRATION_PATH = "benchmark/swebench-rna-first/issue827/registration.json"
ISSUE836_V2_REGISTRATION_PATH = (
    "benchmark/swebench-rna-first/issue836/registration.json"
)
ISSUE836_V3_REGISTRATION_PATH = (
    "benchmark/swebench-rna-first/issue836-v3/registration.json"
)
ISSUE836_V4_REGISTRATION_PATH = (
    "benchmark/swebench-rna-first/issue836-v4/registration.json"
)
CURRENT_REGISTRATION_PATH = ISSUE836_V4_REGISTRATION_PATH
ALLOWED_REGISTRATION_PATHS = {
    REGISTRATION_PATH,
    ISSUE836_V2_REGISTRATION_PATH,
    ISSUE836_V3_REGISTRATION_PATH,
    ISSUE836_V4_REGISTRATION_PATH,
}
REGISTRATION_PATH_BY_SCHEMA = {
    registration_contract.LEGACY_REGISTRATION_SCHEMA: REGISTRATION_PATH,
    registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA: (
        ISSUE836_V2_REGISTRATION_PATH
    ),
    registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA: (
        ISSUE836_V3_REGISTRATION_PATH
    ),
    registration_contract.CURRENT_REGISTRATION_SCHEMA: (
        ISSUE836_V4_REGISTRATION_PATH
    ),
}
SELECTION_PATH = "benchmark/swebench-rna-first/issue827/selection.json"
ISSUE836_V2_SELECTION_PATH = (
    "benchmark/swebench-rna-first/issue836/selection.json"
)
ISSUE836_V3_SELECTION_PATH = (
    "benchmark/swebench-rna-first/issue836-v3/selection.json"
)
ISSUE836_V4_SELECTION_PATH = (
    "benchmark/swebench-rna-first/issue836-v4/selection.json"
)
CURRENT_SELECTION_PATH = ISSUE836_V4_SELECTION_PATH
SELECTION_PATH_BY_SCHEMA = {
    registration_contract.LEGACY_REGISTRATION_SCHEMA: SELECTION_PATH,
    registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA: (
        ISSUE836_V2_SELECTION_PATH
    ),
    registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA: (
        ISSUE836_V3_SELECTION_PATH
    ),
    registration_contract.CURRENT_REGISTRATION_SCHEMA: (
        ISSUE836_V4_SELECTION_PATH
    ),
}
EXCLUSIONS_PATH = "benchmark/swebench-rna-first/issue827/exclusions.json"
SELECTOR_PATH = "benchmark/swebench-rna-first/issue827/select_cases.py"
FROZEN_V2_REGISTRATION_SHA256 = (
    "10345f1ba1b1638f04b6b671a3aa64f5847e17944d955cd08494f91f003275b0"
)
FROZEN_V2_SELECTION_SHA256 = (
    "8b1c0dbfac7a540668f526a656f1a230af497b963b56793f44039686d147b73b"
)
FROZEN_V2_ARTIFACT_ROOT = Path(__file__).resolve().parents[1] / "issue836"
FROZEN_V2_REGISTRATION_FILE = FROZEN_V2_ARTIFACT_ROOT / "registration.json"
FROZEN_V2_SELECTION_FILE = FROZEN_V2_ARTIFACT_ROOT / "selection.json"
FROZEN_V3_REGISTRATION_SHA256 = (
    "6f319f138336aef194cd91962edb75f3db172816cab7e19344d20d39217c5e92"
)
FROZEN_V3_SELECTION_SHA256 = (
    "dfd7ce6f4fcdd6e9b7baf81eb3faca76f32cde86485f3876d6d139154cedac80"
)
FROZEN_V3_ARTIFACT_ROOT = Path(__file__).resolve().parents[1] / "issue836-v3"
FROZEN_V3_REGISTRATION_FILE = FROZEN_V3_ARTIFACT_ROOT / "registration.json"
FROZEN_V3_SELECTION_FILE = FROZEN_V3_ARTIFACT_ROOT / "selection.json"

V3_SELECTION_KEYS = {
    "authoritative",
    "case_replacement_after_model_start",
    "cases",
    "dataset_arrow_sha256",
    "digest",
    "eligible_rows",
    "excluded_ids_sha256",
    "excluded_rows",
    "exclusions_sha256",
    "fresh_case_claim",
    "gold_or_outcomes_inspected_before_selection",
    "model_calls_authorized_before_cache_readiness",
    "population_rows",
    "pre_model_v2_supersession",
    "prefix_lineage",
    "prior_model_calls",
    "problem_statements_inspected_by_human_before_selection",
    "registration_commit",
    "registration_sha256",
    "schema_version",
    "seed",
    "state",
}
V4_SELECTION_KEYS = (
    V3_SELECTION_KEYS
    - {"pre_model_v2_supersession"}
    | {
        "pre_model_v3_supersession",
        "prior_official_evaluator_invocations",
    }
)


class SelectionError(RuntimeError):
    pass


class ExpectedEpisodeIdentity(NamedTuple):
    """An exact one-shot episode required by registration and selection."""

    rank: int
    instance_id: str
    arm: str


def selection_schema(registration: Mapping[str, Any]) -> str:
    """Return the only selection schema valid for a registration version."""

    schema = registration.get("schema_version")
    if schema == registration_contract.LEGACY_REGISTRATION_SCHEMA:
        return LEGACY_SCHEMA
    if schema == registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA:
        return ISSUE836_V2_SCHEMA
    if schema == registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA:
        return ISSUE836_V3_SCHEMA
    if schema == registration_contract.CURRENT_REGISTRATION_SCHEMA:
        return CURRENT_SCHEMA
    raise SelectionError("registration schema has no selection schema")


def selection_publication_path(registration: Mapping[str, Any]) -> str:
    """Return the only selection publication path for a registration."""

    path = SELECTION_PATH_BY_SCHEMA.get(registration.get("schema_version"))
    if path is None:
        raise SelectionError("registration schema has no selection path")
    return path


def require_selection_registration_binding(
    registration: Mapping[str, Any],
    selection: Mapping[str, Any],
    *,
    repository: Path | None = None,
    registration_repo_path: str | None = None,
) -> None:
    """Prove selection registration bytes at the exact versioned Git path."""

    schema = registration.get("schema_version")
    expected_path = REGISTRATION_PATH_BY_SCHEMA.get(schema)
    if expected_path is None:
        raise SelectionError("registration schema has no publication path")
    registered_path = registered_registration_repo_path(
        registration_repo_path or expected_path,
        schema,
    )
    checkout = Path(repository or Path(__file__).resolve().parents[3])
    if checkout.is_symlink() or not (checkout / ".git").is_dir():
        raise SelectionError(
            "selection registration repository must be a regular Git checkout"
        )
    commit = require_hex(
        selection.get("registration_commit"),
        40,
        "selection registration commit",
    )
    committed = subprocess.run(
        ["git", "-C", str(checkout), "show", f"{commit}:{registered_path}"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if committed.returncode != 0:
        raise SelectionError(
            "selection registration is absent at its exact Git commit/path"
        )
    if (
        sha256_bytes(committed.stdout)
        != selection.get("registration_sha256")
    ):
        raise SelectionError(
            "selection registration digest differs from committed bytes"
        )
    try:
        committed_registration = json.loads(committed.stdout)
    except json.JSONDecodeError as error:
        raise SelectionError(
            f"committed selection registration is invalid: {error}"
        ) from error
    if committed_registration != registration:
        raise SelectionError(
            "selection registration object differs from committed bytes"
        )


def load_frozen_v2_artifacts(
    registration_path: Path | None = None,
    selection_path: Path | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Load the exact immutable v2 artifacts that define the v3 base cohort."""

    paths = (
        (
            Path(registration_path or FROZEN_V2_REGISTRATION_FILE),
            FROZEN_V2_REGISTRATION_SHA256,
            "v2 registration",
        ),
        (
            Path(selection_path or FROZEN_V2_SELECTION_FILE),
            FROZEN_V2_SELECTION_SHA256,
            "v2 selection",
        ),
    )
    documents: list[dict[str, Any]] = []
    for path, expected_sha256, label in paths:
        if path.is_symlink() or not path.is_file():
            raise SelectionError(f"frozen {label} artifact is absent or symlinked")
        try:
            data = path.read_bytes()
        except OSError as error:
            raise SelectionError(f"cannot read frozen {label} artifact: {error}") from error
        if sha256_bytes(data) != expected_sha256:
            raise SelectionError(f"frozen {label} artifact bytes changed")
        try:
            value = json.loads(data)
        except json.JSONDecodeError as error:
            raise SelectionError(f"frozen {label} artifact is invalid: {error}") from error
        if not isinstance(value, dict):
            raise SelectionError(f"frozen {label} artifact must be an object")
        documents.append(value)

    v2_registration, v2_selection = documents
    try:
        registration_contract.validate_registration(v2_registration)
    except registration_contract.RegistrationContractError as error:
        raise SelectionError(f"frozen v2 registration is invalid: {error}") from error

    supersession = registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
    expected_provenance = {
        "schema_version": supersession["superseded_selection_schema"],
        "registration_sha256": supersession["superseded_registration_sha256"],
        "registration_commit": supersession["superseded_registration_commit"],
        "digest": supersession["superseded_selection_digest"],
        "dataset_arrow_sha256": supersession["dataset_arrow_sha256"],
        "exclusions_sha256": supersession["exclusions_sha256"],
        "excluded_ids_sha256": supersession["excluded_ids_sha256"],
    }
    for key, expected in expected_provenance.items():
        if v2_selection.get(key) != expected:
            raise SelectionError(f"frozen v2 selection provenance drift: {key}")
    digest_payload = dict(v2_selection)
    digest = digest_payload.pop("digest")
    if digest != sha256_bytes(canonical(digest_payload)):
        raise SelectionError("frozen v2 selection digest mismatch")
    if (
        v2_registration.get("dataset", {}).get("arrow_sha256")
        != supersession["dataset_arrow_sha256"]
        or v2_registration.get("selector", {}).get("exclusions_file_sha256")
        != supersession["exclusions_sha256"]
        or v2_registration.get("selector", {}).get("excluded_ids_sha256")
        != supersession["excluded_ids_sha256"]
    ):
        raise SelectionError("frozen v2 registration provenance drift")
    return v2_registration, v2_selection


def expected_v3_cases(
    v2_selection: Mapping[str, Any],
    *,
    authoritative: bool,
) -> list[dict[str, Any]]:
    """Derive v3 by preserving every v2 case except the registered rank 8."""

    v2_cases = v2_selection.get("cases")
    if not isinstance(v2_cases, list) or any(
        not isinstance(case, dict) for case in v2_cases
    ):
        raise SelectionError("frozen v2 selection cases are malformed")
    expected = [dict(case) for case in v2_cases]
    supersession = registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
    replacement_index = supersession["superseded_rank"] - 1
    replacement = dict(expected[replacement_index])
    replacement.update(
        {
            "instance_id": supersession["replacement_instance_id"],
            "repo": supersession["replacement_repo"],
            "base_commit": supersession["replacement_base_commit"],
            "base_tree": supersession["replacement_base_tree"],
            "ranking_sha256": supersession["replacement_ranking_sha256"],
            "problem_statement_sha256": supersession[
                "replacement_problem_statement_sha256"
            ],
            "arm_order": supersession["preserved_arm_order"],
        }
    )
    expected[replacement_index] = replacement
    if not authoritative:
        expected = [
            {
                key: value
                for key, value in case.items()
                if key not in {"base_tree", "cache_preparation"}
            }
            for case in expected
        ]
    return expected


def validate_v3_selection(
    registration: Mapping[str, Any],
    selection: Mapping[str, Any],
    v2_selection: Mapping[str, Any],
) -> list[dict[str, Any]]:
    """Fail closed unless v3 has exact lineage, provenance, and case bytes."""

    if not isinstance(selection, dict) or set(selection) != V3_SELECTION_KEYS:
        raise SelectionError("v3 selection keys differ from the frozen contract")
    state = selection.get("state")
    authoritative = selection.get("authoritative")
    if (state, authoritative) not in {
        ("ranked_needs_tree_binding", False),
        ("selected_pre_model", True),
    }:
        raise SelectionError("v3 selection state/authority mismatch")

    supersession = registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
    selector = registration.get("selector")
    if (
        not isinstance(selector, dict)
        or selector.get("pre_model_v2_supersession") != supersession
        or selection.get("pre_model_v2_supersession") != supersession
    ):
        raise SelectionError("selection does not bind the registered v2 supersession")
    if selection.get("prefix_lineage") != registration_contract.FROZEN_V3_PREFIX_LINEAGE:
        raise SelectionError("selection v3 prefix lineage drift")

    inherited_fields = {
        "dataset_arrow_sha256",
        "eligible_rows",
        "excluded_ids_sha256",
        "excluded_rows",
        "exclusions_sha256",
        "fresh_case_claim",
        "gold_or_outcomes_inspected_before_selection",
        "model_calls_authorized_before_cache_readiness",
        "population_rows",
        "prior_model_calls",
        "problem_statements_inspected_by_human_before_selection",
        "seed",
    }
    for key in inherited_fields:
        if selection.get(key) != v2_selection.get(key):
            raise SelectionError(f"v3 selection provenance drift: {key}")
    if selection.get("case_replacement_after_model_start") is not False:
        raise SelectionError("v3 cohort replacement was not completed before model start")
    if (
        selection.get("dataset_arrow_sha256")
        != registration.get("dataset", {}).get("arrow_sha256")
        or selection.get("exclusions_sha256")
        != selector.get("exclusions_file_sha256")
        or selection.get("excluded_ids_sha256")
        != selector.get("excluded_ids_sha256")
    ):
        raise SelectionError("v3 selection differs from registered dataset/exclusions")

    registration_commit = require_hex(
        selection.get("registration_commit"), 40, "v3 registration commit"
    )
    registration_sha256 = require_hex(
        selection.get("registration_sha256"), 64, "v3 registration SHA-256"
    )
    if registration_commit == supersession["superseded_registration_commit"]:
        raise SelectionError("v3 selection reuses the superseded registration commit")
    if registration_sha256 == supersession["superseded_registration_sha256"]:
        raise SelectionError("v3 selection reuses the superseded registration bytes")

    digest = require_hex(selection.get("digest"), 64, "v3 selection digest")
    digest_payload = dict(selection)
    digest_payload.pop("digest")
    if digest != sha256_bytes(canonical(digest_payload)):
        raise SelectionError("v3 selection digest mismatch")
    return expected_v3_cases(v2_selection, authoritative=authoritative)


def require_committed_artifact(
    repository: Path,
    commit: Any,
    repo_path: str,
    artifact_bytes: bytes,
    label: str,
) -> None:
    """Require exact artifact bytes at an exact ordinary-clone commit/path."""

    checkout = Path(repository)
    if checkout.is_symlink() or not (checkout / ".git").is_dir():
        raise SelectionError(
            f"{label} repository must be a regular Git checkout"
        )
    commit_id = require_hex(commit, 40, f"{label} commit")
    normalized = normalized_repo_relative_path(repo_path, f"{label} path")
    result = subprocess.run(
        ["git", "-C", str(checkout), "show", f"{commit_id}:{normalized}"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0 or result.stdout != artifact_bytes:
        raise SelectionError(
            f"{label} bytes are absent or changed at exact Git commit/path"
        )


def load_frozen_v3_artifacts(
    registration_path: Path | None = None,
    selection_path: Path | None = None,
    *,
    repository: Path | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Load and independently prove the committed v3 base cohort."""

    paths = (
        (
            Path(registration_path or FROZEN_V3_REGISTRATION_FILE),
            FROZEN_V3_REGISTRATION_SHA256,
            "v3 registration",
        ),
        (
            Path(selection_path or FROZEN_V3_SELECTION_FILE),
            FROZEN_V3_SELECTION_SHA256,
            "v3 selection",
        ),
    )
    documents: list[dict[str, Any]] = []
    payloads: list[bytes] = []
    for path, expected_sha256, label in paths:
        if path.is_symlink() or not path.is_file():
            raise SelectionError(f"frozen {label} artifact is absent or symlinked")
        try:
            data = path.read_bytes()
        except OSError as error:
            raise SelectionError(
                f"cannot read frozen {label} artifact: {error}"
            ) from error
        if sha256_bytes(data) != expected_sha256:
            raise SelectionError(f"frozen {label} artifact bytes changed")
        try:
            value = json.loads(data)
        except json.JSONDecodeError as error:
            raise SelectionError(
                f"frozen {label} artifact is invalid: {error}"
            ) from error
        if not isinstance(value, dict):
            raise SelectionError(f"frozen {label} artifact must be an object")
        payloads.append(data)
        documents.append(value)

    v3_registration, v3_selection = documents
    try:
        registration_contract.validate_registration(v3_registration)
    except registration_contract.RegistrationContractError as error:
        raise SelectionError(f"frozen v3 registration is invalid: {error}") from error

    supersession = registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION
    expected_provenance = {
        "schema_version": supersession["superseded_selection_schema"],
        "registration_sha256": supersession["superseded_registration_sha256"],
        "registration_commit": supersession["superseded_registration_commit"],
        "digest": supersession["superseded_selection_digest"],
        "dataset_arrow_sha256": supersession["dataset_arrow_sha256"],
        "exclusions_sha256": supersession["exclusions_sha256"],
        "excluded_ids_sha256": supersession["excluded_ids_sha256"],
        "prior_model_calls": 0,
        "case_replacement_after_model_start": False,
    }
    for key, expected in expected_provenance.items():
        if v3_selection.get(key) != expected:
            raise SelectionError(f"frozen v3 selection provenance drift: {key}")
    if (
        v3_registration.get("schema_version")
        != supersession["superseded_registration_schema"]
        or v3_registration.get("dataset", {}).get("arrow_sha256")
        != supersession["dataset_arrow_sha256"]
        or v3_registration.get("selector", {}).get("exclusions_file_sha256")
        != supersession["exclusions_sha256"]
        or v3_registration.get("selector", {}).get("excluded_ids_sha256")
        != supersession["excluded_ids_sha256"]
    ):
        raise SelectionError("frozen v3 registration provenance drift")

    checkout = Path(repository or Path(__file__).resolve().parents[3])
    require_committed_artifact(
        checkout,
        supersession["superseded_registration_commit"],
        supersession["superseded_registration_path"],
        payloads[0],
        "frozen v3 registration",
    )
    require_committed_artifact(
        checkout,
        supersession["superseded_selection_commit"],
        supersession["superseded_selection_path"],
        payloads[1],
        "frozen v3 selection",
    )
    require_selection_registration_binding(
        v3_registration,
        v3_selection,
        repository=checkout,
        registration_repo_path=ISSUE836_V3_REGISTRATION_PATH,
    )
    _, v2_selection = load_frozen_v2_artifacts()
    expected_cases = validate_v3_selection(
        v3_registration,
        v3_selection,
        v2_selection,
    )
    if v3_selection.get("cases") != expected_cases:
        raise SelectionError(
            "frozen v3 cases differ from frozen v2 plus rank-8 replacement"
        )
    return v3_registration, v3_selection


def expected_v4_cases(
    v3_selection: Mapping[str, Any],
    *,
    authoritative: bool,
) -> list[dict[str, Any]]:
    """Derive v4 by preserving v3 except the registered rank-12 successor."""

    v3_cases = v3_selection.get("cases")
    if not isinstance(v3_cases, list) or any(
        not isinstance(case, dict) for case in v3_cases
    ):
        raise SelectionError("frozen v3 selection cases are malformed")
    expected = [dict(case) for case in v3_cases]
    supersession = registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION
    replacement_index = supersession["superseded_rank"] - 1
    excluded = expected[replacement_index]
    excluded_identity = {
        "instance_id": supersession["excluded_instance_id"],
        "repo": supersession["excluded_repo"],
        "base_commit": supersession["excluded_base_commit"],
        "base_tree": supersession["excluded_base_tree"],
        "ranking_sha256": supersession["excluded_ranking_sha256"],
        "problem_statement_sha256": supersession[
            "excluded_problem_statement_sha256"
        ],
        "arm_order": supersession["preserved_arm_order"],
    }
    for key, value in excluded_identity.items():
        if excluded.get(key) != value:
            raise SelectionError(f"frozen v3 rank-12 identity drift: {key}")
    replacement = dict(excluded)
    replacement.update(
        {
            "instance_id": supersession["replacement_instance_id"],
            "repo": supersession["replacement_repo"],
            "base_commit": supersession["replacement_base_commit"],
            "base_tree": supersession["replacement_base_tree"],
            "ranking_sha256": supersession["replacement_ranking_sha256"],
            "problem_statement_sha256": supersession[
                "replacement_problem_statement_sha256"
            ],
            "arm_order": supersession["preserved_arm_order"],
        }
    )
    expected[replacement_index] = replacement
    if not authoritative:
        expected = [
            {
                key: value
                for key, value in case.items()
                if key not in {"base_tree", "cache_preparation"}
            }
            for case in expected
        ]
    return expected


def validate_v4_selection(
    registration: Mapping[str, Any],
    selection: Mapping[str, Any],
    v3_selection: Mapping[str, Any],
) -> list[dict[str, Any]]:
    """Fail closed unless v4 is the exact zero-call successor of frozen v3."""

    if not isinstance(selection, dict) or set(selection) != V4_SELECTION_KEYS:
        raise SelectionError("v4 selection keys differ from the frozen contract")
    state = selection.get("state")
    authoritative = selection.get("authoritative")
    if (state, authoritative) not in {
        ("ranked_needs_tree_binding", False),
        ("selected_pre_model", True),
    }:
        raise SelectionError("v4 selection state/authority mismatch")

    supersession = registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION
    selector = registration.get("selector")
    if (
        not isinstance(selector, dict)
        or selector.get("pre_model_v3_supersession") != supersession
        or selection.get("pre_model_v3_supersession") != supersession
    ):
        raise SelectionError("selection does not bind the registered v3 supersession")
    if selection.get("prefix_lineage") != registration_contract.FROZEN_V4_PREFIX_LINEAGE:
        raise SelectionError("selection v4 prefix lineage drift")

    inherited_fields = {
        "dataset_arrow_sha256",
        "eligible_rows",
        "excluded_ids_sha256",
        "excluded_rows",
        "exclusions_sha256",
        "fresh_case_claim",
        "gold_or_outcomes_inspected_before_selection",
        "model_calls_authorized_before_cache_readiness",
        "population_rows",
        "problem_statements_inspected_by_human_before_selection",
        "seed",
    }
    for key in inherited_fields:
        if selection.get(key) != v3_selection.get(key):
            raise SelectionError(f"v4 selection provenance drift: {key}")
    if (
        selection.get("prior_model_calls") != 0
        or selection.get("prior_official_evaluator_invocations") != 0
        or supersession["prior_model_calls"] != 0
        or supersession["prior_official_evaluator_invocations"] != 0
    ):
        raise SelectionError("v4 selection is not a zero-call pre-model successor")
    if selection.get("case_replacement_after_model_start") is not False:
        raise SelectionError("v4 cohort replacement was not completed before model start")
    if (
        selection.get("dataset_arrow_sha256")
        != registration.get("dataset", {}).get("arrow_sha256")
        or selection.get("exclusions_sha256")
        != selector.get("exclusions_file_sha256")
        or selection.get("excluded_ids_sha256")
        != selector.get("excluded_ids_sha256")
    ):
        raise SelectionError("v4 selection differs from registered dataset/exclusions")

    registration_commit = require_hex(
        selection.get("registration_commit"), 40, "v4 registration commit"
    )
    registration_sha256 = require_hex(
        selection.get("registration_sha256"), 64, "v4 registration SHA-256"
    )
    if registration_commit == supersession["superseded_registration_commit"]:
        raise SelectionError("v4 selection reuses the superseded registration commit")
    if registration_sha256 == supersession["superseded_registration_sha256"]:
        raise SelectionError("v4 selection reuses the superseded registration bytes")

    digest = require_hex(selection.get("digest"), 64, "v4 selection digest")
    digest_payload = dict(selection)
    digest_payload.pop("digest")
    if digest != sha256_bytes(canonical(digest_payload)):
        raise SelectionError("v4 selection digest mismatch")
    return expected_v4_cases(v3_selection, authoritative=authoritative)


def expected_episode_identities(
    registration: Mapping[str, Any],
    selection: Mapping[str, Any],
    *,
    frozen_v2_registration_path: Path | None = None,
    frozen_v2_selection_path: Path | None = None,
    frozen_v3_registration_path: Path | None = None,
    frozen_v3_selection_path: Path | None = None,
    registration_repository: Path | None = None,
    registration_repo_path: str | None = None,
) -> tuple[ExpectedEpisodeIdentity, ...]:
    """Validate exact versioned selection identity and enumerate episodes."""

    try:
        dimensions = registration_contract.experiment_dimensions(registration)
    except registration_contract.RegistrationContractError as exc:
        raise SelectionError(f"registration dimensions invalid: {exc}") from exc
    if selection.get("schema_version") != selection_schema(registration):
        raise SelectionError("selection schema does not match registration version")
    expected_cases: list[dict[str, Any]] | None = None
    schema = registration.get("schema_version")
    if schema == registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA:
        require_selection_registration_binding(
            registration,
            selection,
            repository=registration_repository,
            registration_repo_path=registration_repo_path,
        )
        _, v2_selection = load_frozen_v2_artifacts(
            frozen_v2_registration_path,
            frozen_v2_selection_path,
        )
        expected_cases = validate_v3_selection(
            registration,
            selection,
            v2_selection,
        )
    elif schema == registration_contract.CURRENT_REGISTRATION_SCHEMA:
        require_selection_registration_binding(
            registration,
            selection,
            repository=registration_repository,
            registration_repo_path=registration_repo_path,
        )
        _, v3_selection = load_frozen_v3_artifacts(
            frozen_v3_registration_path,
            frozen_v3_selection_path,
            repository=registration_repository,
        )
        expected_cases = validate_v4_selection(
            registration,
            selection,
            v3_selection,
        )
    cases = selection.get("cases")
    if not isinstance(cases, list):
        raise SelectionError("selection cases must be a list")
    if len(cases) != dimensions["case_count"]:
        raise SelectionError("selection case count differs from registration")

    identities: list[ExpectedEpisodeIdentity] = []
    seen_instance_ids: set[str] = set()
    for expected_rank, case in enumerate(cases, start=1):
        if not isinstance(case, dict):
            raise SelectionError("selection case must be an object")
        if case.get("rank") != expected_rank:
            raise SelectionError("selection ranks must be the exact ranked prefix")
        instance_id = case.get("instance_id")
        if not isinstance(instance_id, str) or not instance_id:
            raise SelectionError("selection case instance ID is invalid")
        if instance_id in seen_instance_ids:
            raise SelectionError("selection case instance IDs must be unique")
        seen_instance_ids.add(instance_id)
        expected_arm_order = (
            ["A", "T"] if expected_rank % 2 == 1 else ["T", "A"]
        )
        if case.get("arm_order") != expected_arm_order:
            raise SelectionError("selection arm order does not match rank parity")
        identities.extend(
            ExpectedEpisodeIdentity(expected_rank, instance_id, arm)
            for arm in expected_arm_order
        )
    if expected_cases is not None and cases != expected_cases:
        version = "v4" if schema == registration_contract.CURRENT_REGISTRATION_SCHEMA else "v3"
        raise SelectionError(f"{version} cases differ from frozen successor contract")
    if len(identities) != dimensions["episode_count"]:
        raise SelectionError("selection episode count differs from registration")
    return tuple(identities)


def deterministic_ranked_prefix(
    instance_ids: list[str],
    excluded_instance_ids: set[str],
    seed: str,
    case_count: int,
) -> list[tuple[str, str]]:
    """Rank eligible IDs without consulting problem text, gold, or outcomes."""

    ranked = sorted(
        (
            sha256_bytes(
                seed.encode("utf-8")
                + b"\0"
                + instance_id.encode("utf-8")
            ),
            instance_id,
        )
        for instance_id in instance_ids
        if instance_id not in excluded_instance_ids
    )
    if len(ranked) < case_count:
        raise SelectionError("eligible population is smaller than registered cohort")
    return ranked[:case_count]


def registered_ranked_cohort(
    registration: Mapping[str, Any],
    eligible_ranked: list[tuple[str, str]],
) -> list[tuple[str, str]]:
    """Apply the versioned pre-model replacement to the ranked cohort."""

    try:
        dimensions = registration_contract.experiment_dimensions(registration)
    except registration_contract.RegistrationContractError as exc:
        raise SelectionError(f"registration dimensions invalid: {exc}") from exc
    case_count = dimensions["case_count"]
    if len(eligible_ranked) < case_count:
        raise SelectionError("eligible ranking is smaller than registered cohort")
    selected = list(eligible_ranked[:case_count])
    schema = registration.get("schema_version")
    if schema not in {
        registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA,
        registration_contract.CURRENT_REGISTRATION_SCHEMA,
    }:
        return selected

    def apply_replacement(
        supersession: Mapping[str, Any],
        label: str,
    ) -> None:
        superseded_index = supersession["superseded_rank"] - 1
        replacement_index = supersession["replacement_source_rank"] - 1
        if replacement_index >= len(eligible_ranked):
            raise SelectionError(
                f"registered {label} replacement source rank is unavailable"
            )
        if (
            selected[superseded_index][1]
            != supersession["excluded_instance_id"]
        ):
            raise SelectionError(f"registered {label} superseded identity drift")
        replacement = eligible_ranked[replacement_index]
        if replacement != (
            supersession["replacement_ranking_sha256"],
            supersession["replacement_instance_id"],
        ):
            raise SelectionError(f"registered {label} ranking identity drift")
        if replacement in selected:
            raise SelectionError(f"registered {label} replacement is duplicate")
        selected[superseded_index] = replacement

    selector = registration.get("selector", {})
    if schema == registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA:
        v3_supersession = selector.get("pre_model_v2_supersession")
        if (
            v3_supersession
            != registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
        ):
            raise SelectionError("registration v2 supersession identity drift")
        apply_replacement(v3_supersession, "v3")
    else:
        v4_supersession = selector.get("pre_model_v3_supersession")
        if (
            v4_supersession
            != registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION
        ):
            raise SelectionError("registration v3 supersession identity drift")
        apply_replacement(
            registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION,
            "v3 carry-forward",
        )
        apply_replacement(v4_supersession, "v4")
    if len({instance_id for _, instance_id in selected}) != case_count:
        raise SelectionError("registered replacements produce duplicate cases")
    return selected


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def load_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise SelectionError(f"invalid {label}: {error}") from error
    if not isinstance(value, dict):
        raise SelectionError(f"{label} must be an object")
    return value


def require_hex(value: Any, length: int, label: str) -> str:
    if not isinstance(value, str) or len(value) != length:
        raise SelectionError(f"{label} must be {length} lowercase hex characters")
    if any(character not in "0123456789abcdef" for character in value):
        raise SelectionError(f"{label} must be lowercase hexadecimal")
    return value


def normalized_repo_relative_path(value: str, label: str) -> str:
    """Return an exact normalized Git tree path beneath the repository root."""

    if not isinstance(value, str) or not value or "\\" in value:
        raise SelectionError(f"{label} must be a normalized relative POSIX path")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or path.as_posix() != value
        or any(part in {"", ".", ".."} for part in path.parts)
    ):
        raise SelectionError(f"{label} must be a normalized relative POSIX path")
    return value


def registered_registration_repo_path(
    value: str,
    registration_schema: Any | None = None,
) -> str:
    """Return one of the versioned registration publication paths."""

    normalized = normalized_repo_relative_path(
        value,
        "registration repository path",
    )
    if normalized not in ALLOWED_REGISTRATION_PATHS:
        raise SelectionError("registration repository path is not registered")
    if registration_schema is not None:
        expected = REGISTRATION_PATH_BY_SCHEMA.get(registration_schema)
        if expected is None:
            raise SelectionError("registration schema has no publication path")
        if normalized != expected:
            raise SelectionError(
                "registration schema/publication path mismatch"
            )
    return normalized


def load_arrow(path: Path) -> Any:
    try:
        import pyarrow as pa
        import pyarrow.ipc as ipc
    except ImportError as error:
        raise SelectionError("pyarrow is required to read the frozen dataset") from error
    try:
        with pa.memory_map(str(path), "r") as source:
            return ipc.open_stream(source).read_all()
    except (OSError, pa.ArrowException) as error:
        raise SelectionError(f"invalid Arrow dataset: {error}") from error


def publish_exclusive(path: Path, value: dict[str, Any]) -> None:
    payload = canonical(value)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def git_tree(git_cache_root: Path, repository: str, commit: str) -> str:
    git_dir = git_cache_root / f"{repository.replace('/', '__')}.git"
    if git_dir.is_symlink() or not git_dir.is_dir():
        raise SelectionError(f"missing regular Git cache for {repository}")
    check = subprocess.run(
        ["git", "--git-dir", str(git_dir), "cat-file", "-e", f"{commit}^{{commit}}"],
        check=False,
        capture_output=True,
        text=True,
    )
    if check.returncode != 0:
        raise SelectionError(f"Git cache lacks selected commit for {repository}")
    resolved = subprocess.run(
        ["git", "--git-dir", str(git_dir), "rev-parse", f"{commit}^{{tree}}"],
        check=False,
        capture_output=True,
        text=True,
    )
    if resolved.returncode != 0:
        raise SelectionError(f"cannot resolve selected tree for {repository}")
    return require_hex(resolved.stdout.strip(), 40, "selected base tree")


def require_commit_binding(
    repository: Path,
    commit: str,
    registration_path: Path,
    exclusions_path: Path,
    registration_repo_path: str = REGISTRATION_PATH,
) -> None:
    if repository.is_symlink() or not (repository / ".git").is_dir():
        raise SelectionError("selector repository must be a regular Git checkout")
    if registration_path.is_symlink() or not registration_path.is_file():
        raise SelectionError("registration must be a regular file")
    registration_bytes = registration_path.read_bytes()
    try:
        registration = json.loads(registration_bytes)
    except json.JSONDecodeError as error:
        raise SelectionError(f"registration is invalid JSON: {error}") from error
    if not isinstance(registration, dict):
        raise SelectionError("registration must be an object")
    registration_repo_path = registered_registration_repo_path(
        registration_repo_path,
        registration.get("schema_version"),
    )
    check = subprocess.run(
        ["git", "-C", str(repository), "cat-file", "-e", f"{commit}^{{commit}}"],
        check=False,
        capture_output=True,
    )
    if check.returncode != 0:
        raise SelectionError("registration commit is absent from the repository")
    expected = {
        registration_repo_path: registration_bytes,
        EXCLUSIONS_PATH: exclusions_path.read_bytes(),
        SELECTOR_PATH: Path(__file__).resolve().read_bytes(),
    }
    for path, local_bytes in expected.items():
        result = subprocess.run(
            ["git", "-C", str(repository), "show", f"{commit}:{path}"],
            check=False,
            capture_output=True,
        )
        if result.returncode != 0 or result.stdout != local_bytes:
            raise SelectionError(
                f"registration commit does not contain exact registered bytes: {path}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--registration", type=Path, required=True)
    parser.add_argument("--exclusions", type=Path, required=True)
    parser.add_argument("--arrow", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--registration-commit", required=True)
    parser.add_argument(
        "--registration-repo-path",
        default=REGISTRATION_PATH,
        help=(
            "normalized repo-relative path containing the exact registration "
            "bytes at --registration-commit"
        ),
    )
    parser.add_argument("--git-cache-root", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rank-only", action="store_true")
    arguments = parser.parse_args()

    registration = load_object(arguments.registration, "registration")
    exclusions = load_object(arguments.exclusions, "exclusions")
    registration_commit = require_hex(
        arguments.registration_commit, 40, "registration commit"
    )
    require_commit_binding(
        arguments.repo.resolve(),
        registration_commit,
        arguments.registration,
        arguments.exclusions.resolve(),
        arguments.registration_repo_path,
    )
    try:
        registration_contract.validate_registration(
            registration,
            source_root=Path(__file__).resolve().parent,
        )
    except registration_contract.RegistrationContractError as exc:
        raise SelectionError(f"registration contract invalid: {exc}") from exc
    dataset = registration.get("dataset")
    selector = registration.get("selector")
    if not isinstance(dataset, dict) or not isinstance(selector, dict):
        raise SelectionError("registration dataset/selector is malformed")
    expected_arrow = require_hex(dataset.get("arrow_sha256"), 64, "Arrow SHA-256")
    if sha256_file(arguments.arrow) != expected_arrow:
        raise SelectionError("Arrow dataset identity mismatch")
    seed = selector.get("seed")
    if seed != EXPECTED_SEED:
        raise SelectionError("selector seed differs from the frozen seed")
    if selector.get("ranking") != EXPECTED_RANKING:
        raise SelectionError("selector ranking differs from the frozen algorithm")
    registration_schema = registration.get("schema_version")
    expected_algorithm = {
        registration_contract.LEGACY_REGISTRATION_SCHEMA: (
            LEGACY_ALGORITHM_VERSION
        ),
        registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA: (
            ISSUE836_V2_ALGORITHM_VERSION
        ),
        registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA: (
            ISSUE836_V3_ALGORITHM_VERSION
        ),
        registration_contract.CURRENT_REGISTRATION_SCHEMA: (
            CURRENT_ALGORITHM_VERSION
        ),
    }.get(registration_schema)
    if selector.get("algorithm_version") != expected_algorithm:
        raise SelectionError("selector algorithm version is invalid")
    dimensions = registration_contract.experiment_dimensions(registration)

    if exclusions.get("schema_version") != "issue827-exclusions-v1":
        raise SelectionError("exclusion schema is not frozen for issue #827")
    if exclusions.get("dataset_rows") != EXPECTED_ROWS:
        raise SelectionError("exclusion dataset row count mismatch")
    excluded_count = exclusions.get("excluded_count")
    eligible_count = exclusions.get("eligible_count")
    if (
        type(excluded_count) is not int
        or type(eligible_count) is not int
        or excluded_count < 0
        or eligible_count < dimensions["case_count"]
        or excluded_count + eligible_count != EXPECTED_ROWS
    ):
        raise SelectionError("exclusion population counts are invalid")
    if selector.get("excluded_rows") != excluded_count or selector.get("eligible_rows") != eligible_count:
        raise SelectionError("registration does not freeze exclusion population counts")
    excluded = exclusions.get("excluded_instance_ids")
    if not isinstance(excluded, list) or any(
        not isinstance(value, str) or not value for value in excluded
    ):
        raise SelectionError("exclusion IDs are invalid")
    if excluded != sorted(set(excluded)):
        raise SelectionError("exclusion IDs must be sorted and unique")
    if exclusions.get("excluded_count") != len(excluded):
        raise SelectionError("exclusion count mismatch")
    expected_exclusions_sha = selector.get("excluded_ids_sha256")
    actual_exclusions_sha = sha256_bytes(canonical(excluded))
    if actual_exclusions_sha != expected_exclusions_sha:
        raise SelectionError("exclusion-set identity mismatch")
    if selector.get("exclusions_file_sha256") != sha256_file(arguments.exclusions):
        raise SelectionError("exclusion receipt file identity mismatch")

    table = load_arrow(arguments.arrow)
    if table.num_rows != EXPECTED_ROWS:
        raise SelectionError(f"expected {EXPECTED_ROWS} rows, found {table.num_rows}")
    required_columns = {"instance_id", "repo", "base_commit", "problem_statement"}
    if not required_columns.issubset(table.column_names):
        raise SelectionError("Arrow dataset lacks required columns")
    ids = table.column("instance_id").to_pylist()
    if any(not isinstance(value, str) or not value for value in ids):
        raise SelectionError("dataset contains an invalid instance ID")
    if len(set(ids)) != EXPECTED_ROWS:
        raise SelectionError("dataset instance IDs are not unique")
    if not set(excluded).issubset(ids):
        raise SelectionError("exclusion set contains an unknown instance ID")

    eligible_ranked = deterministic_ranked_prefix(
        ids,
        set(excluded),
        seed,
        exclusions["eligible_count"],
    )
    if len(eligible_ranked) != exclusions.get("eligible_count"):
        raise SelectionError("eligible population count mismatch")
    selected = registered_ranked_cohort(registration, eligible_ranked)
    rows_by_id = {ids[index]: index for index in range(table.num_rows)}
    if arguments.rank_only and arguments.git_cache_root is not None:
        raise SelectionError("rank-only mode cannot consume a Git cache")
    if not arguments.rank_only and arguments.git_cache_root is None:
        raise SelectionError("final selection requires --git-cache-root")

    cases: list[dict[str, Any]] = []
    for rank, (rank_key, instance_id) in enumerate(selected, start=1):
        row = rows_by_id[instance_id]
        repository = table.column("repo")[row].as_py()
        commit = table.column("base_commit")[row].as_py()
        problem = table.column("problem_statement")[row].as_py()
        if not isinstance(repository, str) or not repository:
            raise SelectionError("selected repository is invalid")
        require_hex(commit, 40, "selected base commit")
        if not isinstance(problem, str):
            raise SelectionError("selected problem statement is not UTF-8 text")
        case: dict[str, Any] = {
            "rank": rank,
            "instance_id": instance_id,
            "ranking_sha256": rank_key,
            "repo": repository,
            "base_commit": commit,
            "problem_statement_sha256": sha256_bytes(problem.encode("utf-8")),
            "arm_order": ["A", "T"] if rank % 2 == 1 else ["T", "A"],
        }
        if not arguments.rank_only:
            case["base_tree"] = git_tree(
                arguments.git_cache_root, repository, commit
            )
            case["cache_preparation"] = (
                "cold exact-tree in-place index with the registered exact-CI artifact; "
                "fresh-process readiness and strict hybrid/Metal query verification"
            )
        cases.append(case)

    receipt = {
        "schema_version": selection_schema(registration),
        "state": "ranked_needs_tree_binding" if arguments.rank_only else "selected_pre_model",
        "authoritative": not arguments.rank_only,
        "registration_commit": registration_commit,
        "registration_sha256": sha256_file(arguments.registration),
        "exclusions_sha256": sha256_file(arguments.exclusions),
        "excluded_ids_sha256": actual_exclusions_sha,
        "dataset_arrow_sha256": expected_arrow,
        "seed": seed,
        "population_rows": EXPECTED_ROWS,
        "excluded_rows": len(excluded),
        "eligible_rows": len(eligible_ranked),
        "problem_statements_inspected_by_human_before_selection": False,
        "gold_or_outcomes_inspected_before_selection": False,
        "fresh_case_claim": True,
        "prior_model_calls": 0,
        "cases": cases,
        "model_calls_authorized_before_cache_readiness": False,
        "case_replacement_after_model_start": False,
    }
    if (
        registration.get("schema_version")
        == registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA
    ):
        receipt["prefix_lineage"] = {
            "ranks_1_through_2": "pre_model_carry_forward_prefix",
            "ranks_3_through_20": "deterministic_extension",
            "outcomes_inspected_for_extension": False,
        }
    if (
        registration.get("schema_version")
        == registration_contract.ISSUE836_V3_REGISTRATION_SCHEMA
    ):
        receipt["pre_model_v2_supersession"] = (
            registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
        )
        receipt["prefix_lineage"] = (
            registration_contract.FROZEN_V3_PREFIX_LINEAGE
        )
    if (
        registration.get("schema_version")
        == registration_contract.CURRENT_REGISTRATION_SCHEMA
    ):
        receipt["pre_model_v3_supersession"] = (
            registration_contract.FROZEN_V4_PRE_MODEL_SUPERSESSION
        )
        receipt["prefix_lineage"] = (
            registration_contract.FROZEN_V4_PREFIX_LINEAGE
        )
        receipt["prior_official_evaluator_invocations"] = 0
    receipt["digest"] = sha256_bytes(canonical(receipt))
    expected_episode_identities(
        registration,
        receipt,
        registration_repository=arguments.repo.resolve(),
        registration_repo_path=arguments.registration_repo_path,
    )
    publish_exclusive(arguments.output, receipt)
    print(canonical(receipt).decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
