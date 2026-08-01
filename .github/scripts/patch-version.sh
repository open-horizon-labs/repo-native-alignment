#!/usr/bin/env bash
# Patch the release version into every file that records it, then prove it took.
#
# Usage: patch-version.sh <version>        e.g. patch-version.sh 0.3.0
#
# Extracted from .github/workflows/release.yml, where this logic was duplicated
# across four jobs in two dialects (`sed -i ''` on macOS runners, `sed -i` on
# Linux) and ran only on a real tag push, so a bug in it was discovered by a
# broken release. Keeping it here makes it testable
# (.github/scripts/tests/test-patch-version.sh) and fixable in one place.
#
# Deliberately avoids `sed -i`: the two dialects are why the logic was
# duplicated, and the previous global regex on marketplace.json rewrote *every*
# "version" key rather than the two that describe this package.
set -euo pipefail

# jq is standard on GitHub-hosted and virtually all Linux/macOS CI images, but
# this script previously ran only on a real tag push and its predecessor never
# depended on jq, so a missing binary would otherwise surface as an obscure
# "command not found" deep in a release. Fail clearly instead.
command -v jq > /dev/null || {
  echo "::error::jq is required by patch-version.sh but is not on PATH" >&2
  exit 1
}

VERSION="${1:?usage: patch-version.sh <version>}"

# A tag pattern of 'v*' admits things like `vfoo`, which would otherwise sail
# through every assertion below tautologically and publish a release whose
# Cargo.toml says version = "foo".
if ! printf '%s' "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  echo "::error::version '$VERSION' is not semver" >&2
  exit 1
fi

fail() { echo "::error::$1" >&2; exit 1; }

# --- Cargo.toml -------------------------------------------------------------
# Only the [package] table's own version. A bare `^version = ` match would also
# hit any other top-level table that happens to carry one.
awk -v version="$VERSION" '
  /^\[/ { in_package = ($0 == "[package]") }
  in_package && /^version = / && !done { print "version = \"" version "\""; done = 1; next }
  { print }
' Cargo.toml > Cargo.toml.patched
mv Cargo.toml.patched Cargo.toml

# --- Cargo.lock -------------------------------------------------------------
# Cargo.lock records this workspace member's own version. Omitting it leaves
# main failing every `--locked` command, which is exactly what shipped in
# v0.3.0. `in_block` arms only inside a [[package]] table so a same-named entry
# under [[patch.unused]] is left alone.
#
# awk rather than cargo: this runs in a job with no Rust toolchain and a cold
# cargo cache, where `cargo update --workspace --offline` cannot resolve the
# metal-candle git dependency.
awk -v version="$VERSION" '
  /^\[/ { in_block = ($0 == "[[package]]"); in_pkg = 0 }
  in_block && /^name = "repo-native-alignment"$/ { in_pkg = 1 }
  in_pkg && /^version = / { print "version = \"" version "\""; in_pkg = 0; next }
  { print }
' Cargo.lock > Cargo.lock.patched
mv Cargo.lock.patched Cargo.lock

# --- marketplace.json -------------------------------------------------------
# Exactly the two paths that describe this package. The previous global
# `sed 's/"version": "[^"]*"/.../g'` rewrote every "version" key in the file, so
# an unrelated key — a protocol version, a schema version — would have been
# silently clobbered by a release.
jq --arg v "$VERSION" \
  '.metadata.version = $v | .plugins[].version = $v' \
  .claude-plugin/marketplace.json > .claude-plugin/marketplace.json.patched
mv .claude-plugin/marketplace.json.patched .claude-plugin/marketplace.json

# --- Assertions -------------------------------------------------------------
# These are the only gate that runs on this content: a PR opened by
# create-pull-request with GITHUB_TOKEN cannot trigger workflows, so the bump PR
# receives no Rust CI. Fail the release rather than open a PR we cannot verify.
#
# -F throughout: a version string contains dots, which are wildcards to grep.
grep -qxF "version = \"$VERSION\"" Cargo.toml \
  || fail "Cargo.toml version patch did not apply"

grep -A1 '^name = "repo-native-alignment"$' Cargo.lock \
  | grep -qxF "version = \"$VERSION\"" \
  || fail "Cargo.lock version patch did not apply"

jq -e --arg v "$VERSION" '.metadata.version == $v' \
  .claude-plugin/marketplace.json > /dev/null \
  || fail "marketplace.json metadata.version is not $VERSION"

jq -e --arg v "$VERSION" 'all(.plugins[]; .version == $v)' \
  .claude-plugin/marketplace.json > /dev/null \
  || fail "marketplace.json plugins[].version is not $VERSION"

jq -e '(.plugins | length) > 0' .claude-plugin/marketplace.json > /dev/null \
  || fail "marketplace.json has no plugins to version"

echo "patched and verified: Cargo.toml, Cargo.lock, marketplace.json at $VERSION"
