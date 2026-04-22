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

### Step 0: Check LSP enrichment health

Before analyzing, verify the graph has call edges from LSP enrichment:

```text
search(query="Calls", kind="function", file="<scope>", limit=5)
```

Look for functions with `Calls` edges that have `source: Lsp`. If **no** function
shows LSP-sourced edges, the LSP enrichment didn't run successfully for this repo.

> ⚠️ **Warning:** Without LSP enrichment, the graph only has structural edges
> (Defines, Contains) and import-resolution edges. Method calls (`obj.method()`),
> property access, JSX component usage (`<Component />`), and callback patterns
> are invisible. Results will have a **high false-positive rate** for Python and
> TypeScript repos. Rust and Go repos are less affected (static dispatch).
>
> To fix: run `repo-native-alignment scan --repo <path> --full` and check the
> LSP enrichment logs for errors.

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
  method is on a class inheriting from an external library (SQLAlchemy
  `TypeDecorator`, OTEL `SpanProcessor`, Django `Model`, etc.), it may need an
  `.oh/extractors/` config with a `[[hooks]]` section. Common framework hooks:
  - SQLAlchemy: `process_bind_param`, `process_result_value`, `column_expression`
  - OTEL: `on_start`, `on_end`, `shutdown`, `force_flush`
  - Django: `save`, `delete`, `clean`, `get_queryset`
  - Pydantic: `model_post_init`, validators
  - Python ABCs: any method matching a name defined on the abstract parent
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
