#!/usr/bin/env python3
"""Verify the sealed issue836 successor chain as one frozen 40-episode run."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any, Mapping, Sequence


ROOT = Path(__file__).resolve().parent
BASE = ROOT.parent / "issue827"
sys.path.insert(0, str(BASE))

import run_selector as base  # noqa: E402
import verify_selector as base_verifier  # noqa: E402

import run_wave  # noqa: E402
import schedule_contract as contract  # noqa: E402


AGGREGATE_SCHEMA = contract.FINAL_LEDGER_SCHEMA


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


def verify_episode_with_qualification_compatibility(
    episode_path: Path,
    *,
    compatibility: Mapping[str, Any],
    registration: Mapping[str, Any],
    schedule: Mapping[str, Any],
    where: str,
) -> dict[str, Any]:
    """Run the frozen verifier with only the exact successor bridge replaced."""

    original = base_verifier.runner.verify_qualification_closure

    def bridged(
        manifest: Mapping[str, Any],
        episode_registration: Mapping[str, Any],
    ) -> None:
        run_wave.verify_v4_qualification_compatibility(
            manifest,
            episode_registration,
            schedule,
            qualification_verifier=original,
        )

    base_verifier.runner.verify_qualification_closure = bridged
    try:
        result = base_verifier.verify_episode(episode_path)
    finally:
        base_verifier.runner.verify_qualification_closure = original
    require(
        result.get("evidence_complete") is True
        and result.get("official_evaluator_invoked") is False
        and result.get("errors") == [],
        f"{where} is not verifier-clean: "
        + ",".join(result.get("errors", [])),
    )
    return result


def validate_sealed_wave_documents(
    waves: Sequence[Mapping[str, Any]],
    *,
    expected_identities: Sequence[tuple[int, str, str]],
    envelope_sessions: Mapping[int, Mapping[str, str]],
) -> None:
    """Pure full-cohort ledger validation used by tests and final verification."""

    prior_ranks: set[int] = set()
    prior_sessions: set[str] = set()
    prior_refs: list[dict[str, Any]] = []
    authorized_identities: list[tuple[int, str, str]] = []
    total_budget = 0.0
    for index, wave in enumerate(waves):
        requested = contract.explicit_wave_ranks(wave["requested_ranks"])
        requested_sessions = set(wave["requested_sessions"])
        try:
            state = contract.next_cumulative_state(
                prior_ranks=prior_ranks,
                prior_sessions=prior_sessions,
                requested_ranks=requested,
                requested_sessions=requested_sessions,
            )
        except contract.ContractError as exc:
            raise base.FailClosed(str(exc)) from exc
        expected_wave = [
            {
                "rank": rank,
                "case_id": case_id,
                "arm": arm,
                "session_id": envelope_sessions[rank][arm],
            }
            for rank, case_id, arm in expected_identities
            if rank in set(requested)
        ]
        require(
            wave["prior_wave_receipts"] == prior_refs
            and wave["authorized_episode_keys"] == expected_wave
            and requested_sessions
            == {
                envelope_sessions[rank][arm]
                for rank in requested
                for arm in ("A", "T")
            }
            and wave["case_count"] == len(requested)
            and wave["episode_count"] == 2 * len(requested)
            and wave["per_episode_budget_usd"] == 6.0
            and wave["maximum_budget_usd"] == 12.0 * len(requested)
            and wave["cumulative_ranks"] == state["cumulative_ranks"]
            and wave["cumulative_sessions"] == state["cumulative_sessions"]
            and wave["cumulative_case_count"] == state["cumulative_case_count"]
            and wave["cumulative_episode_count"]
            == state["cumulative_episode_count"]
            and wave["cumulative_maximum_budget_usd"]
            == state["cumulative_maximum_budget_usd"]
            and wave["pending_ranks"] == state["pending_ranks"]
            and wave["all_authorized_episodes_recorded"] is True
            and wave["worker_errors"] == []
            and len(wave["episode_receipts"]) == 2 * len(requested)
            and wave["official_evaluator_invoked"] is False,
            f"sealed wave ledger drift at index {index}",
        )
        authorized_identities.extend(
            (item["rank"], item["case_id"], item["arm"])
            for item in expected_wave
        )
        total_budget += wave["maximum_budget_usd"]
        prior_ranks.update(requested)
        prior_sessions.update(requested_sessions)
        prior_refs.append(wave["self_ref"])
    require(
        prior_ranks == set(range(1, 21))
        and len(prior_sessions) == 40
        and len(authorized_identities) == 40
        and len(set(authorized_identities)) == 40
        and set(authorized_identities) == set(expected_identities)
        and total_budget == 240.0,
        "sealed wave documents do not form the complete 20/40/$240 cohort",
    )


def _verify_compatibility_manifest(
    compatibility_ref: Mapping[str, Any],
    *,
    output_root: Path,
    wave_manifest: Mapping[str, Any],
    schedule_ref: Mapping[str, Any],
    envelope_ref: Mapping[str, Any],
    registration_ref: Mapping[str, Any],
    selection_ref: Mapping[str, Any],
    wave_runner_ref: Mapping[str, Any],
    wave_assembler_ref: Mapping[str, Any],
    selection_binding_ref: Mapping[str, Any],
    envelope_binding_ref: Mapping[str, Any],
    batch_id: str,
    requested_ranks: Sequence[int],
) -> dict[str, Any]:
    compatibility_path, compatibility = load_ref_json(
        compatibility_ref,
        f"wave {batch_id}.compatibility_manifest",
    )
    base.exact_keys(
        compatibility,
        run_wave.COMPATIBILITY_KEYS,
        f"wave {batch_id}.compatibility_manifest",
    )
    evidence_root = Path(compatibility["evidence_root"])
    require(
        compatibility["schema_version"] == base.RUN_SCHEMA
        and evidence_root.is_absolute()
        and evidence_root.is_dir()
        and not evidence_root.is_symlink()
        and evidence_root.resolve(strict=True) == evidence_root
        and compatibility_path.is_relative_to(evidence_root)
        and compatibility["evidence_root"]
        == wave_manifest["evidence_root"]
        and compatibility["registration"] == registration_ref
        and compatibility["selection"] == selection_ref
        and compatibility["runner"]["sha256"]
        == contract.sha_file(BASE / "run_selector.py")
        and compatibility["wave_schedule"] == schedule_ref
        and compatibility["wave_selection_binding"] == selection_binding_ref
        and compatibility["wave_runner"] == wave_runner_ref
        and compatibility["wave_assembler"] == wave_assembler_ref
        and compatibility["episode_envelope"] == envelope_ref
        and compatibility["wave_envelope_binding"] == envelope_binding_ref
        and compatibility["batch_id"] == batch_id
        and compatibility["explicit_requested_ranks"]
        == list(requested_ranks)
        and compatibility["output_root"] == str(output_root)
        and compatibility["cases"] == wave_manifest["cases"]
        and [case.get("rank") for case in compatibility["cases"]]
        == list(requested_ranks),
        f"wave {batch_id} compatibility contract drift",
    )
    return compatibility


def _verify_wave_authorization_lineage(
    *,
    wave_ref: Mapping[str, Any],
    wave_path: Path,
    wave: Mapping[str, Any],
    output_root: Path,
    invocation: Mapping[str, Any],
    wave_runner_ref: Mapping[str, Any],
    wave_assembler_ref: Mapping[str, Any],
    requested: Sequence[int],
) -> dict[str, Any]:
    """Prove the immutable 1–2-rank authorization existed before execution."""

    batch_id = wave["batch_id"]
    wave_root = output_root / "waves" / batch_id
    require(
        wave_path == wave_root / "wave-receipt.json"
        and wave_path.is_file()
        and not wave_path.is_symlink()
        and contract.file_ref(wave_path) == wave_ref,
        f"wave {batch_id} receipt is outside the cumulative root",
    )
    manifest_path, manifest = load_ref_json(
        wave["wave_manifest"],
        f"wave {batch_id}.wave_manifest",
    )
    base.exact_keys(
        manifest,
        run_wave.MANIFEST_KEYS,
        f"wave {batch_id}.wave_manifest",
    )
    evidence_root = Path(manifest["evidence_root"])
    require(
        manifest["schema_version"] == contract.WAVE_MANIFEST_SCHEMA
        and evidence_root.is_absolute()
        and evidence_root.is_dir()
        and not evidence_root.is_symlink()
        and evidence_root.resolve(strict=True) == evidence_root
        and manifest_path.is_relative_to(evidence_root)
        and manifest_path.name == contract.WAVE_MANIFEST_FILENAME
        and manifest["schedule"] == invocation["schedule"]
        and manifest["schedule_contract"]
        == contract.file_ref(ROOT / "schedule_contract.py")
        and manifest["selection_binding"] == invocation["selection_binding"]
        and manifest["episode_envelope"] == invocation["episode_envelope"]
        and manifest["envelope_binding"] == invocation["envelope_binding"]
        and manifest["registration"] == invocation["registration"]
        and manifest["selection"] == invocation["selection"]
        and manifest["registered_runner"]
        == contract.file_ref(BASE / "run_selector.py")
        and manifest["wave_runner"] == wave_runner_ref
        and manifest["wave_assembler"] == wave_assembler_ref
        and manifest["compatibility_manifest"]
        == wave["compatibility_manifest"]
        and manifest["output_root"] == str(output_root)
        and type(manifest["output_root_absent_at_assembly"]) is bool
        and manifest["batch_id"] == batch_id
        and manifest["explicit_requested_ranks"] == list(requested)
        and manifest["unselected_ranks"]
        == [rank for rank in range(1, 21) if rank not in set(requested)]
        and manifest["execution_episode_keys"]
        == wave["authorized_episode_keys"]
        and manifest["same_case_serialized"] is True
        and manifest["max_parallel_cases"] == min(2, len(requested))
        and manifest["per_episode_budget_usd"] == 6.0
        and manifest["wave_maximum_budget_usd"]
        == 12.0 * len(requested)
        and manifest["selection_policy"] == "explicit_rank_arguments_only"
        and manifest["model_outputs_inspected"] is False
        and manifest["evaluator_or_outcome_accessed"] is False
        and manifest["no_spend_assertion"] == contract.NO_SPEND
        and [case.get("rank") for case in manifest["cases"]]
        == list(requested),
        f"wave {batch_id} registered manifest lineage drift",
    )
    v4_manifest_path, _ = contract.check_ref(
        manifest["v4_case_manifest"],
        f"wave {batch_id}.v4_case_manifest",
    )
    v4_receipt_path, _ = contract.check_ref(
        manifest["v4_assembly_receipt"],
        f"wave {batch_id}.v4_assembly_receipt",
    )
    v4_manifest, v4_receipt, _, _ = (
        run_wave.wave_adapter.validate_v4_inputs(
            v4_manifest_path,
            v4_receipt_path,
        )
    )
    require(
        v4_manifest["cases"] == manifest["cases"]
        and v4_manifest["execution_episode_keys"]
        == manifest["execution_episode_keys"]
        and v4_receipt["execution_episode_keys"]
        == manifest["execution_episode_keys"]
        and v4_receipt["batch_id"] == batch_id
        and v4_receipt["explicit_requested_ranks"] == list(requested),
        f"wave {batch_id} differs from approved no-spend assembly",
    )

    start_path = wave_root / "wave-start.json"
    require(
        start_path.is_file()
        and not start_path.is_symlink()
        and start_path.resolve(strict=True) == start_path,
        f"wave {batch_id} immutable start is missing",
    )
    start = base.read_json(start_path)
    base.exact_keys(
        start,
        run_wave.WAVE_START_KEYS,
        f"wave {batch_id}.start",
    )
    require(
        start["schema_version"] == run_wave.WAVE_START_SCHEMA
        and isinstance(start["started_at"], str)
        and bool(start["started_at"])
        and start["batch_id"] == batch_id
        and start["wave_manifest"] == wave["wave_manifest"]
        and start["compatibility_manifest"]
        == wave["compatibility_manifest"]
        and start["schedule"] == invocation["schedule"]
        and start["episode_envelope"] == invocation["episode_envelope"]
        and start["prior_wave_receipts"] == wave["prior_wave_receipts"]
        and start["requested_ranks"] == list(requested)
        and start["authorized_episode_keys"]
        == wave["authorized_episode_keys"]
        and start["models_authorized"] == 2 * len(requested)
        and start["maximum_budget_usd"] == 12.0 * len(requested)
        and start["same_case_serialized"] is True
        and start["max_parallel_cases"] == min(2, len(requested))
        and start["official_evaluator_invoked"] is False,
        f"wave {batch_id} pre-call authorization start drift",
    )
    return manifest


def verify_complete_run(output_root: Path) -> dict[str, Any]:
    output_root = output_root.resolve(strict=True)
    require(
        output_root.is_dir() and not output_root.is_symlink(),
        "cumulative output root invalid",
    )
    invocation_path = output_root / contract.INVOCATION_FILENAME
    invocation = base.read_json(invocation_path)
    require(
        isinstance(invocation, dict)
        and invocation.get("schema_version") == run_wave.INVOCATION_SCHEMA,
        "v7 invocation start missing or invalid",
    )
    schedule_path, schedule = load_ref_json(
        invocation.get("schedule"),
        "v7 invocation.schedule",
    )
    require(
        schedule_path.resolve()
        == (ROOT / contract.SCHEDULE_FILENAME).resolve(),
        "v7 invocation binds another execution schedule",
    )
    try:
        contract.validate_schedule(schedule, ROOT)
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    require(
        schedule["registered_files"]["verify_waves.py"]
        == contract.sha_file(Path(__file__).resolve()),
        "execution schedule does not bind this cumulative verifier",
    )

    registration_path, registration_bytes = base.check_ref(
        invocation["registration"],
        "v7 invocation.registration",
    )
    require(
        invocation["registration"]["sha256"]
        == contract.BASE_REGISTRATION_SHA256,
        "v7 invocation registration differs from frozen v4",
    )
    registration = base.read_json(registration_path)
    base.validate_registered_sources(registration)
    selection_path, _ = base.check_ref(
        invocation["selection"],
        "v7 invocation.selection",
    )
    require(
        invocation["selection"]["sha256"]
        == contract.BASE_SELECTION_SHA256,
        "v7 invocation selection differs from frozen v4",
    )
    selection = base.read_json(selection_path)
    base.validate_authoritative_selection(selection, registration_bytes)
    selection_binding_path, selection_binding = load_ref_json(
        invocation["selection_binding"],
        "v7 invocation.selection_binding",
    )
    require(
        selection_binding_path.resolve()
        == (ROOT / contract.SELECTION_BINDING_FILENAME).resolve(),
        "v7 invocation binds another selection binding",
    )
    try:
        contract.validate_selection_binding(
            selection_binding,
            schedule_sha256=invocation["schedule"]["sha256"],
            selection=selection,
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    expected_identities = base.expected_episode_identities(
        registration,
        selection,
    )
    _, envelope = load_ref_json(
        invocation["episode_envelope"],
        "v7 invocation.episode_envelope",
    )
    try:
        envelope_sessions = contract.validate_episode_envelope(
            envelope,
            selection,
            registration_ref=invocation["registration"],
            selection_ref=invocation["selection"],
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    _, envelope_binding = load_ref_json(
        invocation["envelope_binding"],
        "v7 invocation.envelope_binding",
    )
    try:
        contract.validate_envelope_binding(
            envelope_binding,
            schedule=schedule,
            schedule_ref=invocation["schedule"],
            selection_binding_ref=invocation["selection_binding"],
            envelope_ref=invocation["episode_envelope"],
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    require(
        envelope["source_commit"] == envelope_binding["assembly_source_commit"]
        and envelope["source_tree"] == envelope_binding["assembly_source_tree"],
        "v7 envelope source differs from its successor binding",
    )

    cumulative_ranks, cumulative_sessions, wave_refs = (
        run_wave._prior_wave_receipts(  # noqa: SLF001
            output_root,
            schedule_ref=invocation["schedule"],
            selection_binding_ref=invocation["selection_binding"],
            envelope_ref=invocation["episode_envelope"],
            envelope_binding_ref=invocation["envelope_binding"],
            registration_ref=invocation["registration"],
            selection_ref=invocation["selection"],
        )
    )
    require(
        cumulative_ranks == set(range(1, 21))
        and cumulative_sessions
        == {
            session
            for arms in envelope_sessions.values()
            for session in arms.values()
        }
        and len(wave_refs) >= 10
        and len(wave_refs) <= 20,
        "sealed wave chain is not the complete frozen 20/40 cohort",
    )
    sealed_waves: list[dict[str, Any]] = []
    sealed_paths: list[Path] = []
    for wave_ref in wave_refs:
        wave_path, wave = load_ref_json(wave_ref, "sealed wave receipt")
        sealed_paths.append(wave_path)
        sealed_waves.append({**wave, "self_ref": wave_ref})
    validate_sealed_wave_documents(
        sealed_waves,
        expected_identities=expected_identities,
        envelope_sessions=envelope_sessions,
    )

    wave_runner_ref = contract.file_ref(ROOT / "run_wave.py")
    wave_assembler_ref = contract.file_ref(ROOT / "assemble_wave.py")
    episode_results: list[dict[str, Any]] = []
    identities: list[tuple[int, str, str]] = []
    receipt_refs: list[dict[str, Any]] = []
    total_authorized_budget = 0.0
    consumed_keys: set[run_wave.EpisodeKey] = set()
    consumed_refs: set[tuple[str, str]] = set()
    for wave_ref, wave_path, sealed_wave in zip(
        wave_refs,
        sealed_paths,
        sealed_waves,
        strict=True,
    ):
        wave = {
            key: value
            for key, value in sealed_wave.items()
            if key != "self_ref"
        }
        batch_id = wave["batch_id"]
        requested = contract.explicit_wave_ranks(wave["requested_ranks"])
        require(
            wave["all_authorized_episodes_recorded"] is True
            and wave["worker_errors"] == []
            and len(wave["episode_receipts"]) == 2 * len(requested)
            and wave["official_evaluator_invoked"] is False,
            f"wave {batch_id} is not sealed verifier-ready",
        )
        wave_manifest = _verify_wave_authorization_lineage(
            wave_ref=wave_ref,
            wave_path=wave_path,
            wave=wave,
            output_root=output_root,
            invocation=invocation,
            wave_runner_ref=wave_runner_ref,
            wave_assembler_ref=wave_assembler_ref,
            requested=requested,
        )
        compatibility = _verify_compatibility_manifest(
            wave["compatibility_manifest"],
            output_root=output_root,
            wave_manifest=wave_manifest,
            schedule_ref=invocation["schedule"],
            envelope_ref=invocation["episode_envelope"],
            registration_ref=invocation["registration"],
            selection_ref=invocation["selection"],
            wave_runner_ref=wave_runner_ref,
            wave_assembler_ref=wave_assembler_ref,
            selection_binding_ref=invocation["selection_binding"],
            envelope_binding_ref=invocation["envelope_binding"],
            batch_id=batch_id,
            requested_ranks=requested,
        )
        authorized = {
            (item["rank"], item["case_id"], item["arm"])
            for item in wave["authorized_episode_keys"]
        }
        expected_wave = {
            identity
            for identity in expected_identities
            if identity[0] in set(requested)
        }
        require(
            authorized == expected_wave
            and len(authorized) == 2 * len(requested),
            f"wave {batch_id} authorized identity drift",
        )
        run_wave.validate_consumed_episode_refs(
            wave["episode_receipts"],
            authorized_episode_keys=wave["authorized_episode_keys"],
            compatibility_ref=wave["compatibility_manifest"],
            registration_ref=invocation["registration"],
            selection_ref=invocation["selection"],
            output_root=output_root,
            where=f"wave {batch_id}",
            seen_episode_keys=consumed_keys,
            seen_episode_refs=consumed_refs,
        )
        total_authorized_budget += wave["maximum_budget_usd"]
        for episode_ref in wave["episode_receipts"]:
            episode_path, episode = load_ref_json(
                episode_ref,
                f"wave {batch_id}.episode_receipt",
            )
            identity = (
                episode.get("rank"),
                episode.get("case_id"),
                episode.get("arm"),
            )
            require(
                identity in authorized
                and episode.get("session_id")
                == envelope_sessions[identity[0]][identity[2]]
                and episode.get("run_manifest")
                == wave["compatibility_manifest"]
                and episode.get("registration")
                == invocation["registration"]
                and episode.get("selection") == invocation["selection"],
                f"wave {batch_id} episode identity/session/contract drift",
            )
            selected = [
                case
                for case in compatibility["cases"]
                if case["rank"] == identity[0]
                and case["instance_id"] == identity[1]
            ]
            require(
                len(selected) == 1,
                f"wave {batch_id} episode absent from compatibility manifest",
            )
            result = verify_episode_with_qualification_compatibility(
                episode_path,
                compatibility=compatibility,
                registration=registration,
                schedule=schedule,
                where=f"wave {batch_id} episode",
            )
            episode_results.append(result)
            identities.append(identity)
            receipt_refs.append(dict(episode_ref))
        require(
            contract.file_ref(wave_path) == wave_ref,
            f"wave {batch_id} receipt changed during verification",
        )
    require(
        len(episode_results) == 40
        and len(set(identities)) == 40
        and set(identities) == set(expected_identities)
        and total_authorized_budget == 240.0,
        "final episode identity/count/budget ledger drift",
    )
    return {
        "schema_version": AGGREGATE_SCHEMA,
        "verified": True,
        "output_root": str(output_root),
        "schedule": invocation["schedule"],
        "episode_envelope": invocation["episode_envelope"],
        "registration": invocation["registration"],
        "selection": invocation["selection"],
        "wave_receipts": wave_refs,
        "episode_receipts": sorted(
            receipt_refs,
            key=lambda value: value["path"],
        ),
        "case_count": 20,
        "episode_count": 40,
        "per_episode_budget_usd": 6.0,
        "maximum_budget_usd": 240.0,
        "identities": [
            {"rank": rank, "case_id": case_id, "arm": arm}
            for rank, case_id, arm in expected_identities
        ],
        "all_expected_episodes_verifier_clean": True,
        "evaluation_authorized": False,
        "official_evaluator_invoked": False,
        "errors": [],
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("output_root", type=Path)
    result.add_argument(
        "--write",
        action="store_true",
        help="write the final immutable ledger only after all 40 verify cleanly",
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        result = verify_complete_run(args.output_root)
        if args.write:
            target = args.output_root.resolve(strict=True) / "final-ledger.json"
            require(
                not target.exists() and not target.is_symlink(),
                "final ledger already exists",
            )
            base.atomic_write(target, base.canonical(result))
        print(json.dumps(result, sort_keys=True, indent=2))
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
