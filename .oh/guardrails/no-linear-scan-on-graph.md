---
id: no-linear-scan-on-graph
outcome: context-assembly
severity: hard
statement: Never .iter().find() on graph nodes or edges. Build a HashMap once, look up in O(1). Every linear scan is a performance bug waiting to compound.
---

## Rationale

Graph nodes and edges are the largest in-memory collections. Any O(N) lookup in a loop becomes O(N²). With 8K+ nodes, a search returning 60 results does 480K string comparisons instead of 60 HashMap lookups.

## What this means

- `graph_state.nodes.iter().find(|n| n.stable_id() == id)` → build `HashMap<String, &Node>` once
- `graph.edges.iter().filter(|e| e.from.file == f)` → index edges by file
- Any `.contains()` on `Vec<Node>` or `Vec<Edge>` → use HashSet

## Detection

```bash
grep -rn '\.iter()\.find(' src/ --include='*.rs' | grep -v '#\[test\]' | grep -v '_test'
```

## Evidence

Performance audit 2026-03-15: found 7 instances in service.rs alone, plus helpers.rs, handlers.rs, enrichment.rs, rust.rs, go.rs. Each was a hot path hit on every search or enrichment call.
