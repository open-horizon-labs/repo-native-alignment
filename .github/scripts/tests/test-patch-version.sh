#!/usr/bin/env bash
# Tests for .github/scripts/patch-version.sh.
#
# This logic previously ran only on a real tag push, so its bugs were discovered
# by broken releases: v0.3.0 shipped a stale Cargo.lock that made every
# `--locked` command on main fail. Every case below is one that actually bit, or
# one an independent review identified as reachable.
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/patch-version.sh"
PASS=0
FAIL=0

ok()   { PASS=$((PASS + 1)); echo "  ok   - $1"; }
bad()  { FAIL=$((FAIL + 1)); echo "  FAIL - $1"; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$3', got '$2')"; fi; }

# A minimal but realistic fixture repo.
make_fixture() {
  local dir="$1"
  rm -rf "$dir" && mkdir -p "$dir/.claude-plugin"
  cat > "$dir/Cargo.toml" <<'TOML'
[package]
name = "repo-native-alignment"
version = "0.0.1"
edition = "2024"

[dependencies]
anyhow = { version = "1.0" }
TOML
  cat > "$dir/Cargo.lock" <<'LOCK'
version = 4

[[package]]
name = "anyhow"
version = "1.0.0"

[[package]]
name = "repo-native-alignment"
version = "0.0.1"
dependencies = [
 "anyhow",
]

[[patch.unused]]
name = "repo-native-alignment"
version = "9.9.9"
LOCK
  cat > "$dir/.claude-plugin/marketplace.json" <<'JSON'
{
  "name": "rna-mcp",
  "metadata": { "version": "0.0.1" },
  "mcp": { "protocol": { "version": "2025-11-25" } },
  "plugins": [ { "name": "rna-mcp", "version": "0.0.1" } ]
}
JSON
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "patch-version.sh"

# --- happy path -------------------------------------------------------------
make_fixture "$TMP/happy"
( cd "$TMP/happy" && bash "$SCRIPT" 1.2.3 >/dev/null )
check "Cargo.toml package version bumped" \
  "$(grep -c '^version = "1.2.3"$' "$TMP/happy/Cargo.toml")" "1"
check "Cargo.toml dependency version untouched" \
  "$(grep -c 'anyhow = { version = "1.0" }' "$TMP/happy/Cargo.toml")" "1"
check "Cargo.lock member version bumped" \
  "$(grep -A1 '^name = "repo-native-alignment"$' "$TMP/happy/Cargo.lock" | grep -c '^version = "1.2.3"$')" "1"
check "Cargo.lock other package untouched" \
  "$(grep -c '^version = "1.0.0"$' "$TMP/happy/Cargo.lock")" "1"
check "Cargo.lock lockfile format version untouched" \
  "$(grep -c '^version = 4$' "$TMP/happy/Cargo.lock")" "1"
check "Cargo.lock [[patch.unused]] entry untouched" \
  "$(grep -c '^version = "9.9.9"$' "$TMP/happy/Cargo.lock")" "1"
check "marketplace metadata.version bumped" \
  "$(jq -r '.metadata.version' "$TMP/happy/.claude-plugin/marketplace.json")" "1.2.3"
check "marketplace plugins[].version bumped" \
  "$(jq -r '.plugins[0].version' "$TMP/happy/.claude-plugin/marketplace.json")" "1.2.3"

# The defect an independent review found in the previous assertion: a global
# regex rewrote every "version" key, and the check that guarded it demanded
# every key equal the release version, so it ratified the clobber.
check "marketplace unrelated protocol version NOT clobbered" \
  "$(jq -r '.mcp.protocol.version' "$TMP/happy/.claude-plugin/marketplace.json")" "2025-11-25"

# --- idempotency ------------------------------------------------------------
( cd "$TMP/happy" && bash "$SCRIPT" 1.2.3 >/dev/null )
check "second run is idempotent" \
  "$(jq -r '.metadata.version' "$TMP/happy/.claude-plugin/marketplace.json")" "1.2.3"

# --- version shape rejection ------------------------------------------------
# `on: push: tags: ['v*']` admits vfoo; VERSION would be "foo" and every
# assertion would pass tautologically.
for bad_version in foo "" 1.2 "1.2.3.4" "v1.2.3"; do
  make_fixture "$TMP/shape"
  if ( cd "$TMP/shape" && bash "$SCRIPT" "$bad_version" >/dev/null 2>&1 ); then
    bad "rejects non-semver '$bad_version'"
  else
    ok "rejects non-semver '${bad_version:-<empty>}'"
  fi
  check "  ...and leaves Cargo.toml unmodified" \
    "$(grep -c '^version = "0.0.1"$' "$TMP/shape/Cargo.toml")" "1"
done

# --- prerelease and build metadata are valid semver -------------------------
make_fixture "$TMP/pre"
if ( cd "$TMP/pre" && bash "$SCRIPT" 1.2.3-rc.1 >/dev/null 2>&1 ); then
  ok "accepts prerelease 1.2.3-rc.1"
else
  bad "accepts prerelease 1.2.3-rc.1"
fi

# --- assertion actually fires when a patch cannot apply ---------------------
make_fixture "$TMP/nomarket"
rm "$TMP/nomarket/.claude-plugin/marketplace.json"
if ( cd "$TMP/nomarket" && bash "$SCRIPT" 1.2.3 >/dev/null 2>&1 ); then
  bad "fails when marketplace.json is missing"
else
  ok "fails when marketplace.json is missing"
fi

make_fixture "$TMP/noplugins"
jq '.plugins = []' "$TMP/noplugins/.claude-plugin/marketplace.json" > "$TMP/t" \
  && mv "$TMP/t" "$TMP/noplugins/.claude-plugin/marketplace.json"
if ( cd "$TMP/noplugins" && bash "$SCRIPT" 1.2.3 >/dev/null 2>&1 ); then
  bad "fails when there are no plugins to version"
else
  ok "fails when there are no plugins to version"
fi

# The v0.3.0 defect itself: Cargo.toml bumped, Cargo.lock left behind.
make_fixture "$TMP/nolock"
( cd "$TMP/nolock" && bash "$SCRIPT" 1.2.3 >/dev/null )
if grep -A1 '^name = "repo-native-alignment"$' "$TMP/nolock/Cargo.lock" | grep -qxF 'version = "1.2.3"'; then
  ok "Cargo.lock cannot be left stale (the v0.3.0 defect)"
else
  bad "Cargo.lock cannot be left stale (the v0.3.0 defect)"
fi

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
