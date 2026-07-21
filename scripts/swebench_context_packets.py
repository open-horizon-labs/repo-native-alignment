#!/usr/bin/env python3
"""Build and verify frozen SWE-bench RNA B/C context packets without spend.

The producer consumes one exact dataset row and one immutable, verifier-clean
combined cache.  It never scans, contacts an LSP, encodes corpus vectors,
repairs a cache, or contacts a model API.  Query-vector inference and the
already-sealed local reranker are the only semantic computation performed.
"""

from __future__ import annotations

import argparse
import ast
import dataclasses
import hashlib
import importlib.metadata
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path, PurePosixPath
from types import SimpleNamespace
from typing import Any, Iterable, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from scripts import swebench_combined_cache as COMBINED
from scripts import swebench_lsp_toolchain as LSP
from scripts import validate_swebench_act_context_protocol as PROTOCOL


DEFAULT_PROTOCOL = ROOT / "benchmark/swebench-act-context/protocol.json"
DEFAULT_POPULATION = ROOT / "benchmark/swebench-act-context/population.json"
DEFAULT_LOCK = ROOT / "benchmark/swebench-act-context/protocol.lock.json"
UPSTREAM_ARMS = ROOT / "scripts/vendor/act_context/arms_v2.py"
UPSTREAM_ARMS_SHA256 = "784aa472671dff1ad96551570816959925815a5ab7e90ac5a9c391b43b5577a3"
STRICT_SENTINEL = (
    "status=READY embeddings=true retrieval=hybrid rerank=true "
    "metal=true fallback=false"
)
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
HEX64 = re.compile(r"[0-9a-f]{64}")
ENTRY_HEADER = re.compile(
    r"^- \*\*(?P<kind>[^*]+)\*\* .*?`(?P<path>[^`\n]+)`:"
    r"(?P<start>[0-9]+)-(?P<end>[0-9]+)(?:\s|$)"
)
STABLE_ID_LINE = re.compile(r"^  (?:ID: )?`(?P<stable_id>[^`\n]+)`\s*$")
GROUP_HEADER = re.compile(r"^#### (?P<label>.+) \((?P<count>[0-9]+)\)\s*$")
RESULT_COUNT = re.compile(r"^### Code symbols \((?P<count>[0-9]+) result\(s\)\)\s*$")
FENCE_OPEN = re.compile(r"(?m)^  (?P<fence>`{3,})(?P<tag>[^\n]*)\n")

LANGUAGE_BY_TAG = {
    "python": "Python",
    "rust": "Rust",
    "typescript": "TypeScript",
    "tsx": "TSX",
    "javascript": "JavaScript",
    "jsx": "JSX",
    "go": "Go",
    "csharp": "C#",
    "c#": "C#",
    "markdown": "Markdown",
    "md": "Markdown",
    "rst": "reStructuredText",
    "restructuredtext": "reStructuredText",
    "toml": "TOML",
    "yaml": "YAML",
    "json": "JSON",
    "xml": "XML",
    "sql": "SQL",
    "bash": "Shell",
    "sh": "Shell",
}
MINIFIABLE = {"Python", "Rust", "TypeScript", "TSX", "JavaScript", "JSX", "Go", "C#"}
KNOWN_RAW_LABELS = {
    "calls",
    "referenced_by",
    "depends_on",
    "implements",
    "defines",
    "has_field",
    "belongs_to",
    "contains",
    "tested_by",
    "tests",
    "imports",
    "extends",
}
FORBIDDEN_COMMAND_TOKENS = {"scan", "--full", "--no-lsp", "lsp-preflight"}
FORBIDDEN_STDERR_MARKERS = (
    "falling back",
    "using cpu",
    "original order",
    "qualification failed",
)


class PacketError(RuntimeError):
    """Fail-closed packet construction error."""


@dataclasses.dataclass(frozen=True)
class ParsedNode:
    stable_id: str
    kind: str
    path: str
    start_line: int
    end_line: int
    inline_body: str | None = None


@dataclasses.dataclass(frozen=True)
class CommandResult:
    name: str
    argv: tuple[str, ...]
    exit_code: int
    stdout: bytes
    stderr: bytes
    elapsed_ms: int

    def receipt(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "argv": list(self.argv),
            "exit_code": self.exit_code,
            "stdout_sha256": sha256(self.stdout),
            "stderr_sha256": sha256(self.stderr),
            "stdout_size_bytes": len(self.stdout),
            "stderr_size_bytes": len(self.stderr),
            "elapsed_ms": self.elapsed_ms,
        }


def canonical_json(value: Any) -> bytes:
    return PROTOCOL.canonical_json(value)


def sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_bytes())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PacketError(f"invalid JSON: {path}") from exc


def write_exclusive(path: Path, data: bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    descriptor = os.open(path, flags, mode)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def git(checkout: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(checkout), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode:
        raise PacketError(f"git {' '.join(args)} failed")
    return result.stdout.decode("utf-8", errors="strict").strip()


def tree_receipt(root: Path) -> dict[str, Any]:
    records: list[dict[str, Any]] = []
    if not root.is_dir() or root.is_symlink():
        raise PacketError(f"cache root is not a regular directory: {root}")
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix().encode()):
        if path.is_symlink():
            raise PacketError(f"cache contains symlink: {path.relative_to(root)}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise PacketError(f"cache contains non-regular entry: {path.relative_to(root)}")
        records.append(
            {
                "path": path.relative_to(root).as_posix(),
                "mode": stat.S_IMODE(path.stat().st_mode),
                "size_bytes": path.stat().st_size,
                "sha256": sha256_file(path),
            }
        )
    return {"files": len(records), "digest": sha256(canonical_json(records)), "members": records}


def validate_dataset_row(
    row: Mapping[str, Any], population: Mapping[str, Any]
) -> Mapping[str, Any]:
    instance_id = row.get("instance_id")
    matches = [
        item
        for item in population.get("instances", [])
        if isinstance(item, dict) and item.get("instance_id") == instance_id
    ]
    if len(matches) != 1 or matches[0].get("included") is not True:
        raise PacketError("dataset row is not one included frozen population instance")
    frozen = matches[0]
    checks = {
        "repo": row.get("repo"),
        "base_commit": row.get("base_commit"),
        "dataset_row_sha256": sha256(canonical_json(dict(row))),
        "problem_statement_sha256": sha256(str(row.get("problem_statement", "")).encode("utf-8")),
        "gold_patch_sha256": sha256(str(row.get("patch", "")).encode("utf-8")),
        "test_patch_sha256": sha256(str(row.get("test_patch", "")).encode("utf-8")),
    }
    for field, actual in checks.items():
        if frozen.get(field) != actual:
            raise PacketError(f"frozen dataset {field} mismatch")
    return frozen


def validate_checkout(checkout: Path, frozen: Mapping[str, Any]) -> dict[str, str]:
    if checkout.is_symlink() or not checkout.is_dir():
        raise PacketError("checkout must be a regular directory")
    head = git(checkout, "rev-parse", "HEAD")
    tree = git(checkout, "rev-parse", "HEAD^{tree}")
    if head != frozen["base_commit"]:
        raise PacketError("checkout commit mismatch")
    if git(checkout, "status", "--porcelain", "--untracked-files=no"):
        raise PacketError("checkout has tracked modifications")
    patch_paths = set(patch_file_order(str(frozen.get("patch", ""))))
    untracked = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"],
        cwd=checkout,
        check=False,
        capture_output=True,
    )
    if untracked.returncode:
        raise PacketError("git untracked-file inventory failed")
    untracked_paths = {
        value.decode("utf-8", errors="strict")
        for value in untracked.stdout.split(b"\0")
        if value
    }
    if patch_paths & untracked_paths:
        raise PacketError("checkout has an untracked oracle-path file")
    return {"commit": head, "tree": tree}


def validate_artifact_receipt(
    receipt: Mapping[str, Any],
    receipt_path: Path,
    binary: Path,
    combined_core: Mapping[str, Any],
    expected_receipt_sha256: str,
    expected_head_sha: str,
    expected_github_artifact_digest: str,
) -> str:
    runtime = combined_core.get("runtime", {})
    projection = runtime.get("projection", {})
    components = projection.get("components", {})
    provenance = projection.get("provenance", {})
    artifact = projection.get("artifact", {})
    checks = (
        (receipt.get("head_sha"), provenance.get("head_sha"), "head"),
        (receipt.get("manifest_sha256"), runtime.get("manifest_sha256"), "manifest"),
        (receipt.get("archive_sha256"), artifact.get("archive_sha256"), "archive"),
        (sha256_file(binary), components.get("executable_sha256"), "binary"),
    )
    for actual, expected, label in checks:
        if actual != expected or not isinstance(actual, str):
            raise PacketError(f"qualified artifact {label} mismatch")
    if sha256_file(receipt_path) != expected_receipt_sha256:
        raise PacketError("qualified artifact receipt external digest mismatch")
    if receipt.get("head_sha") != expected_head_sha:
        raise PacketError("qualified artifact external head mismatch")
    if receipt.get("github_artifact_digest") != expected_github_artifact_digest:
        raise PacketError("qualified artifact external GitHub digest mismatch")
    return sha256(canonical_json(dict(receipt)))


def validate_frozen_lock(expected_bundle_digest: str) -> str:
    root = ROOT.resolve()
    errors: list[str] = []
    if PROTOCOL._has_symlink_component(root, PROTOCOL.LOCK_REL.parts):
        raise PacketError("frozen protocol lock path is unsafe")
    lock_path = root / PROTOCOL.LOCK_REL
    lock_bytes = lock_path.read_bytes()
    if PROTOCOL.SECRET_VALUE.search(lock_bytes.decode("utf-8", errors="ignore")):
        errors.append("credential-shaped value in lock manifest")
    lock = PROTOCOL.load_json_object(lock_path, "frozen protocol lock")
    if set(lock) != {
        "schema_version", "algorithm", "material_format", "files", "bundle_sha256"
    }:
        errors.append("lock field set drift")
    if type(lock.get("schema_version")) is not int or lock.get("schema_version") != 1:
        errors.append("lock schema_version drift")
    if lock.get("algorithm") != "sha256":
        errors.append("lock algorithm drift")
    if lock.get("material_format") != PROTOCOL.LOCK_MATERIAL_FORMAT:
        errors.append("lock material format drift")
    entries = lock.get("files")
    if not isinstance(entries, list) or not entries:
        errors.append("lock files are invalid")
        entries = []
    paths = [entry.get("path") for entry in entries if isinstance(entry, dict)]
    if paths != list(PROTOCOL.EXPECTED_LOCK_PATHS):
        errors.append("lock path set/order drift")
    for ordinal, entry in enumerate(entries, 1):
        if not isinstance(entry, dict) or set(entry) != {"path", "bytes", "sha256"}:
            errors.append(f"lock entry {ordinal} field set drift")
            continue
        raw_path = entry.get("path")
        byte_length = entry.get("bytes")
        digest = entry.get("sha256")
        if type(byte_length) is not int or byte_length < 0:
            errors.append(f"lock entry {ordinal} byte length drift")
        if not isinstance(digest, str) or not HEX64.fullmatch(digest):
            errors.append(f"lock entry {ordinal} digest is invalid")
        if not isinstance(raw_path, str):
            errors.append(f"lock entry {ordinal} path is unsafe")
            continue
        relative = PurePosixPath(raw_path)
        if (
            not relative.parts
            or relative.is_absolute()
            or ".." in relative.parts
            or "\\" in raw_path
            or raw_path != relative.as_posix()
            or PROTOCOL._has_symlink_component(root, relative.parts)
        ):
            errors.append(f"lock entry {ordinal} path is unsafe")
            continue
        candidate = root.joinpath(*relative.parts)
        try:
            resolved = candidate.resolve(strict=True)
            resolved.relative_to(root)
        except (OSError, RuntimeError, ValueError):
            errors.append(f"lock entry {ordinal} path is unsafe or missing")
            continue
        if not resolved.is_file():
            errors.append(f"lock entry {ordinal} file is missing")
            continue
        data = resolved.read_bytes()
        if len(data) != byte_length:
            errors.append(f"lock entry {ordinal} byte length drift")
        if sha256(data) != digest:
            errors.append(f"lock entry {ordinal} digest drift")
        if PROTOCOL.SECRET_VALUE.search(data.decode("utf-8", errors="ignore")):
            errors.append("credential-shaped value in locked file")
    material_digest = sha256(PROTOCOL._lock_material(entries))
    if lock.get("bundle_sha256") != material_digest:
        errors.append("bundle digest mismatch in lock")
    if material_digest != expected_bundle_digest:
        errors.append("frozen protocol bundle external digest mismatch")
    if PROTOCOL._has_symlink_component(root, PROTOCOL.DIGEST_REL.parts):
        errors.append("protocol digest path is unsafe")
    else:
        digest_path = root / PROTOCOL.DIGEST_REL
        if not digest_path.is_file() or digest_path.is_symlink():
            errors.append("protocol digest file is unsafe or missing")
        else:
            try:
                digest_file = digest_path.read_text(encoding="ascii").strip()
            except (OSError, UnicodeError):
                errors.append("protocol digest file is invalid")
            else:
                if digest_file != material_digest:
                    errors.append("protocol.sha256 does not match bundle")
    if errors:
        raise PacketError("frozen protocol lock validation failed: " + "; ".join(errors[:8]))
    return material_digest


def validate_frozen_inputs(expected_bundle_digest: str) -> tuple[dict[str, Any], dict[str, Any]]:
    validate_frozen_lock(expected_bundle_digest)
    protocol = PROTOCOL.load_json_object(DEFAULT_PROTOCOL, "frozen protocol")
    population = PROTOCOL.load_json_object(DEFAULT_POPULATION, "frozen population")
    errors: list[str] = []
    if sha256_file(DEFAULT_PROTOCOL) != PROTOCOL.EXPECTED_PROTOCOL_SHA256:
        errors.append("frozen protocol digest mismatch")
    if sha256_file(DEFAULT_POPULATION) != PROTOCOL.EXPECTED_POPULATION_SHA256:
        errors.append("frozen population digest mismatch")
    PROTOCOL.validate_protocol(protocol, errors)
    PROTOCOL.validate_population(population, errors)
    if errors:
        raise PacketError("frozen protocol validation failed: " + "; ".join(errors[:8]))
    return protocol, population


def validate_external_anchors(expected: Mapping[str, str]) -> None:
    sha_fields = (
        "protocol_bundle_sha256",
        "artifact_receipt_sha256",
        "cache_archive_sha256",
        "cache_sidecar_sha256",
        "cache_core_sha256",
    )
    if any(not HEX64.fullmatch(str(expected.get(field, ""))) for field in sha_fields):
        raise PacketError("external SHA-256 anchor is invalid")
    if not re.fullmatch(r"[0-9a-f]{40}", str(expected.get("artifact_head_sha", ""))):
        raise PacketError("external artifact head anchor is invalid")
    if not re.fullmatch(
        r"sha256:[0-9a-f]{64}", str(expected.get("github_artifact_digest", ""))
    ):
        raise PacketError("external GitHub artifact anchor is invalid")


def materialize_command_environment(
    binary: Path,
    output: Path,
    combined_core: Mapping[str, Any],
) -> tuple[dict[str, str], dict[str, Any]]:
    if "ANTHROPIC_API_KEY" in os.environ:
        raise PacketError("no-spend packet generation rejects ANTHROPIC_API_KEY")
    bundle = binary.parent
    models = bundle / "components/models"
    hf_home = models / "huggingface"
    reranker = models / "reranker"
    if not hf_home.is_dir() or not reranker.is_dir():
        raise PacketError("qualified artifact model roots are missing")
    components = combined_core.get("runtime", {}).get("projection", {}).get("components", {})
    lsp = components.get("lsp", {})
    lsp_files = {
        "toolchain-lock.json": "toolchain_lock_sha256",
        "inventory.json": "inventory_sha256",
        "descriptor-inventory.json": "descriptor_inventory_sha256",
        "provision-receipt.json": "provision_receipt_sha256",
        "probe-receipt.json": "probe_receipt_sha256",
    }
    for name, field in lsp_files.items():
        path = bundle / "components/lsp" / name
        if not path.is_file() or sha256_file(path) != lsp.get(field):
            raise PacketError(f"qualified artifact LSP {name} mismatch")
    lsp_root = output / "preprocessing/lsp"
    started = time.monotonic_ns()
    identity = LSP._materialize_verified_bundle_toolchain(
        bundle,
        bundle / "components/lsp/toolchain-lock.json",
        bundle / "components/lsp/inventory.json",
        lsp_root,
        ROOT,
    )
    environment = LSP.toolchain_environment(
        Path(identity["toolchain_root"]), output / "preprocessing/environment"
    )
    environment.update({
        "HF_HOME": str(hf_home),
        "FASTEMBED_CACHE_DIR": str(reranker),
        "HF_HUB_OFFLINE": "1",
        "TRANSFORMERS_OFFLINE": "1",
        "CANDLE_METAL_ENABLE_FAST_MATH": "1",
        "RNA_EMBEDDING_MODEL_FILES_DIGEST": components["embedding"]["files_digest"],
        "RNA_EMBEDDING_MODEL_SHA256": components["embedding"]["assets"]["model.safetensors"]["sha256"],
        "RNA_EMBEDDING_TOKENIZER_SHA256": components["embedding"]["assets"]["tokenizer.json"]["sha256"],
        "RNA_RERANKER_MODEL_FILES_DIGEST": components["reranker"]["files_digest"],
        "NO_PROXY": "*",
        "no_proxy": "*",
        "LANG": "C",
        "LC_ALL": "C",
        "TZ": "UTC",
    })
    LSP.bind_hf_default_cache(Path(environment["HF_HOME"]), Path(environment["HOME"]))
    environment.pop("RNA_SEMANTIC_ASSET_SEEDING", None)
    return environment, {
        "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
        "identity": identity,
    }


class CommandRecorder:
    def __init__(self, evidence: Path, environment: Mapping[str, str]) -> None:
        if evidence.exists() or evidence.is_symlink():
            raise PacketError("command evidence destination must be absent")
        evidence.mkdir(parents=True)
        self.evidence = evidence
        self.environment = dict(environment)
        self.results: list[CommandResult] = []

    def run(self, name: str, argv: Sequence[str], cwd: Path) -> CommandResult:
        if any(token in FORBIDDEN_COMMAND_TOKENS for token in argv):
            raise PacketError("packet command attempted forbidden enrichment operation")
        started = time.monotonic_ns()
        completed = subprocess.run(
            list(argv),
            cwd=cwd,
            env=self.environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        elapsed_ms = (time.monotonic_ns() - started) // 1_000_000
        result = CommandResult(
            name,
            tuple(argv),
            completed.returncode,
            completed.stdout,
            completed.stderr,
            elapsed_ms,
        )
        ordinal = len(self.results) + 1
        prefix = self.evidence / f"{ordinal:04d}-{name}"
        write_exclusive(prefix.with_suffix(".stdout"), result.stdout)
        write_exclusive(prefix.with_suffix(".stderr"), result.stderr)
        write_exclusive(prefix.with_suffix(".json"), canonical_json(result.receipt()) + b"\n")
        self.results.append(result)
        if completed.returncode:
            raise PacketError(f"RNA command failed: {name}")
        stderr = completed.stderr.decode("utf-8", errors="replace").lower()
        if any(marker in stderr for marker in FORBIDDEN_STDERR_MARKERS):
            raise PacketError(f"RNA command used a forbidden fallback: {name}")
        return result


def parse_nodes(output: bytes) -> list[ParsedNode]:
    text = output.decode("utf-8", errors="strict")
    nodes: list[ParsedNode] = []
    count = next((RESULT_COUNT.match(line) for line in text.splitlines() if RESULT_COUNT.match(line)), None)
    expected = int(count.group("count")) if count else None
    heading = f"### Code symbols ({expected} result(s))" if expected is not None else None
    section = text.split(heading, 1)[1] if heading is not None else text
    section = section.split("\n*Index:", 1)[0]
    for block in re.split(r"(?m)(?=^- \*\*)", section):
        kind_match = re.match(r"^- \*\*(?P<kind>[^*]+)\*\*", block)
        if kind_match is None:
            continue
        locations = list(
            re.finditer(
                r"`(?P<path>[^`\n]+)`:(?P<start>[0-9]+)-(?P<end>[0-9]+)(?:\s|$)",
                block,
            )
        )
        stable_ids = [
            match.group("stable_id")
            for line in block.splitlines()
            if (match := STABLE_ID_LINE.match(line)) is not None
        ]
        if not stable_ids:
            stable_ids = re.findall(r"\n  `(.*?)`\n(?=\n|$)", block, flags=re.DOTALL)
        if not locations or len(stable_ids) != 1:
            continue
        location = locations[-1]
        stable_id = stable_ids[0]
        path = location.group("path")
        nodes.append(
            ParsedNode(
                stable_id,
                kind_match.group("kind"),
                path,
                int(location.group("start")),
                int(location.group("end")),
            )
        )
    if expected is not None and len(nodes) != expected:
        raise PacketError(f"parsed {len(nodes)} of {expected} resolved result entries")
    stable_ids = [node.stable_id for node in nodes]
    if len(stable_ids) != len(set(stable_ids)):
        raise PacketError("RNA output contains duplicate stable IDs")
    return nodes


def parse_markdown_nodes(output: bytes, checkout: Path) -> list[ParsedNode]:
    text = output.decode("utf-8", errors="strict")
    marker = re.search(r"(?m)^### Markdown \((?P<count>[0-9]+) result\(s\)\)\s*$", text)
    if marker is None:
        return []
    expected = int(marker.group("count"))
    section = text[marker.end() :].lstrip("\n")
    section = section.split("\n*Index:", 1)[0]
    blocks = section.split("\n\n---\n\n") if section else []
    nodes: list[ParsedNode] = []
    checkout_root = checkout.resolve(strict=True)
    for ordinal, block in enumerate(blocks, 1):
        match = re.match(
            r"^- \(score: -?[0-9]+(?:\.[0-9]+)?\) `(?P<path>[^`\n]+)` > (?P<hierarchy>[^\n]*)\n\n(?P<body>[\s\S]*)$",
            block,
        )
        if match is None:
            raise PacketError(f"Markdown result {ordinal} format drift")
        raw_path = Path(match.group("path"))
        candidate_path = raw_path if raw_path.is_absolute() else checkout_root / raw_path
        try:
            resolved = candidate_path.resolve(strict=True)
            relative = resolved.relative_to(checkout_root).as_posix()
        except (OSError, RuntimeError, ValueError) as error:
            raise PacketError(f"Markdown result {ordinal} path is unsafe") from error
        if not resolved.is_file() or resolved.is_symlink():
            raise PacketError(f"Markdown result {ordinal} path is not a regular file")
        body = match.group("body")
        source = resolved.read_bytes().decode("utf-8", errors="strict")
        offsets = [found.start() for found in re.finditer(re.escape(body), source)]
        if len(offsets) != 1:
            raise PacketError(f"Markdown result {ordinal} body is not uniquely source-backed")
        start_line = source[: offsets[0]].count("\n") + 1
        end_line = start_line + body.count("\n")
        body_sha = sha256(body.encode("utf-8"))
        stable_id = "markdown:" + sha256(
            canonical_json([relative, start_line, end_line, body_sha])
        )
        nodes.append(
            ParsedNode(
                stable_id,
                "markdown_section",
                relative,
                start_line,
                end_line,
                body,
            )
        )
    if len(nodes) != expected:
        raise PacketError(f"parsed {len(nodes)} of {expected} Markdown result entries")
    return nodes


def parse_search_nodes(output: bytes, checkout: Path) -> list[ParsedNode]:
    nodes = [*parse_nodes(output), *parse_markdown_nodes(output, checkout)]
    stable_ids = [node.stable_id for node in nodes]
    if len(stable_ids) != len(set(stable_ids)):
        raise PacketError("RNA search output contains duplicate candidate IDs")
    return nodes


def raw_label_from_heading(heading: str) -> str:
    if not heading or heading != heading.strip():
        raise PacketError("invalid neighbor edge heading")
    raw = heading[0].lower() + heading[1:]
    raw = raw.replace(" ", "_")
    if not re.fullmatch(r"[A-Za-z0-9_.:/-]+", raw):
        raise PacketError("neighbor edge heading cannot be reconstructed")
    return raw


def project_edge_label(raw: str) -> str:
    return {
        "calls": "Calls",
        "referenced_by": "ReferencedBy",
        "depends_on": "DependsOn",
        "implements": "Implements",
        "defines": "Defines",
        "has_field": "Contains",
        "belongs_to": "Contains",
        "contains": "Contains",
        "tested_by": "Tests",
        "tests": "Tests",
        "imports": "Imports",
        "extends": "Extends",
    }.get(raw, "Other")


def parse_neighbors(
    output: bytes,
) -> tuple[list[tuple[str, ParsedNode, int]], list[dict[str, Any]]]:
    text = output.decode("utf-8", errors="strict")
    lines = text.splitlines()
    current_label: str | None = None
    current_count = 0
    seen_in_group = 0
    current_node: tuple[str, str, int, int] | None = None
    resolved: list[tuple[str, ParsedNode, int]] = []
    invalid: list[dict[str, Any]] = []
    cli_ordinal = 0
    for line_number, line in enumerate(lines, 1):
        if line.startswith("*Index:") or line == "### Capability readiness":
            break
        group = GROUP_HEADER.match(line)
        if group:
            if current_label is not None and seen_in_group != current_count:
                raise PacketError("neighbor group entry count drift")
            current_label = raw_label_from_heading(group.group("label"))
            current_count = int(group.group("count"))
            seen_in_group = 0
            current_node = None
            continue
        header = ENTRY_HEADER.match(line)
        if line.startswith("- **") and current_label is not None:
            if current_node is not None:
                cli_ordinal += 1
                invalid.append(
                    {
                        "raw_label": current_label,
                        "line": line_number - 1,
                        "cli_ordinal": cli_ordinal,
                        "reason": "missing_stable_id",
                    }
                )
                seen_in_group += 1
            if header is None:
                cli_ordinal += 1
                invalid.append(
                    {
                        "raw_label": current_label,
                        "line": line_number,
                        "cli_ordinal": cli_ordinal,
                        "reason": "unresolved_without_stable_id",
                    }
                )
                seen_in_group += 1
                current_node = None
                continue
            current_node = (
                header.group("kind"),
                header.group("path"),
                int(header.group("start")),
                int(header.group("end")),
            )
            continue
        stable = STABLE_ID_LINE.match(line)
        if stable and current_label is not None and current_node is not None:
            kind, path, start, end = current_node
            cli_ordinal += 1
            resolved.append(
                (
                    current_label,
                    ParsedNode(stable.group("stable_id"), kind, path, start, end),
                    cli_ordinal,
                )
            )
            seen_in_group += 1
            current_node = None
    if current_node is not None and current_label is not None:
        cli_ordinal += 1
        invalid.append(
            {
                "raw_label": current_label,
                "line": len(lines),
                "cli_ordinal": cli_ordinal,
                "reason": "missing_stable_id",
            }
        )
        seen_in_group += 1
    if current_label is not None and seen_in_group != current_count:
        raise PacketError("neighbor group entry count drift")
    return resolved, invalid


def parse_body(output: bytes, stable_id: str) -> tuple[str, str]:
    text = output.decode("utf-8", errors="strict")
    if f"`{stable_id}`" not in text:
        raise PacketError("body response does not bind requested stable ID")
    opening = FENCE_OPEN.search(text)
    if opening is None:
        raise PacketError("body response has no dynamic fence")
    fence = opening.group("fence")
    closing_token = "\n  " + fence + "\n"
    closing = text.find(closing_token, opening.end())
    if closing < 0:
        raise PacketError("body response dynamic fence is unterminated")
    if text.find(closing_token, closing + 1) >= 0:
        raise PacketError("body response contains multiple fenced payloads")
    return opening.group("tag").strip(), text[opening.end() : closing]


def language_from_tag(tag: str, path: str) -> str:
    normalized = tag.lower().strip()
    if normalized in LANGUAGE_BY_TAG:
        return LANGUAGE_BY_TAG[normalized]
    suffix = Path(path).suffix.lower()
    return {
        ".py": "Python",
        ".rs": "Rust",
        ".ts": "TypeScript",
        ".tsx": "TSX",
        ".js": "JavaScript",
        ".jsx": "JSX",
        ".go": "Go",
        ".cs": "C#",
        ".md": "Markdown",
        ".rst": "reStructuredText",
        ".toml": "TOML",
        ".yaml": "YAML",
        ".yml": "YAML",
        ".json": "JSON",
    }.get(suffix, "Unknown")


def load_upstream_locus_functions() -> SimpleNamespace:
    source = UPSTREAM_ARMS.read_bytes()
    if len(source) != 15118 or sha256(source) != UPSTREAM_ARMS_SHA256:
        raise PacketError("pinned upstream arms_v2.py bytes drift")
    module = ast.parse(source.decode("utf-8", errors="strict"), filename=str(UPSTREAM_ARMS))
    wanted = {
        "module_preamble_segments",
        "_decorator_map",
        "gold_edited_lines",
        "gold_units_v2",
        "_hunk_requirements",
        "hunk_preimages",
        "_contains_tolerant",
        "coverage_report",
    }
    definitions = [
        node
        for node in module.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name in wanted
    ]
    if {node.name for node in definitions} != wanted:
        raise PacketError("pinned upstream locus function closure drift")
    hunk_head = next(
        (
            node
            for node in module.body
            if isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name) and target.id == "_HUNK_HEAD" for target in node.targets)
        ),
        None,
    )
    if hunk_head is None:
        raise PacketError("pinned upstream _HUNK_HEAD is missing")
    isolated = ast.Module(body=[hunk_head, *definitions], type_ignores=[])
    ast.fix_missing_locations(isolated)
    namespace: dict[str, Any] = {
        "ast": ast,
        "re": re,
        "Tree": object,
        "Kind": SimpleNamespace(FUNCTION="function", CLASS="class"),
    }
    exec(compile(isolated, str(UPSTREAM_ARMS), "exec"), namespace)
    return SimpleNamespace(**{name: namespace[name] for name in wanted})


def patch_file_order(patch: str) -> list[str]:
    paths: list[str] = []
    for line in patch.split("\n"):
        if line.startswith("+++ b/"):
            path = line[6:].strip()
            if path not in paths:
                paths.append(path)
    return paths


def source_slice(checkout: Path, path: str, start: int, end: int) -> str:
    candidate = checkout / path
    try:
        candidate.resolve(strict=True).relative_to(checkout.resolve(strict=True))
    except (OSError, ValueError) as exc:
        raise PacketError(f"unsafe locus path: {path}") from exc
    text = candidate.read_bytes().decode("utf-8", errors="strict")
    lines = text.splitlines(keepends=True)
    logical_line_count = len(lines) + (1 if text.endswith(("\n", "\r")) else 0)
    if start < 1 or end < start or end > logical_line_count:
        raise PacketError(f"invalid locus span: {path}:{start}-{end}")
    return "".join(lines[start - 1 : min(end, len(lines))])


def make_locus(
    ordinal: int,
    source_kind: str,
    path: str,
    start: int,
    end: int,
    language: str,
    payload: str,
    seeds: Iterable[str],
) -> tuple[dict[str, Any], dict[str, Any]]:
    body = payload.encode("utf-8")
    body_sha = sha256(body)
    stable_id = f"locus:{source_kind}:{sha256(canonical_json([path, start, end, body_sha]))}"
    locus = {
        "ordinal": ordinal,
        "source_kind": source_kind,
        "stable_id": stable_id,
        "path": path,
        "start_line": start,
        "end_line": end,
        "language": language,
        "preimage_byte_length": len(body),
        "preimage_sha256": body_sha,
        "seed_stable_ids": sorted(set(seeds), key=lambda value: value.encode("utf-8")),
    }
    record = {
        "kind": "locus",
        "header": {
            "kind": "locus",
            "source_kind": source_kind,
            "ordinal": ordinal,
            "stable_id": stable_id,
            "path": path,
            "start_line": start,
            "end_line": end,
            "language": language,
            "full_body_byte_length": len(body),
            "full_body_sha256": body_sha,
            "score": None,
            "relationships": [],
        },
        "full_payload": payload,
        "minified_payload": payload,
    }
    return locus, record


def _node_facade(nodes: Sequence[ParsedNode]) -> Any:
    values = {}
    for node in nodes:
        kind = "function" if node.kind in {"function", "method"} else "class" if node.kind in {"class", "struct"} else node.kind
        values[node.stable_id] = SimpleNamespace(
            id=node.stable_id,
            kind=kind,
            path=node.path,
            start_line=node.start_line,
            end_line=node.end_line,
        )
    return SimpleNamespace(
        nodes=values,
        by_kind=lambda kind: [item for item in values.values() if item.kind == kind],
    )


def derive_loci(
    row: Mapping[str, Any],
    checkout: Path,
    binary: Path,
    recorder: CommandRecorder,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    upstream = load_upstream_locus_functions()
    patch = str(row["patch"])
    edited = upstream.gold_edited_lines(patch)
    paths = patch_file_order(patch)
    all_nodes: list[ParsedNode] = []
    location_evidence: list[dict[str, Any]] = []
    for ordinal, path in enumerate(paths, 1):
        if not (checkout / path).is_file():
            continue
        command = [
            str(binary), "--business-context", "disabled", "search", "--repo", str(checkout),
            "", "--file", path, "--limit", "10000", "--include-artifacts=false", "--compact",
        ]
        result = recorder.run(f"locus-nodes-{ordinal:03d}", command, checkout)
        parsed = parse_nodes(result.stdout)
        all_nodes.extend(parsed)
        location_evidence.append({"path": path, "command": result.receipt(), "nodes": [node.stable_id for node in parsed]})
    tree = _node_facade(all_nodes)
    deco_maps = {
        path: upstream._decorator_map(
            (checkout / path).read_bytes().decode("utf-8", errors="strict")
        )
        for path in paths
        if path.endswith(".py") and (checkout / path).is_file()
    }
    gold_ids = upstream.gold_units_v2(tree, edited, deco_maps)
    raw: list[tuple[int, bytes, int, int, str, str, str, list[str]]] = []
    path_order = {path: index for index, path in enumerate(paths)}
    for stable_id in gold_ids:
        node = tree.nodes[stable_id]
        start = deco_maps.get(node.path, {}).get(node.start_line, (node.start_line,))[0]
        payload = source_slice(checkout, node.path, start, node.end_line)
        raw.append((path_order[node.path], node.path.encode(), start, node.end_line, "gold_unit", node.path, payload, [stable_id]))
    for path in paths:
        file = checkout / path
        if path.endswith(".py") and file.is_file():
            text = file.read_bytes().decode("utf-8", errors="strict")
            for start, end, _ in upstream.module_preamble_segments(text):
                payload = source_slice(checkout, path, start, end)
                exact = [node.stable_id for node in all_nodes if node.path == path and node.start_line == start and node.end_line == end]
                raw.append((path_order[path], path.encode(), start, end, "module_preamble", path, payload, exact))
        elif file.is_file():
            payload = file.read_bytes().decode("utf-8", errors="strict")
            end = max(1, len(payload.splitlines()))
            exact = [node.stable_id for node in all_nodes if node.path == path and node.start_line == 1 and node.end_line == end]
            raw.append((path_order[path], path.encode(), 1, end, "whole_non_python_file", path, payload, exact))
        else:
            raw.append((path_order[path], path.encode(), 0, 0, "new_file", path, "", []))
    raw.sort(key=lambda item: (item[0], item[1], item[2], item[3], item[7][0].encode() if item[7] else b""))
    loci: list[dict[str, Any]] = []
    records: list[dict[str, Any]] = []
    for ordinal, (_, _, start, end, source_kind, path, payload, seeds) in enumerate(raw, 1):
        language = "Python" if source_kind in {"gold_unit", "module_preamble"} else "Unknown"
        locus, record = make_locus(ordinal, source_kind, path, start, end, language, payload, seeds)
        loci.append(locus)
        records.append(record)
    return loci, records, location_evidence


def semantic_search(
    query: str, checkout: Path, binary: Path, recorder: CommandRecorder
) -> tuple[list[ParsedNode], CommandResult]:
    argv = [
        str(binary), "--business-context", "disabled", "search", "--repo", str(checkout),
        "--search-mode", "hybrid", "--rerank", "--limit", "20", "--include-artifacts=false",
        "--include-markdown", "--compact", query,
    ]
    result = recorder.run("semantic-search", argv, checkout)
    text = result.stdout.decode("utf-8", errors="strict")
    if STRICT_SENTINEL not in text:
        raise PacketError("strict hybrid/RRF/rerank READY sentinel is missing")
    nodes = parse_search_nodes(result.stdout, checkout)[:20]
    return nodes, result


def traverse(
    loci: Sequence[Mapping[str, Any]], checkout: Path, binary: Path, recorder: CommandRecorder
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[ParsedNode]]:
    relationships: list[dict[str, Any]] = []
    evidence: list[dict[str, Any]] = []
    seen: set[tuple[Any, ...]] = set()
    nodes: dict[str, ParsedNode] = {}
    for locus in loci:
        for seed in locus["seed_stable_ids"]:
            for direction in ("incoming", "outgoing"):
                name = f"neighbors-{locus['ordinal']:03d}-{direction}-{len(evidence)+1:04d}"
                argv = [
                    str(binary), "--business-context", "disabled", "search", "--repo", str(checkout),
                    "--node", seed, "--mode", "neighbors", "--direction", direction, "--depth", "1",
                    "--include-artifacts=false", "--compact",
                ]
                result = recorder.run(name, argv, checkout)
                parsed, invalid = parse_neighbors(result.stdout)
                valid_stream: list[dict[str, Any]] = []
                for valid_ordinal, (raw_label, node, cli_ordinal) in enumerate(parsed, 1):
                    nodes.setdefault(node.stable_id, node)
                    relation = {
                        "source": node.stable_id if direction == "incoming" else seed,
                        "target": seed if direction == "incoming" else node.stable_id,
                        "edge_type": project_edge_label(raw_label),
                        "direction": direction,
                        "locus_ordinal": locus["ordinal"],
                        "cli_ordinal": cli_ordinal,
                    }
                    valid_stream.append({"raw_label": raw_label, "projected": relation["edge_type"], "relationship": relation})
                    if valid_ordinal > 50:
                        continue
                    key = tuple(relation[field] for field in PROTOCOL.PACKET_RELATIONSHIP_FIELDS)
                    if key not in seen:
                        seen.add(key)
                        relationships.append(relation)
                evidence.append(
                    {
                        "locus_ordinal": locus["ordinal"],
                        "seed_stable_id": seed,
                        "direction": direction,
                        "command": result.receipt(),
                        "valid_stream": valid_stream,
                        "invalid_entries": invalid,
                    }
                )
    relationships.sort(key=PROTOCOL._relationship_sort_key)
    return relationships, evidence, list(nodes.values())


def retrieve_candidate(
    node: ParsedNode,
    checkout: Path,
    binary: Path,
    recorder: CommandRecorder,
    ordinal: int,
) -> tuple[dict[str, Any], str, str, dict[str, Any]]:
    base_argv = [
        str(binary), "--business-context", "disabled", "search", "--repo", str(checkout),
        "--node", node.stable_id, "--include-body", "--include-artifacts=false", "--compact",
    ]
    full_result = recorder.run(f"candidate-{ordinal:03d}-full", base_argv, checkout)
    resolved = parse_nodes(full_result.stdout)
    if len(resolved) != 1 or resolved[0].stable_id != node.stable_id:
        raise PacketError("body response did not resolve exactly the requested node")
    node = resolved[0]
    tag, full = parse_body(full_result.stdout, node.stable_id)
    language = language_from_tag(tag, node.path)
    evidence = {
        "stable_id": node.stable_id,
        "full": full_result.receipt(),
        "minified": None,
    }
    body = full.encode("utf-8")
    candidate = {
        "acquisition_ordinal": 0,
        "stable_id": node.stable_id,
        "path": node.path,
        "start_line": node.start_line,
        "end_line": node.end_line,
        "language": language,
        "full_body_byte_length": len(body),
        "full_body_sha256": sha256(body),
        "semantic_rank": None,
        "graph_hops": None,
        "semantic_component": 0,
        "graph_component": 0,
        "total": 0,
        "eligibility_evidence": {
            "source_backed": bool(node.path and node.start_line >= 1 and node.end_line >= node.start_line),
            "complete_utf8_body": True,
            "locus_overlap": False,
            "excluded_record_class": node.path.startswith(".oh/"),
        },
        "eligibility": "eligible",
        "selected": False,
    }
    return candidate, full, full, evidence


def inline_markdown_candidate(node: ParsedNode) -> tuple[dict[str, Any], str, str, dict[str, Any]]:
    if node.inline_body is None:
        raise PacketError("inline Markdown candidate body is missing")
    body = node.inline_body.encode("utf-8")
    candidate = {
        "acquisition_ordinal": 0,
        "stable_id": node.stable_id,
        "path": node.path,
        "start_line": node.start_line,
        "end_line": node.end_line,
        "language": language_from_tag("", node.path),
        "full_body_byte_length": len(body),
        "full_body_sha256": sha256(body),
        "semantic_rank": None,
        "graph_hops": None,
        "semantic_component": 0,
        "graph_component": 0,
        "total": 0,
        "eligibility_evidence": {
            "source_backed": True,
            "complete_utf8_body": True,
            "locus_overlap": False,
            "excluded_record_class": node.path.startswith(".oh/"),
        },
        "eligibility": "eligible",
        "selected": False,
    }
    return candidate, node.inline_body, node.inline_body, {
        "stable_id": node.stable_id,
        "inline_markdown": True,
    }


def retrieve_minified_candidate(
    node: ParsedNode,
    full: str,
    checkout: Path,
    binary: Path,
    recorder: CommandRecorder,
    ordinal: int,
) -> tuple[str, dict[str, Any]]:
    if language_from_tag("", node.path) not in MINIFIABLE:
        return full, {"stable_id": node.stable_id, "minified": None}
    argv = [
        str(binary), "--business-context", "disabled", "search", "--repo", str(checkout),
        "--node", node.stable_id, "--include-body", "--include-artifacts=false", "--compact",
        "--minify-body",
    ]
    first_result = recorder.run(f"selected-{ordinal:03d}-minified", argv, checkout)
    first_tag, first = parse_body(first_result.stdout, node.stable_id)
    repeat_result = recorder.run(f"selected-{ordinal:03d}-minified-repeat", argv, checkout)
    repeat_tag, second = parse_body(repeat_result.stdout, node.stable_id)
    if (first_tag, first) != (repeat_tag, second):
        raise PacketError("structural minification is not repeat-stable")
    LSP._require_structural_minification_provenance(
        first_result.stdout.decode("utf-8", errors="strict")
    )
    LSP._require_structural_minification_provenance(
        repeat_result.stdout.decode("utf-8", errors="strict")
    )
    if not first or len(first.encode("utf-8")) > len(full.encode("utf-8")):
        raise PacketError("structural minification is empty or longer than full body")
    return first, {
        "stable_id": node.stable_id,
        "minified": first_result.receipt(),
        "minified_repeat": repeat_result.receipt(),
    }


def unavailable_candidate(node: ParsedNode) -> tuple[dict[str, Any], str, str, dict[str, Any]]:
    candidate = {
        "acquisition_ordinal": 0,
        "stable_id": node.stable_id,
        "path": "",
        "start_line": 0,
        "end_line": 0,
        "language": "Unknown",
        "full_body_byte_length": 0,
        "full_body_sha256": EMPTY_SHA256,
        "semantic_rank": None,
        "graph_hops": None,
        "semantic_component": 0,
        "graph_component": 0,
        "total": 0,
        "eligibility_evidence": {
            "source_backed": False,
            "complete_utf8_body": False,
            "locus_overlap": False,
            "excluded_record_class": False,
        },
        "eligibility": "not_source_backed",
        "selected": False,
    }
    return candidate, "", "", {"stable_id": node.stable_id, "unavailable": True}


def candidate_pool(
    semantic_nodes: Sequence[ParsedNode],
    traversal_nodes: Sequence[ParsedNode],
    relationships: Sequence[Mapping[str, Any]],
    loci: Sequence[Mapping[str, Any]],
    checkout: Path,
    binary: Path,
    recorder: CommandRecorder,
) -> tuple[list[dict[str, Any]], dict[str, tuple[str, str]], list[dict[str, Any]]]:
    node_by_id = {node.stable_id: node for node in [*semantic_nodes, *traversal_nodes]}
    ordered_ids = [node.stable_id for node in semantic_nodes]
    for relationship in relationships:
        locus = loci[relationship["locus_ordinal"] - 1]
        seeds = set(locus["seed_stable_ids"])
        candidate_id = relationship["source"] if relationship["direction"] == "incoming" else relationship["target"]
        if candidate_id in seeds:
            raise PacketError("traversal candidate endpoint equals locus seed")
        if candidate_id not in node_by_id:
            raise PacketError("traversal candidate endpoint lacks parsed node evidence")
        if candidate_id not in ordered_ids:
            ordered_ids.append(candidate_id)
    semantic_rank = {node.stable_id: index for index, node in enumerate(semantic_nodes, 1)}
    candidates: list[dict[str, Any]] = []
    payloads: dict[str, tuple[str, str]] = {}
    evidence: list[dict[str, Any]] = []
    for retrieval_ordinal, stable_id in enumerate(ordered_ids, 1):
        node = node_by_id[stable_id]
        if node.inline_body is not None:
            candidate, full, minified, body_evidence = inline_markdown_candidate(node)
        elif node.start_line >= 1 and node.end_line >= node.start_line and node.path:
            candidate, full, minified, body_evidence = retrieve_candidate(
                node, checkout, binary, recorder, retrieval_ordinal
            )
        else:
            candidate, full, minified, body_evidence = unavailable_candidate(node)
        candidate["semantic_rank"] = semantic_rank.get(stable_id)
        candidate["eligibility_evidence"]["locus_overlap"] = bool(
            PROTOCOL._candidate_overlaps_locus(candidate, loci)
        )
        candidate["eligibility"] = PROTOCOL._derived_candidate_eligibility(candidate["eligibility_evidence"])
        candidates.append(candidate)
        payloads[stable_id] = (full, minified)
        evidence.append(body_evidence)
    candidates.sort(key=lambda item: PROTOCOL._replayed_candidate_sort_key(item, relationships))
    for ordinal, candidate in enumerate(candidates, 1):
        candidate["acquisition_ordinal"] = ordinal
        semantic, graph, total, hops = PROTOCOL._derived_candidate_score(candidate, relationships)
        candidate.update(
            semantic_component=semantic,
            graph_component=graph,
            total=total,
            graph_hops=hops,
        )
    selected, omissions = PROTOCOL._replay_candidate_admission(candidates)
    for candidate, is_selected in zip(candidates, selected, strict=True):
        candidate["selected"] = is_selected
    for selected_ordinal, candidate in enumerate(
        (item for item in candidates if item["selected"] is True), 1
    ):
        stable_id = candidate["stable_id"]
        full, _ = payloads[stable_id]
        minified, minification_evidence = retrieve_minified_candidate(
            node_by_id[stable_id],
            full,
            checkout,
            binary,
            recorder,
            selected_ordinal,
        )
        payloads[stable_id] = (full, minified)
        evidence.append(minification_evidence)
    return candidates, payloads, [*evidence, {"omissions": omissions}]


def candidate_records(
    candidates: Sequence[Mapping[str, Any]],
    relationships: Sequence[Mapping[str, Any]],
    payloads: Mapping[str, tuple[str, str]],
    locus_count: int,
) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for selected_ordinal, candidate in enumerate(
        (item for item in candidates if item["selected"] is True), 1
    ):
        full, minified = payloads[candidate["stable_id"]]
        records.append(
            {
                "kind": "candidate",
                "header": {
                    "kind": "candidate",
                    "source_kind": "rna_node",
                    "ordinal": locus_count + selected_ordinal,
                    "stable_id": candidate["stable_id"],
                    "path": candidate["path"],
                    "start_line": candidate["start_line"],
                    "end_line": candidate["end_line"],
                    "language": candidate["language"],
                    "full_body_byte_length": candidate["full_body_byte_length"],
                    "full_body_sha256": candidate["full_body_sha256"],
                    "score": {
                        "semantic_component": candidate["semantic_component"],
                        "graph_component": candidate["graph_component"],
                        "total": candidate["total"],
                    },
                    "relationships": PROTOCOL._project_relationships(candidate["stable_id"], relationships),
                },
                "full_payload": full,
                "minified_payload": minified,
            }
        )
    return records


def packet_tokens(records: Sequence[Mapping[str, Any]], arm: str) -> int:
    try:
        if importlib.metadata.version("tiktoken") != PROTOCOL.TIKTOKEN_VERSION:
            raise PacketError(
                "packet token accounting requires exact tiktoken "
                + PROTOCOL.TIKTOKEN_VERSION
            )
        import tiktoken  # type: ignore
    except importlib.metadata.PackageNotFoundError as exc:
        raise PacketError(
            "packet token accounting requires exact tiktoken "
            + PROTOCOL.TIKTOKEN_VERSION
        ) from exc
    encoder = tiktoken.get_encoding("cl100k_base")
    total = 0
    for record in records:
        if record["kind"] != "candidate":
            continue
        payload = record["full_payload"] if arm == "B" else record["minified_payload"]
        total += len(encoder.encode_ordinary(payload))
    return total


def expressibility(
    row: Mapping[str, Any],
    locus_records: Sequence[Mapping[str, Any]],
    packet_b: bytes,
    packet_c: bytes,
) -> dict[str, Any]:
    upstream = load_upstream_locus_functions()
    preimages = upstream.hunk_preimages(str(row["patch"]))
    locus_payloads = [str(record["full_payload"]) for record in locus_records]
    locus_preimages = []
    for preimage in preimages:
        missing = [
            requirement
            for requirement in preimage["requirements"]
            if not any(
                upstream._contains_tolerant(payload, requirement)
                for payload in locus_payloads
            )
        ]
        locus_preimages.append(
            {
                **preimage,
                "requirements": missing,
            }
        )
    locus_report = upstream.coverage_report("", locus_preimages)
    reports = {
        "B": upstream.coverage_report(packet_b.decode("utf-8", errors="strict"), preimages),
        "C": upstream.coverage_report(packet_c.decode("utf-8", errors="strict"), preimages),
    }
    if not locus_report["full_coverage"]:
        raise PacketError("gold hunk pre-images are not expressible from editable loci")
    if not reports["B"]["full_coverage"] or not reports["C"]["full_coverage"]:
        raise PacketError("gold hunk pre-images are not fully expressible in packets")
    return {
        "upstream_source_sha256": UPSTREAM_ARMS_SHA256,
        "hunk_count": len(preimages),
        "locus_report": locus_report,
        "reports": reports,
        "requirements_sha256": sha256(canonical_json(preimages)),
    }


def verify_vector(vector: Mapping[str, Any]) -> None:
    errors: list[str] = []
    if set(vector) != {"metadata", "records"}:
        errors.append("packet vector field set drift")
    metadata = vector.get("metadata")
    records = vector.get("records")
    if not isinstance(metadata, dict) or set(metadata) != set(PROTOCOL.PACKET_METADATA_FIELDS):
        errors.append("packet metadata field set drift")
    if not isinstance(records, list) or not records:
        errors.append("packet records must be a non-empty list")
        records = []
    if isinstance(metadata, dict):
        if metadata.get("protocol_id") != "rna-act-context-swebench-v1":
            errors.append("packet protocol_id drift")
        if not isinstance(metadata.get("instance_id"), str) or not metadata["instance_id"]:
            errors.append("packet instance_id is invalid")
        if type(metadata.get("record_count")) is not int or metadata.get("record_count") != len(records):
            errors.append("packet record_count drift")
    for ordinal, record in enumerate(records, 1):
        if not isinstance(record, dict) or set(record) != {
            "kind", "header", "full_payload", "minified_payload"
        }:
            errors.append(f"packet record {ordinal} field set drift")
            continue
        header = record.get("header")
        full = record.get("full_payload")
        minified = record.get("minified_payload")
        if not isinstance(header, dict) or set(header) != set(PROTOCOL.PACKET_HEADER_FIELDS):
            errors.append(f"packet record {ordinal} header field set drift")
            continue
        if type(header.get("ordinal")) is not int or header["ordinal"] != ordinal:
            errors.append(f"packet record {ordinal} ordinal drift")
        if header.get("kind") != record.get("kind") or record.get("kind") not in {"locus", "candidate"}:
            errors.append(f"packet record {ordinal} kind drift")
        if not isinstance(full, str) or not isinstance(minified, str):
            errors.append(f"packet record {ordinal} payload must be text")
            continue
        full_bytes = full.encode("utf-8")
        if (
            type(header.get("full_body_byte_length")) is not int
            or header["full_body_byte_length"] != len(full_bytes)
            or header.get("full_body_sha256") != sha256(full_bytes)
        ):
            errors.append(f"packet record {ordinal} full payload binding drift")
        if record["kind"] == "locus":
            if minified != full or header.get("score") is not None or header.get("relationships") != []:
                errors.append(f"packet locus {ordinal} representation drift")
        else:
            if not minified or len(minified.encode("utf-8")) > len(full_bytes):
                errors.append(f"packet candidate {ordinal} minified payload drift")
    PROTOCOL.validate_acquisition_vector(
        metadata.get("acquisition") if isinstance(metadata, dict) else None,
        records,
        errors,
    )
    if errors:
        raise PacketError("packet vector invalid: " + "; ".join(errors[:8]))


def _build_impl(args: argparse.Namespace) -> dict[str, Any]:
    output = args.output.resolve()
    if output.exists() or output.is_symlink():
        raise PacketError("output root must be absent")
    output.mkdir(parents=True)
    validate_external_anchors(
        {
            "protocol_bundle_sha256": args.expected_digest,
            "artifact_receipt_sha256": args.expected_artifact_receipt_digest,
            "artifact_head_sha": args.expected_artifact_head_sha,
            "github_artifact_digest": args.expected_github_artifact_digest,
            "cache_archive_sha256": args.expected_cache_archive_sha256,
            "cache_sidecar_sha256": args.expected_cache_sidecar_sha256,
            "cache_core_sha256": args.expected_cache_core_sha256,
        }
    )
    protocol, population = validate_frozen_inputs(args.expected_digest)
    row = read_json(args.dataset_row)
    frozen = validate_dataset_row(row, population)
    checkout_identity = validate_checkout(args.checkout, {**frozen, "patch": row["patch"]})
    expected = {
        "repository": frozen["repo"],
        "commit": checkout_identity["commit"],
        "tree": checkout_identity["tree"],
    }
    for actual, wanted, label in (
        (sha256_file(args.cache_archive), args.expected_cache_archive_sha256, "cache archive"),
        (sha256_file(args.cache_manifest), args.expected_cache_sidecar_sha256, "cache sidecar"),
    ):
        if actual != wanted:
            raise PacketError(f"{label} external digest mismatch")
    started = time.monotonic_ns()
    cache_result = COMBINED.verify_combined_cache_archive(
        args.cache_archive,
        args.cache_manifest,
        expected=expected,
    )
    verification_ms = (time.monotonic_ns() - started) // 1_000_000
    if cache_result["core_sha256"] != args.expected_cache_core_sha256:
        raise PacketError("cache core external digest mismatch")
    started = time.monotonic_ns()
    injected = COMBINED.verify_combined_cache_archive(
        args.cache_archive,
        args.cache_manifest,
        expected=expected,
        inject_checkout=args.checkout,
    )
    injection_ms = (time.monotonic_ns() - started) // 1_000_000
    if injected != cache_result:
        raise PacketError("cache verification/injection result drift")
    cache_root = args.checkout / ".oh/.cache"
    cache_before = tree_receipt(cache_root)
    cache_before_summary = {"files": cache_before["files"], "digest": cache_before["digest"]}
    write_exclusive(output / "cache-before.json", canonical_json(cache_before_summary) + b"\n")
    artifact_receipt = read_json(args.artifact_receipt)
    artifact_receipt_sha = validate_artifact_receipt(
        artifact_receipt,
        args.artifact_receipt,
        args.rna_binary,
        cache_result["core"],
        args.expected_artifact_receipt_digest,
        args.expected_artifact_head_sha,
        args.expected_github_artifact_digest,
    )
    runtime_directory = tempfile.TemporaryDirectory(
        prefix="rna-packet-runtime-", dir=output.parent
    )
    environment, preprocessing = materialize_command_environment(
        args.rna_binary,
        Path(runtime_directory.name),
        cache_result["core"],
    )
    preprocessing["identity"] = {
        key: value
        for key, value in preprocessing["identity"].items()
        if key != "toolchain_root"
    }
    recorder = CommandRecorder(output / "command-evidence", environment)
    readiness = recorder.run(
        "fresh-reopen-readiness",
        [
            str(args.rna_binary), "--business-context", "disabled", "lsp-readiness",
            "--repo", str(args.checkout), "--json",
        ],
        args.checkout,
    )
    readiness_json = json.loads(readiness.stdout)
    if readiness_json.get("ready") is not True:
        raise PacketError("fresh-reopen full inventory is not READY")
    acquisition_started = time.monotonic_ns()
    loci, locus_records, locus_evidence = derive_loci(row, args.checkout, args.rna_binary, recorder)
    semantic_nodes, search_result = semantic_search(str(row["problem_statement"]), args.checkout, args.rna_binary, recorder)
    relationships, traversal_evidence, traversal_nodes = traverse(
        loci, args.checkout, args.rna_binary, recorder
    )
    candidates, payloads, candidate_evidence = candidate_pool(
        semantic_nodes,
        traversal_nodes,
        relationships,
        loci,
        args.checkout,
        args.rna_binary,
        recorder,
    )
    acquisition_ms = (time.monotonic_ns() - acquisition_started) // 1_000_000
    construction_started = time.monotonic_ns()
    omissions = candidate_evidence[-1]["omissions"]
    acquisition = {
        "schema_version": 1,
        "dataset_row_sha256": frozen["dataset_row_sha256"],
        "query_sha256": sha256(str(row["problem_statement"]).encode("utf-8")),
        "rna_artifact_receipt_sha256": artifact_receipt_sha,
        "loci": loci,
        "candidates": candidates,
        "relationships": relationships,
        "omissions": omissions,
    }
    records = [*locus_records, *candidate_records(candidates, relationships, payloads, len(loci))]
    vector = {
        "metadata": {
            "instance_id": row["instance_id"],
            "protocol_id": protocol["protocol_id"],
            "record_count": len(records),
            "acquisition": acquisition,
        },
        "records": records,
    }
    verify_vector(vector)
    packet_b = PROTOCOL.assemble_packet_vector(vector, "B")
    packet_c = PROTOCOL.assemble_packet_vector(vector, "C")
    coverage = expressibility(row, locus_records, packet_b, packet_c)
    cache_after = tree_receipt(cache_root)
    if cache_after["digest"] != cache_before["digest"]:
        raise PacketError("packet generation mutated the injected cache")
    cache_after_summary = {"files": cache_after["files"], "digest": cache_after["digest"]}
    write_exclusive(output / "cache-after.json", canonical_json(cache_after_summary) + b"\n")
    tokens = {"B": packet_tokens(records, "B"), "C": packet_tokens(records, "C")}
    trace = [result.receipt() for result in recorder.results]
    write_exclusive(output / "acquisition.json", canonical_json(acquisition) + b"\n")
    write_exclusive(output / "packet-vector.json", canonical_json(vector) + b"\n")
    write_exclusive(output / "packet-B.bin", packet_b)
    first_packet_ms = (time.monotonic_ns() - construction_started) // 1_000_000
    write_exclusive(output / "packet-C.bin", packet_c)
    construction_ms = (time.monotonic_ns() - construction_started) // 1_000_000
    runtime_directory.cleanup()
    runtime = cache_result["core"]["runtime"]["projection"]["components"]
    manifest = {
        "schema": "rna-swebench-context-packets-v1",
        "status": "ready",
        "instance_id": row["instance_id"],
        "protocol": {
            "protocol_id": protocol["protocol_id"],
            "bundle_sha256": args.expected_digest,
            "protocol_sha256": PROTOCOL.EXPECTED_PROTOCOL_SHA256,
            "population_sha256": PROTOCOL.EXPECTED_POPULATION_SHA256,
        },
        "dataset": {
            "row_sha256": frozen["dataset_row_sha256"],
            "problem_statement_sha256": frozen["problem_statement_sha256"],
            "gold_patch_sha256": frozen["gold_patch_sha256"],
            "test_patch_sha256": frozen["test_patch_sha256"],
        },
        "checkout": {"repository": frozen["repo"], **checkout_identity},
        "artifact": {
            "receipt_sha256": artifact_receipt_sha,
            "source_file_sha256": args.expected_artifact_receipt_digest,
            **artifact_receipt,
        },
        "cache": {
            "archive_sha256": cache_result["archive_sha256"],
            "sidecar_sha256": cache_result["sidecar_sha256"],
            "core_sha256": cache_result["core_sha256"],
            "combined_cache_tree_digest": cache_result["combined_cache_tree_digest"],
            "injected_tree_digest_before": cache_before["digest"],
            "injected_tree_digest_after": cache_after["digest"],
            "before_receipt_sha256": sha256_file(output / "cache-before.json"),
            "after_receipt_sha256": sha256_file(output / "cache-after.json"),
            "lsp_toolchain_lock_sha256": runtime["lsp"]["toolchain_lock_sha256"],
            "lsp_inventory_sha256": runtime["lsp"]["inventory_sha256"],
            "embedding_files_digest": runtime["embedding"]["files_digest"],
            "reranker_files_digest": runtime["reranker"]["files_digest"],
        },
        "context_mode": "disabled",
        "acquisition_sha256": sha256(canonical_json(acquisition)),
        "packets": {
            "B": {"file": "packet-B.bin", "sha256": sha256(packet_b), "size_bytes": len(packet_b), "cl100k_payload_tokens": tokens["B"]},
            "C": {"file": "packet-C.bin", "sha256": sha256(packet_c), "size_bytes": len(packet_c), "cl100k_payload_tokens": tokens["C"]},
        },
        "tokenizer": {
            "package": "tiktoken",
            "version": PROTOCOL.TIKTOKEN_VERSION,
            "encoding": "cl100k_base",
            "sdist_sha256": PROTOCOL.TIKTOKEN_SDIST_SHA256,
            "darwin_arm64_cp312_wheel_sha256": PROTOCOL.TIKTOKEN_DARWIN_ARM64_CP312_WHEEL_SHA256,
            "mergeable_ranks_sha256": PROTOCOL.CL100K_MERGEABLE_RANKS_SHA256,
        },
        "selection": {
            "candidates": [
                {
                    "stable_id": candidate["stable_id"],
                    "path": candidate["path"],
                    "start_line": candidate["start_line"],
                    "end_line": candidate["end_line"],
                    "score": {
                        "semantic_component": candidate["semantic_component"],
                        "graph_component": candidate["graph_component"],
                        "total": candidate["total"],
                    },
                    "selected": candidate["selected"],
                    "body_mode": {
                        "B": "full",
                        "C": "structural_minified"
                        if candidate["language"] in MINIFIABLE
                        else "full",
                    },
                }
                for candidate in candidates
            ],
            "relationships": relationships,
        },
        "coverage": coverage,
        "timing_ms": {
            "offline_lsp_materialization": preprocessing["elapsed_ms"],
            "cache_verification": verification_ms,
            "cache_injection": injection_ms,
            "fresh_reopen_readiness": readiness.elapsed_ms,
            "task_query_rerank": search_result.elapsed_ms,
            "context_acquisition": acquisition_ms,
            "time_to_first_packet_byte": first_packet_ms,
            "packet_construction_total": construction_ms,
        },
        "preprocessing": preprocessing,
        "command_trace": trace,
        "raw_evidence": {
            "locus_queries": locus_evidence,
            "traversal": traversal_evidence,
            "candidate_bodies": candidate_evidence[:-1],
        },
    }
    manifest["content_digest"] = sha256(canonical_json(manifest))
    write_exclusive(output / "manifest.json", canonical_json(manifest) + b"\n")
    verify_output(
        output,
        dataset_row=args.dataset_row,
        expected={
            "protocol_bundle_sha256": args.expected_digest,
            "artifact_receipt_sha256": args.expected_artifact_receipt_digest,
            "artifact_head_sha": args.expected_artifact_head_sha,
            "github_artifact_digest": args.expected_github_artifact_digest,
            "cache_archive_sha256": args.expected_cache_archive_sha256,
            "cache_sidecar_sha256": args.expected_cache_sidecar_sha256,
            "cache_core_sha256": args.expected_cache_core_sha256,
        },
    )
    return manifest


def build(args: argparse.Namespace) -> dict[str, Any]:
    row = read_json(args.dataset_row)
    repository = row.get("repo")
    if not isinstance(repository, str) or not repository:
        raise PacketError("dataset repository is invalid")
    original_origin = git(args.checkout, "remote", "get-url", "origin")
    git(
        args.checkout,
        "remote",
        "set-url",
        "origin",
        f"https://github.com/{repository}.git",
    )
    try:
        try:
            return _build_impl(args)
        except Exception as error:
            output = args.output.resolve()
            before_path = output / "cache-before.json"
            cache_root = args.checkout / ".oh/.cache"
            if before_path.is_file() and cache_root.is_dir():
                before = read_json(before_path)
                after = tree_receipt(cache_root)
                if after["digest"] != before.get("digest"):
                    raise PacketError(
                        "failed packet generation mutated the injected cache"
                    ) from error
            raise
    finally:
        git(args.checkout, "remote", "set-url", "origin", original_origin)


def _command_stdout(evidence_root: Path, ordinal: int, name: str) -> bytes:
    return (evidence_root / f"{ordinal:04d}-{name}.stdout").read_bytes()


def validate_command_protocol(
    trace: Sequence[Mapping[str, Any]],
    evidence_root: Path,
    vector: Mapping[str, Any],
    raw_evidence: Mapping[str, Any],
    row: Mapping[str, Any],
) -> None:
    if not trace:
        raise PacketError("packet command trace is missing")
    first_argv = trace[0].get("argv")
    if not isinstance(first_argv, list) or len(first_argv) != 7:
        raise PacketError("fresh readiness argv is invalid")
    binary, checkout = first_argv[0], first_argv[5]
    expected_commands: list[tuple[str, list[str]]] = [
        (
            "fresh-reopen-readiness",
            [
                binary,
                "--business-context",
                "disabled",
                "lsp-readiness",
                "--repo",
                checkout,
                "--json",
            ],
        )
    ]
    metadata = vector["metadata"]
    acquisition = metadata["acquisition"]
    loci = acquisition["loci"]
    patch_paths = patch_file_order(str(row["patch"]))
    existing_paths = {
        locus["path"] for locus in loci if locus["source_kind"] != "new_file"
    }
    locus_evidence = raw_evidence.get("locus_queries")
    if not isinstance(locus_evidence, list):
        raise PacketError("locus query evidence is invalid")
    expected_locus_paths = [path for path in patch_paths if path in existing_paths]
    if [item.get("path") for item in locus_evidence if isinstance(item, dict)] != expected_locus_paths:
        raise PacketError("locus query path/order drift")
    for patch_ordinal, path in enumerate(patch_paths, 1):
        if path not in existing_paths:
            continue
        expected_commands.append(
            (
                f"locus-nodes-{patch_ordinal:03d}",
                [
                    binary, "--business-context", "disabled", "search", "--repo", checkout,
                    "", "--file", path, "--limit", "10000", "--include-artifacts=false",
                    "--compact",
                ],
            )
        )
    expected_commands.append(
        (
            "semantic-search",
            [
                binary, "--business-context", "disabled", "search", "--repo", checkout,
                "--search-mode", "hybrid", "--rerank", "--limit", "20",
                "--include-artifacts=false", "--include-markdown", "--compact",
                str(row["problem_statement"]),
            ],
        )
    )
    traversal_evidence = raw_evidence.get("traversal")
    if not isinstance(traversal_evidence, list):
        raise PacketError("traversal evidence is invalid")
    traversal_index = 0
    for locus in loci:
        for seed in locus["seed_stable_ids"]:
            for direction in ("incoming", "outgoing"):
                traversal_index += 1
                expected_commands.append(
                    (
                        f"neighbors-{locus['ordinal']:03d}-{direction}-{traversal_index:04d}",
                        [
                            binary, "--business-context", "disabled", "search", "--repo", checkout,
                            "--node", seed, "--mode", "neighbors", "--direction", direction,
                            "--depth", "1", "--include-artifacts=false", "--compact",
                        ],
                    )
                )
    if len(traversal_evidence) != traversal_index:
        raise PacketError("traversal evidence count drift")
    candidates = acquisition["candidates"]
    candidate_by_id = {candidate["stable_id"]: candidate for candidate in candidates}
    ordered_ids = [
        candidate["stable_id"]
        for candidate in sorted(
            (item for item in candidates if item["semantic_rank"] is not None),
            key=lambda item: item["semantic_rank"],
        )
    ]
    for relationship in acquisition["relationships"]:
        candidate_id = (
            relationship["source"]
            if relationship["direction"] == "incoming"
            else relationship["target"]
        )
        if candidate_id not in ordered_ids:
            ordered_ids.append(candidate_id)
    for retrieval_ordinal, stable_id in enumerate(ordered_ids, 1):
        candidate = candidate_by_id[stable_id]
        if (
            candidate["eligibility_evidence"]["source_backed"]
            and not stable_id.startswith("markdown:")
        ):
            expected_commands.append(
                (
                    f"candidate-{retrieval_ordinal:03d}-full",
                    [
                        binary, "--business-context", "disabled", "search", "--repo", checkout,
                        "--node", stable_id, "--include-body", "--include-artifacts=false",
                        "--compact",
                    ],
                )
            )
    selected = [candidate for candidate in candidates if candidate["selected"] is True]
    for selected_ordinal, candidate in enumerate(selected, 1):
        if language_from_tag("", candidate["path"]) not in MINIFIABLE:
            continue
        argv = [
            binary, "--business-context", "disabled", "search", "--repo", checkout,
            "--node", candidate["stable_id"], "--include-body", "--include-artifacts=false",
            "--compact", "--minify-body",
        ]
        expected_commands.extend(
            (
                (f"selected-{selected_ordinal:03d}-minified", argv),
                (f"selected-{selected_ordinal:03d}-minified-repeat", argv),
            )
        )
    actual_commands = [(item.get("name"), item.get("argv")) for item in trace]
    if actual_commands != expected_commands:
        raise PacketError("command trace does not match frozen argv protocol")

    ordinal_by_name = {item["name"]: ordinal for ordinal, item in enumerate(trace, 1)}
    embedded_receipts: dict[str, Mapping[str, Any]] = {}
    for item in [*locus_evidence, *traversal_evidence, *raw_evidence.get("candidate_bodies", [])]:
        if not isinstance(item, dict):
            raise PacketError("raw command evidence entry is invalid")
        for field in ("command", "full", "minified", "minified_repeat"):
            receipt = item.get(field)
            if receipt is None:
                continue
            if not isinstance(receipt, dict) or not isinstance(receipt.get("name"), str):
                raise PacketError("raw command receipt is invalid")
            if receipt["name"] in embedded_receipts:
                raise PacketError("raw command receipt is duplicated")
            embedded_receipts[receipt["name"]] = receipt
    expected_embedded_names = set(ordinal_by_name) - {
        "fresh-reopen-readiness", "semantic-search"
    }
    if set(embedded_receipts) != expected_embedded_names or any(
        trace[ordinal_by_name[name] - 1] != receipt
        for name, receipt in embedded_receipts.items()
    ):
        raise PacketError("raw command receipt coverage drift")
    readiness = json.loads(_command_stdout(evidence_root, 1, "fresh-reopen-readiness"))
    if readiness.get("ready") is not True:
        raise PacketError("retained fresh readiness evidence is not READY")
    for item in locus_evidence:
        receipt = item.get("command")
        name = receipt.get("name") if isinstance(receipt, dict) else None
        if name not in ordinal_by_name or trace[ordinal_by_name[name] - 1] != receipt:
            raise PacketError("locus command receipt projection drift")
        parsed = parse_nodes(_command_stdout(evidence_root, ordinal_by_name[name], name))
        if [node.stable_id for node in parsed] != item.get("nodes"):
            raise PacketError("locus node evidence drift")
    semantic_ordinal = ordinal_by_name["semantic-search"]
    semantic_nodes = parse_search_nodes(
        _command_stdout(evidence_root, semantic_ordinal, "semantic-search"),
        Path(checkout),
    )[:20]
    semantic_ids = [
        candidate["stable_id"]
        for candidate in sorted(
            (item for item in candidates if item["semantic_rank"] is not None),
            key=lambda item: item["semantic_rank"],
        )
    ]
    if [node.stable_id for node in semantic_nodes] != semantic_ids:
        raise PacketError("semantic rank evidence drift")
    for item in traversal_evidence:
        receipt = item.get("command")
        name = receipt.get("name") if isinstance(receipt, dict) else None
        if name not in ordinal_by_name or trace[ordinal_by_name[name] - 1] != receipt:
            raise PacketError("traversal command receipt projection drift")
        parsed, invalid = parse_neighbors(
            _command_stdout(evidence_root, ordinal_by_name[name], name)
        )
        valid_stream = []
        for raw_label, node, cli_ordinal in parsed:
            direction = item["direction"]
            seed = item["seed_stable_id"]
            relation = {
                "source": node.stable_id if direction == "incoming" else seed,
                "target": seed if direction == "incoming" else node.stable_id,
                "edge_type": project_edge_label(raw_label),
                "direction": direction,
                "locus_ordinal": item["locus_ordinal"],
                "cli_ordinal": cli_ordinal,
            }
            valid_stream.append(
                {"raw_label": raw_label, "projected": relation["edge_type"], "relationship": relation}
            )
        if valid_stream != item.get("valid_stream") or invalid != item.get("invalid_entries"):
            raise PacketError("traversal parse evidence drift")
    records_by_id = {
        record["header"]["stable_id"]: record
        for record in vector["records"]
        if record["kind"] == "candidate"
    }
    semantic_node_by_id = {node.stable_id: node for node in semantic_nodes}
    for retrieval_ordinal, stable_id in enumerate(ordered_ids, 1):
        candidate = candidate_by_id[stable_id]
        if stable_id.startswith("markdown:"):
            node = semantic_node_by_id.get(stable_id)
            if node is None or node.inline_body is None:
                raise PacketError("inline Markdown semantic evidence is missing")
            payload = node.inline_body
            if (
                len(payload.encode("utf-8")) != candidate["full_body_byte_length"]
                or sha256(payload.encode("utf-8")) != candidate["full_body_sha256"]
                or (
                    candidate["selected"]
                    and (
                        records_by_id[stable_id]["full_payload"] != payload
                        or records_by_id[stable_id]["minified_payload"] != payload
                    )
                )
            ):
                raise PacketError("inline Markdown candidate payload drift")
            continue
        if not candidate["eligibility_evidence"]["source_backed"]:
            continue
        name = f"candidate-{retrieval_ordinal:03d}-full"
        parsed = parse_nodes(_command_stdout(evidence_root, ordinal_by_name[name], name))
        if len(parsed) != 1 or parsed[0].stable_id != stable_id:
            raise PacketError("candidate body node evidence drift")
        _, payload = parse_body(
            _command_stdout(evidence_root, ordinal_by_name[name], name), stable_id
        )
        if len(payload.encode("utf-8")) != candidate["full_body_byte_length"] or sha256(
            payload.encode("utf-8")
        ) != candidate["full_body_sha256"]:
            raise PacketError("candidate full-body evidence drift")
        if candidate["selected"] and records_by_id[stable_id]["full_payload"] != payload:
            raise PacketError("selected candidate full payload drift")
    for selected_ordinal, candidate in enumerate(selected, 1):
        if language_from_tag("", candidate["path"]) not in MINIFIABLE:
            continue
        payloads = []
        for suffix in ("minified", "minified-repeat"):
            name = f"selected-{selected_ordinal:03d}-{suffix}"
            stdout = _command_stdout(evidence_root, ordinal_by_name[name], name)
            LSP._require_structural_minification_provenance(
                stdout.decode("utf-8", errors="strict")
            )
            _, payload = parse_body(stdout, candidate["stable_id"])
            payloads.append(payload)
        if payloads[0] != payloads[1] or records_by_id[candidate["stable_id"]]["minified_payload"] != payloads[0]:
            raise PacketError("selected candidate minification evidence drift")


def verify_output(
    root: Path,
    *,
    dataset_row: Path | None = None,
    expected: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    if root.is_symlink() or not root.is_dir():
        raise PacketError("packet evidence root must be a regular directory")
    manifest = read_json(root / "manifest.json")
    if manifest.get("schema") != "rna-swebench-context-packets-v1" or manifest.get("status") != "ready":
        raise PacketError("packet manifest schema/status mismatch")
    projected = dict(manifest)
    content_digest = projected.pop("content_digest", None)
    if content_digest != sha256(canonical_json(projected)):
        raise PacketError("packet manifest content digest mismatch")
    vector = read_json(root / "packet-vector.json")
    verify_vector(vector)
    verified_row: Mapping[str, Any] | None = None
    verified_frozen: Mapping[str, Any] | None = None
    if dataset_row is not None:
        _, population = validate_frozen_inputs(manifest["protocol"]["bundle_sha256"])
        verified_row = read_json(dataset_row)
        verified_frozen = validate_dataset_row(verified_row, population)
    acquisition_bytes = canonical_json(vector.get("metadata", {}).get("acquisition")) + b"\n"
    if (root / "acquisition.json").read_bytes() != acquisition_bytes:
        raise PacketError("acquisition evidence differs from packet vector")
    if sha256(canonical_json(vector.get("metadata", {}).get("acquisition"))) != manifest["acquisition_sha256"]:
        raise PacketError("packet acquisition digest mismatch")
    acquisition = vector["metadata"]["acquisition"]
    expected_selection = {
        "candidates": [
            {
                "stable_id": candidate["stable_id"],
                "path": candidate["path"],
                "start_line": candidate["start_line"],
                "end_line": candidate["end_line"],
                "score": {
                    "semantic_component": candidate["semantic_component"],
                    "graph_component": candidate["graph_component"],
                    "total": candidate["total"],
                },
                "selected": candidate["selected"],
                "body_mode": {
                    "B": "full",
                    "C": "structural_minified"
                    if candidate["language"] in MINIFIABLE
                    else "full",
                },
            }
            for candidate in acquisition["candidates"]
        ],
        "relationships": acquisition["relationships"],
    }
    if "selection" in manifest and manifest["selection"] != expected_selection:
        raise PacketError("manifest selection projection drift")
    for arm in ("B", "C"):
        if manifest["packets"][arm]["file"] != f"packet-{arm}.bin":
            raise PacketError(f"packet {arm} path drift")
        packet = (root / f"packet-{arm}.bin").read_bytes()
        expected_packet = PROTOCOL.assemble_packet_vector(vector, arm)
        if (
            packet != expected_packet
            or sha256(packet) != manifest["packets"][arm]["sha256"]
            or len(packet) != manifest["packets"][arm]["size_bytes"]
            or packet_tokens(vector["records"], arm)
            != manifest["packets"][arm]["cl100k_payload_tokens"]
        ):
            raise PacketError(f"packet {arm} bytes/digest mismatch")
    if verified_row is not None and verified_frozen is not None:
        exact_top = {
            "schema", "status", "instance_id", "protocol", "dataset", "checkout",
            "artifact", "cache", "context_mode", "acquisition_sha256", "packets",
            "tokenizer", "selection", "coverage", "timing_ms", "preprocessing",
            "command_trace", "raw_evidence", "content_digest",
        }
        exact_nested = {
            "protocol": {"protocol_id", "bundle_sha256", "protocol_sha256", "population_sha256"},
            "dataset": {"row_sha256", "problem_statement_sha256", "gold_patch_sha256", "test_patch_sha256"},
            "checkout": {"repository", "commit", "tree"},
            "cache": {
                "archive_sha256", "sidecar_sha256", "core_sha256", "combined_cache_tree_digest",
                "injected_tree_digest_before", "injected_tree_digest_after",
                "before_receipt_sha256", "after_receipt_sha256", "lsp_toolchain_lock_sha256",
                "lsp_inventory_sha256", "embedding_files_digest", "reranker_files_digest",
            },
            "packets": {"B", "C"},
            "tokenizer": {
                "package", "version", "encoding", "sdist_sha256",
                "darwin_arm64_cp312_wheel_sha256", "mergeable_ranks_sha256",
            },
            "selection": {"candidates", "relationships"},
            "timing_ms": {
                "offline_lsp_materialization", "cache_verification", "cache_injection",
                "fresh_reopen_readiness", "task_query_rerank", "context_acquisition",
                "time_to_first_packet_byte", "packet_construction_total",
            },
            "preprocessing": {"elapsed_ms", "identity"},
            "raw_evidence": {"locus_queries", "traversal", "candidate_bodies"},
        }
        if set(manifest) != exact_top or any(
            not isinstance(manifest.get(field), dict)
            or set(manifest[field]) != fields
            for field, fields in exact_nested.items()
        ):
            raise PacketError("packet manifest field set drift")
        if any(
            not isinstance(manifest["packets"].get(arm), dict)
            or set(manifest["packets"][arm])
            != {"file", "sha256", "size_bytes", "cl100k_payload_tokens"}
            for arm in ("B", "C")
        ):
            raise PacketError("packet manifest arm field set drift")
        metadata = vector["metadata"]
        acquisition = metadata["acquisition"]
        if not (
            manifest["instance_id"] == metadata["instance_id"] == verified_row["instance_id"]
            and manifest["protocol"]["protocol_id"] == metadata["protocol_id"]
            == "rna-act-context-swebench-v1"
            and manifest["context_mode"] == "disabled"
            and manifest["dataset"]
            == {
                "row_sha256": verified_frozen["dataset_row_sha256"],
                "problem_statement_sha256": verified_frozen["problem_statement_sha256"],
                "gold_patch_sha256": verified_frozen["gold_patch_sha256"],
                "test_patch_sha256": verified_frozen["test_patch_sha256"],
            }
            and manifest["checkout"]["repository"] == verified_frozen["repo"]
            and manifest["checkout"]["commit"] == verified_frozen["base_commit"]
            and acquisition["dataset_row_sha256"] == verified_frozen["dataset_row_sha256"]
            and acquisition["query_sha256"]
            == sha256(str(verified_row["problem_statement"]).encode("utf-8"))
        ):
            raise PacketError("packet frozen identity drift")
        artifact = dict(manifest["artifact"])
        receipt_sha = artifact.pop("receipt_sha256", None)
        artifact.pop("source_file_sha256", None)
        if receipt_sha != sha256(canonical_json(artifact)) or acquisition[
            "rna_artifact_receipt_sha256"
        ] != receipt_sha:
            raise PacketError("packet artifact receipt projection drift")
    if manifest["cache"]["injected_tree_digest_before"] != manifest["cache"]["injected_tree_digest_after"]:
        raise PacketError("manifest records cache mutation")
    before_path = root / "cache-before.json"
    after_path = root / "cache-after.json"
    before = read_json(before_path)
    after = read_json(after_path)
    if (
        before != after
        or before.get("digest") != manifest["cache"]["injected_tree_digest_before"]
        or after.get("digest") != manifest["cache"]["injected_tree_digest_after"]
        or sha256_file(before_path) != manifest["cache"]["before_receipt_sha256"]
        or sha256_file(after_path) != manifest["cache"]["after_receipt_sha256"]
    ):
        raise PacketError("cache immutability receipts differ")
    trace = manifest.get("command_trace")
    if not isinstance(trace, list) or not trace:
        raise PacketError("packet command trace is missing")
    evidence_root = root / "command-evidence"
    expected_evidence: set[str] = set()
    forbidden = {"scan", "lsp-preflight", "structural-cache-replay"}
    for ordinal, receipt in enumerate(trace, 1):
        if set(receipt) != {
            "name", "argv", "exit_code", "stdout_sha256", "stderr_sha256",
            "stdout_size_bytes", "stderr_size_bytes", "elapsed_ms",
        }:
            raise PacketError("command receipt field set drift")
        name = receipt.get("name")
        argv = receipt.get("argv")
        if (
            not isinstance(name, str)
            or not re.fullmatch(r"[a-z0-9-]+", name)
            or not isinstance(argv, list)
            or not all(isinstance(token, str) for token in argv)
            or receipt.get("exit_code") != 0
            or any(token in forbidden for token in argv)
        ):
            raise PacketError("command receipt is invalid")
        if ordinal == 1:
            if "lsp-readiness" not in argv or "--json" not in argv:
                raise PacketError("first command is not fresh readiness")
        elif "search" not in argv:
            raise PacketError("non-readiness command is not an allowed search")
        prefix = f"{ordinal:04d}-{name}"
        paths = {
            suffix: evidence_root / f"{prefix}.{suffix}"
            for suffix in ("json", "stdout", "stderr")
        }
        expected_evidence.update(path.name for path in paths.values())
        if read_json(paths["json"]) != receipt:
            raise PacketError("command receipt file differs from manifest")
        for stream in ("stdout", "stderr"):
            data = paths[stream].read_bytes()
            if (
                len(data) != receipt[f"{stream}_size_bytes"]
                or sha256(data) != receipt[f"{stream}_sha256"]
            ):
                raise PacketError(f"command {stream} evidence mismatch")
    if json.loads((evidence_root / f"0001-{trace[0]['name']}.stdout").read_bytes()).get("ready") is not True:
        raise PacketError("retained fresh readiness evidence is not READY")
    semantic = next((item for item in trace if item["name"] == "semantic-search"), None)
    if semantic is None:
        raise PacketError("semantic search evidence is missing")
    semantic_index = trace.index(semantic) + 1
    if STRICT_SENTINEL not in (
        evidence_root / f"{semantic_index:04d}-semantic-search.stdout"
    ).read_text(encoding="utf-8", errors="strict"):
        raise PacketError("retained strict semantic sentinel is missing")
    actual_evidence = {
        path.name
        for path in evidence_root.iterdir()
        if path.is_file() and not path.is_symlink()
    }
    if actual_evidence != expected_evidence or any(path.is_symlink() for path in evidence_root.iterdir()):
        raise PacketError("command evidence inventory drift")
    if verified_row is not None:
        validate_command_protocol(
            trace,
            evidence_root,
            vector,
            manifest["raw_evidence"],
            verified_row,
        )
    expected_top = {
        "manifest.json", "acquisition.json", "packet-vector.json", "packet-B.bin",
        "packet-C.bin", "cache-before.json", "cache-after.json", "command-evidence",
    }
    if {path.name for path in root.iterdir()} != expected_top or any(path.is_symlink() for path in root.iterdir()):
        raise PacketError("packet evidence inventory drift")
    if expected is not None:
        validate_external_anchors(expected)
        comparisons = {
            "protocol_bundle_sha256": manifest["protocol"]["bundle_sha256"],
            "artifact_receipt_sha256": manifest["artifact"]["source_file_sha256"],
            "artifact_head_sha": manifest["artifact"]["head_sha"],
            "github_artifact_digest": manifest["artifact"]["github_artifact_digest"],
            "cache_archive_sha256": manifest["cache"]["archive_sha256"],
            "cache_sidecar_sha256": manifest["cache"]["sidecar_sha256"],
            "cache_core_sha256": manifest["cache"]["core_sha256"],
        }
        if comparisons != dict(expected):
            raise PacketError("packet external trust anchor mismatch")
        validate_frozen_inputs(expected["protocol_bundle_sha256"])
    if verified_row is not None:
        locus_records = [record for record in vector["records"] if record["kind"] == "locus"]
        coverage = expressibility(
            verified_row,
            locus_records,
            (root / "packet-B.bin").read_bytes(),
            (root / "packet-C.bin").read_bytes(),
        )
        if coverage != manifest["coverage"]:
            raise PacketError("packet expressibility evidence drift")
    return manifest


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    commands = value.add_subparsers(dest="command", required=True)
    build_command = commands.add_parser("build", help="build and self-verify no-spend B/C packets")
    build_command.add_argument("--checkout", type=Path, required=True)
    build_command.add_argument("--dataset-row", type=Path, required=True)
    build_command.add_argument("--cache-archive", type=Path, required=True)
    build_command.add_argument("--cache-manifest", type=Path, required=True)
    build_command.add_argument("--artifact-receipt", type=Path, required=True)
    build_command.add_argument("--rna-binary", type=Path, required=True)
    build_command.add_argument("--output", type=Path, required=True)
    for command in (build_command,):
        command.add_argument("--expected-digest", required=True)
        command.add_argument("--expected-artifact-receipt-digest", required=True)
        command.add_argument("--expected-artifact-head-sha", required=True)
        command.add_argument("--expected-github-artifact-digest", required=True)
        command.add_argument("--expected-cache-archive-sha256", required=True)
        command.add_argument("--expected-cache-sidecar-sha256", required=True)
        command.add_argument("--expected-cache-core-sha256", required=True)
    verify_command = commands.add_parser("verify", help="verify existing packet evidence offline")
    verify_command.add_argument("--root", type=Path, required=True)
    verify_command.add_argument("--dataset-row", type=Path, required=True)
    verify_command.add_argument("--expected-digest", required=True)
    verify_command.add_argument("--expected-artifact-receipt-digest", required=True)
    verify_command.add_argument("--expected-artifact-head-sha", required=True)
    verify_command.add_argument("--expected-github-artifact-digest", required=True)
    verify_command.add_argument("--expected-cache-archive-sha256", required=True)
    verify_command.add_argument("--expected-cache-sidecar-sha256", required=True)
    verify_command.add_argument("--expected-cache-core-sha256", required=True)
    return value


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "build":
            result = build(args)
        else:
            result = verify_output(
                args.root,
                dataset_row=args.dataset_row,
                expected={
                    "protocol_bundle_sha256": args.expected_digest,
                    "artifact_receipt_sha256": args.expected_artifact_receipt_digest,
                    "artifact_head_sha": args.expected_artifact_head_sha,
                    "github_artifact_digest": args.expected_github_artifact_digest,
                    "cache_archive_sha256": args.expected_cache_archive_sha256,
                    "cache_sidecar_sha256": args.expected_cache_sidecar_sha256,
                    "cache_core_sha256": args.expected_cache_core_sha256,
                },
            )
        print(json.dumps({"status": "ready", "instance_id": result["instance_id"], "manifest_sha256": sha256_file((args.output if args.command == "build" else args.root) / "manifest.json")}, sort_keys=True))
        return 0
    except (PacketError, COMBINED.ToolchainError, OSError, UnicodeError, json.JSONDecodeError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
