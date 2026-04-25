---
id: sweep-bug-class-siblings-even-with-null-findings
outcome: context-assembly
title: 'Sweep Bug-Class Siblings Even When You Expect Null Findings'
---

## Pattern

When fixing a bug class (not a one-off), enumerate the sibling components that might share the class and run the same diagnostic on each — even if you're confident they're fine. Null findings still pay rent: they pin the framing as verified, not assumed, and the regression tests catch future drift.

## The trigger that produced this

The proto extractor (#647) had a hand-rolled line scanner with brace counting, panicking on `message Empty {}` and producing wrong output on oneof / nested message / brace-in-comment. The fix (#648) was to port to tree-sitter-proto. That's a real bug class: "schema extractor uses hand-rolled parsing instead of an AST library."

The siblings: `sql.rs` and `openapi.rs`. My initial framing — "they're out of scope, they probably aren't fragile" — was lazy. It conflated "I read the imports and they use real parsers" with "I have evidence they handle the same edge cases as the proto suite."

The sweep (#649) added 18 regression tests across both. **Production code: zero changes.** Both extractors handled every iceberg case correctly: empty inputs, single-line declarations, comment-only files, self-references, multi-statement, path-level `parameters` blocks (could easily have become phantom endpoints), local `$ref` resolution, invalid input gracefully erroring.

## Why null findings still pay rent

1. **The sweep is a forcing function for the framing.** "Do siblings share this bug class?" is answered by *running* the diagnostic, not by reading the imports. I was right that sql/openapi use real parsers, but the question was about edge-case behavior, not parser identity. The sweep tested the actual question.

2. **Pinned behavior survives parser upgrades.** `sqlparser-rs` and `serde_yaml` semantics shift between versions. The 18 new tests now fail loudly if any of those edges drift. Without them, drift would surface as a downstream consumer bug ("why isn't this `$ref` showing up?") with the parser upgrade as the suspect five files removed.

3. **A few edges *would* be easy to break.** The path-level `parameters` block in OpenAPI: today's code correctly excludes it from the HTTP-method filter. A naive refactor that iterated all path keys would produce phantom `PARAMETERS /users` endpoints. The test now catches that. Same story for self-FK targets and multi-statement files in SQL.

4. **The framing was wrong on the *why*** ("they're not line scanners") **but right on the *outcome*** ("nothing to fix"). Without the sweep, I'd have mis-explained the framing in any future conversation about schema-extractor robustness — saying "we checked them" when we hadn't.

## When to apply

- After fixing a bug that has at least one obvious structural sibling.
- When the fix involved replacing a hand-rolled approach with a library (the bug class is "we still hand-roll X").
- When the sweep is bounded (3–5 sibling files), so the cost is small.

## When not to apply

- When the bug class is genuinely one-off (a typo, a single off-by-one, a transient race).
- When "siblings" means a sprawling set with no clear membership criteria. Pick a smaller bug class first.
- When the sweep would require production-code surgery to stand up the tests. Then you're not sweeping, you're refactoring.

## Concrete shape (cheap version)

For each sibling: write the same regression suite the original fix needed. No `#[ignore]`, no `#[should_panic]`, all positive assertions. If a test fails, you've found another instance of the bug class. If they all pass, you've documented robustness with executable evidence — and the next maintainer doesn't have to re-derive the conclusion.

## References

- Proto extractor port: PR #648 (closes #647)
- Schema-extractor sweep: PR #649
- Tests: `src/extract/sql.rs::tests::regression_*` (11), `src/extract/openapi.rs::tests::regression_*` (7)
