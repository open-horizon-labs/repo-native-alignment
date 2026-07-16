---
session: "755-ship"
artifact_type: ship-session
updated: "2026-07-15"
---

## Ship Pipeline — PR #755

**Started:** 2026-07-15

### Step 1: RNA-Grounded Review

**Verdict:** ADJUST
**Metis checked:** computed-but-not-delivered and #709 supersession context
**Guardrails checked:** repo-native, computed-but-not-delivered,
no-language/domain conditionals in core, dogfood RNA
**Findings:** 3: canonical body ID missing, chart asset absent, positive selector
expectation incomplete.

### Step 2: Independent Code Review

**Verdict:** REQUEST CHANGES
**Findings:** repo-local private-vocabulary pack absent, chart asset absent,
negative trigger shapes weakly asserted, and line-only hashing ambiguous.

### Step 3: Fix

- Defined canonical AST/explicit body-node IDs and duplicate-ID behavior.
- Required byte ranges for valid content-native Markdown and exact source-byte
  hashing without normalization fallback.
- Added a real PPM bitmap and asserted asset validity.
- Added a repo-local pack declaring private worksheet headings.
- Locked essential source/sidecar/asset trigger shapes and a complete positive
  selector with exact byte range and BLAKE3 digest.
- Focused integration test passes.
