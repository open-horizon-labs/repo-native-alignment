#!/usr/bin/env python3
"""Fetch and verify the exact model revisions used by semantic qualification."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import urllib.request


def cache_root(model: dict, home: Path) -> Path:
    if model["cache"] == "huggingface":
        return home / ".cache" / "huggingface" / "hub"
    if model["cache"] == "fastembed":
        return Path(
            os.environ.get("FASTEMBED_CACHE_DIR", home / ".cache" / "rna" / "models")
        )
    raise ValueError(f"unknown cache kind: {model['cache']}")


def repository_dir(repository: str) -> str:
    return "models--" + repository.replace("/", "--")


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def verify_file(path: Path, expected: dict) -> None:
    if not path.is_file():
        raise RuntimeError(f"missing locked model file: {path}")
    actual_size = path.stat().st_size
    if actual_size != expected["size"]:
        raise RuntimeError(
            f"size mismatch for {path}: expected {expected['size']}, got {actual_size}"
        )
    actual_digest = digest(path)
    if actual_digest != expected["sha256"]:
        raise RuntimeError(
            f"sha256 mismatch for {path}: expected {expected['sha256']}, got {actual_digest}"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock", type=Path, default=Path("models/model-lock.json"))
    parser.add_argument("--home", type=Path, default=Path.home())
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--reject-extra", action="store_true")
    args = parser.parse_args()

    lock = json.loads(args.lock.read_text())
    if lock.get("schema_version") != 1:
        raise RuntimeError("unsupported model lock schema")

    allowed_files: set[Path] = set()
    cache_roots: set[Path] = set()
    for model in lock["models"]:
        revision = model["revision"]
        root = cache_root(model, args.home)
        cache_roots.add(root)
        repo_dir = root / repository_dir(model["repository"])
        snapshot = repo_dir / "snapshots" / revision
        for expected in model["files"]:
            destination = snapshot / expected["path"]
            allowed_files.add(destination.resolve())
            if not args.verify_only and not destination.exists():
                destination.parent.mkdir(parents=True, exist_ok=True)
                url = (
                    "https://huggingface.co/"
                    f"{model['repository']}/resolve/{revision}/{expected['path']}"
                )
                temporary = destination.with_suffix(destination.suffix + ".partial")
                urllib.request.urlretrieve(url, temporary)
                temporary.replace(destination)
            verify_file(destination, expected)
        refs = repo_dir / "refs"
        allowed_files.add((refs / "main").resolve())
        if not args.verify_only:
            refs.mkdir(parents=True, exist_ok=True)
            (refs / "main").write_text(revision)
        elif (refs / "main").read_text().strip() != revision:
            raise RuntimeError(f"cache ref main is not pinned to {revision}: {refs / 'main'}")

    if args.reject_extra:
        extras = sorted(
            path
            for root in cache_roots
            if root.exists()
            for path in root.rglob("*")
            if path.is_file() and path.resolve() not in allowed_files
        )
        if extras:
            rendered = "\n".join(str(path) for path in extras)
            raise RuntimeError(f"unattested model cache files found:\n{rendered}")

    print(f"verified {len(lock['models'])} immutable model revisions")


if __name__ == "__main__":
    main()
