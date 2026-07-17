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
PACKET_METADATA_FIELDS = ("instance_id", "protocol_id", "record_count", "acquisition")
PACKET_HEADER_FIELDS = (
    "kind",
    "source_kind",
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
ACQUISITION_FIELDS = (
    "schema_version",
    "dataset_row_sha256",
    "query_sha256",
    "rna_artifact_receipt_sha256",
    "loci",
    "candidates",
    "relationships",
    "omissions",
)
ACQUISITION_LOCUS_FIELDS = (
    "ordinal",
    "source_kind",
    "stable_id",
    "path",
    "start_line",
    "end_line",
    "language",
    "preimage_byte_length",
    "preimage_sha256",
    "seed_stable_ids",
)
ACQUISITION_CANDIDATE_FIELDS = (
    "acquisition_ordinal",
    "stable_id",
    "path",
    "start_line",
    "end_line",
    "language",
    "full_body_byte_length",
    "full_body_sha256",
    "semantic_rank",
    "graph_hops",
    "semantic_component",
    "graph_component",
    "total",
    "eligibility",
    "selected",
)
ACQUISITION_OMISSION_FIELDS = (
    "candidate_stable_id",
    "reason",
    "required_bytes",
    "remaining_budget_bytes",
)
LOCUS_SOURCE_KINDS = {
    "gold_unit",
    "module_preamble",
    "whole_non_python_file",
    "new_file",
}
CANDIDATE_ELIGIBILITY = {
    "eligible",
    "not_source_backed",
    "incomplete_utf8_body",
    "locus_overlap",
    "excluded_record_class",
}
OMISSION_REASONS = CANDIDATE_ELIGIBILITY - {"eligible"} | {
    "maximum_candidates",
    "full_body_budget",
}
QUALIFIED_ARTIFACT_RECEIPT_FIELDS = (
    "schema_version",
    "receipt_type",
    "protocol_id",
    "protocol_bundle_sha256",
    "artifact_commit_sha",
    "artifact_sha256",
    "ci_repository",
    "ci_workflow",
    "ci_run_id",
    "ci_run_url",
    "ci_artifact_name",
    "platform",
    "capability_evidence",
    "qualification_issue",
    "qualification_comment_url",
    "qualified_at_utc",
)
CAPABILITY_EVIDENCE_FIELDS = (
    "release_build",
    "metal",
    "embeddings",
    "reranking",
    "lsp",
)
APPROVED_BUDGET_RECEIPT_FIELDS = (
    "schema_version",
    "receipt_type",
    "protocol_id",
    "protocol_bundle_sha256",
    "authorization_scope",
    "authorization_issue",
    "qualification_instance_id",
    "population_n",
    "maximum_model_requests",
    "maximum_total_usd",
    "approval_comment_url",
    "approval_evidence_sha256",
    "approved_at_utc",
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

EXPECTED_PROTOCOL_SHA256 = "87257d4435b2cd16b985c5bcc72153e3745254e086a4e1bc250bb1abc01efd1d"
EXPECTED_POPULATION_SHA256 = "067a5589b4cdb34c5fbd81bb6ff7ff6ede4dbfc26694758fafbef3544f9e6acf"
EXPECTED_PARSER_SHA256 = "68b44b5b39ff7fbf3e7417b4f16f0c37513a4cd7a96be8ba00611c825f462c2e"
EXPECTED_UPSTREAM_COMMIT = "fd115351d0ab742993aa5d7006f1369fb15b6e74"
EXPECTED_DATASET_REVISION = "c104f840cc67f8b6eec6f759ebc8b2693d585d4a"
EXPECTED_EXCLUSION = "astropy__astropy-8707"
ORDER_SEED = "rna-act-context-bc-order-v1"
RETRY_SUFFIX = "\n\nYOUR PREVIOUS EDIT BLOCKS (for reference):\n{prev}\n\nThese edits FAILED to apply:\n{feedback}\n\nThe other edits were applied successfully. Re-emit corrected edit blocks ONLY for the failed edits,\nin the same *** FILE / SEARCH / REPLACE / END format."
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
UTC_SECOND = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
ARTIFACT_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
POSITIVE_USD = re.compile(r"^(?:0\.(?:0[1-9]|[1-9][0-9])|[1-9][0-9]{0,5}\.[0-9]{2})$")
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
    _require(
        errors,
        set(retry)
        == {
            "maximum_feedback_rounds",
            "retry_trigger",
            "retry_suffix",
            "retry_suffix_sha256",
            "base_prompt_source",
            "previous_response_slice",
            "feedback_source",
            "request_assembly",
            "round_state",
            "byte_vector",
            "model/API transport_retry_policy",
        },
        "retry contract field set drift",
    )
    _require(errors, retry.get("maximum_feedback_rounds") == 2, "edit-feedback retry count drift")
    _require(errors, retry_text == RETRY_SUFFIX, "retry suffix bytes drift")
    if isinstance(retry_text, str):
        _require(
            errors,
            sha256_bytes(retry_text.encode("utf-8")) == retry.get("retry_suffix_sha256"),
            "retry suffix hash mismatch",
        )
    else:
        errors.append("retry suffix must be text")
    _require(
        errors,
        retry.get("request_assembly")
        == 'retry_prompt = prompt + retry_suffix.format(prev=(raw or "")[-6000:], feedback=failure_feedback(all_results))',
        "retry request assembly drift",
    )
    _require(
        errors,
        "Unicode code points" in retry.get("previous_response_slice", "")
        and "immediately preceding" in retry.get("previous_response_slice", "")
        and "[-6000:]" in retry.get("previous_response_slice", ""),
        "retry previous-response slice drift",
    )
    _require(
        errors,
        "original initial-request prompt" in retry.get("base_prompt_source", "")
        and "never to an earlier retry prompt" in retry.get("base_prompt_source", ""),
        "retry base-prompt state drift",
    )
    _require(
        errors,
        "scaled_run.py lines 127-133" in retry.get("round_state", ""),
        "retry round-state update drift",
    )

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
    acquisition_schema = protocol.get("rna_acquisition", {}).get("record_schema", {})
    _require(
        errors,
        acquisition_schema.get("exact_fields") == list(ACQUISITION_FIELDS),
        "acquisition record schema drift",
    )
    _require(
        errors,
        acquisition_schema.get("locus_exact_fields") == list(ACQUISITION_LOCUS_FIELDS),
        "acquisition locus schema drift",
    )
    _require(
        errors,
        acquisition_schema.get("candidate_exact_fields") == list(ACQUISITION_CANDIDATE_FIELDS),
        "acquisition candidate schema drift",
    )
    _require(
        errors,
        acquisition_schema.get("relationship_exact_fields") == list(PACKET_RELATIONSHIP_FIELDS),
        "acquisition relationship schema drift",
    )
    _require(
        errors,
        acquisition_schema.get("omission_exact_fields") == list(ACQUISITION_OMISSION_FIELDS),
        "acquisition omission schema drift",
    )
    _require(
        errors,
        set(acquisition_schema.get("locus_source_kinds", [])) == LOCUS_SOURCE_KINDS
        and "start_line=0" in acquisition_schema.get("new_file_locus", "")
        and "seed_stable_ids=[]" in acquisition_schema.get("new_file_locus", "")
        and EMPTY_SHA256 in acquisition_schema.get("new_file_locus", ""),
        "exceptional locus serialization drift",
    )
    _require(
        errors,
        set(acquisition_schema.get("candidate_eligibility_values", [])) == CANDIDATE_ELIGIBILITY,
        "candidate eligibility vocabulary drift",
    )
    _require(
        errors,
        set(acquisition_schema.get("omission_reason_values", [])) == OMISSION_REASONS,
        "candidate omission vocabulary drift",
    )

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

    receipt_schemas = protocol.get("authorization_receipts", {})
    artifact_schema = receipt_schemas.get("qualified_artifact_receipt_schema", {})
    budget_schema = receipt_schemas.get("approved_budget_receipt_schema", {})
    _require(
        errors,
        artifact_schema.get("exact_fields") == list(QUALIFIED_ARTIFACT_RECEIPT_FIELDS),
        "qualified artifact receipt schema drift",
    )
    _require(
        errors,
        artifact_schema.get("capability_evidence_exact_fields")
        == list(CAPABILITY_EVIDENCE_FIELDS),
        "artifact capability evidence schema drift",
    )
    _require(
        errors,
        artifact_schema.get("lsp_exact_fields")
        == [
            "status",
            "quiescent",
            "languages",
            "skipped_files",
            "partial_jobs",
            "degraded_jobs",
            "cancelled_jobs",
            "crashed_jobs",
            "timed_out_jobs",
            "included_file_count",
            "covered_file_count",
            "coverage_scope",
            "coverage_manifest_sha256",
            "evidence_sha256",
        ],
        "artifact LSP evidence schema drift",
    )
    _require(
        errors,
        budget_schema.get("exact_fields") == list(APPROVED_BUDGET_RECEIPT_FIELDS),
        "approved budget receipt schema drift",
    )
    _require(
        errors,
        "three independent CLI trust anchors"
        in receipt_schemas.get("external_anchor_requirements", ""),
        "authorization receipt trust-anchor policy drift",
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
    _require(errors, "closed artifact/budget receipts" in rejected, "validator no longer requires closed authorization receipts")
    _require(errors, "acquisition/omission" in rejected, "validator no longer rejects acquisition schema drift")
    _require(errors, "codepoint-slice byte vector" in rejected, "validator no longer freezes retry bytes")


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


def validate_qualified_artifact_receipt(
    receipt: Any,
    errors: list[str],
    expected_bundle_digest: str | None,
) -> None:
    _require(errors, isinstance(receipt, dict), "authorized runtime requires structured qualified_artifact_receipt")
    if not isinstance(receipt, dict):
        return
    _require(
        errors,
        set(receipt) == set(QUALIFIED_ARTIFACT_RECEIPT_FIELDS),
        "qualified artifact receipt field set drift",
    )
    constants = {
        "schema_version": 1,
        "receipt_type": "rna-qualified-artifact-v1",
        "protocol_id": "rna-act-context-swebench-v1",
        "ci_repository": "open-horizon-labs/repo-native-alignment",
        "platform": "darwin-arm64-m4",
        "qualification_issue": 786,
    }
    for field, expected in constants.items():
        _require(errors, receipt.get(field) == expected, f"qualified artifact receipt {field} drift")
    if expected_bundle_digest is not None:
        _require(
            errors,
            receipt.get("protocol_bundle_sha256") == expected_bundle_digest,
            "qualified artifact receipt protocol digest mismatch",
        )
    _require(
        errors,
        bool(HEX64.fullmatch(str(receipt.get("protocol_bundle_sha256", "")))),
        "qualified artifact receipt protocol digest is invalid",
    )
    _require(
        errors,
        bool(HEX40.fullmatch(str(receipt.get("artifact_commit_sha", "")))),
        "qualified artifact receipt commit is invalid",
    )
    _require(
        errors,
        bool(HEX64.fullmatch(str(receipt.get("artifact_sha256", "")))),
        "qualified artifact receipt artifact digest is invalid",
    )
    workflow = receipt.get("ci_workflow")
    workflow_path = Path(workflow) if isinstance(workflow, str) else Path(".")
    _require(
        errors,
        isinstance(workflow, str)
        and workflow.startswith(".github/workflows/")
        and workflow_path.suffix in {".yml", ".yaml"}
        and ".." not in workflow_path.parts,
        "qualified artifact receipt CI workflow is invalid",
    )
    run_id = receipt.get("ci_run_id")
    _require(
        errors,
        type(run_id) is int and run_id > 0,
        "qualified artifact receipt CI run ID is invalid",
    )
    _require(
        errors,
        receipt.get("ci_run_url")
        == f"https://github.com/open-horizon-labs/repo-native-alignment/actions/runs/{run_id}",
        "qualified artifact receipt CI run URL mismatch",
    )
    _require(
        errors,
        bool(ARTIFACT_NAME.fullmatch(str(receipt.get("ci_artifact_name", "")))),
        "qualified artifact receipt artifact name is invalid",
    )
    _require(
        errors,
        bool(
            re.fullmatch(
                r"https://github\.com/open-horizon-labs/repo-native-alignment/issues/786#issuecomment-[0-9]+",
                str(receipt.get("qualification_comment_url", "")),
            )
        ),
        "qualified artifact receipt qualification URL is invalid",
    )
    _require(
        errors,
        bool(UTC_SECOND.fullmatch(str(receipt.get("qualified_at_utc", "")))),
        "qualified artifact receipt timestamp is invalid",
    )

    capabilities = receipt.get("capability_evidence")
    _require(errors, isinstance(capabilities, dict), "qualified artifact capability evidence must be an object")
    if not isinstance(capabilities, dict):
        return
    _require(
        errors,
        set(capabilities) == set(CAPABILITY_EVIDENCE_FIELDS),
        "qualified artifact capability field set drift",
    )
    release = capabilities.get("release_build")
    _require(
        errors,
        isinstance(release, dict) and set(release) == {"status", "evidence_sha256"},
        "release-build evidence field set drift",
    )
    if isinstance(release, dict):
        _require(errors, release.get("status") == "passed", "release build was not successful")
        _require(errors, bool(HEX64.fullmatch(str(release.get("evidence_sha256", "")))), "release-build evidence digest is invalid")

    metal = capabilities.get("metal")
    _require(
        errors,
        isinstance(metal, dict)
        and set(metal) == {"required", "observed", "fallback", "evidence_sha256"},
        "Metal evidence field set drift",
    )
    if isinstance(metal, dict):
        _require(
            errors,
            metal.get("required") is True
            and metal.get("observed") is True
            and metal.get("fallback") is False,
            "Metal qualification must be observed with no fallback",
        )
        _require(errors, bool(HEX64.fullmatch(str(metal.get("evidence_sha256", "")))), "Metal evidence digest is invalid")

    for name in ("embeddings", "reranking"):
        evidence = capabilities.get(name)
        _require(
            errors,
            isinstance(evidence, dict)
            and set(evidence) == {"enabled", "complete", "fallback", "evidence_sha256"},
            f"{name} evidence field set drift",
        )
        if isinstance(evidence, dict):
            _require(
                errors,
                evidence.get("enabled") is True
                and evidence.get("complete") is True
                and evidence.get("fallback") is False,
                f"{name} qualification must be enabled and complete with no fallback",
            )
            _require(errors, bool(HEX64.fullmatch(str(evidence.get("evidence_sha256", "")))), f"{name} evidence digest is invalid")

    lsp = capabilities.get("lsp")
    lsp_fields = {
        "status",
        "quiescent",
        "languages",
        "skipped_files",
        "partial_jobs",
        "degraded_jobs",
        "cancelled_jobs",
        "crashed_jobs",
        "timed_out_jobs",
        "included_file_count",
        "covered_file_count",
        "coverage_scope",
        "coverage_manifest_sha256",
        "evidence_sha256",
    }
    _require(errors, isinstance(lsp, dict) and set(lsp) == lsp_fields, "LSP evidence field set drift")
    if not isinstance(lsp, dict):
        return
    _require(
        errors,
        lsp.get("status") == "complete" and lsp.get("quiescent") is True,
        "LSP qualification must be complete and quiescent",
    )
    languages = lsp.get("languages")
    _require(
        errors,
        isinstance(languages, list)
        and all(isinstance(language, str) and language for language in languages)
        and languages == sorted(set(languages), key=lambda value: value.encode("utf-8"))
        and "Python" in languages,
        "LSP language coverage must be sorted, unique, and include Python",
    )
    for counter in (
        "skipped_files",
        "partial_jobs",
        "degraded_jobs",
        "cancelled_jobs",
        "crashed_jobs",
        "timed_out_jobs",
    ):
        _require(errors, lsp.get(counter) == 0, f"LSP qualification {counter} must be zero")
    included_files = lsp.get("included_file_count")
    covered_files = lsp.get("covered_file_count")
    _require(
        errors,
        type(included_files) is int
        and included_files > 0
        and covered_files == included_files,
        "LSP per-file coverage must be complete",
    )
    _require(
        errors,
        lsp.get("coverage_scope") == "every included ordinary docs/source/test/config file",
        "LSP coverage scope drift",
    )
    for field in ("coverage_manifest_sha256", "evidence_sha256"):
        _require(errors, bool(HEX64.fullmatch(str(lsp.get(field, "")))), f"LSP {field} is invalid")


def validate_approved_budget_receipt(
    receipt: Any,
    errors: list[str],
    expected_bundle_digest: str | None,
) -> None:
    _require(errors, isinstance(receipt, dict), "authorized runtime requires structured approved_budget_receipt")
    if not isinstance(receipt, dict):
        return
    _require(
        errors,
        set(receipt) == set(APPROVED_BUDGET_RECEIPT_FIELDS),
        "approved budget receipt field set drift",
    )
    constants = {
        "schema_version": 1,
        "receipt_type": "approved-model-budget-v1",
        "protocol_id": "rna-act-context-swebench-v1",
    }
    for field, expected in constants.items():
        _require(errors, receipt.get(field) == expected, f"approved budget receipt {field} drift")
    if expected_bundle_digest is not None:
        _require(
            errors,
            receipt.get("protocol_bundle_sha256") == expected_bundle_digest,
            "approved budget receipt protocol digest mismatch",
        )
    _require(
        errors,
        bool(HEX64.fullmatch(str(receipt.get("protocol_bundle_sha256", "")))),
        "approved budget receipt protocol digest is invalid",
    )
    scope = receipt.get("authorization_scope")
    issue = receipt.get("authorization_issue")
    instance_id = receipt.get("qualification_instance_id")
    population_n = receipt.get("population_n")
    maximum_requests = receipt.get("maximum_model_requests")
    if scope == "qualification_pair":
        _require(
            errors,
            issue == 789
            and instance_id == "astropy__astropy-13398"
            and population_n == 1
            and type(maximum_requests) is int
            and 1 <= maximum_requests <= 6,
            "qualification-pair budget scope mismatch",
        )
    elif scope == "n70_cohort":
        _require(
            errors,
            issue == 790
            and instance_id is None
            and population_n == 70
            and type(maximum_requests) is int
            and 1 <= maximum_requests <= 420,
            "N70 cohort budget scope mismatch",
        )
    else:
        errors.append("approved budget receipt authorization_scope is invalid")
    _require(
        errors,
        bool(POSITIVE_USD.fullmatch(str(receipt.get("maximum_total_usd", "")))),
        "approved budget receipt maximum_total_usd is invalid",
    )
    _require(
        errors,
        bool(
            re.fullmatch(
                rf"https://github\.com/open-horizon-labs/repo-native-alignment/issues/{issue}#issuecomment-[0-9]+",
                str(receipt.get("approval_comment_url", "")),
            )
        ),
        "approved budget receipt approval URL is invalid",
    )
    _require(
        errors,
        bool(HEX64.fullmatch(str(receipt.get("approval_evidence_sha256", "")))),
        "approved budget receipt evidence digest is invalid",
    )
    _require(
        errors,
        bool(UTC_SECOND.fullmatch(str(receipt.get("approved_at_utc", "")))),
        "approved budget receipt timestamp is invalid",
    )


def validate_runtime(
    runtime: dict[str, Any],
    errors: list[str],
    *,
    allow_authorized: bool = False,
    expected_bundle_digest: str | None = None,
    expected_artifact_receipt_digest: str | None = None,
    expected_budget_receipt_digest: str | None = None,
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
        _require(
            errors,
            expected_bundle_digest is not None,
            "authorized runtime requires an externally anchored bundle digest",
        )
        validate_qualified_artifact_receipt(
            runtime.get("qualified_artifact_receipt"), errors, expected_bundle_digest
        )
        validate_approved_budget_receipt(
            runtime.get("approved_budget_receipt"), errors, expected_bundle_digest
        )
        for label, receipt, expected_receipt_digest in (
            (
                "artifact",
                runtime.get("qualified_artifact_receipt"),
                expected_artifact_receipt_digest,
            ),
            (
                "budget",
                runtime.get("approved_budget_receipt"),
                expected_budget_receipt_digest,
            ),
        ):
            _require(
                errors,
                isinstance(expected_receipt_digest, str)
                and bool(HEX64.fullmatch(expected_receipt_digest)),
                f"authorized runtime requires an externally anchored {label} receipt digest",
            )
            if isinstance(receipt, dict) and isinstance(expected_receipt_digest, str):
                _require(
                    errors,
                    sha256_bytes(canonical_json(receipt)) == expected_receipt_digest,
                    f"externally anchored {label} receipt digest mismatch",
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


def _safe_relative_path(value: Any) -> bool:
    if not isinstance(value, str) or not value:
        return False
    path = Path(value)
    return not path.is_absolute() and ".." not in path.parts and value == path.as_posix()


def _candidate_sort_key(candidate: dict[str, Any]) -> tuple[Any, ...]:
    total = candidate.get("total")
    semantic_rank = candidate.get("semantic_rank")
    graph_hops = candidate.get("graph_hops")
    return (
        -total if type(total) is int else sys.maxsize,
        semantic_rank if type(semantic_rank) is int else sys.maxsize,
        graph_hops if type(graph_hops) is int else sys.maxsize,
        str(candidate.get("stable_id", "")).encode("utf-8"),
    )


def validate_acquisition_vector(
    acquisition: Any,
    records: list[Any],
    errors: list[str],
) -> None:
    _require(errors, isinstance(acquisition, dict), "packet acquisition must be an object")
    if not isinstance(acquisition, dict):
        return
    _require(errors, set(acquisition) == set(ACQUISITION_FIELDS), "packet acquisition field set drift")
    _require(errors, acquisition.get("schema_version") == 1, "packet acquisition schema_version drift")
    for field in ("dataset_row_sha256", "query_sha256", "rna_artifact_receipt_sha256"):
        _require(
            errors,
            bool(HEX64.fullmatch(str(acquisition.get(field, "")))),
            f"packet acquisition {field} is invalid",
        )

    loci = acquisition.get("loci")
    _require(errors, isinstance(loci, list) and bool(loci), "packet acquisition loci must be non-empty")
    if not isinstance(loci, list):
        loci = []
    for ordinal, locus in enumerate(loci, 1):
        _require(errors, isinstance(locus, dict), f"acquisition locus {ordinal} must be an object")
        if not isinstance(locus, dict):
            continue
        _require(errors, set(locus) == set(ACQUISITION_LOCUS_FIELDS), f"acquisition locus {ordinal} field set drift")
        source_kind = locus.get("source_kind")
        _require(errors, locus.get("ordinal") == ordinal, f"acquisition locus {ordinal} ordinal drift")
        _require(errors, source_kind in LOCUS_SOURCE_KINDS, f"acquisition locus {ordinal} source_kind drift")
        _require(errors, _safe_relative_path(locus.get("path")), f"acquisition locus {ordinal} path is unsafe")
        _require(errors, isinstance(locus.get("language"), str) and bool(locus["language"]), f"acquisition locus {ordinal} language is invalid")
        preimage_length = locus.get("preimage_byte_length")
        preimage_sha = locus.get("preimage_sha256")
        _require(errors, type(preimage_length) is int and preimage_length >= 0, f"acquisition locus {ordinal} preimage length is invalid")
        _require(errors, bool(HEX64.fullmatch(str(preimage_sha or ""))), f"acquisition locus {ordinal} preimage digest is invalid")
        expected_stable_id = "locus:{}:{}".format(
            source_kind,
            sha256_bytes(
                canonical_json(
                    [
                        locus.get("path"),
                        locus.get("start_line"),
                        locus.get("end_line"),
                        preimage_sha,
                    ]
                )
            ),
        )
        _require(errors, locus.get("stable_id") == expected_stable_id, f"acquisition locus {ordinal} stable_id drift")
        seeds = locus.get("seed_stable_ids")
        _require(
            errors,
            isinstance(seeds, list)
            and all(isinstance(seed, str) and seed for seed in seeds)
            and seeds == sorted(set(seeds), key=lambda value: value.encode("utf-8")),
            f"acquisition locus {ordinal} seed order drift",
        )
        if source_kind == "new_file":
            _require(
                errors,
                locus.get("start_line") == 0
                and locus.get("end_line") == 0
                and locus.get("language") == "Unknown"
                and preimage_length == 0
                and preimage_sha == EMPTY_SHA256
                and seeds == [],
                f"acquisition new-file locus {ordinal} sentinel drift",
            )
        else:
            _require(
                errors,
                type(locus.get("start_line")) is int
                and type(locus.get("end_line")) is int
                and 1 <= locus["start_line"] <= locus["end_line"],
                f"acquisition locus {ordinal} line bounds are invalid",
            )
            expected_language = "Python" if source_kind in {"gold_unit", "module_preamble"} else "Unknown"
            _require(errors, locus.get("language") == expected_language, f"acquisition locus {ordinal} language drift")

    candidates = acquisition.get("candidates")
    _require(errors, isinstance(candidates, list), "packet acquisition candidates must be a list")
    if not isinstance(candidates, list):
        candidates = []
    for ordinal, candidate in enumerate(candidates, 1):
        _require(errors, isinstance(candidate, dict), f"acquisition candidate {ordinal} must be an object")
        if not isinstance(candidate, dict):
            continue
        _require(errors, set(candidate) == set(ACQUISITION_CANDIDATE_FIELDS), f"acquisition candidate {ordinal} field set drift")
        _require(errors, candidate.get("acquisition_ordinal") == ordinal, f"acquisition candidate {ordinal} ordinal drift")
        _require(errors, isinstance(candidate.get("stable_id"), str) and bool(candidate["stable_id"]), f"acquisition candidate {ordinal} stable_id is invalid")
        _require(errors, _safe_relative_path(candidate.get("path")), f"acquisition candidate {ordinal} path is unsafe")
        _require(
            errors,
            type(candidate.get("start_line")) is int
            and type(candidate.get("end_line")) is int
            and 1 <= candidate["start_line"] <= candidate["end_line"],
            f"acquisition candidate {ordinal} line bounds are invalid",
        )
        _require(errors, isinstance(candidate.get("language"), str) and bool(candidate["language"]), f"acquisition candidate {ordinal} language is invalid")
        _require(errors, type(candidate.get("full_body_byte_length")) is int and candidate["full_body_byte_length"] >= 0, f"acquisition candidate {ordinal} full-body length is invalid")
        _require(errors, bool(HEX64.fullmatch(str(candidate.get("full_body_sha256", "")))), f"acquisition candidate {ordinal} full-body digest is invalid")
        for field in ("semantic_rank", "graph_hops"):
            value = candidate.get(field)
            _require(errors, value is None or (type(value) is int and value >= 1), f"acquisition candidate {ordinal} {field} is invalid")
        for field in ("semantic_component", "graph_component", "total"):
            _require(errors, type(candidate.get(field)) is int and candidate[field] >= 0, f"acquisition candidate {ordinal} {field} is invalid")
        _require(
            errors,
            candidate.get("total")
            == candidate.get("semantic_component", -1) + candidate.get("graph_component", -1),
            f"acquisition candidate {ordinal} score total drift",
        )
        _require(errors, candidate.get("eligibility") in CANDIDATE_ELIGIBILITY, f"acquisition candidate {ordinal} eligibility drift")
        _require(errors, isinstance(candidate.get("selected"), bool), f"acquisition candidate {ordinal} selected flag is invalid")
        if candidate.get("selected") is True:
            _require(errors, candidate.get("eligibility") == "eligible", f"acquisition candidate {ordinal} selected while ineligible")
    if all(isinstance(candidate, dict) for candidate in candidates):
        _require(errors, candidates == sorted(candidates, key=_candidate_sort_key), "packet acquisition candidate order drift")
        stable_ids = [candidate.get("stable_id") for candidate in candidates]
        _require(errors, len(stable_ids) == len(set(stable_ids)), "packet acquisition candidate IDs must be unique")

    relationships = acquisition.get("relationships")
    _require(errors, isinstance(relationships, list), "packet acquisition relationships must be a list")
    if not isinstance(relationships, list):
        relationships = []
    relationships_valid = True
    for relationship in relationships:
        valid = (
            isinstance(relationship, dict)
            and set(relationship) == set(PACKET_RELATIONSHIP_FIELDS)
            and isinstance(relationship.get("source"), str)
            and bool(relationship["source"])
            and isinstance(relationship.get("target"), str)
            and bool(relationship["target"])
            and relationship.get("edge_type") in EDGE_TYPE_ORDINAL
            and relationship.get("direction") in DIRECTION_ORDINAL
            and type(relationship.get("locus_ordinal")) is int
            and 1 <= relationship["locus_ordinal"] <= len(loci)
            and type(relationship.get("cli_ordinal")) is int
            and relationship["cli_ordinal"] >= 1
        )
        _require(
            errors,
            valid,
            "packet acquisition relationship field set drift",
        )
        relationships_valid = relationships_valid and valid
    if relationships_valid:
        _require(errors, relationships == sorted(relationships, key=_relationship_sort_key), "packet acquisition relationship order drift")
        relationship_tuples = [tuple(relationship[field] for field in PACKET_RELATIONSHIP_FIELDS) for relationship in relationships]
        _require(errors, len(relationship_tuples) == len(set(relationship_tuples)), "packet acquisition duplicate relationship")
        new_file_ordinals = {
            locus.get("ordinal")
            for locus in loci
            if isinstance(locus, dict) and locus.get("source_kind") == "new_file"
        }
        _require(
            errors,
            not any(
                relationship.get("locus_ordinal") in new_file_ordinals
                for relationship in relationships
            ),
            "new-file loci must not carry traversal relationships",
        )
    else:
        relationship_tuples = []

    omissions = acquisition.get("omissions")
    _require(errors, isinstance(omissions, list), "packet acquisition omissions must be a list")
    if not isinstance(omissions, list):
        omissions = []
    omission_by_id: dict[str, dict[str, Any]] = {}
    for ordinal, omission in enumerate(omissions, 1):
        _require(errors, isinstance(omission, dict), f"acquisition omission {ordinal} must be an object")
        if not isinstance(omission, dict):
            continue
        _require(errors, set(omission) == set(ACQUISITION_OMISSION_FIELDS), f"acquisition omission {ordinal} field set drift")
        candidate_id = omission.get("candidate_stable_id")
        _require(errors, isinstance(candidate_id, str) and bool(candidate_id), f"acquisition omission {ordinal} candidate ID is invalid")
        _require(errors, candidate_id not in omission_by_id, f"duplicate acquisition omission for {candidate_id}")
        omission_by_id[candidate_id] = omission
        reason = omission.get("reason")
        _require(errors, reason in OMISSION_REASONS, f"acquisition omission {ordinal} reason drift")
        byte_fields = (omission.get("required_bytes"), omission.get("remaining_budget_bytes"))
        if reason in {"maximum_candidates", "full_body_budget"}:
            _require(errors, all(type(value) is int and value >= 0 for value in byte_fields), f"acquisition omission {ordinal} budget fields are invalid")
        else:
            _require(errors, byte_fields == (None, None), f"acquisition omission {ordinal} eligibility budget fields must be null")

    for candidate in candidates:
        if not isinstance(candidate, dict):
            continue
        candidate_id = candidate.get("stable_id")
        omission = omission_by_id.get(candidate_id)
        if candidate.get("selected") is True:
            _require(errors, omission is None, f"selected candidate {candidate_id} must not be omitted")
        else:
            _require(errors, omission is not None, f"unselected candidate {candidate_id} requires one omission")
            if omission is not None and candidate.get("eligibility") != "eligible":
                _require(errors, omission.get("reason") == candidate.get("eligibility"), f"candidate {candidate_id} omission reason mismatch")
            elif omission is not None:
                _require(errors, omission.get("reason") in {"maximum_candidates", "full_body_budget"}, f"eligible candidate {candidate_id} omission reason mismatch")
    _require(
        errors,
        set(omission_by_id).issubset({candidate.get("stable_id") for candidate in candidates if isinstance(candidate, dict)}),
        "acquisition omission references an unknown candidate",
    )
    _require(
        errors,
        [omission.get("candidate_stable_id") for omission in omissions if isinstance(omission, dict)]
        == [
            candidate.get("stable_id")
            for candidate in candidates
            if isinstance(candidate, dict) and candidate.get("selected") is False
        ],
        "packet acquisition omission order drift",
    )

    packet_loci = [record for record in records if isinstance(record, dict) and record.get("kind") == "locus"]
    packet_candidates = [record for record in records if isinstance(record, dict) and record.get("kind") == "candidate"]
    selected_candidates = [candidate for candidate in candidates if isinstance(candidate, dict) and candidate.get("selected") is True]
    _require(errors, len(packet_loci) == len(loci), "packet locus records do not match acquisition loci")
    _require(errors, len(packet_candidates) == len(selected_candidates), "packet candidate records do not match acquisition selection")
    for locus, record in zip(loci, packet_loci):
        if not isinstance(locus, dict):
            continue
        header = record.get("header", {})
        payload = record.get("full_payload")
        _require(
            errors,
            isinstance(header, dict)
            and header.get("stable_id") == locus.get("stable_id")
            and header.get("source_kind") == locus.get("source_kind")
            and header.get("path") == locus.get("path")
            and header.get("start_line") == locus.get("start_line")
            and header.get("end_line") == locus.get("end_line")
            and header.get("language") == locus.get("language"),
            f"packet locus {locus.get('ordinal')} header does not match acquisition",
        )
        if isinstance(payload, str):
            payload_bytes = payload.encode("utf-8")
            _require(
                errors,
                len(payload_bytes) == locus.get("preimage_byte_length")
                and sha256_bytes(payload_bytes) == locus.get("preimage_sha256"),
                f"packet locus {locus.get('ordinal')} payload does not match acquisition",
            )
    for candidate, record in zip(selected_candidates, packet_candidates):
        header = record.get("header", {})
        _require(
            errors,
            isinstance(header, dict)
            and header.get("source_kind") == "rna_node"
            and header.get("stable_id") == candidate.get("stable_id")
            and header.get("path") == candidate.get("path")
            and header.get("start_line") == candidate.get("start_line")
            and header.get("end_line") == candidate.get("end_line")
            and header.get("language") == candidate.get("language")
            and header.get("full_body_byte_length") == candidate.get("full_body_byte_length")
            and header.get("full_body_sha256") == candidate.get("full_body_sha256")
            and header.get("score")
            == {
                "semantic_component": candidate.get("semantic_component"),
                "graph_component": candidate.get("graph_component"),
                "total": candidate.get("total"),
            },
            f"packet candidate {candidate.get('acquisition_ordinal')} does not match acquisition",
        )
    relationship_set = set(relationship_tuples)
    for record in records:
        if not isinstance(record, dict) or not isinstance(record.get("header"), dict):
            continue
        for relationship in record["header"].get("relationships", []):
            if isinstance(relationship, dict) and set(relationship) == set(PACKET_RELATIONSHIP_FIELDS):
                relationship_tuple = tuple(relationship[field] for field in PACKET_RELATIONSHIP_FIELDS)
                _require(errors, relationship_tuple in relationship_set, "packet header relationship missing from acquisition record")


def assemble_retry_prompt_vector(vector: dict[str, Any]) -> bytes:
    previous = "".join(
        run["text"] * run["repeat"] for run in vector["previous_response_codepoint_runs"]
    )
    request = vector["base_prompt"] + RETRY_SUFFIX.format(
        prev=previous[-6000:], feedback=vector["feedback"]
    )
    return request.encode("utf-8")


def validate_retry_prompt_vector(vector: Any, errors: list[str]) -> None:
    expected_fields = {
        "base_prompt",
        "previous_response_codepoint_runs",
        "feedback",
        "expected_previous_response_codepoints",
        "expected_retained_previous_codepoints",
        "expected_retry_request_byte_length",
        "expected_retry_request_sha256",
    }
    _require(errors, isinstance(vector, dict) and set(vector) == expected_fields, "retry prompt vector field set drift")
    if not isinstance(vector, dict):
        return
    _require(errors, isinstance(vector.get("base_prompt"), str), "retry prompt vector base prompt must be text")
    _require(errors, isinstance(vector.get("feedback"), str), "retry prompt vector feedback must be text")
    runs = vector.get("previous_response_codepoint_runs")
    _require(errors, isinstance(runs, list) and bool(runs), "retry prompt vector runs must be non-empty")
    if not isinstance(runs, list):
        return
    valid_runs = True
    for run in runs:
        valid = (
            isinstance(run, dict)
            and set(run) == {"text", "repeat"}
            and isinstance(run.get("text"), str)
            and bool(run["text"])
            and type(run.get("repeat")) is int
            and 1 <= run["repeat"] <= 10_000
        )
        _require(errors, valid, "retry prompt vector codepoint run is invalid")
        valid_runs = valid_runs and valid
    if not valid_runs or not isinstance(vector.get("base_prompt"), str) or not isinstance(vector.get("feedback"), str):
        return
    previous = "".join(run["text"] * run["repeat"] for run in runs)
    _require(errors, len(previous) <= 20_000, "retry prompt vector expansion is too large")
    _require(errors, len(previous) > 6000, "retry prompt vector must exercise truncation")
    _require(errors, len(previous.encode("utf-8")) > len(previous), "retry prompt vector must exercise Unicode codepoint slicing")
    _require(errors, len(previous) == vector.get("expected_previous_response_codepoints"), "retry prompt previous-response length drift")
    _require(errors, len(previous[-6000:]) == vector.get("expected_retained_previous_codepoints"), "retry prompt retained-response length drift")
    request = assemble_retry_prompt_vector(vector)
    _require(errors, len(request) == vector.get("expected_retry_request_byte_length"), "retry prompt byte length drift")
    _require(errors, sha256_bytes(request) == vector.get("expected_retry_request_sha256"), "retry prompt byte digest drift")


def validate_packet_vector(vector: dict[str, Any], errors: list[str]) -> None:
    initial_error_count = len(errors)
    _require(errors, vector.get("schema_version") == 1, "packet vector schema_version drift")
    _require(
        errors,
        set(vector)
        == {
            "schema_version",
            "metadata",
            "records",
            "retry_prompt_vector",
            "expected_b_sha256",
            "expected_c_sha256",
        },
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
        validate_acquisition_vector(metadata.get("acquisition"), records, errors)
    for ordinal, source in enumerate(records, 1):
        _require(errors, isinstance(source, dict), f"packet record {ordinal} must be an object")
        if not isinstance(source, dict):
            continue
        _require(
            errors,
            set(source) == {"kind", "header", "full_payload", "minified_payload"},
            f"packet record {ordinal} field set drift",
        )
        _require(errors, source.get("kind") in {"locus", "candidate"}, f"packet record {ordinal} kind drift")
        header = source.get("header", {})
        _require(errors, isinstance(header, dict), f"packet record {ordinal} header must be an object")
        if not isinstance(header, dict):
            continue
        _require(errors, set(header) == set(PACKET_HEADER_FIELDS), f"packet record {ordinal} header field set drift")
        _require(errors, header.get("ordinal") == ordinal, f"packet record {ordinal} ordinal drift")
        _require(errors, header.get("kind") == source.get("kind"), f"packet record {ordinal} kind drift")
        _require(
            errors,
            header.get("source_kind")
            in (LOCUS_SOURCE_KINDS if source.get("kind") == "locus" else {"rna_node"}),
            f"packet record {ordinal} source_kind drift",
        )
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
            _require(errors, header.get("score") is None, f"packet locus {ordinal} score must be null")
        elif isinstance(full_payload, str) and isinstance(minified_payload, str):
            _require(
                errors,
                isinstance(header.get("score"), dict)
                and set(header["score"])
                == {"semantic_component", "graph_component", "total"},
                f"packet candidate {ordinal} score field set drift",
            )
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
    validate_retry_prompt_vector(vector.get("retry_prompt_vector"), errors)
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
    expected_artifact_receipt_digest: str | None = None,
    expected_budget_receipt_digest: str | None = None,
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
        validate_runtime(
            runtime,
            errors,
            allow_authorized=True,
            expected_bundle_digest=expected_digest,
            expected_artifact_receipt_digest=expected_artifact_receipt_digest,
            expected_budget_receipt_digest=expected_budget_receipt_digest,
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
    parser.add_argument(
        "--expected-artifact-receipt-digest",
        help="externally anchored canonical qualified-artifact receipt SHA-256",
    )
    parser.add_argument(
        "--expected-budget-receipt-digest",
        help="externally anchored canonical approved-budget receipt SHA-256",
    )
    parser.add_argument("--json", action="store_true", help="emit the compatibility record as JSON")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = validate_bundle(
            args.root,
            args.expected_digest,
            args.runtime_config,
            args.expected_artifact_receipt_digest,
            args.expected_budget_receipt_digest,
        )
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
