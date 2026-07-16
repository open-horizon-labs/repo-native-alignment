#!/usr/bin/env python3
"""Create the auditable semantic qualification artifact payload."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile


def sha256(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--model-lock", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--git-sha", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", required=True)
    args = parser.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    binary = args.output / "repo-native-alignment"
    model_lock = args.output / "model-lock.json"
    shutil.copy2(args.binary, binary)
    shutil.copy2(args.model_lock, model_lock)

    payload_hashes = {
        binary.name: sha256(binary),
        model_lock.name: sha256(model_lock),
    }
    qualification_digest = hashlib.sha256(
        json.dumps(payload_hashes, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    manifest = {
        "schema_version": 1,
        "git_sha": args.git_sha,
        "github_run_id": args.run_id,
        "github_run_attempt": args.run_attempt,
        "job": "semantic-artifact",
        "target": "aarch64-apple-darwin",
        "cpu": "apple-m4",
        "features": ["metal"],
        "acceleration": "metal-required",
        "rustflags": "-C target-cpu=apple-m4 -C link-arg=-Wl,-dead_strip",
        "candle_metal_force_release": "1",
        "embedding_model": "sentence-transformers/all-MiniLM-L6-v2",
        "rerank_model": "jinaai/jina-reranker-v1-turbo-en",
        "files": payload_hashes,
        "qualification_digest": qualification_digest,
    }
    manifest_path = args.output / "artifact-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")

    sums = {
        **payload_hashes,
        manifest_path.name: sha256(manifest_path),
    }
    (args.output / "SHA256SUMS").write_text(
        "".join(f"{digest}  {name}\n" for name, digest in sorted(sums.items()))
    )

    args.archive.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(args.archive, "w:gz") as archive:
        for path in sorted(args.output.iterdir()):
            archive.add(path, arcname=path.name)
    Path(f"{args.archive}.sha256").write_text(
        f"{sha256(args.archive)}  {args.archive.name}\n"
    )


if __name__ == "__main__":
    main()
