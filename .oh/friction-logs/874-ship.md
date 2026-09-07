---
id: 874-ship
ship_pr: 874
issue: 873
outcome: context-assembly
severity: friction
---

# Friction log — PR #874 ship

| Step | Tool used | RNA alternative | Reason | Severity |
|---|---|---|---|---|
| pre-flight | `repo-native-alignment search ""` gate command | n/a | Gate snippet greps for "N symbols" but CLI prints "Loaded N symbols from cache." on stderr before the WARN lines; COUNT came back empty although index was loaded. Gate script pattern is fragile. | failed |
| pre-flight | `search "Pass1SymbolIndex"` on worktree | n/a | Returned only markdown hits because index was stale from pre-implementation scan; needed manual `scan --repo .` | fallback |
| step 1 | (none available) | `mcp__rna-mcp__search` / `mcp__rna-server__search` for metis+guardrail lookup | Neither MCP tool prefix exists in this agent session; fell back to CLI `search --include-artifacts --artifact-types` and `ls .oh/metis` | failed |
| step 1 | `repo-native-alignment graph --node "...:$n:function"` | n/a | zsh parsed `$n:function` as a parameter modifier and upper-cased the name; needed `${n}` braces. Tool fine, usage error. | n/a |
| step 1 | grep -n on passes.rs/mod.rs for `matching_owned\|refs_by_file\|find_enclosing_symbol` | `search --node ... --mode neighbors` | Needed proof of *absence* of identifiers across two files (AC1/AC4); graph queries answer presence, not absence | skipped |
| step 1 | grep -rhn `metadata.insert("` across extractors | search | Needed to enumerate metadata keys to judge whether body-free clones still carry large text; RNA search does not match on body text by design | skipped |
| step 1 | `ls .oh/metis | grep -i lsp` + sed | `search --include-artifacts --artifact-types metis,guardrail` | Typed artifact search for "LSP pass 1 work item node clone memory" returned only code symbols and one session markdown section; zero metis/guardrail hits although 6 LSP metis files exist | fallback |
| step 1 | grep in work_items.rs for body/hash | `search "LspWorkItemSeed" --repo .` + neighbors | Needed to confirm ledger identity does not hash body text (absence proof) | skipped |
| step 3 | sed -n on passes.rs 190-320 | `search --node "...:build:function" --include-body` | Two methods named `build` (Pass1SymbolIndex::build, EndpointLookupIndex::build) share the node ID `src/extract/lsp/passes.rs:build:function`; RNA returned only one. Method IDs are not parent-qualified. | fallback |
| step 3 | grep -A6 "fn stable_id" src/graph/types.rs | `search --node "...:stable_id:function" --include-body` | Same collision: Node::stable_id / Edge::stable_id / NodeId::to_stable_id | fallback |
| step 3 | grep -n "fn stable_id" src/graph/mod.rs | `search "stable_id" --kind function` | Search listed `Edge::stable_id` (src/graph/mod.rs:498) but not `Node::stable_id` (src/graph/mod.rs:472): same-file same-name methods collapse to one node ID `src/graph/mod.rs:stable_id:function` | fallback |
| step 7a | `ps -o rss` sampler + pylance dump of `edges.lance` | `stats` / `graph` CLI | No RNA command reports its own peak memory or dumps the edge table for cross-binary diffing; `stats` gives totals only. Needed for the perf gate and AC7 edge-identity proof. | skipped |
| step 7a | RUSTUP_TOOLCHAIN=stable | n/a | rustup proxy for rust-analyzer errors under the repo's pinned 1.97.0 toolchain (no rust-analyzer component); RNA surfaced it only as "Missing Content-Length header". Not RNA friction per se, but the error message hides the cause. | n/a |
