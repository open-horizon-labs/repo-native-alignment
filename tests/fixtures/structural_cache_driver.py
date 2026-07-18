#!/usr/bin/env python3
"""Test-only driver for the verifier-owned structural-cache archive seam."""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "swebench_lsp_toolchain.py"
SPEC = importlib.util.spec_from_file_location("swebench_lsp_toolchain", MODULE_PATH)
assert SPEC and SPEC.loader
TOOLCHAIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TOOLCHAIN
SPEC.loader.exec_module(TOOLCHAIN)

TOOLCHAIN_DIGEST = "a" * 64
INVENTORY_DIGEST = "b" * 64
INVENTORY_FILE_SHA256 = "c" * 64
CASE_INVENTORY_DIGEST = "d" * 64


def archive(args: argparse.Namespace) -> dict[str, object]:
    identity = TOOLCHAIN.structural_cache_identity(args.rna, args.checkout)
    authorization_path = (
        args.checkout / ".oh" / ".cache" / "structural-cache-inheritance.json"
    )
    base_cache = None
    if authorization_path.is_file():
        authorization = TOOLCHAIN.load_json_object(
            authorization_path, "fixture cache authorization"
        )
        base_cache = {
            "archive_sha256": authorization["base_archive_sha256"],
            "sidecar_sha256": authorization["base_sidecar_sha256"],
            "core_sha256": authorization["base_core_sha256"],
            "report_digest": authorization["base_report_digest"],
        }
    return TOOLCHAIN.archive_structural_cache(
        args.checkout,
        args.archive,
        args.sidecar,
        identity=identity,
        toolchain_lock_digest=TOOLCHAIN_DIGEST,
        inventory_digest=INVENTORY_DIGEST,
        inventory_file_sha256=INVENTORY_FILE_SHA256,
        case_inventory_digest=CASE_INVENTORY_DIGEST,
        base_cache=base_cache,
    )


def inject(args: argparse.Namespace) -> dict[str, object]:
    identity = TOOLCHAIN.structural_cache_identity(args.rna, args.checkout)
    with tempfile.TemporaryDirectory(prefix="rna-cache-driver-") as temporary:
        materialized = Path(temporary) / "cache"
        verified = TOOLCHAIN.verify_structural_cache_archive(
            args.archive,
            args.sidecar,
            expected={
                "repository": identity["repository"],
                "root_slug": identity["root_slug"],
                "producer": identity["producer"],
                "toolchain_lock_digest": TOOLCHAIN_DIGEST,
                "inventory_digest": INVENTORY_DIGEST,
                "inventory_file_sha256": INVENTORY_FILE_SHA256,
                "inventory_policy_digest": identity["inventory_policy_digest"],
                "scan_flags": TOOLCHAIN.QUALIFICATION_SCAN_FLAGS,
            },
            materialize_cache=materialized,
        )
        core = verified["core"]
        if TOOLCHAIN._git_commit_tree(args.git_dir, core["commit"]) != core["tree"]:
            raise TOOLCHAIN.ToolchainError("fixture base commit/tree mismatch")
        diff = TOOLCHAIN._git_diff_paths(
            args.git_dir, core["commit"], identity["commit"]
        )
        invalidate_all = core["shared_influence_digest"] != identity[
            "shared_influence_digest"
        ]
        invalidated = sorted(
            language
            for language, signature in core["partition_signatures"].items()
            if invalidate_all
            or language not in identity["partitions"]
            or identity["partitions"][language]["signature"] != signature
        )
        selection = {
            "entry": {
                "archive_path": str(args.archive),
                "sidecar_path": str(args.sidecar),
            },
            "verified": verified,
            "diff": diff,
            "invalidated_partitions": invalidated,
        }
        return TOOLCHAIN.inject_structural_cache(
            selection,
            args.checkout,
            identity,
            args.git_dir,
            toolchain_lock_digest=TOOLCHAIN_DIGEST,
            inventory_digest=INVENTORY_DIGEST,
            inventory_file_sha256=INVENTORY_FILE_SHA256,
            verified=verified,
            materialized_cache=materialized,
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("archive", "inject"))
    parser.add_argument("--rna", type=Path, required=True)
    parser.add_argument("--checkout", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--git-dir", type=Path)
    args = parser.parse_args()
    if args.action == "inject" and args.git_dir is None:
        parser.error("inject requires --git-dir")
    result = archive(args) if args.action == "archive" else inject(args)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
