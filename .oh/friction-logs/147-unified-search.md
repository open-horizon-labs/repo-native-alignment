# Friction Log: #147 Unified Search Tool
**Date:** 2026-03-12
**Pipeline/Issue:** #147 / PR #148

| Phase/Step | Tool | What happened | Workaround | Severity |
|------------|------|---------------|------------|----------|
| Phase 1 (problem statement) | search_symbols | Compound queries like "search_symbols handler" return no results | Searched each term separately | papercut |
| Phase 1 (problem statement) | search_symbols | String constants (tool descriptions, error messages) indexed as symbols, cluttering results | Mentally filtered out const results | papercut |

## Friction Summary

**Total events:** 2 (0 blockers, 0 friction, 2 papercuts)

### Patterns
- Multi-word search doesn't work as expected — agent expects phrase search, gets AND/OR confusion (1 occurrence)
- Synthetic constants noise — string literals surfacing alongside real symbols (1 occurrence)

### Recommendations
- **Update existing issue:** #118 or new — multi-word query handling in search_symbols could split on spaces and AND the terms
- **Known limitation:** synthetic constants are deliberate (#119 tracks NodeId collision); consider a `exclude_synthetic: true` default or lower ranking tier for synthetic Const nodes
