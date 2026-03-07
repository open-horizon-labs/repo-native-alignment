# P5: Implementation Plan — Grounded oh_init + resolve_references

**Updated:** 2026-03-07
**Follows:** P4 Solution Space
**Outcome:** agent-alignment

## Summary

Two changes. Agent is the bridge between OH MCP and RNA MCP — no server-to-server coupling.

1. **Enhance `oh_init`** — accept optional OH graph data, produce grounded artifacts instead of templates
2. **Add `resolve_references`** — match code spans in `.oh/` markdown against tree-sitter symbol table

## Part 1: Grounded oh_init

### New Parameters

```rust
oh_init {
    outcome_name: Option<String>,     // existing
    endeavor_id: Option<String>,      // OH endeavor UUID for traceability
    endeavor_aim: Option<String>,     // aim statement → session file
    outcomes: Option<Vec<OutcomeSpec>>,
    signals: Option<Vec<SignalSpec>>,
    guardrails: Option<Vec<GuardrailSpec>>,
}

OutcomeSpec { id, title, description, file_patterns: Option<Vec<String>> }
SignalSpec { id, description, outcome_id: Option<String> }
GuardrailSpec { id, description, severity: Option<String> }
```

### Behavior

- **With OH data:** writes real artifacts with content from the OH graph
- **Without OH data:** identical to current template behavior (no regression)

### Agent Workflow

1. Agent calls OH MCP: `oh_get_endeavors` → find matching endeavor
2. Agent calls OH MCP: `oh_get_endeavor(id)` → get aim, description
3. Agent extracts outcomes, signals, guardrails from endeavor
4. Agent calls RNA MCP: `oh_init(endeavor_aim: "...", outcomes: [...], ...)`
5. RNA scaffolds `.oh/` with grounded content

### Checklist

- [ ] Define `OutcomeSpec`, `SignalSpec`, `GuardrailSpec` structs with `Deserialize + JsonSchema`
- [ ] Add optional params to `OhInit` tool struct
- [ ] Branch `oh_init_impl` on presence of OH data vs template mode
- [ ] Grounded path: iterate outcomes/signals/guardrails, write real artifacts
- [ ] Session file: include endeavor_aim as aim section
- [ ] Tests: template mode (unchanged), grounded mode, idempotent
- [ ] Update tool description

## Part 2: resolve_references Tool

### How It Works

1. Read `.oh/` markdown, extract code spans (backtick-delimited identifiers)
2. Filter noise: skip primitives (`String`, `Result`, `Option`), short names, non-identifiers
3. Match remaining against tree-sitter symbol table (exact name match, repo-local only)
4. Also check file paths and `files:` frontmatter patterns against filesystem
5. Return JSON: resolved (with file:line:kind), unresolved, file pattern coverage

### Parameters

```rust
resolve_references {
    path: Option<String>,      // specific .oh/ file, or all .oh/ markdown
    outcome_id: Option<String>, // only check references for this outcome
}
```

### Output

```json
{
  "resolved": [
    { "reference": "oh_init_impl", "file": "src/server.rs", "line": 381, "kind": "function" }
  ],
  "unresolved": [
    { "reference": "validate_outcome", "source": ".oh/outcomes/agent-alignment.md" }
  ],
  "file_patterns": {
    "agent-alignment": { "patterns": ["src/server.rs", "src/oh/*"], "matched_files": 8 }
  }
}
```

### Checklist

- [ ] New `src/references.rs` module
  - `extract_code_references(markdown) -> Vec<String>` via pulldown-cmark
  - `filter_noise(candidates) -> Vec<String>` with hardcoded primitive set
  - `resolve_against_symbols(refs, symbols) -> ResolveResult`
- [ ] Symbol table: reuse `code::extract_symbols`, build `HashMap<name, Vec<location>>`
- [ ] File resolution: check paths exist, expand globs
- [ ] Register tool in server.rs
- [ ] Tests: extraction, noise filtering, resolution, file patterns

## Part 3: Agent Workflow Updates

- [ ] Update `/teach-oh` to document the OH bridge workflow
- [ ] Skills preamble tells agents: "call OH tools first, pass data to oh_init"
- [ ] Test: run grounded oh_init on a second repo end-to-end

## Design Decisions

- **Agent bridges, not server-to-server** — MCP servers don't call each other. Agent orchestrates.
- **Exact name matching** — fuzzy introduces false positives. Stale references are actionable signal.
- **Aggressive noise filtering** — false "unresolved" worse than missing a real reference.
- **No persistent symbol table** — code changes between calls. Recompute is fast enough.

## Implementation Order

1. **Phase A:** oh_init enhancement (1 session)
2. **Phase B:** resolve_references tool (1 session)
3. **Phase C:** agent workflow + test on second repo (1 session)
