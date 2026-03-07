# CLAUDE.md

## Project

repo-native-alignment: an agentic harness for business outcome alignment in code repos.

## Language & Stack

- Rust (primary)
- LanceDB (storage: columnar + vectors + full-text)
- git2 (change detection)
- tree-sitter (code parsing)
- pulldown-cmark (markdown parsing)
- MCP protocol (agent interface)

## MCP Tools (use these, not grep/Read)

When searching for code symbols, functions, types, or imports: ALWAYS use `search_symbols` MCP tool, NEVER use Grep or Read for symbol discovery. When tracing relationships: use `graph_query`. When searching business context: use `oh_search_context`. When recording learnings: use `oh_record`.

## Conventions

- Session context lives in `.oh/`
- Business artifacts: `.oh/outcomes/`, `.oh/signals/`, `.oh/guardrails/`, `.oh/metis/`
- Structured markdown with YAML frontmatter for machine-readable fields
- Git is the versioning and history layer — no custom temporal tracking needed

## Key References

- `.oh/repo-native-alignment.md` — session file with aim, exploration, architecture decisions
- Open Horizons endeavor: `ca4aa2e0` (Aim: repo-native alignment)
- Related: CodeState initiative `01b77e81` (converged into this)
