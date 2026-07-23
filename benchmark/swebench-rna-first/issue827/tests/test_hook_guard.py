from __future__ import annotations

from contextlib import redirect_stdout
import hashlib
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


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import common_supervisor
import hook_guard
import tool_supervisor


def canonical(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class GuardFixture:
    def __init__(self, root: Path):
        self.root = root
        self.harness = root / "harness"
        self.bin = self.harness / "bin"
        self.config_dir = self.harness / "config"
        self.evidence = root / "evidence"
        self.bin.mkdir(parents=True)
        self.config_dir.mkdir()
        (self.evidence / "hooks").mkdir(parents=True)
        self.child = self.bin / "common_supervisor.py"
        self.child.write_text(
            """#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
import time

counter = Path(os.environ["FAKE_CHILD_COUNTER"])
count = int(counter.read_text()) if counter.exists() else 0
counter.write_text(str(count + 1))
json.load(sys.stdin)
mode = os.environ.get("FAKE_CHILD_MODE", "allow")
if mode == "crash":
    raise SystemExit(23)
if mode == "timeout":
    time.sleep(2)
if mode == "malformed":
    print("{broken")
elif mode == "stderr":
    print("unexpected", file=sys.stderr)
elif mode == "unterminated_deny":
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "not terminal",
        }
    }))
elif mode == "deny":
    message = "fixture denied"
    print(json.dumps({
        "continue": False,
        "stopReason": message,
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": message,
        },
    }))
elif mode == "allow":
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "fixture allowed",
            "updatedInput": {"command": "registered gateway"},
        },
    }))
elif mode == "empty":
    pass
else:
    raise SystemExit(24)
"""
        )
        self.child.chmod(0o555)
        self.counter = root / "child-count"
        self.config = {
            "schema_version": hook_guard.CONFIG_SCHEMA,
            "policy": "control",
            "harness_root": str(self.harness),
            "episode_evidence_root": str(self.evidence),
            "common_state": str(
                self.evidence / "common-supervisor-state.json"
            ),
            "common_supervisor_sha256": sha256(self.child),
            "tool_supervisor_sha256": "0" * 64,
        }
        self.config_path = self.config_dir / "supervisor.json"
        self.config_path.write_bytes(canonical(self.config))
        self.event = {
            "session_id": "fixture-session",
            "tool_use_id": "fixture-tool",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "printf fixture"},
            "cwd": str(root),
        }

    def invoke(
        self,
        mode: str,
        *,
        timeout_ms: int = 250,
        event: dict[str, object] | None = None,
    ) -> subprocess.CompletedProcess[bytes]:
        env = {
            **os.environ,
            "FAKE_CHILD_MODE": mode,
            "FAKE_CHILD_COUNTER": str(self.counter),
        }
        return subprocess.run(
            [
                sys.executable,
                str(HERE / "hook_guard.py"),
                "--config",
                str(self.config_path),
                "--evidence-root",
                str(self.evidence),
                "--child",
                str(self.child),
                "--child-sha256",
                sha256(self.child),
                "--role",
                "common",
                "--timeout-ms",
                str(timeout_ms),
            ],
            input=canonical(self.event if event is None else event),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            check=False,
        )


class HookGuardTests(unittest.TestCase):
    def assert_terminal(self, result: subprocess.CompletedProcess[bytes]) -> dict:
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, b"")
        document = json.loads(result.stdout)
        self.assertIs(document["continue"], False)
        self.assertTrue(document["stopReason"])
        specific = document["hookSpecificOutput"]
        self.assertEqual(specific["permissionDecision"], "deny")
        self.assertTrue(specific["permissionDecisionReason"])
        return document

    def test_valid_allow_is_forwarded_and_evidenced(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            result = fixture.invoke("allow")
            self.assertEqual(result.returncode, 0, result.stderr)
            document = json.loads(result.stdout)
            self.assertEqual(
                document["hookSpecificOutput"]["permissionDecision"],
                "allow",
            )
            self.assertEqual(
                document["hookSpecificOutput"]["updatedInput"]["command"],
                "registered gateway",
            )
            self.assertFalse(
                (fixture.evidence / "hook-guard-state.json").exists()
            )
            record = json.loads(
                (fixture.evidence / "hooks/hook-guard-events.jsonl").read_text()
            )
            self.assertEqual(record["outcome"], "allow")
            self.assertFalse(record["fatal"])
            self.assertEqual(record["child_sha256"], sha256(fixture.child))

    def test_crash_is_terminal_and_sticky_before_any_later_child_or_provider(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            first = fixture.invoke("crash")
            self.assert_terminal(first)
            self.assertIn(b"hook_guard_child_crash", first.stdout)
            self.assertEqual(fixture.counter.read_text(), "1")

            state = json.loads(
                (fixture.evidence / "hook-guard-state.json").read_text()
            )
            self.assertTrue(state["fatal"])
            self.assertEqual(
                state["fatal_reason"], "hook_guard_child_crash"
            )
            common = json.loads(
                (fixture.evidence / "common-supervisor-state.json").read_text()
            )
            self.assertTrue(common["fatal"])

            # This is the fake CLI's next-loop boundary: a terminal hook result
            # means neither the tool nor another provider turn may be issued.
            tool_marker = fixture.root / "tool-executed"
            provider_marker = fixture.root / "provider-called"
            if json.loads(first.stdout).get("continue", True):
                tool_marker.write_text("unexpected")
                provider_marker.write_text("unexpected")
            self.assertFalse(tool_marker.exists())
            self.assertFalse(provider_marker.exists())

            second = fixture.invoke("allow")
            self.assert_terminal(second)
            self.assertIn(b"hook_guard_already_fatal", second.stdout)
            self.assertEqual(
                fixture.counter.read_text(),
                "1",
                "the sticky guard must not invoke a later child",
            )

    def test_explicit_child_denial_is_already_terminal_and_becomes_sticky(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            result = fixture.invoke("deny")
            document = self.assert_terminal(result)
            self.assertEqual(document["stopReason"], "fixture denied")
            state = json.loads(
                (fixture.evidence / "hook-guard-state.json").read_text()
            )
            self.assertEqual(
                state["fatal_reason"], "hook_guard_child_denied"
            )

    def test_crash_malformed_stderr_and_unterminated_denial_fail_closed(self):
        expectations = {
            "crash": "hook_guard_child_crash",
            "malformed": "hook_guard_child_output_invalid",
            "stderr": "hook_guard_child_stderr",
            "unterminated_deny": "hook_guard_child_denial_not_terminal",
        }
        for mode, reason in expectations.items():
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                fixture = GuardFixture(Path(temporary))
                result = fixture.invoke(mode)
                self.assert_terminal(result)
                self.assertIn(reason.encode(), result.stdout)
                state = json.loads(
                    (fixture.evidence / "hook-guard-state.json").read_text()
                )
                self.assertEqual(state["fatal_reason"], reason)

    def test_internal_timeout_finishes_well_before_claude_five_second_limit(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            started = time.monotonic()
            result = fixture.invoke("timeout", timeout_ms=50)
            elapsed = time.monotonic() - started
            self.assert_terminal(result)
            self.assertIn(b"hook_guard_child_timeout", result.stdout)
            self.assertLess(elapsed, 1.0)
            record = json.loads(
                (fixture.evidence / "hooks/hook-guard-events.jsonl").read_text()
            )
            self.assertLess(record["elapsed_ms"], 1_000)

    def test_parallel_bash_and_read_fail_closed_before_second_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            first_result: list[subprocess.CompletedProcess[bytes]] = []

            def invoke_slow_bash() -> None:
                first_result.append(
                    fixture.invoke("timeout", timeout_ms=750)
                )

            thread = threading.Thread(target=invoke_slow_bash)
            thread.start()
            deadline = time.monotonic() + 2
            while (
                not fixture.counter.exists()
                and time.monotonic() < deadline
            ):
                time.sleep(0.01)
            self.assertTrue(fixture.counter.exists())

            read_event = {
                **fixture.event,
                "tool_use_id": "parallel-read",
                "tool_name": "Read",
                "tool_input": {
                    "file_path": str(fixture.root / "fixture")
                },
            }
            second = fixture.invoke("allow", event=read_event)
            self.assert_terminal(second)
            self.assertIn(b"native_tool_rw_overlap", second.stdout)
            self.assertEqual(
                fixture.counter.read_text(),
                "1",
                "overlapping read child must not start",
            )

            thread.join(timeout=2)
            self.assertFalse(thread.is_alive())
            self.assertEqual(len(first_result), 1)
            self.assert_terminal(first_result[0])

    def test_missing_post_fails_closed_before_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            post = {
                **fixture.event,
                "hook_event_name": "PostToolUse",
            }
            result = fixture.invoke("allow", event=post)
            self.assert_terminal(result)
            self.assertIn(
                b"native_tool_post_without_matching_pre", result.stdout
            )
            self.assertFalse(fixture.counter.exists())

    def test_matching_post_releases_exclusive_reservation(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            pre = fixture.invoke("allow")
            self.assertEqual(pre.returncode, 0, pre.stderr)
            post = {
                **fixture.event,
                "hook_event_name": "PostToolUse",
            }
            completed = fixture.invoke("allow", event=post)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            state = json.loads(
                (
                    fixture.evidence
                    / "hooks/native-tool-state.json"
                ).read_bytes()
            )
            self.assertEqual(state["active"], {})

    def test_malformed_config_still_writes_guard_evidence_and_stops(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = GuardFixture(Path(temporary))
            fixture.config_path.write_text("{bad")
            result = fixture.invoke("allow")
            self.assert_terminal(result)
            self.assertIn(b"hook_guard_config_invalid", result.stdout)
            state = json.loads(
                (fixture.evidence / "hook-guard-state.json").read_text()
            )
            self.assertTrue(state["fatal"])
            self.assertEqual(
                state["fatal_reason"], "hook_guard_config_invalid"
            )

    def test_template_registers_only_guard_wrapped_supervisors(self):
        template = json.loads(
            (HERE / "claude-settings.template.json").read_text()
        )
        commands = [
            hook["command"]
            for groups in template["hooks"].values()
            for group in groups
            for hook in group["hooks"]
        ]
        self.assertTrue(commands)
        self.assertNotIn("__COMMON_SUPERVISOR__", commands)
        self.assertNotIn("__TOOL_SUPERVISOR__", commands)
        self.assertEqual(
            set(commands),
            {
                "__COMMON_HOOK_GUARD_COMMAND__",
                "__TOOL_HOOK_GUARD_COMMAND__",
            },
        )
        for groups in template["hooks"].values():
            for group in groups:
                for hook in group["hooks"]:
                    self.assertEqual(hook["timeout"], 5)


class ExistingSupervisorTerminationTests(unittest.TestCase):
    def test_common_in_process_denial_both_denies_and_stops(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = {
                "policy": "control",
                "common_state": str(root / "common-state.json"),
                "common_hook_ledger": str(root / "common-ledger.jsonl"),
            }
            event = {
                "hook_event_name": "PreToolUse",
                "tool_name": "Read",
                "tool_use_id": "tool",
            }
            output = io.StringIO()
            with redirect_stdout(output):
                common_supervisor._deny(
                    event,
                    config,
                    common_supervisor.IsolationViolation("fixture_denial"),
                )
            document = json.loads(output.getvalue())
            self.assertIs(document["continue"], False)
            self.assertEqual(
                document["hookSpecificOutput"]["permissionDecision"], "deny"
            )

    def test_treatment_in_process_denial_both_denies_and_stops(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = {
                "policy": "treatment",
                "state": str(root / "state.json"),
                "hook_ledger": str(root / "ledger.jsonl"),
            }
            event = {
                "hook_event_name": "PreToolUse",
                "tool_name": "Read",
                "tool_use_id": "tool",
            }
            current = {
                "schema_version": "issue827-rna-supervisor-state-v1",
                "fatal": False,
                "first_traversal_succeeded": False,
                "model_tool_attempts": 1,
                "rna_calls": 0,
            }
            output = io.StringIO()
            with redirect_stdout(output):
                tool_supervisor.deny(
                    event, config, current, "fixture_denial"
                )
            document = json.loads(output.getvalue())
            self.assertIs(document["continue"], False)
            self.assertEqual(
                document["hookSpecificOutput"]["permissionDecision"], "deny"
            )


if __name__ == "__main__":
    unittest.main()
