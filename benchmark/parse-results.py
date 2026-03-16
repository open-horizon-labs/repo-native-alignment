#!/usr/bin/env python3
"""Parse agent transcript JSONL to extract benchmark metrics."""
import json, sys, os
from collections import Counter

def parse_transcript(path):
    tool_calls = Counter()
    solution = ""
    
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            
            msg_type = msg.get("type")
            
            # Count tool uses by name
            if msg_type == "assistant":
                content = msg.get("message", {}).get("content", [])
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "tool_use":
                            tool_name = block.get("name", "unknown")
                            tool_calls[tool_name] += 1
            
            # Capture final assistant text as solution
            if msg_type == "assistant":
                content = msg.get("message", {}).get("content", [])
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "text":
                            solution = block.get("text", "")
    
    return {
        "tool_calls": dict(tool_calls),
        "tool_calls_total": sum(tool_calls.values()),
        "solution_length": len(solution),
        "solution": solution,
    }

if __name__ == "__main__":
    path = sys.argv[1]
    result = parse_transcript(path)
    # Print summary without solution text
    summary = {k: v for k, v in result.items() if k != "solution"}
    print(json.dumps(summary, indent=2))
