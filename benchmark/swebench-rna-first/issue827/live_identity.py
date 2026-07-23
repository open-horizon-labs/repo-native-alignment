#!/usr/bin/env python3
"""Shared fail-closed runtime identity verification for every RNA operation.

The selector verifies immutable registration material while preparing an
episode.  That is not enough for a long-running model invocation: the index
checkout, operational cache, or executable inputs could drift after preflight.
This module revalidates the complete live identity immediately before and
after each RNA entry point and makes any failure irreversible for the episode.
"""

from __future__ import annotations

from collections.abc import Mapping
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from typing import Any


READY = "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"
CONFIG_SCHEMA = "issue827-supervisor-config-v4"
DEFAULT_IDENTITY_SCHEMA = "issue827-runtime-identity-v1"
HEX_64 = re.compile(r"[0-9a-f]{64}")
GITHUB_REPOSITORY_PATTERN = re.compile(
    r"(?P<owner>[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)/"
    r"(?P<repository>[A-Za-z0-9._-]+)"
)
CODE_KINDS = {
    "class", "const", "enum", "function", "interface", "method", "module",
    "struct", "trait", "type", "type_alias", "union",
}


class LiveIdentityError(ValueError):
    """A live RNA identity check failed and the episode is now fatal."""

    def __init__(self, entry_point: str, reason: str):
        super().__init__(f"{entry_point}:{reason}")
        self.entry_point = entry_point
        self.reason = reason


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def derive_projection_authorization(projection: bytes) -> dict[str, object]:
    """Derive stable-ID authority solely from exact final projection bytes."""
    text = projection.decode("utf-8", errors="strict")
    stable_ids: set[str] = set()
    for candidate in re.findall(r"`([^`\r\n]+)`", text):
        parts = candidate.rsplit(":", 1)
        if (
            len(parts) == 2
            and parts[1] in CODE_KINDS
            and ":" in parts[0]
            and not any(ch.isspace() for ch in candidate)
        ):
            stable_ids.add(candidate)
    return {
        "schema_version": "issue827-projection-authorization-v1",
        "projection_sha256": sha(projection),
        "stable_code_ids": sorted(stable_ids),
    }


def sha_file(path: Path) -> tuple[str, int]:
    """Hash a nonsymlink regular file and reject concurrent replacement."""
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"not_regular_file:{path}")
    before = path.stat()
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    after = path.stat()
    if (
        before.st_dev,
        before.st_ino,
        before.st_size,
        before.st_mtime_ns,
    ) != (
        after.st_dev,
        after.st_ino,
        after.st_size,
        after.st_mtime_ns,
    ):
        raise ValueError(f"changed_while_hashing:{path}")
    return digest.hexdigest(), after.st_size


def read_file_stable(path: Path) -> tuple[bytes, str, int]:
    """Read and hash one file descriptor while rejecting path replacement."""
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"not_regular_file:{path}")
    before = path.stat()
    with path.open("rb") as handle:
        data = handle.read()
        descriptor = os.fstat(handle.fileno())
    after = path.stat()
    if (
        before.st_dev,
        before.st_ino,
        before.st_size,
        before.st_mtime_ns,
    ) != (
        descriptor.st_dev,
        descriptor.st_ino,
        descriptor.st_size,
        descriptor.st_mtime_ns,
    ) or (
        descriptor.st_dev,
        descriptor.st_ino,
        descriptor.st_size,
        descriptor.st_mtime_ns,
    ) != (
        after.st_dev,
        after.st_ino,
        after.st_size,
        after.st_mtime_ns,
    ):
        raise ValueError(f"changed_while_reading:{path}")
    return data, sha(data), after.st_size


def cache_inventory_sha256(cache: Path) -> str:
    """Hash every live operational-cache member by path, size, and bytes."""
    if not cache.is_dir() or cache.is_symlink():
        raise ValueError("operational_cache")
    members: list[dict[str, Any]] = []
    for path in sorted(cache.rglob("*"), key=lambda item: item.relative_to(cache).as_posix()):
        relative = path.relative_to(cache).as_posix()
        if path.is_symlink():
            raise ValueError(f"operational_cache_symlink:{relative}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError(f"operational_cache_non_file:{relative}")
        digest, size = sha_file(path)
        members.append({"path": relative, "bytes": size, "sha256": digest})
    if not members:
        raise ValueError("operational_cache_empty")
    return sha(
        canonical(
            {
                "schema_version": "issue827-operational-cache-inventory-v1",
                "members": members,
            }
        )
    )


def _verified_git(config: Mapping[str, Any]) -> tuple[Path, str, int]:
    """Revalidate the configured nonsymlink Git executable before each use."""

    path = _absolute_path(config.get("git_binary"), "git_binary")
    expected_sha = _require_hex(
        config.get("git_binary_sha256"), "git_binary_sha256"
    )
    try:
        observed_sha, observed_size = sha_file(path)
    except (OSError, ValueError) as exc:
        raise ValueError("git_binary_identity") from exc
    if observed_sha != expected_sha:
        raise ValueError("git_binary_digest")
    return path, observed_sha, observed_size


def git_bytes(
    repo: Path,
    config: Mapping[str, Any],
    *args: str,
) -> bytes:
    git_binary, _, _ = _verified_git(config)
    result = subprocess.run(
        [str(git_binary), "-C", str(repo), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise ValueError(f"git_{'_'.join(args)}")
    return result.stdout


def git(repo: Path, config: Mapping[str, Any], *args: str) -> str:
    return git_bytes(repo, config, *args).decode(
        "utf-8", errors="strict"
    ).strip()


def checkout_untracked_material(
    repo: Path,
    config: Mapping[str, Any],
    *,
    ignored: bool,
) -> tuple[bytes, ...]:
    args = ["ls-files", "--others"]
    if ignored:
        args.append("--ignored")
    args.extend(["--exclude-standard", "-z"])
    output = git_bytes(repo, config, *args)
    if output and not output.endswith(b"\0"):
        raise ValueError("live_untracked_inventory_not_nul_terminated")
    return tuple(item for item in output.split(b"\0") if item)


def is_cache_material(path: bytes) -> bool:
    return path.startswith(b".oh/.cache/")


def canonical_repository_slug(value: object) -> str:
    if not isinstance(value, str):
        raise ValueError("repository_identity")
    match = GITHUB_REPOSITORY_PATTERN.fullmatch(value)
    if match is None:
        raise ValueError("repository_identity")
    return f"{match.group('owner').lower()}/{match.group('repository').lower()}"


def canonical_github_origin(value: str) -> str:
    candidate = value.removesuffix("/").removesuffix(".git")
    for prefix in (
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ):
        if candidate.startswith(prefix):
            return canonical_repository_slug(candidate.removeprefix(prefix))
    raise ValueError("live_repository_origin")


def _require_hex(value: object, label: str) -> str:
    if not isinstance(value, str) or HEX_64.fullmatch(value) is None:
        raise ValueError(label)
    return value


def _absolute_path(value: object, label: str) -> Path:
    if not isinstance(value, str) or not value or not Path(value).is_absolute():
        raise ValueError(label)
    return Path(value)


def _load_json_bytes(path: Path, label: str) -> tuple[dict[str, Any], bytes, str, int]:
    try:
        data, digest, size = read_file_stable(path)
        value = json.loads(data)
    except (OSError, json.JSONDecodeError, UnicodeError) as exc:
        raise ValueError(f"{label}_json") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label}_object")
    return value, data, digest, size


def _verify_ref(value: object, label: str) -> tuple[Path, str, int, bytes]:
    if not isinstance(value, dict) or set(value) != {"path", "sha256", "bytes"}:
        raise ValueError(f"identity_{label}")
    path_value = value.get("path")
    size_value = value.get("bytes")
    expected_sha = _require_hex(value.get("sha256"), f"identity_{label}_sha256")
    path = _absolute_path(path_value, f"identity_{label}_path")
    if type(size_value) is not int or size_value < 0:
        raise ValueError(f"identity_{label}_bytes")
    data, observed_sha, observed_size = read_file_stable(path)
    if observed_size != size_value or observed_sha != expected_sha:
        raise ValueError(f"identity_{label}_tampered")
    return path, observed_sha, observed_size, data


class LiveIdentityVerifier:
    """Revalidate live RNA inputs and persist a monotonic fatal state."""

    def __init__(
        self,
        config: Mapping[str, Any],
        fatal_state_path: str | Path | None = None,
    ):
        self.config = dict(config)
        configured_state = self.config.get("state")
        chosen = fatal_state_path if fatal_state_path is not None else configured_state
        if not isinstance(chosen, (str, Path)) or not str(chosen):
            raise ValueError("fatal_state_path")
        self.fatal_state_path = Path(chosen)
        if not self.fatal_state_path.is_absolute():
            raise ValueError("fatal_state_path")

    def _read_state(self) -> dict[str, Any] | None:
        if not self.fatal_state_path.exists():
            return None
        if not self.fatal_state_path.is_file() or self.fatal_state_path.is_symlink():
            return {"fatal": True, "fatal_reason": "fatal_state_not_regular_file"}
        try:
            value = json.loads(self.fatal_state_path.read_bytes())
        except (OSError, json.JSONDecodeError):
            return {"fatal": True, "fatal_reason": "fatal_state_invalid"}
        if not isinstance(value, dict):
            return {"fatal": True, "fatal_reason": "fatal_state_invalid"}
        return value

    def _mark_fatal(self, entry_point: str, reason: str) -> None:
        previous = self._read_state()
        if previous is not None and previous.get("fatal") is True:
            return
        state = dict(previous) if previous is not None else {}
        state.update(
            {
                "schema_version": "issue827-rna-supervisor-state-v1",
                "fatal": True,
                "fatal_reason": reason,
                "fatal_entry_point": entry_point,
            }
        )
        self.fatal_state_path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.fatal_state_path.with_name(
            f".{self.fatal_state_path.name}.{os.getpid()}.tmp"
        )
        try:
            with temporary.open("xb") as handle:
                handle.write(canonical(state))
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self.fatal_state_path)
        finally:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass

    def _verify(self, entry_point: str) -> dict[str, Any]:
        if not isinstance(entry_point, str) or not entry_point:
            raise ValueError("entry_point")
        if self.config.get("schema_version") != CONFIG_SCHEMA:
            raise ValueError("supervisor_config_schema")

        identity_path_value = self.config.get("identity_receipt")
        identity_path = _absolute_path(identity_path_value, "identity_receipt")
        identity, _, identity_sha, identity_size = _load_json_bytes(
            identity_path, "identity_receipt"
        )
        expected_identity_sha = _require_hex(
            self.config.get("expected_identity_sha256"), "expected_identity_sha256"
        )
        if identity_sha != expected_identity_sha:
            raise ValueError("identity_receipt")

        expected_identity_schema = self.config.get(
            "expected_identity_schema", DEFAULT_IDENTITY_SCHEMA
        )
        expected = {
            "schema_version": expected_identity_schema,
            "root": self.config.get("root"),
            "expected_repository_identity": self.config.get(
                "expected_repository_identity"
            ),
            "live_repository_identity": self.config.get(
                "expected_repository_identity"
            ),
            "base_commit": self.config.get("expected_base_commit"),
            "base_tree": self.config.get("expected_base_tree"),
            "producer_commit": self.config.get("expected_producer_commit"),
            "cache_manifest_sha256": self.config.get(
                "expected_cache_manifest_sha256"
            ),
            "cache_archive_sha256": self.config.get(
                "expected_cache_archive_sha256"
            ),
            "operational_cache_inventory_sha256": self.config.get(
                "expected_cache_inventory_sha256"
            ),
            "launcher_sha256": self.config.get("expected_launcher_sha256"),
            "binary_sha256": self.config.get("expected_binary_sha256"),
            "canonical_environment_sha256": self.config.get(
                "expected_canonical_environment_sha256"
            ),
            "cache_bindings_verified": True,
            "fresh_reopen_ready": True,
            "readiness_sentinel": READY,
        }
        for key, value in expected.items():
            if identity.get(key) != value:
                raise ValueError(f"identity_{key}")

        repo_value = self.config.get("repo")
        launcher_value = self.config.get("launcher")
        binary_value = self.config.get("binary")
        environment_value = self.config.get("trusted_rna_environment")
        repo = _absolute_path(repo_value, "live_repo_path")
        launcher = _absolute_path(launcher_value, "live_launcher_path")
        binary = _absolute_path(binary_value, "live_binary_path")
        environment = _absolute_path(
            environment_value, "live_canonical_environment_path"
        )
        if not repo.is_dir() or repo.is_symlink():
            raise ValueError("live_repo")
        identity_repo = _absolute_path(
            identity.get("index_checkout"), "identity_index_checkout_path"
        )
        identity_launcher = _absolute_path(
            identity.get("launcher_path"), "identity_launcher_path"
        )
        identity_binary = _absolute_path(
            identity.get("binary_path"), "identity_binary_path"
        )
        identity_environment, identity_environment_sha, _, _ = _verify_ref(
            identity.get("canonical_environment"), "canonical_environment"
        )
        if identity_repo.resolve(strict=True) != repo.resolve(
            strict=True
        ):
            raise ValueError("identity_index_checkout")
        if identity_launcher.resolve(strict=True) != launcher.resolve(strict=True):
            raise ValueError("identity_launcher_path")
        if identity_binary.resolve(strict=True) != binary.resolve(strict=True):
            raise ValueError("identity_binary_path")
        if identity_environment.resolve(strict=True) != environment.resolve(strict=True):
            raise ValueError("identity_canonical_environment_path")
        if identity_environment_sha != self.config.get(
            "expected_canonical_environment_sha256"
        ):
            raise ValueError("canonical_environment_tampered")

        observed_head = git(repo, self.config, "rev-parse", "HEAD")
        if observed_head != self.config.get("expected_base_commit"):
            raise ValueError("live_HEAD")
        observed_tree = git(repo, self.config, "rev-parse", "HEAD^{tree}")
        if observed_tree != self.config.get("expected_base_tree"):
            raise ValueError("live_tree")
        if git(
            repo,
            self.config,
            "status",
            "--porcelain=v1",
            "--untracked-files=no",
        ):
            raise ValueError("live_checkout_not_pristine")
        untracked = checkout_untracked_material(
            repo, self.config, ignored=False
        )
        ignored = checkout_untracked_material(
            repo, self.config, ignored=True
        )
        if any(
            not is_cache_material(path)
            for path in (*untracked, *ignored)
        ):
            raise ValueError("live_checkout_material_outside_cache")
        origins = git(
            repo,
            self.config,
            "remote",
            "get-url",
            "--all",
            "origin",
        ).splitlines()
        if len(origins) != 1:
            raise ValueError("live_repository_origin_count")
        live_repository = canonical_github_origin(origins[0])
        expected_repository = canonical_repository_slug(
            self.config.get("expected_repository_identity")
        )
        if live_repository != expected_repository:
            raise ValueError("live_repository_identity")

        launcher_sha, launcher_bytes = sha_file(launcher)
        binary_sha, binary_bytes = sha_file(binary)
        git_binary, git_binary_sha, git_binary_bytes = _verified_git(
            self.config
        )
        if launcher_sha != self.config.get("expected_launcher_sha256"):
            raise ValueError("launcher_tampered")
        if binary_sha != self.config.get("expected_binary_sha256"):
            raise ValueError("binary_tampered")

        archive_path, archive_sha, archive_bytes, _ = _verify_ref(
            identity.get("cache_archive"), "cache_archive"
        )
        del archive_path
        if archive_sha != self.config.get("expected_cache_archive_sha256"):
            raise ValueError("cache_archive_tampered")
        manifest_path, manifest_sha, manifest_bytes, manifest_data = _verify_ref(
            identity.get("cache_manifest"), "cache_manifest"
        )
        del manifest_path
        if manifest_sha != self.config.get("expected_cache_manifest_sha256"):
            raise ValueError("cache_manifest_tampered")
        verification_path, verification_sha, verification_bytes, verification_data = (
            _verify_ref(
                identity.get("cache_verification_receipt"),
                "cache_verification_receipt",
            )
        )
        del verification_path
        readiness_path, readiness_sha, readiness_bytes, readiness_data = _verify_ref(
            identity.get("readiness_report"), "readiness_report"
        )
        del readiness_path

        try:
            manifest = json.loads(manifest_data)
            verification = json.loads(verification_data)
            readiness = json.loads(readiness_data)
        except (json.JSONDecodeError, UnicodeError) as exc:
            raise ValueError("cache_identity_json") from exc
        if not all(isinstance(item, dict) for item in (manifest, verification, readiness)):
            raise ValueError("cache_identity_object")
        if readiness.get("status") != "READY":
            raise ValueError("readiness_report_status")
        readiness_body = readiness.get("readiness")
        if (
            not isinstance(readiness_body, dict)
            or readiness_body.get("ready") is not True
            or readiness_body.get("compatibility_violations") != []
        ):
            raise ValueError("readiness_report_not_ready")
        readiness_digest = _require_hex(
            readiness_body.get("report_digest"), "readiness_report_digest"
        )

        observed_cache_inventory = cache_inventory_sha256(repo / ".oh/.cache")
        if observed_cache_inventory != self.config.get(
            "expected_cache_inventory_sha256"
        ):
            raise ValueError("live_cache_inventory")
        if (
            identity.get("operational_cache_inventory_sha256")
            != observed_cache_inventory
        ):
            raise ValueError("identity_cache_inventory")

        receipt = {
            "schema_version": "issue827-live-rna-identity-v1",
            "entry_point": entry_point,
            "identity_sha256": identity_sha,
            "identity_bytes": identity_size,
            "root": identity["root"],
            "repository_identity": live_repository,
            "base_commit": observed_head,
            "base_tree": observed_tree,
            "producer_commit": identity["producer_commit"],
            "operational_cache_inventory_sha256": observed_cache_inventory,
            "cache_archive_sha256": archive_sha,
            "cache_archive_bytes": archive_bytes,
            "cache_manifest_sha256": manifest_sha,
            "cache_manifest_bytes": manifest_bytes,
            "cache_verification_receipt_sha256": verification_sha,
            "cache_verification_receipt_bytes": verification_bytes,
            "readiness_report_sha256": readiness_sha,
            "readiness_report_bytes": readiness_bytes,
            "readiness_report_digest": readiness_digest,
            "launcher_sha256": launcher_sha,
            "launcher_bytes": launcher_bytes,
            "binary_sha256": binary_sha,
            "binary_bytes": binary_bytes,
            "git_binary_path": str(git_binary),
            "git_binary_sha256": git_binary_sha,
            "git_binary_bytes": git_binary_bytes,
            "git_config_global_write_target": "/dev/null",
            "canonical_environment_sha256": identity_environment_sha,
        }
        receipt["live_state_sha256"] = sha(
            canonical({key: value for key, value in receipt.items() if key != "entry_point"})
        )
        return receipt

    def verify(self, entry_point: str) -> dict[str, Any]:
        """Return a complete serializable receipt or irreversibly fail."""
        previous = self._read_state()
        if previous is not None and previous.get("fatal") is True:
            reason = str(previous.get("fatal_reason", "episode_already_fatal"))
            raise LiveIdentityError(entry_point, f"episode_already_fatal:{reason}")
        try:
            return self._verify(entry_point)
        except LiveIdentityError:
            raise
        except Exception as exc:
            reason = str(exc) or type(exc).__name__
            try:
                self._mark_fatal(entry_point, reason)
            except Exception as state_exc:
                reason = f"{reason};fatal_state_write:{state_exc}"
            raise LiveIdentityError(entry_point, reason) from exc


def verify_live_identity(
    config: Mapping[str, Any],
    *,
    entry_point: str,
    fatal_state_path: str | Path | None = None,
) -> dict[str, Any]:
    """One-shot convenience wrapper around :class:`LiveIdentityVerifier`."""
    return LiveIdentityVerifier(config, fatal_state_path).verify(entry_point)
