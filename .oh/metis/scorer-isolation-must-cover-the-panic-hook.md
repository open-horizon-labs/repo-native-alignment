---
id: scorer-isolation-must-cover-the-panic-hook
outcome: context-assembly
title: 'Scorer Isolation Must Cover the Panic Hook and the Delivered Path'
---

A spawned task boundary is not sufficient containment for repository-sensitive scoring. Rust invokes the process panic hook before Tokio returns a `JoinError`, so a response-safe diagnostic can coexist with a raw stderr leak unless the hook path is explicitly redacted.

The regression also has to cross the real delivery seam. Separate tests for “a live worktree maps” and “a scorer panic degrades” can both pass while the mapped handler path never exercises the failure. The useful oracle maps a real worktree, fails the scorer inside the normal hybrid-search function, and proves bounded graph fallback plus a content-safe MCP diagnostic in the same execution.
