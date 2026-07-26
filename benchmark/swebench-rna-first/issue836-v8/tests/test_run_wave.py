from __future__ import annotations

import json
from types import SimpleNamespace
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import run_wave  # noqa: E402
import assemble_wave  # noqa: E402
import observational_tool_supervisor as observer  # noqa: E402
import verify_preconditioned as verifier  # noqa: E402


class PriorWaveSealTests(unittest.TestCase):
    def test_qualified_registration_allows_only_exact_successor_delta(self) -> None:
        registration = json.loads(
            (ROOT.parent / "issue836-v4" / "registration.json").read_bytes()
        )
        contract = run_wave.contract
        contract.validate_qualified_registered_sources(
            run_wave.base.registration_contract,
            registration,
        )
        original_sha = contract.sha_file

        def drift(path: Path) -> str:
            if (
                path.parent == contract.SUCCESSOR_HARNESS_ROOT
                and path.name == "tool_supervisor.py"
            ):
                return "0" * 64
            return original_sha(path)

        with mock.patch.object(contract, "sha_file", side_effect=drift):
            with self.assertRaises(contract.ContractError):
                contract.validate_qualified_registered_sources(
                    run_wave.base.registration_contract,
                    registration,
                )

    def test_deterministic_query_uses_clean_bounded_issue_context(self) -> None:
        problem = (
            b"Generic title\r\n"
            b"<!-- template noise -->\r\n"
            b"Version 1.2.3\r\n\r\n"
            b"Calling `target_api` returns the wrong class.\r\n"
            b"```python\r\nignored_call()\r\n```\r\n"
            + (b"x" * 600)
        )
        query = run_wave.deterministic_rna_query(problem)
        title, context = query.decode().split("\n\n", 1)
        self.assertEqual(title, "Generic title")
        self.assertTrue(
            context.startswith(
                "Version 1.2.3 Calling `target_api` returns the wrong class. "
            )
        )
        self.assertNotIn("template noise", context)
        self.assertNotIn("ignored_call", context)
        self.assertEqual(len(context), run_wave.QUERY_BODY_CHAR_LIMIT)
        self.assertEqual(
            verifier.deterministic_rna_query(problem),
            query,
        )

    def test_successor_dns_aliases_are_literal_and_gateway_is_reachable(self) -> None:
        self.assertEqual(
            run_wave.HOST_DEV_NULL_SMOKE_COMMAND,
            "printf host-null-smoke >/dev/null",
        )
        self.assertEqual(run_wave.GATEWAY_SMOKE_COMMAND, "exit 7")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            read = root / "read"
            write = root / "write"
            read.mkdir()
            write.mkdir()
            profile = run_wave.generate_successor_outer_seatbelt_profile(
                read_roots=[read],
                write_roots=[write],
            )
            loopback_profile = run_wave.generate_successor_outer_seatbelt_profile(
                read_roots=[read],
                write_roots=[write],
                loopback_only_outbound=True,
            )
        self.assertIn('(literal "/etc")', profile)
        self.assertIn('(literal "/var")', profile)
        self.assertIn('(literal "/dev/null")', profile)
        self.assertNotIn('(subpath "/etc")', profile)
        self.assertNotIn('(subpath "/var")', profile)
        self.assertNotIn("rna827-landlock-docker.sock", profile)
        self.assertNotIn("(deny network-outbound", profile)

        self.assertIn("(deny network-outbound", loopback_profile)

    def test_gateway_smoke_uses_its_own_existing_output_root(self) -> None:
        class Configured(RuntimeError):
            pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            paid_output = root / "paid-output"
            smoke_output = root / "smoke-output"
            checkout = root / "checkout"
            index_checkout = root / "index-checkout"
            checkout.mkdir()
            index_checkout.mkdir()
            prepared = run_wave.base.PreparedRun(
                manifest_path=root / "manifest.json",
                manifest_ref={},
                registration_path=root / "registration.json",
                registration_ref={},
                registration={},
                selection_path=root / "selection.json",
                selection_ref={},
                selection={},
                claude_path=root / "claude",
                claude_version="test",
                launcher_path=root / "launcher",
                binary_path=root / "binary",
                rna_refs={},
                trusted_rna_toolchain_root=root,
                mcp_path=root / "mcp.json",
                output_root=paid_output,
                cases=(),
                isolation_host={},
            )
            case = SimpleNamespace(
                rank=1,
                case_id="owner__repo-1",
                checkouts={"A": checkout},
                index_checkout=index_checkout,
            )
            with (
                mock.patch.object(
                    run_wave.base,
                    "materialize_harness",
                    return_value={},
                ),
                mock.patch.object(
                    run_wave.base,
                    "configure_episode",
                    side_effect=Configured,
                ) as configure,
                self.assertRaises(Configured),
            ):
                run_wave.gateway_smoke(
                    prepared,
                    [case],
                    rank=1,
                    output_root=smoke_output,
                )

            self.assertFalse(paid_output.exists())
            self.assertTrue(smoke_output.is_dir())
            configured = configure.call_args.args[0]
            self.assertIsInstance(configured, run_wave.base.PreparedRun)
            self.assertEqual(configured.output_root, smoke_output)
            self.assertEqual(prepared.output_root, paid_output)
            self.assertEqual(configured.manifest_path, prepared.manifest_path)

    def test_treatment_is_preconditioned_without_a_first_tool_gate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            projection = (
                b'READY\n## Search: "Exact issue title"\n'
                b"`src/module.py:symbol:function`\n"
            )
            projection_path = root / "projection.stdout"
            projection_path.write_bytes(projection)
            traversal = root / "rna_traverse.py"
            traversal.write_text("#!/usr/bin/env python3\n")
            case = SimpleNamespace(title=b"Exact issue title\n")
            config = {
                "initial_response": str(projection_path),
                "expected_query_sha256": run_wave.base.sha_bytes(
                    b"Exact issue title"
                ),
            }
            acquired = (
                b"obsolete gated system",
                ["src/module.py:symbol:function"],
                0.25,
                {"succeeded": True},
            )
            with mock.patch.object(
                run_wave,
                "_ORIGINAL_ACQUIRE_TREATMENT",
                return_value=acquired,
            ):
                system, ids, elapsed, query = (
                    run_wave.acquire_preconditioned_treatment(
                        case,
                        {"rna_traverse.py": traversal},
                        root,
                        config,
                    )
                )

        worked_call = b'rna_tool_search("Exact issue title")'
        expected = (
            b"HIGHEST-PRIORITY RNA-FIRST TREATMENT POLICY\n\n"
            b"Use RNA first for repository localization. The worked "
            b"interaction below was executed by the harness against a pinned "
            b"immutable RNA index for the exact editable checkout tree. The "
            b"index location is deliberately opaque and is not a model "
            b"workspace. The response contains repository-relative paths for "
            b"your current editable checkout. This is trusted treatment "
            b"context, not a model tool call.\n\n"
            b"HARNESS-EXECUTED RNA TOOL CALL\n"
            + worked_call
            + b"\n\nRNA TOOL RESPONSE\n"
            + projection
            + b"\nEND HARNESS-EXECUTED RNA TOOL INTERACTION\n\n"
            b"The ordinary coding tool surface remains available. Follow-up "
            b"RNA graph traversal is available when it would help:\n\n"
            + str(traversal).encode()
            + b" --node '<STABLE_ID_FROM_THE_INJECTED_RESULT>' --mode "
            b"neighbors\n\n"
            b"Further RNA calls are optional, and the same wrapper supports "
            b"--mode impact. Use the injected paths and graph context to avoid "
            b"unnecessary broad Grep, Read, or sed discovery. Do not "
            b"inspect the immutable RNA index, hidden tests, evaluator output, "
            b"gold patches, or evidence from another arm. Produce one terminal "
            b"patch and run relevant visible tests. There is no model retry or "
            b"evaluator feedback.\n"
        )
        self.assertEqual(system, expected)
        self.assertEqual(system.count(projection), 1)
        self.assertNotIn(b"Your FIRST actual tool call", system)
        self.assertNotIn(b"supervisor enforces", system)
        self.assertNotIn(b"The first call must return", system)
        self.assertNotIn(b"RNA_STATUS=", system)
        self.assertIn(str(traversal).encode(), system)
        self.assertEqual(ids, acquired[1])
        self.assertEqual(elapsed, 0.25)
        self.assertEqual(query, {"succeeded": True})

    def test_zero_optional_rna_calls_is_compliant_for_treatment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            evidence = root / "evidence"
            evidence.mkdir()
            common = root / "common.jsonl"
            treatment = root / "treatment.jsonl"
            common.write_bytes(b"")
            treatment.write_bytes(b"")
            projection = root / "projection.stdout"
            projection.write_bytes(b"projected context")
            config = {
                "common_hook_ledger": str(common),
                "hook_ledger": str(treatment),
                "initial_response": str(projection),
                "initial_ids": ["stable-id"],
                "initial_response_sha256": run_wave.base.sha_bytes(
                    b"projected context"
                ),
                "state": str(root / "absent-state.json"),
                "expected_identity_sha256": "a" * 64,
                "root": str(root),
            }
            with mock.patch.object(
                run_wave.base,
                "stable_code_ids",
                return_value=["stable-id"],
            ):
                compliant, errors = (
                    run_wave.preconditioned_treatment_compliance(
                        config,
                        evidence,
                        "T",
                    )
                )

        self.assertTrue(compliant)
        self.assertEqual(errors, [])

    def test_actor_ledger_counts_native_tools_and_shell_discovery(self) -> None:
        original = {
            "actions": [
                {"tool": "Read", "bash_command": None},
                {"tool": "Grep", "bash_command": None},
                {"tool": "Bash", "bash_command": "/usr/bin/grep needle file"},
                {"tool": "Bash", "bash_command": "rg symbol src"},
                {"tool": "Bash", "bash_command": "printf x | sed -n '1p'"},
            ],
            "model_tool_counts": {"Bash": 3, "Grep": 1, "Read": 1},
        }
        treatment_hooks = [
            {"decision": "observed_rna_success"},
            {"decision": "observed_rna_failure"},
        ]
        with mock.patch.object(
            run_wave,
            "_ORIGINAL_BUILD_ACTOR_TOOL_LEDGER",
            return_value=original,
        ):
            ledger = run_wave.build_observational_actor_tool_ledger(
                "T",
                [],
                treatment_hooks,
                {"succeeded": True},
                False,
            )

        self.assertEqual(
            ledger["model_tool_counts"],
            {"Bash": 3, "Grep": 1, "Read": 1},
        )
        self.assertEqual(
            ledger["observed_shell_command_family_counts"],
            {"grep": 1, "rg": 1, "sed": 1},
        )
        self.assertEqual(ledger["successful_optional_rna_calls"], 1)
        self.assertEqual(ledger["failed_optional_rna_calls"], 1)
        self.assertTrue(ledger["rna_preconditioned"])

    def test_observer_allows_read_as_the_first_model_tool(self) -> None:
        event = {
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_use_id": "toolu-read",
            "tool_input": {"file_path": "/repo/src/module.py"},
        }
        current = {"model_tool_attempts": 0, "fatal": False}
        with (
            mock.patch.object(observer, "state", return_value=current),
            mock.patch.object(observer, "write_state") as write_state,
            mock.patch.object(observer, "log") as log,
        ):
            self.assertEqual(observer.handle(event, {"policy": "treatment"}), 0)

        self.assertEqual(current["model_tool_attempts"], 1)
        write_state.assert_called_once()
        log.assert_called_once_with(
            {"policy": "treatment"},
            event,
            "observe_allow",
        )

    def test_gateway_binary_parent_is_added_to_outer_read_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            docker = root / "bin" / "docker"
            docker.parent.mkdir()
            docker.write_text("pinned")
            host = {
                "docker_binary": str(docker),
                "system_read_roots": ["/existing"],
            }
            with mock.patch.object(
                run_wave,
                "_ORIGINAL_VERIFY_ISOLATION_HOST",
                return_value=host,
            ):
                result = (
                    run_wave.verify_isolation_host_with_gateway_read_access(
                        {},
                        {},
                    )
                )

        self.assertEqual(
            result["system_read_roots"],
            ["/existing", str(docker.parent)],
        )

    def test_native_tool_guard_allows_parallel_tools_but_still_compiles(
        self,
    ) -> None:
        source = (
            ROOT.parent / "issue827" / "hook_guard.py"
        ).read_bytes()
        transformed = run_wave.allow_parallel_native_tools(source)

        self.assertNotIn(b"native_tool_rw_overlap", transformed)
        self.assertNotIn(b"native_tool_post_unresolved", transformed)
        self.assertIn(b"native_tool_duplicate_pre", transformed)
        self.assertIn(b"native_tool_post_without_matching_pre", transformed)
        compile(transformed, "patched-hook-guard.py", "exec")

    def test_materialized_harness_contains_observer_and_parallel_guard(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case_root = Path(directory).resolve()
            paths = run_wave.materialize_observational_harness(
                case_root,
                "T",
            )
            manifest = json.loads(paths["materialization"].read_bytes())
            observer_bytes = paths["tool_supervisor.py"].read_bytes()
            guard_bytes = paths["hook_guard.py"].read_bytes()

        self.assertEqual(
            observer_bytes,
            (ROOT / "observational_tool_supervisor.py").read_bytes(),
        )
        self.assertNotIn(b"native_tool_rw_overlap", guard_bytes)
        self.assertNotIn(b"native_tool_post_unresolved", guard_bytes)
        self.assertEqual(
            manifest["files"]["hook_guard.py"]["destination"]["sha256"],
            run_wave.base.sha_bytes(guard_bytes),
        )

    def test_token_reporting_uses_whole_invocation_model_usage(self) -> None:
        sentinel = {"valid": True}
        with mock.patch.object(
            run_wave,
            "_ORIGINAL_TOKEN_LEDGER",
            return_value=sentinel,
        ) as original:
            result = run_wave.authoritative_token_ledger(
                {"modelUsage": {}},
                model_events=[{"message_id": "observational"}],
                provider_responses=3,
                provider_requests=None,
            )

        self.assertIs(result, sentinel)
        original.assert_called_once_with(
            {"modelUsage": {}},
            model_invoked=True,
            model_events=None,
            provider_responses=3,
            provider_requests=None,
        )

    def test_adapter_and_runner_manifest_interfaces_are_exact(self) -> None:
        self.assertEqual(
            assemble_wave.WAVE_MANIFEST_KEYS,
            run_wave.MANIFEST_KEYS,
        )
        self.assertEqual(
            assemble_wave.COMPATIBILITY_KEYS,
            run_wave.COMPATIBILITY_KEYS,
        )
        self.assertEqual(
            run_wave.contract.WAVE_MANIFEST_SCHEMA,
            assemble_wave.contract.WAVE_MANIFEST_SCHEMA,
        )

    def test_v4_episode_keys_must_be_projected_from_frozen_cases(self) -> None:
        expected = [
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "A",
                "session_id": "session-a",
            },
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "T",
                "session_id": "session-t",
            },
        ]
        manifest = {
            "cases": [
                {
                    "rank": 1,
                    "instance_id": "owner__repo-1",
                    "arm_order": ["A", "T"],
                    "arms": {
                        "A": {"session_id": "session-a"},
                        "T": {"session_id": "session-t"},
                    },
                }
            ],
            "execution_episode_keys": expected,
        }
        receipt = {"execution_episode_keys": expected}
        handoff = {"exact_requested_episode_order": expected}
        assemble_wave.validate_execution_episode_keys(
            manifest,
            receipt,
            handoff,
        )

        forged = [dict(item) for item in expected]
        forged[0]["case_id"] = "owner__repo-forged"
        manifest["execution_episode_keys"] = forged
        receipt["execution_episode_keys"] = forged
        handoff["exact_requested_episode_order"] = forged
        with self.assertRaises(run_wave.base.FailClosed):
            assemble_wave.validate_execution_episode_keys(
                manifest,
                receipt,
                handoff,
            )

    def test_paid_wave_input_must_be_canonical_adapter_output(self) -> None:
        compatibility_path = (
            Path("/evidence/wave") / run_wave.contract.COMPATIBILITY_FILENAME
        )
        canonical = (
            compatibility_path.parent
            / run_wave.contract.WAVE_MANIFEST_FILENAME
        )
        run_wave.require_canonical_wave_manifest_path(
            canonical,
            compatibility_path,
        )
        with self.assertRaises(run_wave.base.FailClosed):
            run_wave.require_canonical_wave_manifest_path(
                Path("/evidence/copied-wave-manifest.json"),
                compatibility_path,
            )

    def test_successor_envelope_binding_coexists_with_preserved_v5(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_root = Path(directory).resolve()
            old_binding = evidence_root / "v5-envelope-binding.json"
            old_binding.write_text("{}\n")
            successor = (
                evidence_root
                / run_wave.contract.ENVELOPE_BINDING_FILENAME
            )
            assemble_wave.validate_envelope_binding_path(
                successor,
                evidence_root,
            )
            self.assertTrue(old_binding.is_file())
            self.assertFalse(successor.exists())
            with self.assertRaises(run_wave.base.FailClosed):
                assemble_wave.validate_envelope_binding_path(
                    old_binding,
                    evidence_root,
                )

    def test_qualification_bridge_accepts_only_exact_registered_delta(
        self,
    ) -> None:
        current = json.loads(
            (ROOT.parent / "issue836-v4" / "registration.json").read_bytes()
        )
        qualified = json.loads(
            (ROOT.parent / "issue830" / "registration.json").read_bytes()
        )
        qualification = json.loads(
            (
                ROOT.parent
                / "issue830"
                / "qualification-closure.manifest.json"
            ).read_bytes()
        )
        run_wave.contract.validate_qualification_compatibility(
            run_wave.contract.QUALIFICATION_COMPATIBILITY,
            qualification,
            current,
            qualified,
        )
        compatibility = {
            "qualification_closure": {
                "manifest": run_wave.contract.file_ref(
                    ROOT.parent
                    / "issue830"
                    / "qualification-closure.manifest.json"
                )
            }
        }
        schedule = {
            "qualification_compatibility": (
                run_wave.contract.QUALIFICATION_COMPATIBILITY
            )
        }

        def exact_registered_files_mismatch(*_args: object) -> None:
            raise run_wave.base.FailClosed(
                "qualification manifest binding mismatch: "
                "registered_files_sha256"
            )

        run_wave.verify_v4_qualification_compatibility(
            compatibility,
            current,
            schedule,
            qualification_verifier=exact_registered_files_mismatch,
        )

        def another_failure(*_args: object) -> None:
            raise run_wave.base.FailClosed("another qualification failure")

        with self.assertRaises(run_wave.base.FailClosed):
            run_wave.verify_v4_qualification_compatibility(
                compatibility,
                current,
                schedule,
                qualification_verifier=another_failure,
            )

        tampered = json.loads(json.dumps(current))
        tampered["registered_files"]["runner_sha256"] = "0" * 64
        with self.assertRaises(run_wave.contract.ContractError):
            run_wave.contract.validate_qualification_compatibility(
                run_wave.contract.QUALIFICATION_COMPATIBILITY,
                qualification,
                tampered,
                qualified,
            )

    def test_exact_complete_prior_wave_is_accepted(self) -> None:
        run_wave.require_sealed_prior_wave(
            {
                "all_authorized_episodes_recorded": True,
                "worker_errors": [],
                "episode_receipts": [{}, {}, {}, {}],
            },
            (1, 2),
        )

    def test_partial_or_failed_prior_wave_blocks_more_spend(self) -> None:
        invalid = (
            {
                "all_authorized_episodes_recorded": False,
                "worker_errors": [],
                "episode_receipts": [{}, {}, {}, {}],
            },
            {
                "all_authorized_episodes_recorded": True,
                "worker_errors": ["failed"],
                "episode_receipts": [{}, {}, {}, {}],
            },
            {
                "all_authorized_episodes_recorded": True,
                "worker_errors": [],
                "episode_receipts": [{}, {}, {}],
            },
        )
        for receipt in invalid:
            with self.subTest(receipt=receipt), self.assertRaises(
                run_wave.base.FailClosed
            ):
                run_wave.require_sealed_prior_wave(receipt, (1, 2))

    def test_arbitrary_or_zero_model_episode_receipts_do_not_consume(self) -> None:
        compatibility = {"path": "/compat", "bytes": 1, "sha256": "a" * 64}
        registration = {"path": "/registration", "bytes": 1, "sha256": "b" * 64}
        selection = {"path": "/selection", "bytes": 1, "sha256": "c" * 64}
        authorized = [
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "A",
                "session_id": "session-a",
            }
        ]
        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory).resolve() / "output"
            receipt_path = (
                output_root
                / "rank-01-owner__repo-1"
                / "A"
                / "episode-receipt.json"
            )
            receipt_path.parent.mkdir(parents=True)
            for document in (
                {"not": "an episode receipt"},
                {
                    "schema_version": run_wave.base.RECEIPT_SCHEMA,
                    **authorized[0],
                    "run_manifest": compatibility,
                    "registration": registration,
                    "selection": selection,
                    "official_evaluator_invoked": False,
                    "token_ledger": {"model_invoked": False},
                },
            ):
                receipt_path.write_text(
                    json.dumps(document, sort_keys=True) + "\n"
                )
                with self.subTest(document=document), self.assertRaises(
                    run_wave.base.FailClosed
                ):
                    run_wave.validate_consumed_episode_refs(
                        [run_wave.contract.file_ref(receipt_path)],
                        authorized_episode_keys=authorized,
                        compatibility_ref=compatibility,
                        registration_ref=registration,
                        selection_ref=selection,
                        output_root=output_root,
                        where="synthetic wave",
                    )

    def test_consumed_failure_requires_canonical_path_and_exact_identity(self) -> None:
        compatibility = {"path": "/compat", "bytes": 1, "sha256": "a" * 64}
        registration = {"path": "/registration", "bytes": 1, "sha256": "b" * 64}
        selection = {"path": "/selection", "bytes": 1, "sha256": "c" * 64}
        authorized = [
            {
                "rank": 1,
                "case_id": "owner__repo-1",
                "arm": "A",
                "session_id": "session-a",
            }
        ]
        receipt = {
            "schema_version": run_wave.base.RECEIPT_SCHEMA,
            **authorized[0],
            "run_manifest": compatibility,
            "registration": registration,
            "selection": selection,
            "official_evaluator_invoked": False,
            "token_ledger": {"model_invoked": True},
            "errors": ["model_exit_1"],
            "policy_compliant": False,
        }
        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory).resolve() / "output"
            canonical = (
                output_root
                / "rank-01-owner__repo-1"
                / "A"
                / "episode-receipt.json"
            )
            canonical.parent.mkdir(parents=True)
            canonical.write_text(json.dumps(receipt, sort_keys=True) + "\n")
            accepted = run_wave.validate_consumed_episode_refs(
                [run_wave.contract.file_ref(canonical)],
                authorized_episode_keys=authorized,
                compatibility_ref=compatibility,
                registration_ref=registration,
                selection_ref=selection,
                output_root=output_root,
                where="synthetic wave",
            )
            self.assertEqual(
                accepted,
                [(1, "owner__repo-1", "A", "session-a")],
            )
            escaped = output_root.parent / "episode-receipt.json"
            escaped.write_text(json.dumps(receipt, sort_keys=True) + "\n")
            with self.assertRaises(run_wave.base.FailClosed):
                run_wave.validate_consumed_episode_refs(
                    [run_wave.contract.file_ref(escaped)],
                    authorized_episode_keys=authorized,
                    compatibility_ref=compatibility,
                    registration_ref=registration,
                    selection_ref=selection,
                    output_root=output_root,
                    where="synthetic wave",
                )

    def test_zero_model_first_arm_stops_same_case(self) -> None:
        prepared = SimpleNamespace(
            output_root=None,
            manifest_ref={"manifest": 1},
            registration_ref={"registration": 1},
            selection_ref={"selection": 1},
        )
        case = SimpleNamespace(
            rank=1,
            case_id="owner__repo-1",
            arm_order=("A", "T"),
            sessions={"A": "session-a", "T": "session-t"},
        )
        with tempfile.TemporaryDirectory() as directory:
            prepared.output_root = Path(directory).resolve() / "output"
            receipt = {
                "episode_receipt": {"path": "/unused"},
                "token_ledger": {"model_invoked": False},
            }
            with (
                mock.patch.object(
                    run_wave.base,
                    "materialize_harness",
                    return_value={},
                ),
                mock.patch.object(
                    run_wave.base,
                    "launch_episode",
                    return_value=receipt,
                ) as launch,
            ):
                receipts, errors = run_wave.execute_case_once(prepared, case)
            self.assertEqual(launch.call_count, 1)
            self.assertEqual(receipts, [receipt])
            self.assertEqual(
                errors,
                [
                    "owner__repo-1/A: "
                    "retryable_pre_model_failure_not_consumed"
                ],
            )

    def test_execution_claim_is_cross_process_exclusive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory).resolve() / "output"
            script = (
                "import pathlib,sys;"
                "sys.path.insert(0,sys.argv[2]);"
                "import run_wave;"
                "\ntry:\n"
                "  with run_wave.execution_claim(pathlib.Path(sys.argv[1])):"
                "\n    pass\n"
                "except run_wave.base.FailClosed:\n"
                "  raise SystemExit(3)\n"
            )
            with run_wave.execution_claim(output_root):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-c",
                        script,
                        str(output_root),
                        str(ROOT),
                    ],
                    check=False,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
            self.assertEqual(result.returncode, 3, result.stderr.decode())


if __name__ == "__main__":
    unittest.main()
