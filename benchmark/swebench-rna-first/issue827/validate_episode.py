#!/usr/bin/env python3
"""Compatibility entry point for the authoritative issue #827 episode verifier."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import verify_selector


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("episode_root", type=Path)
    arguments = parser.parse_args()
    receipt = arguments.episode_root.resolve() / "episode-receipt.json"
    result = verify_selector.verify_episode(receipt)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0 if result["evidence_complete"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
