# Friction log — PR #738

| When | Friction | Impact | Response |
|---|---|---|---|
| Exploration | A broad RNA `neighbors` query returned about 21,000 results despite a small requested limit. | The response was too large to use and consumed avoidable context. | Re-scoped subsequent RNA queries to exact symbols and batch retrieval. |
| Verification | The first optimized test build filled the shared worktree volume. | The focused test could not complete until space was reclaimed. | Removed only the completed #721 worktree target and kept cargo execution serialized. |
| Formatting | Plain `rustfmt` recursively formatted an unchanged child module. | It introduced unrelated diff noise. | Reverted the unrelated formatting and used `skip_children=true` for targeted formatting. |
| Delivery verification | RNA returned no results for the top-level JavaScript MCP smoke script. | The official real-client assertions could not be inspected through the dogfood index. | Logged the miss, used `git show` once, and added the missing `list_roots` queue assertion to the official smoke script. |
