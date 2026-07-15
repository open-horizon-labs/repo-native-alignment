---
title: Repo-owned forks must carry the parent fix
date: 2026-07-15
outcome: context-assembly
source_issue: 735
---

When an advisory is selected inside a repo-owned fork, the consuming repository
should not hide the feature or force the leaf package. Move the owning parent in
the fork, verify the real platform capability there, merge it, and pin the
reviewed merge commit downstream.

For `number_prefix`, upgrading metal-candle from hf-hub 0.4 to 0.5 moved
indicatif from 0.17 to 0.18 and replaced the leaf with `unit-prefix`. In RNA's
resolved graph, that upgrade also deduplicated metal-candle onto the hf-hub 0.5
already used by fastembed, removing 17 obsolete lockfile packages rather than
adding another parallel HTTP/progress stack.

The fork verification exposed a second contract issue: its declared Rust 1.75
floor was not compatible with a fresh resolution of its existing `main`
dependencies. The prerequisite PR made Rust 1.88 explicit, verified its full
suite and real Metal inference at that floor, and remained below RNA's Rust
1.91 boundary. Dependency security work should make adjacent version contracts
more truthful, not preserve a nominal MSRV that current consumers cannot
resolve.
