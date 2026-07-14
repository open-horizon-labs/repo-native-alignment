#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
toolchain_file="$repo_root/rust-toolchain.toml"

toolchain="$(
  awk -F'"' '
    $1 ~ /^[[:space:]]*channel[[:space:]]*=/ { print $2; exit }
  ' "$toolchain_file"
)"

if [[ -z "$toolchain" ]]; then
  echo "FAIL: no toolchain channel found in $toolchain_file" >&2
  exit 1
fi

awk -v expected="$toolchain" '
  /uses:[[:space:]]*dtolnay\/rust-toolchain@/ {
    count++
    actual = $0
    sub(/^.*dtolnay\/rust-toolchain@/, "", actual)
    sub(/[[:space:]#].*$/, "", actual)
    if (actual != expected) {
      printf "FAIL: %s:%d does not use Rust %s: %s\n", FILENAME, FNR, expected, $0 > "/dev/stderr"
      mismatch = 1
    }
  }
  END {
    if (count == 0) {
      print "FAIL: no dtolnay/rust-toolchain workflow references found" > "/dev/stderr"
      exit 1
    }
    if (mismatch) {
      exit 1
    }
    printf "Rust toolchain pins agree at %s across %d workflow references\n", expected, count
  }
' "$repo_root"/.github/workflows/*.yml

sccache_action="v0.0.10"

awk -v expected="$sccache_action" '
  /uses:[[:space:]]*mozilla-actions\/sccache-action@/ {
    count++
    actual = $0
    sub(/^.*mozilla-actions\/sccache-action@/, "", actual)
    sub(/[[:space:]#].*$/, "", actual)
    if (actual != expected) {
      printf "FAIL: %s:%d does not use sccache-action %s: %s\n", FILENAME, FNR, expected, $0 > "/dev/stderr"
      mismatch = 1
    }
  }
  END {
    if (count == 0) {
      print "FAIL: no mozilla-actions/sccache-action workflow references found" > "/dev/stderr"
      exit 1
    }
    if (mismatch) {
      exit 1
    }
    printf "sccache-action pins agree at %s across %d workflow references\n", expected, count
  }
' "$repo_root"/.github/workflows/*.yml
