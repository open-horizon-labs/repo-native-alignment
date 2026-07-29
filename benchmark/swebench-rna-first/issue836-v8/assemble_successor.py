#!/usr/bin/env python3
"""Assemble fresh #836 successor episodes over the immutable v4 assembler.

The predecessor wave exposed two operational representation defects before any
provider request completed:

* the qualified rank-1 readiness report uses the current report shape, while
  the frozen runner consumes the legacy ``status/readiness`` projection;
* the successor must use new episode sessions and fresh checkouts.

This adapter preserves and validates the raw cache evidence, writes a small
derived readiness projection beside the new rolling evidence, and delegates
all other assembly work to the byte-pinned v4 assembler.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Any, Mapping, Sequence


sys.dont_write_bytecode = True
ADAPTER = Path(__file__).resolve()
REPO = ADAPTER.parents[3]
BASE_ASSEMBLER = Path(
    "/Users/muness/swebench-evidence/issue836-selector-20case-20260724/"
    "run-assembly/assemble_run.py"
)
BASE_ASSEMBLER_SHA256 = (
    "72744c08c98467d15e171bcb5f1de95ddfad809e177559240b73c95b4b0666ee"
)
BRIDGE_SCHEMA = "issue836-readiness-compatibility-bridge-v1"
RANK_1 = 1
RANK_1_CASE = "sympy__sympy-23534"
HEX_64 = re.compile(r"[0-9a-f]{64}")


def sha_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def successor_explicit_ranks(values: Sequence[int]) -> tuple[int, ...]:
    """Return up to three unique ranks in frozen-selection order."""

    require(values, "at least one explicit --rank is required")
    require(len(values) <= 3, "at most three different cases may be requested")
    require(
        all(type(rank) is int and 1 <= rank <= 20 for rank in values),
        "requested ranks must be integers from 1 through 20",
    )
    require(len(set(values)) == len(values), "requested ranks must be unique")
    require(
        tuple(values) == tuple(sorted(values)),
        "requested ranks must be supplied in increasing frozen-selection order",
    )
    return tuple(values)


def git(*arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(REPO), *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    require(
        result.returncode == 0,
        "source Git command failed: "
        + result.stderr.decode(errors="replace").strip(),
    )
    return result.stdout.decode("ascii").strip()


def load_base() -> Any:
    require(
        BASE_ASSEMBLER.is_file()
        and not BASE_ASSEMBLER.is_symlink()
        and sha_file(BASE_ASSEMBLER) == BASE_ASSEMBLER_SHA256,
        "immutable base assembler identity drift",
    )
    spec = importlib.util.spec_from_file_location(
        "issue836_v4_immutable_assembler",
        BASE_ASSEMBLER,
    )
    require(spec is not None and spec.loader is not None, "cannot load base assembler")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def canonical(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")


def write_or_verify(path: Path, value: Mapping[str, Any]) -> None:
    encoded = canonical(value)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() or path.is_symlink():
        require(
            path.is_file()
            and not path.is_symlink()
            and path.read_bytes() == encoded,
            f"successor bridge drift: {path}",
        )
        return
    with path.open("xb") as handle:
        handle.write(encoded)


def rolling_root(argv: Sequence[str]) -> Path | None:
    for index, value in enumerate(argv):
        if value == "--rolling-root" and index + 1 < len(argv):
            candidate = Path(argv[index + 1])
            require(candidate.is_absolute(), "rolling root must be absolute")
            return candidate.resolve(strict=False)
    return None


def configure(base: Any, argv: Sequence[str]) -> None:
    commit = git("rev-parse", "HEAD^{commit}")
    tree = git("rev-parse", "HEAD^{tree}")
    require(
        re.fullmatch(r"[0-9a-f]{40}", commit) is not None,
        "source commit identity invalid",
    )
    require(
        re.fullmatch(r"[0-9a-f]{40}", tree) is not None,
        "source tree identity invalid",
    )
    if list(argv) != ["self-test"]:
        require(
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(REPO),
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=no",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            ).stdout
            == b"",
            "tracked source must be pristine",
        )
    # Keep the byte-qualified v4 registration check pointed at its original
    # source root.  The successor implementation tree intentionally contains
    # a newer runner/common supervisor; those live bytes are bound separately
    # by the v10 schedule and every assembled wave manifest.
    qualified_harness = Path(base.HARNESS).resolve(strict=True)
    bridge_root = rolling_root(argv)
    base.ASSEMBLER = ADAPTER
    # The immutable assembler was originally qualified in a different ordinary
    # clone. Rebind only its source paths to this byte-identical pristine clone
    # so Git identity and every registered file reference describe one tree.
    base.SOURCE_CLONE = REPO
    base.HARNESS = REPO / "benchmark/swebench-rna-first/issue827"
    base.ISSUE836 = REPO / "benchmark/swebench-rna-first/issue836-v4"
    base.REGISTRATION = base.ISSUE836 / "registration.json"
    base.SELECTION = base.ISSUE836 / "selection.json"
    base.SOURCE_COMMIT = commit
    base.SOURCE_TREE = tree
    original_validate_frozen_inputs = base.validate_frozen_inputs

    def validate_successor_frozen_inputs() -> tuple[dict, dict]:
        successor_harness = base.HARNESS
        base.HARNESS = qualified_harness
        try:
            return original_validate_frozen_inputs()
        finally:
            base.HARNESS = successor_harness

    base.validate_frozen_inputs = validate_successor_frozen_inputs
    if list(argv) == ["self-test"]:
        original_synthetic_self_test = base.synthetic_self_test

        def successor_synthetic_self_test() -> int:
            captured = io.StringIO()
            with contextlib.redirect_stdout(captured):
                result = original_synthetic_self_test()
            require(result == 0, "immutable assembler self-test failed")
            report = json.loads(captured.getvalue())
            tests = report.get("tests")
            require(isinstance(tests, list), "immutable self-test report drift")
            require(
                successor_explicit_ranks([1, 12, 20]) == (1, 12, 20),
                "three-rank successor scope self-test failed",
            )
            try:
                successor_explicit_ranks([1, 2, 3, 4])
            except RuntimeError:
                pass
            else:
                raise RuntimeError("four-rank successor scope did not fail closed")
            report["tests"] = [
                test
                for test in tests
                if test
                not in {
                    "explicit_one_or_two_rank_scope",
                    "three_rank_wave_rejected",
                }
            ]
            report["tests"][:0] = [
                "explicit_one_to_three_rank_scope",
                "four_rank_wave_rejected",
            ]
            print(json.dumps(report, sort_keys=True, indent=2))
            return 0

        base.synthetic_self_test = successor_synthetic_self_test
    else:
        # The immutable v4 assembler's parser already accepts repeated
        # --rank arguments. Replace only its two-case validation boundary so
        # the v17 runner can occupy its registered three independent case
        # lanes without changing cohort membership or within-case arm order.
        base.explicit_ranks = successor_explicit_ranks
    original_validate = base.validate_cache_envelope

    def validate_cache_envelope(
        path: Path,
        case: Mapping[str, Any],
        registration: Mapping[str, Any],
    ) -> dict[str, Any]:
        envelope = original_validate(path, case, registration)
        if case.get("rank") != RANK_1:
            return envelope
        require(
            case.get("instance_id") == RANK_1_CASE,
            "rank-1 readiness bridge case drift",
        )
        require(
            bridge_root is not None,
            "rank-1 readiness bridge requires --rolling-root",
        )
        raw_input = base.load_json(path)
        raw_ref = raw_input["readiness_report"]
        raw_path = Path(raw_ref["path"]).resolve(strict=True)
        require(
            base.ref(raw_path) == raw_ref,
            "rank-1 raw readiness reference drift",
        )
        raw = base.load_json(raw_path)
        report = raw.get("report")
        report_digest = report.get("digest") if isinstance(report, dict) else None
        require(
            raw.get("ready") is True
            and raw.get("compatibility_violations") == []
            and isinstance(report_digest, str)
            and HEX_64.fullmatch(report_digest) is not None,
            "rank-1 raw readiness semantics are not bridgeable",
        )
        case_root = (
            bridge_root
            / "readiness-bridges"
            / f"rank-{RANK_1:02d}-{RANK_1_CASE}"
        )
        bridge_path = case_root / "readiness-bridge.json"
        bridge = {
            "schema_version": BRIDGE_SCHEMA,
            "status": "READY",
            "readiness": {
                "ready": True,
                "compatibility_violations": [],
                "report_digest": report_digest,
            },
            "compatibility_bridge": {
                "case_id": RANK_1_CASE,
                "rank": RANK_1,
                "raw_readiness_report": raw_ref,
                "verification_receipt": raw_input["verification_receipt"],
                "derivation": {
                    "status": "literal_READY",
                    "readiness.ready": "raw.ready",
                    "readiness.compatibility_violations": (
                        "raw.compatibility_violations"
                    ),
                    "readiness.report_digest": "raw.report.digest",
                },
                "raw_evidence_modified": False,
                "cohort_or_arm_order_changed": False,
            },
        }
        write_or_verify(bridge_path, bridge)
        derived_input = dict(raw_input)
        derived_input["readiness_report"] = base.ref(bridge_path)
        derived_path = case_root / "run-selector-cache-input.json"
        write_or_verify(derived_path, derived_input)
        result = dict(envelope)
        result["input"] = base.ref(derived_path)
        return result

    base.validate_cache_envelope = validate_cache_envelope


def main(argv: Sequence[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    try:
        base = load_base()
        configure(base, arguments)
        return base.main(arguments)
    except (OSError, RuntimeError) as exc:
        print(f"FAIL CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
