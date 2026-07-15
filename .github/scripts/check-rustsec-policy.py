#!/usr/bin/env python3
"""Fail closed on RustSec vulnerabilities and undeclared warning decisions."""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


CARGO_AUDIT_VERSION = "0.22.2"
REQUIRED_FIELDS = {
    "advisory_id",
    "package",
    "version",
    "kind",
    "dependency_paths",
    "rationale",
    "owner",
    "removal_issue",
    "review_triggers",
    "expires",
    "approved_by",
    "approval_evidence",
}
ADVISORY_ID = re.compile(r"^RUSTSEC-\d{4}-\d{4}$")
ISSUE_URL = re.compile(
    r"^https://github\.com/open-horizon-labs/repo-native-alignment/issues/\d+$"
)


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"cannot read JSON from {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def finding_key(finding: dict[str, Any], kind: str) -> tuple[str, str, str, str]:
    advisory = finding.get("advisory", {})
    package = finding.get("package", {})
    if not isinstance(advisory, dict):
        advisory = {}
    if not isinstance(package, dict):
        package = {}
    return (
        str(advisory.get("id", "")),
        str(package.get("name", "")),
        str(package.get("version", "")),
        kind,
    )


def policy_key(entry: dict[str, Any]) -> tuple[str, str, str, str]:
    return (
        str(entry.get("advisory_id", "")),
        str(entry.get("package", "")),
        str(entry.get("version", "")),
        str(entry.get("kind", "")),
    )


def validate_policy(policy: dict[str, Any], today: dt.date) -> list[str]:
    errors: list[str] = []
    if policy.get("schema_version") != 1:
        errors.append("policy schema_version must equal 1")
    entries = policy.get("warnings")
    if not isinstance(entries, list):
        return errors + ["policy warnings must be a list"]

    seen: set[tuple[str, str, str, str]] = set()
    for index, entry in enumerate(entries):
        prefix = f"policy warnings[{index}]"
        if not isinstance(entry, dict):
            errors.append(f"{prefix} must be an object")
            continue
        missing = sorted(REQUIRED_FIELDS - entry.keys())
        if missing:
            errors.append(f"{prefix} is missing: {', '.join(missing)}")
        key = policy_key(entry)
        if key in seen:
            errors.append(f"{prefix} duplicates {' / '.join(key)}")
        seen.add(key)
        advisory, package, version, kind = key
        if not ADVISORY_ID.fullmatch(advisory):
            errors.append(f"{prefix} has invalid advisory_id {advisory!r}")
        if not package or not version or "*" in version:
            errors.append(f"{prefix} must name an exact package version")
        if kind not in {"unmaintained", "unsound", "notice"}:
            errors.append(f"{prefix} has unsupported warning kind {kind!r}")
        for field in ("dependency_paths", "review_triggers"):
            values = entry.get(field)
            if not isinstance(values, list) or not values or not all(
                isinstance(value, str) and value.strip() for value in values
            ):
                errors.append(f"{prefix} {field} must be a non-empty string list")
        for field in ("rationale", "owner", "approved_by", "approval_evidence"):
            if not isinstance(entry.get(field), str) or not entry[field].strip():
                errors.append(f"{prefix} {field} must be non-empty")
        if kind == "unsound" and not str(entry.get("approval_evidence", "")).strip():
            errors.append(f"{prefix} unsound exception requires human approval evidence")
        if not ISSUE_URL.fullmatch(str(entry.get("removal_issue", ""))):
            errors.append(f"{prefix} removal_issue must link an RNA GitHub issue")
        try:
            expiry = dt.date.fromisoformat(str(entry.get("expires", "")))
            if expiry < today:
                errors.append(f"{prefix} expired on {expiry.isoformat()}")
        except ValueError:
            errors.append(f"{prefix} expires must be an ISO date")
    return errors


def evaluate(report: dict[str, Any], policy: dict[str, Any], today: dt.date) -> list[str]:
    errors = validate_policy(policy, today)
    vulnerability_block = report.get("vulnerabilities")
    if not isinstance(vulnerability_block, dict):
        return errors + ["audit report is missing vulnerabilities"]
    vulnerabilities = vulnerability_block.get("list")
    if not isinstance(vulnerabilities, list):
        return errors + ["audit report vulnerabilities.list must be a list"]
    for finding in vulnerabilities:
        if not isinstance(finding, dict):
            errors.append("audit report contains a malformed vulnerability")
            continue
        advisory, package, version, _ = finding_key(finding, "vulnerability")
        errors.append(f"vulnerability {advisory}: {package} {version}")

    warning_block = report.get("warnings")
    if not isinstance(warning_block, dict):
        return errors + ["audit report is missing warnings"]
    findings: dict[tuple[str, str, str, str], dict[str, Any]] = {}
    for kind, group in warning_block.items():
        if not isinstance(group, list):
            errors.append(f"audit report warning group {kind!r} must be a list")
            continue
        for finding in group:
            if not isinstance(finding, dict):
                errors.append(f"audit report warning group {kind!r} is malformed")
                continue
            key = finding_key(finding, str(kind))
            if key in findings:
                errors.append(f"audit report duplicates warning {' / '.join(key)}")
            findings[key] = finding

    policy_entries = policy.get("warnings", [])
    declared = {
        policy_key(entry): entry for entry in policy_entries if isinstance(entry, dict)
    }
    for key in sorted(findings.keys() - declared.keys()):
        errors.append(f"undeclared warning {' / '.join(key)}")
    for key in sorted(declared.keys() - findings.keys()):
        errors.append(f"stale policy warning {' / '.join(key)}")
    return errors


def print_report(report: dict[str, Any], policy: dict[str, Any]) -> None:
    database = report.get("database", {})
    lockfile = report.get("lockfile", {})
    print(
        "RustSec database "
        f"{database.get('last-commit', 'unknown')} | "
        f"{lockfile.get('dependency-count', 'unknown')} locked dependencies"
    )
    entries = policy.get("warnings", [])
    declared = {
        policy_key(entry): entry for entry in entries if isinstance(entry, dict)
    }
    findings: list[tuple[tuple[str, str, str, str], dict[str, Any]]] = []
    warning_block = report.get("warnings", {})
    if isinstance(warning_block, dict):
        for kind, group in warning_block.items():
            if isinstance(group, list):
                findings.extend(
                    (finding_key(finding, str(kind)), finding)
                    for finding in group
                    if isinstance(finding, dict)
                )
    for key, _ in sorted(findings):
        advisory, package, version, kind = key
        entry = declared.get(key)
        expiry = entry.get("expires", "UNDECLARED") if entry else "UNDECLARED"
        print(
            "WARN allowed until "
            f"{expiry}: {advisory} {package} {version} ({kind})"
        )
        if entry:
            for path in entry.get("dependency_paths", []):
                print(f"  path: {path}")
            print(
                f"  owner: {entry.get('owner', 'MISSING')} | "
                f"removal: {entry.get('removal_issue', 'MISSING')}"
            )


def run_live() -> tuple[dict[str, Any], int]:
    version = subprocess.run(
        ["cargo", "audit", "--version"], capture_output=True, text=True, check=False
    )
    expected = re.compile(
        rf"^cargo-audit(?:-audit)? {re.escape(CARGO_AUDIT_VERSION)}$"
    )
    if version.returncode != 0 or not expected.fullmatch(version.stdout.strip()):
        raise ValueError(
            f"expected cargo-audit {CARGO_AUDIT_VERSION}; install with: "
            "cargo install cargo-audit "
            f"--version {CARGO_AUDIT_VERSION} --locked"
        )
    audit = subprocess.run(
        ["cargo", "audit", "--json"], capture_output=True, text=True, check=False
    )
    if audit.stderr:
        print(audit.stderr, file=sys.stderr, end="")
    try:
        report = json.loads(audit.stdout)
    except json.JSONDecodeError as exc:
        raise ValueError(f"cargo audit did not return JSON: {exc}") from exc
    if not isinstance(report, dict):
        raise ValueError("cargo audit report must be a JSON object")
    return report, audit.returncode


def self_test(script_dir: Path, policy: dict[str, Any]) -> list[str]:
    fixture_dir = script_dir.parent / "fixtures" / "rustsec"
    current = load_json(fixture_dir / "current-policy.json")
    vulnerable = load_json(fixture_dir / "vulnerability.json")
    today = dt.date(2026, 7, 15)
    failures: list[str] = []
    if evaluate(current, policy, today):
        failures.append("current-policy fixture should pass")
    if not any("vulnerability" in error for error in evaluate(vulnerable, policy, today)):
        failures.append("vulnerability fixture should fail on a vulnerability")

    unknown = copy.deepcopy(current)
    unknown["warnings"].setdefault("notice", []).append(
        {
            "kind": "notice",
            "package": {"name": "surprise", "version": "1.0.0"},
            "advisory": {"id": "RUSTSEC-2099-0001"},
        }
    )
    if not any("undeclared warning" in error for error in evaluate(unknown, policy, today)):
        failures.append("unknown warning should fail")

    stale = copy.deepcopy(current)
    stale["warnings"]["unmaintained"] = stale["warnings"]["unmaintained"][1:]
    if not any("stale policy" in error for error in evaluate(stale, policy, today)):
        failures.append("stale policy should fail")

    expired = copy.deepcopy(policy)
    expired["warnings"][0]["expires"] = "2026-07-14"
    if not any("expired" in error for error in evaluate(current, expired, today)):
        failures.append("expired policy should fail")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--policy", type=Path, default=Path("security/rustsec-policy.json")
    )
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--live", action="store_true")
    source.add_argument("--report", type=Path)
    source.add_argument("--self-test", action="store_true")
    parser.add_argument("--today", type=dt.date.fromisoformat, default=dt.date.today())
    args = parser.parse_args()
    try:
        policy = load_json(args.policy)
        if args.self_test:
            errors = self_test(Path(__file__).resolve().parent, policy)
            if errors:
                for error in errors:
                    print(f"FAIL: {error}", file=sys.stderr)
                return 1
            print("RustSec policy fixtures: pass")
            return 0
        if args.live:
            report, audit_status = run_live()
        else:
            report, audit_status = load_json(args.report), 0
        print_report(report, policy)
        errors = evaluate(report, policy, args.today)
        if audit_status != 0 and not any("vulnerability" in error for error in errors):
            errors.append(f"cargo audit exited with status {audit_status}")
        if errors:
            for error in errors:
                print(f"FAIL: {error}", file=sys.stderr)
            return 1
        print("RustSec policy decision: pass (0 vulnerabilities, all warnings declared)")
        return 0
    except (OSError, ValueError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
