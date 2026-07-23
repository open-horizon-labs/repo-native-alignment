from __future__ import annotations

import importlib.util
import json
from pathlib import Path


SOURCE = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "issue825_verify_published_result", SOURCE / "verify_published_result.py"
)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_published_evidence_verifies() -> None:
    result = MODULE.verify(SOURCE / "evidence/amended-selector", SOURCE)
    assert result["valid"] is True
    assert result["decision"] == "selected_T"
    assert result["checks"]["original_xarray_symmetric_wall_timeout"] is True
    assert result["checks"]["fresh_amended_xarray_sessions"] is True
    assert result["checks"]["official_evaluations_once"] == 4
    assert result["aggregates"]["A"]["resolved"] == 2
    assert result["aggregates"]["T"]["resolved"] == 2


def test_tampered_original_timeout_fails_closed(tmp_path: Path) -> None:
    root = tmp_path / "evidence"
    source = SOURCE / "evidence/amended-selector"
    import shutil

    shutil.copytree(source, root)
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
    root = tmp_path / "evidence"
    source = SOURCE / "evidence/amended-selector"
    import shutil

    shutil.copytree(source, root)
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
