---
id: working-set-selection-must-preserve-causal-paths
outcome: context-assembly
title: 'A Small Lexical Slice Is Not a Working Set'
---

## Pattern

Bounding injected repository context is necessary, but lexical overlap must not
be reapplied after graph traversal. Doing so can delete the very control-flow
mechanism that makes the graph useful. A working set is a bounded causal slice,
not merely a smaller bag of names that repeat task words.

## Evidence

The issue-836 rank-3 T4 gate queried `Poly multiplication`, selected the test
node `test_Poly_mul`, traversed two hops, and retained only graph records whose
names overlapped the query. That produced a 2,610-byte prompt containing
`mul` and `mul_ground`. Both models officially resolved the case, but the true
fix was at the operator-dispatch layer (`_op_priority`). The projection had
discarded `call_highest_priority` and `binary_op_wrapper` because their names
did not repeat `Poly` or `multiplication`.

The result was a longer discovery loop. Relative to the matching anti-slop A,
T4 used 40.8% more total tokens on Sonnet and 277.4% more on Spark. Spark made
35 additional tool calls; cumulative cache-read input grew from 813,056 to
3,163,136 tokens. The injected prefix itself was only about 2.1 KiB. The cost
came from steering, tool output, and repeated conversation replay.

## Design consequence

A bounded task-context assembler should:

- prefer a confident production protocol root over a test root;
- include the selected root body;
- retain records because they lie on relevant structural paths, not because
  every node repeats title tokens;
- cap whole records and total bytes only after structural selection; and
- abstain when no sufficiently confident production root exists.

Task-derived query expansion may use explicit code syntax and language
protocols. In this case, `Poly __mul__ __rmul__` ranked the production
`polytools.__rmul__` first, and traversal from `Expr.__mul__` exposed the
dispatch wrapper. That probe was post hoc, so it is a design clue rather than
efficacy evidence; the rule must be frozen and tested on different cases.

## Reporting consequence

Always split cumulative input into ordinary/cache-write/cache-read tokens and
report tool-result volume. A tiny treatment prompt can still increase total
tokens dramatically by lengthening the trajectory that is replayed at every
turn.
