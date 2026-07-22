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
import run_selector
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
        for name in ("rna_query.py", "rna_traverse.py", "tool_supervisor.py"):
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
    print(ready)
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
        self.readiness.write_bytes(canonical({"status": "READY"}))
        self.identity = self.evidence / "identity.json"
        identity = {
            "schema_version": "issue825-runtime-identity-v2",
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
            "schema_version": "rna-supervisor-config-v3",
            "policy": "treatment",
            "launcher": str(self.launcher),
            "binary": str(self.binary),
            "repo": str(self.repo),
            "checkout": str(self.checkout),
            "root": "repo-root",
            "expected_repository_identity": "repo/repo",
            "initial_response": str(self.evidence / "query/projection.stdout"),
            "initial_response_sha256": sha(projection),
            "initial_ids": ["foo.py:target:function"],
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
            "expected_base_commit": self.base_commit,
            "expected_base_tree": self.base_tree,
            "expected_producer_commit": "c" * 40,
            "expected_cache_manifest_sha256": sha(self.cache_manifest.read_bytes()),
            "expected_cache_archive_sha256": sha(self.cache_archive.read_bytes()),
            "expected_cache_inventory_sha256": self.cache_inventory_sha256,
            "expected_launcher_sha256": sha(self.launcher.read_bytes()),
            "expected_binary_sha256": sha(self.binary.read_bytes()),
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
            "policy": policy,
            "checkout": str(checkout),
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

    def test_pair_private_harness_hidden_but_exact_treatment_wrapper_allowed(self):
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
                self.assertEqual(record["decision"], "deny" if policy == "control" else "allow")


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
            self.assertEqual(receipt["projected_stable_code_ids"], ["foo.py:target:function"])
            self.assertEqual(
                receipt["raw_stable_code_ids"],
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
            with self.assertRaises(run_selector.TreatmentAcquisitionFailure) as failure:
                run_selector.acquire_treatment(
                    SimpleNamespace(case_id="repo__repo-1", root="repo-root"),
                    {"rna_query.py": wrapper},
                    evidence,
                    {"expected_query_sha256": "a" * 64},
                )
            retained = failure.exception.evidence
            self.assertEqual(retained["schema_version"], run_selector.QUERY_EVIDENCE_SCHEMA)
            self.assertFalse(retained["succeeded"])
            self.assertEqual(
                retained["wrapper_command"],
                [str(wrapper), "--query-sha256", "a" * 64],
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
            "model": "claude-sonnet-5", "effort": "high", "permission_mode": "bypassPermissions",
            "tools": ["Bash", "Edit", "Read", "Write", "Glob", "Grep"],
            "disallowed_tools": ["WebSearch", "WebFetch"], "budget_usd": 3.0,
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

    def test_token_ledger_prefers_whole_invocation_model_usage_without_double_counting(self):
        summary = {
            "usage": {"input_tokens": 999, "output_tokens": 999},
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
        self.assertEqual(ledger["schema_version"], "issue825-token-ledger-v3")
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
                canonical({"type": "assistant", "message": {"role": "assistant", "id": "msg-1"}}).rstrip(),
                canonical({"type": "assistant", "message": {"role": "assistant", "id": "msg-1"}}).rstrip(),
                canonical({"type": "user", "message": {"role": "user"}}).rstrip(),
                canonical({"type": "assistant", "message": {"role": "assistant", "id": "msg-2"}}).rstrip(),
            )) + b"\n")
            observed = run_selector.transcript_provider_response_count([
                {"source_path": "/private/source", "retained": ref(transcript)},
            ])
            self.assertEqual(observed, 2)
            ledger = run_selector.token_ledger(
                {"usage": {"input_tokens": 1, "output_tokens": 2}, "num_turns": 4},
                provider_responses=observed,
            )
            self.assertEqual(ledger["cli_turns"], 4)
            self.assertEqual(ledger["provider_responses"], 2)
            self.assertIsNone(ledger["provider_requests"])

    def test_token_ledger_top_level_fallback_uses_all_provider_components(self):
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
        self.assertTrue(ledger["valid"])
        self.assertEqual(ledger["source"], "top_level_usage")
        self.assertEqual(ledger["provider_total_tokens"], 26)
        self.assertEqual(ledger["reasoning_tokens"], 8)

    def test_missing_token_usage_is_invalid_not_zero(self):
        ledger = run_selector.token_ledger({"valid_json": True, "num_turns": 2})
        self.assertFalse(ledger["valid"])
        self.assertTrue(ledger["model_invoked"])
        self.assertIsNone(ledger["input_tokens"])
        self.assertIsNone(ledger["output_tokens"])
        self.assertIsNone(ledger["provider_total_tokens"])

    def test_pre_model_absence_is_valid_only_in_noncompliant_no_model_context(self):
        ledger = run_selector.token_ledger({}, model_invoked=False)
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
                "schema_version": "issue825-actor-tool-ledger-v1", "arm": "A",
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
                "token_ledger": {"schema_version": run_selector.TOKEN_LEDGER_SCHEMA, "valid": True,
                    "errors": [], "source": "top_level_usage", "model_invoked": True,
                    "input_tokens": 0,
                    "output_tokens": 0, "provider_total_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
                    "reasoning_tokens": 0, "cli_turns": 0,
                    "provider_responses": None, "provider_requests": None},
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


if __name__ == "__main__":
    unittest.main()
