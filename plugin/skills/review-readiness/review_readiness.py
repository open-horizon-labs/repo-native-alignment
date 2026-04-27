#!/usr/bin/env python3
"""Generate a review-readiness report from git diff + RNA extracted symbols.

This is intentionally a skill-level walking skeleton, not a core RNA API.
It uses cheap sources first:
- git diff hunks
- current RNA extracted symbol ranges from `repo-native-alignment search`
- symbol metadata already rendered by RNA (stable ID, parent, in/out counts)

It does not run LSP, embeddings, or before/after graph extraction.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable

SYMBOL_KINDS = ["function", "struct", "enum", "trait", "type_alias", "const", "module"]


@dataclass
class Symbol:
    kind: str
    name: str
    language: str
    file: str
    start: int
    end: int
    node_id: str | None = None
    signature: str | None = None
    parent: str | None = None
    in_edges: int | None = None
    out_edges: int | None = None

    def is_low_signal_literal(self) -> bool:
        """Return true for synthetic literal-like constants that make review output noisy."""
        if self.kind != "const" or self.signature is None:
            return False
        stripped = self.signature.strip()
        return stripped.startswith(('"', "'"))

    @property
    def span_len(self) -> int:
        return max(0, self.end - self.start)


@dataclass
class Hunk:
    old_start: int
    old_len: int
    new_start: int
    new_len: int
    header: str


@dataclass
class ChangedFile:
    path: str
    old_path: str
    status: str = "modified"
    hunks: list[Hunk] = field(default_factory=list)


@dataclass
class MappedHunk:
    file: ChangedFile
    hunk: Hunk
    symbols: list[Symbol]


def run(cmd: list[str], cwd: Path) -> str:
    try:
        return subprocess.check_output(cmd, cwd=cwd, text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as exc:
        raise SystemExit(f"command failed ({' '.join(cmd)}):\n{exc.output}") from exc


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Report review readiness by mapping a diff to RNA extracted symbols."
    )
    parser.add_argument("--repo", default=".", help="repository path (default: current directory)")
    parser.add_argument("--base", help="base ref/sha for git diff")
    parser.add_argument("--head", help="head ref/sha for git diff; requires --base")
    parser.add_argument("--pr", type=int, help="GitHub PR number; uses gh pr diff")
    parser.add_argument(
        "--symbol-limit",
        type=int,
        default=20000,
        help="max symbols per kind to request from RNA (default: 20000)",
    )
    parser.add_argument(
        "--max-symbols-per-hunk",
        type=int,
        default=3,
        help="max overlapping symbols to print per hunk (default: 3)",
    )
    return parser.parse_args()


def diff_text(args: argparse.Namespace, repo: Path) -> str:
    if args.pr is not None:
        return run(["gh", "pr", "diff", str(args.pr), "--", "--unified=0"], repo)
    if args.base and args.head:
        return run(["git", "diff", "--unified=0", args.base, args.head], repo)
    if args.base:
        return run(["git", "diff", "--unified=0", f"{args.base}...HEAD"], repo)
    return run(["git", "diff", "--unified=0"], repo)


def parse_diff(text: str) -> list[ChangedFile]:
    files: list[ChangedFile] = []
    current: ChangedFile | None = None
    for line in text.splitlines():
        if line.startswith("diff --git "):
            if current is not None:
                files.append(current)
            parts = line.split()
            old_path = parts[2][2:]
            path = parts[3][2:]
            current = ChangedFile(path=path, old_path=old_path)
            continue
        if current is None:
            continue
        if line.startswith("new file mode"):
            current.status = "added"
        elif line.startswith("deleted file mode"):
            current.status = "deleted"
        elif line.startswith("rename from "):
            current.old_path = line.removeprefix("rename from ")
            current.status = "renamed"
        elif line.startswith("rename to "):
            current.path = line.removeprefix("rename to ")
            current.status = "renamed"
        elif line.startswith("@@"):
            match = re.search(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))?", line)
            if match:
                current.hunks.append(
                    Hunk(
                        old_start=int(match.group(1)),
                        old_len=int(match.group(2) or "1"),
                        new_start=int(match.group(3)),
                        new_len=int(match.group(4) or "1"),
                        header=line,
                    )
                )
    if current is not None:
        files.append(current)
    return files


def load_symbols(repo: Path, limit: int) -> dict[str, list[Symbol]]:
    symbols: list[Symbol] = []
    header_re = re.compile(
        r"- \*\*(?P<kind>[^*]+)\*\* `(?P<name>[^`]+)` "
        r"\((?P<language>[^)]+)\) `(?P<file>[^`]+)`:(?P<start>\d+)-(?P<end>\d+)"
    )
    current: Symbol | None = None

    for kind in SYMBOL_KINDS:
        output = run(
            [
                "repo-native-alignment",
                "search",
                "",
                "--kind",
                kind,
                "--limit",
                str(limit),
                "--search-mode",
                "keyword",
            ],
            repo,
        )
        for line in output.splitlines():
            header = header_re.search(line)
            if header:
                current = Symbol(
                    kind=header.group("kind"),
                    name=header.group("name"),
                    language=header.group("language"),
                    file=header.group("file"),
                    start=int(header.group("start")),
                    end=int(header.group("end")),
                )
                symbols.append(current)
                continue
            if current is None:
                continue
            stripped = line.strip()
            if stripped.startswith("ID: `"):
                current.node_id = stripped.removeprefix("ID: `").removesuffix("`")
            elif stripped.startswith("Sig: `"):
                current.signature = stripped.removeprefix("Sig: `").removesuffix("`")
            elif stripped.startswith("Parent: "):
                current.parent = stripped.removeprefix("Parent: ")
            elif stripped.startswith("Out: "):
                edge_match = re.search(r"Out: (\d+)", stripped)
                current.out_edges = int(edge_match.group(1)) if edge_match else None
            elif stripped.startswith("In: "):
                edge_match = re.search(r"In: (\d+)", stripped)
                current.in_edges = int(edge_match.group(1)) if edge_match else None

    by_file: dict[str, list[Symbol]] = {}
    for symbol in symbols:
        by_file.setdefault(symbol.file, []).append(symbol)
    for file_symbols in by_file.values():
        file_symbols.sort(key=lambda s: (s.span_len, s.start, s.end))
    return by_file


def map_hunk(file_symbols: Iterable[Symbol], hunk: Hunk, max_symbols: int) -> list[Symbol]:
    if hunk.new_len == 0:
        return []
    start = hunk.new_start
    end = hunk.new_start + max(hunk.new_len, 1) - 1
    candidates: list[tuple[int, int, Symbol]] = []
    for symbol in file_symbols:
        if symbol.is_low_signal_literal():
            continue
        if symbol.end >= start and symbol.start <= end:
            overlap = max(0, min(symbol.end, end) - max(symbol.start, start) + 1)
            candidates.append((overlap, symbol.span_len, symbol))
    candidates.sort(key=lambda item: (-item[0], item[1], item[2].start))
    return [item[2] for item in candidates[:max_symbols]]


def hunk_reason(hunk: Hunk) -> str:
    if hunk.new_len == 0:
        return "deleted hunk; base-side symbol identity unavailable without base graph"
    return "no current extracted symbol overlap; keep as file/hunk-level change"


def append_untracked_files(files: list[ChangedFile], repo: Path) -> list[ChangedFile]:
    """Include untracked files in working-tree mode so new skill/files are visible."""
    seen = {f.path for f in files}
    status = run(["git", "status", "--porcelain", "--untracked-files=all"], repo)
    for line in status.splitlines():
        if not line.startswith("?? "):
            continue
        path = line[3:]
        if path not in seen:
            files.append(ChangedFile(path=path, old_path="/dev/null", status="untracked"))
            seen.add(path)
    return files


def render_report(files: list[ChangedFile], by_file: dict[str, list[Symbol]], max_symbols: int) -> str:
    mapped: list[MappedHunk] = []
    unmapped: list[MappedHunk] = []
    for changed_file in files:
        file_symbols = by_file.get(changed_file.path, [])
        for hunk in changed_file.hunks:
            symbols = map_hunk(file_symbols, hunk, max_symbols)
            bucket = mapped if symbols else unmapped
            bucket.append(MappedHunk(file=changed_file, hunk=hunk, symbols=symbols))

    total_hunks = len(mapped) + len(unmapped)
    code_files = [f for f in files if by_file.get(f.path)]
    lines: list[str] = []
    lines.append("# Review Readiness")
    lines.append("")
    lines.append("## Summary")
    lines.append(f"- changed files: {len(files)}")
    lines.append(f"- files with extracted symbols: {len(code_files)}")
    lines.append(f"- hunks: {total_hunks}")
    if total_hunks:
        lines.append(f"- hunks mapped to current extracted symbols: {len(mapped)}/{total_hunks} ({len(mapped) / total_hunks:.1%})")
    else:
        lines.append("- hunks mapped to current extracted symbols: 0/0")
    lines.append("")
    lines.append("## Changed Symbols")
    seen_symbols: set[str] = set()
    for item in mapped:
        range_label = f"+{item.hunk.new_start},{item.hunk.new_len} / -{item.hunk.old_start},{item.hunk.old_len}"
        for symbol in item.symbols:
            key = symbol.node_id or f"{symbol.file}:{symbol.name}:{symbol.kind}:{symbol.start}"
            if key in seen_symbols:
                continue
            seen_symbols.add(key)
            lines.append(f"### `{symbol.name}` ({symbol.kind}, {symbol.language})")
            lines.append(f"- file: `{symbol.file}`:{symbol.start}-{symbol.end}")
            if symbol.node_id:
                lines.append(f"- node_id: `{symbol.node_id}`")
            if symbol.parent:
                lines.append(f"- parent: {symbol.parent}")
            if symbol.in_edges is not None or symbol.out_edges is not None:
                lines.append(f"- graph context: in={symbol.in_edges if symbol.in_edges is not None else 'unknown'}, out={symbol.out_edges if symbol.out_edges is not None else 'unknown'}")
            if symbol.signature:
                lines.append(f"- signature: `{symbol.signature}`")
            lines.append(f"- mapped from hunk: `{range_label}`")
            lines.append("- mapping provenance: git diff line range + current RNA extracted symbol range")
            lines.append("")
    if not seen_symbols:
        lines.append("No hunks mapped to extracted symbols. Treat this as file/artifact-level review context.")
        lines.append("")

    lines.append("## File / Hunk-Level Changes")
    for item in unmapped:
        range_label = f"+{item.hunk.new_start},{item.hunk.new_len} / -{item.hunk.old_start},{item.hunk.old_len}"
        lines.append(f"- `{item.file.path}` ({item.file.status}) `{range_label}` — {hunk_reason(item.hunk)}")
    files_without_hunks = [changed_file for changed_file in files if not changed_file.hunks]
    for changed_file in files_without_hunks:
        lines.append(f"- `{changed_file.path}` ({changed_file.status}) — file-level change; no diff hunks available")
    if not unmapped and not files_without_hunks:
        lines.append("No unmapped hunks or file-level changes.")
    lines.append("")

    lines.append("## Readiness")
    lines.append("- changed files/hunks: ready (git diff)")
    lines.append("- current symbol overlap: ready for mapped hunks, partial overall if unmapped hunks exist")
    lines.append("- existing graph context: ready where stable node IDs / edge counts are present in RNA output")
    lines.append("- exact incoming semantic refs/callers: not run (no LSP used)")
    lines.append("- deleted symbol identity: unavailable without base-side graph/symbol mapping")
    lines.append("- embeddings / semantic search: not required for first-pass review readiness")
    lines.append("")
    lines.append("## Recommended Follow-Ups")
    lines.append("- For mapped high-risk/exported symbols, run targeted impact/reference lookup only if review needs exact callers.")
    lines.append("- For deleted hunks, use a base graph or base-side symbol extraction before claiming which symbol was removed.")
    lines.append("- For unmapped config/docs/artifact changes, review at file/hunk level and join to outcomes/guardrails manually if relevant.")
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    repo = Path(args.repo).resolve()
    diff = diff_text(args, repo)
    files = parse_diff(diff)
    if args.pr is None and args.base is None and args.head is None:
        files = append_untracked_files(files, repo)
    symbols = load_symbols(repo, args.symbol_limit)
    print(render_report(files, symbols, args.max_symbols_per_hunk))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
