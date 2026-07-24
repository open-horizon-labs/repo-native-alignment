#!/usr/bin/env python3
"""Adapt one approved no-spend v4 case assembly into a v5 wave manifest."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
from typing import Any, Mapping, Sequence


ROOT = Path(__file__).resolve().parent
BASE = ROOT.parent / "issue827"
sys.path.insert(0, str(BASE))

import run_selector as base  # noqa: E402

import schedule_contract as contract  # noqa: E402


V4_MANIFEST_SCHEMA = "issue836-v4-rolling-case-manifest-v1"
V4_RECEIPT_SCHEMA = "issue836-v4-rolling-assembly-receipt-v1"
V4_HANDOFF_SCHEMA = "issue836-v4-rolling-runner-handoff-v1"
COMPATIBILITY_KEYS = {
    "schema_version",
    "evidence_root",
    "registration",
    "selection",
    "runner",
    "common_supervisor",
    "claude",
    "rna_artifact",
    "mcp_config",
    "qualification_closure",
    "isolation",
    "output_root",
    "cases",
    "wave_schedule",
    "wave_schedule_contract",
    "wave_selection_binding",
    "wave_runner",
    "wave_assembler",
    "episode_envelope",
    "wave_envelope_binding",
    "batch_id",
    "explicit_requested_ranks",
}
WAVE_MANIFEST_KEYS = {
    "schema_version",
    "evidence_root",
    "schedule",
    "schedule_contract",
    "selection_binding",
    "episode_envelope",
    "envelope_binding",
    "registration",
    "selection",
    "registered_runner",
    "wave_runner",
    "wave_assembler",
    "common_supervisor",
    "runtime_manifest_inputs",
    "static_setup",
    "compatibility_manifest",
    "v4_case_manifest",
    "v4_assembly_receipt",
    "claude",
    "rna_artifact",
    "mcp_config",
    "qualification_closure",
    "isolation",
    "output_root",
    "output_root_absent_at_assembly",
    "batch_id",
    "explicit_requested_ranks",
    "unselected_ranks",
    "execution_episode_keys",
    "same_case_serialized",
    "max_parallel_cases",
    "per_episode_budget_usd",
    "wave_maximum_budget_usd",
    "selection_policy",
    "model_outputs_inspected",
    "evaluator_or_outcome_accessed",
    "cases",
    "no_spend_assertion",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise base.FailClosed(message)


def load_ref_json(value: Any, where: str) -> tuple[Path, dict[str, Any]]:
    try:
        path, data = contract.check_ref(value, where)
        document = json.loads(data)
    except (
        contract.ContractError,
        OSError,
        json.JSONDecodeError,
    ) as exc:
        raise base.FailClosed(f"{where}: {exc}") from exc
    require(isinstance(document, dict), f"{where} must contain an object")
    return path, document


def write_new(path: Path, value: Mapping[str, Any]) -> None:
    require(
        not path.exists() and not path.is_symlink(),
        f"refusing to overwrite {path}",
    )
    data = contract.canonical(value)
    with path.open("xb") as handle:
        handle.write(data)


def validate_execution_episode_keys(
    manifest: Mapping[str, Any],
    receipt: Mapping[str, Any],
    handoff: Mapping[str, Any],
) -> None:
    expected = [
        {
            "rank": case["rank"],
            "case_id": case["instance_id"],
            "arm": arm,
            "session_id": case["arms"][arm]["session_id"],
        }
        for case in manifest["cases"]
        for arm in case["arm_order"]
    ]
    require(
        receipt.get("execution_episode_keys")
        == manifest.get("execution_episode_keys")
        == handoff.get("exact_requested_episode_order")
        == expected,
        "v4 requested episode order drift",
    )


def validate_v4_inputs(
    v4_manifest_path: Path,
    v4_receipt_path: Path,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], dict[str, Any]]:
    v4_manifest_path = v4_manifest_path.resolve(strict=True)
    v4_receipt_path = v4_receipt_path.resolve(strict=True)
    manifest = json.loads(v4_manifest_path.read_bytes())
    receipt = json.loads(v4_receipt_path.read_bytes())
    require(
        isinstance(manifest, dict)
        and isinstance(receipt, dict)
        and manifest.get("schema_version") == V4_MANIFEST_SCHEMA
        and receipt.get("schema_version") == V4_RECEIPT_SCHEMA
        and manifest.get("assembler", {}).get("sha256")
        == contract.APPROVED_ASSEMBLER_SHA256
        and receipt.get("assembler") == manifest.get("assembler")
        and receipt.get("case_manifest")
        == contract.file_ref(v4_manifest_path)
        and manifest.get("launch_authorized") is False
        and manifest.get("model_outputs_inspected") is False
        and manifest.get("evaluator_or_outcome_accessed") is False
        and manifest.get("no_spend_assertion") == contract.NO_SPEND
        and receipt.get("no_spend_assertion") == contract.NO_SPEND,
        "v4 case assembly is not the approved blocked/no-spend form",
    )
    contract.check_ref(manifest["assembler"], "v4 manifest.assembler")
    handoff_path, handoff = load_ref_json(
        receipt["runner_handoff"],
        "v4 receipt.runner_handoff",
    )
    del handoff_path
    require(
        handoff.get("schema_version") == V4_HANDOFF_SCHEMA
        and handoff.get("status")
        == "CASE_INPUTS_READY_REGISTERED_RUNNER_BLOCKED"
        and handoff.get("case_manifest") == receipt["case_manifest"]
        and handoff.get("manual_launch_authorized") is False
        and handoff.get("models_launched") == 0
        and handoff.get("provider_requests") == 0
        and handoff.get("official_evaluator_invocations") == 0,
        "v4 handoff is not strictly blocked/no-spend",
    )
    registration_path, registration_bytes = base.check_ref(
        manifest["registration"],
        "v4 manifest.registration",
    )
    selection_path, _ = base.check_ref(
        manifest["selection"],
        "v4 manifest.selection",
    )
    require(
        manifest["registration"]["sha256"]
        == contract.BASE_REGISTRATION_SHA256
        and manifest["selection"]["sha256"]
        == contract.BASE_SELECTION_SHA256
        and receipt["registration"] == manifest["registration"]
        and receipt["selection"] == manifest["selection"],
        "v4 frozen registration/selection drift",
    )
    registration = base.read_json(registration_path)
    selection = base.read_json(selection_path)
    base.validate_registered_sources(registration)
    base.validate_authoritative_selection(selection, registration_bytes)
    require(
        manifest["registered_runner"] == contract.file_ref(BASE / "run_selector.py")
        and manifest["common_supervisor"]
        == contract.file_ref(BASE / "common_supervisor.py"),
        "v4 manifest runner/common-supervisor identity drift",
    )
    _, envelope = load_ref_json(
        manifest["episode_envelope"],
        "v4 manifest.episode_envelope",
    )
    try:
        sessions = contract.validate_episode_envelope(
            envelope,
            selection,
            registration_ref=manifest["registration"],
            selection_ref=manifest["selection"],
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    ranks = contract.explicit_wave_ranks(
        manifest["explicit_requested_ranks"]
    )
    require(
        receipt["explicit_requested_ranks"] == list(ranks)
        and receipt["case_count"] == len(ranks)
        and receipt["episode_count"] == 2 * len(ranks)
        and receipt["per_episode_budget_usd"] == 6.0
        and receipt["batch_maximum_budget_usd"] == 12.0 * len(ranks)
        and manifest["materialized_case_count"] == len(ranks)
        and manifest["materialized_episode_count"] == 2 * len(ranks)
        and manifest["wave_maximum_budget_usd"] == 12.0 * len(ranks)
        and [case.get("rank") for case in manifest["cases"]] == list(ranks),
        "v4 explicit rank/count/budget scope drift",
    )
    selected_by_rank = {
        case["rank"]: case for case in selection["cases"]
    }
    receipt_by_rank = {
        case["rank"]: case for case in receipt["cases"]
    }
    for case in manifest["cases"]:
        rank = case["rank"]
        chosen = selected_by_rank[rank]
        frozen_receipt = receipt_by_rank[rank]
        require(
            case["instance_id"] == chosen["instance_id"]
            and case["base_commit"] == chosen["base_commit"]
            and case["base_tree"] == chosen["base_tree"]
            and case["arm_order"] == chosen["arm_order"]
            and frozen_receipt["instance_id"] == chosen["instance_id"]
            and frozen_receipt["arm_order"] == chosen["arm_order"]
            and all(
                case["arms"][arm]["session_id"] == sessions[rank][arm]
                == frozen_receipt["sessions"][arm]
                for arm in ("A", "T")
            ),
            f"v4 case identity/session drift at rank {rank}",
        )
        cache_path, cache_bytes = contract.check_ref(
            frozen_receipt["cache_input"],
            f"v4 receipt rank {rank}.cache_input",
        )
        del cache_path
        require(
            json.loads(cache_bytes) == case["cache"],
            f"v4 case cache drift at rank {rank}",
        )
    validate_execution_episode_keys(manifest, receipt, handoff)
    return manifest, receipt, registration, selection


def assemble(
    *,
    v4_manifest_path: Path,
    v4_receipt_path: Path,
    cumulative_output_root: Path,
    envelope_binding_path: Path,
) -> dict[str, Any]:
    manifest, receipt, _, selection = validate_v4_inputs(
        v4_manifest_path,
        v4_receipt_path,
    )
    evidence_root = Path(manifest["evidence_root"]).resolve(strict=True)
    cumulative_output_root = cumulative_output_root.resolve(strict=False)
    require(
        cumulative_output_root.is_absolute()
        and cumulative_output_root.is_relative_to(evidence_root)
        and cumulative_output_root != evidence_root
        and not cumulative_output_root.is_symlink(),
        "cumulative output root must be a canonical child of evidence root",
    )
    schedule_path = ROOT / "execution-schedule.json"
    selection_binding_path = ROOT / "selection-binding.json"
    schedule = json.loads(schedule_path.read_bytes())
    selection_binding = json.loads(selection_binding_path.read_bytes())
    contract.validate_schedule(schedule, ROOT)
    contract.validate_selection_binding(
        selection_binding,
        schedule_sha256=contract.sha_file(schedule_path),
        selection=selection,
    )
    schedule_ref = contract.file_ref(schedule_path)
    selection_binding_ref = contract.file_ref(selection_binding_path)
    envelope_ref = manifest["episode_envelope"]
    envelope_binding_path = envelope_binding_path.resolve(strict=False)
    require(
        envelope_binding_path.is_absolute()
        and envelope_binding_path.is_relative_to(evidence_root)
        and envelope_binding_path.name == "v5-envelope-binding.json"
        and envelope_binding_path.parent.is_dir()
        and not envelope_binding_path.parent.is_symlink(),
        "v5 envelope-binding path invalid",
    )
    if not envelope_binding_path.exists():
        binding = {
            "schema_version": contract.ENVELOPE_BINDING_SCHEMA,
            "verified": True,
            "schedule": schedule_ref,
            "selection_binding": selection_binding_ref,
            "episode_envelope": envelope_ref,
            "approved_assembler_sha256": contract.APPROVED_ASSEMBLER_SHA256,
            "case_count": 20,
            "episode_count": 40,
            "per_episode_budget_usd": 6.0,
            "maximum_budget_usd": 240.0,
            "model_outputs_inspected": False,
            "evaluator_or_outcome_accessed": False,
            "no_spend_assertion": contract.NO_SPEND,
        }
        write_new(envelope_binding_path, binding)
    envelope_binding = json.loads(envelope_binding_path.read_bytes())
    contract.validate_envelope_binding(
        envelope_binding,
        schedule_ref=schedule_ref,
        selection_binding_ref=selection_binding_ref,
        envelope_ref=envelope_ref,
    )
    envelope_binding_ref = contract.file_ref(envelope_binding_path)
    attempt_root = v4_manifest_path.resolve(strict=True).parent
    compatibility_path = attempt_root / "v5-compatibility-manifest.json"
    wave_path = attempt_root / "rolling-wave-manifest.json"
    require(
        not compatibility_path.exists()
        and not compatibility_path.is_symlink()
        and not wave_path.exists()
        and not wave_path.is_symlink(),
        "v5 wave outputs must be absent",
    )
    schedule_contract_ref = contract.file_ref(ROOT / "schedule_contract.py")
    wave_runner_ref = contract.file_ref(ROOT / "run_wave.py")
    wave_assembler_ref = contract.file_ref(Path(__file__).resolve())
    compatibility = {
        "schema_version": base.RUN_SCHEMA,
        "evidence_root": manifest["evidence_root"],
        "registration": manifest["registration"],
        "selection": manifest["selection"],
        "runner": manifest["registered_runner"],
        "common_supervisor": manifest["common_supervisor"],
        "claude": manifest["claude"],
        "rna_artifact": manifest["rna_artifact"],
        "mcp_config": manifest["mcp_config"],
        "qualification_closure": manifest["qualification_closure"],
        "isolation": manifest["isolation"],
        "output_root": str(cumulative_output_root),
        "cases": manifest["cases"],
        "wave_schedule": schedule_ref,
        "wave_schedule_contract": schedule_contract_ref,
        "wave_selection_binding": selection_binding_ref,
        "wave_runner": wave_runner_ref,
        "wave_assembler": wave_assembler_ref,
        "episode_envelope": envelope_ref,
        "wave_envelope_binding": envelope_binding_ref,
        "batch_id": receipt["batch_id"],
        "explicit_requested_ranks": manifest["explicit_requested_ranks"],
    }
    require(
        set(compatibility) == COMPATIBILITY_KEYS,
        "v5 compatibility manifest interface drift",
    )
    write_new(compatibility_path, compatibility)
    ranks = contract.explicit_wave_ranks(
        manifest["explicit_requested_ranks"]
    )
    wave = {
        "schema_version": contract.WAVE_MANIFEST_SCHEMA,
        "evidence_root": manifest["evidence_root"],
        "schedule": schedule_ref,
        "schedule_contract": schedule_contract_ref,
        "selection_binding": selection_binding_ref,
        "episode_envelope": envelope_ref,
        "envelope_binding": envelope_binding_ref,
        "registration": manifest["registration"],
        "selection": manifest["selection"],
        "registered_runner": manifest["registered_runner"],
        "wave_runner": wave_runner_ref,
        "wave_assembler": wave_assembler_ref,
        "common_supervisor": manifest["common_supervisor"],
        "runtime_manifest_inputs": manifest["runtime_manifest_inputs"],
        "static_setup": manifest["static_setup"],
        "compatibility_manifest": contract.file_ref(compatibility_path),
        "v4_case_manifest": contract.file_ref(v4_manifest_path.resolve(strict=True)),
        "v4_assembly_receipt": contract.file_ref(v4_receipt_path.resolve(strict=True)),
        "claude": manifest["claude"],
        "rna_artifact": manifest["rna_artifact"],
        "mcp_config": manifest["mcp_config"],
        "qualification_closure": manifest["qualification_closure"],
        "isolation": manifest["isolation"],
        "output_root": str(cumulative_output_root),
        "output_root_absent_at_assembly": not cumulative_output_root.exists(),
        "batch_id": receipt["batch_id"],
        "explicit_requested_ranks": list(ranks),
        "unselected_ranks": [
            rank for rank in range(1, 21) if rank not in set(ranks)
        ],
        "execution_episode_keys": manifest["execution_episode_keys"],
        "same_case_serialized": True,
        "max_parallel_cases": min(2, len(ranks)),
        "per_episode_budget_usd": 6.0,
        "wave_maximum_budget_usd": 12.0 * len(ranks),
        "selection_policy": "explicit_rank_arguments_only",
        "model_outputs_inspected": False,
        "evaluator_or_outcome_accessed": False,
        "cases": manifest["cases"],
        "no_spend_assertion": contract.NO_SPEND,
    }
    require(
        set(wave) == WAVE_MANIFEST_KEYS,
        "v5 wave manifest interface drift",
    )
    write_new(wave_path, wave)
    return {
        "status": "READY_FOR_ZERO_SPEND_V5_PREFLIGHT",
        "wave_manifest": contract.file_ref(wave_path),
        "compatibility_manifest": contract.file_ref(compatibility_path),
        "envelope_binding": envelope_binding_ref,
        "runner": wave_runner_ref,
        "preflight_argv": [
            sys.executable,
            str(ROOT / "run_wave.py"),
            "preflight",
            "--manifest",
            str(wave_path),
        ],
        "models_launched": 0,
        "provider_requests": 0,
        "official_evaluator_invocations": 0,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--v4-manifest", type=Path, required=True)
    result.add_argument("--v4-receipt", type=Path, required=True)
    result.add_argument("--cumulative-output-root", type=Path, required=True)
    result.add_argument("--envelope-binding", type=Path, required=True)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        summary = assemble(
            v4_manifest_path=args.v4_manifest,
            v4_receipt_path=args.v4_receipt,
            cumulative_output_root=args.cumulative_output_root,
            envelope_binding_path=args.envelope_binding,
        )
        print(json.dumps(summary, sort_keys=True, indent=2))
        return 0
    except (
        base.FailClosed,
        contract.ContractError,
        OSError,
        json.JSONDecodeError,
    ) as exc:
        print(f"FAIL CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
