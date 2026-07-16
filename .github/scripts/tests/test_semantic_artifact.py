from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tarfile
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[3]
PREPARE = REPO / ".github/scripts/prepare-semantic-models.py"
PACKAGE = REPO / ".github/scripts/package-semantic-artifact.py"
QUALIFY = REPO / ".github/scripts/qualify-semantic-artifact.py"


def load_qualifier():
    spec = importlib.util.spec_from_file_location("qualify_semantic_artifact", QUALIFY)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def sample_report() -> dict:
    return {
        "requested_mode": "hybrid",
        "effective_mode": "hybrid",
        "keyword_candidates": 3,
        "vector_candidates": 4,
        "fusion_candidates": 5,
        "fusion": "reciprocal_rank_fusion",
        "rerank_candidates": 5,
        "rerank_applied": True,
        "degradations": [],
        "embedding_model": "sentence-transformers/all-MiniLM-L6-v2",
        "rerank_model": "jinaai/jina-reranker-v1-turbo-en",
        "acceleration": "metal",
        "index_generation_unix_ms": 1,
        "index_blake3": "a" * 64,
        "index_hash_scope": "active_lance_manifests",
        "results": [
            {
                "id": "src/lib.rs:hello:function",
                "kind": "code:function",
                "title": "hello",
                "retrieval_rank": 2,
                "retrieval_score": 0.5,
                "rerank_score": 0.75,
                "final_rank": 1,
            }
        ],
    }


class SemanticArtifactScriptsTest(unittest.TestCase):
    def test_run_json_surfaces_captured_failure_evidence(self) -> None:
        qualifier = load_qualifier()
        command = [
            sys.executable,
            "-c",
            "import sys; print('visible-out'); "
            "print('visible-err', file=sys.stderr); sys.exit(7)",
        ]
        with self.assertRaisesRegex(RuntimeError, "exit: 7") as raised:
            qualifier.run_json(command)
        message = str(raised.exception)
        self.assertIn("visible-out", message)
        self.assertIn("visible-err", message)
        self.assertIn(f"command: {shlex.join(command)}", message)

    def test_package_manifest_digest_covers_payload(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "repo-native-alignment"
            lock = root / "model-lock.json"
            output = root / "artifact"
            archive = root / "repo-native-alignment-darwin-arm64-m4-semantic.tar.gz"
            binary.write_bytes(b"binary")
            lock.write_text('{"schema_version":1,"models":[]}')

            subprocess.run(
                [
                    sys.executable,
                    str(PACKAGE),
                    "--binary",
                    str(binary),
                    "--model-lock",
                    str(lock),
                    "--output",
                    str(output),
                    "--archive",
                    str(archive),
                    "--git-sha",
                    "abc123",
                    "--run-id",
                    "7",
                    "--run-attempt",
                    "2",
                ],
                check=True,
            )

            manifest = json.loads((output / "artifact-manifest.json").read_text())
            expected = hashlib.sha256(
                json.dumps(
                    manifest["files"], sort_keys=True, separators=(",", ":")
                ).encode()
            ).hexdigest()
            self.assertEqual(manifest["qualification_digest"], expected)
            self.assertEqual(manifest["cpu"], "apple-m4")
            self.assertEqual(manifest["features"], ["metal"])
            self.assertTrue(archive.is_file())
            archive_checksum = Path(f"{archive}.sha256").read_text().split()[0]
            self.assertEqual(
                archive_checksum, hashlib.sha256(archive.read_bytes()).hexdigest()
            )
            extracted = root / "extracted"
            extracted.mkdir()
            with tarfile.open(archive, "r:gz") as package:
                self.assertEqual(
                    sorted(package.getnames()),
                    [
                        "SHA256SUMS",
                        "artifact-manifest.json",
                        "model-lock.json",
                        "repo-native-alignment",
                    ],
                )
                package.extractall(extracted, filter="data")
            self.assertEqual(
                (extracted / "repo-native-alignment").read_bytes(), binary.read_bytes()
            )

    def test_reject_extra_fails_on_unattested_cache_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "home"
            cache = home / ".cache/huggingface/hub/models--owner--model"
            snapshot = cache / "snapshots/revision"
            snapshot.mkdir(parents=True)
            payload = b"locked"
            (snapshot / "model.bin").write_bytes(payload)
            refs = cache / "refs"
            refs.mkdir()
            (refs / "main").write_text("revision")
            lock = root / "model-lock.json"
            lock.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "models": [
                            {
                                "purpose": "embedding",
                                "repository": "owner/model",
                                "revision": "revision",
                                "cache": "huggingface",
                                "files": [
                                    {
                                        "path": "model.bin",
                                        "sha256": hashlib.sha256(payload).hexdigest(),
                                        "size": len(payload),
                                    }
                                ],
                            }
                        ],
                    }
                )
            )
            base = [
                sys.executable,
                str(PREPARE),
                "--lock",
                str(lock),
                "--home",
                str(home),
                "--verify-only",
                "--reject-extra",
            ]
            subprocess.run(base, check=True)
            (snapshot / "unattested.bin").write_bytes(b"extra")
            rejected = subprocess.run(base, text=True, capture_output=True)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("unattested model cache files", rejected.stderr)

    def test_reopen_comparison_rejects_rank_or_score_drift(self) -> None:
        qualifier = load_qualifier()
        first = sample_report()
        qualifier.validate_search_report(first)
        identical = json.loads(json.dumps(first))
        qualifier.compare_reopened_report(first, identical)

        changed_score = json.loads(json.dumps(first))
        changed_score["results"][0]["rerank_score"] += 0.01
        with self.assertRaisesRegex(RuntimeError, "rerank_score"):
            qualifier.compare_reopened_report(first, changed_score)

        changed_rank = json.loads(json.dumps(first))
        changed_rank["results"][0]["final_rank"] = 2
        with self.assertRaisesRegex(RuntimeError, "contiguous"):
            qualifier.compare_reopened_report(first, changed_rank)

    def test_traversal_requires_resolved_non_empty_context(self) -> None:
        qualifier = load_qualifier()
        node = "src/lib.rs:hello:function"
        output = (
            f"## Graph neighbors (outgoing) of `{node}`\n\n"
            "2 result(s)\n\n- neighbor"
        )
        self.assertEqual(qualifier.traversal_result_count(output, node), 2)
        with self.assertRaisesRegex(RuntimeError, "did not resolve"):
            qualifier.traversal_result_count(
                "No graph nodes found for `missing`.", node
            )
        with self.assertRaisesRegex(RuntimeError, "no graph context"):
            qualifier.traversal_result_count(
                f"## Graph neighbors (outgoing) of `{node}`\n\n0 result(s)", node
            )


if __name__ == "__main__":
    unittest.main()
