#!/usr/bin/env python3
"""Generate the absent-path successor schedule after its implementation commit."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from typing import Sequence

import schedule_contract as contract


ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
REGISTERED_FILES = (
    "schedule_contract.py",
    "run_wave.py",
    "observational_tool_supervisor.py",
    "verify_preconditioned.py",
    "precondition-prefix.txt",
    "precondition-suffix.txt",
    "verify_waves.py",
    "assemble_wave.py",
    "assemble_successor.py",
    "generate_schedule.py",
    "generate_selection_binding.py",
    contract.PREDECESSOR_ACTIVITY_FILENAME,
)


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(REPO), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise contract.ContractError(
            result.stderr.decode(errors="replace").strip()
        )
    return result.stdout.decode("ascii").strip()


def repo_relative(path: Path) -> str:
    return path.resolve(strict=True).relative_to(REPO.resolve(strict=True)).as_posix()


def build(implementation_commit: str) -> dict:
    commit = git("rev-parse", f"{implementation_commit}^{{commit}}")
    tree = git("rev-parse", f"{commit}^{{tree}}")
    for filename in REGISTERED_FILES:
        path = ROOT / filename
        relative = repo_relative(path)
        tracked = subprocess.run(
            ["git", "-C", str(REPO), "cat-file", "-e", f"{commit}:{relative}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        contract.require(
            tracked.returncode == 0,
            f"registered v8 source is absent from implementation commit: {filename}",
        )
        committed = subprocess.run(
            ["git", "-C", str(REPO), "show", f"{commit}:{relative}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        contract.require(
            committed.returncode == 0
            and contract.sha_file(path)
            == hashlib.sha256(committed.stdout).hexdigest(),
            f"working v8 source differs from implementation commit: {filename}",
        )
    qualified_registration = subprocess.run(
        [
            "git",
            "-C",
            str(REPO),
            "show",
            (
                f"{commit}:"
                f"{contract.QUALIFIED_REGISTRATION_RELATIVE_PATH}"
            ),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    contract.require(
        qualified_registration.returncode == 0
        and hashlib.sha256(qualified_registration.stdout).hexdigest()
        == contract.QUALIFIED_REGISTRATION_SHA256,
        "qualified predecessor registration differs from implementation commit",
    )
    return {
        "schema_version": contract.SCHEDULE_SCHEMA,
        "authoritative": True,
        "protocol_change": (
            "restore_title_query_preconditioning_and_trusted_gateway_access_"
            "plus_native_tool_parallelism_no_cohort_or_arm_order_change"
        ),
        "base_source_commit": contract.BASE_SOURCE_COMMIT,
        "base_source_tree": contract.BASE_SOURCE_TREE,
        "implementation_commit": commit,
        "implementation_tree": tree,
        "base_registration_sha256": contract.BASE_REGISTRATION_SHA256,
        "base_selection_sha256": contract.BASE_SELECTION_SHA256,
        "approved_assembler_sha256": contract.APPROVED_ASSEMBLER_SHA256,
        "qualification_compatibility": contract.QUALIFICATION_COMPATIBILITY,
        "registered_files": {
            filename: contract.sha_file(ROOT / filename)
            for filename in REGISTERED_FILES
        },
        "case_count": 20,
        "episode_count": 40,
        "per_episode_budget_usd": 6.0,
        "maximum_budget_usd": 240.0,
        "max_cases_per_wave": 2,
        "max_episodes_per_wave": 4,
        "same_case_serialized": True,
        "different_cases_max_parallel": 2,
        "one_shot_per_rank": True,
        "append_only_cumulative_ledger": True,
        "evaluation_before_full_cohort_allowed": False,
        "predecessor_activity": contract.file_ref(
            ROOT / contract.PREDECESSOR_ACTIVITY_FILENAME
        ),
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--implementation-commit", required=True)
    result.add_argument(
        "--output",
        type=Path,
        default=ROOT / contract.SCHEDULE_FILENAME,
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        output = args.output.resolve(strict=False)
        contract.require(
            output
            == (ROOT / contract.SCHEDULE_FILENAME).resolve(strict=False),
            "schedule output path is fixed",
        )
        contract.require(
            not output.exists() and not output.is_symlink(),
            "schedule output must be absent",
        )
        schedule = build(args.implementation_commit)
        data = contract.canonical(schedule)
        with output.open("xb") as handle:
            handle.write(data)
        contract.validate_schedule(schedule, ROOT)
        print(
            json.dumps(
                {
                    "status": "SCHEDULE_GENERATED_ZERO_CALL",
                    "schedule": contract.file_ref(output),
                    "implementation_commit": schedule["implementation_commit"],
                    "implementation_tree": schedule["implementation_tree"],
                    "models_launched": 0,
                    "provider_requests": 0,
                    "official_evaluator_invocations": 0,
                },
                sort_keys=True,
                indent=2,
            )
        )
        return 0
    except (contract.ContractError, OSError) as exc:
        print(f"FAIL CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
