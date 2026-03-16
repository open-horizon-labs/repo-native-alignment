# Task: Code Impact Analysis for unified-hifi-control

The repo is at /Users/muness1/src/hiphi-repos/unified-hifi-control.

Answer these 5 questions. For each, provide specific function names, file paths, and line numbers.

## Q1: Transitive dependents (grep can't do this)

List ALL functions that would be affected if `PrefixedZoneId::parse()` changed its validation logic. Include transitive callers — not just direct callers, but functions that call functions that call parse(). Go at least 3 levels deep.

## Q2: Shared dependencies (requires cross-referencing)

Which functions are called by BOTH the OpenHome adapter AND the LMS adapter? List the shared code paths — functions that both adapters depend on but that aren't in the shared traits.

## Q3: Dead code detection (requires reachability analysis)

Are there any public functions in `src/aggregator.rs` that are never called from anywhere in the codebase? List them.

## Q4: Architectural coupling (requires structural analysis)

If I wanted to extract the bus event system (everything in `src/bus/`) into a separate crate, what functions outside `src/bus/` would break? Categorize by: direct imports, type usage in signatures, trait bounds.

## Q5: Test coverage gap (requires call graph)

Which public functions in `src/adapters/openhome.rs` are NOT called by any test function (directly or transitively)? List the untested public API surface.

For each answer, show your work — what queries you ran and how you arrived at the answer. Completeness matters more than speed.
