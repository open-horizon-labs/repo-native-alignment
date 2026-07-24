#!/usr/bin/env python3
"""Prepare cache-only reserve cases with the qualified RNA artifact.

Reserve caches are deliberately outside the frozen issue836-v4 analysis
population.  This helper performs no model, provider, or evaluator action.  It
retains every attempt and writes enough immutable identity evidence for a
later, separately preregistered experiment to decide whether the cache can be
promoted.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from typing import Any, Mapping, Sequence


ROOT = Path(__file__).resolve().parent
PLAN = ROOT / "reserve-plan.json"
QUALIFICATION_ROOT = Path(
    "/Users/muness/swebench-evidence/"
    "issue829-ci-13c74539441adeef1ffd7d68b413ff148203f21c"
)
BUNDLE = QUALIFICATION_ROOT / "verified-bundle-official/rna-semantic-bundle"
RNA_BINARY = BUNDLE / "repo-native-alignment"
RNA_BINARY_SHA256 = (
    "d4d264da1a012b38814f0f2e9ee92f77c5aab3ed558a0f23abcd830d4b78ca94"
)
RUNTIME_SEED_ROOT = Path(
    "/Users/muness/swebench-evidence/issue836-selector-20case-20260724/"
    "cache-prep/cases/rank-08-django__django-11163-attempt-004/"
    "runtime/preprocessing/lsp/provisioned"
)
RUNTIME_SEED_RECEIPT = Path(
    "/Users/muness/swebench-evidence/issue836-selector-20case-20260724/"
    "cache-prep/cases/rank-08-django__django-11163-attempt-004/"
    "scan/runtime-environment-resume-receipt.json"
)
GIT_CACHE_ROOT = Path(
    "/Users/muness/swebench-evidence/issue836-selector-20case-20260724/"
    "cache-reserve-v1/git-cache"
)
GIT = Path("/usr/bin/git")
TIME = Path("/usr/bin/time")


class ReserveFailure(RuntimeError):
    """A cache-only reserve preparation precondition failed."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ReserveFailure(message)


def canonical(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode()


def sha_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def file_ref(path: Path) -> dict[str, Any]:
    require(path.is_file() and not path.is_symlink(), f"invalid file: {path}")
    return {
        "path": str(path.resolve()),
        "bytes": path.stat().st_size,
        "sha256": sha_file(path),
    }


def atomic_write(path: Path, value: Any) -> None:
    data = value if isinstance(value, bytes) else canonical(value)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    require(not temporary.exists(), f"temporary evidence path exists: {temporary}")
    with temporary.open("xb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError, UnicodeError) as error:
        raise ReserveFailure(f"unable to read {label}: {path}") from error
    require(isinstance(value, dict), f"{label} is not an object")
    return value


def git_environment() -> dict[str, str]:
    return {
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
        "HOME": "/var/empty",
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
        "TZ": "UTC",
    }


def run_checked(
    argv: Sequence[str],
    *,
    cwd: Path | None = None,
    environment: Mapping[str, str] | None = None,
) -> bytes:
    completed = subprocess.run(
        list(argv),
        cwd=cwd,
        env=dict(environment or git_environment()),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    require(
        completed.returncode == 0,
        "command failed: "
        + " ".join(argv)
        + f"; stderr={completed.stderr.decode(errors='replace')[-1000:]}",
    )
    return completed.stdout


def reserve_case(source_rank: int) -> dict[str, Any]:
    plan = load_json(PLAN, "reserve plan")
    require(
        plan.get("schema_version") == "issue836-cache-reserve-plan-v1",
        "reserve plan schema drift",
    )
    ranks = plan.get("reserved_source_ranks")
    require(
        isinstance(ranks, dict)
        and type(ranks.get("first")) is int
        and type(ranks.get("last")) is int
        and ranks["first"] <= source_rank <= ranks["last"],
        "source rank is outside the frozen cache reserve",
    )
    source = plan.get("ranking", {}).get("source")
    require(
        isinstance(source, dict)
        and set(source) == {"path", "bytes", "sha256"},
        "ranking source reference is invalid",
    )
    source_path = Path(source["path"])
    require(file_ref(source_path) == source, "ranking source reference drift")
    ranking = load_json(source_path, "ranking source")
    require(
        ranking.get("problem_statements_exposed") is False
        and ranking.get("gold_or_outcomes_inspected") is False
        and ranking.get("model_provider_evaluator_calls") == 0,
        "ranking source does not prove cache-only no-outcome selection",
    )
    candidates = ranking.get("next_candidates")
    require(isinstance(candidates, list), "ranking candidates missing")
    matches = [
        candidate
        for candidate in candidates
        if isinstance(candidate, dict) and candidate.get("rank") == source_rank
    ]
    require(len(matches) == 1, "source rank is not unique")
    candidate = matches[0]
    required = {
        "rank",
        "ranking_sha256",
        "instance_id",
        "repo",
        "base_commit",
    }
    require(
        required <= set(candidate)
        and all(isinstance(candidate[key], str) for key in required - {"rank"}),
        "reserve candidate identity is incomplete",
    )
    return {key: candidate[key] for key in sorted(required)}


def repo_slug_to_cache_name(repo: str) -> str:
    owner, name = repo.split("/", 1)
    require(bool(owner) and bool(name), "repository slug invalid")
    return f"{owner}__{name}.git"


def expected_output_name(case: Mapping[str, Any], attempt: int) -> str:
    return (
        f"source-rank-{case['rank']:02d}-{case['instance_id']}"
        f"-attempt-{attempt:03d}"
    )


def scan_environment(output_root: Path, provisioned: Path) -> dict[str, str]:
    environment_root = output_root / "runtime/environment"
    directories = {
        name: environment_root / name
        for name in (
            "home",
            "pip-cache",
            "tmp",
            "xdg-cache",
            "xdg-config",
            "xdg-data",
            "xdg-state",
            "npm-cache",
        )
    }
    for directory in directories.values():
        directory.mkdir(parents=True)
    java_home = (
        provisioned
        / "runtimes/jdk-21.0.11+10-jre/Contents/Home"
    )
    node_bin = provisioned / "runtimes/node-v22.12.0-darwin-arm64/bin"
    python_bin = provisioned / "runtimes/python/bin"
    return {
        "CANDLE_METAL_ENABLE_FAST_MATH": "1",
        "FASTEMBED_CACHE_DIR": str(BUNDLE / "components/models/reranker"),
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
        "HF_HOME": str(BUNDLE / "components/models/huggingface"),
        "HF_HUB_OFFLINE": "1",
        "HOME": str(directories["home"]),
        "JAVA_HOME": str(java_home),
        "LANG": "C",
        "LC_ALL": "C",
        "NODE_DISABLE_COMPILE_CACHE": "1",
        "NO_PROXY": "*",
        "PATH": ":".join(
            [
                str(provisioned / "bin"),
                str(node_bin),
                str(python_bin),
                str(java_home / "bin"),
            ]
        ),
        "PIP_CACHE_DIR": str(directories["pip-cache"]),
        "PIP_CONFIG_FILE": "/dev/null",
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PIP_NO_INDEX": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONNOUSERSITE": "1",
        "PYTHONSAFEPATH": "1",
        "PYTHONUTF8": "1",
        "RNA_EMBEDDING_MODEL_FILES_DIGEST": (
            "0ca325220d8166233c657cbf58db3b249d83d8e25b19f6a198e3a61764904d8a"
        ),
        "RNA_EMBEDDING_MODEL_SHA256": (
            "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db"
        ),
        "RNA_EMBEDDING_TOKENIZER_SHA256": (
            "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037"
        ),
        "RNA_LSP_JOB_TIMEOUT_MS": "3600000",
        "RNA_RERANKER_MODEL_FILES_DIGEST": (
            "a99cd3d59640b7eee8d4b38a28603130ae0643a605dccb07e58f85cd1c0d2313"
        ),
        "TMPDIR": str(directories["tmp"]),
        "TRANSFORMERS_OFFLINE": "1",
        "TZ": "UTC",
        "XDG_CACHE_HOME": str(directories["xdg-cache"]),
        "XDG_CONFIG_HOME": str(directories["xdg-config"]),
        "XDG_DATA_HOME": str(directories["xdg-data"]),
        "XDG_STATE_HOME": str(directories["xdg-state"]),
        "no_proxy": "*",
        "npm_config_cache": str(directories["npm-cache"]),
        "npm_config_offline": "true",
        "npm_config_userconfig": "/dev/null",
    }


def cache_inventory(cache: Path) -> dict[str, Any]:
    require(cache.is_dir() and not cache.is_symlink(), "RNA cache absent")
    digest = hashlib.sha256()
    files = 0
    total_bytes = 0
    for path in sorted(cache.rglob("*"), key=lambda item: item.relative_to(cache).as_posix()):
        relative = path.relative_to(cache).as_posix()
        require(not path.is_symlink(), f"cache symlink: {relative}")
        if path.is_dir():
            continue
        require(path.is_file(), f"cache special member: {relative}")
        reference = file_ref(path)
        member = {
            "path": relative,
            "bytes": reference["bytes"],
            "sha256": reference["sha256"],
        }
        digest.update(canonical(member))
        files += 1
        total_bytes += reference["bytes"]
    require(files > 0, "RNA cache is empty")
    return {
        "schema_version": "issue836-cache-reserve-inventory-v1",
        "files": files,
        "bytes": total_bytes,
        "sha256": digest.hexdigest(),
    }


def prepare(source_rank: int, attempt: int, output_root: Path) -> None:
    case = reserve_case(source_rank)
    require(attempt > 0, "attempt must be positive")
    require(output_root.is_absolute(), "output root must be absolute")
    require(
        output_root.name == expected_output_name(case, attempt),
        "output root name does not bind rank, case, and attempt",
    )
    require(
        output_root.parent.is_dir()
        and not output_root.parent.is_symlink()
        and not output_root.exists()
        and not output_root.is_symlink(),
        "output root must be absent under an existing real parent",
    )
    require(
        sha_file(RNA_BINARY) == RNA_BINARY_SHA256,
        "qualified RNA binary identity drift",
    )
    require(
        RUNTIME_SEED_ROOT.is_dir()
        and not RUNTIME_SEED_ROOT.is_symlink()
        and RUNTIME_SEED_RECEIPT.is_file()
        and not RUNTIME_SEED_RECEIPT.is_symlink(),
        "qualified runtime seed missing",
    )
    bare_repo = GIT_CACHE_ROOT / repo_slug_to_cache_name(case["repo"])
    require(
        bare_repo.is_dir()
        and not bare_repo.is_symlink()
        and (bare_repo / "HEAD").is_file(),
        "reserve bare repository missing",
    )
    observed_commit = run_checked(
        [str(GIT), f"--git-dir={bare_repo}", "rev-parse", case["base_commit"]]
    ).decode().strip()
    require(observed_commit == case["base_commit"], "base commit absent from reserve Git cache")
    base_tree = run_checked(
        [
            str(GIT),
            f"--git-dir={bare_repo}",
            "rev-parse",
            f"{case['base_commit']}^{{tree}}",
        ]
    ).decode().strip()

    output_root.mkdir()
    evidence = output_root / "evidence"
    checkout = output_root / "checkout" / case["instance_id"]
    provisioned = output_root / "runtime/lsp/provisioned"
    evidence.mkdir()
    checkout.parent.mkdir()
    provisioned.parent.mkdir(parents=True)
    try:
        run_checked(
            [
                str(GIT),
                "clone",
                "--no-hardlinks",
                "--no-checkout",
                str(bare_repo),
                str(checkout),
            ]
        )
        run_checked(
            [
                str(GIT),
                "-C",
                str(checkout),
                "fetch",
                "--depth=1",
                "--no-tags",
                str(bare_repo),
                case["base_commit"],
            ]
        )
        run_checked(
            [
                str(GIT),
                "-C",
                str(checkout),
                "remote",
                "set-url",
                "origin",
                f"https://github.com/{case['repo']}.git",
            ]
        )
        run_checked(
            [
                str(GIT),
                "-C",
                str(checkout),
                "checkout",
                "--detach",
                case["base_commit"],
            ]
        )
        observed_tree = run_checked(
            [str(GIT), "-C", str(checkout), "rev-parse", "HEAD^{tree}"]
        ).decode().strip()
        require(observed_tree == base_tree, "checkout base tree drift")
        require(
            run_checked(
                [
                    str(GIT),
                    "-C",
                    str(checkout),
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=no",
                ]
            )
            == b"",
            "checkout is not pristine before scan",
        )

        copied = subprocess.run(
            ["/bin/cp", "-cR", str(RUNTIME_SEED_ROOT), str(provisioned)],
            env={"PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        require(
            copied.returncode == 0,
            "APFS runtime clone failed: "
            + copied.stderr.decode(errors="replace")[-1000:],
        )
        environment = scan_environment(output_root, provisioned)
        scan_log = evidence / "scan.log"
        scan_command = [
            str(TIME),
            "-l",
            str(RNA_BINARY),
            "--business-context",
            "disabled",
            "scan",
            "--repo",
            str(checkout),
            "--full",
            "--timings",
        ]
        start = {
            "schema_version": "issue836-cache-reserve-start-v1",
            "started_at": utc_now(),
            "case": {
                **case,
                "base_tree": base_tree,
                "checkout": str(checkout),
            },
            "attempt": attempt,
            "reserve_plan": file_ref(PLAN),
            "qualified_rna_binary": file_ref(RNA_BINARY),
            "runtime_seed_receipt": file_ref(RUNTIME_SEED_RECEIPT),
            "runtime_seed_root": str(RUNTIME_SEED_ROOT.resolve()),
            "scan_command": scan_command,
            "environment": environment,
            "credentials_accessed": False,
            "model_provider_evaluator_calls": 0,
        }
        atomic_write(evidence / "scan-start.json", start)
        started_ns = time.monotonic_ns()
        with scan_log.open("xb") as log_handle:
            completed = subprocess.run(
                scan_command,
                cwd=checkout,
                env=environment,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
                check=False,
            )
        duration_ms = (time.monotonic_ns() - started_ns) // 1_000_000
        if completed.returncode != 0:
            atomic_write(
                evidence / "scan-failure.json",
                {
                    "schema_version": "issue836-cache-reserve-failure-v1",
                    "ended_at": utc_now(),
                    "returncode": completed.returncode,
                    "duration_ms": duration_ms,
                    "scan_start": file_ref(evidence / "scan-start.json"),
                    "scan_log": file_ref(scan_log),
                    "credentials_accessed": False,
                    "model_provider_evaluator_calls": 0,
                },
            )
            raise ReserveFailure(f"RNA scan failed with exit {completed.returncode}")
        require(
            run_checked(
                [
                    str(GIT),
                    "-C",
                    str(checkout),
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=no",
                ]
            )
            == b"",
            "checkout tracked state changed during scan",
        )
        inventory = cache_inventory(checkout / ".oh/.cache")
        atomic_write(
            evidence / "scan-receipt.json",
            {
                "schema_version": "issue836-cache-reserve-scan-receipt-v1",
                "status": "completed_pending_readiness_and_archive",
                "ended_at": utc_now(),
                "duration_ms": duration_ms,
                "case": {
                    **case,
                    "base_tree": base_tree,
                    "checkout": str(checkout),
                },
                "attempt": attempt,
                "scan_start": file_ref(evidence / "scan-start.json"),
                "scan_log": file_ref(scan_log),
                "cache_inventory": inventory,
                "credentials_accessed": False,
                "model_provider_evaluator_calls": 0,
            },
        )
        print(canonical(load_json(evidence / "scan-receipt.json", "scan receipt")).decode(), end="")
    except Exception as error:
        if not (evidence / "orchestration-failure.json").exists():
            atomic_write(
                evidence / "orchestration-failure.json",
                {
                    "schema_version": "issue836-cache-reserve-orchestration-failure-v1",
                    "ended_at": utc_now(),
                    "error_type": type(error).__name__,
                    "error": str(error),
                    "credentials_accessed": False,
                    "model_provider_evaluator_calls": 0,
                },
            )
        raise


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    result.add_argument("--source-rank", type=int, required=True)
    result.add_argument("--attempt", type=int, default=1)
    result.add_argument("--output-root", type=Path, required=True)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    prepare(arguments.source_rank, arguments.attempt, arguments.output_root)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ReserveFailure as error:
        print(f"FAIL CLOSED: {error}", file=sys.stderr)
        raise SystemExit(2) from error
