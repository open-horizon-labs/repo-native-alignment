#!/usr/bin/env python3
"""Offline manipulation/evidence validator for one #825 A or T episode."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shlex
import sys


VALID_RNA = {"OK_NONEMPTY", "OK_EMPTY"}


def load(path: Path) -> object:
    return json.loads(path.read_text())


def load_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("episode_root", type=Path)
    args = parser.parse_args()
    root = args.episode_root.resolve()
    config = load(root / "config/supervisor.json")
    receipt = load(root / "model-receipt.json")
    state = load(root / "evidence/supervisor-state.json")
    hooks = load_jsonl(root / "evidence/hooks/events.jsonl")
    errors: list[str] = []

    if receipt.get("invocations") != 1 or receipt.get("resumed") is not False:
        errors.append("model_session_not_single_fresh_invocation")
    prompt = root / "user-prompt.bin"
    if receipt.get("user_prompt", {}).get("sha256") != sha(prompt):
        errors.append("user_prompt_hash_mismatch")

    pre = [record for record in hooks if record.get("event", {}).get("hook_event_name") == "PreToolUse"]
    if receipt.get("tool_attempts") != len(pre):
        errors.append("tool_attempt_count_mismatch")

    policy = config["policy"]
    compliant = True
    if policy == "treatment":
        if not pre:
            errors.append("missing_first_model_tool")
            compliant = False
        else:
            first = pre[0]
            event = first["event"]
            command = event.get("tool_input", {}).get("command", "")
            forbidden_shell_syntax = any(token in command for token in ("|", ";", "&&", "||", ">", "<", "$(", "`", "\n"))
            try:
                argv = shlex.split(command)
            except ValueError:
                argv = []
            exact = (
                event.get("tool_name") == "Bash"
                and not forbidden_shell_syntax
                and len(argv) == 5
                and argv[0] == config["wrapper"]
                and argv[1] == "--node"
                and argv[2] in config["initial_ids"]
                and argv[3:] == ["--mode", "neighbors"]
            )
            if not exact:
                errors.append("first_tool_not_exact_model_selected_rna_neighbors")
                compliant = False

        rna_receipts = sorted((root / "evidence/rna-events").glob("*.json"))
        if not rna_receipts:
            errors.append("missing_first_rna_receipt")
            compliant = False
        else:
            first_rna = load(rna_receipts[0])
            if first_rna.get("sequence") != 1 or first_rna.get("mode") != "neighbors":
                errors.append("invalid_first_rna_receipt_identity")
                compliant = False
            if first_rna.get("classification") not in VALID_RNA:
                errors.append("first_rna_not_valid_response")
                compliant = False
            if first_rna.get("node") not in config["initial_ids"]:
                errors.append("first_rna_node_not_injected")
                compliant = False

        if not state.get("first_traversal_succeeded") or state.get("first_traversal_status") not in VALID_RNA:
            errors.append("first_traversal_state_not_verified")
            compliant = False
        if state.get("fatal"):
            errors.append("fatal_rna_state")
            compliant = False
        if any(record.get("decision") == "deny" for record in hooks):
            errors.append("supervisor_denied_model_tool")
            compliant = False
    elif policy != "control":
        errors.append("unknown_policy")

    result = {
        "schema_version": "issue825-episode-validation-v1",
        "policy": policy,
        "evidence_complete": not errors,
        "policy_compliant": compliant and not errors,
        "errors": errors,
        "tool_attempts": len(pre),
        "rna_calls": int(state.get("rna_calls", 0)),
        "first_traversal_status": state.get("first_traversal_status"),
        "terminal_patch_sha256": receipt.get("terminal_patch", {}).get("sha256"),
    }
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0 if result["evidence_complete"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
