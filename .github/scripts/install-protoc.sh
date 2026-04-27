#!/usr/bin/env bash
set -euo pipefail

if command -v protoc >/dev/null 2>&1; then
  protoc --version
  exit 0
fi

if command -v brew >/dev/null 2>&1; then
  brew install protobuf
  protoc --version
  exit 0
fi

if command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update
  sudo apt-get install -y protobuf-compiler
  protoc --version
  exit 0
fi

echo "Unable to install protoc: no supported package manager found" >&2
exit 1
