"""exp-014b patch builder v2 — fixes the audited defects of exp-014's edit_patch.py.

Fixes (SOTA_FAILURE_AUDIT.md E4 + B2/fix list):
  - path strip: `lstrip("ab/")` was a char-SET strip that mangled paths like a/astropy/... ->
    stropy/... ; replaced with an anchored regex.
  - ambiguity: a SEARCH matching more than once is now a FAILURE (status "ambiguous"), not a
    silent first-occurrence edit.
  - per-edit status: apply_edits_detailed returns (state, results) where each result carries a
    reason usable as retry feedback (matched / no_match / ambiguous / no_file / empty_search /
    created).
  - new files: an edit with an EMPTY SEARCH whose path does not exist creates the file with the
    REPLACE content; make_diff emits a proper /dev/null new-file diff.
  - state threading: `state` ({rel: content}) persists across retry rounds so later edits apply on
    top of earlier ones.
"""
from __future__ import annotations

import difflib
import re
from pathlib import Path

_EDIT_RE = re.compile(
    r"\*\*\* FILE:\s*(?P<file>.+?)[ \t]*\n"
    r"\*\*\* SEARCH[ \t]*\n(?P<search>.*?)"
    r"\*\*\* REPLACE[ \t]*\n(?P<replace>.*?)\n?"
    r"\*\*\* END",
    re.S,
)
_AB_PREFIX = re.compile(r"^[ab]/")


def parse_edits(text: str) -> list[dict]:
    out = []
    for m in _EDIT_RE.finditer(text):
        out.append({"file": _AB_PREFIX.sub("", m.group("file").strip()).lstrip("/"),
                    "search": m.group("search").rstrip("\n"),
                    "replace": m.group("replace")})
    return out


def _find_block(content: str, search: str) -> tuple[int, int, int]:
    """Locate `search` in `content`. Returns (n_matches, start, end) for the first match.
    Exact match first, then trailing-whitespace-tolerant line match."""
    n = content.count(search)
    if n:
        i = content.index(search)
        return n, i, i + len(search)
    clines = content.split("\n")
    slines = [l.rstrip() for l in search.split("\n")]
    while slines and slines[-1] == "":
        slines.pop()
    if not slines:
        return 0, -1, -1
    hits = []
    for i in range(len(clines) - len(slines) + 1):
        if [l.rstrip() for l in clines[i:i + len(slines)]] == slines:
            hits.append(i)
    if not hits:
        return 0, -1, -1
    start = sum(len(l) + 1 for l in clines[:hits[0]])
    end = start + sum(len(l) + 1 for l in clines[hits[0]:hits[0] + len(slines)]) - 1
    return len(hits), start, end


def apply_edits_detailed(repo_path, edits: list[dict], state: dict[str, str] | None = None,
                         ) -> tuple[dict[str, str], list[dict]]:
    """Apply edits on top of `state` ({rel: content}, mutated and returned). Each result:
    {"file", "status", "detail"} with status in
    matched | created | no_match | ambiguous | no_file | empty_search."""
    repo_path = Path(repo_path)
    state = state if state is not None else {}
    results = []

    def res(e, status, detail=""):
        results.append({"file": e["file"], "status": status, "detail": detail,
                        "search": e["search"]})

    for e in edits:
        rel = e["file"]
        p = repo_path / rel
        on_disk = p.is_file()
        search = e["search"].strip("\n")
        replace = e["replace"].strip("\n")

        if not search.strip():
            if on_disk or rel in state:
                res(e, "empty_search", "SEARCH is empty but the file already exists — "
                                       "empty SEARCH is only for creating a NEW file")
            else:
                state[rel] = replace + ("\n" if not replace.endswith("\n") else "")
                res(e, "created")
            continue

        if rel in state:
            cur = state[rel]
        elif on_disk:
            cur = p.read_text(encoding="utf-8", errors="ignore")
        else:
            res(e, "no_file", f"{rel} does not exist in the repo (to create a new file, "
                              f"emit an edit block with an EMPTY SEARCH section)")
            continue

        n, s, t = _find_block(cur, search)
        if n == 0:
            res(e, "no_match", "SEARCH text not found verbatim — copy the exact lines "
                               "(including indentation) from the provided code")
        elif n > 1:
            res(e, "ambiguous", f"SEARCH matches {n} locations in {rel} — extend it with "
                                f"surrounding lines so it matches exactly once")
        else:
            new = cur[:s] + replace + cur[t:]
            if cur.endswith("\n") and not new.endswith("\n"):
                new += "\n"
            state[rel] = new
            res(e, "matched")
    return state, results


def make_diff(repo_path, state: dict[str, str]) -> str:
    """state {rel: new_content} -> git-applyable unified diff (handles new files)."""
    repo_path = Path(repo_path)
    parts = []
    for f, new in state.items():
        p = repo_path / f
        if p.is_file():
            orig = p.read_text(encoding="utf-8", errors="ignore")
            if orig == new:
                continue
            diff = difflib.unified_diff(orig.splitlines(keepends=True), new.splitlines(keepends=True),
                                        fromfile=f"a/{f}", tofile=f"b/{f}")
            body = "".join(diff)
            if body:
                parts.append(f"diff --git a/{f} b/{f}\n{body}")
        else:
            diff = difflib.unified_diff([], new.splitlines(keepends=True),
                                        fromfile="/dev/null", tofile=f"b/{f}")
            body = "".join(diff)
            if body:
                parts.append(f"diff --git a/{f} b/{f}\nnew file mode 100644\n{body}")
    return "".join(parts)


def failure_feedback(results: list[dict]) -> str:
    """Human-readable feedback for the retry round, listing only failed edits."""
    lines = []
    for r in results:
        if r["status"] in ("matched", "created"):
            continue
        head = r["search"].split("\n", 1)[0][:120]
        lines.append(f"- FILE {r['file']}: {r['status'].upper()} — {r['detail']} "
                     f"(your SEARCH began: {head!r})")
    return "\n".join(lines)
