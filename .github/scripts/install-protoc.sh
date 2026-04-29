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
  if command -v sudo >/dev/null 2>&1; then
    sudo apt-get update
    sudo apt-get install -y protobuf-compiler
  elif [ "$(id -u)" -eq 0 ]; then
    apt-get update
    apt-get install -y protobuf-compiler
  else
    echo "Unable to install protoc with apt-get: sudo is unavailable and the current user is not root" >&2
    exit 1
  fi
  protoc --version
  exit 0
fi

echo "Unable to install protoc: no supported package manager found" >&2
exit 1
