from __future__ import annotations

import copy
import gzip
import io
import json
import subprocess
import tarfile
import tempfile
import unittest
import unittest.mock as mock
from pathlib import Path

from scripts import swebench_combined_cache as COMBINED
from scripts import swebench_lsp_toolchain as STRUCTURAL


class SwebenchCombinedCacheTests(unittest.TestCase):
    def test_fresh_reopen_query_probe_set_is_fail_closed_and_archivable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = "src/lib.rs:fixture:function"

            def fake_profiled_query(
                args: list[str],
                _cwd: Path,
                _environment: dict[str, str],
                stdout_path: Path,
                stderr_path: Path,
                *,
                timeout_seconds: float = 300.0,
            ) -> dict[str, object]:
                self.assertEqual(timeout_seconds, 300.0)
                if "--mode" in args:
                    stdout = f"Graph neighbors of `{selected}`\n1 result(s)\n"
                elif "--include-body" in args:
                    stdout = f"`{selected}`\n```rust\nfn fixture() {{}}\n```\n"
                else:
                    stdout = (
                        f"{STRUCTURAL.COMBINED_STRICT_SEARCH_SENTINEL}\n"
                        f"`{selected}`\n"
                    )
                stdout_path.write_text(stdout)
                stderr_path.write_bytes(b"")
                return {
                    "duration_ms": 3,
                    "ttfe_ms": 1,
                    "peak_memory_bytes": 1024,
                    "stdout_file": stdout_path.name,
                    "stdout_sha256": STRUCTURAL.sha256_file(stdout_path),
                    "stderr_file": stderr_path.name,
                    "stderr_sha256": STRUCTURAL.sha256_file(stderr_path),
                }

            with mock.patch.object(
                STRUCTURAL,
                "_run_profiled_query",
                side_effect=fake_profiled_query,
            ):
                result = STRUCTURAL._run_combined_query_probes(
                    combined=COMBINED,
                    rna_binary=root / "rna",
                    checkout=root,
                    environment={},
                    evidence_root=root / "query-evidence",
                    case_identity={
                        "case_index": 1,
                        "attempt_index": 1,
                        "instance_id": "owner__repo-1",
                    },
                )
            verified = COMBINED._validate_query_evidence_root(result["root"])
            self.assertEqual(verified["evidence_digest"], result["evidence_digest"])
            self.assertEqual(set(result["probes"]), set(COMBINED.QUERY_PROBE_NAMES))

    def test_qualifier_combined_mode_is_explicit_and_projects_structural_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            cache = checkout / ".oh/.cache"
            (cache / "lance").mkdir(parents=True)
            (cache / "lance/graph.bin").write_bytes(b"graph")
            (cache / "embeddings/generations/current/lance").mkdir(parents=True)
            (cache / "embeddings/generations/current/lance/vectors.bin").write_bytes(
                b"vectors"
            )
            default_command = STRUCTURAL._qualification_scan_command(
                Path("/tmp/rna"), checkout, combined_cache=False
            )
            combined_command = STRUCTURAL._qualification_scan_command(
                Path("/tmp/rna"), checkout, combined_cache=True
            )
            self.assertIn("--no-embed", default_command)
            self.assertNotIn("--no-embed", combined_command)
            self.assertEqual(
                [part for part in default_command if part != "--no-embed"],
                combined_command,
            )

            projection = STRUCTURAL._structural_projection_checkout(
                checkout, root / "projection"
            )
            self.assertTrue((projection / ".oh/.cache/lance/graph.bin").is_file())
            self.assertFalse((projection / ".oh/.cache/embeddings").exists())
            self.assertTrue((cache / "embeddings/generations/current/lance/vectors.bin").is_file())

            operation_store = {
                "schema_version": 1,
                "reports": [
                    {
                        "operation_id": "scan-fixture",
                        "operation": "scan",
                        "state": "completed",
                        "duration_ms": 125,
                        "phases": [
                            {"phase": "lsp", "state": "ran", "duration_ms": 75},
                            {
                                "phase": "persist_graph",
                                "state": "ran",
                                "duration_ms": 10,
                            },
                            {
                                "phase": "embeddings",
                                "state": "ran",
                                "duration_ms": 40,
                            },
                        ],
                    }
                ],
            }
            STRUCTURAL.write_canonical_json(
                cache / "operation_reports.json", operation_store
            )
            self.assertEqual(
                STRUCTURAL._scan_phase_timings(checkout),
                {
                    "operation_id": "scan-fixture",
                    "total_ms": 125,
                    "structural_ms": 85,
                    "semantic_ms": 40,
                    "persistence_ms": 10,
                },
            )

            parsed = STRUCTURAL.build_parser().parse_args(
                [
                    "qualify",
                    "--lock", "lock.json",
                    "--inventory", "inventory.json",
                    "--population", "population.json",
                    "--git-cache", "git-cache",
                    "--root", "lsp",
                    "--rna", "repo-native-alignment",
                    "--output", "evidence",
                    "--combined-runtime-manifest", "manifest.json",
                    "--combined-bundle-archive", "bundle.tar.gz",
                    "--combined-upload-attestation", "upload.json",
                    "--combined-expected-manifest-sha256", "1" * 64,
                    "--combined-expected-upload-attestation-sha256", "2" * 64,
                    "--combined-expected-github-artifact-digest", "sha256:" + "3" * 64,
                    "--combined-expected-head-sha", "4" * 40,
                ]
            )
            self.assertEqual(parsed.combined_runtime_manifest, Path("manifest.json"))

    def test_cold_archive_is_deterministic_materializable_and_not_structural_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = self.fixture(root)
            first = self.archive(fixture, root / "first.tar.gz", root / "first.json")
            second = self.archive(fixture, root / "second.tar.gz", root / "second.json")

            self.assertEqual(first["archive_sha256"], second["archive_sha256"])
            structural_before = STRUCTURAL.sha256_file(fixture["structural_archive"])
            materialized = root / "materialized"
            verified = COMBINED.verify_combined_cache_archive(
                Path(first["archive_path"]), Path(first["sidecar_path"]), materialize_cache=materialized
            )
            self.assertEqual(verified["semantic_generation_digest"], fixture["generation_digest"])
            self.assertTrue((materialized / "lance/symbols.bin").is_file())
            self.assertTrue((materialized / "embeddings/current.json").is_file())
            self.assertEqual(
                STRUCTURAL.sha256_file(fixture["structural_archive"]), structural_before
            )
            catalog_root = root / "catalog"
            catalog_root.mkdir()
            catalog_path = COMBINED.publish_combined_cache_receipt(catalog_root, first)
            self.assertEqual(
                COMBINED.load_combined_cache_catalog(catalog_root)["entries"], [first]
            )
            catalog_value = json.loads(catalog_path.read_bytes())
            self.assertEqual(catalog_path.read_bytes(), STRUCTURAL.canonical_json(catalog_value))
            with self.assertRaisesRegex(COMBINED.ToolchainError, "overwrite"):
                COMBINED.publish_combined_cache_receipt(catalog_root, first)
            runtime_path = fixture["runtime_manifest"]
            runtime = json.loads(runtime_path.read_bytes())
            runtime["components"]["embedding"]["assets"]["tokenizer.json"]["sha256"] = "0" * 64
            STRUCTURAL.write_canonical_json(runtime_path, runtime)
            with self.assertRaisesRegex(COMBINED.ToolchainError, "tokenizer"):
                self.archive(
                    fixture,
                    root / "asset-mismatch.tar.gz",
                    root / "asset-mismatch.json",
                )
            self.runtime_fixture(runtime_path)
            runtime = json.loads(runtime_path.read_bytes())
            runtime["components"]["embedding"]["files"][0]["size"] += 1
            STRUCTURAL.write_canonical_json(runtime_path, runtime)
            with self.assertRaisesRegex(COMBINED.ToolchainError, "inventory digest"):
                self.archive(
                    fixture,
                    root / "file-inventory-mismatch.tar.gz",
                    root / "file-inventory-mismatch.json",
                )
            with self.assertRaises(COMBINED.ToolchainError):
                COMBINED.verify_combined_cache_archive(
                    fixture["structural_archive"], fixture["structural_sidecar"]
                )

    def test_semantic_verifier_rejects_drift_but_projects_only_active_generation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = self.fixture(root)
            semantic_root = fixture["semantic_root"]
            prior = semantic_root / "generations" / ("e" * 64) / "lance"
            prior.mkdir(parents=True)
            (prior / "prior.bin").write_bytes(b"retained immutable prior")

            verified = COMBINED.verify_semantic_cache_root(semantic_root)
            paths = {member["path"] for member in verified["members"]}
            self.assertFalse(any(("e" * 64) in path for path in paths))
            receipt = self.archive(fixture, root / "combined.tar.gz", root / "combined.json")
            with tarfile.open(receipt["archive_path"], "r:gz") as archive:
                names = {member.name for member in archive.getmembers()}
            self.assertFalse(any(("e" * 64) in name for name in names))
            self.assertTrue((prior / "prior.bin").is_file())

            manifest_path = semantic_root / "generations" / fixture["generation_digest"] / "manifest.json"
            manifest = json.loads(manifest_path.read_bytes())
            manifest["semantic_identity"]["schema_signature"] = "wrong"
            manifest_path.write_bytes(COMBINED.semantic_canonical_json(manifest))
            with self.assertRaisesRegex(COMBINED.ToolchainError, "schema signature"):
                COMBINED.verify_semantic_cache_root(semantic_root)

    def test_combined_archive_rejects_tamper_traversal_links_partial_and_extra(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = self.fixture(root)
            receipt = self.archive(fixture, root / "valid.tar.gz", root / "valid.json")
            archive_path = Path(receipt["archive_path"])
            sidecar_path = Path(receipt["sidecar_path"])
            original_archive = archive_path.read_bytes()
            archive_path.write_bytes(original_archive + b"tamper")
            with self.assertRaisesRegex(COMBINED.ToolchainError, "size|digest"):
                COMBINED.verify_combined_cache_archive(archive_path, sidecar_path)
            archive_path.write_bytes(original_archive)

            sidecar = json.loads(sidecar_path.read_bytes())
            structural_archive_member = sidecar["core"]["structural"]["archive_member"]
            hostile_cases = {
                "traversal": [("file", "combined/../escape", b"escape")],
                "symlink": [("symlink", "combined/components/link", b"")],
                "extra": [("file", "combined/components/extra.bin", b"extra")],
                "partial": [],
            }
            for label, additions in hostile_cases.items():
                with self.subTest(label=label):
                    hostile_archive = root / f"{label}.tar.gz"
                    hostile_sidecar = root / f"{label}.json"
                    self.write_hostile_archive(
                        archive_path,
                        hostile_archive,
                        hostile_sidecar,
                        sidecar,
                        additions,
                        omit=(structural_archive_member if label == "partial" else None),
                    )
                    with self.assertRaises(COMBINED.ToolchainError):
                        COMBINED.verify_combined_cache_archive(hostile_archive, hostile_sidecar)

    def test_case1_to_case2_selection_injection_and_lineage(self) -> None:
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
            self.git(source, "commit", "--quiet", "-m", "case 1")
            commit1 = self.git(source, "rev-parse", "HEAD")
            tree1 = self.git(source, "rev-parse", "HEAD^{tree}")
            (source / "src/a.py").write_text("VALUE = 2\n")
            self.git(source, "commit", "--quiet", "-am", "case 2")
            commit2 = self.git(source, "rev-parse", "HEAD")
            tree2 = self.git(source, "rev-parse", "HEAD^{tree}")
            git_dir = root / "owner__repo.git"
            subprocess.run(
                ["git", "clone", "--quiet", "--bare", str(source), str(git_dir)], check=True
            )

            fixture1 = self.fixture(root / "one", commit=commit1, tree=tree1)
            receipt1 = self.archive(fixture1, root / "case1.tar.gz", root / "case1.json")
            target_identity = copy.deepcopy(fixture1["identity"])
            target_identity["commit"] = commit2
            target_identity["tree"] = tree2
            selection = COMBINED.select_combined_cache(
                [receipt1],
                "owner/repo",
                commit2,
                target_identity,
                git_dir,
                2,
                runtime_manifest_path=fixture1["runtime_manifest"],
                semantic_identity=fixture1["semantic_identity"],
                scan_flags=self.scan_flags(),
                toolchain_lock_digest="1" * 64,
                inventory_digest="2" * 64,
                inventory_file_sha256="3" * 64,
            )
            self.assertIsNotNone(selection)
            self.assertEqual(selection["diff"]["changed_paths"], ["src/a.py"])
            target_checkout = root / "target"
            (target_checkout / "src").mkdir(parents=True)
            (target_checkout / "src/a.py").write_text("VALUE = 2\n")
            injected = COMBINED.inject_combined_cache(
                selection,
                target_checkout,
                target_identity,
                git_dir,
                toolchain_lock_digest="1" * 64,
                inventory_digest="2" * 64,
                inventory_file_sha256="3" * 64,
            )
            self.assertEqual(injected["changed_file_count"], 1)
            self.assertEqual(
                injected["base_semantic_generation_digest"], fixture1["generation_digest"]
            )
            self.assertTrue(
                (target_checkout / ".oh/.cache/structural-cache-inheritance.json").is_file()
            )
            self.assertTrue((target_checkout / ".oh/.cache/embeddings/current.json").is_file())

            base = selection["base_combined_cache"]
            identical_fixture = self.fixture(
                root / "identical-generation",
                commit=commit2,
                tree=tree2,
                graph_digest="6" * 64,
                structural_base={
                    "archive_sha256": fixture1["structural_receipt"]["archive_sha256"],
                    "sidecar_sha256": fixture1["structural_receipt"]["sidecar_sha256"],
                    "core_sha256": fixture1["structural_receipt"]["core_sha256"],
                    "report_digest": "report-digest",
                },
            )
            identical_receipt = self.archive(
                identical_fixture,
                root / "identical-generation.tar.gz",
                root / "identical-generation.json",
                case_index=2,
                base=base,
                inherited_vectors=1,
                encoded_vectors=0,
            )
            self.assertEqual(
                identical_receipt["semantic_generation_digest"], fixture1["generation_digest"]
            )

            fixture2 = self.fixture(
                root / "two",
                commit=commit2,
                tree=tree2,
                graph_digest="7" * 64,
                prior_generation=fixture1["generation_digest"],
                reused_vectors=1,
                structural_base={
                    "archive_sha256": fixture1["structural_receipt"]["archive_sha256"],
                    "sidecar_sha256": fixture1["structural_receipt"]["sidecar_sha256"],
                    "core_sha256": fixture1["structural_receipt"]["core_sha256"],
                    "report_digest": "report-digest",
                },
            )
            receipt2 = self.archive(
                fixture2,
                root / "case2.tar.gz",
                root / "case2.json",
                case_index=2,
                base=base,
                inherited_vectors=1,
                encoded_vectors=0,
            )
            verified2 = COMBINED.verify_combined_cache_archive(
                Path(receipt2["archive_path"]), Path(receipt2["sidecar_path"])
            )
            verified1 = COMBINED.verify_combined_cache_archive(
                Path(receipt1["archive_path"]), Path(receipt1["sidecar_path"])
            )
            STRUCTURAL._require_actual_combined_incremental_lineage(
                COMBINED, receipt1, verified1, receipt2, verified2
            )
            catalog_root = root / "lineage-catalog"
            catalog_root.mkdir()
            COMBINED.publish_combined_cache_receipt(catalog_root, receipt1)
            runtime = COMBINED._project_runtime_manifest(fixture1["runtime_manifest"])
            STRUCTURAL._require_combined_case2_selection(
                COMBINED,
                output_root=catalog_root,
                first_case_index=1,
                first_instance={
                    "instance_id": "owner__repo-1",
                    "repo": "owner/repo",
                    "base_commit": commit1,
                },
                runtime=runtime,
                selection=selection,
            )
            COMBINED.publish_combined_cache_receipt(catalog_root, receipt2)
            STRUCTURAL._require_combined_pair_ready(
                COMBINED,
                output_root=catalog_root,
                indexed_instances=[
                    (
                        1,
                        {
                            "instance_id": "owner__repo-1",
                            "repo": "owner/repo",
                            "base_commit": commit1,
                        },
                    ),
                    (
                        2,
                        {
                            "instance_id": "owner__repo-2",
                            "repo": "owner/repo",
                            "base_commit": commit2,
                        },
                    ),
                ],
                runtime=runtime,
            )
            self.assertEqual(verified2["core"]["base_combined_cache"], base)
            self.assertEqual(
                verified2["core"]["semantic"]["prior_generation_digest"],
                fixture1["generation_digest"],
            )

            null_receipt = copy.deepcopy(receipt2)
            null_verified = copy.deepcopy(verified2)
            null_receipt["base_combined_cache"] = None
            null_verified["core"]["base_combined_cache"] = None
            with self.assertRaisesRegex(STRUCTURAL.ToolchainError, "null case-1"):
                STRUCTURAL._require_actual_combined_incremental_lineage(
                    COMBINED,
                    receipt1,
                    verified1,
                    null_receipt,
                    null_verified,
                )

            wrong_receipt = copy.deepcopy(receipt2)
            wrong_verified = copy.deepcopy(verified2)
            wrong_base = copy.deepcopy(base)
            wrong_base["archive_sha256"] = "0" * 64
            wrong_receipt["base_combined_cache"] = wrong_base
            wrong_verified["core"]["base_combined_cache"] = wrong_base
            with self.assertRaisesRegex(STRUCTURAL.ToolchainError, "wrong immutable"):
                STRUCTURAL._require_actual_combined_incremental_lineage(
                    COMBINED,
                    receipt1,
                    verified1,
                    wrong_receipt,
                    wrong_verified,
                )

            cold_verified = copy.deepcopy(verified2)
            cold_verified["core"]["work"].update(
                {
                    "structural_inherited_file_count": 0,
                    "structural_inherited_operation_count": 0,
                    "vector_inherited_count": 0,
                }
            )
            with self.assertRaisesRegex(STRUCTURAL.ToolchainError, "cold instead"):
                STRUCTURAL._require_actual_combined_incremental_lineage(
                    COMBINED,
                    receipt1,
                    verified1,
                    receipt2,
                    cold_verified,
                )

    def fixture(
        self,
        root: Path,
        *,
        commit: str = "a" * 40,
        tree: str = "b" * 40,
        graph_digest: str = "6" * 64,
        prior_generation: str | None = None,
        reused_vectors: int = 0,
        structural_base: dict[str, object] | None = None,
    ) -> dict[str, object]:
        root.mkdir(parents=True, exist_ok=True)
        checkout = root / "checkout"
        cache = checkout / ".oh/.cache"
        (cache / "lance").mkdir(parents=True)
        report = {
            "identity": {"checkout_sha": commit},
            "files": [
                {
                    "path": "src/a.py",
                    "language": "python",
                    "terminal_status": {"status": "processed"},
                    "expected_result_ids": ["result-1"],
                    "persisted_results": {"provenance": ["result-1"]},
                }
            ],
            "violations": [],
            "digest": "report-digest",
            "graph_snapshot_digest": graph_digest,
        }
        STRUCTURAL.write_canonical_json(cache / "lsp_completeness.json", report)
        STRUCTURAL.write_canonical_json(
            cache / "lsp_pass1_work_items.json",
            {
                "records": {
                    "job:1": {
                        "state": "completed",
                        "file": "src/a.py",
                        "input_hash": "input",
                        "requested_operations": ["textDocument/documentSymbol"],
                        "produced_result_ids": ["result-1"],
                    }
                }
            },
        )
        (cache / "lance/symbols.bin").write_bytes(b"structural graph")
        identity = {
            "schema_version": 1,
            "repository": "owner/repo",
            "commit": commit,
            "tree": tree,
            "root_slug": "checkout",
            "configuration_digest": "configuration",
            "inventory_policy_digest": "policy",
            "context_mode": "disabled",
            "producer": {
                "producer_commit": "c" * 40,
                "package_version": "1.0.0",
                "binary_sha256": "4" * 64,
                "graph_schema_version": 25,
                "graph_schema_signature": "5" * 64,
                "completeness_schema_version": 6,
                "work_item_schema_version": 4,
                "validation_evidence_schema_version": 1,
            },
            "shared_influence_digest": "8" * 64,
            "partitions": {
                "python": {
                    "language": "python",
                    "descriptor_signature": "9" * 64,
                    "influence_patterns": ["pyproject.toml"],
                    "influence_digest": "0" * 64,
                    "signature": "f" * 64,
                    "matched_file_count": 1,
                }
            },
        }
        structural_archive = root / "structural.tar.gz"
        structural_sidecar = root / "structural.json"
        structural_receipt = STRUCTURAL.archive_structural_cache(
            checkout,
            structural_archive,
            structural_sidecar,
            identity=identity,
            toolchain_lock_digest="1" * 64,
            inventory_digest="2" * 64,
            inventory_file_sha256="3" * 64,
            case_inventory_digest="d" * 64,
            base_cache=structural_base,
        )
        semantic_root, semantic_identity, generation_digest = self.semantic_fixture(
            root,
            graph_digest=graph_digest,
            prior_generation=prior_generation,
            reused_vectors=reused_vectors,
        )
        runtime_manifest = root / "semantic-bundle-manifest.json"
        self.runtime_fixture(runtime_manifest)
        return {
            "root": root,
            "identity": identity,
            "structural_archive": structural_archive,
            "structural_sidecar": structural_sidecar,
            "structural_receipt": structural_receipt,
            "semantic_root": semantic_root,
            "semantic_identity": semantic_identity,
            "generation_digest": generation_digest,
            "runtime_manifest": runtime_manifest,
            "commit": commit,
            "tree": tree,
        }

    def semantic_fixture(
        self,
        root: Path,
        *,
        graph_digest: str,
        prior_generation: str | None,
        reused_vectors: int,
    ) -> tuple[Path, dict[str, object], str]:
        semantic_root = root / "embeddings"
        files = self.runtime_file_inventories()
        semantic_identity = {
            "model": COMBINED.EMBEDDING_MODEL_ID,
            "tokenizer": COMBINED.EMBEDDING_TOKENIZER_IDENTITY,
            "model_files_digest": STRUCTURAL.sha256_bytes(
                STRUCTURAL.canonical_json(files["embedding"])
            ),
            "model_sha256": "e" * 64,
            "tokenizer_sha256": "f" * 64,
            "reranker_files_digest": STRUCTURAL.sha256_bytes(
                STRUCTURAL.canonical_json(files["reranker"])
            ),
            "preprocessing_version": COMBINED.EMBEDDING_PREPROCESSING_VERSION,
            "artifact_sha256": "4" * 64,
            "schema_signature": COMBINED.SEMANTIC_SCHEMA_SIGNATURE,
            "dimension": COMBINED.EMBEDDING_DIMENSION,
            "flags": {"business_context": "disabled", "full": "true"},
        }
        rows = [
            {
                "id": "node:1",
                "canonical_input_digest": "1" * 64,
                "vector_sha256": "2" * 64,
            }
        ]
        canonical_input_digest = COMBINED._sha256_semantic(
            [{"id": row["id"], "canonical_input_digest": row["canonical_input_digest"]} for row in rows]
        )
        target_graph_digest = "3" * 64
        generation_digest = COMBINED._sha256_semantic(
            {
                "schema_version": 1,
                "semantic_identity": semantic_identity,
                "canonical_input_digest": canonical_input_digest,
                "target_graph_digest": target_graph_digest,
                "structural_graph_snapshot_digest": graph_digest,
            }
        )
        generation = semantic_root / "generations" / generation_digest
        lance = generation / "lance"
        lance.mkdir(parents=True)
        (lance / "rows.bin").write_bytes(b"lance rows")
        coverage = {
            "schema_version": 1,
            "generation_digest": generation_digest,
            "semantic_identity_digest": COMBINED._sha256_semantic(semantic_identity),
            "canonical_input_digest": canonical_input_digest,
            "target_graph_digest": target_graph_digest,
            "structural_graph_snapshot_digest": graph_digest,
            "row_count": len(rows),
            "rows": rows,
        }
        coverage_path = generation / "coverage.json"
        coverage_path.write_bytes(COMBINED.semantic_canonical_json(coverage))
        lance_members = COMBINED._regular_tree(lance, "fixture Lance")
        manifest = {
            "schema_version": 1,
            "generation_digest": generation_digest,
            "semantic_identity": semantic_identity,
            "semantic_identity_digest": COMBINED._sha256_semantic(semantic_identity),
            "canonical_input_digest": canonical_input_digest,
            "target_graph_digest": target_graph_digest,
            "structural_graph_snapshot_digest": graph_digest,
            "row_count": len(rows),
            "coverage_digest": STRUCTURAL.sha256_file(coverage_path),
            "lance_tree_digest": COMBINED._lance_tree_digest(lance_members),
            "reused_vector_count": reused_vectors,
            "encoded_vector_count": len(rows) - reused_vectors,
            "created_by_artifact_sha256": "4" * 64,
            "device_attestation": {
                "required_device": "metal",
                "observed_device": "metal",
                "backend": "candle-metal",
                "device_index": 0,
                "artifact_sha256": "4" * 64,
            },
        }
        if prior_generation is not None:
            manifest["prior_generation_digest"] = prior_generation
        manifest_path = generation / "manifest.json"
        manifest_path.write_bytes(COMBINED.semantic_canonical_json(manifest))
        verification = {
            "schema_version": 1,
            "generation_digest": generation_digest,
            "manifest_sha256": STRUCTURAL.sha256_file(manifest_path),
            "coverage_digest": STRUCTURAL.sha256_file(coverage_path),
            "lance_tree_digest": manifest["lance_tree_digest"],
            "structural_graph_snapshot_digest": graph_digest,
            "target_graph_digest": target_graph_digest,
            "row_count": len(rows),
            "one_to_one_coverage": True,
            "fresh_reopen_ready": True,
        }
        verification_path = generation / "verification.json"
        verification_path.write_bytes(COMBINED.semantic_canonical_json(verification))
        current = {
            "schema_version": 1,
            "generation_digest": generation_digest,
            "manifest_sha256": STRUCTURAL.sha256_file(manifest_path),
            "verification_sha256": STRUCTURAL.sha256_file(verification_path),
        }
        semantic_root.mkdir(parents=True, exist_ok=True)
        (semantic_root / "current.json").write_bytes(COMBINED.semantic_canonical_json(current))
        return semantic_root, semantic_identity, generation_digest

    @staticmethod
    def runtime_fixture(path: Path) -> None:
        files = SwebenchCombinedCacheTests.runtime_file_inventories()
        embedding_by_name = {
            Path(record["path"]).name: record for record in files["embedding"]
        }
        manifest = {
            "schema": COMBINED.SEMANTIC_BUNDLE_SCHEMA,
            "artifact": {
                "name": (
                    "repo-native-alignment-swebench-semantic-darwin-arm64-apple-m4-"
                    + "c" * 40
                ),
                "archive_file": (
                    "repo-native-alignment-swebench-semantic-darwin-arm64-apple-m4-"
                    + "c" * 40
                    + ".tar.gz"
                ),
                "archive_sha256": "a" * 64,
            },
            "provenance": {
                "repository": "open-horizon-labs/repo-native-alignment",
                "head_sha": "c" * 40,
                "workflow": ".github/workflows/swebench-semantic-bundle.yml",
                "job": "build-semantic-bundle",
                "run_id": 1,
                "run_attempt": 1,
            },
            "host": {
                "architecture": "arm64",
                "chip": "Apple M4 Max",
                "metal_device_observed": True,
                "system_profiler_sha256": "7" * 64,
            },
            "build": {
                "target": "aarch64-apple-darwin",
                "target_cpu": "apple-m4",
                "features": ["embeddings", "metal", "swebench-semantic-bundle"],
                "profile": "release",
                "rustc": "rustc fixture",
                "cargo": "cargo fixture",
                "rustflags": "-C target-cpu=apple-m4",
                "link_flags": ["-Wl,-dead_strip"],
                "metal_kernel_profile": "release-fast-math",
                "candle_metal_enable_fast_math": True,
                "metal_kernel_compilation": "embedded-source-runtime",
            },
            "components": {
                "executable": {"path": "repo-native-alignment", "sha256": "4" * 64},
                "embedding": {
                    "model_id": COMBINED.EMBEDDING_MODEL_ID,
                    "files": files["embedding"],
                    "files_digest": STRUCTURAL.sha256_bytes(
                        STRUCTURAL.canonical_json(files["embedding"])
                    ),
                    "assets": {
                        name: embedding_by_name[name]
                        for name in ("config.json", "tokenizer.json", "model.safetensors")
                    },
                },
                "reranker": {
                    "model_id": COMBINED.RERANKER_MODEL_ID,
                    "files": files["reranker"],
                    "files_digest": STRUCTURAL.sha256_bytes(
                        STRUCTURAL.canonical_json(files["reranker"])
                    ),
                },
                "lsp": {
                    "toolchain_lock_sha256": "1" * 64,
                    "inventory_sha256": "3" * 64,
                    "descriptor_inventory_sha256": "d" * 64,
                    "provision_receipt_sha256": "e" * 64,
                    "probe_receipt_sha256": "f" * 64,
                    "files": files["lsp"],
                    "files_digest": STRUCTURAL.sha256_bytes(
                        STRUCTURAL.canonical_json(files["lsp"])
                    ),
                },
            },
            "qualification": {
                "strict_mode": True,
                "offline": True,
                "embeddings": True,
                "rerank": True,
                "metal": True,
                "lsp": True,
                "fallbacks": [],
                "evidence_sha256": "9" * 64,
                "lsp_readiness_sha256": "8" * 64,
            },
        }
        STRUCTURAL.write_canonical_json(path, manifest)

    @staticmethod
    def runtime_file_inventories() -> dict[str, list[dict[str, object]]]:
        embedding = [
            {
                "path": "models/snapshots/fixture/config.json",
                "size": 1,
                "sha256": "d" * 64,
            },
            {
                "path": "models/snapshots/fixture/model.safetensors",
                "size": 3,
                "sha256": "e" * 64,
            },
            {
                "path": "models/snapshots/fixture/tokenizer.json",
                "size": 2,
                "sha256": "f" * 64,
            },
        ]
        reranker = [
            {"path": "model.onnx", "size": 4, "sha256": "c" * 64},
        ]
        lsp = [
            {"path": "descriptor-inventory.json", "size": 1, "sha256": "d" * 64},
            {"path": "inventory.json", "size": 1, "sha256": "3" * 64},
            {"path": "probe-receipt.json", "size": 1, "sha256": "f" * 64},
            {"path": "provision-receipt.json", "size": 1, "sha256": "e" * 64},
            {"path": "toolchain-lock.json", "size": 1, "sha256": "1" * 64},
        ]
        return {"embedding": embedding, "reranker": reranker, "lsp": lsp}

    def archive(
        self,
        fixture: dict[str, object],
        archive: Path,
        sidecar: Path,
        *,
        case_index: int = 1,
        base: dict[str, object] | None = None,
        inherited_vectors: int = 0,
        encoded_vectors: int = 1,
    ) -> dict[str, object]:
        query_evidence = archive.with_name(f"{archive.name}.query-evidence")
        self.query_evidence_fixture(
            query_evidence,
            case_index=case_index,
            instance_id=f"owner__repo-{case_index}",
        )
        return COMBINED.archive_combined_cache(
            fixture["structural_archive"],
            fixture["structural_sidecar"],
            fixture["semantic_root"],
            fixture["runtime_manifest"],
            query_evidence,
            archive,
            sidecar,
            case_identity={
                "case_index": case_index,
                "attempt_index": 1,
                "instance_id": f"owner__repo-{case_index}",
            },
            repository="owner/repo",
            commit=fixture["commit"],
            tree=fixture["tree"],
            scan_flags=self.scan_flags(),
            work={
                "structural_inherited_file_count": 0 if base is None else 1,
                "structural_executed_file_count": 1,
                "structural_invalidated_file_count": 0,
                "structural_inherited_operation_count": 0 if base is None else 1,
                "structural_executed_operation_count": 1,
                "vector_inherited_count": inherited_vectors,
                "vector_encoded_count": encoded_vectors,
                "vector_purged_count": 0,
            },
            timings_ms={field: 1 for field in COMBINED.TIMING_FIELDS},
            peak_memory_bytes={
                field: 1 for field in COMBINED.PEAK_MEMORY_FIELDS
            },
            base_combined_cache=base,
        )

    @staticmethod
    def query_evidence_fixture(
        root: Path, *, case_index: int, instance_id: str
    ) -> None:
        root.mkdir()
        probes: dict[str, dict[str, object]] = {}
        for name in COMBINED.QUERY_PROBE_NAMES:
            stdout = root / f"{name}.stdout"
            stderr = root / f"{name}.stderr"
            stdout.write_text(f"{name} output\n")
            stderr.write_bytes(b"")
            probes[name] = {
                "duration_ms": 1,
                "ttfe_ms": 1,
                "peak_memory_bytes": 1,
                "stdout_file": stdout.name,
                "stdout_sha256": STRUCTURAL.sha256_file(stdout),
                "stderr_file": stderr.name,
                "stderr_sha256": STRUCTURAL.sha256_file(stderr),
            }
        receipt = {
            "schema_version": COMBINED.QUERY_EVIDENCE_SCHEMA_VERSION,
            "status": "ready",
            "case": {
                "case_index": case_index,
                "attempt_index": 1,
                "instance_id": instance_id,
            },
            "query": STRUCTURAL.COMBINED_QUERY,
            "retrieval": {"mode": "hybrid", "fusion": "rrf", "rerank": True},
            "selected_node_id": "src/lib.rs:fixture:function",
            "strict_sentinel": STRUCTURAL.COMBINED_STRICT_SEARCH_SENTINEL,
            "repeat_stable": True,
            "probes": probes,
            "peak_memory_bytes": 1,
            "evidence_digest": "",
        }
        receipt["evidence_digest"] = STRUCTURAL.sha256_bytes(
            STRUCTURAL.canonical_json(receipt)
        )
        STRUCTURAL.write_canonical_json(
            root / COMBINED.QUERY_EVIDENCE_RECEIPT, receipt
        )

    @staticmethod
    def scan_flags() -> list[str]:
        return ["--business-context=disabled", "scan", "--full", "--timings"]

    @staticmethod
    def write_hostile_archive(
        valid_archive: Path,
        output_archive: Path,
        output_sidecar: Path,
        original_sidecar: dict[str, object],
        additions: list[tuple[str, str, bytes]],
        *,
        omit: str | None,
    ) -> None:
        with tarfile.open(valid_archive, "r:gz") as source:
            existing = []
            for member in source.getmembers():
                if member.isfile():
                    stream = source.extractfile(member)
                    data = b"" if stream is None else stream.read()
                else:
                    data = b""
                if omit is not None and member.name == f"combined/{omit}":
                    continue
                existing.append((member, data))
        with output_archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as archive:
                    for member, data in existing:
                        archive.addfile(member, io.BytesIO(data) if member.isfile() else None)
                    for kind, name, data in additions:
                        info = tarfile.TarInfo(name)
                        info.mode = 0o644
                        info.uid = info.gid = 0
                        info.mtime = COMBINED.FIXED_MTIME
                        if kind == "symlink":
                            info.type = tarfile.SYMTYPE
                            info.linkname = "../escape"
                            archive.addfile(info)
                        else:
                            info.size = len(data)
                            archive.addfile(info, io.BytesIO(data))
        sidecar = copy.deepcopy(original_sidecar)
        sidecar["archive_name"] = output_archive.name
        sidecar["archive_size_bytes"] = output_archive.stat().st_size
        sidecar["archive_sha256"] = STRUCTURAL.sha256_file(output_archive)
        STRUCTURAL.write_canonical_json(output_sidecar, sidecar)

    @staticmethod
    def git(root: Path, *args: str) -> str:
        return subprocess.check_output(["git", *args], cwd=root, text=True).strip()


if __name__ == "__main__":
    unittest.main()
