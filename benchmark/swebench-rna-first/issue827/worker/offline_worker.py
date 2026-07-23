#!/usr/bin/python3
"""Exact offline shell worker for the #827 selector.

The gateway sends one canonical, bounded request on stdin.  This process
consumes the request completely before applying Landlock; the model shell
inherits EOF and has no request file to inspect.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys


LAUNCHER = Path("/opt/rna827/landlock-launcher")
STRACE = Path("/usr/bin/strace")
SELF = Path("/opt/rna827/offline-worker")
REQUEST_SCHEMA = "issue827-bash-request-v1"
MAX_REQUEST_BYTES = 1024 * 1024
REQUEST_ID_RE = re.compile(r"^[0-9a-f]{32}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REQUEST_KEYS = {
    "schema_version",
    "request_id",
    "arm",
    "execution_plane",
    "issued_at",
    "issued_monotonic_ns",
    "session_id",
    "tool_use_id",
    "cwd",
    "command",
    "command_sha256",
    "run_in_background",
}


def canonical(value):
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
        )
        + "\n"
    ).encode("utf-8")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_request(stream):
    raw = stream.read(MAX_REQUEST_BYTES + 1)
    if not raw or len(raw) > MAX_REQUEST_BYTES:
        raise ValueError("request stdin missing or exceeds bound")
    try:
        request = json.loads(raw)
    except (json.JSONDecodeError, UnicodeError) as exc:
        raise ValueError("request stdin is not JSON") from exc
    if (
        not isinstance(request, dict)
        or set(request) != REQUEST_KEYS
        or canonical(request) != raw
    ):
        raise ValueError("request stdin is not canonical")
    command = request.get("command")
    if (
        request.get("schema_version") != REQUEST_SCHEMA
        or REQUEST_ID_RE.fullmatch(str(request.get("request_id", ""))) is None
        or request.get("arm") not in {"A", "T"}
        or request.get("execution_plane") != "offline_bash"
        or not isinstance(request.get("issued_at"), str)
        or not request["issued_at"]
        or type(request.get("issued_monotonic_ns")) is not int
        or request["issued_monotonic_ns"] < 0
        or not isinstance(request.get("session_id"), str)
        or not request["session_id"]
        or not isinstance(request.get("tool_use_id"), str)
        or not request["tool_use_id"]
        or not isinstance(request.get("cwd"), str)
        or not request["cwd"]
        or not isinstance(command, str)
        or not command
        or "\x00" in command
        or SHA256_RE.fullmatch(str(request.get("command_sha256", ""))) is None
        or request["command_sha256"]
        != hashlib.sha256(command.encode("utf-8")).hexdigest()
        or request.get("run_in_background") is not False
    ):
        raise ValueError("request stdin contract invalid")
    return request


def policy(required, deny):
    result = [str(LAUNCHER), "--require-abi", str(required)]
    for path in (
        "/usr",
        "/bin",
        "/lib",
        "/etc",
        "/dev",
        "/proc",
        "/declared-toolchain",
    ):
        if Path(path).exists():
            result += ["--ro", path]
    for path in ("/workspace", "/private", "/tmp"):
        if Path(path).exists():
            result += ["--rw", path]
    result += ["--deny-probe", deny]
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--require-landlock", action="store_true")
    parser.add_argument("--landlock-abi-min", type=int, default=1)
    parser.add_argument("--deny-path", default="/run/rna-trace")
    parser.add_argument("--self-test-json", action="store_true")
    args = parser.parse_args()
    if not args.require_landlock:
        raise SystemExit("Landlock must be required")
    if args.self_test_json:
        command = policy(args.landlock_abi_min, "/root") + [
            "--self-test-json"
        ]
        probe = subprocess.run(
            command, check=True, capture_output=True, text=True
        )
        value = json.loads(probe.stdout)
        print(
            json.dumps(
                {
                    "schema_version": "issue827-worker-self-test-v1",
                    "verified": value
                    == {
                        "abi": value["abi"],
                        "denied_probe": True,
                        "enforced": True,
                    }
                    and value["abi"] >= args.landlock_abi_min,
                    "worker_entrypoint_sha256": digest(SELF),
                    "strace_artifact_sha256": digest(STRACE),
                    "network": "none",
                    "uid": os.getuid(),
                    "gid": os.getgid(),
                    "landlock_abi": value["abi"],
                },
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return

    try:
        request = read_request(sys.stdin.buffer)
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc
    command = request["command"]
    argv = policy(args.landlock_abi_min, args.deny_path)
    argv += ["--", "/bin/bash", "--noprofile", "--norc", "-lc", command]
    os.execv(LAUNCHER, argv)


if __name__ == "__main__":
    main()
