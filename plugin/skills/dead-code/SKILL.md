---
name: dead-code
description: Detect potentially dead code using RNA graph data. Finds public functions with zero non-test callers. No code changes needed — queries existing graph edges.
---

# /dead-code

Find public functions that nothing calls (excluding tests). Uses RNA's graph to identify candidates — not a compiler, so results are heuristic. False positives exist (framework callbacks, trait impls, FFI exports, CLI dispatch targets).

## Arguments

`$ARGUMENTS` should be: `[scope]`

- No arguments: scan the whole repo
- File path: `src/service.rs` — scan one file
- Directory: `src/extract/` — scan a subtree
- Subsystem: `subsystem:code` — scan an RNA-detected subsystem

Examples:
- `/dead-code`
- `/dead-code src/embed.rs`
- `/dead-code src/graph/`

## Procedure

### Step 0: Verify capability readiness (precondition)

This skill requires complete enough LSP call/reference coverage. Without those edges, every function can look dead and every result is a false positive. Verify readiness through RNA's MCP-visible output before doing anything else; do **not** rely on log scraping.

**Self-check (run this first):**

```text
search(query="capability", verbose=true, limit=1)
```

`verbose=true` includes the `Capability readiness` section for any query; the query just gives the search tool a harmless term to return alongside the readiness footer.

Read the `Capability readiness` section:

- `extracted graph / exact search` must be `ready` for basic symbol listing.
- `LSP call/reference coverage` must be `ready` with a non-zero edge count.
- `global dead-code prerequisites` must be `ready` (this is the workflow gate that aggregates the LSP coverage requirement for dead-code analysis).

If `global dead-code prerequisites` is `running`, `partial/degraded`, `failed`, `unavailable`, or `stale`, **abort and report the precondition failure** rather than emitting candidates. Common causes:

- The repo's language server is not on `PATH` (e.g., `pyright-langserver`, `typescript-language-server`, `gopls`, `rust-analyzer`).
- Enrichment is still running or has not started.
- Enrichment completed with zero call/reference edges.
- Enrichment computed edges but persistence failed, so data may not survive restart.

Tell the user the dead-code analysis cannot run reliably until RNA reports `global dead-code prerequisites: ready`. Do not fall back to a structural-edges-only scan.

### Step 1: Gather functions

List all functions in scope. Use non-compact mode to see `In:` and `Out:` edge counts separately.

```text
search(kind="function", file="<scope>", limit=50)
```

If no `file` scope, omit it to search the whole repo. Increase `limit` if the scope is large.

> **Note:** Compact mode (`compact=true`) saves tokens but merges in/out into a single `edges:` count.
> Use non-compact for the initial scan so you can see `In: N edge(s)` separately.

From the results, note each function's:
- **ID** (the stable node ID)
- **Name** (from the signature)
- **In-edge count** (`In: N edge(s)`)
- **Test flag** (`Test: yes` means this IS a test — skip it as a candidate)
- **Decorators** (framework annotations suggest the function is an entry point)

### Step 2: Filter obvious non-candidates

Remove from consideration:
- **Test functions** — `Test: yes` or name starts with `test_`
- **Entry points** — `main`, `run`, `setup`, `__init__`, `__main__`
- **Framework callbacks** — functions with decorators like `#[tokio::main]`, `@app.route`, `@pytest.fixture`, `#[test]`, `@click.command`, `#[handler]`, `#[endpoint]`, `@celery.task`
- **Trait/interface implementations** — if the function is inside an `impl Trait for` block or implements an interface method
- **Method overrides on library base classes** — if a function has
  `metadata["framework_hook"]` set, it was identified by an `.oh/extractors/`
  config as a library hook method. Skip it. If the metadata is NOT set but the
  method looks like it is called by an external framework (SQLAlchemy
  `TypeDecorator`, OTEL `SpanProcessor`, Django `Model`, Pydantic, etc.), it
  may need an `.oh/extractors/` config with a `[[hooks]]` section.

  Hook matching is **name-based on the enclosing class**, not inheritance-based:
  the matcher checks only that the function's immediate `parent_scope` contains
  `class_contains` and that the function's own name is listed in `method_names`.
  It does not traverse inherited base classes.

  Concretely: a `[[hooks]]` entry with `class_contains = "TypeDecorator"` only
  fires on methods whose enclosing class name contains the substring
  `TypeDecorator` (e.g., `class JsonTypeDecorator(TypeDecorator):`). It does
  **not** fire on `class Money(TypeDecorator):` because `Money` lacks the
  substring. To cover concrete subclasses with arbitrary names, add a separate
  `[[hooks]]` section per known class, or rename the class to include the
  framework name when feasible.

  Example configs that work with the current matcher (assume the user's class
  name contains the framework substring — add per-class entries otherwise):

  - SQLAlchemy: `class_contains = "TypeDecorator"`, `method_names = ["process_bind_param", "process_result_value", "column_expression"]`
  - OTEL: `class_contains = "SpanProcessor"`, `method_names = ["on_start", "on_end", "shutdown", "force_flush"]`
  - Django: `class_contains = "Model"`, `method_names = ["save", "delete", "clean", "get_queryset"]`
  - Pydantic: `class_contains = "BaseModel"`, `method_names = ["model_post_init"]`
- **Nested/local functions** — if the result shows `Parent: <outer-name> (function)`,
  this is a function defined inside another function/closure. Skip it for dead-code
  analysis — it is a local helper, not a top-level candidate. The outer function owns it.
- **CLI script functions** — functions in `scripts/` directories are typically
  called from `if __name__ == "__main__"` blocks or CLI tools, not imported
- **Functions with high in-edge count** — if `In: 5+ edge(s)`, it's clearly used; skip

Focus on functions where `In: 0-2 edge(s)` — these are the candidates worth checking.

> **Edge count gotcha:** `In: 1` usually means zero callers — the single incoming edge is the
> `Defines` relationship from the parent module/struct. A function with `In: 1` and only a `Defines`
> edge has no callers at all. True "has one caller" shows as `In: 2` (one `Defines` + one `Calls`).

### Step 3: Check each candidate's callers

For each remaining candidate, query incoming neighbors:

```text
search(node="<function-id>", mode="neighbors", direction="incoming", limit=10)
```

Examine each caller:
- If the caller has `Test: yes` → it's a test reference, **ignore it**
- If the caller's file path contains `/test`, `/tests/`, `_test.`, `.test.`, `spec/` → it's a test reference, **ignore it**
- If the caller is in the same file and is itself a dead candidate → weak signal, still counts but note it

After filtering test callers: if **zero non-test callers remain**, the function is a dead code candidate.

### Step 4: Chase dead chains

When a candidate's only non-test caller is itself a low-in-edge function, check that caller too.
Dead code often forms chains: `A` → `B` → `C` where `A` is the only caller of `B`, and nothing calls `A`.

```text
search(node="<caller-id>", mode="neighbors", direction="incoming", limit=10)
```

If the entire chain has zero external callers, the whole chain is dead — report the root function
and note the chain members.

### Step 5: Batch efficiency

If there are many candidates (>10), use the `nodes` parameter to batch-retrieve:

```text
search(nodes=["<id1>", "<id2>", "<id3>"], compact=true)
```

This gives updated edge counts. Focus detailed neighbor queries (Step 3) on the ones with genuinely low in-edge counts.
### Step 6: Report

Present results as a table:

| Function | File | Non-test callers | Confidence | Notes |
|----------|------|-----------------|------------|-------|
| `unused_helper` | src/utils.rs:42 | 0 | High | No callers at all |
| `old_format` | src/format.rs:88 | 0 | Medium | Has test callers only |
| `dispatch_cmd` | src/cli.rs:15 | 0 | Low | Likely CLI dispatch target |

**Confidence levels:**
- **High** — zero total incoming edges, no decorators, not in a trait impl
- **Medium** — has callers but all are tests, or low edge count with ambiguous decorators
- **Low** — zero callers but has characteristics of a framework entry point, trait impl, or exported API

## Limitations

- **LSP enrichment required** — `Calls` and `ReferencedBy` edges come from LSP. If the repo hasn't been LSP-enriched, the graph only has structural edges (defines, contains), and every function will look "dead." Check: if no function has `Calls` edges, warn the user that LSP enrichment hasn't run.
- **Dynamic dispatch** — trait objects, function pointers, and reflection-based calls won't have graph edges.
- **Macros** — macro-generated call sites may not be captured by tree-sitter or LSP.
- **Re-exports** — a function re-exported from a library crate may have zero in-repo callers but be the public API.
- **Cross-root** — callers in a different workspace root won't show unless both roots are indexed.
