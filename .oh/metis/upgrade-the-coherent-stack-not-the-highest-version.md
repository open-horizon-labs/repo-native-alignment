---
title: Upgrade the coherent stack, not the highest version
date: 2026-07-15
outcome: context-assembly
source_issue: 734
---

A transitive advisory owned by a storage stack should be removed by moving the
coherent parent stack, not by forcing the leaf package. But "coherent" does not
mean "latest." LanceDB 0.31 / Lance 8 removed the original `lru` path while
adding target-specific RustSec vulnerabilities through `lance-testing -> pprof
-> inferno -> quick-xml 0.26`.

The safer migration was LanceDB 0.30 / Lance 7 / Arrow 58. It also removes
Tantivy and `lru`, retains Rust 1.91, and avoids the new vulnerable path. The
choice only became visible because the fail-closed RustSec gate audited the
all-target lockfile after dependency resolution rather than treating successful
compilation on the host as sufficient evidence.

For dependency migrations, evaluate the fully resolved all-feature/all-target
graph before adapting APIs. Prefer the newest coherent stack that satisfies the
actual security and compatibility constraints, then verify the repository's
storage boundary and old-cache behavior with exact CI artifacts.
