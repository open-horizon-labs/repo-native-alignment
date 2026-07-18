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
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "swebench_lsp_toolchain.py"
SPEC = importlib.util.spec_from_file_location("swebench_lsp_toolchain", MODULE_PATH)
assert SPEC and SPEC.loader
TOOLCHAIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TOOLCHAIN
SPEC.loader.exec_module(TOOLCHAIN)


class SwebenchLspToolchainTests(unittest.TestCase):
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
                        "import subprocess,time; "
                        "subprocess.Popen([\"sleep\",\"30\"]); "
                        "print(\"started\", flush=True); time.sleep(30)",
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
            entry = self.lock_entry(
                artifact_sha256=TOOLCHAIN.sha256_file(artifact), languages=["python"]
            )
            lock = {
                "schema_version": 1,
                "platform": {"os": "macos", "architecture": "arm64"},
                "inventory_sha256": TOOLCHAIN.sha256_file(inventory_path),
                "inventory_digest": inventory["inventory_digest"],
                "repo_parser_bundle": {
                    "artifact": "parser.tgz",
                    "artifact_sha256": TOOLCHAIN.sha256_file(parser_artifact),
                    "sources": [{"path": "fixture.py", "sha256": "c" * 64}],
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

            result = TOOLCHAIN.verify_lock(lock_path, inventory_path, cache, None)
            self.assertFalse(result["compatible"])
            self.assertEqual(result["unsupported_languages"], ["restructuredtext"])

            artifact.write_bytes(b"drifted")
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "digest mismatch"):
                TOOLCHAIN.verify_lock(lock_path, inventory_path, cache, None)

            artifact.write_bytes(b"locked artifact")
            descriptors_path = root / "descriptors.json"
            descriptors_path.write_text('{"schema_version":1,"servers":[]}')
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "language mismatch"):
                TOOLCHAIN.verify_lock(
                    lock_path, inventory_path, cache, descriptors_path
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
                lock_path, inventory_path, cache, descriptors_path
            )
            self.assertTrue(result["descriptors_verified"])

            descriptor_inventory["servers"][0]["command"] = "wrong-server"
            descriptors_path.write_text(json.dumps(descriptor_inventory))
            with self.assertRaisesRegex(TOOLCHAIN.ToolchainError, "profile mismatch"):
                TOOLCHAIN.verify_lock(
                    lock_path, inventory_path, cache, descriptors_path
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
                    self.assertIsNone(
                        self.select_fixture_cache(
                            output,
                            target_commit,
                            mismatched_identity,
                            git_dir,
                            **overrides,
                        )
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

    def test_identical_target_injection_authorizes_all_work_with_bound_digest(self) -> None:
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
                    "archive_path": str(archive),
                    "sidecar_path": str(sidecar),
                },
                "verified": verified,
                "diff": TOOLCHAIN._git_diff_paths(git_dir, commit, commit),
                "invalidated_partitions": [],
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
            self.assertEqual(receipt["changed_file_count"], 0)
            self.assertEqual(receipt["authorization_sha256"], TOOLCHAIN.sha256_file(authorization_path))
            self.assertEqual(archived["archive_sha256"], TOOLCHAIN.sha256_file(archive))

    @staticmethod
    def structural_cache_fixture(root: Path) -> tuple[Path, dict[str, object]]:
        checkout = root / "cache-checkout"
        cache = checkout / ".oh" / ".cache"
        (cache / "lance").mkdir(parents=True)
        file_record = {
            "path": "src/a.py",
            "role": "source",
            "language": "python",
            "expected_server": {"command": "pyright", "version": "1", "digest": "x"},
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
                        "requested_operations": ["textDocument/documentSymbol"],
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
        )

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
