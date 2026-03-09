# Session: claude-memory-108

## Aim
Index Claude Code auto memory (`~/.claude/projects/<project>/memory/`) as a scanner root so `oh_search_context` finds operational knowledge alongside business context.

## Solution Space
Add the memory dir as an optional root in the scanner. Path: `~/.claude/projects/{repo_root_slashes_as_dashes}/memory/`. Documented by Anthropic.

## Execute Status
- **In progress:** Need to find where roots are constructed in `roots.rs` (not `resolved_roots` which just filters)
- `resolved_roots()` at `src/roots.rs:210` iterates `self.roots` and filters by `.exists()`
- Need to find where `self.roots` is populated — likely a `new()` or `from_repo()` constructor
- Then add the memory dir as an additional root with type "claude-memory" or similar

## Key facts
- Memory path: `~/.claude/projects/-Users-muness-src-open-horizon-labs-repo-native-alignment/memory/`
- Contains: `MEMORY.md` + optional topic files (`debugging.md`, `patterns.md`, etc.)
- MEMORY.md first 200 lines loaded by Claude Code every session
- Machine-local, not git-tracked
- The markdown extractor already handles `.md` files — just needs to be scanned

## Related
- v0.1.4 shipped with graph quality fixes
- #81 still open (agents prefer Grep over RNA)
