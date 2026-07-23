from __future__ import annotations

from contextlib import redirect_stdout
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

WORKER_SPEC = importlib.util.spec_from_file_location(
    "issue827_offline_worker", HERE / "worker/offline_worker.py"
)
assert WORKER_SPEC is not None and WORKER_SPEC.loader is not None
offline_worker = importlib.util.module_from_spec(WORKER_SPEC)
WORKER_SPEC.loader.exec_module(offline_worker)

import bash_gateway
import common_supervisor
import trusted_rna_broker
from isolation import (
    BROKER_READY_SCHEMA,
    BROKER_STOP_SCHEMA,
    BROKER_TRIGGER_SCHEMA,
    HashChainLedger,
    IsolationViolation,
    audit_private_tree,
    build_docker_worker_argv,
    canonical,
    generate_outer_seatbelt_profile,
    generate_trusted_rna_seatbelt_profile,
    mint_request,
    parse_strace_directory,
    sha256_bytes,
    validate_effective_path,
    validate_trusted_rna_root_separation,
    verify_event_chain,
)


def file_sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class EffectiveRootTests(unittest.TestCase):
    def test_file_and_default_directory_paths_are_effective_root_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            checkout = root / "checkout"
            private = root / "private"
            checkout.mkdir()
            private.mkdir()
            target = checkout / "a.py"
            target.write_text("x = 1\n")
            read = validate_effective_path(
                tool_name="Read",
                tool_input={"file_path": "a.py"},
                cwd=str(checkout),
                read_roots=[str(checkout), str(private)],
                write_roots=[str(checkout), str(private)],
            )
            self.assertEqual(read["effective_path"], str(target))
            grep = validate_effective_path(
                tool_name="Grep",
                tool_input={"pattern": "x"},
                cwd=str(checkout),
                read_roots=[str(checkout)],
                write_roots=[str(checkout)],
            )
            self.assertEqual(grep["effective_path"], str(checkout))

    def test_escape_and_symlink_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            checkout = root / "checkout"
            outside = root / "outside"
            checkout.mkdir()
            outside.mkdir()
            (outside / "secret").write_text("secret")
            (checkout / "link").symlink_to(outside, target_is_directory=True)
            for path, code in (
                ("../outside/secret", "native_tool_path_outside_allowed_roots"),
                ("link/secret", "native_tool_path_outside_allowed_roots"),
            ):
                with self.subTest(path=path), self.assertRaises(
                    IsolationViolation
                ) as raised:
                    validate_effective_path(
                        tool_name="Read",
                        tool_input={"file_path": path},
                        cwd=str(checkout),
                        read_roots=[str(checkout)],
                        write_roots=[str(checkout)],
                    )
                self.assertEqual(raised.exception.code, code)
            internal = checkout / "internal"
            internal.mkdir()
            (checkout / "inside-link").symlink_to(
                internal, target_is_directory=True
            )
            with self.assertRaises(IsolationViolation) as raised:
                validate_effective_path(
                    tool_name="Glob",
                    tool_input={"pattern": "**/*", "path": "inside-link"},
                    cwd=str(checkout),
                    read_roots=[str(checkout)],
                    write_roots=[str(checkout)],
                )
            self.assertEqual(
                raised.exception.code, "native_tool_path_uses_link"
            )

    def test_glob_pattern_cannot_override_or_escape_validated_base(self):
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary).resolve()
            accepted = validate_effective_path(
                tool_name="Glob",
                tool_input={"path": str(checkout), "pattern": "**/*.py"},
                cwd=str(checkout),
                read_roots=[str(checkout)],
                write_roots=[str(checkout)],
            )
            self.assertEqual(
                accepted["pattern_sha256"],
                hashlib.sha256(b"**/*.py").hexdigest(),
            )
            for pattern in ("/private/**/*", "../private/**/*"):
                with self.subTest(pattern=pattern), self.assertRaises(
                    IsolationViolation
                ) as raised:
                    validate_effective_path(
                        tool_name="Glob",
                        tool_input={
                            "path": str(checkout),
                            "pattern": pattern,
                        },
                        cwd=str(checkout),
                        read_roots=[str(checkout)],
                        write_roots=[str(checkout)],
                    )
                self.assertEqual(
                    raised.exception.code, "native_glob_pattern_escape"
                )


class PrivateTreeAndLedgerTests(unittest.TestCase):
    def test_private_tree_rejects_hardlinks_and_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            source.mkdir()
            original = source / "a"
            original.write_text("a")
            good = audit_private_tree(source)
            self.assertEqual(good["hardlinks"], 0)
            os.link(original, source / "hard")
            with self.assertRaises(IsolationViolation) as raised:
                audit_private_tree(source)
            self.assertEqual(
                raised.exception.code, "private_tree_contains_hardlink"
            )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "target").write_text("x")
            (root / "link").symlink_to(root / "target")
            with self.assertRaises(IsolationViolation) as raised:
                audit_private_tree(root)
            self.assertEqual(
                raised.exception.code, "private_tree_contains_symlink"
            )

    def test_ledger_is_monotonic_hash_chained_and_tamper_evident(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "ledger.jsonl"
            ledger = HashChainLedger(path)
            first = ledger.append(
                actor="test",
                event_type="one",
                outcome="allow",
                arm="A",
            )
            second = ledger.append(
                actor="test",
                event_type="two",
                outcome="deny",
                arm="A",
            )
            records = ledger.read()
            self.assertEqual(
                second["previous_event_sha256"], first["event_sha256"]
            )
            self.assertEqual(verify_event_chain(records), second["event_sha256"])
            records[0]["outcome"] = "tampered"
            path.write_bytes(b"".join(canonical(record) for record in records))
            with self.assertRaises(IsolationViolation) as raised:
                ledger.read()
            self.assertEqual(
                raised.exception.code, "ledger_event_hash_mismatch"
            )


class WorkerContractTests(unittest.TestCase):
    def worker_config(self, root: Path) -> dict:
        docker = root / "docker"
        docker.write_text("#!/bin/sh\nexit 0\n")
        docker.chmod(0o755)
        checkout = root / "checkout"
        private = root / "private"
        toolchain = root / "toolchain"
        checkout.mkdir()
        private.mkdir()
        toolchain.mkdir()
        return {
            "docker_binary": str(docker),
            "docker_binary_sha256": file_sha(docker),
            "worker_image": "rna827/case@sha256:" + "a" * 64,
            "worker_image_manifest_sha256": "b" * 64,
            "worker_image_preflight_verified": True,
            "worker_landlock_required": True,
            "worker_landlock_abi_min": 1,
            "worker_landlock_preflight_verified": True,
            "worker_entrypoint": "/opt/rna827/offline-worker",
            "worker_entrypoint_sha256": "c" * 64,
            "strace_path": "/usr/bin/strace",
            "strace_artifact_sha256": "d" * 64,
            "worker_uid": 501,
            "worker_gid": 20,
            "worker_pids_limit": 128,
            "worker_memory_bytes": 1024 * 1024 * 1024,
            "worker_cpus": 4,
            "worker_cwd": "/workspace",
            "worker_env": {
                "HOME": "/private/home",
                "TMPDIR": "/private/tmp",
                "PATH": "/opt/rna827/bin:/usr/bin:/bin",
                "LANG": "C.UTF-8",
                "LC_ALL": "C.UTF-8",
                "PIP_NO_INDEX": "1",
            },
            "checkout_mount": {
                "source": str(checkout),
                "target": "/workspace",
                "mode": "rw",
            },
            "private_tmp_mount": {
                "source": str(private),
                "target": "/private",
                "mode": "rw",
            },
            "declared_toolchain_mounts": [
                {
                    "source": str(toolchain),
                    "target": "/opt/rna827",
                    "mode": "ro",
                }
            ],
        }

    def test_worker_argv_is_offline_nonroot_readonly_and_traced(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config = self.worker_config(root)
            request = root / "request.json"
            request.write_text("{}\n")
            trace = root / "trace"
            trace.mkdir()
            request_id = "e" * 32
            argv = build_docker_worker_argv(
                config=config,
                request_path=request,
                trace_directory=trace,
                container_name=f"rna827-{request_id}",
            )
            rendered = "\0".join(argv)
            for required in (
                "--pull=never",
                "--network=none",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges:true",
                "--read-only",
                "--user\000501:20",
                "trace=%file,%network,%process",
                "/usr/bin/strace",
                "rna827/case@sha256:",
            ):
                if required == "trace=%file,%network,%process":
                    self.assertIn(required, rendered)
                else:
                    self.assertIn(required, rendered)
            self.assertIn("landlock_restrict_self", rendered)
            self.assertIn("-i", argv)
            self.assertIn("-q", argv)
            self.assertNotIn("-qq", argv)
            self.assertNotIn("ANTHROPIC", rendered)
            self.assertNotIn("command", rendered)
            self.assertNotIn(",rw", rendered)
            self.assertNotIn(str(request), rendered)
            self.assertNotIn("/run/rna-request", rendered)
            self.assertIn(
                f"type=bind,src={config['declared_toolchain_mounts'][0]['source']},dst=/opt/rna827,readonly=true",
                argv,
            )
            self.assertNotIn("--request", argv)
            self.assertEqual(argv[-2:], ["--deny-path", "/run/rna-trace"])

    def test_worker_consumes_only_bounded_canonical_stdin_before_landlock(self):
        command = "printf fixture"
        request = {
            "schema_version": "issue827-bash-request-v1",
            "request_id": "a" * 32,
            "arm": "T",
            "execution_plane": "offline_bash",
            "issued_at": "2026-07-23T00:00:00Z",
            "issued_monotonic_ns": 1,
            "session_id": "session",
            "tool_use_id": "tool",
            "cwd": "/workspace",
            "command": command,
            "command_sha256": hashlib.sha256(
                command.encode("utf-8")
            ).hexdigest(),
            "run_in_background": False,
        }
        encoded = offline_worker.canonical(request)
        self.assertEqual(
            offline_worker.read_request(io.BytesIO(encoded)), request
        )
        with self.assertRaisesRegex(ValueError, "not canonical"):
            offline_worker.read_request(
                io.BytesIO(json.dumps(request, indent=2).encode("utf-8"))
            )
        with self.assertRaisesRegex(ValueError, "exceeds bound"):
            offline_worker.read_request(
                io.BytesIO(b"x" * (offline_worker.MAX_REQUEST_BYTES + 1))
            )
        policy = offline_worker.policy(1, "/run/rna-trace")
        self.assertNotIn("/run/rna-request", policy)

    def test_trace_requires_completion_and_flags_masked_attempts(self):
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary)
            (trace / "trace.123").write_text(
                "1699.9 landlock_restrict_self(3, 0) = 0\n"
                '1699.95 newfstatat(AT_FDCWD, "/", {}, 0) = 0\n'
                '1699.96 connect(3, {sa_family=AF_UNIX, sun_path="/var/run/nscd/socket"}, 110) = -1 ENOENT\n'
                '1700.0 socket(AF_INET, SOCK_STREAM, IPPROTO_IP) = -1 ENETUNREACH\n'
                '1700.1 openat(AT_FDCWD, "/shared/evidence/x", O_RDONLY) = -1 ENOENT\n'
                "1700.2 +++ exited with 0 +++\n"
            )
            report = parse_strace_directory(
                trace,
                allowed_path_prefixes=[
                    "/workspace",
                    "/usr",
                    "/shared",
                    "/var/run",
                ],
                forbidden_path_fragments=["/shared/evidence"],
            )
            self.assertTrue(report["complete"])
            self.assertEqual(
                {item["code"] for item in report["violations"]},
                {
                    "network_syscall_attempt",
                    "forbidden_fragment_attempt",
                    "forbidden_path_attempt",
                },
            )
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary)
            (trace / "trace.1").write_text(
                "1699.9 landlock_restrict_self(3, 0) = 0\n"
                '1700.0 openat(AT_FDCWD, "/workspace/a", O_RDONLY) = 3\n'
            )
            with self.assertRaises(IsolationViolation) as raised:
                parse_strace_directory(
                    trace,
                    allowed_path_prefixes=["/workspace"],
                    forbidden_path_fragments=[],
                )
            self.assertEqual(
                raised.exception.code,
                "trace_member_missing_terminal_record",
            )

    def test_seatbelt_profile_is_deterministic_and_network_outbound_survives(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            read = root / "read"
            write = root / "write"
            read.mkdir()
            write.mkdir()
            one = generate_outer_seatbelt_profile(
                read_roots=[read, write], write_roots=[write]
            )
            two = generate_outer_seatbelt_profile(
                read_roots=[write, read], write_roots=[write]
            )
            self.assertEqual(one, two)
            self.assertIn("(deny network-inbound)", one)
            self.assertNotIn("(deny network-outbound)", one)
            self.assertIn(str(read), one)
            self.assertIn('(literal "/")', one)

    def test_outer_qualification_profile_can_limit_outbound_to_loopback(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            profile = generate_outer_seatbelt_profile(
                read_roots=[root],
                write_roots=[root],
                loopback_only_outbound=True,
            )
            self.assertIn(
                '(deny network-outbound '
                '(require-not (remote ip "localhost:*")))',
                profile,
            )

    def test_trusted_rna_profile_is_deterministic_and_denies_all_network(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            read = root / "read"
            write = root / "write"
            read.mkdir()
            write.mkdir()
            one = generate_trusted_rna_seatbelt_profile(
                read_roots=[read, write], write_roots=[write]
            )
            two = generate_trusted_rna_seatbelt_profile(
                read_roots=[write, read], write_roots=[write]
            )
            self.assertEqual(one, two)
            self.assertIn("(deny network-inbound)", one)
            self.assertIn("(deny network-outbound)", one)
            self.assertNotIn(str(root / "forbidden"), one)

    def test_outer_profile_excludes_index_and_rna_artifacts_broker_profile_includes_them(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            checkout = root / "checkout"
            harness = root / "harness"
            index = root / "index"
            artifact = root / "rna-artifact"
            for path in (checkout, harness, index, artifact):
                path.mkdir()
            outer = generate_outer_seatbelt_profile(
                read_roots=[checkout, harness],
                write_roots=[checkout],
            )
            broker = generate_trusted_rna_seatbelt_profile(
                read_roots=[checkout, harness, index, artifact],
                write_roots=[harness],
            )
            self.assertNotIn(str(index), outer)
            self.assertNotIn(str(artifact), outer)
            self.assertIn(str(index), broker)
            self.assertIn(str(artifact), broker)

    def test_trusted_rna_roots_reject_broad_access_to_sensitive_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            shared = root / "shared"
            episode = shared / "case" / "T"
            sibling = shared / "case" / "A"
            credentials = root / "credentials"
            for path in (episode, sibling, credentials):
                path.mkdir(parents=True)
            validate_trusted_rna_root_separation(
                allowed_roots=[episode],
                forbidden_roots=[shared, sibling, credentials],
            )
            for broad in (root, shared, sibling, credentials):
                with self.subTest(broad=broad), self.assertRaises(
                    IsolationViolation
                ) as raised:
                    validate_trusted_rna_root_separation(
                        allowed_roots=[broad],
                        forbidden_roots=[shared, sibling, credentials],
                    )
                self.assertEqual(
                    raised.exception.code,
                    "trusted_rna_root_exposes_forbidden_path",
                )

    @unittest.skipUnless(
        sys.platform == "darwin" and Path("/usr/bin/sandbox-exec").is_file(),
        "requires macOS Seatbelt",
    )
    def test_trusted_rna_profile_blocks_network_and_forbidden_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            allowed = root / "allowed"
            forbidden = root / "forbidden"
            allowed.mkdir()
            forbidden.mkdir()
            secret = forbidden / "secret"
            secret.write_text("forbidden\n")
            runtime_roots = [
                path
                for path in (
                    Path("/usr"),
                    Path("/System"),
                    Path("/Library"),
                    Path("/opt/homebrew"),
                    Path(sys.prefix),
                    Path("/private/var/select"),
                )
                if path.exists()
            ]
            profile = root / "trusted-rna.sb"
            profile.write_text(
                generate_trusted_rna_seatbelt_profile(
                    read_roots=[*runtime_roots, allowed],
                    write_roots=[allowed],
                )
            )
            profile.chmod(0o444)
            network_reached = allowed / "network-reached"
            network_escaped = allowed / "network-escaped"
            network = subprocess.run(
                [
                    "/usr/bin/sandbox-exec",
                    "-f",
                    str(profile),
                    str(Path(sys.executable).resolve()),
                    "-c",
                    (
                        "from pathlib import Path; import socket; "
                        f"Path({str(network_reached)!r}).write_text('yes'); "
                        "socket.socket().connect(('127.0.0.1', 9)); "
                        f"Path({str(network_escaped)!r}).write_text('bad')"
                    ),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                cwd=allowed,
                check=False,
            )
            file_reached = allowed / "file-reached"
            forbidden_read = subprocess.run(
                [
                    "/usr/bin/sandbox-exec",
                    "-f",
                    str(profile),
                    str(Path(sys.executable).resolve()),
                    "-c",
                    (
                        "from pathlib import Path; "
                        f"Path({str(file_reached)!r}).write_text('yes'); "
                        f"print(Path({str(secret)!r}).read_text())"
                    ),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                cwd=allowed,
                check=False,
            )
            self.assertNotEqual(network.returncode, 0, network.stderr)
            self.assertTrue(network_reached.is_file(), network.stderr)
            self.assertFalse(network_escaped.exists())
            self.assertNotEqual(
                forbidden_read.returncode, 0, forbidden_read.stderr
            )
            self.assertTrue(file_reached.is_file(), forbidden_read.stderr)
            self.assertNotIn(b"forbidden", forbidden_read.stdout)

    def test_container_absence_requires_exact_not_found_tuple(self):
        name = "rna827-" + "a" * 32
        exact = subprocess.CompletedProcess(
            [],
            1,
            b"[]\n",
            f"Error response from daemon: No such container: {name}\n".encode(),
        )
        evidence = {"exception": None}
        self.assertEqual(
            bash_gateway._classify_container_probe(exact, evidence, name),
            "absent",
        )
        for result, probe in (
            (
                subprocess.CompletedProcess(
                    [], 1, b"", b"Cannot connect to the Docker daemon\n"
                ),
                {"exception": None},
            ),
            (subprocess.CompletedProcess([], 124, b"", b""), {"exception": "TimeoutExpired"}),
            (subprocess.CompletedProcess([], 1, b"[]\n", b"permission denied\n"), {"exception": None}),
            (subprocess.CompletedProcess([], 0, b"not-json", b""), {"exception": None}),
        ):
            with self.subTest(result=result, probe=probe):
                self.assertEqual(
                    bash_gateway._classify_container_probe(result, probe, name),
                    "unknown",
                )

    def test_worker_launch_failure_still_cleans_and_persists_teardown(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config = self.worker_config(root)
            trace_root = root / "traces"
            teardown_root = root / "teardowns"
            trace_root.mkdir()
            teardown_root.mkdir()
            config.update(
                {
                    "gateway_trace_directory": str(trace_root),
                    "gateway_teardown_directory": str(teardown_root),
                    "docker_host_env": {"PATH": "/usr/bin:/bin"},
                    "docker_control_timeout_seconds": 1,
                    "worker_timeout_seconds": 1,
                    "trace_allowed_path_prefixes": ["/workspace"],
                    "trace_forbidden_path_fragments": [],
                }
            )
            request_id = "b" * 32
            request_path = root / "request.json"
            request = {"request_id": request_id}
            request_path.write_bytes(canonical(request))
            name = f"rna827-{request_id}"
            cleanup_result = subprocess.CompletedProcess([], 0, b"", b"")
            cleanup_evidence = {
                "exception": None,
                "returncode": 0,
                "argv_sha256": "c" * 64,
                "stdout_bytes": 0,
                "stdout_sha256": hashlib.sha256(b"").hexdigest(),
                "stderr_bytes": 0,
                "stderr_sha256": hashlib.sha256(b"").hexdigest(),
                "elapsed_monotonic_ns": 1,
            }
            inspect_result = subprocess.CompletedProcess(
                [],
                1,
                b"[]\n",
                f"Error response from daemon: No such container: {name}\n".encode(),
            )
            inspect_evidence = {
                **cleanup_evidence,
                "returncode": 1,
                "stdout_bytes": len(inspect_result.stdout),
                "stdout_sha256": hashlib.sha256(inspect_result.stdout).hexdigest(),
                "stderr_bytes": len(inspect_result.stderr),
                "stderr_sha256": hashlib.sha256(inspect_result.stderr).hexdigest(),
            }
            with mock.patch.object(
                bash_gateway, "build_docker_worker_argv", return_value=["docker", "run"]
            ), mock.patch.object(
                bash_gateway.subprocess, "run", side_effect=OSError("launch failed")
            ) as launch, mock.patch.object(
                bash_gateway,
                "_docker_control",
                side_effect=[
                    (cleanup_result, cleanup_evidence),
                    (inspect_result, inspect_evidence),
                ],
            ) as control:
                with self.assertRaises(IsolationViolation) as raised:
                    bash_gateway._run_offline(
                        request=request,
                        request_path=request_path,
                        config=config,
                    )
            self.assertEqual(raised.exception.code, "worker_launch_OSError")
            self.assertEqual(
                launch.call_args.kwargs["input"], canonical(request)
            )
            self.assertEqual(control.call_count, 2)
            teardown_path = teardown_root / f"{request_id}.json"
            teardown = json.loads(teardown_path.read_bytes())
            self.assertEqual(teardown["container_state"], "absent")
            self.assertTrue(teardown["cleanup_verified"])
            self.assertEqual(teardown["primary_failure"], "worker_launch_OSError")
            observed = teardown.pop("receipt_sha256")
            self.assertEqual(observed, sha256_bytes(canonical(teardown)))


class RequestAndSettingsTests(unittest.TestCase):
    def test_trusted_rna_uses_registered_python_not_wrapper_shebang(self):
        sandbox_exec = Path("/usr/bin/sandbox-exec")
        if not sandbox_exec.is_file():
            self.skipTest("requires exact /usr/bin/sandbox-exec")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            wrapper = root / "rna_traverse.py"
            wrapper.write_text("#!/usr/bin/env python3\n")
            profile = root / "trusted-rna.sb"
            profile.write_text("(version 1)\n(allow default)\n")
            profile.chmod(0o444)
            environment = root / "canonical-environment.json"
            trusted_env = {"PATH": "/usr/bin:/bin"}
            environment.write_bytes(canonical(trusted_env))
            gateway_python = Path(sys.executable).resolve()
            command = f"{wrapper} --node foo.py:target:function --mode neighbors"
            config = {
                "wrapper": str(wrapper),
                "gateway_python": str(gateway_python),
                "gateway_python_sha256": file_sha(gateway_python),
                "checkout": str(root),
                "trusted_rna_env": trusted_env,
                "trusted_rna_timeout_seconds": 5,
                "sandbox_exec": str(sandbox_exec),
                "sandbox_exec_sha256": file_sha(sandbox_exec),
                "trusted_rna_seatbelt_profile": str(profile),
                "trusted_rna_seatbelt_profile_sha256": file_sha(profile),
                "trusted_rna_environment": str(environment),
                "trusted_rna_environment_sha256": file_sha(environment),
                "trusted_rna_read_roots": [str(root), str(environment)],
                "trusted_rna_write_roots": [str(root)],
            }
            completed = subprocess.CompletedProcess([], 0, b"ok", b"")
            with mock.patch.object(
                bash_gateway.subprocess, "run", return_value=completed
            ) as invoked:
                result, evidence = bash_gateway._run_trusted_rna(
                    {"command": command}, config
                )
            self.assertIs(result, completed)
            self.assertEqual(
                invoked.call_args.args[0],
                [
                    str(sandbox_exec),
                    "-f",
                    str(profile),
                    str(gateway_python),
                    str(wrapper),
                    "--node",
                    "foo.py:target:function",
                    "--mode",
                    "neighbors",
                ],
            )
            self.assertEqual(evidence["execution_plane"], "trusted_rna")
            self.assertEqual(evidence["network_inbound"], "denied")
            self.assertEqual(evidence["network_outbound"], "denied")
            self.assertEqual(
                evidence["seatbelt_profile"]["sha256"], file_sha(profile)
            )
            self.assertEqual(
                evidence["canonical_environment"]["sha256"],
                file_sha(environment),
            )

    def test_request_is_single_use_and_canonical(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            requests = root / "requests"
            claimed = root / "claimed"
            config = {
                "policy": "control",
                "gateway_request_directory": str(requests),
                "gateway_claimed_directory": str(claimed),
            }
            event = {
                "session_id": "session",
                "tool_use_id": "tool",
                "cwd": str(root),
            }
            request, path, digest = mint_request(
                config=config,
                event=event,
                execution_plane="offline_bash",
                command="python -c 'print(1)'",
            )
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            claimed_request, claimed_path = bash_gateway.claim_request(
                config=config,
                request_id=str(request["request_id"]),
                expected_sha256=digest,
            )
            self.assertEqual(claimed_request, request)
            self.assertTrue(claimed_path.is_file())
            with self.assertRaises(IsolationViolation) as raised:
                bash_gateway.claim_request(
                    config=config,
                    request_id=str(request["request_id"]),
                    expected_sha256=digest,
                )
            self.assertEqual(
                raised.exception.code, "request_missing_or_consumed"
            )

    def test_settings_are_allow_only_gateway_for_bash(self):
        template = json.loads(
            (HERE / "claude-settings.template.json").read_text()
        )
        permissions = template["permissions"]
        self.assertEqual(permissions["defaultMode"], "dontAsk")
        bash_rules = [
            value
            for value in permissions["allow"]
            if value.startswith("Bash")
        ]
        self.assertEqual(len(bash_rules), 1)
        self.assertIn("__BASH_GATEWAY__", bash_rules[0])
        self.assertNotIn("Bash(*)", bash_rules)

    def test_common_hook_replaces_bash_and_rejects_background(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            checkout = root / "checkout"
            private = root / "private"
            toolchain = root / "toolchain"
            evidence = root / "evidence"
            for path in (checkout, private, toolchain, evidence):
                path.mkdir()
            gateway = root / "bash_gateway.py"
            gateway.write_text("# gateway\n")
            python = root / "python"
            python.write_text("# python\n")
            supervisor = root / "supervisor.json"
            supervisor.write_text("{}\n")
            config = {
                "schema_version": "issue827-supervisor-config-v4",
                "policy": "control",
                "checkout": str(checkout),
                "wrapper": str(root / "rna_traverse.py"),
                "native_read_roots": [str(checkout), str(private)],
                "native_write_roots": [str(checkout), str(private)],
                "common_state": str(evidence / "state.json"),
                "common_hook_ledger": str(evidence / "common.jsonl"),
                "gateway_request_directory": str(evidence / "requests"),
                "gateway_claimed_directory": str(evidence / "claimed"),
                "gateway_revoked_directory": str(evidence / "revoked"),
                "gateway_receipt_directory": str(evidence / "receipts"),
                "gateway_python": str(python),
                "bash_gateway": str(gateway),
                "gateway_config": str(supervisor),
                "gateway_tool_timeout_ms": 1000,
            }
            event = {
                "session_id": "session",
                "tool_use_id": "tool",
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {
                    "command": "printf ok",
                    "timeout": 20,
                    "run_in_background": False,
                },
                "cwd": str(checkout),
            }
            output = io.StringIO()
            with redirect_stdout(output):
                common_supervisor.handle(event, config)
            decision = json.loads(output.getvalue())["hookSpecificOutput"]
            self.assertEqual(decision["permissionDecision"], "allow")
            self.assertIn(str(gateway), decision["updatedInput"]["command"])
            self.assertNotIn("printf ok", decision["updatedInput"]["command"])
            event["tool_input"]["run_in_background"] = True
            event["tool_use_id"] = "tool2"
            output = io.StringIO()
            with redirect_stdout(output):
                common_supervisor.handle(event, config)
            denied = json.loads(output.getvalue())["hookSpecificOutput"]
            self.assertEqual(denied["permissionDecision"], "deny")


class TrustedRnaBrokerTests(unittest.TestCase):
    def fixture(self, root: Path) -> tuple[dict, dict, Path, bytes, bytes]:
        for name in (
            "claimed",
            "receipts",
            "broker-requests",
            "broker-claimed",
            "broker-output",
        ):
            (root / name).mkdir()
        broker_path = HERE / "trusted_rna_broker.py"
        config = {
            "trusted_rna_broker": str(broker_path),
            "trusted_rna_broker_sha256": file_sha(broker_path),
            "trusted_rna_broker_ready": str(root / "ready.json"),
            "trusted_rna_broker_stop": str(root / "stop.json"),
            "trusted_rna_broker_teardown": str(root / "teardown.json"),
            "trusted_rna_broker_request_directory": str(
                root / "broker-requests"
            ),
            "trusted_rna_broker_claimed_directory": str(
                root / "broker-claimed"
            ),
            "trusted_rna_broker_output_directory": str(root / "broker-output"),
            "trusted_rna_broker_client_timeout_seconds": 0.15,
            "gateway_claimed_directory": str(root / "claimed"),
            "gateway_receipt_directory": str(root / "receipts"),
                "isolation_ledger": str(root / "isolation.jsonl"),
                "trusted_rna_env": {},
        }
        request_id = "a" * 32
        request = {
            "schema_version": "issue827-bash-request-v1",
            "request_id": request_id,
            "arm": "T",
            "execution_plane": "trusted_rna",
            "issued_at": "2026-07-23T00:00:00+00:00",
            "issued_monotonic_ns": 1,
            "session_id": "session",
            "tool_use_id": "tool",
            "cwd": str(root),
            "command": "/wrapper --node foo.py:target:function --mode neighbors",
            "command_sha256": sha256_bytes(
                b"/wrapper --node foo.py:target:function --mode neighbors"
            ),
            "run_in_background": False,
        }
        request_path = root / "claimed" / f"{request_id}.json"
        request_path.write_bytes(canonical(request))
        request_path.chmod(0o600)
        stdout = b"graph response\n"
        stderr = b""
        return config, request, request_path, stdout, stderr

    def ready(self, config: dict, config_sha256: str) -> None:
        value = {
            "schema_version": BROKER_READY_SCHEMA,
            "config_sha256": config_sha256,
            "broker": {
                "path": config["trusted_rna_broker"],
                "sha256": config["trusted_rna_broker_sha256"],
            },
            "pid": os.getpid(),
            "started_monotonic_ns": 1,
            "process_environment_sha256": sha256_bytes(
                canonical(config.get("trusted_rna_env", {}))
            ),
            "canonical_environment_sha256": sha256_bytes(
                canonical(config.get("trusted_rna_env", {}))
            ),
            "environment_names": sorted(config.get("trusted_rna_env", {})),
            "os_injected_environment": {},
            "credential_environment_names": [],
            "provider_environment_inherited": False,
        }
        value["receipt_sha256"] = sha256_bytes(canonical(value))
        Path(config["trusted_rna_broker_ready"]).write_bytes(canonical(value))

    def receipt(
        self,
        config: dict,
        request: dict,
        request_path: Path,
        stdout: bytes,
        stderr: bytes,
    ) -> dict:
        request_id = request["request_id"]
        stdout_path = (
            Path(config["trusted_rna_broker_output_directory"])
            / f"{request_id}.stdout"
        )
        stderr_path = (
            Path(config["trusted_rna_broker_output_directory"])
            / f"{request_id}.stderr"
        )
        stdout_path.write_bytes(stdout)
        stderr_path.write_bytes(stderr)
        value = {
            "schema_version": bash_gateway.RECEIPT_SCHEMA,
            "request_id": request_id,
            "request_sha256": file_sha(request_path),
            "session_id": request["session_id"],
            "tool_use_id": request["tool_use_id"],
            "arm": "T",
            "execution_plane": "trusted_rna",
            "original_command_sha256": request["command_sha256"],
            "status": "success",
            "returncode": 0,
            "stdout_bytes": len(stdout),
            "stdout_sha256": sha256_bytes(stdout),
            "stderr_bytes": len(stderr),
            "stderr_sha256": sha256_bytes(stderr),
            "execution": {},
            "violations": [],
            "broker_owned": True,
            "broker_trigger_sha256": "b" * 64,
            "stdout": {
                "path": str(stdout_path),
                "bytes": len(stdout),
                "sha256": sha256_bytes(stdout),
            },
            "stderr": {
                "path": str(stderr_path),
                "bytes": len(stderr),
                "sha256": sha256_bytes(stderr),
            },
        }
        value["receipt_sha256"] = sha256_bytes(canonical(value))
        return value

    def test_nested_seatbelt_client_never_invokes_subprocess(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config, request, request_path, stdout, stderr = self.fixture(root)
            config_sha256 = "c" * 64
            self.ready(config, config_sha256)
            receipt = self.receipt(
                config, request, request_path, stdout, stderr
            )
            claimed_trigger = (
                Path(config["trusted_rna_broker_claimed_directory"])
                / f"{request['request_id']}.json"
            )
            def broker_claim(_path: Path, value: object) -> None:
                claimed_trigger.write_bytes(canonical(value))
                claimed_trigger.chmod(0o600)
            trigger_value = {
                "schema_version": BROKER_TRIGGER_SCHEMA,
                "config_sha256": config_sha256,
                "request_id": request["request_id"],
                "request_sha256": file_sha(request_path),
                "claimed_request": str(request_path),
            }
            broker_claim(Path(), trigger_value)
            receipt["broker_trigger_sha256"] = file_sha(claimed_trigger)
            receipt.pop("receipt_sha256")
            receipt["receipt_sha256"] = sha256_bytes(canonical(receipt))
            receipt_path = (
                Path(config["gateway_receipt_directory"])
                / f"{request['request_id']}.json"
            )
            receipt_path.write_bytes(canonical(receipt))
            with mock.patch.object(
                bash_gateway.subprocess,
                "run",
                side_effect=AssertionError("nested sandbox-exec attempted"),
            ), mock.patch.object(
                bash_gateway,
                "exclusive_json",
                side_effect=broker_claim,
            ):
                code, observed, actual_stdout, actual_stderr = (
                    bash_gateway._wait_trusted_rna_broker(
                        request=request,
                        request_path=request_path,
                        config=config,
                        config_sha256=config_sha256,
                    )
                )
            self.assertEqual(code, 0)
            self.assertEqual(observed, receipt)
            self.assertEqual(actual_stdout, stdout)
            self.assertEqual(actual_stderr, stderr)
            self.assertTrue(claimed_trigger.is_file())

    def test_client_rejects_tampered_receipt_and_times_out_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config, request, request_path, stdout, stderr = self.fixture(root)
            config_sha256 = "d" * 64
            self.ready(config, config_sha256)
            receipt = self.receipt(
                config, request, request_path, stdout, stderr
            )
            receipt["returncode"] = 7
            receipt_path = (
                Path(config["gateway_receipt_directory"])
                / f"{request['request_id']}.json"
            )
            receipt_path.write_bytes(canonical(receipt))
            with self.assertRaises(IsolationViolation) as raised:
                bash_gateway._wait_trusted_rna_broker(
                    request=request,
                    request_path=request_path,
                    config=config,
                    config_sha256=config_sha256,
                )
            self.assertEqual(
                raised.exception.code,
                "trusted_rna_broker_receipt_binding_mismatch",
            )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config, request, request_path, _, _ = self.fixture(root)
            config["trusted_rna_broker_client_timeout_seconds"] = 0.03
            config_sha256 = "e" * 64
            self.ready(config, config_sha256)
            with self.assertRaises(IsolationViolation) as raised:
                bash_gateway._wait_trusted_rna_broker(
                    request=request,
                    request_path=request_path,
                    config=config,
                    config_sha256=config_sha256,
                )
            self.assertEqual(
                raised.exception.code,
                "trusted_rna_broker_response_timeout",
            )

    def test_broker_claims_exact_trigger_and_writes_clean_teardown(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config, request, request_path, stdout, stderr = self.fixture(root)
            config_sha256 = "f" * 64
            bare_receipt = self.receipt(
                config, request, request_path, stdout, stderr
            )
            for key in (
                "broker_owned",
                "broker_trigger_sha256",
                "stdout",
                "stderr",
                "receipt_sha256",
            ):
                bare_receipt.pop(key, None)
            for suffix in ("stdout", "stderr"):
                (
                    Path(config["trusted_rna_broker_output_directory"])
                    / f"{request['request_id']}.{suffix}"
                ).unlink()
            trigger = {
                "schema_version": BROKER_TRIGGER_SCHEMA,
                "config_sha256": config_sha256,
                "request_id": request["request_id"],
                "request_sha256": file_sha(request_path),
                "claimed_request": str(request_path),
            }
            trigger_path = (
                Path(config["trusted_rna_broker_request_directory"])
                / f"{request['request_id']}.json"
            )
            trigger_path.write_bytes(canonical(trigger))
            trigger_path.chmod(0o600)
            outcomes: list[int] = []
            with mock.patch.object(
                trusted_rna_broker.bash_gateway,
                "execute",
                return_value=(0, bare_receipt, stdout, stderr),
            ), mock.patch.dict(os.environ, {}, clear=True):
                thread = threading.Thread(
                    target=lambda: outcomes.append(
                        trusted_rna_broker.serve(config, config_sha256)
                    )
                )
                thread.start()
                receipt_path = (
                    Path(config["gateway_receipt_directory"])
                    / f"{request['request_id']}.json"
                )
                deadline = time.monotonic() + 2
                while not receipt_path.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(receipt_path.is_file())
                stop = {
                    "schema_version": BROKER_STOP_SCHEMA,
                    "config_sha256": config_sha256,
                }
                Path(config["trusted_rna_broker_stop"]).write_bytes(
                    canonical(stop)
                )
                Path(config["trusted_rna_broker_stop"]).chmod(0o600)
                thread.join(timeout=2)
            self.assertFalse(thread.is_alive())
            self.assertEqual(outcomes, [0])
            teardown = json.loads(
                Path(config["trusted_rna_broker_teardown"]).read_bytes()
            )
            observed = teardown.pop("receipt_sha256")
            self.assertTrue(teardown["clean"])
            self.assertEqual(teardown["pending"], [])
            self.assertFalse(teardown["active_child"])
            self.assertEqual(observed, sha256_bytes(canonical(teardown)))

    def test_broker_rejects_tampered_trigger_without_running_rna(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config, request, request_path, _, _ = self.fixture(root)
            config_sha256 = "1" * 64
            trigger = {
                "schema_version": BROKER_TRIGGER_SCHEMA,
                "config_sha256": config_sha256,
                "request_id": request["request_id"],
                "request_sha256": "0" * 64,
                "claimed_request": str(request_path),
            }
            path = (
                Path(config["trusted_rna_broker_request_directory"])
                / f"{request['request_id']}.json"
            )
            path.write_bytes(canonical(trigger))
            path.chmod(0o600)
            with mock.patch.object(
                trusted_rna_broker.bash_gateway, "execute"
            ) as execute:
                outcome = trusted_rna_broker.process_trigger(
                    path, config=config, config_sha256=config_sha256
                )
            execute.assert_not_called()
            self.assertEqual(
                outcome,
                "trusted_rna_broker_claimed_request_digest_mismatch",
            )
            failure = json.loads(
                (
                    Path(config["gateway_receipt_directory"])
                    / f"{request['request_id']}.json"
                ).read_bytes()
            )
            self.assertEqual(failure["status"], "violation")
            self.assertEqual(
                failure["violation"]["code"],
                "trusted_rna_broker_claimed_request_digest_mismatch",
            )

    def test_broker_rejects_any_inherited_credential_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            config, _, _, _, _ = self.fixture(root)
            with mock.patch.dict(
                os.environ,
                {"ANTHROPIC_API_KEY": "not-observed"},
                clear=True,
            ), self.assertRaises(IsolationViolation) as raised:
                trusted_rna_broker.serve(config, "2" * 64)
            self.assertEqual(
                raised.exception.code,
                "trusted_rna_broker_environment_mismatch",
            )
            self.assertFalse(
                Path(config["trusted_rna_broker_ready"]).exists()
            )


if __name__ == "__main__":
    unittest.main()
