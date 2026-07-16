---
session: "756-ship"
artifact_type: ship-session
updated: "2026-07-15"
---

## Ship Pipeline — PR #756

**Started:** 2026-07-15

### Implementation

- Extended the existing registered Markdown extractor with an offset-aware body AST pass.
- Preserved legacy heading sections while adding canonical source selectors.
- Final focused Markdown suite: 76 passed.

### Step 1: RNA-Grounded Review

**Verdict:** ADJUST; five concerns identified and fixed, including full-graph
anchor resolution, duplicate-ID invalidation, inclusive line ends, and leaf paths.

### Step 2: Independent Code Review

**Verdict:** REQUEST CHANGES; all five findings fixed. Prompt/exercise/caption
nodes, reference links, diagnostic messages, selector path validation, and merged
contract fixtures now have regression coverage.

### Regression and Delivery

- Final Markdown suite: 76 passed.
- Full CI test job passed on the implementation commit.
- Local clippy with `-D warnings` passed after the lint fix.
- Release candidate corpus scan exposed canonical selectors and diagnostics.
- Standard MCP smoke passed; feature-specific stdio MCP search delivered a
  persisted `::body::ast:heading[0]` node.
