---
pr: 878
outcome: context-assembly
---

# RNA friction log — /ship 878

| When | Tool wanted | Fallback used | Severity | Why |
|------|-------------|---------------|----------|-----|
| Pre-flight / Step 1 | `mcp__rna-mcp__search` (metis/guardrail lookup, graph impact) | RNA CLI `search --repo .` / `graph --mode neighbors` | failed | MCP tools not exposed in the ship agent's tool set ("No such tool available"); CLI worked for symbols and neighbors |
| Step 1 | RNA `search` (artifact_types=guardrail/metis) | `ls .oh/guardrails .oh/metis` + `sed` | skipped | CLI search ranked markdown sections from the session file above `.oh/` artifacts; listed the directory to pick the relevant guardrails directly |
| Step 1 | RNA `search` for metadata persistence/render sites | `grep -n metadata_json / generic_metadata_json` | skipped | Needed every consumer of `Node.metadata` across store/server layers; cross-cutting string lookup was faster than several symbol searches |
| Step 1 | RNA node lookup for `import_calls_pass` gate body | `sed -n 170,215p src/extract/import_calls.rs` | skipped | Needed the exact gate lines; `search` gave the symbol range, then read the slice with sed |
