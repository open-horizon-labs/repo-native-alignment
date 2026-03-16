# Task: Developer Questions for unified-hifi-control

The repo is at /Users/muness1/src/hiphi-repos/unified-hifi-control. A Rust hi-fi control bridge with adapters for Roon, LMS, OpenHome, UPnP, and HQPlayer.

Use RNA MCP tools (search, repo_map) for code exploration. The RNA MCP server is indexed for this repo with LSP call edges.

Answer these 5 questions as if you're a developer about to make changes. Be specific — name files, functions, line numbers.

## Q1: "I need to add a volume step size override per zone. What code path do I need to understand?"

Use `search("volume", mode: "neighbors", direction: "both")` to trace the volume change flow. Then `search("VolumeControl", kind: "struct", mode: "impact")` to find everything that depends on VolumeControl. Name every function and file in the chain.

## Q2: "I'm adding a new adapter. What's the minimal set of things I need to implement?"

Use `search("Startable", kind: "trait", mode: "impact")` and `search("AdapterLogic", kind: "trait", mode: "impact")` to find all implementations. Use `repo_map()` for orientation.

## Q3: "The OpenHome adapter's SOAP parsing is buggy. What tests cover it, and what's untested?"

Use `search(kind: "function", file: "openhome.rs")` to list functions, then `search(node: "<id>", mode: "tests_for")` on each to identify test coverage gaps.

## Q4: "I want to refactor the Zone struct. What breaks?"

Use `search("Zone", kind: "struct", file: "events.rs", mode: "impact", hops: 2)` to find all code that depends on Zone.

## Q5: "Which adapter has the most complex control flow? I need to assess tech debt."

Use `search(sort_by: "complexity", file: "adapters", min_complexity: 10)` to find the highest-complexity functions across all adapters.

Show every tool call and results. Be specific — name files, functions, line numbers.
