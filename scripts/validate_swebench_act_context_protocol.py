#!/usr/bin/env python3
"""Offline, fail-closed validator for the frozen SWE-bench ActContext protocol.

This module intentionally uses only Python's standard library and performs no
network, subprocess, model, evaluator, or credential access. A future runner
must call it with the externally anchored bundle digest before it reads an API
key or dispatches a model request.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any


PROTOCOL_REL = Path("benchmark/swebench-act-context/protocol.json")
POPULATION_REL = Path("benchmark/swebench-act-context/population.json")
RUNTIME_REL = Path("benchmark/swebench-act-context/runtime-config.json")
VECTOR_REL = Path("benchmark/swebench-act-context/packet-vector.json")
LOCK_REL = Path("benchmark/swebench-act-context/protocol.lock.json")
DIGEST_REL = Path("benchmark/swebench-act-context/protocol.sha256")
PARSER_REL = Path("benchmark/swebench-act-context/upstream/edit_patch_v2.py")

EXPECTED_LOCK_PATHS = (
    "benchmark/swebench-act-context/README.md",
    "benchmark/swebench-act-context/packet-vector.json",
    "benchmark/swebench-act-context/population.json",
    "benchmark/swebench-act-context/protocol.json",
    "benchmark/swebench-act-context/runtime-config.json",
    "benchmark/swebench-act-context/upstream/LICENSE",
    "benchmark/swebench-act-context/upstream/edit_patch_v2.py",
    "scripts/tests/test_swebench_act_context_protocol.py",
    "scripts/validate_swebench_act_context_protocol.py",
)
LOCK_MATERIAL_FORMAT = "<sha256> <byte-count> <repo-relative-path> LF, sorted by path"
PACKET_METADATA_FIELDS = ("instance_id", "protocol_id", "record_count")
PACKET_HEADER_FIELDS = (
    "kind",
    "ordinal",
    "stable_id",
    "path",
    "start_line",
    "end_line",
    "language",
    "full_body_byte_length",
    "full_body_sha256",
    "score",
    "relationships",
)
PACKET_RELATIONSHIP_FIELDS = (
    "source",
    "target",
    "edge_type",
    "direction",
    "locus_ordinal",
    "cli_ordinal",
)
DIRECTION_ORDINAL = {"incoming": 1, "outgoing": 2}
EDGE_TYPE_ORDINAL = {
    name: ordinal
    for ordinal, name in enumerate(
        (
            "Calls",
            "ReferencedBy",
            "Imports",
            "DependsOn",
            "Implements",
            "Extends",
            "Defines",
            "Contains",
            "Tests",
            "Other",
        ),
        1,
    )
}

EXPECTED_PROTOCOL_SHA256 = "a6a590c14811dfb2616c4557753aa3bddb61ed02f90fe90ea10361e7e1c6fa63"
EXPECTED_POPULATION_SHA256 = "067a5589b4cdb34c5fbd81bb6ff7ff6ede4dbfc26694758fafbef3544f9e6acf"
EXPECTED_PARSER_SHA256 = "68b44b5b39ff7fbf3e7417b4f16f0c37513a4cd7a96be8ba00611c825f462c2e"
EXPECTED_UPSTREAM_COMMIT = "fd115351d0ab742993aa5d7006f1369fb15b6e74"
EXPECTED_DATASET_REVISION = "c104f840cc67f8b6eec6f759ebc8b2693d585d4a"
EXPECTED_EXCLUSION = "astropy__astropy-8707"
ORDER_SEED = "rna-act-context-bc-order-v1"
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
SECRET_VALUE = re.compile(
    r"(?:sk-ant-[A-Za-z0-9_-]{8,}|(?:org|acct|account|workspace|wrkspc)_[A-Za-z0-9_-]{8,})"
)


class DuplicateKey(ValueError):
    """Raised when JSON contains a duplicate object key."""


def _object_no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKey(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_object_no_duplicates)


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def _require(errors: list[str], condition: bool, message: str) -> None:
    if not condition:
        errors.append(message)


def validate_protocol(protocol: dict[str, Any], errors: list[str]) -> None:
    _require(errors, protocol.get("schema_version") == 1, "protocol schema_version must be 1")
    _require(errors, protocol.get("status") == "frozen", "protocol status must be frozen")
    upstream = protocol.get("references", {}).get("upstream", {})
    _require(errors, upstream.get("commit") == EXPECTED_UPSTREAM_COMMIT, "upstream commit drift")

    request = protocol.get("instrument", {}).get("request", {})
    _require(errors, request.get("method") == "client.messages.create", "model API method drift")
    _require(errors, request.get("requested_model") == "claude-sonnet-4-6", "requested model drift")
    _require(
        errors,
        request.get("resolved_model_check")
        == "response.model must equal claude-sonnet-4-6 exactly",
        "resolved-model policy drift",
    )
    _require(errors, request.get("temperature") == 0.0, "temperature drift")
    _require(errors, request.get("max_tokens") == 8000, "max_tokens drift")
    _require(errors, request.get("system") is None, "system prompt must remain absent")

    prompt = protocol.get("instrument", {}).get("prompt", {})
    prompt_text = prompt.get("template")
    _require(errors, isinstance(prompt_text, str), "prompt template must be text")
    if isinstance(prompt_text, str):
        _require(
            errors,
            sha256_bytes(prompt_text.encode("utf-8")) == prompt.get("template_sha256"),
            "prompt template hash mismatch",
        )
    retry = protocol.get("instrument", {}).get("retry", {})
    retry_text = retry.get("retry_suffix")
    _require(errors, retry.get("maximum_feedback_rounds") == 2, "edit-feedback retry count drift")
    if isinstance(retry_text, str):
        _require(
            errors,
            sha256_bytes(retry_text.encode("utf-8")) == retry.get("retry_suffix_sha256"),
            "retry suffix hash mismatch",
        )
    else:
        errors.append("retry suffix must be text")

    parser = protocol.get("instrument", {}).get("parser", {})
    _require(errors, parser.get("reference_sha256") == EXPECTED_PARSER_SHA256, "parser pin drift")

    evaluator = protocol.get("evaluator", {})
    _require(errors, evaluator.get("package") == "swebench", "evaluator package drift")
    _require(errors, evaluator.get("version") == "4.1.0", "SWE-bench version drift")
    _require(
        errors,
        evaluator.get("entrypoint") == "python -m swebench.harness.run_evaluation",
        "evaluator entrypoint drift",
    )
    _require(errors, evaluator.get("max_workers") == 1, "evaluator parallelism drift")

    dataset = protocol.get("dataset", {})
    _require(errors, dataset.get("revision") == EXPECTED_DATASET_REVISION, "dataset revision drift")
    _require(
        errors,
        dataset.get("population_sha256") == EXPECTED_POPULATION_SHA256,
        "protocol population digest drift",
    )

    arms = protocol.get("arms", {})
    _require(errors, arms.get("A", {}).get("execute") is False, "A must never be rerun")
    _require(errors, arms.get("A", {}).get("reported_n") == 70, "A population drift")
    _require(errors, arms.get("A", {}).get("resolved") == 19, "A outcome total drift")
    _require(errors, protocol.get("rna_acquisition", {}).get("access") == "CLI only; MCP is forbidden", "RNA access must remain CLI-only")
    _require(errors, protocol.get("rna_acquisition", {}).get("business_context", "").startswith("disabled;"), "business context must remain disabled")

    packet = protocol.get("packet_serialization", {})
    _require(
        errors,
        packet.get("metadata_schema", {}).get("exact_fields") == list(PACKET_METADATA_FIELDS),
        "packet metadata schema drift",
    )
    _require(
        errors,
        packet.get("header_schema", {}).get("base_exact_fields") == list(PACKET_HEADER_FIELDS),
        "packet header schema drift",
    )
    _require(
        errors,
        packet.get("relationship_schema", {}).get("exact_fields")
        == list(PACKET_RELATIONSHIP_FIELDS),
        "packet relationship schema drift",
    )
    _require(
        errors,
        "locus_ordinal" in packet.get("relationship_schema", {}).get("ordering", ""),
        "packet relationship ordering drift",
    )
    _require(
        errors,
        "final line of the same payload"
        in protocol.get("definitions", {}).get("minified_body", ""),
        "minifier legend framing drift",
    )

    h1 = protocol.get("hypotheses", {}).get("H1", {})
    _require(errors, "at least 40% fewer" in h1.get("claim", ""), "H1 efficiency threshold drift")
    _require(errors, "frozen N=70 divided by resolved_count" in h1.get("claim", ""), "H1 denominator drift")
    _require(errors, "greater than -0.10" in h1.get("quality_gate", ""), "H1 non-inferiority drift")
    h2 = protocol.get("hypotheses", {}).get("H2", {})
    _require(errors, "at least 30% fewer" in h2.get("claim", ""), "H2 ActContext threshold drift")
    _require(errors, "at least 20% fewer" in h2.get("claim", ""), "H2 total threshold drift")
    _require(errors, "greater than -0.10" in h2.get("quality_gate", ""), "H2 non-inferiority drift")

    compatibility = protocol.get("compatibility", {})
    not_comparable = set(compatibility.get("not_comparable", []))
    _require(errors, "A retry-inclusive input tokens" in not_comparable, "A retry-token null was removed")
    _require(errors, "A total episode input tokens" in not_comparable, "A episode-token null was removed")
    _require(errors, "never reconstruct or fabricate" in compatibility.get("on_total_episode_h1_request", ""), "missing total-episode methodology stop")

    validator_policy = protocol.get("validator_policy", {})
    rejected = " ".join(validator_policy.get("must_reject", []))
    _require(errors, validator_policy.get("offline") is True, "validator must remain offline")
    _require(errors, validator_policy.get("network_calls") is False, "validator network policy drift")
    _require(errors, "non-null A retry-inclusive" in rejected, "validator no longer rejects inferred A retry totals")
    _require(errors, "missing A initial-request" in rejected, "validator no longer blocks missing A initial counts")


def validate_population(population: dict[str, Any], errors: list[str]) -> None:
    selection = population.get("selection", {})
    instances = population.get("instances", [])
    _require(errors, selection.get("n_selected") == 71, "selected population must remain N=71")
    _require(errors, selection.get("n_included") == 70, "included population must remain N=70")
    _require(errors, selection.get("excluded_by_gold") == [EXPECTED_EXCLUSION], "gold exclusion drift")
    _require(errors, isinstance(instances, list) and len(instances) == 71, "population must have 71 rows")
    if not isinstance(instances, list):
        return

    identifiers = [row.get("instance_id") for row in instances if isinstance(row, dict)]
    _require(errors, len(set(identifiers)) == 71, "instance IDs must be unique")
    included = [row for row in instances if row.get("included") is True]
    excluded = [row for row in instances if row.get("included") is False]
    _require(errors, len(included) == 70, "exactly 70 rows must be included")
    _require(errors, len(excluded) == 1 and excluded[0].get("instance_id") == EXPECTED_EXCLUSION, "only astropy__astropy-8707 may be excluded")
    if excluded:
        _require(errors, excluded[0].get("upstream_a") is None, "excluded row must not have A evidence")

    resolved = 0
    initial_total = 0
    feedback_rounds = 0
    for row in included:
        iid = row.get("instance_id", "<missing>")
        _require(errors, bool(HEX40.fullmatch(str(row.get("base_commit", "")))), f"{iid}: invalid base commit")
        for field in (
            "dataset_row_sha256",
            "problem_statement_sha256",
            "gold_patch_sha256",
            "test_patch_sha256",
        ):
            _require(errors, bool(HEX64.fullmatch(str(row.get(field, "")))), f"{iid}: invalid {field}")
        _require(errors, isinstance(row.get("gold_file_count"), int) and row["gold_file_count"] >= 2, f"{iid}: not multi-file")
        evidence = row.get("upstream_a")
        _require(errors, isinstance(evidence, dict), f"{iid}: missing A evidence")
        if not isinstance(evidence, dict):
            continue
        _require(errors, evidence.get("outcome") in {"resolved", "unresolved"}, f"{iid}: invalid A outcome")
        _require(errors, evidence.get("resolved") == (evidence.get("outcome") == "resolved"), f"{iid}: A outcome fields disagree")
        resolved += int(evidence.get("resolved") is True)
        count = evidence.get("anthropic_initial_request_input_tokens")
        _require(errors, isinstance(count, int) and count > 0, f"{iid}: missing A initial-request token count")
        if isinstance(count, int):
            initial_total += count
        rounds = evidence.get("edit_feedback_rounds")
        _require(errors, isinstance(rounds, int) and 0 <= rounds <= 2, f"{iid}: invalid feedback-round count")
        if isinstance(rounds, int):
            feedback_rounds += rounds
        _require(errors, evidence.get("retry_inclusive_input_tokens") is None, f"{iid}: inferred A retry-inclusive tokens are forbidden")
        _require(errors, evidence.get("retry_inclusive_input_tokens_reason") == "upstream_not_measured", f"{iid}: missing A retry-token null reason")
        _require(errors, evidence.get("wall_clock_seconds") is None, f"{iid}: inferred A wall-clock timing is forbidden")

    summary = population.get("upstream_a", {})
    _require(errors, resolved == 19, "A must remain 19/70 resolved")
    _require(errors, initial_total == 2_279_203, "A initial-request token total drift")
    _require(errors, feedback_rounds == 22, "A edit-feedback round total drift")
    _require(errors, summary.get("resolved") == 19 and summary.get("unresolved") == 51, "A summary outcome drift")
    _require(errors, summary.get("total_initial_request_input_tokens") == initial_total, "A summary initial-token total mismatch")
    _require(errors, summary.get("retry_inclusive_input_tokens") is None, "A summary retry-inclusive tokens must be null")
    _require(errors, summary.get("retry_inclusive_input_tokens_reason") == "upstream_not_measured", "A summary retry-token null reason drift")
    _require(errors, summary.get("retrying_instances") == 11, "A retrying-instance count drift")

    schedule = population.get("run_schedule", {})
    episodes = schedule.get("episodes", [])
    _require(errors, schedule.get("seed") == ORDER_SEED, "run-order seed drift")
    _require(errors, isinstance(episodes, list) and len(episodes) == 70, "schedule must contain 70 episodes")
    included_by_id = {row["instance_id"] for row in included}
    scheduled_ids: list[str] = []
    if isinstance(episodes, list):
        expected_order = sorted(
            included_by_id,
            key=lambda iid: (sha256_bytes((ORDER_SEED + "\0" + iid).encode("utf-8")), iid),
        )
        for ordinal, episode in enumerate(episodes, 1):
            iid = episode.get("instance_id", "")
            scheduled_ids.append(iid)
            key = sha256_bytes((ORDER_SEED + "\0" + iid).encode("utf-8"))
            arm_order = ["B", "C"] if int(key[:2], 16) % 2 == 0 else ["C", "B"]
            _require(errors, episode.get("ordinal") == ordinal, f"schedule ordinal drift at {ordinal}")
            _require(errors, episode.get("order_key_sha256") == key, f"{iid}: schedule key drift")
            _require(errors, episode.get("arm_order") == arm_order, f"{iid}: arm-order drift")
        _require(errors, scheduled_ids == expected_order, "scheduled instance order drift")


def validate_runtime(
    runtime: dict[str, Any],
    errors: list[str],
    *,
    allow_authorized: bool = False,
) -> None:
    expected = {
        "schema_version": 1,
        "execute_a": False,
        "rna_access": "cli",
        "business_context": "disabled",
        "include_ordinary_docs": True,
        "requested_model": "claude-sonnet-4-6",
        "expected_resolved_model": "claude-sonnet-4-6",
        "temperature": 0.0,
        "max_tokens": 8000,
        "max_edit_feedback_rounds": 2,
        "swebench_version": "4.1.0",
        "dataset_revision": EXPECTED_DATASET_REVISION,
        "population_n": 70,
        "parallelism": 1,
        "credential_source": "environment-only; read only after validator success",
    }
    for key, value in expected.items():
        _require(errors, runtime.get(key) == value, f"runtime {key} drift")
    expected_keys = set(expected) | {
        "paid_calls_authorized",
        "qualified_artifact_receipt",
        "approved_budget_receipt",
    }
    _require(errors, set(runtime) == expected_keys, "runtime config field set drift")
    normalized_keys = {str(key).lower().replace("-", "_") for key in runtime}
    _require(
        errors,
        not any("api_key" in key for key in normalized_keys),
        "runtime config must not contain an API key field",
    )
    _require(
        errors,
        SECRET_VALUE.search(canonical_json(runtime).decode("utf-8")) is None,
        "runtime config contains a credential-shaped value",
    )

    authorized = runtime.get("paid_calls_authorized")
    _require(errors, isinstance(authorized, bool), "runtime paid_calls_authorized must be boolean")
    if authorized:
        _require(errors, allow_authorized, "committed runtime template must not authorize paid calls")
        for field in ("qualified_artifact_receipt", "approved_budget_receipt"):
            receipt = runtime.get(field)
            _require(
                errors,
                isinstance(receipt, str) and bool(receipt.strip()),
                f"authorized runtime requires non-empty {field}",
            )
    else:
        _require(errors, runtime.get("qualified_artifact_receipt") is None, "unauthorized runtime must not carry artifact receipt")
        _require(errors, runtime.get("approved_budget_receipt") is None, "unauthorized runtime must not carry budget receipt")


def assemble_packet_vector(vector: dict[str, Any], arm: str) -> bytes:
    if arm not in {"B", "C"}:
        raise ValueError("arm must be B or C")
    output = bytearray(b"RNA_PACKET_V1\n")
    output.extend(canonical_json(vector["metadata"]))
    output.extend(b"\n")
    for source in vector["records"]:
        payload_key = "full_payload" if arm == "B" or source["kind"] == "locus" else "minified_payload"
        representation = "verbatim" if source["kind"] == "locus" else ("full" if arm == "B" else "minified")
        payload = source[payload_key].encode("utf-8")
        header = dict(source["header"])
        header.update(
            {
                "payload_representation": representation,
                "payload_byte_length": len(payload),
                "payload_sha256": sha256_bytes(payload),
            }
        )
        output.extend(canonical_json(header))
        output.extend(b"\n")
        output.extend(payload)
        output.extend(b"\n")
    return bytes(output)


def _relationship_sort_key(relationship: dict[str, Any]) -> tuple[Any, ...]:
    return (
        relationship.get("locus_ordinal"),
        DIRECTION_ORDINAL.get(relationship.get("direction"), 99),
        relationship.get("cli_ordinal"),
        EDGE_TYPE_ORDINAL.get(relationship.get("edge_type"), 99),
        str(relationship.get("source", "")).encode("utf-8"),
        str(relationship.get("target", "")).encode("utf-8"),
    )


def validate_packet_vector(vector: dict[str, Any], errors: list[str]) -> None:
    initial_error_count = len(errors)
    _require(errors, vector.get("schema_version") == 1, "packet vector schema_version drift")
    _require(
        errors,
        set(vector)
        == {"schema_version", "metadata", "records", "expected_b_sha256", "expected_c_sha256"},
        "packet vector field set drift",
    )
    metadata = vector.get("metadata", {})
    _require(errors, isinstance(metadata, dict), "packet metadata must be an object")
    if isinstance(metadata, dict):
        _require(errors, set(metadata) == set(PACKET_METADATA_FIELDS), "packet metadata field set drift")
        _require(errors, metadata.get("protocol_id") == "rna-act-context-swebench-v1", "packet protocol_id drift")
    records = vector.get("records", [])
    _require(errors, isinstance(records, list) and bool(records), "packet vector records must be non-empty")
    if not isinstance(records, list):
        return
    if isinstance(metadata, dict):
        _require(errors, metadata.get("record_count") == len(records), "packet record_count drift")
    for ordinal, source in enumerate(records, 1):
        _require(errors, isinstance(source, dict), f"packet record {ordinal} must be an object")
        if not isinstance(source, dict):
            continue
        _require(
            errors,
            set(source) == {"kind", "header", "full_payload", "minified_payload"},
            f"packet record {ordinal} field set drift",
        )
        header = source.get("header", {})
        _require(errors, isinstance(header, dict), f"packet record {ordinal} header must be an object")
        if not isinstance(header, dict):
            continue
        _require(errors, set(header) == set(PACKET_HEADER_FIELDS), f"packet record {ordinal} header field set drift")
        _require(errors, header.get("ordinal") == ordinal, f"packet record {ordinal} ordinal drift")
        _require(errors, header.get("kind") == source.get("kind"), f"packet record {ordinal} kind drift")
        full_payload = source.get("full_payload")
        minified_payload = source.get("minified_payload")
        _require(errors, isinstance(full_payload, str), f"packet record {ordinal} full payload must be text")
        _require(errors, isinstance(minified_payload, str), f"packet record {ordinal} minified payload must be text")
        if isinstance(full_payload, str):
            full_bytes = full_payload.encode("utf-8")
            _require(errors, header.get("full_body_byte_length") == len(full_bytes), f"packet record {ordinal} full-body length drift")
            _require(errors, header.get("full_body_sha256") == sha256_bytes(full_bytes), f"packet record {ordinal} full-body digest drift")
        if source.get("kind") == "locus":
            _require(errors, minified_payload == full_payload, f"packet locus {ordinal} must remain verbatim")
        elif isinstance(full_payload, str) and isinstance(minified_payload, str):
            _require(errors, bool(minified_payload), f"packet candidate {ordinal} minified payload is empty")
            _require(errors, len(minified_payload.encode("utf-8")) <= len(full_payload.encode("utf-8")), f"packet candidate {ordinal} minified payload grew")
            _require(
                errors,
                "\n# mvb=accumulated_value" in minified_payload,
                "C packet vector must include the minifier legend",
            )
        relationships = header.get("relationships", [])
        _require(errors, isinstance(relationships, list), f"packet record {ordinal} relationships must be a list")
        if isinstance(relationships, list):
            for relationship in relationships:
                _require(
                    errors,
                    isinstance(relationship, dict)
                    and set(relationship) == set(PACKET_RELATIONSHIP_FIELDS),
                    f"packet record {ordinal} relationship field set drift",
                )
            if all(isinstance(relationship, dict) for relationship in relationships):
                _require(
                    errors,
                    relationships == sorted(relationships, key=_relationship_sort_key),
                    f"packet record {ordinal} relationship order drift",
                )
                tuples = [tuple(relationship[field] for field in PACKET_RELATIONSHIP_FIELDS) for relationship in relationships]
                _require(errors, len(tuples) == len(set(tuples)), f"packet record {ordinal} duplicate relationship")
    if len(errors) != initial_error_count:
        return
    b_packet = assemble_packet_vector(vector, "B")
    c_packet = assemble_packet_vector(vector, "C")
    _require(errors, sha256_bytes(b_packet) == vector.get("expected_b_sha256"), "B packet test-vector digest drift")
    _require(errors, sha256_bytes(c_packet) == vector.get("expected_c_sha256"), "C packet test-vector digest drift")
    _require(errors, b_packet != c_packet, "packet vector must exercise C-only minification")


def _lock_material(entries: list[dict[str, Any]]) -> bytes:
    return "".join(
        f"{entry['sha256']} {entry['bytes']} {entry['path']}\n" for entry in entries
    ).encode("utf-8")


def validate_lock(root: Path, errors: list[str]) -> str | None:
    lock_path = root / LOCK_REL
    lock_bytes = lock_path.read_bytes()
    if SECRET_VALUE.search(lock_bytes.decode("utf-8", errors="ignore")):
        errors.append("credential-shaped value in lock manifest")
    lock = load_json(lock_path)
    entries = lock.get("files", [])
    _require(
        errors,
        set(lock) == {"schema_version", "algorithm", "material_format", "files", "bundle_sha256"},
        "lock field set drift",
    )
    _require(errors, lock.get("schema_version") == 1, "lock schema_version must be 1")
    _require(errors, lock.get("algorithm") == "sha256", "lock algorithm drift")
    _require(errors, lock.get("material_format") == LOCK_MATERIAL_FORMAT, "lock material format drift")
    _require(errors, isinstance(entries, list) and bool(entries), "lock must contain files")
    if not isinstance(entries, list):
        return None
    paths = [entry.get("path") for entry in entries if isinstance(entry, dict)]
    _require(errors, paths == sorted(paths), "lock paths must be sorted")
    _require(errors, len(paths) == len(set(paths)), "lock paths must be unique")
    _require(errors, paths == list(EXPECTED_LOCK_PATHS), "lock path set drift")
    for entry in entries:
        _require(errors, isinstance(entry, dict), "lock file entry must be an object")
        if not isinstance(entry, dict):
            continue
        _require(errors, set(entry) == {"path", "bytes", "sha256"}, f"lock entry field set drift: {entry.get('path')}")
        _require(errors, isinstance(entry.get("bytes"), int) and entry["bytes"] >= 0, f"invalid lock byte length: {entry.get('path')}")
        _require(errors, bool(HEX64.fullmatch(str(entry.get("sha256", "")))), f"invalid lock digest: {entry.get('path')}")
        rel = Path(entry.get("path", ""))
        _require(errors, not rel.is_absolute() and ".." not in rel.parts, f"unsafe lock path: {rel}")
        path = root / rel
        _require(errors, path.is_file(), f"missing locked file: {rel}")
        if not path.is_file():
            continue
        data = path.read_bytes()
        _require(errors, len(data) == entry.get("bytes"), f"byte length drift: {rel}")
        _require(errors, sha256_bytes(data) == entry.get("sha256"), f"digest drift: {rel}")
        if SECRET_VALUE.search(data.decode("utf-8", errors="ignore")):
            errors.append(f"credential-shaped value in locked file: {rel}")
    bundle_root = root / "benchmark/swebench-act-context"
    actual_bundle_paths = {
        path.relative_to(root).as_posix()
        for path in bundle_root.rglob("*")
        if path.is_file()
    }
    expected_bundle_paths = {
        path for path in EXPECTED_LOCK_PATHS if path.startswith("benchmark/swebench-act-context/")
    } | {LOCK_REL.as_posix(), DIGEST_REL.as_posix()}
    _require(
        errors,
        actual_bundle_paths == expected_bundle_paths,
        "bundle file inventory drift",
    )
    digest = sha256_bytes(_lock_material(entries))
    _require(errors, digest == lock.get("bundle_sha256"), "bundle digest mismatch in lock")
    digest_file = (root / DIGEST_REL).read_text(encoding="ascii").strip()
    _require(errors, digest_file == digest, "protocol.sha256 does not match bundle")
    return digest


def validate_bundle(
    root: Path,
    expected_digest: str | None = None,
    runtime_config: Path | None = None,
) -> dict[str, Any]:
    root = root.resolve()
    errors: list[str] = []
    protocol_path = root / PROTOCOL_REL
    population_path = root / POPULATION_REL
    _require(errors, sha256_file(protocol_path) == EXPECTED_PROTOCOL_SHA256, "frozen protocol.json digest drift")
    _require(errors, sha256_file(population_path) == EXPECTED_POPULATION_SHA256, "frozen population.json digest drift")
    _require(errors, sha256_file(root / PARSER_REL) == EXPECTED_PARSER_SHA256, "vendored parser digest drift")

    protocol = load_json(protocol_path)
    population = load_json(population_path)
    runtime_template = load_json(root / RUNTIME_REL)
    vector = load_json(root / VECTOR_REL)
    validate_protocol(protocol, errors)
    validate_population(population, errors)
    validate_runtime(runtime_template, errors)
    runtime = runtime_template
    if runtime_config is not None:
        runtime = load_json(runtime_config)
        validate_runtime(runtime, errors, allow_authorized=True)
    if runtime.get("paid_calls_authorized") is True:
        _require(
            errors,
            expected_digest is not None,
            "paid authorization requires an externally anchored bundle digest",
        )
    validate_packet_vector(vector, errors)
    bundle_digest = validate_lock(root, errors)
    if expected_digest is not None:
        _require(errors, bool(HEX64.fullmatch(expected_digest)), "--expected-digest must be 64 lowercase hex characters")
        _require(errors, bundle_digest == expected_digest, "externally anchored protocol digest mismatch")

    if errors:
        raise ValueError("protocol validation failed:\n- " + "\n- ".join(errors))
    return {
        "compatible": True,
        "protocol_id": protocol["protocol_id"],
        "bundle_sha256": bundle_digest,
        "selected": 71,
        "included": 70,
        "a_resolved": 19,
        "a_binary_outcomes_comparable": True,
        "a_initial_request_tokens_comparable": True,
        "a_retry_inclusive_tokens_comparable": False,
        "a_retry_inclusive_tokens_reason": "upstream_not_measured",
        "paid_calls_authorized": runtime["paid_calls_authorized"],
        "network_accessed": False,
        "model_accessed": False,
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--expected-digest", help="externally anchored bundle SHA-256")
    parser.add_argument(
        "--runtime-config",
        type=Path,
        help="separate sealed run config; paid calls require artifact and budget receipts",
    )
    parser.add_argument("--json", action="store_true", help="emit the compatibility record as JSON")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = validate_bundle(args.root, args.expected_digest, args.runtime_config)
    except (OSError, json.JSONDecodeError, DuplicateKey, KeyError, TypeError, ValueError) as error:
        print(f"INCOMPATIBLE: {error}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    else:
        print(f"COMPATIBLE {result['protocol_id']} {result['bundle_sha256']}")
        print("A: binary outcomes + initial-request tokens comparable; retry-inclusive tokens unavailable")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
