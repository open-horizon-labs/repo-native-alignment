from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER_PATH = ROOT / "scripts" / "swebench_act_context_runner.py"
PARSER_PATH = (
    ROOT / "benchmark" / "swebench-act-context" / "upstream" / "edit_patch_v2.py"
)
PROTOCOL_PATH = ROOT / "benchmark" / "swebench-act-context" / "protocol.json"

RUNNER_SPEC = importlib.util.spec_from_file_location(
    "swebench_act_context_runner", RUNNER_PATH
)
assert RUNNER_SPEC and RUNNER_SPEC.loader
RUNNER = importlib.util.module_from_spec(RUNNER_SPEC)
sys.modules[RUNNER_SPEC.name] = RUNNER
RUNNER_SPEC.loader.exec_module(RUNNER)

PARSER_SPEC = importlib.util.spec_from_file_location("edit_patch_v2", PARSER_PATH)
assert PARSER_SPEC and PARSER_SPEC.loader
PARSER = importlib.util.module_from_spec(PARSER_SPEC)
sys.modules[PARSER_SPEC.name] = PARSER
PARSER_SPEC.loader.exec_module(PARSER)


class SwebenchActContextRunnerTests(unittest.TestCase):
    def _git_fixture(self, root: Path) -> Path:
        checkout = root / "checkout"
        checkout.mkdir()
        subprocess.run(["git", "init", "--quiet"], cwd=checkout, check=True)
        subprocess.run(
            ["git", "config", "user.name", "Runner Test"], cwd=checkout, check=True
        )
        subprocess.run(
            ["git", "config", "user.email", "runner@example.invalid"],
            cwd=checkout,
            check=True,
        )
        (checkout / "module.py").write_text('VALUE = "old"\n', encoding="utf-8")
        subprocess.run(["git", "add", "module.py"], cwd=checkout, check=True)
        subprocess.run(
            ["git", "commit", "--quiet", "-m", "fixture"], cwd=checkout, check=True
        )
        cache = checkout / "__pycache__"
        cache.mkdir()
        (cache / "module.cpython-312.pyc").write_bytes(b"not bytecode")
        return checkout

    def test_frozen_prompt_and_registered_c_then_b_order(self) -> None:
        self.assertEqual(
            RUNNER.registered_arm_order("astropy__astropy-13398"), ("C", "B")
        )
        row = {
            "repo": "owner/project",
            "problem_statement": "Change the value.",
        }
        packet = b'*** FILE: module.py\nVALUE = "old"\n'
        protocol = json.loads(PROTOCOL_PATH.read_text(encoding="utf-8"))
        expected = protocol["instrument"]["prompt"]["template"].format(
            repo=row["repo"], issue=row["problem_statement"], context=packet.decode()
        )
        self.assertEqual(RUNNER.format_prompt(row, packet), expected)

    def test_retry_prompt_is_deterministic_and_uses_immediate_failure_feedback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = self._git_fixture(Path(temporary))
            raw = (
                "reasoning\n"
                "*** FILE: module.py\n"
                "*** SEARCH\nmissing = True\n"
                "*** REPLACE\nVALUE = \"new\"\n"
                "*** END\n"
            )
            _, results = PARSER.apply_edits_detailed(
                checkout, PARSER.parse_edits(raw)
            )
            previous = "discarded" + ("\N{GRINNING FACE}" * 6001)
            base = "INITIAL PROMPT"
            protocol = json.loads(PROTOCOL_PATH.read_text(encoding="utf-8"))
            expected = base + protocol["instrument"]["retry"]["retry_suffix"].format(
                prev=previous[-6000:], feedback=PARSER.failure_feedback(results)
            )
            first = RUNNER.assemble_retry_prompt(base, previous, results)
            second = RUNNER.assemble_retry_prompt(base, previous, results)
            self.assertEqual(first, expected)
            self.assertEqual(second, first)
            self.assertNotIn("discarded", first)

    def test_authorized_text_edit_prediction_excludes_untracked_bytecode(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = self._git_fixture(Path(temporary))
            raw = (
                "*** FILE: module.py\n"
                "*** SEARCH\nVALUE = \"old\"\n"
                "*** REPLACE\nVALUE = \"new\"\n"
                "*** END\n"
            )
            state, results = RUNNER.apply_response(checkout, raw)
            self.assertTrue(
                all(result.get("status") in {"matched", "created"} for result in results),
                results,
            )
            prediction = RUNNER.prediction_from_state(
                "fixture__fixture-1", checkout, state
            )
            patch = prediction["model_patch"]
            self.assertIn("module.py", patch)
            self.assertIn('+VALUE = "new"', patch)
            self.assertNotIn("__pycache__", patch)
            self.assertNotIn(".pyc", patch)

    def test_api_worker_reads_key_file_without_key_in_argv_or_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake_sdk = root / "fake-sdk"
            fake_sdk.mkdir()
            capture = root / "sdk-capture.json"
            (fake_sdk / "anthropic.py").write_text(
                """\
import json
import os
import sys
from types import SimpleNamespace

class _Messages:
    def create(self, **kwargs):
        with open(os.environ["RUNNER_TEST_CAPTURE"], "w", encoding="utf-8") as handle:
            json.dump({"argv": sys.argv, "request": kwargs}, handle, sort_keys=True)
        return SimpleNamespace(
            id="msg_fixture",
            model="claude-sonnet-4-6",
            stop_reason="end_turn",
            content=[SimpleNamespace(
                type="text",
                text="*** FILE: module.py\\n*** SEARCH\\nVALUE = \\\"old\\\"\\n"
                     "*** REPLACE\\nVALUE = \\\"new\\\"\\n*** END\\n",
            )],
            usage=SimpleNamespace(
                input_tokens=10,
                output_tokens=1,
                cache_creation_input_tokens=0,
                cache_read_input_tokens=0,
            ),
        )

class Anthropic:
    def __init__(self, api_key):
        if not api_key:
            raise RuntimeError("missing key")
        self.messages = _Messages()
""",
                encoding="utf-8",
            )
            request = root / "request.json"
            response = root / "response.json"
            request.write_text(
                json.dumps(
                    {
                        "model": "claude-sonnet-4-6",
                        "temperature": 0.0,
                        "max_tokens": 8000,
                        "messages": [{"role": "user", "content": "Say ACK"}],
                    }
                ),
                encoding="utf-8",
            )
            fake_key = "sk-ant-api03-" + ("T" * 95)
            key_file = root / "anthropic-key"
            key_file.write_text(fake_key + "\n", encoding="utf-8")
            key_file.chmod(0o600)
            env = os.environ.copy()
            env.update(
                {
                    "PYTHONPATH": os.pathsep.join((str(fake_sdk), str(ROOT))),
                    "RUNNER_TEST_CAPTURE": str(capture),
                    "SWE_BENCH_ANTHROPIC_KEY_FILE": str(key_file),
                }
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER_PATH),
                    "_api-worker",
                    "--request",
                    str(request),
                    "--response",
                    str(response),
                ],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            capture_payload = capture.read_text(encoding="utf-8")
            response_payload = response.read_text(encoding="utf-8")
            all_output = completed.stdout + completed.stderr + capture_payload + response_payload
            self.assertNotIn(fake_key, all_output)
            self.assertNotIn(fake_key, "\0".join(json.loads(capture_payload)["argv"]))
            worker_response = json.loads(response_payload)
            checkout = self._git_fixture(root)
            state, results = RUNNER.apply_response(checkout, worker_response["text"])
            self.assertEqual([result["status"] for result in results], ["matched"])
            prediction = RUNNER.prediction_from_state(
                "fixture__fixture-1", checkout, state
            )
            self.assertIn('+VALUE = "new"', prediction["model_patch"])
            self.assertNotIn("__pycache__", prediction["model_patch"])


if __name__ == "__main__":
    unittest.main()
