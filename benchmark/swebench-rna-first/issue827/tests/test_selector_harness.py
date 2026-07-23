from __future__ import annotations

from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import common_supervisor
import live_identity
import provider_usage
import run_selector
import select_cases
import select_result
import verify_selector


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def ref(path: Path) -> dict:
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": sha(data)}


class WrapperFixture:
    def __init__(self, root: Path):
        self.root = root
        self.harness = root / "harness"
        self.bin = self.harness / "bin"
        self.config_dir = self.harness / "config"
        self.checkout = root / "edit"
        self.repo = root / "index"
        self.evidence = root / "evidence"
        self.bin.mkdir(parents=True)
        self.config_dir.mkdir(parents=True)
        self.checkout.mkdir()
        (self.repo / ".oh/.cache").mkdir(parents=True)
        (self.repo / ".oh/.cache/cache.bin").write_bytes(b"cache")
        (self.repo / ".gitignore").write_text(".oh/.cache/\n")
        (self.repo / "tracked.py").write_text("value = 1\n")
        subprocess.run(["git", "-C", str(self.repo), "init", "-q"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "remote", "add", "origin", "https://github.com/repo/repo.git"],
            check=True,
        )
        subprocess.run(["git", "-C", str(self.repo), "config", "user.name", "Fixture"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "config", "user.email", "fixture@example.invalid"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "add", ".gitignore", "tracked.py"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qm", "fixture"], check=True)
        self.base_commit = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            stdout=subprocess.PIPE, check=True,
        ).stdout.decode().strip()
        self.base_tree = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD^{tree}"],
            stdout=subprocess.PIPE, check=True,
        ).stdout.decode().strip()
        self.cache_inventory_sha256 = run_selector.cache_inventory_sha256(self.repo / ".oh/.cache")
        self.evidence.mkdir()
        for name in (
            "rna_query.py",
            "rna_traverse.py",
            "frontier_replay.py",
            "tool_supervisor.py",
            "live_identity.py",
            "isolation.py",
        ):
            target = self.bin / name
            shutil.copyfile(HERE / name, target)
            target.chmod(0o755)
        self.launcher = root / "launcher.py"
        self.launcher.write_text(
            """#!/usr/bin/env python3
import os, sys
mode = os.environ.get('FAKE_MODE', 'nonempty')
if mode == 'error':
    print('fake failure', file=sys.stderr)
    raise SystemExit(9)
ready = '`status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false`'
if mode == 'no_metal':
    ready = '`status=READY embeddings=true retrieval=hybrid rerank=true metal=false fallback=false`'
if '--node' in sys.argv:
    node = sys.argv[sys.argv.index('--node') + 1]
    if mode == 'empty':
        print(f'No neighbors found for `{node}` within 1 hops.')
    else:
        print(f'## Graph neighbors (outgoing) of `{node}`')
        print('- **function** `helper` `foo.py`:2-2')
        print('  `foo.py:helper:function`')
    print('*Index: 2 symbols · schema v1*')
    print('### Capability readiness')
    print('- **extracted graph / exact search**: ready — 2 symbols available without LSP or embeddings')
    if mode == 'degraded_completeness':
        print('- **benchmark per-file LSP completeness**: partial/degraded — 1/2 included files covered; 1 violation(s); digest=' + 'a' * 64)
    elif mode == 'malformed_completeness':
        print('- **benchmark per-file LSP completeness**: ready — 1/2 included files covered; 0 violation(s); digest=' + 'a' * 64)
    else:
        print('- **benchmark per-file LSP completeness**: ready — 1/1 included files covered; 0 violation(s); digest=' + 'a' * 64)
else:
    print('## Search: "Bug title"')
    print('- **function** `target` `foo.py`:1-1')
    print('  `foo.py:target:function`')
    print('### Strict semantic qualification')
    print('strict details')
    print('  `foo.py:hidden:function`')
    print('*Index: 2 symbols · schema v1*')
    print('### Capability readiness')
    print(ready)
"""
        )
        self.launcher.chmod(0o755)
        self.binary = root / "rna-binary"
        self.binary.write_bytes(b"binary")
        self.environment = root / "canonical-environment.json"
        self.environment.write_bytes(canonical({"PATH": "/usr/bin:/bin"}))
        self.cache_archive = root / "cache.tar"
        self.cache_archive.write_bytes(b"archive")
        self.cache_manifest = root / "cache-manifest.json"
        self.cache_manifest.write_bytes(canonical({
            "cache": "ok",
            "operational_cache_inventory_sha256": self.cache_inventory_sha256,
        }))
        self.cache_verification = root / "cache-verification.json"
        self.cache_verification.write_bytes(canonical({
            "verified": True,
            "operational_cache_inventory_sha256": self.cache_inventory_sha256,
        }))
        self.readiness = root / "readiness.json"
        self.readiness.write_bytes(canonical({
            "status": "READY",
            "readiness": {
                "ready": True,
                "compatibility_violations": [],
                "report_digest": "a" * 64,
            },
        }))
        self.identity = self.evidence / "identity.json"
        identity = {
            "schema_version": run_selector.IDENTITY_SCHEMA,
            "case_id": "repo__repo-1",
            "base_commit": self.base_commit,
            "base_tree": self.base_tree,
            "root": "repo-root",
            "expected_repository_identity": "repo/repo",
            "live_repository_identity": "repo/repo",
            "index_checkout": str(self.repo),
            "producer_commit": "c" * 40,
            "launcher_path": str(self.launcher),
            "launcher_sha256": sha(self.launcher.read_bytes()),
            "binary_path": str(self.binary),
            "binary_sha256": sha(self.binary.read_bytes()),
            "canonical_environment": ref(self.environment),
            "canonical_environment_sha256": sha(self.environment.read_bytes()),
            "cache_archive": ref(self.cache_archive),
            "cache_archive_sha256": sha(self.cache_archive.read_bytes()),
            "cache_manifest": ref(self.cache_manifest),
            "cache_manifest_sha256": sha(self.cache_manifest.read_bytes()),
            "operational_cache_inventory_sha256": self.cache_inventory_sha256,
            "cache_verification_receipt": ref(self.cache_verification),
            "readiness_report": ref(self.readiness),
            "cache_bindings_verified": True,
            "fresh_reopen_ready": True,
            "readiness_sentinel": run_selector.READY_SENTINEL,
        }
        self.identity.write_bytes(canonical(identity))
        self.wrapper = self.bin / "rna_traverse.py"
        self.query = self.bin / "rna_query.py"
        self.tool = self.bin / "tool_supervisor.py"
        (self.harness / "title-query.txt").write_bytes(b"Bug title\n")
        projection = (
            b"## Search: \"Bug title\"\n"
            b"- **function** `target` `foo.py`:1-1\n"
            b"  `foo.py:target:function`\n\n"
            b"`status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false`\n"
        )
        (self.evidence / "query").mkdir()
        (self.evidence / "query/projection.stdout").write_bytes(projection)
        self.config = {
            "schema_version": "issue827-supervisor-config-v4",
            "policy": "treatment",
            "launcher": str(self.launcher),
            "binary": str(self.binary),
            "trusted_rna_environment": str(self.environment),
            "repo": str(self.repo),
            "checkout": str(self.checkout),
            "root": "repo-root",
            "expected_repository_identity": "repo/repo",
            "initial_response": str(self.evidence / "query/projection.stdout"),
            "initial_response_sha256": sha(projection),
            "initial_ids": ["foo.py:target:function"],
            "initial_authorization_sha256": sha(canonical(live_identity.derive_projection_authorization(projection))),
            "wrapper": str(self.wrapper),
            "query_wrapper": str(self.query),
            "harness_root": str(self.harness),
            "episode_evidence_root": str(self.evidence),
            "state": str(self.evidence / "supervisor-state.json"),
            "common_state": str(self.evidence / "common-state.json"),
            "lock": str(self.evidence / "supervisor.lock"),
            "common_lock": str(self.evidence / "common.lock"),
            "hook_ledger": str(self.evidence / "treatment.jsonl"),
            "common_hook_ledger": str(self.evidence / "common.jsonl"),
            "rna_events": str(self.evidence / "rna-events"),
            "query_events": str(self.evidence / "query"),
            "identity_receipt": str(self.identity),
            "expected_identity_sha256": sha(self.identity.read_bytes()),
            "expected_identity_schema": run_selector.IDENTITY_SCHEMA,
            "expected_base_commit": self.base_commit,
            "expected_base_tree": self.base_tree,
            "expected_producer_commit": "c" * 40,
            "expected_cache_manifest_sha256": sha(self.cache_manifest.read_bytes()),
            "expected_cache_archive_sha256": sha(self.cache_archive.read_bytes()),
            "expected_cache_inventory_sha256": self.cache_inventory_sha256,
            "expected_launcher_sha256": sha(self.launcher.read_bytes()),
            "expected_binary_sha256": sha(self.binary.read_bytes()),
            "expected_canonical_environment_sha256": sha(
                self.environment.read_bytes()
            ),
            "expected_query_sha256": sha(b"Bug title"),
            "result_limit": 10,
        }
        self.write_config()

    def write_config(self):
        (self.config_dir / "supervisor.json").write_bytes(canonical(self.config))

    def invoke(self, script: Path, args: list[str], *, mode: str = "nonempty", event: dict | None = None):
        env = {**os.environ, "FAKE_MODE": mode}
        return subprocess.run(
            [str(script), *args],
            input=canonical(event) if event is not None else None,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            check=False,
        )


class CommonSupervisorTests(unittest.TestCase):
    def config(self, root: Path, policy: str) -> dict:
        checkout = root / "checkout"
        checkout.mkdir(exist_ok=True)
        return {
            "schema_version": "issue827-supervisor-config-v4",
            "policy": policy,
            "checkout": str(checkout.resolve()),
            "repo": str(root / "index"),
            "launcher": str(root / "launcher"),
            "binary": str(root / "binary"),
            "query_wrapper": str(root / "query"),
            "wrapper": str(root / "wrapper"),
            "harness_root": str(root / "harness"),
            "episode_evidence_root": str(root / "evidence"),
            "identity_receipt": str(root / "identity"),
            "common_hook_ledger": str(root / f"{policy}.jsonl"),
            "common_state": str(root / f"{policy}-state.json"),
            "native_read_roots": [str(checkout.resolve())],
            "native_write_roots": [str(checkout.resolve())],
            "isolation_ledger": str(root / f"{policy}-isolation.jsonl"),
        }

    def test_identical_confinement_for_control_and_treatment(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            outside = root / "outside.py"
            outside.write_text("x")
            decisions = []
            for policy in ("control", "treatment"):
                config = self.config(root, policy)
                event = {
                    "hook_event_name": "PreToolUse",
                    "tool_name": "Read",
                    "tool_input": {"file_path": str(outside)},
                    "cwd": config["checkout"],
                }
                with redirect_stdout(io.StringIO()):
                    common_supervisor.handle(event, config)
                record = json.loads(Path(config["common_hook_ledger"]).read_text())
                decisions.append((record["decision"], record["reason"]))
            self.assertEqual(decisions[0], decisions[1])
            self.assertEqual(decisions[0][0], "deny")

    def test_inside_checkout_allowed_for_both(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for policy in ("control", "treatment"):
                config = self.config(root, policy)
                target = Path(config["checkout"]) / "ok.py"
                event = {
                    "hook_event_name": "PreToolUse", "tool_name": "Edit",
                    "tool_input": {"file_path": str(target)}, "cwd": config["checkout"],
                }
                common_supervisor.handle(event, config)
                record = json.loads(Path(config["common_hook_ledger"]).read_text())
                self.assertEqual(record["decision"], "allow")

    def test_unregistered_gateway_fails_closed_for_both_arms(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for policy in ("control", "treatment"):
                config = self.config(root, policy)
                command = f"{config['wrapper']} --node 'foo.py:x:function' --mode neighbors"
                event = {
                    "hook_event_name": "PreToolUse", "tool_name": "Bash",
                    "tool_input": {"command": command}, "cwd": config["checkout"],
                }
                with redirect_stdout(io.StringIO()):
                    common_supervisor.handle(event, config)
                record = json.loads(Path(config["common_hook_ledger"]).read_text())
                self.assertEqual(record["decision"], "deny")


class RnaWrapperTests(unittest.TestCase):
    def fixture(self, tmp: str) -> WrapperFixture:
        return WrapperFixture(Path(tmp))

    def first_pre_event(self, fixture: WrapperFixture, command: str) -> dict:
        return {
            "hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_input": {"command": command}, "cwd": str(fixture.checkout),
        }

    def test_query_retains_raw_and_requires_metal_and_code_id(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            result = fixture.invoke(fixture.query, ["--query-sha256", fixture.config["expected_query_sha256"]])
            self.assertEqual(result.returncode, 0, result.stderr)
            receipt = json.loads((fixture.evidence / "query/title-query.json").read_text())
            self.assertEqual(
                receipt["projection_authorization"]["stable_code_ids"],
                ["foo.py:target:function"],
            )
            self.assertEqual(
                receipt["raw_stable_code_ids_observational_only"],
                ["foo.py:hidden:function", "foo.py:target:function"],
            )
            self.assertEqual(Path(receipt["stdout"]["path"]).read_bytes().startswith(b"## Search"), True)
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            result = fixture.invoke(fixture.query, ["--query-sha256", fixture.config["expected_query_sha256"]], mode="no_metal")
            self.assertEqual(result.returncode, 42)
            self.assertIn(b"readiness", result.stderr)

    def test_first_neighbors_accepts_nonempty_and_empty(self):
        for mode, expected in (("nonempty", "OK_NONEMPTY"), ("empty", "OK_EMPTY")):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                fixture = self.fixture(tmp)
                result = fixture.invoke(fixture.wrapper, ["--node", "foo.py:target:function", "--mode", "neighbors"], mode=mode)
                self.assertEqual(result.returncode, 0, result.stderr)
                state = json.loads(Path(fixture.config["state"]).read_text())
                self.assertEqual(state["first_traversal_status"], expected)
                raw_node_output = (fixture.evidence / "rna-events/0001.stdout").read_text()
                self.assertNotIn(run_selector.READY_SENTINEL, raw_node_output)
                receipt = json.loads((fixture.evidence / "rna-events/0001.json").read_text())
                self.assertEqual(receipt["benchmark_completeness"], {
                    "status": "ready",
                    "covered_files": 1,
                    "total_files": 1,
                    "violations": 0,
                    "report_digest": "a" * 64,
                })

    def test_node_traversal_rejects_degraded_or_incomplete_benchmark_coverage(self):
        for mode, reason in (
            ("degraded_completeness", "benchmark_completeness_not_ready"),
            ("malformed_completeness", "benchmark_completeness_coverage_mismatch"),
        ):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                fixture = self.fixture(tmp)
                result = fixture.invoke(
                    fixture.wrapper,
                    ["--node", "foo.py:target:function", "--mode", "neighbors"],
                    mode=mode,
                )
                self.assertEqual(result.returncode, 43)
                self.assertIn(reason.encode(), result.stderr)
                state = json.loads(Path(fixture.config["state"]).read_text())
                self.assertTrue(state["fatal"])

    def test_uninjected_first_fails_and_freezes_later_tools(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            result = fixture.invoke(fixture.wrapper, ["--node", "foo.py:other:function", "--mode", "neighbors"])
            self.assertEqual(result.returncode, 43)
            later = {
                "hook_event_name": "PreToolUse", "tool_name": "Read",
                "tool_input": {"file_path": str(fixture.checkout / "foo.py")}, "cwd": str(fixture.checkout),
            }
            denied = fixture.invoke(fixture.tool, [], event=later)
            self.assertEqual(denied.returncode, 0)
            self.assertIn(b'"permissionDecision": "deny"', denied.stdout)

    def test_traversal_frontier_expands_only_from_visible_graph_projection(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            first = fixture.invoke(
                fixture.wrapper,
                [
                    "--node",
                    "foo.py:target:function",
                    "--mode",
                    "neighbors",
                ],
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            state = json.loads(Path(fixture.config["state"]).read_text())
            frontier = state["authorization_frontier"]
            self.assertEqual(
                frontier["authorized_stable_code_ids"],
                [
                    "foo.py:helper:function",
                    "foo.py:target:function",
                ],
            )
            self.assertEqual(
                len(frontier["sources"]),
                2,
            )
            body = {
                key: value
                for key, value in frontier.items()
                if key != "authorization_frontier_sha256"
            }
            self.assertEqual(
                frontier["authorization_frontier_sha256"],
                sha(canonical(body)),
            )
            first_receipt = json.loads(
                (fixture.evidence / "rna-events/0001.json").read_text()
            )
            self.assertEqual(
                first_receipt["emitted_projection_authorization"][
                    "stable_code_ids"
                ],
                [
                    "foo.py:helper:function",
                    "foo.py:target:function",
                ],
            )
            self.assertEqual(
                first_receipt["authorization_frontier_after"],
                frontier,
            )
            projection_ref = first_receipt["model_visible_projection"]
            projection_path = Path(projection_ref["path"])
            self.assertEqual(
                projection_path.read_bytes(),
                first.stdout,
            )
            self.assertEqual(
                sha(projection_path.read_bytes()),
                projection_ref["sha256"],
            )

            second = fixture.invoke(
                fixture.wrapper,
                [
                    "--node",
                    "foo.py:helper:function",
                    "--mode",
                    "impact",
                ],
            )
            self.assertEqual(second.returncode, 0, second.stderr)
            second_receipt = json.loads(
                (fixture.evidence / "rna-events/0002.json").read_text()
            )
            self.assertEqual(
                second_receipt["requested_node_authorized_by"],
                [
                    {
                        "source_sequence": 1,
                        "source_kind": "rna_traversal_projection",
                        "classification": "OK_NONEMPTY",
                        "projection_sha256": first_receipt[
                            "projection_sha256"
                        ],
                        "model_visible_projection_sha256": projection_ref[
                            "sha256"
                        ],
                        "projection_authorization_sha256": first_receipt[
                            "emitted_projection_authorization_sha256"
                        ],
                    }
                ],
            )

    def test_projection_external_guess_after_success_is_terminal(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            first = fixture.invoke(
                fixture.wrapper,
                [
                    "--node",
                    "foo.py:target:function",
                    "--mode",
                    "neighbors",
                ],
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            guessed = fixture.invoke(
                fixture.wrapper,
                [
                    "--node",
                    "foo.py:guessed:function",
                    "--mode",
                    "neighbors",
                ],
            )
            self.assertEqual(guessed.returncode, 43)
            self.assertIn(
                b"node_not_in_authorization_frontier",
                guessed.stderr,
            )
            state = json.loads(Path(fixture.config["state"]).read_text())
            self.assertTrue(state["fatal"])
            self.assertEqual(
                state["fatal_reason"],
                "node_not_in_authorization_frontier",
            )
            receipt = json.loads(
                (fixture.evidence / "rna-events/0002.json").read_text()
            )
            self.assertEqual(receipt["requested_node_authorized_by"], [])
            self.assertEqual(receipt["classification"], "ERROR")

    def test_empty_traversal_does_not_expand_frontier(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            empty = fixture.invoke(
                fixture.wrapper,
                [
                    "--node",
                    "foo.py:target:function",
                    "--mode",
                    "neighbors",
                ],
                mode="empty",
            )
            self.assertEqual(empty.returncode, 0, empty.stderr)
            state = json.loads(Path(fixture.config["state"]).read_text())
            frontier = state["authorization_frontier"]
            self.assertEqual(
                frontier["authorized_stable_code_ids"],
                ["foo.py:target:function"],
            )
            receipt = json.loads(
                (fixture.evidence / "rna-events/0001.json").read_text()
            )
            self.assertEqual(
                receipt["emitted_projection_authorization"][
                    "stable_code_ids"
                ],
                [],
            )
            guessed = fixture.invoke(
                fixture.wrapper,
                [
                    "--node",
                    "foo.py:helper:function",
                    "--mode",
                    "neighbors",
                ],
            )
            self.assertEqual(guessed.returncode, 43)
            self.assertIn(
                b"node_not_in_authorization_frontier",
                guessed.stderr,
            )

    def test_frontier_state_or_projection_tamper_fails_closed(self):
        for tamper in ("state", "projection"):
            with self.subTest(tamper=tamper), tempfile.TemporaryDirectory() as tmp:
                fixture = self.fixture(tmp)
                first = fixture.invoke(
                    fixture.wrapper,
                    [
                        "--node",
                        "foo.py:target:function",
                        "--mode",
                        "neighbors",
                    ],
                )
                self.assertEqual(first.returncode, 0, first.stderr)
                if tamper == "state":
                    state_path = Path(fixture.config["state"])
                    state = json.loads(state_path.read_text())
                    state["authorization_frontier"][
                        "authorized_stable_code_ids"
                    ].append("foo.py:guessed:function")
                    state_path.write_bytes(canonical(state))
                    expected = b"authorization_frontier_hash_or_union"
                else:
                    projection = (
                        fixture.evidence
                        / "rna-events/0001.projection"
                    )
                    projection.write_bytes(
                        projection.read_bytes() + b"tampered\n"
                    )
                    expected = b"authorization_frontier_projection_tampered"
                second = fixture.invoke(
                    fixture.wrapper,
                    [
                        "--node",
                        "foo.py:helper:function",
                        "--mode",
                        "neighbors",
                    ],
                )
                self.assertEqual(second.returncode, 43)
                self.assertIn(expected, second.stderr)
                state = json.loads(Path(fixture.config["state"]).read_text())
                self.assertTrue(state["fatal"])

    def test_chained_first_denied_and_ordinary_tool_allowed_after_valid(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            exact = f"{fixture.wrapper} --node 'foo.py:target:function' --mode neighbors"
            bad = self.first_pre_event(fixture, exact + " && true")
            denied = fixture.invoke(fixture.tool, [], event=bad)
            self.assertIn(b'"permissionDecision": "deny"', denied.stdout)
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            exact = f"{fixture.wrapper} --node 'foo.py:target:function' --mode neighbors"
            first = fixture.invoke(fixture.tool, [], event=self.first_pre_event(fixture, exact))
            self.assertEqual(first.stdout, b"")
            traverse = fixture.invoke(fixture.wrapper, ["--node", "foo.py:target:function", "--mode", "neighbors"])
            self.assertEqual(traverse.returncode, 0)
            ordinary = {
                "hook_event_name": "PreToolUse", "tool_name": "Read",
                "tool_input": {"file_path": str(fixture.checkout / "foo.py")}, "cwd": str(fixture.checkout),
            }
            allowed = fixture.invoke(fixture.tool, [], event=ordinary)
            self.assertEqual(allowed.stdout, b"")
            records = [json.loads(line) for line in Path(fixture.config["hook_ledger"]).read_text().splitlines()]
            self.assertEqual(records[-1]["decision"], "allow")

    def test_launcher_error_freezes_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            result = fixture.invoke(fixture.wrapper, ["--node", "foo.py:target:function", "--mode", "neighbors"], mode="error")
            self.assertEqual(result.returncode, 43)
            state = json.loads(Path(fixture.config["state"]).read_text())
            self.assertTrue(state["fatal"])

    def test_live_cache_or_checkout_drift_fails_before_rna(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            (fixture.repo / ".oh/.cache/cache.bin").write_bytes(b"tampered")
            result = fixture.invoke(
                fixture.query,
                ["--query-sha256", fixture.config["expected_query_sha256"]],
            )
            self.assertEqual(result.returncode, 42)
            self.assertIn(b"live_cache_inventory", result.stderr)
        with tempfile.TemporaryDirectory() as tmp:
            fixture = self.fixture(tmp)
            (fixture.repo / "tracked.py").write_text("value = 2\n")
            result = fixture.invoke(
                fixture.query,
                ["--query-sha256", fixture.config["expected_query_sha256"]],
            )
            self.assertEqual(result.returncode, 42)
            self.assertIn(b"live_checkout_not_pristine", result.stderr)

    def test_live_origin_mutation_after_preflight_fails_every_rna_wrapper(self):
        invocations = (
            ("query", ["--query-sha256", None], 42),
            ("wrapper", ["--node", "foo.py:target:function", "--mode", "neighbors"], 43),
        )
        for attribute, args, expected_returncode in invocations:
            with self.subTest(wrapper=attribute), tempfile.TemporaryDirectory() as tmp:
                fixture = self.fixture(tmp)
                subprocess.run(
                    [
                        "git", "-C", str(fixture.repo), "remote", "set-url", "origin",
                        "https://github.com/other/repository.git",
                    ],
                    check=True,
                )
                actual_args = [
                    fixture.config["expected_query_sha256"] if item is None else item
                    for item in args
                ]
                result = fixture.invoke(getattr(fixture, attribute), actual_args)
                self.assertEqual(result.returncode, expected_returncode)
                self.assertIn(b"live_repository_identity", result.stderr)


class RunnerAndVerifierTests(unittest.TestCase):
    def test_failed_acquisition_retains_exact_command_and_wrapper_streams(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            wrapper = root / "rna_query.py"
            wrapper.write_text(
                "#!/usr/bin/env python3\nimport sys\nprint('partial projection')\n"
                "print('RNA_QUERY_STATUS=ERROR readiness', file=sys.stderr)\nraise SystemExit(42)\n"
            )
            wrapper.chmod(0o755)
            evidence = root / "evidence"
            profile = root / "trusted-rna.sb"
            profile.write_text("(version 1)\n(allow default)\n")
            environment = root / "canonical-environment.json"
            environment.write_text("{}\n")
            config = {
                "expected_query_sha256": "a" * 64,
                "sandbox_exec": str(Path(sys.executable).resolve()),
                "trusted_rna_seatbelt_profile": str(profile),
                "gateway_python": str(Path(sys.executable).resolve()),
                "checkout": str(root),
                "trusted_rna_env": {
                    "PATH": "/usr/bin:/bin",
                    "HOME": str(root),
                },
                "trusted_rna_timeout_seconds": 5,
                "trusted_rna_environment": str(environment),
                "trusted_rna_read_roots": [str(root)],
                "trusted_rna_write_roots": [str(root)],
            }
            completed = subprocess.CompletedProcess(
                [],
                42,
                b"partial projection\n",
                b"RNA_QUERY_STATUS=ERROR readiness\n",
            )
            with mock.patch.object(
                run_selector.subprocess, "run", return_value=completed
            ), self.assertRaises(
                run_selector.TreatmentAcquisitionFailure
            ) as failure:
                run_selector.acquire_treatment(
                    SimpleNamespace(case_id="repo__repo-1", root="repo-root"),
                    {"rna_query.py": wrapper},
                    evidence,
                    config,
                )
            retained = failure.exception.evidence
            self.assertEqual(retained["schema_version"], run_selector.QUERY_EVIDENCE_SCHEMA)
            self.assertFalse(retained["succeeded"])
            self.assertEqual(
                retained["wrapper_command"],
                [
                    str(Path(sys.executable).resolve()),
                    "-f",
                    str(profile),
                    str(Path(sys.executable).resolve()),
                    str(wrapper),
                    "--query-sha256",
                    "a" * 64,
                ],
            )
            self.assertEqual(
                retained["requested_wrapper_command"],
                [str(wrapper), "--query-sha256", "a" * 64],
            )
            self.assertEqual(
                retained["trusted_rna_confinement"]["network_outbound"],
                "denied",
            )
            self.assertEqual(retained["wrapper_returncode"], 42)
            self.assertEqual(run_selector.check_ref(retained["wrapper_stdout"], "stdout")[1], b"partial projection\n")
            self.assertEqual(
                run_selector.check_ref(retained["wrapper_stderr"], "stderr")[1],
                b"RNA_QUERY_STATUS=ERROR readiness\n",
            )

    def test_index_repository_identity_rejects_local_origin_and_accepts_github_urls(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            checkout = root / "index"
            checkout.mkdir()
            subprocess.run(["git", "-C", str(checkout), "init", "-q"], check=True)
            subprocess.run(
                ["git", "-C", str(checkout), "remote", "add", "origin", str(root / "seed")],
                check=True,
            )
            manifest = {"core": {"repository": "django/django"}}
            with self.assertRaises(run_selector.FailClosed) as failure:
                run_selector.verify_index_repository_identity(
                    checkout,
                    manifest,
                    "django__django-16379.cache.index_checkout",
                )
            self.assertEqual(
                str(failure.exception),
                "django__django-16379.cache.index_checkout.origin is not a canonical GitHub HTTPS or SSH URL",
            )

            for origin in (
                "https://github.com/django/django.git",
                "git@github.com:django/django.git",
                "ssh://git@github.com/django/django.git",
            ):
                subprocess.run(
                    ["git", "-C", str(checkout), "remote", "set-url", "origin", origin],
                    check=True,
                )
                expected, live = run_selector.verify_index_repository_identity(
                    checkout,
                    manifest,
                    "django__django-16379.cache.index_checkout",
                )
                self.assertEqual(expected, "django/django")
                self.assertEqual(live, "django/django")

    def test_claude_command_is_frozen_and_differs_only_by_treatment_system(self):
        runtime = {
            "model": "claude-sonnet-5", "effort": "high", "permission_mode": "dontAsk",
            "tools": ["Bash", "Edit", "Read", "Write", "Glob", "Grep"],
            "disallowed_tools": ["WebSearch", "WebFetch"], "budget_usd": 6.0,
        }
        prepared = SimpleNamespace(
            registration={"model_runtime": runtime},
            claude_path=Path("/exact/claude"),
            mcp_path=Path("/strict-empty.json"),
        )
        a = run_selector.claude_command(prepared, "00000000-0000-4000-8000-000000000000", Path("/settings"), None)
        t = run_selector.claude_command(prepared, "00000000-0000-4000-8000-000000000000", Path("/settings"), Path("/system"))
        self.assertNotIn("--safe-mode", a)
        self.assertNotIn("--resume", a)
        self.assertEqual(t[:-2], a)
        self.assertEqual(t[-2:], ["--append-system-prompt-file", "/system"])

    def test_provider_parent_env_confines_all_claude_temp_variables(self):
        with tempfile.TemporaryDirectory() as tmp:
            model_private = Path(tmp) / "private"
            (model_private / "tmp").mkdir(parents=True)
            with mock.patch.dict(
                os.environ,
                {"CLAUDE_CODE_OAUTH_TOKEN": "not-recorded", "TMPDIR": "/outside"},
                clear=True,
            ):
                env = run_selector.provider_parent_env(model_private)
            expected = str((model_private / "tmp").resolve())
            self.assertEqual(env["CLAUDE_CODE_OAUTH_TOKEN"], "not-recorded")
            for name in (
                "TMPDIR",
                "CLAUDE_TMPDIR",
                "CLAUDE_CODE_TMPDIR",
                "BUN_TMPDIR",
            ):
                self.assertEqual(env[name], expected)

    def test_token_ledger_prefers_whole_invocation_model_usage_without_double_counting(self):
        summary = {
            "usage": {
                "input_tokens": 13,
                "cache_creation_input_tokens": 18,
                "cache_read_input_tokens": 24,
                "output_tokens": 22,
                "reasoning_tokens": 34,
            },
            "modelUsage": {
                "claude-sonnet": {
                    "inputTokens": 11,
                    "cacheCreationInputTokens": 13,
                    "cacheReadInputTokens": 17,
                    "outputTokens": 19,
                    "reasoningTokens": 23,
                },
                "claude-haiku": {
                    "inputTokens": 2,
                    "cacheCreationInputTokens": 5,
                    "cacheReadInputTokens": 7,
                    "outputTokens": 3,
                    "reasoningTokens": 11,
                },
            },
            "num_turns": 3,
        }
        ledger = run_selector.token_ledger(summary)
        self.assertTrue(ledger["valid"])
        self.assertEqual(ledger["schema_version"], provider_usage.SCHEMA_VERSION)
        self.assertEqual(ledger["source"], "whole_invocation_model_usage")
        self.assertEqual(ledger["input_tokens"], 13)
        self.assertEqual(ledger["cache_creation_input_tokens"], 18)
        self.assertEqual(ledger["cache_read_input_tokens"], 24)
        self.assertEqual(ledger["output_tokens"], 22)
        self.assertEqual(ledger["provider_total_tokens"], 77)
        self.assertEqual(ledger["reasoning_tokens"], 34)
        self.assertEqual(ledger["cli_turns"], 3)
        self.assertIsNone(ledger["provider_requests"])

    def test_token_ledger_rejects_present_malformed_optional_counter(self):
        ledger = run_selector.token_ledger({
            "usage": {
                "input_tokens": 10,
                "output_tokens": 4,
                "cache_read_input_tokens": -1,
            },
            "num_turns": 2,
        })
        self.assertFalse(ledger["valid"])
        self.assertIsNone(ledger["provider_total_tokens"])

    def test_provider_responses_are_independently_counted_from_retained_transcript(self):
        with tempfile.TemporaryDirectory() as tmp:
            transcript = Path(tmp) / "session.jsonl"
            transcript.write_bytes(b"\n".join((
                canonical({"type": "assistant", "message": {"role": "assistant", "id": "msg-1", "usage": {"input_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1, "reasoning_tokens": 0}}}).rstrip(),
                canonical({"type": "assistant", "message": {"role": "assistant", "id": "msg-1", "usage": {"input_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1, "reasoning_tokens": 0}}}).rstrip(),
                canonical({"type": "user", "message": {"role": "user"}}).rstrip(),
                canonical({"type": "assistant", "message": {"role": "assistant", "id": "msg-2", "usage": {"input_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1, "reasoning_tokens": 0}}}).rstrip(),
            )) + b"\n")
            observed = run_selector.transcript_provider_response_count([
                {"source_path": "/private/source", "retained": ref(transcript)},
            ])
            self.assertEqual(observed, 2)
            ledger = run_selector.token_ledger(
                {"usage": {"input_tokens": 2, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 2, "reasoning_tokens": 0}, "num_turns": 4},
                model_events=run_selector.transcript_model_events([{"retained": ref(transcript)}]),
                provider_responses=observed,
            )
            self.assertEqual(ledger["cli_turns"], 4)
            self.assertEqual(ledger["provider_responses"], 2)
            self.assertIsNone(ledger["provider_requests"])

    def test_token_ledger_rejects_top_level_usage_without_whole_invocation_usage(self):
        ledger = run_selector.token_ledger({
            "usage": {
                "input_tokens": 10,
                "cache_creation_input_tokens": 5,
                "cache_read_input_tokens": 7,
                "output_tokens": 4,
                "reasoning_tokens": 8,
            },
            "num_turns": 2,
        })
        self.assertFalse(ledger["valid"])
        self.assertIn("modelUsage_missing", ledger["errors"])
        self.assertIsNone(ledger["provider_total_tokens"])
        self.assertEqual(ledger["top_level_usage"]["provider_total_tokens"], 26)
        self.assertEqual(ledger["top_level_usage"]["reasoning_tokens"], 8)

    def test_missing_token_usage_is_invalid_not_zero(self):
        ledger = run_selector.token_ledger({"valid_json": True, "num_turns": 2})
        self.assertFalse(ledger["valid"])
        self.assertTrue(ledger["model_invoked"])
        self.assertIsNone(ledger["input_tokens"])
        self.assertIsNone(ledger["output_tokens"])
        self.assertIsNone(ledger["provider_total_tokens"])

    def test_pre_model_absence_is_valid_only_in_noncompliant_no_model_context(self):
        ledger = run_selector.token_ledger(
            {"num_turns": 0}, model_invoked=False,
            provider_responses=0, provider_requests=0,
        )
        self.assertTrue(ledger["valid"])
        self.assertFalse(ledger["model_invoked"])
        self.assertIsNone(ledger["provider_total_tokens"])
        self.assertEqual(ledger["cli_turns"], 0)
        self.assertEqual(ledger["provider_responses"], 0)
        receipt = {
            "returncode": None,
            "policy_compliant": False,
            "evaluator_authorized": False,
            "timing_ledger": {"model_wall_seconds": 0.0},
        }
        errors = []
        verify_selector.validate_token_ledger(ledger, receipt, errors)
        self.assertEqual(errors, [])

        invoked_without_usage = run_selector.token_ledger(
            {"valid_json": True, "num_turns": 1},
            model_invoked=True,
        )
        errors = []
        verify_selector.validate_token_ledger(invoked_without_usage, receipt, errors)
        self.assertIn("token_usage_not_observed", errors)
        self.assertIn("token_provider_total_tokens_invalid", errors)

        errors = []
        verify_selector.validate_token_ledger(
            ledger,
            {**receipt, "policy_compliant": True},
            errors,
        )
        self.assertEqual(errors, ["token_no_model_context_invalid"])

    def test_dry_run_never_calls_execute(self):
        prepared = object()
        with mock.patch.object(run_selector, "prepare", return_value=prepared), \
             mock.patch.object(run_selector, "preflight_summary", return_value={"models_launched": 0}), \
             mock.patch.object(run_selector, "execute") as execute:
            code = run_selector.main(["run", "--manifest", "/does/not/matter"])
        self.assertEqual(code, 0)
        execute.assert_not_called()

    def test_tampered_ref_and_missing_run_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            path = root / "evidence"
            path.write_bytes(b"before")
            frozen = ref(path)
            path.write_bytes(b"after")
            with self.assertRaises(run_selector.FailClosed):
                run_selector.check_ref(frozen, "tampered")
            aggregate = verify_selector.verify_run(root)
            self.assertIn("expected_four_episode_receipts_found_0", aggregate["errors"])

    def test_reordered_actor_actions_are_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            actor = root / "actor.json"
            actor.write_bytes(canonical({
                "schema_version": "issue827-actor-tool-ledger-v1", "arm": "A",
                "actions": [{"sequence": 2, "actor": "model"}, {"sequence": 1, "actor": "harness"}],
            }))
            receipt = root / "episode-receipt.json"
            receipt.write_bytes(canonical({
                "schema_version": run_selector.RECEIPT_SCHEMA,
                "case_id": "repo__repo-1", "rank": 1, "arm": "A", "policy": "control",
                "run_manifest": None, "registration": None, "selection": None,
                "runtime_identity": None, "prompt": None, "command": None,
                "treatment_system": None, "query_evidence": None,
                "stdout": None, "stderr": None, "transcripts": [],
                "actor_tool_ledger": ref(actor), "supervisor": {},
                "token_ledger": {"schema_version": provider_usage.SCHEMA_VERSION, "valid": True,
                    "errors": [], "source": "top_level_usage", "model_invoked": True,
                    "input_tokens": 1,
                    "output_tokens": 1, "provider_total_tokens": 2,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
                    "reasoning_tokens": 0, "cli_turns": 0,
                    "provider_responses": 1, "provider_requests": None},
                "timing_ledger": {"rna_preprocessing_seconds": 0.0, "model_wall_seconds": 0.0,
                    "combined_pre_evaluator_wall_seconds": 0.0},
                "terminal_patch": None, "official_evaluator_invoked": False,
                "evaluator_authorized": False, "policy_compliant": False,
                "evidence_complete": True, "returncode": None, "timed_out": False, "errors": [],
            }))
            result = verify_selector.verify_episode(receipt)
            self.assertIn("actor_sequence_not_contiguous", result["errors"])

    def test_evidence_aggregator_sums_registered_efficiency_fields(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = []
            for case in (1, 2):
                for arm in ("A", "T"):
                    path = root / f"rank-{case:02d}-repo__repo-{case}" / arm / "episode-receipt.json"
                    path.parent.mkdir(parents=True)
                    path.write_text("{}")
                    paths.append(path)
            results = []
            for index, path in enumerate(paths):
                arm = path.parent.name
                results.append({
                    "case_id": path.parents[1].name, "arm": arm, "evidence_complete": True,
                    "policy_compliant": True, "evaluator_authorized": True,
                    "token_ledger": {"provider_total_tokens": 10 + index},
                    "timing_ledger": {"combined_pre_evaluator_wall_seconds": 1.5},
                })
            with mock.patch.object(verify_selector, "verify_episode", side_effect=results):
                aggregate = verify_selector.verify_run(root)
            self.assertTrue(aggregate["all_four_verifier_clean"])
            self.assertEqual(aggregate["by_arm"]["A"]["episodes"], 2)
            self.assertEqual(aggregate["by_arm"]["T"]["combined_pre_evaluator_wall_seconds"], 3.0)


class CheckoutHygieneTests(unittest.TestCase):
    def fixture(self, root: Path) -> tuple[Path, str, str]:
        checkout = root / "checkout"
        checkout.mkdir()
        subprocess.run(["git", "-C", str(checkout), "init", "-q"], check=True)
        subprocess.run(
            ["git", "-C", str(checkout), "config", "user.name", "Fixture"],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(checkout),
                "config",
                "user.email",
                "fixture@example.invalid",
            ],
            check=True,
        )
        (checkout / ".gitignore").write_text(".oh/.cache/\nbuild/\n")
        (checkout / ".oh/metis").mkdir(parents=True)
        (checkout / ".oh/metis/context.md").write_text("tracked business context\n")
        (checkout / "tracked.py").write_text("value = 1\n")
        subprocess.run(["git", "-C", str(checkout), "add", "."], check=True)
        subprocess.run(
            ["git", "-C", str(checkout), "commit", "-qm", "fixture"],
            check=True,
        )
        commit = subprocess.run(
            ["git", "-C", str(checkout), "rev-parse", "HEAD"],
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.decode().strip()
        tree = subprocess.run(
            ["git", "-C", str(checkout), "rev-parse", "HEAD^{tree}"],
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.decode().strip()
        return checkout, commit, tree

    def test_cache_checkout_allows_only_cache_material(self):
        with tempfile.TemporaryDirectory() as tmp:
            checkout, commit, tree = self.fixture(Path(tmp))
            cache = checkout / ".oh/.cache"
            cache.mkdir()
            (cache / "index.bin").write_bytes(b"cache")
            self.assertEqual(
                run_selector.verify_checkout(
                    str(checkout), commit, tree, "index", cache=True
                ),
                checkout,
            )

            (checkout / "build").mkdir()
            (checkout / "build/ignored.bin").write_bytes(b"ignored")
            with self.assertRaisesRegex(
                run_selector.FailClosed, "outside .oh/.cache"
            ):
                run_selector.verify_checkout(
                    str(checkout), commit, tree, "index", cache=True
                )

    def test_model_checkout_recheck_rejects_accidental_cache(self):
        with tempfile.TemporaryDirectory() as tmp:
            checkout, commit, tree = self.fixture(Path(tmp))
            self.assertEqual(
                run_selector.verify_checkout(
                    str(checkout), commit, tree, "arm"
                ),
                checkout,
            )
            cache = checkout / ".oh/.cache"
            cache.mkdir()
            (cache / "index.bin").write_bytes(b"accidental RNA state")
            with self.assertRaisesRegex(
                run_selector.FailClosed, "untracked or ignored material"
            ):
                run_selector.verify_checkout(
                    str(checkout), commit, tree, "arm prelaunch"
                )

    def test_model_checkout_rejects_other_untracked_and_ignored_material(self):
        for relative in ("notes.txt", "build/ignored.bin"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as tmp:
                checkout, commit, tree = self.fixture(Path(tmp))
                path = checkout / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"unexpected")
                with self.assertRaisesRegex(
                    run_selector.FailClosed, "untracked or ignored material"
                ):
                    run_selector.verify_checkout(
                        str(checkout), commit, tree, "arm"
                    )

    def test_tracked_business_context_is_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            checkout, commit, tree = self.fixture(Path(tmp))
            self.assertEqual(
                run_selector.verify_checkout(
                    str(checkout), commit, tree, "arm"
                ),
                checkout,
            )
            (checkout / ".oh/metis/context.md").write_text("mutated\n")
            with self.assertRaisesRegex(
                run_selector.FailClosed, "tracked state is not pristine"
            ):
                run_selector.verify_checkout(
                    str(checkout), commit, tree, "arm"
                )

    def test_broker_start_failure_terminates_started_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            checkout = root / "checkout"
            evidence = root / "evidence"
            (evidence / "isolation").mkdir(parents=True)
            checkout.mkdir()
            broker = root / "trusted_rna_broker.py"
            broker.write_text("# fixture\n")
            config_path = root / "supervisor.json"
            config_path.write_bytes(canonical({"fixture": True}))
            config = {
                "gateway_config": str(config_path),
                "trusted_rna_broker": str(broker),
                "trusted_rna_broker_ready": str(root / "ready.json"),
                "trusted_rna_env": {"PATH": "/usr/bin:/bin"},
                "checkout": str(checkout),
            }
            prepared = SimpleNamespace(
                isolation_host={"gateway_python": Path(sys.executable)}
            )
            process = mock.Mock(pid=31415)
            process.poll.return_value = 9
            with mock.patch.object(
                run_selector.subprocess, "Popen", return_value=process
            ), mock.patch.object(run_selector, "terminate_group") as terminate:
                with self.assertRaisesRegex(
                    run_selector.FailClosed, "exited before ready"
                ):
                    run_selector.start_trusted_rna_broker(
                        prepared, config, evidence
                    )
            terminate.assert_called_once_with(process)


class ResultAggregationAuthorityTests(unittest.TestCase):
    def winning_treatment_metrics(self) -> list[dict]:
        metrics = []
        for rank in (1, 2):
            for arm in ("A", "T"):
                metrics.append({
                    "rank": rank,
                    "arm": arm,
                    "policy_compliant": True,
                    "evidence_complete": True,
                    "outcome_valid": True,
                    "resolved": arm == "T",
                    "pass_to_pass_regressions": 0,
                    "provider_total_tokens": 100,
                    "combined_pre_evaluator_wall_seconds": 10,
                })
        return metrics

    def test_non_authoritative_qualification_can_never_select_treatment(self):
        decision = select_result.decide_for_selection(
            {
                "authoritative": False,
                "state": "postfix_setup_qualification_not_selection_evidence",
            },
            self.winning_treatment_metrics(),
        )
        self.assertEqual(decision["decision"], "no_selection")
        self.assertEqual(decision["classification"], "non_authoritative_qualification")
        self.assertFalse(decision["selection_authoritative"])
        self.assertEqual(
            decision["selection_state"],
            "postfix_setup_qualification_not_selection_evidence",
        )

    def test_authoritative_selection_still_applies_registered_decision(self):
        decision = select_result.decide_for_selection(
            {
                "authoritative": True,
                "state": "selected_pre_model",
                "problem_statements_inspected_by_human_before_selection": False,
                "gold_or_outcomes_inspected_before_selection": False,
                "fresh_case_claim": True,
                "prior_model_calls": 0,
            },
            self.winning_treatment_metrics(),
        )
        self.assertEqual(decision["decision"], "selected_T")
        self.assertEqual(decision["classification"], "efficacy_selection")
        self.assertTrue(decision["selection_authoritative"])

    def test_missing_or_malformed_authority_fails_closed(self):
        invalid = (
            {},
            {"authoritative": None},
            {"authoritative": 1},
            {"authoritative": "false"},
        )
        for selection in invalid:
            with self.subTest(selection=selection), self.assertRaises(
                select_result.evaluator.FailClosed
            ):
                select_result.decide_for_selection(
                    selection,
                    self.winning_treatment_metrics(),
                )

    def test_authoritative_selection_requires_frozen_state_and_inspection_flags(self):
        base = {
            "authoritative": True,
            "state": "selected_pre_model",
            "problem_statements_inspected_by_human_before_selection": False,
            "gold_or_outcomes_inspected_before_selection": False,
            "fresh_case_claim": True,
            "prior_model_calls": 0,
        }
        invalid_updates = (
            {"state": "postfix_setup_qualification_not_selection_evidence"},
            {"problem_statements_inspected_by_human_before_selection": True},
            {"gold_or_outcomes_inspected_before_selection": True},
        )
        for update in invalid_updates:
            with self.subTest(update=update), self.assertRaises(
                select_result.evaluator.FailClosed
            ):
                select_result.decide_for_selection(
                    {**base, **update},
                    self.winning_treatment_metrics(),
                )


class FrozenSelectorRegistrationTests(unittest.TestCase):
    def test_selector_version_and_population_match_runtime(self):
        if not (HERE / "registration.json").exists():
            self.skipTest("registration artifact is assembled by the preregistration batch")
        registration = json.loads((HERE / "registration.json").read_text())
        exclusions = json.loads((HERE / "exclusions.json").read_text())
        self.assertEqual(registration["selector"]["algorithm_version"], "issue827-selector-v1")
        self.assertEqual(registration["selector"]["excluded_rows"], exclusions["excluded_count"])
        self.assertEqual(registration["selector"]["eligible_rows"], exclusions["eligible_count"])
        self.assertEqual(len(exclusions["excluded_instance_ids"]), exclusions["excluded_count"])


if __name__ == "__main__":
    unittest.main()
