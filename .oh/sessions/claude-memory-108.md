# Session: claude-memory-108

## Aim
Index Claude Code auto memory (`~/.claude/projects/<project>/memory/`) as a scanner root so `oh_search_context` finds operational knowledge alongside business context.

## Solution Space
Add the memory dir as an optional root in the scanner. Path: `~/.claude/projects/{repo_root_slashes_as_dashes}/memory/`. Documented by Anthropic.

## Execute Status
- **Complete:** 2026-03-09
- Added `claude_memory_dir()` helper — computes `~/.claude/projects/-{path}/memory/`
- Added `WorkspaceConfig::with_claude_memory()` — builder method, adds Notes root if dir exists
- Wired into all 3 call sites in `server.rs` (background scanner, initial build, list_roots)
- 3 new tests: path format, adds-when-exists, skips-when-missing
- 22/22 roots tests pass, clean build

## Key facts
- Memory path: `~/.claude/projects/-Users-muness-src-open-horizon-labs-repo-native-alignment/memory/`
- Contains: `MEMORY.md` + optional topic files (`debugging.md`, `patterns.md`, etc.)
- MEMORY.md first 200 lines loaded by Claude Code every session
- Machine-local, not git-tracked
- The markdown extractor already handles `.md` files — just needs to be scanned

## Related
- v0.1.4 shipped with graph quality fixes
- #81 still open (agents prefer Grep over RNA)
