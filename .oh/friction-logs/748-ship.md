---
date: "2026-07-15"
pipeline_issue: "/ship PR #748"
pr: 748
phase: ship
---

# PR #748 ship friction

| Phase/Step | Tool | What happened | Workaround | Severity |
|---|---|---|---|---|
| Exploration | RNA worktree search | The new #736 worktree had no persisted Lance cache; a new worktree should be immediately queryable from its parent index. | Used the adjacent indexed #734 worktree for unchanged policy-checker structure and recorded current graph evidence from the #736 checkout. | friction |
| Exploration | RNA manifest search | RNA found the `lancedb` package node but did not return the raw dependency declaration or feature table; package search should render version, source, and features directly. | Used authoritative `git show` only after the RNA query failed to answer the manifest question. | friction |
| Final review | RNA worktree search | RNA returned the older 302-line policy checker and could not find the exact-head reachability function added later in the branch. | Diagnosed the stale worktree index with broader RNA queries, then used authoritative `git show HEAD:<path>` for the bounded review finding. | friction |
| Solution space | Published-parent Cargo probes | The pinned parents cannot resolve newer DataFusion or Candle versions, so a normal update cannot reveal whether those releases remove `paste`; this needs a non-mutating dependency probe. | Used isolated temporary manifests to query the current published LanceDB, DataFusion, FastEmbed, and Candle graphs. | friction |
