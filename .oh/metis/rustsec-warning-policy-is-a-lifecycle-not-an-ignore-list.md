---
title: RustSec warning policy is a lifecycle, not an ignore list
date: 2026-07-15
outcome: context-assembly
source_issue: 731
---

`cargo audit` already separates vulnerability findings from informational
warnings, but a successful exit does not prove that warning-class risk has an
owner or removal path. A repository policy should therefore join each exact
advisory/package/version/kind tuple to its dependency paths, rationale, owner,
removal issue, review triggers, approval evidence, and expiry.

The join must be exact in both directions. A new warning without policy is an
unreviewed decision; a policy record without a current warning is stale and
should be removed. This makes dependency migrations close their warning records
instead of leaving permanent suppressions behind.

Deterministic report fixtures test the decision mechanism without freezing the
live advisory database. CI can still query current RustSec data, while fixture
tests prove that vulnerability, unknown-warning, stale-policy, and expiry paths
fail closed.
