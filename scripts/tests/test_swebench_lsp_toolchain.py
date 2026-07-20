from __future__ import annotations

import importlib.util
import copy
import gzip
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import unittest.mock as mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "swebench_lsp_toolchain.py"
SPEC = importlib.util.spec_from_file_location("swebench_lsp_toolchain", MODULE_PATH)
assert SPEC and SPEC.loader
TOOLCHAIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TOOLCHAIN
SPEC.loader.exec_module(TOOLCHAIN)


class SwebenchLspToolchainTests(unittest.TestCase):
    def test_minified_body_provenance_accepts_direct_and_wrapped_structural_ast(
        self,
    ) -> None:
        for wrapped in ("false", "true"):
            with self.subTest(wrapped=wrapped):
                TOOLCHAIN._require_structural_minification_provenance(
                    "`owner/repo:file.py:symbol:function`\n"
                    f"  body_minification.v1 provenance=structural_ast wrapper={wrapped}\n"
                    "```python\ndef symbol(): ...\n```\n"
                )

    def test_minified_body_provenance_rejects_non_strict_markers(self) -> None:
        structural = (
            "body_minification.v1 provenance=structural_ast wrapper=false"
        )
        cases = {
            "missing": "```python\ndef symbol(): ...\n```\n",
            "duplicate": f"{structural}\n{structural}\n",
            "fallback": (
                "body_minification.v1 provenance=unsupported_language_text\n"
            ),
            "failure": (
                "body_minification.v1 failure language=python "
                "stage=parse reason=syntax_error\n"
            ),
        }
        for name, stdout in cases.items():
            with self.subTest(name=name):
                with self.assertRaises(TOOLCHAIN.ToolchainError):
                    TOOLCHAIN._require_structural_minification_provenance(stdout)

    def test_esbonio_entrypoint_is_relocatable_and_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = root / "first/bin/esbonio"
            second = root / "different-length-root/bin/esbonio"
            first.parent.mkdir(parents=True)
            second.parent.mkdir(parents=True)
            first.write_text(f"#!{first.parent}/python\npath dependent\n")
            second.write_text(f"#!{second.parent}/python\npath dependent\n")

            TOOLCHAIN._write_relocatable_esbonio_entrypoint(first)
            TOOLCHAIN._write_relocatable_esbonio_entrypoint(second)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            self.assertEqual(
                TOOLCHAIN.sha256_file(first), TOOLCHAIN.sha256_file(second)
            )
            self.assertTrue(first.stat().st_mode & 0o111)
            self.assertIn(b"from esbonio.cli import main", first.read_bytes())

    def test_case_readiness_uses_live_gate_and_matches_persisted_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "readiness.log"
            report = {"violations": [], "digest": "persisted-digest"}
            live = {
                "ready": True,
                "report": report,
                "compatibility_violations": [],
            }
            log.write_bytes(
                json.dumps(live).encode() + b"\n\n--- stderr ---\nwarning\n"
            )

            TOOLCHAIN._require_ready_case(log, report, "owner__repo-1")

    def test_case_readiness_fails_closed_on_live_or_persisted_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "readiness.log"
            report = {"violations": [], "digest": "persisted-digest"}
            cases = [
                {
                    "report": report,
                    "compatibility_violations": [],
                },
                {
                    "ready": 1,
                    "report": report,
                    "compatibility_violations": [],
                },
                {
                    "ready": False,
                    "report": report,
                    "compatibility_violations": [],
                },
                {
                    "ready": True,
                    "report": report,
                    "compatibility_violations": [{"kind": "fixture"}],
                },
                {
                    "ready": True,
                    "report": {"violations": [], "digest": "live-drift"},
                    "compatibility_violations": [],
                },
                {
                    "ready": True,
                    "report": {
                        "violations": [{"kind": "discarded_lsp_evidence"}],
                        "digest": "not-ready",
                    },
                    "compatibility_violations": [],
                },
            ]
            for live in cases:
                with self.subTest(live=live):
                    log.write_bytes(
                        json.dumps(live).encode() + b"\n\n--- stderr ---\n"
                    )
                    with self.assertRaises(TOOLCHAIN.ToolchainError):
                        TOOLCHAIN._require_ready_case(log, report, "owner__repo-1")

    def test_java_jar_launcher_reports_locked_version_without_starting_java(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            launcher = root / "bin" / "lemminx"
            launcher.parent.mkdir()
            launcher.write_bytes(
                TOOLCHAIN._wrapper(
                    "java-jar",
                    "servers/lemminx.jar",
                    command_name="lemminx",
                    version="0.3.0",
                )
            )
            launcher.chmod(0o755)

            completed = subprocess.run(
                [str(launcher), "--version"],
                check=True,
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.stdout, "lemminx 0.3.0\n")
            self.assertEqual(completed.stderr, "")

    def test_node_launcher_reports_locked_version_without_path_bearing_diagnostics(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            launcher = root / "bin" / "vscode-json-language-server"
            launcher.parent.mkdir()
            launcher.write_bytes(
                TOOLCHAIN._wrapper(
                    "node",
                    "missing/server.js",
                    command_name="vscode-json-language-server",
                    version="4.10.0",
                )
            )
            launcher.chmod(0o755)

            completed = subprocess.run(
                [str(launcher), "--version"],
                check=True,
                capture_output=True,
                text=True,
            )

            self.assertEqual(
                completed.stdout,
                "vscode-json-language-server 4.10.0\n",
            )
            self.assertEqual(completed.stderr, "")

    def test_operation_capabilities_preserve_actual_negotiated_providers(self) -> None:
        text_document_client = TOOLCHAIN._probe_client_capabilities()[
            "textDocument"
        ]
        self.assertEqual(
            text_document_client["definition"], {"dynamicRegistration": False}
        )
        self.assertEqual(
            text_document_client["references"], {"dynamicRegistration": False}
        )
        self.assertEqual(
            text_document_client["callHierarchy"],
            {"dynamicRegistration": False},
        )
        capabilities = {
            "documentSymbolProvider": {"label": "symbols"},
            "definitionProvider": True,
            "referencesProvider": {"workDoneProgress": True},
            "callHierarchyProvider": {},
            "codeActionProvider": False,
        }
        evidence = TOOLCHAIN._operation_capability_evidence(capabilities)
        self.assertEqual(
            TOOLCHAIN._operation_capabilities(capabilities),
            [
                "textDocument/documentSymbol",
                "textDocument/definition",
                "textDocument/references",
                "textDocument/prepareCallHierarchy",
                "callHierarchy/incomingCalls",
                "callHierarchy/outgoingCalls",
            ],
        )
        by_provider = {record["provider"]: record for record in evidence}
        self.assertEqual(
            by_provider["documentSymbolProvider"]["advertised_value"],
            {"label": "symbols"},
        )
        self.assertEqual(
            by_provider["callHierarchyProvider"]["methods"],
            [
                "textDocument/prepareCallHierarchy",
                "callHierarchy/incomingCalls",
                "callHierarchy/outgoingCalls",
            ],
        )
        self.assertFalse(by_provider["codeActionProvider"]["supported"])

    def test_failed_case_evidence_survives_ephemeral_checkout_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh" / ".cache"
            (cache / "lance").mkdir(parents=True)
            completeness = {
                "status": "not_ready",
                "graph_snapshot_digest": "graph-digest",
                "violations": [{"kind": "fixture"}],
            }
            (cache / "lsp_completeness.json").write_text(json.dumps(completeness))
            (cache / "lsp_pass1_work_items.json").write_text('{"finished":1}')
            (cache / "scan-state.json").write_text('{"files":1}')
            (cache / "operation_reports.json").write_text('{"reports":[]}')
            (cache / "lance" / "scan_version").write_text("7")
            cases = root / "cases"
            log = root / "logs" / "scan.log"
            log.parent.mkdir()
            log.write_text("failed scan")

            receipt_path = TOOLCHAIN._preserve_failed_case_evidence(
                checkout,
                "owner__repo-1",
                cases,
                log,
                TOOLCHAIN.ToolchainError("not ready"),
            )

            receipt = json.loads(receipt_path.read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertTrue(receipt["full_cache_retained"])
            self.assertIsNone(receipt["full_cache_error"])
            self.assertEqual(receipt["graph_snapshot_digest"], "graph-digest")
            self.assertEqual(receipt["scan_log_sha256"], TOOLCHAIN.sha256_file(log))
            self.assertEqual(receipt["publication_artifacts"], [])
            paths = {entry["cache_path"] for entry in receipt["evidence"]}
            self.assertEqual(
                paths,
                {
                    "lsp_completeness.json",
                    "lsp_pass1_work_items.json",
                    "scan-state.json",
                    "operation_reports.json",
                    "lance/scan_version",
                },
            )
            evidence_root = cases / "owner__repo-1-failure-evidence"
            self.assertEqual(
                json.loads((evidence_root / "lsp_completeness.json").read_text()),
                completeness,
            )
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "overwrite"):
                TOOLCHAIN._preserve_failed_case_evidence(
                    checkout,
                    "owner__repo-1",
                    cases,
                    log,
                    TOOLCHAIN.ToolchainError("second failure"),
                )

    def test_failed_publication_retains_ready_artifact_identities(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh/.cache"
            cache.mkdir(parents=True)
            (cache / "lsp_completeness.json").write_text(
                '{"graph_snapshot_digest":"graph"}'
            )
            cases = root / "cases"
            log = root / "publication.log"
            log.write_text("catalog publication failed")
            archive = root / "attempt.tar.gz"
            sidecar = root / "attempt.manifest.json"
            archive.write_bytes(b"archive")
            sidecar.write_bytes(b"manifest")

            receipt_path = TOOLCHAIN._preserve_failed_case_evidence(
                checkout,
                "owner__repo-1",
                cases,
                log,
                OSError("catalog publication failed"),
                publication_artifacts=[archive, sidecar],
            )

            receipt = json.loads(receipt_path.read_text())
            retained = {entry["path"]: entry for entry in receipt["publication_artifacts"]}
            self.assertEqual(set(retained), {str(archive.resolve()), str(sidecar.resolve())})
            self.assertEqual(retained[str(archive.resolve())]["sha256"], TOOLCHAIN.sha256_file(archive))
            self.assertEqual(retained[str(sidecar.resolve())]["sha256"], TOOLCHAIN.sha256_file(sidecar))

    def test_failed_case_evidence_retains_combined_cache_without_mutating_it(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh/.cache"
            graph = cache / "lance/graph.bin"
            vectors = cache / "embeddings/generations/generation/lance/vectors.bin"
            graph.parent.mkdir(parents=True)
            vectors.parent.mkdir(parents=True)
            graph.write_bytes(b"persisted graph")
            vectors.write_bytes(b"persisted vectors")
            before = {
                graph: TOOLCHAIN.sha256_file(graph),
                vectors: TOOLCHAIN.sha256_file(vectors),
            }
            log = root / "scan.log"
            log.write_text("post-scan failure")

            receipt_path = TOOLCHAIN._preserve_failed_case_evidence(
                checkout,
                "owner__repo-1",
                root / "cases",
                log,
                TOOLCHAIN.ToolchainError("query probe failed"),
            )

            receipt = json.loads(receipt_path.read_text())
            self.assertTrue(receipt["full_cache_retained"])
            self.assertIsNone(receipt["full_cache_error"])
            evidence = {entry["cache_path"]: entry for entry in receipt["evidence"]}
            self.assertEqual(
                set(evidence),
                {
                    "lance/graph.bin",
                    "embeddings/generations/generation/lance/vectors.bin",
                },
            )
            evidence_root = root / "cases/owner__repo-1-failure-evidence"
            for source, digest in before.items():
                relative = source.relative_to(cache)
                retained = evidence_root / relative
                self.assertEqual(TOOLCHAIN.sha256_file(source), digest)
                self.assertEqual(TOOLCHAIN.sha256_file(retained), digest)
                self.assertEqual(
                    evidence[relative.as_posix()]["size_bytes"], source.stat().st_size
                )

    def test_failed_case_evidence_rejects_symlinked_combined_cache_member(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh/.cache/embeddings"
            cache.mkdir(parents=True)
            target = root / "outside.bin"
            target.write_bytes(b"outside")
            (cache / "vectors.bin").symlink_to(target)
            log = root / "scan.log"
            log.write_text("post-scan failure")

            receipt_path = TOOLCHAIN._preserve_failed_case_evidence(
                checkout,
                "owner__repo-1",
                root / "cases",
                log,
                TOOLCHAIN.ToolchainError("query probe failed"),
            )

            receipt = json.loads(receipt_path.read_text())
            self.assertFalse(receipt["full_cache_retained"])
            self.assertIn("contains a symlink", receipt["full_cache_error"])
            self.assertFalse(
                (root / "cases/owner__repo-1-failure-evidence/embeddings/vectors.bin").exists()
            )

    def test_failed_case_evidence_rejects_symlinked_cache_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh/.cache"
            cache.mkdir(parents=True)
            outside = root / "outside"
            outside.mkdir()
            (outside / "scan_version").write_bytes(b"outside")
            (cache / "lance").symlink_to(outside, target_is_directory=True)
            log = root / "scan.log"
            log.write_text("post-scan failure")

            receipt_path = TOOLCHAIN._preserve_failed_case_evidence(
                checkout,
                "owner__repo-1",
                root / "cases",
                log,
                TOOLCHAIN.ToolchainError("query probe failed"),
            )

            receipt = json.loads(receipt_path.read_text())
            self.assertFalse(receipt["full_cache_retained"])
            self.assertIn("symlink", receipt["full_cache_error"])
            self.assertFalse(
                (root / "cases/owner__repo-1-failure-evidence/lance/scan_version").exists()
            )

    def test_run_logged_timeout_preserves_evidence_and_kills_process_group(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "timeout.log"
            evidence = root / "timeout-evidence.json"
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "timed out"):
                TOOLCHAIN._run_logged(
                    [
                        sys.executable,
                        "-c",
                        "import subprocess,time; subprocess.Popen([\"sleep\",\"30\"]); print(\"started\", flush=True); time.sleep(30)",
                    ],
                    root,
                    dict(os.environ),
                    log,
                    timeout_seconds=0.1,
                    timeout_evidence_path=evidence,
                )
            self.assertIn(b"started", log.read_bytes())
            self.assertIn(b"--- stderr ---", log.read_bytes())
            receipt = json.loads(evidence.read_text())
            self.assertEqual(receipt["status"], "timed_out")
            self.assertEqual(receipt["timeout_ms"], 100)
            self.assertEqual(receipt["log_sha256"], TOOLCHAIN.sha256_file(log))
            self.assertIn("SIGTERM", receipt["termination"])

    def test_exact_inventory_uses_frozen_trees_and_content_exclusions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            self.git(source, "init", "--quiet")
            self.git(source, "config", "user.name", "Fixture")
            self.git(source, "config", "user.email", "fixture@example.invalid")
            (source / "README").write_text("fixture docs\n")
            (source / "src").mkdir()
            (source / "src" / "main.py").write_text("VALUE = 1\n")
            (source / "tests").mkdir()
            (source / "tests" / "data.txt").write_text("1 2 3\n")
            (source / "tests" / "kernel.pyx").write_text("cdef int value = 1\n")
            (source / "src" / "uncommon.language").write_text("real source\n")
            (source / "tests" / "binary.dat").write_bytes(b"data\0payload")
            (source / "vendor").mkdir()
            (source / "vendor" / "ignored.py").write_text("IGNORED = True\n")
            (source / "pyproject.toml").write_text("[project]\nname='fixture'\n")
            self.git(source, "add", "--all")
            self.git(source, "commit", "--quiet", "-m", "fixture")
            commit = self.git(source, "rev-parse", "HEAD")

            cache = root / "git-cache"
            cache.mkdir()
            subprocess.run(
                ["git", "clone", "--quiet", "--bare", str(source), str(cache / "owner__repo.git")],
                check=True,
            )
            population_path = root / "population.json"
            population = {
                "schema_version": 1,
                "population_id": "fixture-n70",
                "instances": [
                    {
                        "instance_id": f"owner__repo-{index:02d}",
                        "repo": "owner/repo",
                        "base_commit": commit,
                        "included": True,
                    }
                    for index in range(70)
                ],
            }
            population_path.write_text(json.dumps(population))

            inventory = TOOLCHAIN.inventory_population(population_path, cache)

            self.assertEqual(inventory["included_instance_count"], 70)
            self.assertEqual(len(inventory["cases"]), 70)
            extensions = {
                entry["extension"]: entry for entry in inventory["extensions"]
            }
            self.assertEqual(extensions["py"]["file_count"], 70)
            self.assertNotIn("txt", extensions)
            self.assertEqual(extensions["pyx"]["roles"], {"test": 70})
            self.assertEqual(extensions["language"]["roles"], {"source": 70})
            self.assertEqual(extensions["<none>"]["roles"], {"docs": 70})
            self.assertEqual(extensions["toml"]["roles"], {"config": 70})
            languages = {
                entry["language"]: entry for entry in inventory["languages"]
            }
            self.assertEqual(languages["python"]["file_count"], 70)
            self.assertEqual(languages["cython"]["file_count"], 70)
            self.assertEqual(languages["cohort-text"]["file_count"], 70)
            first = inventory["cases"][0]
            self.assertEqual(first["tracked_file_count"], 8)
            self.assertEqual(first["included_file_count"], 5)
            self.assertEqual(
                first["exclusion_counts"],
                {"binary": 1, "non_language_data": 1, "vendor": 1},
            )

    def test_language_boundary_is_fail_closed_for_source_and_docs(self) -> None:
        cases = {
            "src/kernel.pyx": ("source", None),
            "tests/kernel.pyx": ("test", None),
            "docs/guide.rst": ("docs", None),
            "docs/notes.txt": ("docs", None),
            "requirements-dev.txt": ("config", None),
            "tests/data/table.txt": ("excluded_data", "non_language_data"),
            "tests/data/result.unknown": ("excluded_data", "non_language_data"),
            "src/unrecognized.lang": ("source", None),
        }
        for path, expected in cases.items():
            with self.subTest(path=path):
                role, exclusion, _ = TOOLCHAIN.classify_path(path, b"text\n")
                self.assertEqual((role, exclusion), expected)

    def test_verify_lock_fails_on_cache_drift_and_descriptor_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inventory_path = root / "inventory.json"
            inventory = {
                "schema_version": 1,
                "inventory_digest": "a" * 64,
                "languages": [
                    {"language": "python", "extensions": {"py": 1}},
                    {"language": "restructuredtext", "extensions": {"rst": 1}},
                ],
            }
            inventory_path.write_bytes(TOOLCHAIN.canonical_json(inventory))
            cache = root / "cache"
            cache.mkdir()
            artifact = cache / "server.tgz"
            artifact.write_bytes(b"locked artifact")
            parser_artifact = cache / "parser.tgz"
            parser_artifact.write_bytes(b"locked parser")
            parser_source = root / "fixture.py"
            parser_source.write_text("#!/usr/bin/env python3\n")
            entry = self.lock_entry(
                artifact_sha256=TOOLCHAIN.sha256_file(parser_artifact),
                languages=["python"],
            )
            entry["artifact"] = "parser.tgz"
            acquisition = self.write_repo_acquisition_contract(
                root,
                artifact="parser.tgz",
                artifact_sha256=TOOLCHAIN.sha256_file(parser_artifact),
                root_name="fixture-parser",
                sources=[
                    {
                        "path": "fixture.py",
                        "sha256": TOOLCHAIN.sha256_file(parser_source),
                        "destination": "fixture-server",
                    }
                ],
            )
            lock = {
                "acquisition": acquisition,
                "schema_version": 1,
                "platform": {"os": "macos", "architecture": "arm64"},
                "inventory_sha256": TOOLCHAIN.sha256_file(inventory_path),
                "inventory_digest": inventory["inventory_digest"],
                "repo_parser_bundle": {
                    "artifact": "parser.tgz",
                    "artifact_sha256": TOOLCHAIN.sha256_file(parser_artifact),
                    "sources": [
                        {
                            "path": "fixture.py",
                            "sha256": TOOLCHAIN.sha256_file(parser_source),
                        }
                    ],
                },
                "runtimes": [],
                "servers": [entry],
                "unsupported_languages": [
                    {
                        "language": "restructuredtext",
                        "reason": "fixture blocker",
                        "sample": "docs/index.rst",
                    }
                ],
            }
            lock_path = root / "lock.json"
            lock_path.write_bytes(TOOLCHAIN.canonical_json(lock))

            result = TOOLCHAIN.verify_lock(
                lock_path, inventory_path, cache, None, root
            )
            self.assertFalse(result["compatible"])
            self.assertEqual(result["unsupported_languages"], ["restructuredtext"])

            parser_artifact.write_bytes(b"drifted")
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "digest mismatch"):
                TOOLCHAIN.verify_lock(
                    lock_path, inventory_path, cache, None, root
                )

            parser_artifact.write_bytes(b"locked parser")
            descriptors_path = root / "descriptors.json"
            descriptors_path.write_text('{"schema_version":1,"servers":[]}')
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "language mismatch"):
                TOOLCHAIN.verify_lock(
                    lock_path, inventory_path, cache, descriptors_path, root
                )

            descriptor_inventory = {
                "schema_version": 1,
                "servers": [
                    {
                        "languages": ["python"],
                        "extensions": ["py"],
                        "command": "server",
                        "args": ["--stdio"],
                    }
                ],
            }
            descriptors_path.write_text(json.dumps(descriptor_inventory))
            result = TOOLCHAIN.verify_lock(
                lock_path, inventory_path, cache, descriptors_path, root
            )
            self.assertTrue(result["descriptors_verified"])

            descriptor_inventory["servers"][0]["command"] = "wrong-server"
            descriptors_path.write_text(json.dumps(descriptor_inventory))
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "profile mismatch"):
                TOOLCHAIN.verify_lock(
                    lock_path, inventory_path, cache, descriptors_path, root
                )

    def test_verify_lock_rejects_unknown_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inventory_path = root / "inventory.json"
            inventory = {
                "schema_version": 1,
                "inventory_digest": "a" * 64,
                "languages": [],
            }
            inventory_path.write_bytes(TOOLCHAIN.canonical_json(inventory))
            lock = {
                "schema_version": 1,
                "platform": {"os": "macos", "architecture": "arm64"},
                "inventory_sha256": TOOLCHAIN.sha256_file(inventory_path),
                "inventory_digest": inventory["inventory_digest"],
                "runtimes": [],
                "servers": [],
                "unsupported_languages": [],
                "unexpected": True,
            }
            lock_path = root / "lock.json"
            lock_path.write_bytes(TOOLCHAIN.canonical_json(lock))
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "top-level fields"):
                TOOLCHAIN.verify_lock(lock_path, inventory_path, None, None)

    def test_real_acquisition_contract_and_repo_sources_are_verifier_clean(self) -> None:
        lock = TOOLCHAIN.load_json_object(
            ROOT / "benchmark/swebench-act-context/lsp-toolchain/toolchain-lock.json",
            "toolchain lock",
        )
        python_server = next(
            server for server in lock["servers"] if server["languages"] == ["python"]
        )
        self.assertEqual(python_server["name"], "pyrefly")
        self.assertEqual(python_server["version"], "1.1.1")
        self.assertEqual(
            python_server["artifact_sha256"],
            "d6b238e1362622d47a6eb5af704fd8b613c94e8c303386efd6350e3da59fecc8",
        )
        self.assertEqual(
            python_server["executable_sha256"],
            "d471718bb618c4e6e7c30549da6efdd8eca8abea138dc1dec1524564bc4da396",
        )
        result = TOOLCHAIN.verify_lock(
            ROOT / "benchmark/swebench-act-context/lsp-toolchain/toolchain-lock.json",
            ROOT / "benchmark/swebench-act-context/lsp-toolchain/inventory.json",
            None,
            ROOT
            / "benchmark/swebench-act-context/lsp-toolchain/descriptor-inventory.json",
            ROOT,
        )
        self.assertTrue(result["compatible"])
        self.assertTrue(result["repository_sources_verified"])
        self.assertEqual(result["acquisition_recipe_count"], 4)
        self.assertEqual(result["acquisition_artifact_count"], 14)

    def test_real_generated_launcher_digests_match_lock(self) -> None:
        lock = TOOLCHAIN.load_json_object(
            ROOT / "benchmark/swebench-act-context/lsp-toolchain/toolchain-lock.json",
            "toolchain lock",
        )
        checked = []
        for entry in lock["servers"]:
            launcher = entry["launcher"]
            if launcher["kind"] == "direct":
                continue
            generated = TOOLCHAIN._wrapper(
                launcher["kind"],
                launcher["target"],
                command_name=entry["command"],
                version=entry["version"],
            )
            self.assertEqual(
                TOOLCHAIN.sha256_bytes(generated),
                entry["executable_sha256"],
                entry["command"],
            )
            checked.append(entry["command"])
        self.assertGreaterEqual(len(checked), 10)

    def test_empty_cache_repo_recipe_verifies_and_provisions_offline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "fixture_server.py"
            source.write_text("#!/usr/bin/env python3\nprint('fixture')\n")
            source_digest = TOOLCHAIN.sha256_file(source)

            expected_root = root / "expected-tree"
            expected_root.mkdir()
            expected_server = expected_root / "server"
            expected_server.write_bytes(source.read_bytes())
            expected_server.chmod(0o755)
            expected_archive = root / "expected.tar.gz"
            TOOLCHAIN.seal_directory(
                expected_root, expected_archive, "fixture-bundle"
            )
            artifact_digest = TOOLCHAIN.sha256_file(expected_archive)
            acquisition = self.write_repo_acquisition_contract(
                root,
                artifact="fixture-bundle.tar.gz",
                artifact_sha256=artifact_digest,
                root_name="fixture-bundle",
                sources=[
                    {
                        "path": source.name,
                        "sha256": source_digest,
                        "destination": "server",
                    }
                ],
            )
            inventory = {
                "schema_version": 1,
                "inventory_digest": "a" * 64,
                "languages": [{"language": "python", "extensions": {"py": 1}}],
            }
            inventory_path = root / "inventory.json"
            inventory_path.write_bytes(TOOLCHAIN.canonical_json(inventory))
            entry = self.lock_entry(
                artifact_sha256=artifact_digest, languages=["python"]
            )
            entry["artifact"] = "fixture-bundle.tar.gz"
            entry["launcher"] = {
                "kind": "repo-python",
                "target": "servers/fixture-bundle/server",
            }
            wrapper_bytes = TOOLCHAIN._wrapper(
                "repo-python", "servers/fixture-bundle/server"
            )
            entry["executable_sha256"] = TOOLCHAIN.sha256_bytes(wrapper_bytes)
            entry["install"] = {
                "kind": "tar",
                "destination": "servers",
                "member": "",
            }
            lock = {
                "acquisition": acquisition,
                "schema_version": 1,
                "platform": {"os": "macos", "architecture": "arm64"},
                "inventory_sha256": TOOLCHAIN.sha256_file(inventory_path),
                "inventory_digest": inventory["inventory_digest"],
                "repo_parser_bundle": {
                    "artifact": "fixture-bundle.tar.gz",
                    "artifact_sha256": artifact_digest,
                    "sources": [{"path": source.name, "sha256": source_digest}],
                },
                "runtimes": [],
                "servers": [entry],
                "unsupported_languages": [],
            }
            lock_path = root / "lock.json"
            lock_path.write_bytes(TOOLCHAIN.canonical_json(lock))
            cache = root / "empty-cache"
            with mock.patch.object(
                TOOLCHAIN.urllib.request,
                "urlopen",
                side_effect=AssertionError("network must not be used"),
            ):
                acquisition_result = TOOLCHAIN.acquire_artifacts(
                    lock_path, cache, root
                )
            self.assertEqual(acquisition_result["downloaded"], 0)
            self.assertEqual(acquisition_result["built"], 1)
            self.assertEqual(
                TOOLCHAIN.sha256_file(cache / "fixture-bundle.tar.gz"),
                artifact_digest,
            )
            verification = TOOLCHAIN.verify_lock(
                lock_path, inventory_path, cache, None, root
            )
            self.assertTrue(verification["cache_verified"])

            toolchain_root = root / "toolchain"
            receipt_path = root / "provision.json"
            receipt = TOOLCHAIN.provision_toolchain(
                lock_path,
                inventory_path,
                cache,
                toolchain_root,
                receipt_path,
                offline=True,
                repo_root=root,
            )
            self.assertTrue(receipt["offline"])
            self.assertEqual(
                TOOLCHAIN.sha256_file(toolchain_root / "bin/server"),
                TOOLCHAIN.sha256_bytes(wrapper_bytes),
            )
            provisioned_identity = TOOLCHAIN._validate_provisioned_toolchain(
                lock_path, inventory_path, toolchain_root
            )
            self.assertEqual(
                provisioned_identity["toolchain_root"], str(toolchain_root.resolve())
            )
            self.assertEqual(
                provisioned_identity["inventory_sha256"],
                TOOLCHAIN.sha256_file(inventory_path),
            )
            self.assertEqual(len(provisioned_identity["installed"]), 1)
            self.assertEqual(len(provisioned_identity["launchers"]), 1)

            launcher = toolchain_root / "bin/server"
            launcher.unlink()
            launcher.write_text("#!/bin/sh\nexit 1\n")
            launcher.chmod(0o755)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "executable digest mismatch"
            ):
                TOOLCHAIN._validate_provisioned_toolchain(
                    lock_path, inventory_path, toolchain_root
                )
            launcher.unlink()
            launcher.write_bytes(wrapper_bytes)
            launcher.chmod(0o755)
            self.assertEqual(
                TOOLCHAIN._validate_provisioned_toolchain(
                    lock_path, inventory_path, toolchain_root
                ),
                provisioned_identity,
            )
            installed_executable = (
                toolchain_root / "servers/fixture-bundle/server"
            )
            installed_bytes = installed_executable.read_bytes()
            installed_executable.write_bytes(b"tampered installed executable\n")
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "launcher target digest mismatch"
            ):
                TOOLCHAIN._validate_provisioned_toolchain(
                    lock_path, inventory_path, toolchain_root
                )
            installed_executable.write_bytes(installed_bytes)
            installed_executable.chmod(0o755)
            self.assertEqual(
                TOOLCHAIN._validate_provisioned_toolchain(
                    lock_path, inventory_path, toolchain_root
                ),
                provisioned_identity,
            )

            source.write_text("drifted\n")
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "source digest mismatch"
            ):
                TOOLCHAIN.verify_lock(
                    lock_path, inventory_path, cache, None, root
                )

    def test_toolchain_environment_is_closed_and_uses_isolated_state(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            toolchain_root = root / "toolchain"
            isolation_root = root / "isolation"
            hostile_environment = {
                "PATH": "/host/bin",
                "HOME": "/host/home",
                "PYTHONPATH": "/host/python",
                "NODE_OPTIONS": "--require=/host/inject.js",
                "JAVA_TOOL_OPTIONS": "-javaagent:/host/inject.jar",
                "DYLD_INSERT_LIBRARIES": "/host/inject.dylib",
                "AWS_SECRET_ACCESS_KEY": "must-not-propagate",
            }
            with mock.patch.dict(os.environ, hostile_environment, clear=True):
                environment = TOOLCHAIN.toolchain_environment(
                    toolchain_root, isolation_root
                )

            self.assertNotIn("PYTHONPATH", environment)
            self.assertNotIn("NODE_OPTIONS", environment)
            self.assertNotIn("JAVA_TOOL_OPTIONS", environment)
            self.assertNotIn("DYLD_INSERT_LIBRARIES", environment)
            self.assertNotIn("AWS_SECRET_ACCESS_KEY", environment)
            self.assertEqual(environment["LANG"], "C")
            self.assertEqual(environment["LC_ALL"], "C")
            self.assertEqual(environment["TZ"], "UTC")
            self.assertEqual(environment["NODE_DISABLE_COMPILE_CACHE"], "1")
            self.assertEqual(environment["GIT_CONFIG_NOSYSTEM"], "1")
            self.assertEqual(environment["GIT_CONFIG_GLOBAL"], os.devnull)
            for key in (
                "HOME",
                "TMPDIR",
                "XDG_CONFIG_HOME",
                "XDG_CACHE_HOME",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
                "PIP_CACHE_DIR",
                "npm_config_cache",
            ):
                path = Path(environment[key])
                self.assertTrue(path.is_dir(), key)
                self.assertTrue(path.is_relative_to(isolation_root.resolve()), key)

    def test_hf_default_cache_binding_projects_read_only_cache_without_mutation(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            hf_home = root / "verified-bundle/components/models/huggingface"
            source_hub = hf_home / "hub"
            source_hub.mkdir(parents=True)
            source_file = source_hub / "sentinel"
            source_file.write_bytes(b"verified model bytes")
            isolated_home = root / "isolated/home"
            isolated_home.mkdir(parents=True)

            source_file.chmod(0o444)
            source_hub.chmod(0o555)
            hf_home.chmod(0o555)
            before = (
                source_file.read_bytes(),
                source_file.stat().st_mtime_ns,
                source_file.stat().st_mode,
                source_hub.stat().st_mode,
                hf_home.stat().st_mode,
            )
            try:
                receipt = TOOLCHAIN.bind_hf_default_cache(hf_home, isolated_home)
                default_hub = isolated_home / ".cache/huggingface/hub"
                self.assertTrue(default_hub.is_symlink())
                self.assertEqual(default_hub.resolve(strict=True), source_hub.resolve())
                self.assertEqual(
                    (default_hub / "sentinel").read_bytes(), b"verified model bytes"
                )
                self.assertEqual(
                    receipt,
                    {
                        "schema_version": TOOLCHAIN.SCHEMA_VERSION,
                        "status": "bound",
                        "binding": "symlink",
                        "default_cache_relative_path": ".cache/huggingface/hub",
                        "hf_home_relative_path": "hub",
                    },
                )
                after = (
                    source_file.read_bytes(),
                    source_file.stat().st_mtime_ns,
                    source_file.stat().st_mode,
                    source_hub.stat().st_mode,
                    hf_home.stat().st_mode,
                )
                self.assertEqual(after, before)
            finally:
                hf_home.chmod(0o755)
                source_hub.chmod(0o755)
                source_file.chmod(0o644)

    def test_hf_default_cache_binding_fails_closed_on_unsafe_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            missing_hf_home = root / "missing-hub"
            missing_hf_home.mkdir()
            missing_home = root / "missing-home"
            missing_home.mkdir()
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "hub must be"):
                TOOLCHAIN.bind_hf_default_cache(missing_hf_home, missing_home)

            symlink_hf_home = root / "symlink-hf-home"
            symlink_hf_home.mkdir()
            real_hub = root / "real-hub"
            real_hub.mkdir()
            (symlink_hf_home / "hub").symlink_to(
                real_hub, target_is_directory=True
            )
            symlink_home = root / "symlink-home"
            symlink_home.mkdir()
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "hub must be"):
                TOOLCHAIN.bind_hf_default_cache(symlink_hf_home, symlink_home)

            occupied_hf_home = root / "occupied-hf-home"
            (occupied_hf_home / "hub").mkdir(parents=True)
            occupied_home = root / "occupied-home"
            (occupied_home / ".cache/huggingface/hub").mkdir(parents=True)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "destination must be absent"
            ):
                TOOLCHAIN.bind_hf_default_cache(occupied_hf_home, occupied_home)

            parent_hf_home = root / "parent-hf-home"
            (parent_hf_home / "hub").mkdir(parents=True)
            parent_home = root / "parent-home"
            parent_home.mkdir()
            redirected_cache = root / "redirected-cache"
            redirected_cache.mkdir()
            (parent_home / ".cache").symlink_to(
                redirected_cache, target_is_directory=True
            )
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "cache must be a real directory"
            ):
                TOOLCHAIN.bind_hf_default_cache(parent_hf_home, parent_home)

    def test_hf_default_cache_binding_cli(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            hf_home = root / "hf-home"
            (hf_home / "hub").mkdir(parents=True)
            isolated_home = root / "home"
            isolated_home.mkdir()
            output = io.StringIO()
            with mock.patch("sys.stdout", output):
                status = TOOLCHAIN.main(
                    [
                        "bind-hf-default-cache",
                        "--hf-home",
                        str(hf_home),
                        "--home",
                        str(isolated_home),
                    ]
                )
            self.assertEqual(status, 0)
            self.assertEqual(json.loads(output.getvalue())["status"], "bound")
            self.assertTrue((isolated_home / ".cache/huggingface/hub").is_symlink())

    def test_semantic_bundle_offline_environment_contract(self) -> None:
        workflow = (
            ROOT / ".github/workflows/swebench-semantic-bundle.yml"
        ).read_text()
        source = MODULE_PATH.read_text()
        self.assertEqual(workflow.count("bind-hf-default-cache"), 2)
        self.assertIn(
            "            unset HF_HOME\n"
            '            "$BUNDLE_ROOT/repo-native-alignment" search \\\n',
            workflow,
        )
        self.assertIn("components: rust-analyzer", workflow)
        self.assertIn(
            'RUST_ANALYZER_BIN="$(rustup which --toolchain 1.97.0 rust-analyzer)"',
            workflow,
        )
        self.assertIn(
            'export PATH="$RUST_TOOLCHAIN_BIN:$LSP_ROOT/bin:', workflow
        )
        self.assertIn("offline-rust-analyzer-version.txt", workflow)
        self.assertIn("offline-rust-analyzer-sha256.txt", workflow)
        self.assertIn(
            'if re.fullmatch(r"[0-9a-f]{64}", artifact_digest):', workflow
        )
        self.assertIn(
            'artifact_digest = f"sha256:{artifact_digest}"', workflow
        )
        self.assertIn(
            'if re.fullmatch(r"sha256:[0-9a-f]{64}", artifact_digest) is None:',
            workflow,
        )
        self.assertIn('"artifact_digest": artifact_digest', workflow)
        self.assertIn(
            '            "$BUNDLE_ROOT/repo-native-alignment" \\\n'
            "              --business-context disabled \\\n"
            "              search \\\n",
            workflow,
        )
        self.assertIn('"CANDLE_METAL_ENABLE_FAST_MATH": "1"', source)
        self.assertIn(
            'bind_hf_default_cache(\n            Path(environment["HF_HOME"]), '
            'Path(environment["HOME"])\n        )',
            source,
        )

    def test_probe_server_requests_use_initialized_workspace_and_sections(self) -> None:
        rpc = object.__new__(TOOLCHAIN.JsonRpcProcess)
        rpc.next_id = 1
        rpc.configuration = {
            "python": {"analysis": {"typeCheckingMode": "strict"}},
            "feature": True,
        }
        rpc.workspace_folders = []
        rpc.send = mock.Mock()
        rpc.receive = mock.Mock(return_value={"id": 1, "result": {}})
        folder = {"uri": "file:///fixture", "name": "fixture"}
        rpc.request(
            "initialize",
            {"workspaceFolders": [folder], "capabilities": {}},
            0.1,
        )
        self.assertEqual(
            rpc._server_request_result(
                {"method": "workspace/workspaceFolders", "params": {}}
            ),
            [folder],
        )
        self.assertEqual(
            rpc._server_request_result(
                {
                    "method": "workspace/configuration",
                    "params": {
                        "items": [
                            {"section": "python.analysis"},
                            {"section": "feature"},
                            {"section": "missing.section"},
                            {"scopeUri": "file:///fixture"},
                        ]
                    },
                }
            ),
            [
                {"typeCheckingMode": "strict"},
                True,
                None,
                rpc.configuration,
            ],
        )

    def test_probe_evidence_binds_stable_identity_across_extraction_roots(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock_path = root / "lock.json"
            expected_capabilities = [
                method
                for _, methods in TOOLCHAIN.OPERATION_CAPABILITY_PROVIDERS
                for method in methods
            ]
            lock_server = {
                "name": "pyrefly",
                "version": "1.1.1",
                "languages": ["python"],
                "command": "pyrefly",
                "args": ["lsp", "--threads", "1"],
                "executable": "bin/pyrefly",
                "executable_sha256": "a" * 64,
                "expected_capabilities": expected_capabilities,
                "probe": {"operation": "textDocument/documentSymbol"},
            }
            TOOLCHAIN.write_canonical_json(lock_path, {"servers": [lock_server]})
            operation_evidence = [
                {
                    "provider": provider,
                    "present": True,
                    "advertised_value": True,
                    "supported": True,
                    "methods": list(methods),
                }
                for provider, methods in TOOLCHAIN.OPERATION_CAPABILITY_PROVIDERS
            ]
            server_receipt = {
                "name": lock_server["name"],
                "version": lock_server["version"],
                "languages": lock_server["languages"],
                "command": lock_server["command"],
                "args": lock_server["args"],
                "executable": lock_server["executable"],
                "executable_sha256": lock_server["executable_sha256"],
                "negotiated_capabilities": expected_capabilities,
                "negotiated_operation_capabilities": operation_evidence,
                "operation": "textDocument/documentSymbol",
                "result_count": 1,
                "initialize_ms": 1,
                "workspace_ready_ms": 2,
                "operation_ms": 1,
                "shutdown_ms": 1,
                "quiescence_messages": 0,
                "stderr_tail": "",
                "status": "ready",
            }
            provisioned_identity = {
                "toolchain_root": str((root / "toolchain").resolve()),
                "inventory_sha256": "b" * 64,
                "provision_receipt_digest": "c" * 64,
                "provision_receipt_sha256": "d" * 64,
                "installed": [
                    {
                        "name": "pyrefly",
                        "executable": "bin/pyrefly",
                        "sha256": "a" * 64,
                    }
                ],
                "launchers": [
                    {
                        "name": "pyrefly",
                        "command": "pyrefly",
                        "path": "bin/pyrefly",
                        "sha256": "a" * 64,
                        "target_path": "bin/pyrefly",
                        "target_sha256": "a" * 64,
                    }
                ],
            }

            def publish_probe(receipt: dict[str, object]) -> Path:
                path = root / "probe.json"
                probe = {
                    "schema_version": TOOLCHAIN.SCHEMA_VERSION,
                    "lock_sha256": TOOLCHAIN.sha256_file(lock_path),
                    **provisioned_identity,
                    "server_count": 1,
                    "servers": [receipt],
                }
                probe["probe_digest"] = TOOLCHAIN.sha256_bytes(
                    TOOLCHAIN.canonical_json(probe)
                )
                TOOLCHAIN.write_canonical_json(path, probe)
                return path

            probe_path = publish_probe(server_receipt)
            TOOLCHAIN._validate_toolchain_probe_evidence(
                probe_path, lock_path, provisioned_identity
            )

            relocated_verifier_clean_identity = copy.deepcopy(provisioned_identity)
            relocated_verifier_clean_identity["toolchain_root"] = str(
                (root / "different-bundle-extraction" / "toolchain").resolve()
            )
            TOOLCHAIN._validate_toolchain_probe_evidence(
                probe_path, lock_path, relocated_verifier_clean_identity
            )

            relative_recorded_root = TOOLCHAIN.load_json_object(
                probe_path, "test probe"
            )
            relative_recorded_root["toolchain_root"] = "relative/toolchain"
            relative_recorded_root.pop("probe_digest")
            relative_recorded_root["probe_digest"] = TOOLCHAIN.sha256_bytes(
                TOOLCHAIN.canonical_json(relative_recorded_root)
            )
            TOOLCHAIN.write_canonical_json(probe_path, relative_recorded_root)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError,
                "toolchain probe roots must be absolute paths",
            ):
                TOOLCHAIN._validate_toolchain_probe_evidence(
                    probe_path, lock_path, provisioned_identity
                )

            probe_path = publish_probe(server_receipt)
            relative_current_root = copy.deepcopy(provisioned_identity)
            relative_current_root["toolchain_root"] = "relative/toolchain"
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError,
                "toolchain probe roots must be absolute paths",
            ):
                TOOLCHAIN._validate_toolchain_probe_evidence(
                    probe_path, lock_path, relative_current_root
                )

            stale_pyright = copy.deepcopy(server_receipt)
            stale_pyright.update(
                {
                    "name": "pyright",
                    "version": "1.1.405",
                    "command": "pyright-langserver",
                    "args": ["--stdio"],
                }
            )
            publish_probe(stale_pyright)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "server identity mismatch"
            ):
                TOOLCHAIN._validate_toolchain_probe_evidence(
                    probe_path, lock_path, provisioned_identity
                )

            probe_path = publish_probe(server_receipt)
            tampered = TOOLCHAIN.load_json_object(probe_path, "test probe")
            tampered["servers"][0]["status"] = "blocked"
            TOOLCHAIN.write_canonical_json(probe_path, tampered)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "self-digest mismatch"
            ):
                TOOLCHAIN._validate_toolchain_probe_evidence(
                    probe_path, lock_path, provisioned_identity
                )

            probe_path = publish_probe(server_receipt)
            wrong_provision = copy.deepcopy(provisioned_identity)
            wrong_provision["provision_receipt_sha256"] = "e" * 64
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "lock/server identity mismatch"
            ):
                TOOLCHAIN._validate_toolchain_probe_evidence(
                    probe_path, lock_path, wrong_provision
                )

    def test_probe_rejects_toolchain_mutation_during_last_server(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock_path = root / "lock.json"
            inventory_path = root / "inventory.json"
            output_path = root / "probe.json"
            TOOLCHAIN.write_canonical_json(
                lock_path, {"servers": [{"name": "pyrefly"}]}
            )
            TOOLCHAIN.write_canonical_json(inventory_path, {})
            identity = {
                "toolchain_root": str((root / "toolchain").resolve()),
                "inventory_sha256": "a" * 64,
                "provision_receipt_digest": "b" * 64,
                "provision_receipt_sha256": "c" * 64,
                "installed": [],
                "launchers": [],
            }
            changed_identity = copy.deepcopy(identity)
            changed_identity["provision_receipt_sha256"] = "d" * 64
            with (
                mock.patch.object(
                    TOOLCHAIN,
                    "verify_lock",
                    return_value={"compatible": True},
                ),
                mock.patch.object(
                    TOOLCHAIN,
                    "_validate_provisioned_toolchain",
                    side_effect=[identity, identity, changed_identity],
                ),
                mock.patch.object(
                    TOOLCHAIN,
                    "probe_server",
                    return_value={"name": "pyrefly", "status": "ready"},
                ),
            ):
                with self.assertRaisesRegex(
                    TOOLCHAIN.ToolchainError, "identity changed during probe"
                ):
                    TOOLCHAIN.probe_toolchain(
                        lock_path,
                        inventory_path,
                        root / "toolchain",
                        output_path,
                        1.0,
                    )
            self.assertFalse(output_path.exists())

    def test_json_rpc_partial_body_obeys_receive_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            server_code = "\n".join(
                [
                    "import sys, time",
                    "sys.stdout.buffer.write(b'Content-Length: 10\\r\\n\\r\\n{}')",
                    "sys.stdout.buffer.flush()",
                    "time.sleep(30)",
                ]
            )
            rpc = TOOLCHAIN.JsonRpcProcess(
                [sys.executable, "-c", server_code],
                Path(temporary),
                dict(os.environ),
            )
            started = time.monotonic()
            try:
                with self.assertRaisesRegex(
                    TOOLCHAIN.ToolchainError, "response timed out"
                ):
                    rpc.receive(0.1)
                self.assertLess(time.monotonic() - started, 1.0)
            finally:
                rpc.process.kill()
                rpc.process.wait()
                if rpc.process.stdin:
                    rpc.process.stdin.close()
                if rpc.process.stdout:
                    rpc.process.stdout.close()
                if rpc.process.stderr:
                    rpc.process.stderr.close()

    def test_non_toolchain_archive_failure_preserves_failure_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh/.cache"
            cache.mkdir(parents=True)
            (cache / "lsp_completeness.json").write_text(
                '{"graph_snapshot_digest":"graph"}'
            )
            cases = root / "cases"
            cases.mkdir()
            log = root / "scan.log"
            log.write_text("scan complete")
            archive = root / "archive.tar.gz"
            sidecar = root / "archive.manifest.json"
            archive.write_bytes(b"partial archive")
            sidecar.write_bytes(b"partial sidecar")
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "failure evidence="
            ):
                TOOLCHAIN._raise_archive_failure(
                    checkout=checkout,
                    instance_id="owner__repo-1",
                    cases_root=cases,
                    scan_log_path=log,
                    error=OSError("disk full"),
                    attempt_slug="owner__repo-1-attempt-001",
                    archive_path=archive,
                    sidecar_path=sidecar,
                )
            receipt_path = cases / "owner__repo-1-attempt-001-failure.json"
            receipt = json.loads(receipt_path.read_text())
            self.assertEqual(receipt["error"], "disk full")
            self.assertEqual(len(receipt["publication_artifacts"]), 2)

    def test_structural_archive_is_deterministic_verifier_clean_and_immutable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, identity = self.structural_cache_fixture(root)
            first_archive = root / "first.tar.gz"
            first_sidecar = root / "first.manifest.json"
            second_archive = root / "second.tar.gz"
            second_sidecar = root / "second.manifest.json"
            first = self.archive_fixture(
                checkout, identity, first_archive, first_sidecar
            )
            second = self.archive_fixture(
                checkout, identity, second_archive, second_sidecar
            )
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "overwrite"):
                self.archive_fixture(
                    checkout, identity, first_archive, first_sidecar
                )

            self.assertEqual(first_archive.read_bytes(), second_archive.read_bytes())
            self.assertEqual(first["archive_sha256"], second["archive_sha256"])
            archive_before = first_archive.read_bytes()
            sidecar_before = first_sidecar.read_bytes()
            injected = root / "injected"
            injected.mkdir()
            verified = TOOLCHAIN.verify_structural_cache_archive(
                first_archive,
                first_sidecar,
                expected=self.cache_expected(identity),
                inject_checkout=injected,
            )
            self.assertEqual(
                verified["structural_cache_tree_digest"],
                first["structural_cache_tree_digest"],
            )
            self.assertTrue(
                (injected / ".oh/.cache/lsp_completeness.json").is_file()
            )
            self.assertEqual(first_archive.read_bytes(), archive_before)
            self.assertEqual(first_sidecar.read_bytes(), sidecar_before)

    def test_structural_archive_requires_real_producer_and_verifies_before_ready_sidecar(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, identity = self.structural_cache_fixture(root)
            archive = root / "base.tar.gz"
            sidecar = root / "base.manifest.json"

            unknown = copy.deepcopy(identity)
            unknown["producer"]["producer_commit"] = "unknown"
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "40-character"):
                self.archive_fixture(checkout, unknown, archive, sidecar)
            self.assertFalse(archive.exists())
            self.assertFalse(sidecar.exists())

            with mock.patch.object(
                TOOLCHAIN,
                "verify_structural_cache_archive",
                side_effect=TOOLCHAIN.ToolchainError("fixture verification failure"),
            ):
                with self.assertRaisesRegex(
                    TOOLCHAIN.ToolchainError, "fixture verification failure"
                ):
                    self.archive_fixture(checkout, identity, archive, sidecar)
            self.assertTrue(archive.is_file())
            self.assertFalse(
                sidecar.exists(),
                "READY publication marker must remain absent until verification succeeds",
            )

    def test_structural_archive_rejects_tamper_traversal_links_partial_and_embeddings(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, identity = self.structural_cache_fixture(root)
            archive = root / "base.tar.gz"
            sidecar = root / "base.manifest.json"
            self.archive_fixture(checkout, identity, archive, sidecar)

            original_archive = archive.read_bytes()
            archive.write_bytes(original_archive + b"tamper")
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "partial|changed size|digest"
            ):
                TOOLCHAIN.verify_structural_cache_archive(archive, sidecar)
            archive.write_bytes(original_archive)

            original_sidecar = json.loads(sidecar.read_text())
            tampered_sidecar = copy.deepcopy(original_sidecar)
            tampered_sidecar["core"]["configuration_digest"] = "tampered"
            TOOLCHAIN.write_canonical_json(sidecar, tampered_sidecar)
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "core digest"):
                TOOLCHAIN.verify_structural_cache_archive(archive, sidecar)
            TOOLCHAIN.write_canonical_json(sidecar, original_sidecar)

            linked_sidecar = root / "linked.manifest.json"
            linked_sidecar.symlink_to(sidecar)
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "symlink"):
                TOOLCHAIN.verify_structural_cache_archive(archive, linked_sidecar)

            redirected_checkout = root / "redirected-checkout"
            redirected_checkout.mkdir()
            outside = root / "outside"
            outside.mkdir()
            (redirected_checkout / ".oh").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "symlink/non-directory .oh"
            ):
                TOOLCHAIN.verify_structural_cache_archive(
                    archive, sidecar, inject_checkout=redirected_checkout
                )

            hostile_cases = {
                "traversal": [("file", "cache/../escape", b"escape")],
                "symlink": [("symlink", "cache/link", b"")],
                "partial": [
                    (
                        "file",
                        f"cache/{TOOLCHAIN.STRUCTURAL_CACHE_CORE}",
                        TOOLCHAIN.canonical_json(original_sidecar["core"]),
                    )
                ],
            }
            for label, members in hostile_cases.items():
                with self.subTest(label=label):
                    hostile_archive = root / f"{label}.tar.gz"
                    hostile_sidecar = root / f"{label}.manifest.json"
                    self.write_hostile_archive(
                        hostile_archive,
                        hostile_sidecar,
                        original_sidecar["core"],
                        members,
                    )
                    with self.assertRaises(TOOLCHAIN.ToolchainError):
                        TOOLCHAIN.verify_structural_cache_archive(
                            hostile_archive, hostile_sidecar
                        )

            nested_semantic_archive = root / "nested-semantic.tar.gz"
            nested_semantic_sidecar = root / "nested-semantic.manifest.json"
            self.write_hostile_archive(
                nested_semantic_archive,
                nested_semantic_sidecar,
                original_sidecar["core"],
                [
                    (
                        "file",
                        "cache/lance/artifacts.lance/data/0000000000000001.lance",
                        b"forbidden",
                    )
                ],
            )
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "embeddings/rerank"
            ):
                TOOLCHAIN.verify_structural_cache_archive(
                    nested_semantic_archive, nested_semantic_sidecar
                )

            embeddings = checkout / ".oh/.cache/embeddings/index.bin"
            embeddings.parent.mkdir(parents=True)
            embeddings.write_bytes(b"forbidden")
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "embeddings/rerank"
            ):
                TOOLCHAIN.archive_structural_cache(
                    checkout,
                    root / "forbidden.tar.gz",
                    root / "forbidden.manifest.json",
                    identity=identity,
                    toolchain_lock_digest="a" * 64,
                    inventory_digest="b" * 64,
                    inventory_file_sha256="c" * 64,
                    case_inventory_digest="d" * 64,
                    base_cache=None,
                )
            embeddings.unlink()

            semantic_artifacts = checkout / ".oh/.cache/lance/artifacts.lance"
            semantic_data = semantic_artifacts / "data"
            semantic_data.mkdir(parents=True)
            (semantic_data / "0000000000000001.lance").write_bytes(b"forbidden")
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "embeddings/rerank"
            ):
                TOOLCHAIN.archive_structural_cache(
                    checkout,
                    root / "forbidden-artifacts.tar.gz",
                    root / "forbidden-artifacts.manifest.json",
                    identity=identity,
                    toolchain_lock_digest="a" * 64,
                    inventory_digest="b" * 64,
                    inventory_file_sha256="c" * 64,
                    case_inventory_digest="d" * 64,
                    base_cache=None,
                )

    def test_cache_selection_mismatches_rebuild_and_partition_changes_invalidate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            self.git(source, "init", "--quiet")
            self.git(source, "config", "user.name", "Fixture")
            self.git(source, "config", "user.email", "fixture@example.invalid")
            (source / "src").mkdir()
            (source / "src/a.py").write_text("VALUE = 1\n")
            self.git(source, "add", "--all")
            self.git(source, "commit", "--quiet", "-m", "base")
            base_commit = self.git(source, "rev-parse", "HEAD")
            base_tree = self.git(source, "rev-parse", "HEAD^{tree}")
            (source / "src/a.py").write_text("VALUE = 2\n")
            self.git(source, "commit", "--quiet", "-am", "target")
            target_commit = self.git(source, "rev-parse", "HEAD")
            target_tree = self.git(source, "rev-parse", "HEAD^{tree}")
            git_dir = root / "owner__repo.git"
            subprocess.run(
                ["git", "clone", "--quiet", "--bare", str(source), str(git_dir)],
                check=True,
            )

            checkout, base_identity = self.structural_cache_fixture(root)
            base_identity["commit"] = base_commit
            base_identity["tree"] = base_tree
            archive = root / "base.tar.gz"
            sidecar = root / "base.manifest.json"
            archived = self.archive_fixture(checkout, base_identity, archive, sidecar)
            output = root / "output"
            output.mkdir()
            entry = {
                "schema_version": TOOLCHAIN.STRUCTURAL_CACHE_SCHEMA_VERSION,
                "status": "ready",
                "case_index": 1,
                "instance_id": "owner__repo-1",
                "repository": "owner/repo",
                "archive_path": str(archive),
                "archive_sha256": archived["archive_sha256"],
                "sidecar_path": str(sidecar),
                "sidecar_sha256": archived["sidecar_sha256"],
                "core_sha256": archived["core_sha256"],
            }
            TOOLCHAIN._publish_cache_catalog_entry(output, entry)
            target_identity = copy.deepcopy(base_identity)
            target_identity["commit"] = target_commit
            target_identity["tree"] = target_tree

            selected = self.select_fixture_cache(
                output, target_commit, target_identity, git_dir
            )
            self.assertIsNotNone(selected)
            self.assertEqual(selected["diff"]["changed_paths"], ["src/a.py"])

            partition_changed = copy.deepcopy(target_identity)
            partition_changed["partitions"]["python"]["signature"] = "f" * 64
            selected = self.select_fixture_cache(
                output, target_commit, partition_changed, git_dir
            )
            self.assertEqual(selected["invalidated_partitions"], ["python"])

            mismatch_cases = []
            producer_changed = copy.deepcopy(target_identity)
            producer_changed["producer"]["graph_schema_signature"] = "9" * 64
            mismatch_cases.append(("producer/schema", producer_changed, {}))
            mismatch_cases.append(("toolchain", target_identity, {"toolchain": "7" * 64}))
            mismatch_cases.append(("inventory", target_identity, {"inventory": "6" * 64}))
            for label, mismatched_identity, overrides in mismatch_cases:
                with self.subTest(label=label):
                    diagnostics = {}
                    self.assertIsNone(
                        self.select_fixture_cache(
                            output,
                            target_commit,
                            mismatched_identity,
                            git_dir,
                            diagnostics=diagnostics,
                            **overrides,
                        )
                    )
                    self.assertTrue(diagnostics["cold_rebuild_reasons"])
                    if label == "toolchain":
                        self.assertIn(
                            "toolchain_lock_digest_mismatch",
                            {
                                reason["code"]
                                for reason in diagnostics[
                                    "cold_rebuild_reasons"
                                ]
                            },
                        )

            shared_config_changed = copy.deepcopy(target_identity)
            shared_config_changed["shared_influence_digest"] = "8" * 64
            selected = self.select_fixture_cache(
                output, target_commit, shared_config_changed, git_dir
            )
            self.assertIsNotNone(selected)
            self.assertEqual(
                selected["invalidated_partitions"],
                sorted(target_identity["partitions"]),
            )

            catalog_path = output / TOOLCHAIN.STRUCTURAL_CACHE_CATALOG
            catalog = json.loads(catalog_path.read_text())
            catalog["entries"][0]["schema_version"] += 1
            TOOLCHAIN.write_canonical_json(catalog_path, catalog)
            self.assertIsNone(
                self.select_fixture_cache(
                    output, target_commit, target_identity, git_dir
                )
            )

    def test_identical_target_injection_authorizes_work_and_validation_lineage(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            self.git(source, "init", "--quiet")
            self.git(source, "config", "user.name", "Fixture")
            self.git(source, "config", "user.email", "fixture@example.invalid")
            (source / "src").mkdir()
            (source / "src/a.py").write_text("VALUE = 1\n")
            self.git(source, "add", "--all")
            self.git(source, "commit", "--quiet", "-m", "base")
            commit = self.git(source, "rev-parse", "HEAD")
            tree = self.git(source, "rev-parse", "HEAD^{tree}")
            git_dir = root / "owner__repo.git"
            subprocess.run(
                ["git", "clone", "--quiet", "--bare", str(source), str(git_dir)],
                check=True,
            )
            checkout, identity = self.structural_cache_fixture(root)
            cache = checkout / ".oh/.cache"
            report = TOOLCHAIN.load_json_object(
                cache / "lsp_completeness.json", "fixture completeness report"
            )
            report["files"][0]["expected_result_ids"].append("result-2")
            report["files"][0]["persisted_results"]["provenance"].append(
                "result-2"
            )
            TOOLCHAIN.write_canonical_json(cache / "lsp_completeness.json", report)
            TOOLCHAIN.write_canonical_json(
                cache / "enrichment_jobs.json",
                {
                    "events": [],
                    "jobs": [
                        {
                            "job_id": "validation-job",
                            "capability": "call_references",
                            "state": "completed",
                            "lsp_evidence": {
                                "validations": [
                                    {
                                        "status": "processed",
                                        "document_symbols": [
                                            {
                                                "file": "src/a.py",
                                                "graph_result_id": "result-2",
                                            }
                                        ],
                                    }
                                ]
                            },
                        }
                    ],
                },
            )
            identity["commit"] = commit
            identity["tree"] = tree
            archive = root / "base.tar.gz"
            sidecar = root / "base.manifest.json"
            archived = self.archive_fixture(checkout, identity, archive, sidecar)
            materialized = root / "materialized"
            verified = TOOLCHAIN.verify_structural_cache_archive(
                archive, sidecar, materialize_cache=materialized
            )
            selection = {
                "entry": {
                    "instance_id": "owner__repo-1",
                    "attempt_index": 1,
                    "archive_path": str(archive),
                    "sidecar_path": str(sidecar),
                },
                "verified": verified,
                "diff": TOOLCHAIN._git_diff_paths(git_dir, commit, commit),
                "invalidated_partitions": [],
                "compatible_partitions": ["python"],
                "invalidated_partition_reasons": {},
            }
            target = root / "target"
            target.mkdir()
            receipt = TOOLCHAIN.inject_structural_cache(
                selection,
                target,
                identity,
                git_dir,
                toolchain_lock_digest="a" * 64,
                inventory_digest="b" * 64,
                inventory_file_sha256="c" * 64,
                verified=verified,
                materialized_cache=materialized,
            )
            authorization_path = Path(receipt["authorization_path"])
            authorization = json.loads(authorization_path.read_text())
            digest = authorization.pop("digest")
            authorization["digest"] = ""
            self.assertEqual(digest, TOOLCHAIN.sha256_bytes(TOOLCHAIN.canonical_json(authorization)))
            self.assertEqual(receipt["inherited_file_count"], 1)
            self.assertEqual(receipt["predicted_executed_file_count"], 0)
            self.assertEqual(
                receipt["predicted_total_graph_enrichment_operation_count"], 1
            )
            self.assertEqual(receipt["changed_file_count"], 0)
            self.assertEqual(receipt["authorization_sha256"], TOOLCHAIN.sha256_file(authorization_path))
            self.assertEqual(
                authorization["executed_operation_budget"],
                {
                    "max_operations": 12_288,
                    "executed_estimate": 0,
                    "authorized_operations_by_language": {
                        "python": ["document_symbols"]
                    },
                    "basis": TOOLCHAIN.STRUCTURAL_CACHE_OPERATION_BUDGET_BASIS,
                    "estimated_file_count": 0,
                },
            )
            inherited = authorization["inherited_files"][0]
            self.assertEqual(inherited["producer_work_ids"], ["job:1"])
            self.assertEqual(
                {
                    lineage["result_id"]: lineage["producer_ids"]
                    for lineage in inherited["result_producers"]
                },
                {
                    "result-1": ["job:1"],
                    "result-2": ["enrichment-job:validation-job"],
                },
            )
            self.assertEqual(archived["archive_sha256"], TOOLCHAIN.sha256_file(archive))
            preflight = TOOLCHAIN.build_structural_cache_preflight(
                case_index=2,
                instance_id="owner__repo-2",
                inventory_case={"included_file_count": 1},
                target_identity=identity,
                selection=selection,
                injection_receipt=receipt,
            )
            self.assertEqual(
                preflight["predicted_file_counts"],
                {"target": 1, "inherited": 1, "executed": 0},
            )
            self.assertEqual(
                preflight["expected_operation_count"],
                {
                    "inherited_exact": 1,
                    "executed_estimate": 0,
                    "total_estimate": 1,
                    "max_executed": 12_288,
                    "authorized_operations_by_language": {
                        "python": ["document_symbols"]
                    },
                    "basis": TOOLCHAIN.STRUCTURAL_CACHE_OPERATION_BUDGET_BASIS,
                    "estimated_file_count": 0,
                },
            )
            over_limit = copy.deepcopy(preflight)
            over_limit["expected_operation_count"]["executed_estimate"] = 12_289
            over_limit["expected_operation_count"]["total_estimate"] = 12_290
            over_limit["digest"] = ""
            over_limit["digest"] = TOOLCHAIN.sha256_bytes(
                TOOLCHAIN.canonical_json(over_limit)
            )
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "operation estimate is inconsistent"
            ):
                TOOLCHAIN.validate_structural_cache_preflight(
                    over_limit, identity
                )

            original_authorization = authorization_path.read_bytes()
            legacy_authorization = json.loads(original_authorization)
            legacy_authorization["schema_version"] = 1
            legacy_authorization["digest"] = ""
            legacy_authorization["digest"] = TOOLCHAIN.sha256_bytes(
                TOOLCHAIN.canonical_json(legacy_authorization)
            )
            TOOLCHAIN.write_canonical_json(authorization_path, legacy_authorization)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "authorization schema/status mismatch"
            ):
                TOOLCHAIN.build_structural_cache_preflight(
                    case_index=2,
                    instance_id="owner__repo-2",
                    inventory_case={"included_file_count": 1},
                    target_identity=identity,
                    selection=selection,
                    injection_receipt=receipt,
                )
            authorization_path.write_bytes(original_authorization)

    def test_lineage_invalidated_partition_is_a_fixed_point_seed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            self.git(source, "init", "--quiet")
            self.git(source, "config", "user.name", "Fixture")
            self.git(source, "config", "user.email", "fixture@example.invalid")
            (source / "src").mkdir()
            (source / "src/a.py").write_text("VALUE = 1\n")
            (source / "src/b.rs").write_text("fn b() {}\n")
            self.git(source, "add", "--all")
            self.git(source, "commit", "--quiet", "-m", "base")
            commit = self.git(source, "rev-parse", "HEAD")
            tree = self.git(source, "rev-parse", "HEAD^{tree}")
            git_dir = root / "owner__repo.git"
            subprocess.run(
                ["git", "clone", "--quiet", "--bare", str(source), str(git_dir)],
                check=True,
            )

            checkout, identity = self.structural_cache_fixture(root)
            cache = checkout / ".oh/.cache"
            report = TOOLCHAIN.load_json_object(
                cache / "lsp_completeness.json", "fixture completeness report"
            )
            crossing = (
                "checkout:src/a.py:a:function->calls->"
                "checkout:src/b.rs:b:function"
            )
            python_file = report["files"][0]
            python_file["expected_result_ids"] = [crossing]
            python_file["persisted_results"]["provenance"] = [crossing]
            rust_file = copy.deepcopy(python_file)
            rust_file["path"] = "src/b.rs"
            rust_file["language"] = "rust"
            rust_file["expected_server"] = {
                "command": "rust-analyzer",
                "version": "1",
                "digest": "y",
            }
            report["files"].append(rust_file)
            report["readiness_validation_requests_by_language"]["rust"] = 1
            TOOLCHAIN.write_canonical_json(cache / "lsp_completeness.json", report)

            work_path = cache / "lsp_pass1_work_items.json"
            work = TOOLCHAIN.load_json_object(work_path, "fixture work ledger")
            work["records"]["job:2"] = {
                "job_id": "job",
                "item_id": 2,
                "state": "completed",
                "file": "src/b.rs",
                "input_hash": "input-2",
                "requested_operations": ["call_hierarchy"],
                "produced_result_ids": [crossing],
            }
            TOOLCHAIN.write_canonical_json(work_path, work)

            rust_partition = copy.deepcopy(identity["partitions"]["python"])
            rust_partition["language"] = "rust"
            rust_partition["descriptor_signature"] = "a" * 64
            rust_partition["influence_patterns"] = ["Cargo.toml"]
            rust_partition["influence_digest"] = "b" * 64
            rust_partition["signature"] = "c" * 64
            identity["partitions"]["rust"] = rust_partition
            identity["commit"] = commit
            identity["tree"] = tree

            archive = root / "base.tar.gz"
            sidecar = root / "base.manifest.json"
            self.archive_fixture(checkout, identity, archive, sidecar)
            materialized = root / "materialized"
            verified = TOOLCHAIN.verify_structural_cache_archive(
                archive, sidecar, materialize_cache=materialized
            )
            selection = {
                "entry": {
                    "instance_id": "owner__repo-1",
                    "attempt_index": 1,
                    "archive_path": str(archive),
                    "sidecar_path": str(sidecar),
                },
                "verified": verified,
                "diff": TOOLCHAIN._git_diff_paths(git_dir, commit, commit),
                "invalidated_partitions": [],
                "compatible_partitions": ["python", "rust"],
                "invalidated_partition_reasons": {},
            }
            target = root / "target"
            target.mkdir()

            receipt = TOOLCHAIN.inject_structural_cache(
                selection,
                target,
                identity,
                git_dir,
                toolchain_lock_digest="a" * 64,
                inventory_digest="b" * 64,
                inventory_file_sha256="c" * 64,
                verified=verified,
                materialized_cache=materialized,
            )
            authorization = TOOLCHAIN.load_json_object(
                Path(receipt["authorization_path"]), "target authorization"
            )

            self.assertEqual(receipt["invalidated_partitions"], ["python"])
            self.assertEqual(
                receipt["impact_closure_paths"], ["src/a.py", "src/b.rs"]
            )
            self.assertEqual(
                authorization["invalidated_paths"], ["src/a.py", "src/b.rs"]
            )
            self.assertEqual(receipt["inherited_file_count"], 0)
            self.assertEqual(receipt["predicted_executed_file_count"], 2)

    def test_lsp_impact_paths_reach_bidirectional_fixed_point(self) -> None:
        a_to_b = "checkout:src/a.py:a:function->calls->checkout:src/b.py:b:function"
        b_to_c = "checkout:src/b.py:b:function->calls->checkout:src/c.py:c:function"
        files = [
            {
                "path": "src/a.py",
                "expected_result_ids": [a_to_b],
                "persisted_results": {"provenance": [a_to_b]},
            },
            {
                "path": "src/b.py",
                "expected_result_ids": [b_to_c],
                "persisted_results": {"provenance": [a_to_b, b_to_c]},
            },
            {
                "path": "src/c.py",
                "expected_result_ids": [b_to_c],
                "persisted_results": {"provenance": [b_to_c]},
            },
            {
                "path": "src/d.py",
                "expected_result_ids": [
                    "external:builtins.py:len:function->calls->checkout:src/d.py:d:function"
                ],
                "persisted_results": {
                    "provenance": [
                        "external:builtins.py:len:function->calls->checkout:src/d.py:d:function"
                    ]
                },
            },
        ]

        self.assertEqual(
            TOOLCHAIN._fixed_point_lsp_impact_paths(
                files,
                root_slug="checkout",
                direct_paths={"src/a.py"},
                target_paths={"src/a.py", "src/b.py", "src/c.py", "src/d.py"},
            ),
            ["src/b.py", "src/c.py"],
        )
        self.assertEqual(
            TOOLCHAIN._fixed_point_lsp_impact_paths(
                files,
                root_slug="checkout",
                direct_paths={"src/c.py"},
                target_paths={"src/a.py", "src/b.py", "src/c.py", "src/d.py"},
            ),
            ["src/a.py", "src/b.py"],
        )

    def test_lsp_impact_paths_cannot_cross_target_or_root_boundary(self) -> None:
        a_to_b = "checkout:src/a.py:a:function->calls->checkout:src/b.py:b:function"
        b_to_c = "checkout:src/b.py:b:function->calls->checkout:src/c.py:c:function"
        b_to_external = (
            "checkout:src/b.py:b:function->calls->external:src/d.py:d:function"
        )
        other_to_target = (
            "other-root:src/b.py:b:function->calls->checkout:src/d.py:d:function"
        )
        files = [
            {
                "path": "src/a.py",
                "persisted_results": {"provenance": [a_to_b]},
            },
            {
                "path": "src/b.py",
                "persisted_results": {
                    "provenance": [a_to_b, b_to_c, b_to_external, other_to_target]
                },
            },
            {
                "path": "src/c.py",
                "persisted_results": {"provenance": [b_to_c]},
            },
            {
                "path": "src/d.py",
                "persisted_results": {"provenance": [other_to_target]},
            },
        ]

        self.assertEqual(
            TOOLCHAIN._fixed_point_lsp_impact_paths(
                files,
                root_slug="checkout",
                direct_paths={"src/a.py"},
                target_paths={"src/a.py", "src/b.py", "src/d.py"},
            ),
            ["src/b.py"],
        )

    def test_bound_partition_expansion_recloses_cross_language_edges(self) -> None:
        edges = [
            "checkout:src/a.py:a:function->calls->checkout:src/b.py:b:function",
            "checkout:src/b.py:b:function->calls->checkout:src/c.py:c:function",
            "checkout:src/c.py:c:function->calls->checkout:src/d.py:d:function",
            "checkout:src/x.py:x:function->calls->checkout:src/y.rs:y:function",
        ]
        files = [
            {
                "path": path,
                "persisted_results": {
                    "provenance": [
                        edge
                        for edge in edges
                        if f"checkout:{path}:" in edge
                    ]
                },
            }
            for path in (
                "src/a.py",
                "src/b.py",
                "src/c.py",
                "src/d.py",
                "src/x.py",
                "src/y.rs",
                "src/z.rs",
            )
        ]
        path_partitions = {
            "src/a.py": "python",
            "src/b.py": "python",
            "src/c.py": "python",
            "src/d.py": "python",
            "src/x.py": "python",
            "src/y.rs": "rust",
            "src/z.rs": "rust",
        }

        self.assertEqual(
            TOOLCHAIN._bounded_fixed_point_lsp_impact_paths(
                files,
                root_slug="checkout",
                direct_paths={"src/a.py"},
                path_partitions=path_partitions,
                base_languages=path_partitions,
                invalidated_partitions=set(),
            ),
            [
                "src/b.py",
                "src/c.py",
                "src/d.py",
                "src/x.py",
                "src/y.rs",
                "src/z.rs",
            ],
        )

    def test_cache_preflight_rejects_unexpected_all_partition_invalidation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            _, identity = self.structural_cache_fixture(Path(temporary))
            identity["partitions"] = {
                f"language-{index:02d}": {
                    "language": f"language-{index:02d}",
                    "descriptor_signature": "7" * 64,
                    "influence_patterns": [],
                    "influence_digest": "8" * 64,
                    "signature": TOOLCHAIN.sha256_bytes(
                        f"base-{index}".encode()
                    ),
                    "matched_file_count": 0,
                }
                for index in range(58)
            }
            core = {
                "configuration_digest": identity["configuration_digest"],
                "shared_influence_digest": identity["shared_influence_digest"],
                "partition_signatures": {
                    language: partition["signature"]
                    for language, partition in identity["partitions"].items()
                },
            }
            target = copy.deepcopy(identity)
            for index, partition in enumerate(target["partitions"].values()):
                partition["signature"] = TOOLCHAIN.sha256_bytes(
                    f"target-{index}".encode()
                )
            invalidated, compatible, reasons = TOOLCHAIN._partition_invalidation_plan(
                core, target
            )
            selection = {
                "invalidated_partitions": invalidated,
                "compatible_partitions": compatible,
                "invalidated_partition_reasons": reasons,
            }
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError,
                "unexpected all-partition invalidation.*partition_count=58",
            ), mock.patch.object(
                TOOLCHAIN, "verify_structural_cache_archive"
            ) as archive_verifier:
                TOOLCHAIN._validate_selected_partition_plan(selection, target)
            archive_verifier.assert_not_called()

    def test_ci_yaml_and_test_json_changes_do_not_invalidate_all_partitions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            _, identity = self.structural_cache_fixture(Path(temporary))
            identity["partitions"] = {
                f"language-{index:02d}": {
                    "language": f"language-{index:02d}",
                    "descriptor_signature": "7" * 64,
                    "influence_patterns": [],
                    "influence_digest": "8" * 64,
                    "signature": TOOLCHAIN.sha256_bytes(
                        f"stable-{index}".encode()
                    ),
                    "matched_file_count": 0,
                }
                for index in range(58)
            }
            core = {
                "configuration_digest": identity["configuration_digest"],
                "shared_influence_digest": identity["shared_influence_digest"],
                "partition_signatures": {
                    language: partition["signature"]
                    for language, partition in identity["partitions"].items()
                },
            }
            invalidated, compatible, reasons = TOOLCHAIN._partition_invalidation_plan(
                core, identity
            )
            selection = {
                "diff": {
                    "changed_paths": [
                        ".github/workflows/ci.yml",
                        "tests/fixtures/snapshot.json",
                    ]
                },
                "invalidated_partitions": invalidated,
                "compatible_partitions": compatible,
                "invalidated_partition_reasons": reasons,
            }
            TOOLCHAIN._validate_selected_partition_plan(selection, identity)
            self.assertEqual(invalidated, [])
            self.assertEqual(compatible, sorted(identity["partitions"]))
            self.assertEqual(reasons, {})

            configuration_changed = copy.deepcopy(identity)
            configuration_changed["configuration_digest"] = "changed-configuration"
            invalidated, compatible, reasons = TOOLCHAIN._partition_invalidation_plan(
                core, configuration_changed
            )
            TOOLCHAIN._validate_selected_partition_plan(
                {
                    "invalidated_partitions": invalidated,
                    "compatible_partitions": compatible,
                    "invalidated_partition_reasons": reasons,
                },
                configuration_changed,
            )
            self.assertEqual(invalidated, sorted(identity["partitions"]))
            self.assertEqual(compatible, [])
            self.assertEqual(
                {
                    reason["code"]
                    for language_reasons in reasons.values()
                    for reason in language_reasons
                },
                {"configuration_digest_mismatch"},
            )

    def test_cold_preflight_marks_operation_count_unknown_and_closes_toctou(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _, identity = self.structural_cache_fixture(root)
            second = copy.deepcopy(identity["partitions"]["python"])
            second["language"] = "yaml"
            second["signature"] = "a" * 64
            identity["partitions"]["yaml"] = second
            preflight = TOOLCHAIN.build_structural_cache_preflight(
                case_index=1,
                instance_id="owner__repo-1",
                inventory_case={"included_file_count": 2},
                target_identity=identity,
                selection=None,
                injection_receipt=None,
            )
            self.assertIsNone(
                preflight["expected_operation_count"]["executed_estimate"]
            )
            self.assertIsNone(
                preflight["expected_operation_count"]["total_estimate"]
            )
            approved = root / "approved.json"
            with mock.patch("builtins.print"):
                TOOLCHAIN.publish_structural_cache_preflight(preflight, approved)
            TOOLCHAIN.require_approved_structural_cache_preflight(
                preflight, approved
            )
            drifted = copy.deepcopy(preflight)
            drifted["target_tree"] = "f" * 40
            drifted["digest"] = ""
            drifted["digest"] = TOOLCHAIN.sha256_bytes(
                TOOLCHAIN.canonical_json(drifted)
            )
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "differs from recomputed plan"
            ):
                TOOLCHAIN.require_approved_structural_cache_preflight(
                    drifted, approved
                )

    def test_isolated_sphinx_pair_preserves_qualification_indexes(self) -> None:
        population_document = TOOLCHAIN.load_json_object(
            ROOT / "benchmark/swebench-act-context/population.json",
            "test population",
        )
        population = TOOLCHAIN.included_population(population_document)
        selected = TOOLCHAIN._select_qualification_instances(
            population,
            ["sphinx-doc__sphinx-8548", "sphinx-doc__sphinx-8551"],
            True,
        )
        self.assertEqual(
            [(index, case["instance_id"]) for index, case in selected],
            [
                (59, "sphinx-doc__sphinx-8548"),
                (60, "sphinx-doc__sphinx-8551"),
            ],
        )
        raw_indexes = {
            case["instance_id"]: index
            for index, case in enumerate(population_document["instances"], start=1)
        }
        self.assertEqual(
            [
                raw_indexes["sphinx-doc__sphinx-8548"],
                raw_indexes["sphinx-doc__sphinx-8551"],
            ],
            [60, 61],
        )
        with self.assertRaises(TOOLCHAIN.ToolchainError):
            TOOLCHAIN._select_qualification_instances(
                population,
                ["sphinx-doc__sphinx-8548", "sphinx-doc__sphinx-8551"],
                False,
            )
        with self.assertRaises(TOOLCHAIN.ToolchainError):
            TOOLCHAIN._select_qualification_instances(
                population,
                ["sphinx-doc__sphinx-8551", "sphinx-doc__sphinx-8548"],
                True,
            )
        with self.assertRaisesRegex(
            TOOLCHAIN.ToolchainError, "must share one repository"
        ):
            TOOLCHAIN._select_qualification_instances(
                [
                    {"instance_id": "owner__one-1", "repo": "owner/one"},
                    {"instance_id": "owner__two-1", "repo": "owner/two"},
                ],
                ["owner__one-1", "owner__two-1"],
                True,
            )

    def test_checkpoint_is_partial_and_never_writes_ready_aggregate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            checkpoint = TOOLCHAIN._qualification_checkpoint(
                output_root=output,
                last_case_index=2,
                cohort_cases=[{"instance_id": "owner__repo-2"}],
                timing_cases=[{"instance_id": "owner__repo-2", "scan_ms": 1}],
                isolated=False,
            )
            self.assertEqual(checkpoint["status"], "checkpoint")
            self.assertTrue(Path(checkpoint["path"]).is_file())
            self.assertFalse((output / "cohort-manifest.json").exists())
            self.assertFalse((output / "aggregate.json").exists())
            self.assertFalse((output / "seal.json").exists())
            repeated = TOOLCHAIN._qualification_checkpoint(
                output_root=output,
                last_case_index=2,
                cohort_cases=[{"instance_id": "owner__repo-2"}],
                timing_cases=[{"instance_id": "owner__repo-2", "scan_ms": 1}],
                isolated=False,
            )
            self.assertEqual(repeated["digest"], checkpoint["digest"])

    def test_frozen_cohort_manifest_uses_report_schema_and_rejects_mixed_versions(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cases = []
            for index, commit in enumerate(("a" * 40, "b" * 40), start=1):
                report_path = root / f"report-{index}.json"
                TOOLCHAIN.write_canonical_json(
                    report_path,
                    {"identity": {"schema_version": 6}},
                )
                cases.append(
                    {
                        "instance_id": f"owner__repo-{index}",
                        "repository": "owner/repo",
                        "base_commit": commit,
                        "report_path": str(report_path),
                    }
                )

            manifest = TOOLCHAIN._build_frozen_cohort_manifest(cases)
            self.assertEqual(manifest, {"schema_version": 6, "cases": cases})
            self.assertNotEqual(manifest["schema_version"], TOOLCHAIN.SCHEMA_VERSION)

            TOOLCHAIN.write_canonical_json(
                Path(cases[1]["report_path"]),
                {"identity": {"schema_version": 7}},
            )
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "do not share one completeness schema"
            ):
                TOOLCHAIN._build_frozen_cohort_manifest(cases)

    def test_ready_aggregate_uses_counts_and_checkouts_without_status(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cases = []
            report_digests = ("d" * 64, "e" * 64)
            for index, (commit, report_digest) in enumerate(
                zip(("a" * 40, "b" * 40), report_digests), start=1
            ):
                report_path = root / f"report-{index}.json"
                TOOLCHAIN.write_canonical_json(
                    report_path,
                    {
                        "identity": {
                            "schema_version": 6,
                            "context_mode": "disabled",
                            "repository": "owner/repo",
                            "checkout_sha": commit,
                        },
                        "summary": {"total_files": 1},
                        "violations": [],
                        "digest": report_digest,
                    },
                )
                cases.append(
                    {
                        "instance_id": f"owner__repo-{index}",
                        "repository": "owner/repo",
                        "base_commit": commit,
                        "report_path": str(report_path),
                    }
                )
            manifest = TOOLCHAIN._build_frozen_cohort_manifest(cases)
            population_digest = "c" * 64
            aggregate = {
                "schema_version": 6,
                "cohort_digest": population_digest,
                "checkouts": [
                {
                    "instance_id": case["instance_id"],
                    "repository": case["repository"],
                    "base_commit": case["base_commit"],
                    "checkout_sha": case["base_commit"],
                    "report_digest": report_digest,
                    "ready": True,
                    "file_count": 1,
                    "violation_count": 0,
                }
                    for case, report_digest in zip(cases, report_digests)
                ],
                "counts": {
                    "checkouts": 2,
                    "unique_instances": 2,
                    "ready_checkouts": 2,
                    "files": 2,
                    "by_extension": {"py": 2},
                    "by_role": {"source": 2},
                    "by_status": {"complete": 2},
                },
                "digest": "f" * 64,
            }
            self.assertNotIn("status", aggregate)
            TOOLCHAIN._validate_ready_aggregate(
                aggregate,
                manifest,
                recomputed_aggregate=copy.deepcopy(aggregate),
                expected_population_digest=population_digest,
            )

            tampered_digest = copy.deepcopy(aggregate)
            tampered_digest["digest"] = "0" * 64
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "independently recomputed producer output"
            ):
                TOOLCHAIN._validate_ready_aggregate(
                    tampered_digest,
                    manifest,
                    recomputed_aggregate=aggregate,
                    expected_population_digest=population_digest,
                )

            blocked_counts = copy.deepcopy(aggregate)
            blocked_counts["counts"]["ready_checkouts"] = 1
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "counts are not fully READY"
            ):
                TOOLCHAIN._validate_ready_aggregate(
                    blocked_counts,
                    manifest,
                    recomputed_aggregate=copy.deepcopy(blocked_counts),
                    expected_population_digest=population_digest,
                )

            blocked_checkout = copy.deepcopy(aggregate)
            blocked_checkout["checkouts"][0]["ready"] = False
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "checkout is not verifier-clean READY"
            ):
                TOOLCHAIN._validate_ready_aggregate(
                    blocked_checkout,
                    manifest,
                    recomputed_aggregate=copy.deepcopy(blocked_checkout),
                    expected_population_digest=population_digest,
                )

            fabricated_checkout = copy.deepcopy(aggregate)
            fabricated_checkout["checkouts"][0]["report_digest"] = "0" * 64
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "checkout is not verifier-clean READY"
            ):
                TOOLCHAIN._validate_ready_aggregate(
                    fabricated_checkout,
                    manifest,
                    recomputed_aggregate=copy.deepcopy(fabricated_checkout),
                    expected_population_digest=population_digest,
                )

            fabricated_cohort = copy.deepcopy(aggregate)
            fabricated_cohort["cohort_digest"] = "0" * 64
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "differs from frozen population digest"
            ):
                TOOLCHAIN._validate_ready_aggregate(
                    fabricated_cohort,
                    manifest,
                    recomputed_aggregate=copy.deepcopy(fabricated_cohort),
                    expected_population_digest=population_digest,
                )

    def test_resume_receipt_rejects_population_identity_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            output.mkdir()
            _, identity = self.structural_cache_fixture(root)
            rna_binary = root / "rna"
            rna_binary.write_bytes(b"exact CI artifact")
            identity["producer"]["binary_sha256"] = TOOLCHAIN.sha256_file(
                rna_binary
            )
            instance = {
                "instance_id": "owner__repo-1",
                "repo": "owner/repo",
                "base_commit": identity["commit"],
            }
            inventory_case = {
                "tree": identity["tree"],
                "per_file_digest": "d" * 64,
                "included_file_count": 1,
            }
            preflight = TOOLCHAIN.build_structural_cache_preflight(
                case_index=1,
                instance_id=instance["instance_id"],
                inventory_case=inventory_case,
                target_identity=identity,
                selection=None,
                injection_receipt=None,
            )
            preflight_path = output / "preflight.json"
            TOOLCHAIN.write_canonical_json(preflight_path, preflight)
            report = {
                "violations": [],
                "digest": "report-digest",
                "graph_snapshot_digest": "graph-digest",
            }
            report_path = output / "report.json"
            TOOLCHAIN.write_canonical_json(report_path, report)
            archive_path = output / "cache.tar.gz"
            sidecar_path = output / "cache.manifest.json"
            archive_path.write_bytes(b"fixture archive")
            sidecar_path.write_bytes(b"fixture sidecar")
            cache_identity = {
                "archive_path": str(archive_path),
                "archive_sha256": "a" * 64,
                "sidecar_path": str(sidecar_path),
                "sidecar_sha256": "b" * 64,
                "core_sha256": "c" * 64,
            }
            receipt = {
                "schema_version": TOOLCHAIN.SCHEMA_VERSION,
                "status": "ready",
                "offline_preprocessing": True,
                "population_index": 1,
                "instance_id": instance["instance_id"],
                "repository": instance["repo"],
                "base_commit": instance["base_commit"],
                "tree": inventory_case["tree"],
                "producer": identity["producer"],
                "toolchain_lock_digest": "a" * 64,
                "inventory_digest": "b" * 64,
                "inventory_file_sha256": "c" * 64,
                "case_inventory_digest": inventory_case["per_file_digest"],
                "configuration_digest": identity["configuration_digest"],
                "scan_flags": TOOLCHAIN.QUALIFICATION_SCAN_FLAGS,
                "preflight_path": str(preflight_path),
                "preflight_sha256": TOOLCHAIN.sha256_file(preflight_path),
                "preflight_digest": preflight["digest"],
                "report_path": str(report_path),
                "report_sha256": TOOLCHAIN.sha256_file(report_path),
                "report_digest": report["digest"],
                "graph_snapshot_digest": report["graph_snapshot_digest"],
                "cache": cache_identity,
                "timings_ms": {
                    "cache_selection": 1,
                    "cache_verification": 2,
                    "cache_injection": 3,
                    "scan_update": 4,
                    "full_readiness_validation": 5,
                    "cache_archive": 6,
                },
            }
            receipt["receipt_digest"] = TOOLCHAIN.sha256_bytes(
                TOOLCHAIN.canonical_json(receipt)
            )
            receipt_path = output / "receipt.json"
            TOOLCHAIN.write_canonical_json(receipt_path, receipt)
            TOOLCHAIN._publish_cache_catalog_entry(
                output,
                {
                    "schema_version": TOOLCHAIN.STRUCTURAL_CACHE_SCHEMA_VERSION,
                    "status": "ready",
                    "case_index": 1,
                    "population_index": 1,
                    "attempt_index": 1,
                    "instance_id": instance["instance_id"],
                    "repository": instance["repo"],
                    "commit": instance["base_commit"],
                    "tree": inventory_case["tree"],
                    "archive_path": str(archive_path),
                    "archive_sha256": cache_identity["archive_sha256"],
                    "sidecar_path": str(sidecar_path),
                    "sidecar_sha256": cache_identity["sidecar_sha256"],
                    "core_sha256": cache_identity["core_sha256"],
                    "report_digest": report["digest"],
                    "receipt_path": str(receipt_path),
                    "receipt_sha256": TOOLCHAIN.sha256_file(receipt_path),
                },
            )
            verified = {
                "archive_sha256": cache_identity["archive_sha256"],
                "sidecar_sha256": cache_identity["sidecar_sha256"],
                "core_sha256": cache_identity["core_sha256"],
                "core": {
                    "commit": instance["base_commit"],
                    "tree": inventory_case["tree"],
                    "case_inventory_digest": inventory_case["per_file_digest"],
                    "configuration_digest": identity["configuration_digest"],
                    "completeness_report_digest": report["digest"],
                    "graph_snapshot_digest": report["graph_snapshot_digest"],
                },
            }
            with mock.patch.object(
                TOOLCHAIN,
                "verify_structural_cache_archive",
                return_value=verified,
            ):
                self.assertIsNotNone(
                    TOOLCHAIN._resume_ready_case(
                        output_root=output,
                        case_index=1,
                        instance=instance,
                        inventory_case=inventory_case,
                        rna_binary=rna_binary,
                        toolchain_lock_digest="a" * 64,
                        inventory_digest="b" * 64,
                        inventory_file_sha256="c" * 64,
                    )
                )

            receipt["population_index"] = 2
            receipt["receipt_digest"] = ""
            receipt.pop("receipt_digest")
            receipt["receipt_digest"] = TOOLCHAIN.sha256_bytes(
                TOOLCHAIN.canonical_json(receipt)
            )
            TOOLCHAIN.write_canonical_json(receipt_path, receipt)
            catalog_path = output / TOOLCHAIN.STRUCTURAL_CACHE_CATALOG
            catalog = TOOLCHAIN.load_json_object(catalog_path, "test catalog")
            catalog["entries"][0]["receipt_sha256"] = TOOLCHAIN.sha256_file(
                receipt_path
            )
            TOOLCHAIN.write_canonical_json(catalog_path, catalog)
            with self.assertRaisesRegex(
                TOOLCHAIN.ToolchainError, "frozen identity mismatch"
            ):
                TOOLCHAIN._resume_ready_case(
                    output_root=output,
                    case_index=1,
                    instance=instance,
                    inventory_case=inventory_case,
                    rna_binary=rna_binary,
                    toolchain_lock_digest="a" * 64,
                    inventory_digest="b" * 64,
                    inventory_file_sha256="c" * 64,
                )

    @staticmethod
    def structural_cache_fixture(root: Path) -> tuple[Path, dict[str, object]]:
        checkout = root / "cache-checkout"
        cache = checkout / ".oh" / ".cache"
        (cache / "lance").mkdir(parents=True)
        file_record = {
            "path": "src/a.py",
            "role": "source",
            "language": "python",
            "expected_server": {"command": "pyrefly", "version": "1", "digest": "x"},
            "advertised_capabilities": [
                {"name": "textDocument/documentSymbol", "supported": True}
            ],
            "requests_attempted": [
                {
                    "method": "textDocument/documentSymbol",
                    "outcome": "completed",
                    "result_count": 1,
                    "duration_ms": 1,
                    "detail": None,
                }
            ],
            "expected_results": ["document_symbol"],
            "expected_result_ids": ["result-1"],
            "persisted_results": {"provenance": ["result-1"]},
            "terminal_status": {"status": "processed", "result_count": 1},
            "exclusion": None,
        }
        report = {
            "identity": {"checkout_sha": "1" * 40},
            "files": [file_record],
            "readiness_validation_requests_by_language": {"python": 1},
            "violations": [],
            "digest": "report-digest",
            "graph_snapshot_digest": "graph-digest",
        }
        TOOLCHAIN.write_canonical_json(cache / "lsp_completeness.json", report)
        TOOLCHAIN.write_canonical_json(
            cache / "lsp_pass1_work_items.json",
            {
                "records": {
                    "job:1": {
                        "job_id": "job",
                        "item_id": 1,
                        "state": "completed",
                        "file": "src/a.py",
                        "input_hash": "input-1",
                        "requested_operations": ["document_symbols"],
                        "produced_result_ids": ["result-1"],
                    }
                }
            },
        )
        (cache / "lance" / "symbols.bin").write_bytes(b"structural graph")
        identity = {
            "schema_version": 1,
            "repository": "owner/repo",
            "commit": "1" * 40,
            "tree": "2" * 40,
            "root_slug": "checkout",
            "configuration_digest": "configuration",
            "inventory_policy_digest": "policy",
            "context_mode": "disabled",
            "producer": {
                "producer_commit": "3" * 40,
                "package_version": "1.0.0",
                "binary_sha256": "4" * 64,
                "graph_schema_version": 24,
                "graph_schema_signature": "5" * 64,
                "completeness_schema_version": 6,
                "work_item_schema_version": 4,
                "validation_evidence_schema_version": 1,
            },
            "shared_influence_digest": "6" * 64,
            "partitions": {
                "python": {
                    "language": "python",
                    "descriptor_signature": "7" * 64,
                    "influence_patterns": ["pyproject.toml"],
                    "influence_digest": "8" * 64,
                    "signature": "9" * 64,
                    "matched_file_count": 0,
                }
            },
        }
        return checkout, identity

    @staticmethod
    def archive_fixture(
        checkout: Path,
        identity: dict[str, object],
        archive: Path,
        sidecar: Path,
    ) -> dict[str, object]:
        return TOOLCHAIN.archive_structural_cache(
            checkout,
            archive,
            sidecar,
            identity=identity,
            toolchain_lock_digest="a" * 64,
            inventory_digest="b" * 64,
            inventory_file_sha256="c" * 64,
            case_inventory_digest="d" * 64,
            base_cache=None,
        )

    @staticmethod
    def cache_expected(identity: dict[str, object]) -> dict[str, object]:
        return {
            "repository": identity["repository"],
            "root_slug": identity["root_slug"],
            "producer": identity["producer"],
            "toolchain_lock_digest": "a" * 64,
            "inventory_digest": "b" * 64,
            "inventory_file_sha256": "c" * 64,
            "inventory_policy_digest": identity["inventory_policy_digest"],
            "scan_flags": TOOLCHAIN.QUALIFICATION_SCAN_FLAGS,
            "shared_influence_digest": identity["shared_influence_digest"],
        }

    @staticmethod
    def write_hostile_archive(
        archive_path: Path,
        sidecar_path: Path,
        core: dict[str, object],
        members: list[tuple[str, str, bytes]],
    ) -> None:
        with archive_path.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(
                    fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT
                ) as archive:
                    root_info = tarfile.TarInfo("cache")
                    root_info.type = tarfile.DIRTYPE
                    root_info.mode = 0o755
                    root_info.uid = root_info.gid = 0
                    root_info.mtime = 1_577_836_800
                    archive.addfile(root_info)
                    for kind, name, data in members:
                        info = tarfile.TarInfo(name)
                        info.mode = 0o644
                        info.uid = info.gid = 0
                        info.mtime = 1_577_836_800
                        if kind == "symlink":
                            info.type = tarfile.SYMTYPE
                            info.linkname = "../escape"
                            archive.addfile(info)
                        else:
                            info.size = len(data)
                            archive.addfile(info, io.BytesIO(data))
        sidecar = {
            "schema_version": TOOLCHAIN.STRUCTURAL_CACHE_SCHEMA_VERSION,
            "publication_status": "ready",
            "archive_name": archive_path.name,
            "archive_size_bytes": archive_path.stat().st_size,
            "archive_sha256": TOOLCHAIN.sha256_file(archive_path),
            "core_sha256": TOOLCHAIN.sha256_bytes(TOOLCHAIN.canonical_json(core)),
            "core": core,
        }
        TOOLCHAIN.write_canonical_json(sidecar_path, sidecar)

    @staticmethod
    def select_fixture_cache(
        output: Path,
        target_commit: str,
        target_identity: dict[str, object],
        git_dir: Path,
        *,
        toolchain: str = "a" * 64,
        inventory: str = "b" * 64,
        diagnostics: dict[str, object] | None = None,
    ) -> dict[str, object] | None:
        return TOOLCHAIN.select_structural_cache(
            output,
            "owner/repo",
            target_commit,
            target_identity,
            git_dir,
            2,
            toolchain_lock_digest=toolchain,
            inventory_digest=inventory,
            inventory_file_sha256="c" * 64,
            diagnostics=diagnostics,
        )

    @staticmethod
    def write_repo_acquisition_contract(
        root: Path,
        *,
        artifact: str,
        artifact_sha256: str,
        root_name: str,
        sources: list[dict[str, str]],
    ) -> dict[str, str]:
        contract = {
            "schema_version": 1,
            "artifacts": [
                {
                    "artifact": artifact,
                    "artifact_sha256": artifact_sha256,
                    "kind": "repo-sources",
                    "root_name": root_name,
                    "sources": sources,
                }
            ],
        }
        path = root / "acquisition-recipes.json"
        TOOLCHAIN.write_canonical_json(path, contract)
        return {
            "path": path.relative_to(root).as_posix(),
            "sha256": TOOLCHAIN.sha256_file(path),
        }

    @staticmethod
    def lock_entry(
        *, artifact_sha256: str, languages: list[str]
    ) -> dict[str, object]:
        return {
            "name": "fixture-server",
            "version": "1.0.0",
            "license": "MIT",
            "source_url": "https://example.invalid/server.tgz",
            "artifact": "server.tgz",
            "artifact_sha256": artifact_sha256,
            "executable": "bin/server",
            "executable_sha256": "b" * 64,
            "command": "server",
            "args": ["--stdio"],
            "languages": languages,
            "extensions": ["py"],
            "expected_capabilities": ["textDocument/documentSymbol"],
            "platform": {"os": "macos", "architecture": "arm64"},
            "runtime_dependencies": [],
            "install": {
                "kind": "copy",
                "destination": "servers/server",
                "member": "",
            },
            "launcher": {"kind": "direct", "target": "servers/server"},
            "probe": {
                "file_name": "probe.py",
                "language_id": "python",
                "operation": "textDocument/documentSymbol",
            },
        }

    @staticmethod
    def git(root: Path, *args: str) -> str:
        return subprocess.check_output(
            ["git", *args], cwd=root, text=True
        ).strip()


if __name__ == "__main__":
    unittest.main()
