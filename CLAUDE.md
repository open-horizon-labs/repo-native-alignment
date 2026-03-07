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

## Conventions

- Session context lives in `.oh/`
- Business artifacts: `.oh/outcomes/`, `.oh/signals/`, `.oh/guardrails/`, `.oh/metis/`
- Structured markdown with YAML frontmatter for machine-readable fields
- Git is the versioning and history layer — no custom temporal tracking needed

## Key References

- `.oh/repo-native-alignment.md` — session file with aim, exploration, architecture decisions
- Open Horizons endeavor: `ca4aa2e0` (Aim: repo-native alignment)
- Related: CodeState initiative `01b77e81` (converged into this)
