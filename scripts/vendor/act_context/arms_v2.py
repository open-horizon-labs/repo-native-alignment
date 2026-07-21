"""exp-014b oracle arms v2 — coverage-fair representation arms + per-instance coverage metrics.

Fixes the audited confounds (SOTA_FAILURE_AUDIT.md B1/B2 + strategic-scoping coverage finding):
  - module preamble: imports / constants / module-level statements of every gold .py file are
    rendered VERBATIM in the compressed arms (they were structurally absent before, making 21/30
    frozen instances' gold fixes inexpressible there);
  - class-attribute edits: a hunk inside a class but outside any method now promotes the CLASS to a
    full-source gold unit (was: silently unrendered);
  - non-Python gold files (setup.cfg, ...) are rendered verbatim in ALL arms (were: only in full);
  - no truncation here — arms return full text; the runner sends it whole and logs real sizes;
  - coverage instrumentation: hunk_preimages() + coverage_report() verify, per arm x instance, that
    every gold hunk's pre-image text appears verbatim (rstrip-tolerant) in the rendered context, so
    the analysis can be conditioned on expressibility instead of assuming it.

The preamble is identical in oracle_tier and oracle_keep_drop, so it cannot bias the centerpiece
tier-vs-keep/drop comparison; it only removes the artificial handicap both had against oracle_full.
"""
from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "src"))

from dhcm_ng.context_arms import (  # noqa: E402
    ntok, render, gold_files_from_patch, gold_hunks_from_patch, _file_full_source, _assemble,
)
from dhcm_ng.model import Kind, Tree  # noqa: E402


# --- module preamble (everything outside top-level def/class spans) -------------
def module_preamble_segments(src: str) -> list[tuple[int, int, str]]:
    """Contiguous line segments of a .py source NOT covered by any top-level function/class
    (including decorators). Returns [(start_line, end_line, text)], 1-based inclusive."""
    try:
        mod = ast.parse(src)
    except SyntaxError:
        return [(1, src.count("\n") + 1, src)] if src.strip() else []
    lines = src.split("\n")
    covered = [False] * (len(lines) + 2)
    for stmt in mod.body:
        if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            start = min([stmt.lineno] + [d.lineno for d in stmt.decorator_list])
            end = getattr(stmt, "end_lineno", stmt.lineno)
            for i in range(start, min(end, len(lines)) + 1):
                covered[i] = True
    segs, cur = [], None
    for i in range(1, len(lines) + 1):
        if not covered[i]:
            cur = i if cur is None else cur
        elif cur is not None:
            segs.append((cur, i - 1))
            cur = None
    if cur is not None:
        segs.append((cur, len(lines)))
    out = []
    for a, b in segs:
        text = "\n".join(lines[a - 1:b])
        if text.strip():
            out.append((a, b, text))
    return out


# --- decorators: ast.get_source_segment EXCLUDES them, so node.source lacks them ----
def _decorator_map(src: str) -> dict[int, tuple[int, str]]:
    """{def/class lineno: (first_decorator_lineno, decorator_text)} for every decorated
    function/class (any nesting level)."""
    try:
        mod = ast.parse(src)
    except SyntaxError:
        return {}
    lines = src.split("\n")
    out = {}
    for node in ast.walk(mod):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) \
                and node.decorator_list:
            start = min(d.lineno for d in node.decorator_list)
            out[node.lineno] = (start, "\n".join(lines[start - 1:node.lineno - 1]))
    return out


# --- gold unit mapping v2 -----------------------------------------------------------
def gold_edited_lines(patch: str) -> dict[str, set[int]]:
    """{file: old-file line numbers the gold patch actually EDITS} — '-' lines plus, for each pure
    insertion, the nearest NON-BLANK old line as anchor (a blank anchor would sit in module scope
    between elements and fail to promote the class the insertion actually lives in). Excludes the
    +-3 context lines, which routinely spill into neighbouring elements and would over-promote
    whole classes to full rendering."""
    out: dict[str, set[int]] = {}
    cur, old_ln = None, 0
    last_nonblank = 0
    pending_insert = False
    for line in patch.split("\n"):
        if line.startswith("+++ b/"):
            cur = line[6:].strip()
        elif line.startswith("--- "):
            pass
        elif line.startswith("@@") and cur is not None:
            m = re.search(r"@@ -(\d+)", line)
            old_ln = int(m.group(1)) if m else 0
            last_nonblank, pending_insert = 0, False
        elif cur is not None and old_ln:
            tag, text = line[:1], line[1:]
            if tag == "-":
                out.setdefault(cur, set()).add(old_ln)
                if text.strip():
                    last_nonblank = old_ln
                old_ln += 1
            elif tag == "+":
                if last_nonblank:
                    out.setdefault(cur, set()).add(last_nonblank)
                else:
                    pending_insert = True  # insertion before any non-blank old line in this hunk
            elif tag == " ":
                if text.strip():
                    last_nonblank = old_ln
                    if pending_insert:
                        out.setdefault(cur, set()).add(old_ln)
                        pending_insert = False
                old_ln += 1
    return out


def gold_units_v2(tree: Tree, edited_lines: dict[str, set[int]],
                  deco_maps: dict[str, dict[int, tuple[int, str]]] | None = None) -> set[str]:
    """Map edited lines to units: functions claim their lines; remaining lines inside a class
    (attributes, header/docstring, class decorators) promote the INNERMOST covering class."""
    units = set()
    funcs = list(tree.by_kind(Kind.FUNCTION))
    classes = list(tree.by_kind(Kind.CLASS))
    deco_maps = deco_maps or {}

    def ext_start(n, gf):
        # a node's editable span starts at its first decorator line, not its def/class line
        return deco_maps.get(gf, {}).get(n.start_line, (n.start_line,))[0]

    for gfile, lns in edited_lines.items():
        gf = gfile.replace("\\", "/")
        ffns = [fn for fn in funcs if fn.path.replace("\\", "/").endswith(gf)]
        fcls = [c for c in classes if c.path.replace("\\", "/").endswith(gf)]
        for line in sorted(lns):
            hit = [fn for fn in ffns if ext_start(fn, gf) <= line <= fn.end_line]
            if hit:
                units.update(fn.id for fn in hit)
                continue
            covering = [c for c in fcls if ext_start(c, gf) <= line <= c.end_line]
            if covering:
                units.add(min(covering, key=lambda c: c.end_line - c.start_line).id)
    return units


def _descendants(tree: Tree, nid: str) -> set[str]:
    out, stack = set(), [nid]
    while stack:
        cur = stack.pop()
        for c in tree.nodes[cur].children:
            out.add(c)
            stack.append(c)
    return out


def _elements_in_files(tree: Tree, files: list[str]) -> list:
    fs = [f.replace("\\", "/") for f in files]
    return [n for n in tree.nodes.values()
            if n.kind in (Kind.FUNCTION, Kind.CLASS)
            and any(n.path.replace("\\", "/").endswith(f) for f in fs)]


def build_oracle_arms_v2(tree: Tree, repo_path, gold_files: list[str],
                         edited_lines: dict[str, set[int]]) -> dict:
    """oracle_full / oracle_tier / oracle_keep_drop over the SAME gold files, coverage-fair.
    `edited_lines` from gold_edited_lines(patch)."""
    repo_path = Path(repo_path)
    deco_maps = {f.replace("\\", "/"): _decorator_map(_file_full_source(repo_path, f))
                 for f in gold_files if f.endswith(".py")}
    gold_units = gold_units_v2(tree, edited_lines, deco_maps)
    gold_nodes = [tree.nodes[i] for i in gold_units]
    gold_ids = set(gold_units)
    # everything inside a full-rendered gold node is already shown
    shown_ids = set(gold_ids)
    for i in gold_ids:
        shown_ids |= _descendants(tree, i)
    encl_ids = {n.parent for n in gold_nodes
                if n.parent and tree.nodes[n.parent].kind == Kind.CLASS
                and n.parent not in shown_ids}

    py_files = [f for f in gold_files if f.endswith(".py")]
    other_files = [f for f in gold_files if not f.endswith(".py")]

    preamble_items = []
    for f in py_files:
        for a, b, text in module_preamble_segments(_file_full_source(repo_path, f)):
            preamble_items.append((f"{f} (module-level, lines {a}-{b})", text))
    verbatim_items = [(f, _file_full_source(repo_path, f)) for f in other_files]

    of = _assemble([(f, _file_full_source(repo_path, f)) for f in gold_files], None)

    els = _elements_in_files(tree, py_files)

    def _verbatim_slice(n):
        """Byte-faithful file slice for a gold unit (decorators included). node.source comes from
        ast.get_source_segment, which slices from col_offset — a method's FIRST line loses its
        leading indentation there, so SEARCH blocks copied from it could never match the file."""
        src = _file_full_source(repo_path, n.path)
        lines = src.split("\n")
        start = n.start_line
        rel = n.path.replace("\\", "/")
        for gf, dm in deco_maps.items():
            if rel.endswith(gf) and n.start_line in dm:
                start = dm[n.start_line][0]
                break
        return "\n".join(lines[start - 1:n.end_line])

    gold_full = [(f"{n.path}:{n.name}", _verbatim_slice(n)) for n in gold_nodes]

    tier_items = list(verbatim_items) + list(preamble_items) + list(gold_full)
    tier_items += [(f"{tree.nodes[i].path}:{tree.nodes[i].name}", render(tree.nodes[i], "uml"))
                   for i in encl_ids]
    tier_items += [(f"{n.path}:{n.name}", render(n, "signature")) for n in els
                   if n.id not in shown_ids and n.id not in encl_ids and n.parent not in encl_ids]
    ot = _assemble(tier_items, None)

    kd_items = list(verbatim_items) + list(preamble_items) + list(gold_full)
    kd_items += [(f"{n.path}:{n.name}", render(n, "drop")) for n in els
                 if n.id not in shown_ids and n.parent not in shown_ids]
    okd = _assemble(kd_items, None)

    return {"oracle_full": {"tokens": of[1], "text": of[0]},
            "oracle_tier": {"tokens": ot[1], "text": ot[0]},
            "oracle_keep_drop": {"tokens": okd[1], "text": okd[0]},
            "_gold_units": sorted(gold_ids)}


# --- coverage instrumentation ----------------------------------------------------
# Expressibility criterion (per EDIT RUN, not per whole hunk): a unified-diff hunk carries +-3
# context lines that routinely cross element boundaries (the gap between two methods), which
# per-element rendering can never reproduce contiguously even when every edited line is shown.
# What a SEARCH/REPLACE edit actually needs from the context is, per maximal run of -/+ lines:
#   - modification/deletion run: its contiguous '-' lines, verbatim;
#   - pure-insertion run: one adjacent non-blank context line to anchor the SEARCH on.
_HUNK_HEAD = re.compile(r"^@@ -\d+(?:,\d+)? \+\d+(?:,\d+)? @@")


def _hunk_requirements(lines: list[tuple[str, str]]) -> list[str]:
    """lines: [(tag, text)] with tag in ' -+'. Returns the verbatim strings the context must
    contain for every edit run in this hunk to be expressible as a SEARCH block."""
    reqs = []
    i = 0
    while i < len(lines):
        if lines[i][0] == " ":
            i += 1
            continue
        j = i
        while j < len(lines) and lines[j][0] != " ":
            j += 1
        dels = [t for tag, t in lines[i:j] if tag == "-"]
        while dels and not dels[0].strip():     # boundary blank lines: the model can match the
            dels.pop(0)                          # non-blank core and still effect the edit
        while dels and not dels[-1].strip():
            dels.pop()
        if dels:
            reqs.append("\n".join(dels))
        else:  # pure insertion: anchor on nearest non-blank context line (prev, else next)
            anchor = None
            for k in range(i - 1, -1, -1):
                if lines[k][0] == " " and lines[k][1].strip():
                    anchor = lines[k][1]
                    break
            if anchor is None:
                for k in range(j, len(lines)):
                    if lines[k][0] == " " and lines[k][1].strip():
                        anchor = lines[k][1]
                        break
            if anchor is not None:
                reqs.append(anchor)
            # no anchor at all (insertion into empty file): nothing checkable
        i = j
    return reqs


def hunk_preimages(patch: str) -> list[dict]:
    """Per gold hunk: {file, new_file, requirements} — the verbatim blocks (per edit run) that an
    arm's context must contain for the gold edit to be expressible. See criterion note above."""
    out: list[dict] = []
    cur_file, new_file, hunk = None, False, None

    def flush():
        if hunk is not None:
            out.append({"file": cur_file, "new_file": new_file,
                        "requirements": [] if new_file else _hunk_requirements(hunk)})

    for line in patch.split("\n") + ["diff --git sentinel sentinel"]:
        if line.startswith("diff --git"):
            flush()
            cur_file, new_file, hunk = None, False, None
        elif line.startswith("--- "):
            new_file = line[4:].strip() == "/dev/null"
        elif line.startswith("+++ b/"):
            cur_file = line[6:].strip()
        elif _HUNK_HEAD.match(line) and cur_file is not None:
            flush()
            hunk = []
        elif hunk is not None and line[:1] in (" ", "-", "+"):
            hunk.append((line[:1], line[1:]))
    return out


def _contains_tolerant(haystack: str, needle: str) -> bool:
    if not needle.strip():
        return True
    if needle in haystack:
        return True
    h = [l.rstrip() for l in haystack.split("\n")]
    n = [l.rstrip() for l in needle.split("\n")]
    while n and n[-1] == "":
        n.pop()
    if not n:
        return True
    return any(h[i:i + len(n)] == n for i in range(len(h) - len(n) + 1))


def coverage_report(arm_text: str, preimages: list[dict]) -> dict:
    """How many gold hunks are fully expressible from this arm's context (every edit-run
    requirement present verbatim, rstrip-tolerant)."""
    hunks = covered = newf = 0
    uncovered = []
    for h in preimages:
        if h["new_file"]:
            newf += 1
            continue
        hunks += 1
        missing = [r for r in h["requirements"] if not _contains_tolerant(arm_text, r)]
        if not missing:
            covered += 1
        else:
            first = missing[0].strip().split("\n", 1)[0][:80]
            uncovered.append(f"{h['file']}: {first!r}")
    return {"hunks": hunks, "covered": covered, "new_file_hunks": newf,
            "full_coverage": covered == hunks, "uncovered": uncovered}
