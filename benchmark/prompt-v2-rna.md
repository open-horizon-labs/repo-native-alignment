# Task: Code Impact Analysis for unified-hifi-control

The repo is at /Users/muness1/src/hiphi-repos/unified-hifi-control. The RNA MCP server is indexed for this repo.

Answer these 5 questions using RNA MCP tools (search with mode parameter for graph traversal).

## Q1: Transitive dependents

Use `search(query: "parse", kind: "function", file: "events.rs", mode: "impact", hops: 3)` to find all functions that would be affected if `PrefixedZoneId::parse()` changed its validation logic. List the full transitive caller chain.

## Q2: Shared dependencies

Use `search(query: "OpenHomeAdapter", mode: "reachable")` and `search(query: "LmsAdapter", mode: "reachable")` to find which functions both adapters depend on. List the intersection.

## Q3: Dead code detection

Use `search(kind: "function", file: "aggregator.rs")` to list public functions, then `search(node: "<id>", mode: "impact")` on each to check if any have zero callers.

## Q4: Architectural coupling

Use `search(file: "bus", mode: "impact")` to find all code outside src/bus/ that depends on the bus event system. Categorize by type.

## Q5: Test coverage gap

Use `search(kind: "function", file: "openhome.rs")` to list public functions, then `search(node: "<id>", mode: "tests_for")` on each to identify untested functions.

For each answer, show the RNA tool calls you made and the results. Completeness matters more than speed.
