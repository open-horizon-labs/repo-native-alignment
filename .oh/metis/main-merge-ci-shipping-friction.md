---
id: main-merge-ci-shipping-friction
outcome: agent-alignment
title: Main-merge CI reduces shipping friction after setup rollout
---

Shipping PR #6 exposed delivery-path tax in merge integration: a README conflict delayed merge despite code being ready.

Actions taken:
- Added main-branch CI workflow (`.github/workflows/rust-main-merge.yml`) to run targeted setup tests and release build on PR/push to `main`.
- Completed merge and verified main contains setup command + docs + workflow.

Learning: for this repo, delivery friction now shifts from manual install docs to branch integration quality and merge readiness. Lightweight CI on main merges reduces regressions and improves confidence-to-merge.
