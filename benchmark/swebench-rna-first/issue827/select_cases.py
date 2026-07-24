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
from pathlib import Path
import subprocess
from typing import Any, Mapping, NamedTuple

import registration_contract

LEGACY_SCHEMA = "issue827-fresh-pair-selection-v1"
CURRENT_SCHEMA = "issue836-fresh-cohort-selection-v2"
SCHEMA = CURRENT_SCHEMA
LEGACY_ALGORITHM_VERSION = "issue827-selector-v1"
CURRENT_ALGORITHM_VERSION = "issue836-selector-v2"
EXPECTED_ROWS = 500
EXPECTED_SEED = "rna-first-sonnet-hermetic-selector-v1"
EXPECTED_RANKING = "ascending SHA256(seed_utf8 || 0x00 || instance_id_utf8)"
REGISTRATION_PATH = "benchmark/swebench-rna-first/issue827/registration.json"
EXCLUSIONS_PATH = "benchmark/swebench-rna-first/issue827/exclusions.json"
SELECTOR_PATH = "benchmark/swebench-rna-first/issue827/select_cases.py"


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
    if schema == registration_contract.CURRENT_REGISTRATION_SCHEMA:
        return CURRENT_SCHEMA
    raise SelectionError("registration schema has no selection schema")


def expected_episode_identities(
    registration: Mapping[str, Any],
    selection: Mapping[str, Any],
) -> tuple[ExpectedEpisodeIdentity, ...]:
    """Validate selection count/rank/parity and enumerate exact episodes."""

    try:
        dimensions = registration_contract.experiment_dimensions(registration)
    except registration_contract.RegistrationContractError as exc:
        raise SelectionError(f"registration dimensions invalid: {exc}") from exc
    if selection.get("schema_version") != selection_schema(registration):
        raise SelectionError("selection schema does not match registration version")
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
) -> None:
    if repository.is_symlink() or not (repository / ".git").is_dir():
        raise SelectionError("selector repository must be a regular Git checkout")
    check = subprocess.run(
        ["git", "-C", str(repository), "cat-file", "-e", f"{commit}^{{commit}}"],
        check=False,
        capture_output=True,
    )
    if check.returncode != 0:
        raise SelectionError("registration commit is absent from the repository")
    expected = {
        REGISTRATION_PATH: registration_path.read_bytes(),
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
        arguments.registration.resolve(),
        arguments.exclusions.resolve(),
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
    expected_algorithm = (
        LEGACY_ALGORITHM_VERSION
        if registration.get("schema_version")
        == registration_contract.LEGACY_REGISTRATION_SCHEMA
        else CURRENT_ALGORITHM_VERSION
    )
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
    selected = eligible_ranked[: dimensions["case_count"]]
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
        == registration_contract.CURRENT_REGISTRATION_SCHEMA
    ):
        receipt["prefix_lineage"] = {
            "ranks_1_through_2": "pre_model_carry_forward_prefix",
            "ranks_3_through_20": "deterministic_extension",
            "outcomes_inspected_for_extension": False,
        }
    expected_episode_identities(registration, receipt)
    receipt["digest"] = sha256_bytes(canonical(receipt))
    publish_exclusive(arguments.output, receipt)
    print(canonical(receipt).decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
