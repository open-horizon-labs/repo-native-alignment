# Friction Log: PR #672 Ship
**Date:** 2026-05-07
**Pipeline/Issue:** /ship PR #672

| Phase/Step | Tool | What happened | Workaround | Severity |
|------------|------|---------------|------------|----------|
| Pre-flight | `mcp_rna_server_search` | Flat search calls failed with `Empty nodes list. Provide at least one stable node ID.`; dummy `nodes` values were treated as batch retrieval, not query search. | Used worktree RNA CLI `repo-native-alignment search --repo .` and logged the MCP shape mismatch. | high |
| Pre-flight | `repo-native-alignment search` | Hybrid search warned no inverted index existed and fell back to vector-only; exact symbol results were stale before an explicit worktree rescan. | Ran `scan --repo . --extract-only --no-embed --no-lsp --timings` using the PR binary, then retried focused searches. | medium |
| Pre-flight | `repo-native-alignment scan --repo . --full` | Mandatory scan gate using installed binary timed out after 600s while background enrichment was active. | Used bounded extract-only/no-LSP/no-embed scan for live structural review; full/performance verification remains a separate manual gate. | medium |
