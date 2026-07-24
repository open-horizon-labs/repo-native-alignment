#!/usr/bin/env python3
"""Fail-closed staged runner for one or two frozen issue836-v4 cases.

Preflight is read-only. Paid execution requires both ``run`` and ``--execute``.
The official evaluator is never invoked here.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from concurrent.futures import ThreadPoolExecutor, as_completed
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
from typing import Any, Iterator, Mapping, Sequence


ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
BASE = ROOT.parent / "issue827"
sys.path.insert(0, str(BASE))

import run_selector as base  # noqa: E402

import assemble_wave as wave_adapter  # noqa: E402
import schedule_contract as contract  # noqa: E402


def generate_successor_outer_seatbelt_profile(
    *,
    read_roots: Sequence[Path],
    write_roots: Sequence[Path],
    loopback_only_outbound: bool = False,
) -> str:
    """Preserve the outer boundary while permitting resolver metadata aliases."""

    if not read_roots or not write_roots:
        raise base.isolation.IsolationViolation("seatbelt_roots_empty")
    read = sorted({path.resolve(strict=True) for path in read_roots})
    write = sorted({path.resolve(strict=True) for path in write_roots})
    read_literals = set(read)
    for path in read:
        read_literals.update(path.parents)
    # macOS resolver APIs traverse these aliases even though the material they
    # reach is already authorized under /private/etc and /private/var.
    read_literals.update({Path("/etc"), Path("/var")})
    read_clauses = " ".join(
        [
            *(
                f"(subpath {base.isolation._seatbelt_literal(path)})"
                for path in read
            ),
            *(
                f"(literal {base.isolation._seatbelt_literal(path)})"
                for path in sorted(read_literals)
            ),
        ]
    )
    network_rules = ["(deny network-inbound)"]
    if loopback_only_outbound:
        network_rules.append(
            '(deny network-outbound (require-not (remote ip "localhost:*")))'
        )
    return "\n".join(
        [
            "(version 1)",
            "(allow default)",
            *network_rules,
            "(deny file-read*",
            f"  (require-not (require-any {read_clauses})))",
            "(deny file-write*",
            (
                "  (require-not "
                f"{base.isolation._seatbelt_any(write)}))"
            ),
            "",
        ]
    )


# The registered v8 runner owns this exact successor policy. Both arms receive
# the same generated profile through the unchanged issue827 execution path.
base.isolation.generate_outer_seatbelt_profile = (
    generate_successor_outer_seatbelt_profile
)

_ORIGINAL_VERIFY_ISOLATION_HOST = base.verify_isolation_host
_ORIGINAL_MATERIALIZE_HARNESS = base.materialize_harness
_ORIGINAL_ACQUIRE_TREATMENT = base.acquire_treatment
_ORIGINAL_TREATMENT_COMPLIANCE = base.treatment_compliance
_ORIGINAL_BUILD_ACTOR_TOOL_LEDGER = base.build_actor_tool_ledger


def verify_isolation_host_with_gateway_read_access(
    manifest: Mapping[str, Any],
    registration: Mapping[str, Any],
) -> dict[str, Any]:
    """Expose the pinned Docker client only to the already-hooked parent.

    Every model Bash request is still replaced by the common single-use
    gateway.  The parent must nevertheless be able to read/execute the pinned
    Docker client and connect to its forwarded Unix socket when the gateway
    launches the offline worker.
    """

    host = _ORIGINAL_VERIFY_ISOLATION_HOST(manifest, registration)
    docker_parent = Path(host["docker_binary"]).resolve(strict=True).parent
    roots = set(host["system_read_roots"])
    roots.add(str(docker_parent))
    host["system_read_roots"] = sorted(roots)
    return host


def materialize_observational_harness(
    case_root: Path,
    arm: str,
) -> dict[str, Path]:
    """Replace only the behavioral treatment gate with telemetry."""

    paths = _ORIGINAL_MATERIALIZE_HARNESS(case_root, arm)
    source = ROOT / "observational_tool_supervisor.py"
    base.require(
        source.is_file() and not source.is_symlink(),
        "observational supervisor source invalid",
    )
    destination = paths["tool_supervisor.py"]
    destination.chmod(0o755)
    shutil.copyfile(source, destination)
    destination.chmod(0o555)
    base.require(
        destination.stat().st_nlink == 1
        and base.sha_file(destination) == base.sha_file(source),
        "observational supervisor materialization failed",
    )

    manifest_path = paths["materialization"]
    manifest = base.read_json(manifest_path)
    manifest["files"]["tool_supervisor.py"] = {
        "source_sha256": base.sha_file(source),
        "destination": base.file_ref(destination),
        "mode": "0555",
        "link_count": 1,
    }
    base.atomic_write(manifest_path, base.canonical(manifest))
    manifest_path.chmod(0o444)
    return paths


def acquire_preconditioned_treatment(
    case: base.PreparedCase,
    harness_paths: Mapping[str, Path],
    evidence: Path,
    config: dict[str, Any],
) -> tuple[bytes, list[str], float, dict[str, Any]]:
    """Inject the exact issue title and its already-executed RNA result."""

    _, ids, elapsed, query_evidence = _ORIGINAL_ACQUIRE_TREATMENT(
        case,
        harness_paths,
        evidence,
        config,
    )
    projection = Path(config["initial_response"]).read_bytes()
    title = case.title.rstrip(b"\r\n")
    prefix = (ROOT / "precondition-prefix.txt").read_bytes()
    suffix = (ROOT / "precondition-suffix.txt").read_bytes().replace(
        b"__TRAVERSAL_WRAPPER__",
        str(harness_paths["rna_traverse.py"]).encode(),
    )
    system = (
        prefix
        + b"\n"
        + title
        + b"\n\nRNA RESULT FOR THAT EXACT TITLE QUERY\n"
        + projection
        + suffix
    )
    base.require(
        title
        and base.sha_bytes(title) == config["expected_query_sha256"]
        and system.startswith(prefix + b"\n" + title)
        and system.count(projection) == 1
        and b"Your FIRST actual tool call" not in system
        and b"supervisor enforces" not in system,
        "preconditioned treatment construction drift",
    )
    return system, ids, elapsed, query_evidence


def preconditioned_treatment_compliance(
    config: Mapping[str, Any],
    evidence: Path,
    arm: str,
) -> tuple[bool, list[str]]:
    """Validate injected context and any optional RNA calls, never require one."""

    if arm == "A":
        return _ORIGINAL_TREATMENT_COMPLIANCE(config, evidence, arm)

    errors: list[str] = []
    common_hooks = base.load_jsonl(Path(config["common_hook_ledger"]))
    treatment_hooks = base.load_jsonl(Path(config["hook_ledger"]))
    if any(item.get("decision") == "deny" for item in common_hooks):
        errors.append("common_supervisor_denial")
    if any(item.get("decision") == "deny" for item in treatment_hooks):
        errors.append("treatment_supervisor_denial")

    try:
        projection = Path(str(config["initial_response"])).read_bytes()
        projected_ids = base.stable_code_ids(
            projection.decode("utf-8", errors="strict")
        )
        if (
            not projected_ids
            or projected_ids != config.get("initial_ids")
            or base.sha_bytes(projection)
            != config.get("initial_response_sha256")
        ):
            errors.append("injected_query_projection_invalid")
    except (OSError, UnicodeError):
        projection = b""
        errors.append("injected_query_projection_unreadable")

    state_path = Path(str(config["state"]))
    state = base.read_json(state_path) if state_path.is_file() else {}
    if state.get("fatal"):
        errors.append("fatal_rna_state")

    receipts = sorted((evidence / "rna-events").glob("*.json"))
    loaded_receipts: list[dict[str, Any]] = []
    for index, path in enumerate(receipts):
        receipt = base.read_json(path)
        loaded_receipts.append(receipt)
        if receipt.get("classification") not in {"OK_NONEMPTY", "OK_EMPTY"}:
            errors.append(f"rna_call_{index + 1}_classification_invalid")
        if receipt.get("identity_sha256") != config["expected_identity_sha256"]:
            errors.append(f"rna_call_{index + 1}_identity_mismatch")
        if receipt.get("root") != config["root"]:
            errors.append(f"rna_call_{index + 1}_root_mismatch")

    if loaded_receipts:
        try:
            replayed = base.replay_treatment_frontier(
                projection,
                loaded_receipts,
                evidence / "rna-events",
            )
            if (
                state.get("authorization_frontier") != replayed
                or state.get("rna_calls") != len(loaded_receipts)
            ):
                errors.append("rna_authorization_frontier_state_mismatch")
        except (
            OSError,
            base.FailClosed,
            base.frontier_replay.FrontierReplayError,
        ) as exc:
            errors.append(f"rna_authorization_frontier_replay:{exc}")
    elif state.get("rna_calls") not in (None, 0):
        errors.append("rna_call_count_without_receipt")
    return not errors, errors


def build_observational_actor_tool_ledger(
    arm: str,
    common_hooks: list[dict[str, Any]],
    treatment_hooks: list[dict[str, Any]],
    query: dict[str, Any] | None,
    authorization_requested: bool,
) -> dict[str, Any]:
    ledger = _ORIGINAL_BUILD_ACTOR_TOOL_LEDGER(
        arm,
        common_hooks,
        treatment_hooks,
        query,
        authorization_requested,
    )
    command_families = {"grep": 0, "rg": 0, "sed": 0}
    for action in ledger["actions"]:
        command = action.get("bash_command")
        if not isinstance(command, str):
            continue
        for family in command_families:
            if re.search(
                rf"(?:^|[\s;|&])(?:/[^\s;|&]+/)?{family}(?:\s|$)",
                command,
            ):
                command_families[family] += 1
    ledger["observed_shell_command_family_counts"] = command_families
    ledger["successful_optional_rna_calls"] = sum(
        item.get("decision") == "observed_rna_success"
        for item in treatment_hooks
    )
    ledger["failed_optional_rna_calls"] = sum(
        item.get("decision") == "observed_rna_failure"
        for item in treatment_hooks
    )
    ledger["rna_preconditioned"] = query is not None
    return ledger


base.verify_isolation_host = verify_isolation_host_with_gateway_read_access
base.materialize_harness = materialize_observational_harness
base.acquire_treatment = acquire_preconditioned_treatment
base.treatment_compliance = preconditioned_treatment_compliance
base.build_actor_tool_ledger = build_observational_actor_tool_ledger


MANIFEST_KEYS = {
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
INVOCATION_SCHEMA = "issue836-rolling-invocation-v8"
WAVE_START_SCHEMA = "issue836-rolling-wave-start-v8"
WAVE_START_KEYS = {
    "schema_version",
    "started_at",
    "batch_id",
    "wave_manifest",
    "compatibility_manifest",
    "schedule",
    "episode_envelope",
    "prior_wave_receipts",
    "requested_ranks",
    "authorized_episode_keys",
    "models_authorized",
    "maximum_budget_usd",
    "same_case_serialized",
    "max_parallel_cases",
    "official_evaluator_invoked",
}


def fail(message: str) -> None:
    raise base.FailClosed(message)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


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


def _registered_source_ref(
    value: Any,
    where: str,
    expected: Path,
) -> dict[str, Any]:
    path, _ = base.check_ref(value, where)
    require(path.resolve() == expected.resolve(), f"{where} binds another source")
    return dict(value)


EpisodeKey = tuple[int, str, str, str]


def expected_wave_episode_keys(
    expected_identities: Sequence[tuple[int, str, str]],
    envelope_sessions: Mapping[int, Mapping[str, str]],
    requested: Sequence[int],
) -> list[dict[str, Any]]:
    selected = set(requested)
    return [
        {
            "rank": rank,
            "case_id": case_id,
            "arm": arm,
            "session_id": envelope_sessions[rank][arm],
        }
        for rank, case_id, arm in expected_identities
        if rank in selected
    ]


def _episode_key(value: Mapping[str, Any], where: str) -> EpisodeKey:
    rank = value.get("rank")
    case_id = value.get("case_id")
    arm = value.get("arm")
    session_id = value.get("session_id")
    require(
        type(rank) is int
        and 1 <= rank <= 20
        and isinstance(case_id, str)
        and bool(case_id)
        and "/" not in case_id
        and "\\" not in case_id
        and arm in {"A", "T"}
        and isinstance(session_id, str)
        and bool(session_id),
        f"{where} identity is invalid",
    )
    return rank, case_id, arm, session_id


def _authorized_episode_map(
    value: Any,
    where: str,
) -> dict[EpisodeKey, dict[str, Any]]:
    require(isinstance(value, list), f"{where} must be a list")
    result: dict[EpisodeKey, dict[str, Any]] = {}
    for index, item in enumerate(value):
        item_where = f"{where}[{index}]"
        require(isinstance(item, dict), f"{item_where} must be an object")
        base.exact_keys(
            item,
            {"rank", "case_id", "arm", "session_id"},
            item_where,
        )
        key = _episode_key(item, item_where)
        require(key not in result, f"{where} contains a duplicate identity")
        result[key] = dict(item)
    return result


def _consumed_episode_key(
    receipt: Mapping[str, Any],
    *,
    authorized: Mapping[EpisodeKey, Mapping[str, Any]],
    compatibility_ref: Mapping[str, Any],
    registration_ref: Mapping[str, Any],
    selection_ref: Mapping[str, Any],
    where: str,
) -> EpisodeKey:
    require(
        receipt.get("schema_version") == base.RECEIPT_SCHEMA,
        f"{where} schema mismatch",
    )
    key = _episode_key(receipt, where)
    require(key in authorized, f"{where} is not an authorized episode")
    require(
        receipt.get("run_manifest") == compatibility_ref
        and receipt.get("registration") == registration_ref
        and receipt.get("selection") == selection_ref,
        f"{where} registration/selection/run binding drift",
    )
    require(
        receipt.get("official_evaluator_invoked") is False,
        f"{where} has official evaluator contamination",
    )
    ledger = receipt.get("token_ledger")
    require(
        isinstance(ledger, Mapping)
        and ledger.get("model_invoked") is True,
        f"{where} did not consume its authorized model invocation",
    )
    return key


def validate_consumed_episode_refs(
    refs: Any,
    *,
    authorized_episode_keys: Any,
    compatibility_ref: Mapping[str, Any],
    registration_ref: Mapping[str, Any],
    selection_ref: Mapping[str, Any],
    output_root: Path,
    where: str,
    seen_episode_keys: set[EpisodeKey] | None = None,
    seen_episode_refs: set[tuple[str, str]] | None = None,
) -> list[EpisodeKey]:
    """Prove an exact one-to-one, model-consumed episode receipt set."""

    authorized = _authorized_episode_map(
        authorized_episode_keys,
        f"{where}.authorized_episode_keys",
    )
    require(
        isinstance(refs, list) and len(refs) == len(authorized),
        f"{where} episode receipt count differs from authorization",
    )
    local_keys: set[EpisodeKey] = set()
    local_refs: set[tuple[str, str]] = set()
    ordered: list[EpisodeKey] = []
    for index, reference in enumerate(refs):
        ref_where = f"{where}.episode_receipts[{index}]"
        path, receipt = load_ref_json(reference, ref_where)
        key = _consumed_episode_key(
            receipt,
            authorized=authorized,
            compatibility_ref=compatibility_ref,
            registration_ref=registration_ref,
            selection_ref=selection_ref,
            where=ref_where,
        )
        expected_path = (
            output_root
            / f"rank-{key[0]:02d}-{key[1]}"
            / key[2]
            / "episode-receipt.json"
        )
        require(
            path == expected_path,
            f"{ref_where} is outside its canonical cumulative path",
        )
        ref_identity = (reference["path"], reference["sha256"])
        require(
            key not in local_keys
            and ref_identity not in local_refs
            and (
                seen_episode_keys is None
                or key not in seen_episode_keys
            )
            and (
                seen_episode_refs is None
                or ref_identity not in seen_episode_refs
            ),
            f"{ref_where} reuses an episode identity or receipt",
        )
        local_keys.add(key)
        local_refs.add(ref_identity)
        ordered.append(key)
    require(
        local_keys == set(authorized),
        f"{where} consumed identities differ from authorization",
    )
    if seen_episode_keys is not None:
        seen_episode_keys.update(local_keys)
    if seen_episode_refs is not None:
        seen_episode_refs.update(local_refs)
    return ordered


def require_sealed_prior_wave(
    receipt: Mapping[str, Any],
    requested: Sequence[int],
) -> None:
    """Check completion markers; receipt-content proof is separately required."""

    require(
        receipt.get("all_authorized_episodes_recorded") is True
        and receipt.get("worker_errors") == []
        and isinstance(receipt.get("episode_receipts"), list)
        and len(receipt["episode_receipts"]) == 2 * len(requested),
        "a partial or failed prior wave blocks all further spend",
    )


@contextmanager
def execution_claim(output_root: Path) -> Iterator[None]:
    """Hold one cross-process claim; kernel release avoids stale-lock guessing."""

    parent = output_root.parent
    require(
        parent.is_dir()
        and not parent.is_symlink()
        and parent.resolve(strict=True) == parent,
        "cumulative output parent is invalid",
    )
    claim_path = parent / f".{output_root.name}.issue836-v8-execution.lock"
    flags = os.O_CREAT | os.O_RDWR
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(claim_path, flags, 0o600)
    try:
        opened = os.fstat(descriptor)
        visible = os.stat(claim_path, follow_symlinks=False)
        require(
            stat.S_ISREG(opened.st_mode)
            and (opened.st_dev, opened.st_ino)
            == (visible.st_dev, visible.st_ino),
            "execution claim path is not a stable regular file",
        )
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise base.FailClosed(
                "another v8 wave execution already holds the cumulative claim"
            ) from exc
        yield
    finally:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)


def _prior_wave_receipts(
    output_root: Path,
    *,
    schedule_ref: Mapping[str, Any],
    selection_binding_ref: Mapping[str, Any],
    envelope_ref: Mapping[str, Any],
    envelope_binding_ref: Mapping[str, Any],
    registration_ref: Mapping[str, Any],
    selection_ref: Mapping[str, Any],
) -> tuple[set[int], set[str], list[dict[str, Any]]]:
    """Validate the append-only execution chain without reading model outputs."""

    if not output_root.exists():
        return set(), set(), []
    require(
        output_root.is_dir()
        and not output_root.is_symlink()
        and output_root.resolve(strict=True) == output_root,
        "cumulative output root is invalid",
    )
    invocation_path = output_root / contract.INVOCATION_FILENAME
    require(
        invocation_path.is_file() and not invocation_path.is_symlink(),
        "cumulative output root lacks the immutable v8 invocation start",
    )
    invocation = base.read_json(invocation_path)
    base.exact_keys(
        invocation,
        {
            "schema_version",
            "started_at",
            "schedule",
            "selection_binding",
            "episode_envelope",
            "envelope_binding",
            "wave_runner",
            "wave_assembler",
            "registration",
            "selection",
            "case_count",
            "episode_count",
            "per_episode_budget_usd",
            "maximum_budget_usd",
            "max_cases_per_wave",
            "max_episodes_per_wave",
            "same_case_serialized",
            "official_evaluator_invoked",
        },
        "v8 invocation start",
    )
    require(
        invocation["schema_version"] == INVOCATION_SCHEMA
        and invocation["schedule"] == schedule_ref
        and invocation["selection_binding"] == selection_binding_ref
        and invocation["wave_runner"] == contract.file_ref(ROOT / "run_wave.py")
        and invocation["wave_assembler"]
        == contract.file_ref(ROOT / "assemble_wave.py")
        and invocation["episode_envelope"] == envelope_ref
        and invocation["envelope_binding"] == envelope_binding_ref
        and invocation["registration"] == registration_ref
        and invocation["selection"] == selection_ref
        and invocation["case_count"] == 20
        and invocation["episode_count"] == 40
        and invocation["per_episode_budget_usd"] == 6.0
        and invocation["maximum_budget_usd"] == 240.0
        and invocation["max_cases_per_wave"] == 2
        and invocation["max_episodes_per_wave"] == 4
        and invocation["same_case_serialized"] is True
        and invocation["official_evaluator_invoked"] is False,
        "v8 invocation start drift",
    )
    waves_root = output_root / "waves"
    require(
        waves_root.is_dir() and not waves_root.is_symlink(),
        "cumulative waves root is missing or invalid",
    )
    entries = sorted(waves_root.iterdir())
    require(
        all(path.is_dir() and not path.is_symlink() for path in entries),
        "cumulative waves root contains an invalid entry",
    )
    ranks: set[int] = set()
    sessions: set[str] = set()
    refs: list[dict[str, Any]] = []
    seen_episode_keys: set[EpisodeKey] = set()
    seen_episode_refs: set[tuple[str, str]] = set()
    for wave_root in entries:
        receipt_path = wave_root / "wave-receipt.json"
        require(
            receipt_path.is_file() and not receipt_path.is_symlink(),
            f"incomplete prior wave blocks all further spend: {wave_root.name}",
        )
        receipt = base.read_json(receipt_path)
        base.exact_keys(
            receipt,
            {
                "schema_version",
                "batch_id",
                "schedule",
                "episode_envelope",
                "wave_manifest",
                "compatibility_manifest",
                "prior_wave_receipts",
                "requested_ranks",
                "requested_sessions",
                "authorized_episode_keys",
                "case_count",
                "episode_count",
                "per_episode_budget_usd",
                "maximum_budget_usd",
                "cumulative_ranks",
                "cumulative_sessions",
                "cumulative_case_count",
                "cumulative_episode_count",
                "cumulative_maximum_budget_usd",
                "pending_ranks",
                "episode_receipts",
                "worker_errors",
                "all_authorized_episodes_recorded",
                "official_evaluator_invoked",
            },
            f"prior wave {wave_root.name}",
        )
        requested = contract.explicit_wave_ranks(receipt["requested_ranks"])
        requested_sessions = set(receipt["requested_sessions"])
        try:
            state = contract.next_cumulative_state(
                prior_ranks=ranks,
                prior_sessions=sessions,
                requested_ranks=requested,
                requested_sessions=requested_sessions,
            )
        except contract.ContractError as exc:
            raise base.FailClosed(str(exc)) from exc
        require(
            receipt["schema_version"] == contract.WAVE_RECEIPT_SCHEMA
            and receipt["batch_id"] == wave_root.name
            and receipt["schedule"] == schedule_ref
            and receipt["episode_envelope"] == envelope_ref
            and receipt["prior_wave_receipts"] == refs
            and receipt["case_count"] == len(requested)
            and receipt["episode_count"] == 2 * len(requested)
            and receipt["per_episode_budget_usd"] == 6.0
            and receipt["maximum_budget_usd"] == 12.0 * len(requested)
            and receipt["cumulative_ranks"] == state["cumulative_ranks"]
            and receipt["cumulative_sessions"] == state["cumulative_sessions"]
            and receipt["cumulative_case_count"] == state["cumulative_case_count"]
            and receipt["cumulative_episode_count"] == state["cumulative_episode_count"]
            and receipt["cumulative_maximum_budget_usd"]
            == state["cumulative_maximum_budget_usd"]
            and receipt["pending_ranks"] == state["pending_ranks"]
            and receipt["official_evaluator_invoked"] is False,
            f"prior wave ledger drift: {wave_root.name}",
        )
        require_sealed_prior_wave(receipt, requested)
        for key in (
            "wave_manifest",
            "compatibility_manifest",
        ):
            contract.check_ref(receipt[key], f"{wave_root.name}.{key}")
        _, prior_manifest = load_ref_json(
            receipt["wave_manifest"],
            f"{wave_root.name}.wave_manifest",
        )
        _, prior_compatibility = load_ref_json(
            receipt["compatibility_manifest"],
            f"{wave_root.name}.compatibility_manifest",
        )
        base.exact_keys(
            prior_manifest,
            MANIFEST_KEYS,
            f"{wave_root.name}.wave_manifest",
        )
        base.exact_keys(
            prior_compatibility,
            COMPATIBILITY_KEYS,
            f"{wave_root.name}.compatibility_manifest",
        )
        require(
            prior_manifest["schema_version"]
            == contract.WAVE_MANIFEST_SCHEMA
            and prior_manifest["schedule"] == schedule_ref
            and prior_manifest["schedule_contract"]
            == contract.file_ref(ROOT / "schedule_contract.py")
            and prior_manifest["selection_binding"]
            == selection_binding_ref
            and prior_manifest["episode_envelope"] == envelope_ref
            and prior_manifest["envelope_binding"]
            == envelope_binding_ref
            and prior_manifest["registration"] == registration_ref
            and prior_manifest["selection"] == selection_ref
            and prior_manifest["registered_runner"]
            == contract.file_ref(BASE / "run_selector.py")
            and prior_manifest["wave_runner"]
            == contract.file_ref(ROOT / "run_wave.py")
            and prior_manifest["wave_assembler"]
            == contract.file_ref(ROOT / "assemble_wave.py")
            and prior_manifest["compatibility_manifest"]
            == receipt["compatibility_manifest"]
            and prior_manifest["output_root"] == str(output_root)
            and type(prior_manifest["output_root_absent_at_assembly"])
            is bool
            and prior_manifest["batch_id"] == wave_root.name
            and prior_manifest["explicit_requested_ranks"]
            == list(requested)
            and prior_manifest["execution_episode_keys"]
            == receipt["authorized_episode_keys"]
            and prior_manifest["same_case_serialized"] is True
            and prior_manifest["max_parallel_cases"]
            == min(2, len(requested))
            and prior_manifest["per_episode_budget_usd"] == 6.0
            and prior_manifest["wave_maximum_budget_usd"]
            == 12.0 * len(requested)
            and prior_manifest["model_outputs_inspected"] is False
            and prior_manifest["evaluator_or_outcome_accessed"] is False
            and prior_manifest["no_spend_assertion"] == contract.NO_SPEND
            and [case.get("rank") for case in prior_manifest["cases"]]
            == list(requested),
            f"prior wave registered authorization drift: {wave_root.name}",
        )
        require(
            prior_compatibility["schema_version"] == base.RUN_SCHEMA
            and prior_compatibility["registration"] == registration_ref
            and prior_compatibility["selection"] == selection_ref
            and prior_compatibility["runner"]
            == prior_manifest["registered_runner"]
            and prior_compatibility["output_root"] == str(output_root)
            and prior_compatibility["cases"] == prior_manifest["cases"]
            and prior_compatibility["wave_schedule"] == schedule_ref
            and prior_compatibility["wave_selection_binding"]
            == selection_binding_ref
            and prior_compatibility["wave_runner"]
            == prior_manifest["wave_runner"]
            and prior_compatibility["wave_assembler"]
            == prior_manifest["wave_assembler"]
            and prior_compatibility["episode_envelope"] == envelope_ref
            and prior_compatibility["wave_envelope_binding"]
            == envelope_binding_ref
            and prior_compatibility["batch_id"] == wave_root.name
            and prior_compatibility["explicit_requested_ranks"]
            == list(requested),
            f"prior wave compatibility lineage drift: {wave_root.name}",
        )
        v4_manifest_path, _ = contract.check_ref(
            prior_manifest["v4_case_manifest"],
            f"{wave_root.name}.v4_case_manifest",
        )
        v4_receipt_path, _ = contract.check_ref(
            prior_manifest["v4_assembly_receipt"],
            f"{wave_root.name}.v4_assembly_receipt",
        )
        v4_manifest, v4_receipt, _, _ = (
            wave_adapter.validate_v4_inputs(
                v4_manifest_path,
                v4_receipt_path,
            )
        )
        require(
            v4_manifest["cases"] == prior_manifest["cases"]
            and v4_manifest["execution_episode_keys"]
            == prior_manifest["execution_episode_keys"]
            and v4_receipt["execution_episode_keys"]
            == prior_manifest["execution_episode_keys"]
            and v4_receipt["batch_id"] == wave_root.name
            and v4_receipt["explicit_requested_ranks"]
            == list(requested),
            f"prior wave approved assembly drift: {wave_root.name}",
        )
        start_path = wave_root / "wave-start.json"
        require(
            start_path.is_file()
            and not start_path.is_symlink()
            and start_path.resolve(strict=True) == start_path,
            f"prior wave start missing: {wave_root.name}",
        )
        start = base.read_json(start_path)
        base.exact_keys(
            start,
            WAVE_START_KEYS,
            f"{wave_root.name}.wave_start",
        )
        require(
            start["schema_version"] == WAVE_START_SCHEMA
            and isinstance(start["started_at"], str)
            and bool(start["started_at"])
            and start["batch_id"] == wave_root.name
            and start["wave_manifest"] == receipt["wave_manifest"]
            and start["compatibility_manifest"]
            == receipt["compatibility_manifest"]
            and start["schedule"] == schedule_ref
            and start["episode_envelope"] == envelope_ref
            and start["prior_wave_receipts"] == receipt["prior_wave_receipts"]
            and start["requested_ranks"] == list(requested)
            and start["authorized_episode_keys"]
            == receipt["authorized_episode_keys"]
            and start["models_authorized"] == 2 * len(requested)
            and start["maximum_budget_usd"] == 12.0 * len(requested)
            and start["same_case_serialized"] is True
            and start["max_parallel_cases"] == min(2, len(requested))
            and start["official_evaluator_invoked"] is False,
            f"prior wave pre-call authorization drift: {wave_root.name}",
        )
        consumed = validate_consumed_episode_refs(
            receipt["episode_receipts"],
            authorized_episode_keys=receipt["authorized_episode_keys"],
            compatibility_ref=receipt["compatibility_manifest"],
            registration_ref=invocation["registration"],
            selection_ref=invocation["selection"],
            output_root=output_root,
            where=f"prior wave {wave_root.name}",
            seen_episode_keys=seen_episode_keys,
            seen_episode_refs=seen_episode_refs,
        )
        require(
            {key[0] for key in consumed} == set(requested)
            and {key[3] for key in consumed} == requested_sessions,
            f"prior wave authorization/session drift: {wave_root.name}",
        )
        ranks.update(requested)
        sessions.update(requested_sessions)
        refs.append(contract.file_ref(receipt_path))
    return ranks, sessions, refs


def require_canonical_wave_manifest_path(
    manifest_path: Path,
    compatibility_path: Path,
) -> None:
    require(
        manifest_path
        == compatibility_path.parent / contract.WAVE_MANIFEST_FILENAME,
        "paid wave input is not the canonical assembled manifest",
    )


def verify_v4_qualification_compatibility(
    compatibility: Mapping[str, Any],
    registration: Mapping[str, Any],
    schedule: Mapping[str, Any],
    *,
    qualification_verifier: Any | None = None,
) -> None:
    """Accept only the exact registered-files-only v4 closure mismatch."""

    verifier = (
        base.verify_qualification_closure
        if qualification_verifier is None
        else qualification_verifier
    )
    try:
        verifier(compatibility, registration)
        return
    except base.FailClosed as exc:
        require(
            str(exc)
            == (
                "qualification manifest binding mismatch: "
                "registered_files_sha256"
            ),
            str(exc),
        )
    _, qualification_bytes = base.check_ref(
        compatibility["qualification_closure"]["manifest"],
        "wave compatibility qualification manifest",
    )
    try:
        qualification_manifest = json.loads(qualification_bytes)
    except json.JSONDecodeError as exc:
        raise base.FailClosed(
            f"qualification compatibility manifest invalid JSON: {exc}"
        ) from exc
    qualified_registration_path = (
        REPO / contract.QUALIFIED_REGISTRATION_RELATIVE_PATH
    )
    require(
        qualified_registration_path.is_file()
        and not qualified_registration_path.is_symlink()
        and contract.sha_file(qualified_registration_path)
        == contract.QUALIFIED_REGISTRATION_SHA256,
        "qualified predecessor registration drift",
    )
    qualified_registration = base.read_json(qualified_registration_path)
    try:
        contract.validate_qualification_compatibility(
            schedule["qualification_compatibility"],
            qualification_manifest,
            registration,
            qualified_registration,
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc


def prepare_wave(
    manifest_path: Path,
) -> tuple[
    base.PreparedRun,
    dict[str, Any],
    dict[str, Any],
    tuple[int, ...],
    list[dict[str, Any]],
]:
    manifest_path = manifest_path.resolve(strict=True)
    manifest = base.read_json(manifest_path)
    require(isinstance(manifest, dict), "wave manifest must be an object")
    base.exact_keys(manifest, MANIFEST_KEYS, "wave manifest")
    require(
        manifest["schema_version"] == contract.WAVE_MANIFEST_SCHEMA,
        "wave manifest schema mismatch",
    )
    evidence_root = Path(manifest["evidence_root"])
    require(
        evidence_root.is_absolute()
        and evidence_root.is_dir()
        and not evidence_root.is_symlink()
        and evidence_root.resolve(strict=True) == evidence_root
        and manifest_path.is_relative_to(evidence_root),
        "wave evidence root invalid",
    )
    schedule_path, schedule = load_ref_json(manifest["schedule"], "wave manifest.schedule")
    schedule_contract_ref = _registered_source_ref(
        manifest["schedule_contract"],
        "wave manifest.schedule_contract",
        ROOT / "schedule_contract.py",
    )
    del schedule_contract_ref
    try:
        contract.validate_schedule(schedule, ROOT)
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    require(
        schedule_path.resolve()
        == (ROOT / contract.SCHEDULE_FILENAME).resolve(),
        "wave manifest binds another execution schedule",
    )
    v4_manifest_path, _ = contract.check_ref(
        manifest["v4_case_manifest"],
        "wave manifest.v4_case_manifest",
    )
    v4_receipt_path, _ = contract.check_ref(
        manifest["v4_assembly_receipt"],
        "wave manifest.v4_assembly_receipt",
    )
    (
        v4_manifest,
        v4_receipt,
        _,
        _,
    ) = wave_adapter.validate_v4_inputs(
        v4_manifest_path,
        v4_receipt_path,
    )
    require(
        v4_manifest["cases"] == manifest["cases"]
        and v4_receipt["batch_id"] == manifest["batch_id"]
        and v4_receipt["explicit_requested_ranks"]
        == manifest["explicit_requested_ranks"],
        "wave manifest differs from its approved v4 assembly",
    )
    _registered_source_ref(
        manifest["wave_runner"],
        "wave manifest.wave_runner",
        Path(__file__),
    )
    _registered_source_ref(
        manifest["wave_assembler"],
        "wave manifest.wave_assembler",
        ROOT / "assemble_wave.py",
    )
    _registered_source_ref(
        manifest["registered_runner"],
        "wave manifest.registered_runner",
        BASE / "run_selector.py",
    )
    _registered_source_ref(
        manifest["common_supervisor"],
        "wave manifest.common_supervisor",
        BASE / "common_supervisor.py",
    )

    registration_path, registration_bytes = base.check_ref(
        manifest["registration"],
        "wave manifest.registration",
    )
    require(
        manifest["registration"]["sha256"] == contract.BASE_REGISTRATION_SHA256,
        "wave registration differs from frozen v4",
    )
    registration = base.read_json(registration_path)
    base.validate_registered_sources(registration)
    dimensions = base.experiment_dimensions(registration)
    require(
        dimensions["case_count"] == 20
        and dimensions["episode_count"] == 40
        and dimensions["max_parallel_cases"] == 2
        and registration["model_runtime"]["budget_usd"] == 6.0,
        "wave registration dimensions/budget drift",
    )
    selection_path, _ = base.check_ref(
        manifest["selection"],
        "wave manifest.selection",
    )
    require(
        manifest["selection"]["sha256"] == contract.BASE_SELECTION_SHA256,
        "wave selection differs from frozen v4",
    )
    selection = base.read_json(selection_path)
    base.validate_authoritative_selection(selection, registration_bytes)
    selection_binding_path, selection_binding = load_ref_json(
        manifest["selection_binding"],
        "wave manifest.selection_binding",
    )
    require(
        selection_binding_path.resolve()
        == (ROOT / contract.SELECTION_BINDING_FILENAME).resolve(),
        "wave manifest binds another selection binding",
    )
    try:
        contract.validate_selection_binding(
            selection_binding,
            schedule_sha256=manifest["schedule"]["sha256"],
            selection=selection,
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    expected_full = base.expected_episode_identities(registration, selection)

    envelope_path, envelope = load_ref_json(
        manifest["episode_envelope"],
        "wave manifest.episode_envelope",
    )
    del envelope_path
    try:
        envelope_sessions = contract.validate_episode_envelope(
            envelope,
            selection,
            registration_ref=manifest["registration"],
            selection_ref=manifest["selection"],
        )
        contract.check_ref(
            envelope["assembler"],
            "wave envelope.assembler",
        )
        ranks = contract.explicit_wave_ranks(
            manifest["explicit_requested_ranks"]
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    _, envelope_binding = load_ref_json(
        manifest["envelope_binding"],
        "wave manifest.envelope_binding",
    )
    try:
        contract.validate_envelope_binding(
            envelope_binding,
            schedule=schedule,
            schedule_ref=manifest["schedule"],
            selection_binding_ref=manifest["selection_binding"],
            envelope_ref=manifest["episode_envelope"],
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    require(
        envelope["source_commit"] == envelope_binding["assembly_source_commit"]
        and envelope["source_tree"] == envelope_binding["assembly_source_tree"],
        "wave envelope source differs from its successor binding",
    )
    require(
        manifest["unselected_ranks"]
        == [rank for rank in range(1, 21) if rank not in set(ranks)],
        "wave unselected ranks drift",
    )
    requested_episode_keys = [
        {
            "rank": rank,
            "case_id": case_id,
            "arm": arm,
            "session_id": envelope_sessions[rank][arm],
        }
        for rank, case_id, arm in expected_full
        if rank in set(ranks)
    ]
    require(
        manifest["execution_episode_keys"] == requested_episode_keys
        and manifest["same_case_serialized"] is True
        and manifest["max_parallel_cases"] == min(2, len(ranks))
        and manifest["per_episode_budget_usd"] == 6.0
        and manifest["wave_maximum_budget_usd"] == 12.0 * len(ranks)
        and manifest["selection_policy"] == "explicit_rank_arguments_only"
        and manifest["model_outputs_inspected"] is False
        and manifest["evaluator_or_outcome_accessed"] is False
        and manifest["no_spend_assertion"] == contract.NO_SPEND,
        "wave scope/order/budget/no-spend contract drift",
    )
    require(
        isinstance(manifest["batch_id"], str)
        and re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}",
            manifest["batch_id"],
        )
        is not None,
        "wave batch ID invalid",
    )
    output_root = Path(manifest["output_root"])
    require(
        output_root.is_absolute()
        and output_root.is_relative_to(evidence_root)
        and output_root.resolve(strict=False) == output_root
        and not output_root.is_symlink()
        and type(manifest["output_root_absent_at_assembly"]) is bool
        and manifest["output_root_absent_at_assembly"]
        is (not output_root.exists()),
        "wave cumulative output root invalid",
    )

    compatibility_path, compatibility = load_ref_json(
        manifest["compatibility_manifest"],
        "wave manifest.compatibility_manifest",
    )
    require_canonical_wave_manifest_path(
        manifest_path,
        compatibility_path,
    )
    base.exact_keys(
        compatibility,
        COMPATIBILITY_KEYS,
        "wave compatibility manifest",
    )
    require(
        compatibility["schema_version"] == base.RUN_SCHEMA
        and compatibility["evidence_root"] == manifest["evidence_root"]
        and compatibility["registration"] == manifest["registration"]
        and compatibility["selection"] == manifest["selection"]
        and compatibility["runner"] == manifest["registered_runner"]
        and compatibility["common_supervisor"] == manifest["common_supervisor"]
        and compatibility["claude"] == manifest["claude"]
        and compatibility["rna_artifact"] == manifest["rna_artifact"]
        and compatibility["mcp_config"] == manifest["mcp_config"]
        and compatibility["qualification_closure"]
        == manifest["qualification_closure"]
        and compatibility["isolation"] == manifest["isolation"]
        and compatibility["output_root"] == manifest["output_root"]
        and compatibility["cases"] == manifest["cases"]
        and compatibility["wave_schedule"] == manifest["schedule"]
        and compatibility["wave_schedule_contract"]
        == manifest["schedule_contract"]
        and compatibility["wave_selection_binding"]
        == manifest["selection_binding"]
        and compatibility["wave_runner"] == manifest["wave_runner"]
        and compatibility["wave_assembler"] == manifest["wave_assembler"]
        and compatibility["episode_envelope"] == manifest["episode_envelope"]
        and compatibility["wave_envelope_binding"]
        == manifest["envelope_binding"]
        and compatibility["batch_id"] == manifest["batch_id"]
        and compatibility["explicit_requested_ranks"] == list(ranks),
        "wave compatibility manifest drift",
    )

    claude_path, claude_version = base.verify_runtime(
        compatibility,
        registration,
    )
    launcher_path, binary_path, rna_refs = base.verify_rna_artifact(
        compatibility,
        registration,
    )
    trusted_rna_toolchain_root = base.trusted_rna_toolchain_read_root(
        rna_refs["runtime_receipt"]
    )
    verify_v4_qualification_compatibility(
        compatibility,
        registration,
        schedule,
    )
    mcp_path, mcp_bytes = base.check_ref(
        compatibility["mcp_config"],
        "wave compatibility manifest.mcp_config",
    )
    require(
        mcp_bytes == base.EMPTY_MCP_BYTES
        and compatibility["mcp_config"]["sha256"]
        == registration["model_runtime"]["strict_empty_mcp_sha256"],
        "wave MCP config differs from registered strict-empty bytes",
    )
    isolation_host = base.verify_isolation_host(
        compatibility,
        registration,
    )

    selected_by_rank = {
        case["rank"]: case for case in selection["cases"]
    }
    case_values = manifest["cases"]
    require(
        isinstance(case_values, list)
        and len(case_values) == len(ranks)
        and [case.get("rank") for case in case_values] == list(ranks),
        "wave materialized cases differ from explicit rank scope",
    )
    cases: list[base.PreparedCase] = []
    sessions_seen: set[str] = set()
    checkouts: set[Path] = set()
    for index, case in enumerate(case_values):
        chosen = selected_by_rank[ranks[index]]
        where = f"wave manifest.cases[{index}]"
        base.exact_keys(
            case,
            {
                "rank",
                "instance_id",
                "base_commit",
                "base_tree",
                "problem_statement",
                "user_prompt",
                "cache",
                "arm_order",
                "arms",
                "isolation_worker",
            },
            where,
        )
        require(
            case["rank"] == chosen["rank"]
            and case["instance_id"] == chosen["instance_id"]
            and case["base_commit"] == chosen["base_commit"]
            and case["base_tree"] == chosen["base_tree"]
            and case["arm_order"] == chosen["arm_order"],
            f"{where} differs from frozen selection",
        )
        case_id = case["instance_id"]
        problem_path, problem = base.check_ref(
            case["problem_statement"],
            f"{where}.problem_statement",
        )
        del problem_path
        require(
            base.sha_bytes(problem) == chosen["problem_statement_sha256"],
            f"{where} problem statement mismatch",
        )
        _, prompt = base.check_ref(case["user_prompt"], f"{where}.user_prompt")
        require(
            prompt.count(problem) == 1 and prompt.endswith(problem),
            f"{where} prompt construction mismatch",
        )
        (
            index_checkout,
            cache_root,
            cache_refs,
            cache_bindings,
            cache_inventory,
            expected_repository,
            live_repository,
        ) = base.verify_cache(
            case["cache"],
            case_id=case_id,
            commit=case["base_commit"],
            tree=case["base_tree"],
            registration=registration,
        )
        require(index_checkout not in checkouts, f"checkout reused: {index_checkout}")
        checkouts.add(index_checkout)
        arms = case["arms"]
        base.exact_keys(arms, {"A", "T"}, f"{where}.arms")
        arm_checkouts: dict[str, Path] = {}
        arm_sessions: dict[str, str] = {}
        for arm in ("A", "T"):
            base.exact_keys(
                arms[arm],
                {"checkout", "session_id"},
                f"{where}.arms.{arm}",
            )
            checkout = base.verify_model_checkout(
                arms[arm]["checkout"],
                case["base_commit"],
                case["base_tree"],
                f"{where}.arms.{arm}.checkout",
            )
            session = arms[arm]["session_id"]
            require(
                session == envelope_sessions[case["rank"]][arm],
                f"{where}.arms.{arm}.session differs from full envelope",
            )
            require(
                checkout not in checkouts
                and session not in sessions_seen
                and not base.find_transcripts(session),
                f"{where}.arms.{arm} checkout/session is not fresh",
            )
            checkouts.add(checkout)
            sessions_seen.add(session)
            arm_checkouts[arm] = checkout
            arm_sessions[arm] = session
        isolation_worker = base.verify_isolation_worker(
            case["isolation_worker"],
            case_id=case_id,
            commit=case["base_commit"],
            tree=case["base_tree"],
            host=isolation_host,
        )
        cases.append(
            base.PreparedCase(
                rank=case["rank"],
                case_id=case_id,
                base_commit=case["base_commit"],
                base_tree=case["base_tree"],
                problem=problem,
                prompt=prompt,
                title=base.title_bytes(problem),
                index_checkout=index_checkout,
                root=cache_root,
                cache_refs=cache_refs,
                cache_bindings=cache_bindings,
                cache_inventory_sha256=cache_inventory,
                expected_repository_identity=expected_repository,
                live_repository_identity=live_repository,
                arm_order=tuple(case["arm_order"]),
                checkouts=arm_checkouts,
                sessions=arm_sessions,
                isolation_worker=isolation_worker,
            )
        )
    require(
        tuple(
            (case.rank, case.case_id, arm)
            for case in cases
            for arm in case.arm_order
        )
        == tuple(
            identity for identity in expected_full if identity[0] in set(ranks)
        ),
        "wave prepared episode identities/order drift",
    )
    prepared = base.PreparedRun(
        manifest_path=compatibility_path,
        manifest_ref=contract.file_ref(compatibility_path),
        registration_path=registration_path,
        registration_ref=dict(manifest["registration"]),
        registration=registration,
        selection_path=selection_path,
        selection_ref=dict(manifest["selection"]),
        selection=selection,
        claude_path=claude_path,
        claude_version=claude_version,
        launcher_path=launcher_path,
        binary_path=binary_path,
        rna_refs=rna_refs,
        trusted_rna_toolchain_root=trusted_rna_toolchain_root,
        mcp_path=mcp_path,
        output_root=output_root,
        cases=tuple(cases),
        isolation_host=isolation_host,
    )
    return prepared, manifest, envelope, ranks, requested_episode_keys


def preflight_summary(
    prepared: base.PreparedRun,
    manifest: Mapping[str, Any],
    ranks: Sequence[int],
    episode_keys: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    prior_ranks, prior_sessions, prior_refs = _prior_wave_receipts(
        prepared.output_root,
        schedule_ref=manifest["schedule"],
        selection_binding_ref=manifest["selection_binding"],
        envelope_ref=manifest["episode_envelope"],
        envelope_binding_ref=manifest["envelope_binding"],
        registration_ref=manifest["registration"],
        selection_ref=manifest["selection"],
    )
    requested_sessions = {
        session
        for case in prepared.cases
        for session in case.sessions.values()
    }
    try:
        cumulative = contract.next_cumulative_state(
            prior_ranks=prior_ranks,
            prior_sessions=prior_sessions,
            requested_ranks=ranks,
            requested_sessions=requested_sessions,
        )
    except contract.ContractError as exc:
        raise base.FailClosed(str(exc)) from exc
    require(
        manifest["batch_id"]
        not in {
            Path(reference["path"]).parent.name
            for reference in prior_refs
        },
        "wave batch ID already exists",
    )
    prior_batch_ids = [
        Path(reference["path"]).parent.name
        for reference in prior_refs
    ]
    require(
        not prior_batch_ids
        or manifest["batch_id"] > max(prior_batch_ids),
        "wave batch ID must be lexically after every prior wave",
    )
    return {
        "schema_version": "issue836-rolling-wave-preflight-v8",
        "status": "READY_TO_EXECUTE_WAVE",
        "wave_manifest": contract.file_ref(
            Path(manifest["compatibility_manifest"]["path"]).parent
            / contract.WAVE_MANIFEST_FILENAME
        ),
        "compatibility_manifest": prepared.manifest_ref,
        "schedule": manifest["schedule"],
        "episode_envelope": manifest["episode_envelope"],
        "batch_id": manifest["batch_id"],
        "requested_ranks": list(ranks),
        "requested_episode_keys": list(episode_keys),
        "case_count": len(ranks),
        "episode_count": 2 * len(ranks),
        "per_episode_budget_usd": 6.0,
        "maximum_budget_usd": 12.0 * len(ranks),
        "prior_wave_receipts": prior_refs,
        **cumulative,
        "same_case_serialized": True,
        "max_parallel_cases": min(2, len(ranks)),
        "models_launched": 0,
        "provider_requests": 0,
        "official_evaluator_invoked": False,
    }


def execute_case_once(
    prepared: base.PreparedRun,
    case: base.PreparedCase,
) -> tuple[list[dict[str, Any]], list[str]]:
    """Run a same-case pair, stopping before arm two after a zero-model failure."""

    case_root = (
        prepared.output_root / f"rank-{case.rank:02d}-{case.case_id}"
    )
    case_root.mkdir(parents=True, exist_ok=False)
    receipts: list[dict[str, Any]] = []
    errors: list[str] = []
    authorized = _authorized_episode_map(
        [
            {
                "rank": case.rank,
                "case_id": case.case_id,
                "arm": arm,
                "session_id": case.sessions[arm],
            }
            for arm in case.arm_order
        ],
        f"rank {case.rank} authorization",
    )
    for arm in case.arm_order:
        try:
            harness_paths = base.materialize_harness(case_root, arm)
            receipt = base.launch_episode(
                prepared,
                case,
                arm,
                case_root,
                harness_paths,
            )
            receipts.append(receipt)
            if not (
                isinstance(receipt.get("token_ledger"), Mapping)
                and receipt["token_ledger"].get("model_invoked") is True
            ):
                errors.append(
                    f"{case.case_id}/{arm}: "
                    "retryable_pre_model_failure_not_consumed"
                )
                break
            try:
                _consumed_episode_key(
                    receipt,
                    authorized=authorized,
                    compatibility_ref=prepared.manifest_ref,
                    registration_ref=prepared.registration_ref,
                    selection_ref=prepared.selection_ref,
                    where=f"{case.case_id}/{arm}",
                )
            except base.FailClosed as exc:
                errors.append(f"{case.case_id}/{arm}: {exc}")
                break
        except Exception as exc:
            errors.append(
                f"{case.case_id}/{arm}: {type(exc).__name__}: {exc}"
            )
            # Same-case serialization is frozen. A setup failure does not
            # authorize launching the following arm.
            break
    return receipts, errors


def execute_wave(
    prepared: base.PreparedRun,
    manifest: Mapping[str, Any],
    summary: Mapping[str, Any],
) -> int:
    output_root = prepared.output_root
    if not output_root.exists():
        output_root.mkdir(parents=True, exist_ok=False)
        base.atomic_write(
            output_root / contract.INVOCATION_FILENAME,
            base.canonical(
                {
                    "schema_version": INVOCATION_SCHEMA,
                    "started_at": base.utc_now(),
                    "schedule": manifest["schedule"],
                    "selection_binding": manifest["selection_binding"],
                    "episode_envelope": manifest["episode_envelope"],
                    "envelope_binding": manifest["envelope_binding"],
                    "wave_runner": manifest["wave_runner"],
                    "wave_assembler": manifest["wave_assembler"],
                    "registration": manifest["registration"],
                    "selection": manifest["selection"],
                    "case_count": 20,
                    "episode_count": 40,
                    "per_episode_budget_usd": 6.0,
                    "maximum_budget_usd": 240.0,
                    "max_cases_per_wave": 2,
                    "max_episodes_per_wave": 4,
                    "same_case_serialized": True,
                    "official_evaluator_invoked": False,
                }
            ),
        )
        (output_root / "waves").mkdir(exist_ok=False)
    wave_root = output_root / "waves" / manifest["batch_id"]
    require(
        not wave_root.exists() and not wave_root.is_symlink(),
        "wave batch already exists",
    )
    for case in prepared.cases:
        require(
            not (
                output_root
                / f"rank-{case.rank:02d}-{case.case_id}"
            ).exists(),
            f"rank {case.rank} already has output; refusing duplicate spend",
        )
    wave_root.mkdir(exist_ok=False)
    start = {
        "schema_version": WAVE_START_SCHEMA,
        "started_at": base.utc_now(),
        "batch_id": manifest["batch_id"],
        "wave_manifest": contract.file_ref(
            Path(manifest["compatibility_manifest"]["path"]).parent
            / contract.WAVE_MANIFEST_FILENAME
        ),
        "compatibility_manifest": prepared.manifest_ref,
        "schedule": manifest["schedule"],
        "episode_envelope": manifest["episode_envelope"],
        "prior_wave_receipts": summary["prior_wave_receipts"],
        "requested_ranks": summary["requested_ranks"],
        "authorized_episode_keys": summary["requested_episode_keys"],
        "models_authorized": summary["episode_count"],
        "maximum_budget_usd": summary["maximum_budget_usd"],
        "same_case_serialized": True,
        "max_parallel_cases": summary["max_parallel_cases"],
        "official_evaluator_invoked": False,
    }
    base.atomic_write(wave_root / "wave-start.json", base.canonical(start))
    receipts: list[dict[str, Any]] = []
    errors: list[str] = []
    with ThreadPoolExecutor(
        max_workers=summary["max_parallel_cases"],
        thread_name_prefix="issue836-v8",
    ) as executor:
        futures = {
            executor.submit(execute_case_once, prepared, case): case
            for case in prepared.cases
        }
        for future in as_completed(futures):
            case = futures[future]
            try:
                case_receipts, case_errors = future.result()
                receipts.extend(case_receipts)
                errors.extend(case_errors)
            except Exception as exc:
                errors.append(
                    f"{case.case_id}: {type(exc).__name__}: {exc}"
                )
    receipt_refs = sorted(
        (receipt["episode_receipt"] for receipt in receipts),
        key=lambda value: value["path"],
    )
    result = {
        "schema_version": contract.WAVE_RECEIPT_SCHEMA,
        "batch_id": manifest["batch_id"],
        "schedule": manifest["schedule"],
        "episode_envelope": manifest["episode_envelope"],
        "wave_manifest": start["wave_manifest"],
        "compatibility_manifest": prepared.manifest_ref,
        "prior_wave_receipts": summary["prior_wave_receipts"],
        "requested_ranks": summary["requested_ranks"],
        "requested_sessions": sorted(
            session
            for case in prepared.cases
            for session in case.sessions.values()
        ),
        "authorized_episode_keys": summary["requested_episode_keys"],
        "case_count": summary["case_count"],
        "episode_count": summary["episode_count"],
        "per_episode_budget_usd": 6.0,
        "maximum_budget_usd": summary["maximum_budget_usd"],
        "cumulative_ranks": summary["cumulative_ranks"],
        "cumulative_sessions": summary["cumulative_sessions"],
        "cumulative_case_count": summary["cumulative_case_count"],
        "cumulative_episode_count": summary["cumulative_episode_count"],
        "cumulative_maximum_budget_usd": summary[
            "cumulative_maximum_budget_usd"
        ],
        "pending_ranks": summary["pending_ranks"],
        "episode_receipts": receipt_refs,
        "worker_errors": sorted(errors),
        "all_authorized_episodes_recorded": (
            not errors
            and len(receipts) == summary["episode_count"]
            and all(
                isinstance(receipt.get("token_ledger"), Mapping)
                and receipt["token_ledger"].get("model_invoked") is True
                for receipt in receipts
            )
        ),
        "official_evaluator_invoked": False,
    }
    base.atomic_write(wave_root / "wave-receipt.json", base.canonical(result))
    print(json.dumps(result, sort_keys=True, indent=2))
    return 0 if result["all_authorized_episodes_recorded"] else 1


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="command", required=True)
    preflight = sub.add_parser("preflight", help="read-only wave validation")
    preflight.add_argument("--manifest", type=Path, required=True)
    run = sub.add_parser(
        "run",
        help="preflight and optionally execute one registered wave",
    )
    run.add_argument("--manifest", type=Path, required=True)
    run.add_argument(
        "--execute",
        action="store_true",
        help="authorize only the selected 2 or 4 paid episodes",
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        prepared, manifest, _, ranks, episode_keys = prepare_wave(args.manifest)
        summary = preflight_summary(
            prepared,
            manifest,
            ranks,
            episode_keys,
        )
        print(json.dumps(summary, sort_keys=True, indent=2))
        if args.command == "preflight" or not args.execute:
            if args.command == "run":
                print(
                    "DRY RUN ONLY: add --execute to launch this one wave",
                    file=sys.stderr,
                )
            return 0
        # The initial pass is useful diagnostic output only. Re-read every
        # input and the cumulative chain while holding the single paid claim,
        # then keep that claim through the immutable wave receipt.
        with execution_claim(prepared.output_root):
            (
                locked_prepared,
                locked_manifest,
                _,
                locked_ranks,
                locked_episode_keys,
            ) = prepare_wave(args.manifest)
            locked_summary = preflight_summary(
                locked_prepared,
                locked_manifest,
                locked_ranks,
                locked_episode_keys,
            )
            return execute_wave(
                locked_prepared,
                locked_manifest,
                locked_summary,
            )
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
