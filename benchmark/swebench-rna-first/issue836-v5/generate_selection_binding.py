#!/usr/bin/env python3
"""Bind a committed successor schedule to the unchanged frozen v4 cohort."""

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
V4 = ROOT.parent / "issue836-v4"


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


def build(schedule_commit: str) -> dict:
    schedule_path = ROOT / contract.SCHEDULE_FILENAME
    schedule = json.loads(schedule_path.read_bytes())
    contract.validate_schedule(schedule, ROOT)
    commit = git("rev-parse", f"{schedule_commit}^{{commit}}")
    tree = git("rev-parse", f"{commit}^{{tree}}")
    relative = schedule_path.relative_to(REPO).as_posix()
    committed = subprocess.run(
        ["git", "-C", str(REPO), "show", f"{commit}:{relative}"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    contract.require(
        committed.returncode == 0
        and hashlib.sha256(committed.stdout).hexdigest()
        == contract.sha_file(schedule_path),
        "live schedule differs from the schedule commit",
    )
    registration_path = V4 / "registration.json"
    selection_path = V4 / "selection.json"
    contract.require(
        contract.sha_file(registration_path)
        == contract.BASE_REGISTRATION_SHA256
        and contract.sha_file(selection_path)
        == contract.BASE_SELECTION_SHA256,
        "frozen v4 registration/selection drift",
    )
    selection = json.loads(selection_path.read_bytes())
    cases = [
        {
            key: case[key]
            for key in (
                "rank",
                "instance_id",
                "repo",
                "base_commit",
                "base_tree",
                "problem_statement_sha256",
                "arm_order",
            )
        }
        for case in selection["cases"]
    ]
    return {
        "schema_version": contract.SELECTION_BINDING_SCHEMA,
        "authoritative": True,
        "protocol_change": (
            "execution_and_qualification_compatibility_only_"
            "no_cohort_or_arm_change"
        ),
        "schedule_sha256": contract.sha_file(schedule_path),
        "schedule_commit": commit,
        "schedule_tree": tree,
        "base_registration_sha256": contract.BASE_REGISTRATION_SHA256,
        "base_selection_sha256": contract.BASE_SELECTION_SHA256,
        "base_selection_digest": selection["digest"],
        "case_count": 20,
        "episode_count": 40,
        "cases": cases,
        "episode_identities": [
            {
                "rank": case["rank"],
                "case_id": case["instance_id"],
                "arm": arm,
            }
            for case in cases
            for arm in case["arm_order"]
        ],
        "problem_statements_inspected_for_schedule_change": False,
        "gold_or_outcomes_inspected_for_schedule_change": False,
        "prior_model_calls": 0,
        "prior_provider_requests": 0,
        "prior_official_evaluator_invocations": 0,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--schedule-commit", required=True)
    result.add_argument(
        "--output",
        type=Path,
        default=ROOT / contract.SELECTION_BINDING_FILENAME,
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        output = args.output.resolve(strict=False)
        contract.require(
            output
            == (
                ROOT / contract.SELECTION_BINDING_FILENAME
            ).resolve(strict=False),
            "selection-binding output path is fixed",
        )
        contract.require(
            not output.exists() and not output.is_symlink(),
            "selection-binding output must be absent",
        )
        binding = build(args.schedule_commit)
        selection = json.loads((V4 / "selection.json").read_bytes())
        contract.validate_selection_binding(
            binding,
            schedule_sha256=contract.sha_file(
                ROOT / contract.SCHEDULE_FILENAME
            ),
            selection=selection,
        )
        with output.open("xb") as handle:
            handle.write(contract.canonical(binding))
        print(
            json.dumps(
                {
                    "status": "SELECTION_BINDING_GENERATED_ZERO_CALL",
                    "selection_binding": contract.file_ref(output),
                    "schedule_commit": binding["schedule_commit"],
                    "schedule_tree": binding["schedule_tree"],
                    "models_launched": 0,
                    "provider_requests": 0,
                    "official_evaluator_invocations": 0,
                },
                sort_keys=True,
                indent=2,
            )
        )
        return 0
    except (
        contract.ContractError,
        OSError,
        json.JSONDecodeError,
    ) as exc:
        print(f"FAIL CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
