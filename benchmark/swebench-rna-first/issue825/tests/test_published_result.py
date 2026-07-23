from __future__ import annotations

import importlib.util
import hashlib
import json
import shutil
from pathlib import Path

import pytest


SOURCE = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "issue825_verify_published_result", SOURCE / "verify_published_result.py"
)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def copied_evidence(tmp_path: Path) -> Path:
    root = tmp_path / "evidence"
    shutil.copytree(SOURCE / "evidence/amended-selector", root)
    return root


def rewrite(root: Path, relative: str, mutate: object) -> None:
    path = root / relative
    value = json.loads(path.read_text())
    mutate(value)  # type: ignore[operator]
    path.write_text(json.dumps(value, sort_keys=True) + "\n")


def rejects(root: Path) -> None:
    with pytest.raises(MODULE.EvidenceError):
        MODULE.verify(root, SOURCE)


def test_published_evidence_verifies() -> None:
    result = MODULE.verify(SOURCE / "evidence/amended-selector", SOURCE)
    assert result["valid"] is True
    assert result["protocol_classification"] == "amended_development_selector"
    assert result["decision"] == "no_RNA_treatment"
    assert result["decision_classification"] == "treatment_noncompliance"
    assert result["decision_reason"] == "at least one T episode failed the mandatory RNA-first manipulation contract"
    assert result["checks"]["original_xarray_symmetric_wall_timeout"] is True
    assert result["checks"]["fresh_amended_xarray_sessions"] is True
    assert result["checks"]["official_evaluations_once"] == 4
    assert result["checks"]["xarray_T_policy_compliant"] is False
    assert result["checks"]["xarray_T_forbidden_attempts_proven"] == 2
    assert result["checks"]["erroneous_post_nonadherence_evaluator_invocations"] == 1
    assert result["aggregates"]["A"]["resolved"] == 2
    assert result["aggregates"]["T"]["resolved"] == 2


def test_xarray_t_frozen_policy_violations_are_proven_from_the_ledger() -> None:
    root = SOURCE / "evidence/amended-selector"
    violations = MODULE.prove_xarray_t_policy_violations(
        SOURCE,
        root / "final/model-evidence/xarray/T/actor-tool-ledger.json",
    )
    assert [item["model_action_index"] for item in violations] == [27, 37]
    assert [item["classification"] for item in violations] == [
        "forbidden_network_access_attempt",
        "forbidden_other_arm_evidence_access_attempt",
    ]
    assert all(item["allow_decisions"] == {"common": "allow", "treatment": "allow"} for item in violations)


def test_tampered_original_timeout_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    path = root / "original-xarray/A/episode-receipt.json"
    value = json.loads(path.read_text())
    value["timed_out"] = False
    path.write_text(json.dumps(value, sort_keys=True) + "\n")
    try:
        MODULE.verify(root, SOURCE)
    except MODULE.EvidenceError:
        pass
    else:
        raise AssertionError("tampered timeout evidence must fail closed")


def test_tampered_selection_result_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    path = root / "final/selection-result.json"
    value = json.loads(path.read_text())
    value["decision"] = "no_RNA_treatment"
    path.write_text(json.dumps(value, sort_keys=True) + "\n")
    try:
        MODULE.verify(root, SOURCE)
    except MODULE.EvidenceError:
        pass
    else:
        raise AssertionError("tampered final result must fail closed")


def test_tampered_superseding_correction_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(
        root,
        "final/superseding-selection-correction.json",
        lambda value: value["corrected_result"].__setitem__("decision", "selected_T"),
    )
    rejects(root)


@pytest.mark.parametrize(
    "relative",
    ["README.md", "SHA256SUMS", "verification-receipt.json"],
)
def test_published_manifest_and_receipt_tamper_fail_closed(
    tmp_path: Path, relative: str
) -> None:
    root = copied_evidence(tmp_path)
    with (root / relative).open("ab") as stream:
        stream.write(b"tamper")
    rejects(root)


def test_verification_level_evaluator_feedback_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, "final/xarray/A/episode-verification.json", lambda value: value.__setitem__("official_evaluator_invoked", True))
    rejects(root)


def test_receipt_session_must_match_command_session(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, "final/xarray/A/episode-receipt.json", lambda value: value.__setitem__("session_id", "different-session"))
    rejects(root)


def test_resume_lineage_is_rejected(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, "final/xarray/T/episode-receipt.json", lambda value: value["command"].extend(["--resume", "prior-session"]))
    rejects(root)


def test_invalid_usage_ledger_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, "final/django/A/episode-receipt.json", lambda value: value["token_ledger"].__setitem__("provider_total_tokens", 0))
    rejects(root)


def test_terminal_patch_evaluator_link_drift_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, "final/evaluations/django/T/evaluation.receipt.json", lambda value: value["official_outputs"]["patch"].__setitem__("sha256", "0" * 64))
    rejects(root)


def test_injected_projection_anchor_drift_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, "final/evidence-trust-anchor.json", lambda value: value["projections"][0].__setitem__("base64", ""))
    rejects(root)


def test_unexpected_secret_file_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    (root / "final/.env").write_text("API_TOKEN=should-not-be-here\n")
    rejects(root)


def test_secret_in_allowed_file_fails_closed(tmp_path: Path) -> None:
    root = copied_evidence(tmp_path)
    with (root / "README.md").open("a") as stream:
        stream.write("ANTHROPIC_API_KEY=not-a-real-secret\n")
    rejects(root)


@pytest.mark.parametrize(
    "relative",
    [
        "final/model-evidence/xarray/T/actor-tool-ledger.json",
        "final/model-evidence/xarray/T/treatment-system.bin",
        "final/model-evidence/django/T/actor-tool-ledger.json",
        "final/model-evidence/django/T/treatment-system.bin",
    ],
)
def test_exact_t_evidence_tamper_fails_closed(tmp_path: Path, relative: str) -> None:
    root = copied_evidence(tmp_path)
    with (root / relative).open("ab") as stream:
        stream.write(b"tamper")
    rejects(root)


def test_embedded_official_report_digest_tamper_fails_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = copied_evidence(tmp_path)
    path = root / "final/evidence-trust-anchor.json"
    anchor = json.loads(path.read_text())
    anchor["reports"][0]["base64"] = "e30="
    path.write_text(json.dumps(anchor, sort_keys=True) + "\n")
    monkeypatch.setattr(MODULE, "TRUST_ANCHOR", hashlib.sha256(path.read_bytes()).hexdigest())
    rejects(root)


def test_registered_source_tamper_fails_closed(tmp_path: Path) -> None:
    issue_dir = tmp_path / "issue825"
    shutil.copytree(SOURCE, issue_dir, ignore=shutil.ignore_patterns("evidence", "__pycache__"))
    with (issue_dir / "rna_traverse.py").open("a") as stream:
        stream.write("\n# tamper\n")
    with pytest.raises(MODULE.EvidenceError):
        MODULE.verify(SOURCE / "evidence/amended-selector", issue_dir)


@pytest.mark.parametrize(
    ("relative", "field", "value"),
    [
        ("final/run-manifest.json", "schema_version", "tampered"),
        ("final/preparation-receipt.json", "source_commit", "0" * 40),
        ("final/evaluator-static-preflight.receipt.json", "script_sha256", "0" * 64),
        ("final/evaluation-batch.receipt.json", "official_evaluations_started", 3),
    ],
)
def test_anchored_source_and_count_drift_fails_closed(
    tmp_path: Path, relative: str, field: str, value: object
) -> None:
    root = copied_evidence(tmp_path)
    rewrite(root, relative, lambda document: document.__setitem__(field, value))
    rejects(root)
