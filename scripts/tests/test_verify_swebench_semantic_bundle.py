from __future__ import annotations

import gzip
import json
import os
import stat
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import verify_swebench_semantic_bundle as verifier  # noqa: E402


class SemanticBundleVerifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = Path(tempfile.mkdtemp(prefix="rna-semantic-verifier-test-"))

    def tearDown(self) -> None:
        for path in sorted(self.temporary.rglob("*"), reverse=True):
            try:
                path.chmod(0o755 if path.is_dir() else 0o644)
            except FileNotFoundError:
                pass
        import shutil

        shutil.rmtree(self.temporary)

    def _write(self, root: Path, relative: str, value: bytes) -> Path:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)
        return path

    def _fixture(
        self,
        *,
        hostile_member: str | None = None,
        hostile_type: bytes | None = None,
        tamper_payload: bool = False,
    ) -> dict[str, object]:
        head = "a" * 40
        artifact_name = (
            "repo-native-alignment-swebench-semantic-darwin-arm64-apple-m4-"
            f"{head}"
        )
        bundle = self.temporary / f"bundle-{len(list(self.temporary.iterdir()))}"
        bundle.mkdir()
        executable = self._write(bundle, "repo-native-alignment", b"arm64-binary")
        executable.chmod(0o755)
        encoder_root = bundle / "components/models/huggingface"
        reranker_root = bundle / "components/models/reranker"
        lsp_root = bundle / "components/lsp"
        snapshot = "hub/models--sentence-transformers--all-MiniLM-L6-v2/snapshots/revision"
        self._write(encoder_root, f"{snapshot}/config.json", b"{}")
        self._write(encoder_root, f"{snapshot}/tokenizer.json", b'{"tokenizer":1}')
        self._write(encoder_root, f"{snapshot}/model.safetensors", b"safetensors")
        self._write(reranker_root, "model.onnx", b"onnx")
        self._write(reranker_root, "tokenizer.json", b'{"tokenizer":2}')
        self._write(reranker_root, "config.json", b"{}")
        for name in (
            "toolchain-lock.json",
            "inventory.json",
            "descriptor-inventory.json",
            "provision-receipt.json",
            "probe-receipt.json",
        ):
            self._write(lsp_root, name, f'{{"name":"{name}"}}'.encode())
        self._write(lsp_root, "artifact-cache.tar.gz", b"lsp-cache")
        profiler = self._write(bundle, "evidence/apple-system-profiler.json", b'{"chip":"M4"}')
        readiness = self._write(bundle, "evidence/offline-lsp-readiness.json", b'{"ready":true}')
        self._write(bundle, "evidence/offline-full-scan.stdout", b"READY")
        self._write(bundle, "evidence/offline-full-scan.stderr", b"")
        self._write(bundle, "evidence/offline-lsp-readiness.stderr", b"")
        self._write(bundle, "evidence/offline-strict-search.stdout", b"status=READY")
        self._write(bundle, "evidence/offline-strict-search.stderr", b"")

        encoder_files = verifier.regular_file_records(encoder_root)
        reranker_files = verifier.regular_file_records(reranker_root)
        lsp_files = verifier.regular_file_records(lsp_root)
        by_encoder_name = {
            Path(record["path"]).name: record
            for record in encoder_files
            if "snapshots" in Path(record["path"]).parts
        }
        by_lsp_name = {record["path"]: record for record in lsp_files}
        payload = {
            "schema": verifier.PAYLOAD_SCHEMA,
            "files": verifier.regular_file_records(bundle),
        }
        self._write(bundle, "payload-file-manifest.json", verifier.canonical_json(payload))
        if tamper_payload:
            executable.write_bytes(b"tampered-after-payload-manifest")

        for model_root in (encoder_root, reranker_root):
            for path in sorted(model_root.rglob("*"), reverse=True):
                path.chmod(0o555 if path.is_dir() else 0o444)
            model_root.chmod(0o555)

        archive = self.temporary / f"{artifact_name}.tar.gz"
        with archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as output:
                    paths = sorted(
                        [bundle, *bundle.rglob("*")],
                        key=lambda path: path.relative_to(bundle.parent).as_posix().encode(),
                    )
                    for path in paths:
                        relative = Path(verifier.ARCHIVE_ROOT) / path.relative_to(bundle)
                        info = tarfile.TarInfo(relative.as_posix())
                        info.uid = info.gid = 0
                        info.uname = info.gname = ""
                        info.mtime = 0
                        model_member = relative.parts[1:3] == ("components", "models")
                        if path.is_dir():
                            info.type = tarfile.DIRTYPE
                            info.mode = 0o555 if model_member else 0o755
                            output.addfile(info)
                        else:
                            info.size = path.stat().st_size
                            info.mode = 0o755 if path.name == "repo-native-alignment" else 0o444 if model_member else 0o644
                            with path.open("rb") as stream:
                                output.addfile(info, stream)
                    if hostile_member is not None:
                        info = tarfile.TarInfo(hostile_member)
                        info.mode = 0o644
                        info.type = hostile_type or tarfile.REGTYPE
                        if info.type == tarfile.REGTYPE:
                            info.size = 1
                            import io

                            output.addfile(info, io.BytesIO(b"x"))
                        else:
                            info.linkname = "repo-native-alignment"
                            output.addfile(info)

        manifest = {
            "schema": verifier.MANIFEST_SCHEMA,
            "artifact": {
                "name": artifact_name,
                "archive_file": archive.name,
                "archive_sha256": verifier.sha256_file(archive),
            },
            "provenance": {
                "repository": "open-horizon-labs/repo-native-alignment",
                "head_sha": head,
                "workflow": ".github/workflows/swebench-semantic-bundle.yml",
                "job": "build-semantic-bundle",
                "run_id": 42,
                "run_attempt": 1,
            },
            "build": {
                "target": "aarch64-apple-darwin",
                "target_cpu": "apple-m4",
                "features": ["embeddings", "metal", "swebench-semantic-bundle"],
                "profile": "release",
                "rustc": "rustc 1.97.0",
                "cargo": "cargo 1.97.0",
                "rustflags": "-C target-cpu=apple-m4",
                "link_flags": ["-Wl,-dead_strip"],
                "metal_kernel_profile": "release-fast-math",
                "candle_metal_enable_fast_math": True,
                "metal_kernel_compilation": "embedded-source-runtime",
            },
            "host": {
                "architecture": "arm64",
                "chip": "Apple M4 Max",
                "metal_device_observed": True,
                "system_profiler_sha256": verifier.sha256_file(profiler),
            },
            "components": {
                "executable": {
                    "path": "repo-native-alignment",
                    "sha256": verifier.sha256_file(executable),
                },
                "embedding": {
                    "model_id": "sentence-transformers/all-MiniLM-L6-v2",
                    "assets": {
                        name: by_encoder_name[name]
                        for name in ("config.json", "tokenizer.json", "model.safetensors")
                    },
                    "files": encoder_files,
                    "files_digest": verifier.records_digest(encoder_files),
                },
                "reranker": {
                    "model_id": "jinaai/jina-reranker-v1-turbo-en",
                    "files": reranker_files,
                    "files_digest": verifier.records_digest(reranker_files),
                },
                "lsp": {
                    "toolchain_lock_sha256": by_lsp_name["toolchain-lock.json"]["sha256"],
                    "inventory_sha256": by_lsp_name["inventory.json"]["sha256"],
                    "descriptor_inventory_sha256": by_lsp_name["descriptor-inventory.json"]["sha256"],
                    "provision_receipt_sha256": by_lsp_name["provision-receipt.json"]["sha256"],
                    "probe_receipt_sha256": by_lsp_name["probe-receipt.json"]["sha256"],
                    "files": lsp_files,
                    "files_digest": verifier.records_digest(lsp_files),
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
                "evidence_sha256": "b" * 64,
                "lsp_readiness_sha256": verifier.sha256_file(readiness),
            },
        }
        manifest_path = self.temporary / f"manifest-{len(list(self.temporary.iterdir()))}.json"
        manifest_path.write_bytes(verifier.canonical_json(manifest))
        manifest_sha = verifier.sha256_file(manifest_path)
        github_digest = "sha256:" + "c" * 64
        upload = {
            "schema": verifier.UPLOAD_SCHEMA,
            "artifact_name": manifest["artifact"]["name"],
            "artifact_id": 99,
            "artifact_url": "https://github.invalid/artifacts/99",
            "artifact_digest": github_digest,
            "manifest_sha256": manifest_sha,
            "head_sha": head,
            "run_id": 42,
            "run_attempt": 1,
        }
        upload_path = self.temporary / f"upload-{len(list(self.temporary.iterdir()))}.json"
        upload_path.write_bytes(verifier.canonical_json(upload))
        return {
            "archive": archive,
            "manifest": manifest_path,
            "upload": upload_path,
            "manifest_sha": manifest_sha,
            "upload_sha": verifier.sha256_file(upload_path),
            "github_digest": github_digest,
            "head": head,
        }

    def _verify(self, fixture: dict[str, object], suffix: str) -> dict[str, object]:
        return verifier.verify_bundle(
            archive=fixture["archive"],
            manifest_path=fixture["manifest"],
            upload_attestation_path=fixture["upload"],
            output=self.temporary / f"verified-{suffix}",
            expected_manifest_sha256=fixture["manifest_sha"],
            expected_upload_attestation_sha256=fixture["upload_sha"],
            expected_github_artifact_digest=fixture["github_digest"],
            expected_head_sha=fixture["head"],
        )

    def test_verifies_and_extracts_exact_runtime_bytes(self) -> None:
        receipt = self._verify(self._fixture(), "valid")
        self.assertEqual(receipt["schema"], verifier.RECEIPT_SCHEMA)
        model = self.temporary / "verified-valid/rna-semantic-bundle/components/models/huggingface"
        self.assertEqual(stat.S_IMODE(model.stat().st_mode), 0o555)

    def test_rejects_traversal_even_when_archive_digest_is_anchored(self) -> None:
        fixture = self._fixture(hostile_member="../escape")
        with self.assertRaisesRegex(verifier.BundleVerificationError, "unsafe"):
            self._verify(fixture, "traversal")

    def test_rejects_symlink_member(self) -> None:
        fixture = self._fixture(
            hostile_member="rna-semantic-bundle/link", hostile_type=tarfile.SYMTYPE
        )
        with self.assertRaisesRegex(verifier.BundleVerificationError, "link or special"):
            self._verify(fixture, "symlink")

    def test_rejects_hardlink_member(self) -> None:
        fixture = self._fixture(
            hostile_member="rna-semantic-bundle/hardlink",
            hostile_type=tarfile.LNKTYPE,
        )
        with self.assertRaisesRegex(verifier.BundleVerificationError, "link or special"):
            self._verify(fixture, "hardlink")

    def test_rejects_casefold_archive_member_collision(self) -> None:
        fixture = self._fixture(
            hostile_member="rna-semantic-bundle/REPO-NATIVE-ALIGNMENT"
        )
        with self.assertRaisesRegex(verifier.BundleVerificationError, "duplicate"):
            self._verify(fixture, "casefold")

    def test_rejects_upload_digest_not_bound_to_external_github_identity(self) -> None:
        fixture = self._fixture()
        upload_path = fixture["upload"]
        upload = json.loads(upload_path.read_bytes())
        upload["artifact_digest"] = "sha256:" + "d" * 64
        upload_path.write_bytes(verifier.canonical_json(upload))
        fixture["upload_sha"] = verifier.sha256_file(upload_path)
        with self.assertRaisesRegex(verifier.BundleVerificationError, "trust anchor"):
            self._verify(fixture, "upload-digest")

    def test_rejects_payload_tampering(self) -> None:
        fixture = self._fixture(tamper_payload=True)
        with self.assertRaisesRegex(verifier.BundleVerificationError, "payload file tree"):
            self._verify(fixture, "tamper")


if __name__ == "__main__":
    unittest.main()
