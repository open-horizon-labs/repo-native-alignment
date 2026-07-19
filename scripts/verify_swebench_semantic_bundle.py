#!/usr/bin/env python3
"""Fail-closed verifier/extractor for the #786 CI semantic bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import tarfile
import tempfile
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

try:
    from scripts import swebench_combined_cache as COMBINED
except ModuleNotFoundError:  # Direct execution from scripts/.
    import swebench_combined_cache as COMBINED  # type: ignore[no-redef]


MANIFEST_SCHEMA = "rna-swebench-semantic-bundle-manifest-v1"
UPLOAD_SCHEMA = "rna-swebench-semantic-bundle-upload-v1"
PAYLOAD_SCHEMA = "rna-swebench-semantic-bundle-payload-v1"
RECEIPT_SCHEMA = "rna-swebench-semantic-bundle-verification-v1"
ARCHIVE_ROOT = "rna-semantic-bundle"
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
GIT_COMMIT = re.compile(r"[0-9a-f]{40}\Z")
GITHUB_ARTIFACT_DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
MAX_ARCHIVE_MEMBER_BYTES = 16 * 1024 * 1024 * 1024
MAX_ARCHIVE_TOTAL_BYTES = 50 * 1024 * 1024 * 1024


class BundleVerificationError(ValueError):
    """The bundle failed an identity, archive, or payload check."""


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    ).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise BundleVerificationError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_canonical_json(path: Path, label: str) -> dict[str, Any]:
    raw = path.read_bytes()
    try:
        value = json.loads(raw, object_pairs_hook=_unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BundleVerificationError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise BundleVerificationError(f"{label} must be a JSON object")
    if raw != canonical_json(value):
        raise BundleVerificationError(
            f"{label} must be sorted compact canonical JSON with one trailing newline"
        )
    return value


def require_keys(value: Mapping[str, Any], expected: Iterable[str], label: str) -> None:
    expected_set = set(expected)
    observed = set(value)
    if observed != expected_set:
        raise BundleVerificationError(
            f"{label} fields mismatch: missing={sorted(expected_set - observed)} "
            f"unexpected={sorted(observed - expected_set)}"
        )


def require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BundleVerificationError(f"{label} must be an object")
    return value


def require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise BundleVerificationError(f"{label} must be a non-empty string")
    return value


def require_sha256(value: Any, label: str) -> str:
    value = require_string(value, label)
    if SHA256.fullmatch(value) is None:
        raise BundleVerificationError(f"{label} must be a lowercase SHA-256")
    return value


def require_true(value: Any, label: str) -> None:
    if value is not True:
        raise BundleVerificationError(f"{label} must be true")


def validate_file_records(value: Any, label: str) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value:
        raise BundleVerificationError(f"{label} must be a non-empty array")
    records: list[dict[str, Any]] = []
    previous: bytes | None = None
    for index, item in enumerate(value):
        record = require_object(item, f"{label}[{index}]")
        require_keys(record, ("path", "size", "sha256"), f"{label}[{index}]")
        path = require_string(record["path"], f"{label}[{index}].path")
        try:
            encoded_path = path.encode("utf-8")
        except UnicodeEncodeError as error:
            raise BundleVerificationError(f"{label}[{index}].path is not UTF-8") from error
        candidate = Path(path)
        if (
            candidate.is_absolute()
            or "\\" in path
            or not candidate.parts
            or any(part in ("", ".", "..") for part in candidate.parts)
        ):
            raise BundleVerificationError(f"{label}[{index}].path is unsafe")
        if previous is not None and previous >= encoded_path:
            raise BundleVerificationError(f"{label} paths must be unique byte-sorted UTF-8")
        previous = encoded_path
        size = record["size"]
        if isinstance(size, bool) or not isinstance(size, int) or size < 0:
            raise BundleVerificationError(f"{label}[{index}].size is invalid")
        require_sha256(record["sha256"], f"{label}[{index}].sha256")
        records.append(dict(record))
    return records


def records_digest(records: list[dict[str, Any]]) -> str:
    return sha256_bytes(canonical_json(records))


def regular_file_records(root: Path) -> list[dict[str, Any]]:
    metadata = root.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise BundleVerificationError(f"tree root is not a regular directory: {root}")
    records: list[dict[str, Any]] = []

    def walk(directory: Path) -> None:
        try:
            children = sorted(
                os.scandir(directory), key=lambda entry: entry.name.encode("utf-8")
            )
        except UnicodeEncodeError as error:
            raise BundleVerificationError("tree contains a non-UTF-8 path") from error
        for child in children:
            path = Path(child.path)
            child_stat = path.lstat()
            relative = path.relative_to(root).as_posix()
            try:
                relative.encode("utf-8")
            except UnicodeEncodeError as error:
                raise BundleVerificationError("tree contains a non-UTF-8 path") from error
            if stat.S_ISLNK(child_stat.st_mode):
                raise BundleVerificationError(f"tree contains a symlink: {relative}")
            if stat.S_ISDIR(child_stat.st_mode):
                walk(path)
            elif stat.S_ISREG(child_stat.st_mode):
                records.append(
                    {
                        "path": relative,
                        "size": child_stat.st_size,
                        "sha256": sha256_file(path),
                    }
                )
            else:
                raise BundleVerificationError(f"tree contains a special file: {relative}")

    walk(root)
    records.sort(key=lambda record: record["path"].encode("utf-8"))
    return records


def validate_upload_attestation(
    upload: dict[str, Any], projection: dict[str, Any], manifest_sha256: str,
    expected_github_artifact_digest: str,
) -> None:
    require_keys(
        upload,
        (
            "schema", "artifact_name", "artifact_id", "artifact_url", "artifact_digest",
            "manifest_sha256", "head_sha", "run_id", "run_attempt",
        ),
        "upload attestation",
    )
    if upload["schema"] != UPLOAD_SCHEMA:
        raise BundleVerificationError("upload attestation schema mismatch")
    if upload["artifact_name"] != projection["artifact"]["name"]:
        raise BundleVerificationError("upload artifact name mismatch")
    if upload["manifest_sha256"] != manifest_sha256:
        raise BundleVerificationError("upload manifest digest mismatch")
    if upload["head_sha"] != projection["provenance"]["head_sha"]:
        raise BundleVerificationError("upload head mismatch")
    for name in ("run_id", "run_attempt"):
        if upload[name] != projection["provenance"][name]:
            raise BundleVerificationError(f"upload {name} mismatch")
    if isinstance(upload["artifact_id"], bool) or not isinstance(upload["artifact_id"], int) or upload["artifact_id"] < 1:
        raise BundleVerificationError("upload artifact_id is invalid")
    require_string(upload["artifact_url"], "upload artifact_url")
    digest = require_string(upload["artifact_digest"], "upload artifact_digest")
    if GITHUB_ARTIFACT_DIGEST.fullmatch(digest) is None or digest != expected_github_artifact_digest:
        raise BundleVerificationError("GitHub artifact digest trust anchor mismatch")


def safe_extract(archive: Path, destination: Path) -> Path:
    seen: set[str] = set()
    seen_casefold: set[str] = set()
    total_size = 0
    directory_modes: list[tuple[Path, int]] = []
    with tarfile.open(archive, mode="r:gz") as source:
        members = source.getmembers()
        if not members or len(members) > 200_000:
            raise BundleVerificationError("semantic bundle archive entry count is invalid")
        for member in members:
            name = member.name
            if "\\" in name or "\x00" in name:
                raise BundleVerificationError("archive contains an unsafe member name")
            path = Path(name)
            if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
                raise BundleVerificationError(f"archive member path is unsafe: {name}")
            if not path.parts or path.parts[0] != ARCHIVE_ROOT:
                raise BundleVerificationError(f"archive member escapes bundle root: {name}")
            normalized = path.as_posix()
            folded = normalized.casefold()
            if normalized in seen or folded in seen_casefold:
                raise BundleVerificationError(f"archive contains a duplicate member: {name}")
            seen.add(normalized)
            seen_casefold.add(folded)
            if not (member.isdir() or member.isfile()):
                raise BundleVerificationError(f"archive contains a link or special member: {name}")
            if getattr(member, "sparse", None):
                raise BundleVerificationError(f"archive contains a sparse member: {name}")
            model_member = path.parts[1:3] == ("components", "models")
            expected_mode = (
                0o555
                if member.isdir() and model_member
                else 0o444
                if member.isfile() and model_member
                else 0o755
                if member.isdir() or path.name == "repo-native-alignment"
                else 0o644
            )
            if member.mode & 0o777 != expected_mode:
                raise BundleVerificationError(f"archive member mode mismatch: {name}")
            if member.size < 0:
                raise BundleVerificationError(f"archive member has invalid size: {name}")
            if member.size > MAX_ARCHIVE_MEMBER_BYTES:
                raise BundleVerificationError(f"archive member is too large: {name}")
            total_size += member.size
            if total_size > MAX_ARCHIVE_TOTAL_BYTES:
                raise BundleVerificationError("semantic bundle archive is too large")

        for member in members:
            target = destination.joinpath(*Path(member.name).parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            if member.isdir():
                target.mkdir(mode=0o755, parents=True, exist_ok=True)
                directory_modes.append((target, member.mode & 0o777))
                continue
            extracted = source.extractfile(member)
            if extracted is None:
                raise BundleVerificationError(f"archive file is partial: {member.name}")
            written = 0
            with target.open("xb") as output:
                for chunk in iter(lambda: extracted.read(1024 * 1024), b""):
                    output.write(chunk)
                    written += len(chunk)
            if written != member.size:
                raise BundleVerificationError(f"archive file is truncated: {member.name}")
            target.chmod(member.mode & 0o777)
    # Make immutable directories read-only only after all descendants exist.
    for target, mode in sorted(
        directory_modes, key=lambda item: len(item[0].parts), reverse=True
    ):
        target.chmod(mode)
    return destination / ARCHIVE_ROOT


def verify_extracted_payload(bundle_root: Path, projection: dict[str, Any]) -> str:
    payload_path = bundle_root / "payload-file-manifest.json"
    payload = load_canonical_json(payload_path, "payload file manifest")
    require_keys(payload, ("schema", "files"), "payload file manifest")
    if payload["schema"] != PAYLOAD_SCHEMA:
        raise BundleVerificationError("payload file manifest schema mismatch")
    expected_payload = validate_file_records(payload["files"], "payload file manifest.files")
    observed_payload = [
        record
        for record in regular_file_records(bundle_root)
        if record["path"] != "payload-file-manifest.json"
    ]
    if observed_payload != expected_payload:
        raise BundleVerificationError("extracted payload file tree does not match its manifest")

    profiler = bundle_root / "evidence/apple-system-profiler.json"
    if sha256_file(profiler) != projection["host"]["system_profiler_sha256"]:
        raise BundleVerificationError("system-profiler evidence digest mismatch")
    readiness = bundle_root / "evidence/offline-lsp-readiness.json"
    if sha256_file(readiness) != projection["qualification"]["lsp_readiness_sha256"]:
        raise BundleVerificationError("offline LSP readiness evidence digest mismatch")
    return records_digest(regular_file_records(bundle_root))


def verify_bundle(
    *,
    archive: Path,
    manifest_path: Path,
    upload_attestation_path: Path,
    output: Path,
    expected_manifest_sha256: str,
    expected_upload_attestation_sha256: str,
    expected_github_artifact_digest: str,
    expected_head_sha: str,
) -> dict[str, Any]:
    for value, label in (
        (expected_manifest_sha256, "expected manifest SHA-256"),
        (expected_upload_attestation_sha256, "expected upload-attestation SHA-256"),
    ):
        require_sha256(value, label)
    if GIT_COMMIT.fullmatch(expected_head_sha) is None:
        raise BundleVerificationError("expected source head is not a full lowercase Git commit")
    if GITHUB_ARTIFACT_DIGEST.fullmatch(expected_github_artifact_digest) is None:
        raise BundleVerificationError("expected GitHub artifact digest is invalid")
    manifest_sha256 = sha256_file(manifest_path)
    if manifest_sha256 != expected_manifest_sha256:
        raise BundleVerificationError("semantic manifest external trust anchor mismatch")
    try:
        runtime = COMBINED._project_runtime_manifest(manifest_path)
    except COMBINED.ToolchainError as error:
        raise BundleVerificationError(str(error)) from error
    projection = runtime["projection"]
    if projection["provenance"]["head_sha"] != expected_head_sha:
        raise BundleVerificationError("semantic manifest source head trust anchor mismatch")
    if archive.name != projection["artifact"]["archive_file"]:
        raise BundleVerificationError("semantic archive filename mismatch")
    archive_sha256 = sha256_file(archive)
    if archive_sha256 != projection["artifact"]["archive_sha256"]:
        raise BundleVerificationError("semantic archive digest mismatch")

    upload_sha256 = sha256_file(upload_attestation_path)
    if upload_sha256 != expected_upload_attestation_sha256:
        raise BundleVerificationError("upload-attestation external trust anchor mismatch")
    upload = load_canonical_json(upload_attestation_path, "upload attestation")
    validate_upload_attestation(
        upload, projection, manifest_sha256, expected_github_artifact_digest
    )

    if output.exists():
        raise BundleVerificationError("verified output path must not already exist")
    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{output.name}.verify-", dir=output.parent))
    try:
        bundle_root = safe_extract(archive, staging)
        try:
            runtime = COMBINED.verify_runtime_bundle_directory(manifest_path, bundle_root)
        except COMBINED.ToolchainError as error:
            raise BundleVerificationError(str(error)) from error
        projection = runtime["projection"]
        tree_digest = verify_extracted_payload(bundle_root, projection)
        os.replace(staging, output)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise

    return {
        "schema": RECEIPT_SCHEMA,
        "head_sha": expected_head_sha,
        "manifest_sha256": manifest_sha256,
        "archive_sha256": archive_sha256,
        "upload_attestation_sha256": upload_sha256,
        "github_artifact_digest": expected_github_artifact_digest,
        "extracted_tree_digest": tree_digest,
        "artifact_name": projection["artifact"]["name"],
        "artifact_id": upload["artifact_id"],
        "run_id": upload["run_id"],
        "run_attempt": upload["run_attempt"],
    }


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--upload-attestation", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expected-manifest-sha256", required=True)
    parser.add_argument("--expected-upload-attestation-sha256", required=True)
    parser.add_argument("--expected-github-artifact-digest", required=True)
    parser.add_argument("--expected-head-sha", required=True)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        receipt = verify_bundle(
            archive=args.archive,
            manifest_path=args.manifest,
            upload_attestation_path=args.upload_attestation,
            output=args.output,
            expected_manifest_sha256=args.expected_manifest_sha256,
            expected_upload_attestation_sha256=args.expected_upload_attestation_sha256,
            expected_github_artifact_digest=args.expected_github_artifact_digest,
            expected_head_sha=args.expected_head_sha,
        )
    except (BundleVerificationError, OSError, tarfile.TarError) as error:
        print(f"semantic bundle verification failed: {error}", file=os.sys.stderr)
        return 1
    os.sys.stdout.buffer.write(canonical_json(receipt))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
