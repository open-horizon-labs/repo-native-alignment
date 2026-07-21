from __future__ import annotations

import copy
import hashlib
import json
import os
import tempfile
import unittest
import unittest.mock
from pathlib import Path

from scripts import swebench_context_packets as PACKETS
from scripts import validate_swebench_act_context_protocol as PROTOCOL


ROOT = Path(__file__).resolve().parents[2]


def _node_entry(stable_id: str, *, path: str = "src/example.py", line: int = 1) -> str:
    return (
        f"- **function** `value` (python) `{path}`:{line}-{line}\n"
        f"  ID: `{stable_id}`\n"
        "  Extraction source: tree_sitter\n"
    )


def _packet_manifest(root: Path) -> dict:
    source = json.loads(
        (ROOT / "benchmark/swebench-act-context/packet-vector.json").read_text()
    )
    vector = {"metadata": source["metadata"], "records": source["records"]}
    (root / "packet-vector.json").write_bytes(PACKETS.canonical_json(vector) + b"\n")
    (root / "acquisition.json").write_bytes(
        PACKETS.canonical_json(vector["metadata"]["acquisition"]) + b"\n"
    )
    packets = {}
    for arm in ("B", "C"):
        payload = PROTOCOL.assemble_packet_vector(vector, arm)
        name = f"packet-{arm}.bin"
        (root / name).write_bytes(payload)
        packets[arm] = {
            "file": name,
            "sha256": PACKETS.sha256(payload),
            "size_bytes": len(payload),
            "cl100k_payload_tokens": 1,
        }
    manifest = {
        "schema": "rna-swebench-context-packets-v1",
        "status": "ready",
        "instance_id": vector["metadata"]["instance_id"],
        "acquisition_sha256": PACKETS.sha256(
            PACKETS.canonical_json(vector["metadata"]["acquisition"])
        ),
        "packets": packets,
        "cache": {
            "injected_tree_digest_before": "a" * 64,
            "injected_tree_digest_after": "a" * 64,
        },
        "command_trace": [],
    }
    cache_receipt = {"files": 1, "digest": "a" * 64}
    for name in ("cache-before.json", "cache-after.json"):
        (root / name).write_bytes(PACKETS.canonical_json(cache_receipt) + b"\n")
    manifest["cache"].update(
        before_receipt_sha256=PACKETS.sha256_file(root / "cache-before.json"),
        after_receipt_sha256=PACKETS.sha256_file(root / "cache-after.json"),
    )
    evidence = root / "command-evidence"
    evidence.mkdir(exist_ok=True)
    commands = (
        ("fresh-reopen-readiness", ["rna", "lsp-readiness", "--json"], b'{"ready":true}'),
        ("semantic-search", ["rna", "search"], PACKETS.STRICT_SENTINEL.encode()),
    )
    for ordinal, (name, argv, stdout) in enumerate(commands, 1):
        result = PACKETS.CommandResult(name, tuple(argv), 0, stdout, b"", 1)
        receipt = result.receipt()
        prefix = evidence / f"{ordinal:04d}-{name}"
        prefix.with_suffix(".stdout").write_bytes(stdout)
        prefix.with_suffix(".stderr").write_bytes(b"")
        prefix.with_suffix(".json").write_bytes(PACKETS.canonical_json(receipt) + b"\n")
        manifest["command_trace"].append(receipt)
    manifest["content_digest"] = PACKETS.sha256(PACKETS.canonical_json(manifest))
    (root / "manifest.json").write_bytes(PACKETS.canonical_json(manifest) + b"\n")
    return manifest


class FakeRecorder:
    def __init__(self, stdout: bytes) -> None:
        self.stdout = stdout
        self.calls: list[tuple[str, tuple[str, ...]]] = []

    def run(self, name, argv, _cwd):
        self.calls.append((name, tuple(argv)))
        return PACKETS.CommandResult(name, tuple(argv), 0, self.stdout, b"", 1)


class SwebenchContextPacketTests(unittest.TestCase):
    def test_source_slice_preserves_exact_line_endings(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "sample.py"
            path.write_bytes(b"first\r\nsecond\r\nthird")
            self.assertEqual(
                PACKETS.source_slice(root, "sample.py", 1, 2),
                "first\r\nsecond\r\n",
            )
            terminal = root / "terminal.py"
            terminal.write_bytes(b"first\n")
            self.assertEqual(PACKETS.source_slice(root, "terminal.py", 1, 2), "first\n")

    def test_bound_edge_label_projection_and_unknowns(self) -> None:
        expected = {
            "calls": "Calls",
            "referenced_by": "ReferencedBy",
            "depends_on": "DependsOn",
            "implements": "Implements",
            "defines": "Defines",
            "has_field": "Contains",
            "belongs_to": "Contains",
            "contains": "Contains",
            "tested_by": "Tests",
            "tests": "Tests",
            "imports": "Imports",
            "extends": "Extends",
            "references": "Other",
            "re_exports": "Other",
            "repo.custom/value": "Other",
        }
        for raw, projected in expected.items():
            with self.subTest(raw=raw):
                self.assertEqual(PACKETS.project_edge_label(raw), projected)

        headings = {
            "Calls": "calls",
            "Referenced by": "referenced_by",
            "Has field": "has_field",
            "Repo.custom/value": "repo.custom/value",
        }
        for heading, raw in headings.items():
            with self.subTest(heading=heading):
                self.assertEqual(PACKETS.raw_label_from_heading(heading), raw)
        for invalid in ("", " Calls", "Calls ", "Calls (forged)"):
            with self.subTest(invalid=invalid):
                with self.assertRaises(PACKETS.PacketError):
                    PACKETS.raw_label_from_heading(invalid)

    def test_strict_result_parser_preserves_cli_order_and_fails_closed(self) -> None:
        output = (
            "## Search: exact bytes\n\n"
            "### Code symbols (2 result(s))\n\n"
            + _node_entry("src/z.py:z:function", path="src/z.py", line=9)
            + _node_entry("docs/readme.md:Intro:markdown_section", path="docs/readme.md", line=2)
        ).encode()
        nodes = PACKETS.parse_nodes(output)
        self.assertEqual(
            [node.stable_id for node in nodes],
            ["src/z.py:z:function", "docs/readme.md:Intro:markdown_section"],
        )
        with self.assertRaisesRegex(PACKETS.PacketError, "parsed 1 of 2"):
            PACKETS.parse_nodes(
                b"### Code symbols (2 result(s))\n" + _node_entry("only", line=1).encode()
            )
        duplicate = (
            b"### Code symbols (2 result(s))\n"
            + _node_entry("same", line=1).encode()
            + _node_entry("same", line=2).encode()
        )
        with self.assertRaisesRegex(PACKETS.PacketError, "duplicate stable IDs"):
            PACKETS.parse_nodes(duplicate)
        with self.assertRaises(UnicodeDecodeError):
            PACKETS.parse_nodes(b"\xff")

        multiline = (
            "### Code symbols (1 result(s))\n\n"
            "- **const** `first line\nsecond line` `src/example.py`:4-5 src:tree_sitter\n"
            "  `src/example.py:first line\nsecond line:const`\n\n"
            "*Index: sealed*\n"
        ).encode()
        self.assertEqual(
            PACKETS.parse_nodes(multiline)[0].stable_id,
            "src/example.py:first line\nsecond line:const",
        )

    def test_neighbors_capture_raw_labels_unresolved_and_cap_first_fifty(self) -> None:
        entries = [_node_entry(f"src/n{i}.py:n{i}:function", line=i) for i in range(1, 56)]
        unresolved = (
            "- **function** `missing` (python) `src/missing.py`:99-99\n"
            "  Extraction source: lsp\n"
        )
        output = (
            "## Graph neighbors (incoming)\n\n"
            "#### Referenced by (56)\n\n"
            + unresolved
            + "".join(entries)
        ).encode()
        parsed, invalid = PACKETS.parse_neighbors(output)
        self.assertEqual(len(parsed), 55)
        self.assertTrue(all(raw == "referenced_by" for raw, _, _ in parsed))
        self.assertEqual([ordinal for _, _, ordinal in parsed], list(range(2, 57)))
        self.assertEqual(invalid[0]["reason"], "missing_stable_id")

        recorder = FakeRecorder(output)
        loci = [{"ordinal": 1, "seed_stable_ids": ["src/seed.py:seed:function"]}]
        relationships, evidence, traversal_nodes = PACKETS.traverse(
            loci, Path("/checkout"), Path("/bundle/rna"), recorder
        )
        self.assertEqual(len(recorder.calls), 2)
        self.assertEqual(len(relationships), 100)
        incoming = [item for item in relationships if item["direction"] == "incoming"]
        self.assertEqual(len(incoming), 50)
        self.assertEqual(incoming[0]["cli_ordinal"], 2)
        self.assertEqual(incoming[-1]["cli_ordinal"], 51)
        self.assertEqual(len(evidence[0]["valid_stream"]), 55)
        self.assertEqual(evidence[0]["valid_stream"][50]["relationship"]["cli_ordinal"], 52)
        self.assertEqual(evidence[0]["invalid_entries"][0]["raw_label"], "referenced_by")
        self.assertEqual(len(traversal_nodes), 55)
        self.assertNotIn("--limit", recorder.calls[0][1])

    def test_candidate_pool_includes_unique_traversal_only_nodes(self) -> None:
        candidate = PACKETS.ParsedNode("src/c.py:c:function", "function", "", 0, 0)
        seed = "src/s.py:s:function"
        relationships = [
            {
                "source": candidate.stable_id,
                "target": seed,
                "edge_type": "Calls",
                "direction": "incoming",
                "locus_ordinal": 1,
                "cli_ordinal": ordinal,
            }
            for ordinal in (1, 2)
        ]
        candidates, _, _ = PACKETS.candidate_pool(
            [],
            [candidate],
            relationships,
            [{"ordinal": 1, "seed_stable_ids": [seed], "path": "src/s.py", "start_line": 1, "end_line": 1}],
            Path("/checkout"),
            Path("/bundle/rna"),
            FakeRecorder(b""),
        )
        self.assertEqual([item["stable_id"] for item in candidates], [candidate.stable_id])
        self.assertEqual(candidates[0]["eligibility"], "not_source_backed")
        self.assertIs(candidates[0]["selected"], False)

    def test_ordinary_docs_and_history_paths_remain_eligible(self) -> None:
        for path, tag, language in (
            ("README.md", "markdown", "Markdown"),
            ("docs/guide.rst", "rst", "reStructuredText"),
            ("history/implementation.py", "python", "Python"),
        ):
            with self.subTest(path=path):
                node = PACKETS.ParsedNode(f"{path}:section:function", "function", path, 1, 1)
                output = (
                    f"### Code symbols (1 result(s))\n\n"
                    f"- **function** `section` `{path}`:1-1 src:tree_sitter\n"
                    f"  `{node.stable_id}`\n"
                    f"  ```{tag}\npayload\n  ```\n"
                ).encode()
                candidate, full, minified, _ = PACKETS.retrieve_candidate(
                    node, Path("/checkout"), Path("/bundle/rna"), FakeRecorder(output), 1
                )
                self.assertEqual(candidate["language"], language)
                self.assertFalse(candidate["eligibility_evidence"]["excluded_record_class"])
                self.assertEqual((full, minified), ("payload", "payload"))

    def test_dynamic_fence_body_parser_binds_id_and_payload(self) -> None:
        stable_id = "src/example.py:value:function"
        output = (
            f"- **function** `value` (python) `src/example.py`:1-3\n"
            f"  ID: `{stable_id}`\n"
            "  ````python\n"
            "def value():\n"
            "    return '``` is body text'\n"
            "  ````\n"
            "\n*Index: sealed*\n"
        ).encode()
        tag, body = PACKETS.parse_body(output, stable_id)
        self.assertEqual(tag, "python")
        self.assertEqual(body, "def value():\n    return '``` is body text'")
        with self.assertRaisesRegex(PACKETS.PacketError, "does not bind"):
            PACKETS.parse_body(output, "different:id")
        with self.assertRaisesRegex(PACKETS.PacketError, "unterminated"):
            PACKETS.parse_body(output.replace(b"  ````\n\n*Index", b"\n*Index"), stable_id)
        forged = output.replace(b"\n*Index", b"noise\n  ````\n\n*Index")
        with self.assertRaisesRegex(PACKETS.PacketError, "multiple fenced"):
            PACKETS.parse_body(forged, stable_id)

    def test_exact_upstream_source_and_isolated_functions(self) -> None:
        source = PACKETS.UPSTREAM_ARMS.read_bytes()
        self.assertEqual(len(source), 15118)
        self.assertEqual(hashlib.sha256(source).hexdigest(), PACKETS.UPSTREAM_ARMS_SHA256)
        upstream = PACKETS.load_upstream_locus_functions()
        patch = (
            "diff --git a/pkg/m.py b/pkg/m.py\n"
            "--- a/pkg/m.py\n+++ b/pkg/m.py\n"
            "@@ -2,2 +2,2 @@\n-old = 1\n+old = 2\n context = 3"
        )
        self.assertEqual(upstream.gold_edited_lines(patch), {"pkg/m.py": {2}})
        self.assertEqual(upstream.hunk_preimages(patch)[0]["requirements"], ["old = 1"])
        self.assertTrue(upstream.coverage_report("old = 1", upstream.hunk_preimages(patch))["full_coverage"])
        segments = upstream.module_preamble_segments(
            "import os\n\ndef f():\n    return 1\n\nVALUE = 2"
        )
        self.assertEqual(segments, [(1, 2, "import os\n"), (5, 6, "\nVALUE = 2")])

    def test_dataset_row_binds_complete_row_and_all_frozen_digests(self) -> None:
        row = {
            "instance_id": "owner__repo-1",
            "repo": "owner/repo",
            "base_commit": "1" * 40,
            "problem_statement": "exact issue / bytes\n",
            "patch": "diff --git a/a.py b/a.py\n",
            "test_patch": "tests",
            "extra_dataset_field": [1, "two"],
        }
        frozen = {
            "instance_id": row["instance_id"],
            "repo": row["repo"],
            "base_commit": row["base_commit"],
            "dataset_row_sha256": PACKETS.sha256(PACKETS.canonical_json(row)),
            "problem_statement_sha256": PACKETS.sha256(row["problem_statement"].encode()),
            "gold_patch_sha256": PACKETS.sha256(row["patch"].encode()),
            "test_patch_sha256": PACKETS.sha256(row["test_patch"].encode()),
            "included": True,
        }
        self.assertIs(PACKETS.validate_dataset_row(row, {"instances": [frozen]}), frozen)
        drifted = copy.deepcopy(row)
        drifted["extra_dataset_field"].append(3)
        with self.assertRaisesRegex(PACKETS.PacketError, "dataset_row_sha256"):
            PACKETS.validate_dataset_row(drifted, {"instances": [frozen]})
        drifted = copy.deepcopy(row)
        drifted["problem_statement"] += " "
        with self.assertRaises(PACKETS.PacketError):
            PACKETS.validate_dataset_row(drifted, {"instances": [frozen]})

    def test_checkout_rejects_untracked_markdown_before_packet_queries(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            PACKETS.git(checkout, "init", "--quiet")
            PACKETS.git(checkout, "config", "user.email", "packet-test@example.invalid")
            PACKETS.git(checkout, "config", "user.name", "Packet Test")
            (checkout / "source.py").write_text("VALUE = 1\n", encoding="utf-8")
            PACKETS.git(checkout, "add", "source.py")
            PACKETS.git(checkout, "commit", "--quiet", "-m", "fixture")
            head = PACKETS.git(checkout, "rev-parse", "HEAD")

            (checkout / "README.md").write_text(
                "# Untracked query-visible context\n", encoding="utf-8"
            )

            with self.assertRaisesRegex(PACKETS.PacketError, "untracked files"):
                PACKETS.validate_checkout(
                    checkout,
                    {"base_commit": head, "patch": ""},
                )

    def test_vector_uses_one_selection_and_verbatim_loci_for_b_and_c(self) -> None:
        source = json.loads(
            (ROOT / "benchmark/swebench-act-context/packet-vector.json").read_text()
        )
        vector = {"metadata": source["metadata"], "records": source["records"]}
        PACKETS.verify_vector(vector)
        selected = [
            candidate["stable_id"]
            for candidate in vector["metadata"]["acquisition"]["candidates"]
            if candidate["selected"]
        ]
        records = [record for record in vector["records"] if record["kind"] == "candidate"]
        self.assertEqual(selected, [record["header"]["stable_id"] for record in records])
        for record in vector["records"]:
            if record["kind"] == "locus":
                self.assertEqual(record["full_payload"], record["minified_payload"])
        packet_b = PROTOCOL.assemble_packet_vector(vector, "B")
        packet_c = PROTOCOL.assemble_packet_vector(vector, "C")
        self.assertNotEqual(packet_b, packet_c)
        self.assertIn(PACKETS.canonical_json(vector["metadata"]), packet_b)
        self.assertIn(PACKETS.canonical_json(vector["metadata"]), packet_c)
        for locus in (record for record in vector["records"] if record["kind"] == "locus"):
            self.assertIn(locus["full_payload"].encode(), packet_b)
            self.assertIn(locus["full_payload"].encode(), packet_c)

    def test_realistic_markdown_cli_results_flow_into_selection_and_packets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            readme = checkout / "README.md"
            nested = checkout / "docs/configuration.rst"
            readme_body = "# Project\n\nUse the deterministic packet builder."
            nested_body = "Configuration\n=============\n\nSet offline mode."
            readme.write_text(readme_body, encoding="utf-8")
            nested.parent.mkdir()
            nested.write_text(nested_body, encoding="utf-8")
            stdout = (
                "## Search: \"deterministic offline configuration\"\n\n"
                "### Strict semantic qualification\n\n"
                f"`{PACKETS.STRICT_SENTINEL}`\n\n"
                "### Markdown (2 result(s))\n\n"
                f"- (score: 0.91) `{readme}` > # Project\n\n{readme_body}\n\n"
                "---\n\n"
                f"- (score: 0.74) `{nested}` > Configuration\n\n{nested_body}"
                "\n*Index: sealed*\n"
            ).encode()
            recorder = FakeRecorder(stdout)
            semantic_nodes, _ = PACKETS.semantic_search(
                "deterministic offline configuration",
                checkout,
                Path("/bundle/rna"),
                recorder,
            )
            self.assertEqual(
                [(node.path, node.kind) for node in semantic_nodes],
                [
                    ("README.md", "markdown_section"),
                    ("docs/configuration.rst", "markdown_section"),
                ],
            )
            locus, locus_record = PACKETS.make_locus(
                1, "module_preamble", "src/edit.py", 1, 1, "Python", "EDIT = 1", []
            )
            candidates, payloads, evidence = PACKETS.candidate_pool(
                semantic_nodes,
                [],
                [],
                [locus],
                checkout,
                Path("/bundle/rna"),
                FakeRecorder(b""),
            )
            self.assertEqual([candidate["selected"] for candidate in candidates], [True, True])
            self.assertTrue(
                all(item.get("inline_markdown") for item in evidence[: len(semantic_nodes)])
            )
            acquisition = {
                "schema_version": 1,
                "dataset_row_sha256": "a" * 64,
                "query_sha256": "b" * 64,
                "rna_artifact_receipt_sha256": "c" * 64,
                "loci": [locus],
                "candidates": candidates,
                "relationships": [],
                "omissions": evidence[-1]["omissions"],
            }
            records = [
                locus_record,
                *PACKETS.candidate_records(candidates, [], payloads, 1),
            ]
            vector = {
                "metadata": {
                    "instance_id": "owner__repo-1",
                    "protocol_id": "rna-act-context-swebench-v1",
                    "record_count": len(records),
                    "acquisition": acquisition,
                },
                "records": records,
            }
            PACKETS.verify_vector(vector)
            packet_b = PROTOCOL.assemble_packet_vector(vector, "B")
            packet_c = PROTOCOL.assemble_packet_vector(vector, "C")
            for body in (readme_body, nested_body):
                self.assertIn(body.encode(), packet_b)
                self.assertIn(body.encode(), packet_c)

    def test_semantic_search_keeps_docs_after_twenty_code_results(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            readme = checkout / "README.md"
            guide = checkout / "docs/guide.rst"
            readme_body = "# Project\n\nRepository overview."
            guide_body = "Guide\n=====\n\nRepository guide."
            readme.write_text(readme_body, encoding="utf-8")
            guide.parent.mkdir()
            guide.write_text(guide_body, encoding="utf-8")
            code = "\n".join(
                _node_entry(
                    f"src/module_{ordinal}.py:value:function",
                    path=f"src/module_{ordinal}.py",
                    line=ordinal,
                )
                for ordinal in range(1, 21)
            )
            stdout = (
                "## Search: \"repository behavior\"\n\n"
                "### Code symbols (20 result(s))\n\n"
                f"{code}\n"
                "### Strict semantic qualification\n\n"
                f"`{PACKETS.STRICT_SENTINEL}`\n\n"
                "### Markdown (2 result(s))\n\n"
                f"- (score: 0.91) `{readme}` > # Project\n\n{readme_body}\n\n"
                "---\n\n"
                f"- (score: 0.74) `{guide}` > Guide\n\n{guide_body}"
                "\n*Index: sealed*\n"
            ).encode()

            semantic_nodes, _ = PACKETS.semantic_search(
                "repository behavior",
                checkout,
                Path("/bundle/rna"),
                FakeRecorder(stdout),
            )

            self.assertEqual(len(semantic_nodes), 22)
            self.assertEqual(
                [node.stable_id for node in semantic_nodes[:20]],
                [f"src/module_{ordinal}.py:value:function" for ordinal in range(1, 21)],
            )
            self.assertEqual(
                [(node.path, node.kind) for node in semantic_nodes[20:]],
                [
                    ("README.md", "markdown_section"),
                    ("docs/guide.rst", "markdown_section"),
                ],
            )

    def test_vector_rejects_payload_and_closed_schema_tamper(self) -> None:
        source = json.loads(
            (ROOT / "benchmark/swebench-act-context/packet-vector.json").read_text()
        )
        vector = {"metadata": source["metadata"], "records": source["records"]}
        candidate = next(record for record in vector["records"] if record["kind"] == "candidate")
        candidate["full_payload"] += "\nforged"
        with self.assertRaisesRegex(PACKETS.PacketError, "full payload binding"):
            PACKETS.verify_vector(vector)

        vector = {"metadata": copy.deepcopy(source["metadata"]), "records": source["records"]}
        vector["metadata"]["unexpected"] = True
        with self.assertRaisesRegex(PACKETS.PacketError, "metadata field set"):
            PACKETS.verify_vector(vector)

    def test_locus_expressibility_cannot_cross_record_boundaries(self) -> None:
        row = {
            "patch": (
                "diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n"
                "@@ -1,2 +1,2 @@\n-alpha\n-beta\n+changed\n+lines"
            )
        }
        loci = [
            {"full_payload": "alpha"},
            {"full_payload": "beta"},
        ]
        with self.assertRaisesRegex(PACKETS.PacketError, "editable loci"):
            PACKETS.expressibility(row, loci, b"alpha\nbeta", b"alpha\nbeta")

    def test_python_module_preamble_preserves_crlf_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            (checkout / "module.py").write_bytes(b"VALUE = 1\r\nOTHER = 2\r\n")
            row = {
                "patch": (
                    "diff --git a/module.py b/module.py\n--- a/module.py\n+++ b/module.py\n"
                    "@@ -1 +1 @@\n-VALUE = 1\n+VALUE = 3"
                )
            }
            recorder = FakeRecorder(b"### Code symbols (0 result(s))\n")
            _, records, _ = PACKETS.derive_loci(
                row, checkout, Path("/bundle/rna"), recorder
            )
            preamble = next(
                record for record in records if record["header"]["source_kind"] == "module_preamble"
            )
            self.assertEqual(preamble["full_payload"], "VALUE = 1\r\nOTHER = 2\r\n")

    def test_frozen_inputs_use_exact_lock_members_without_sibling_inventory(self) -> None:
        expected_digest = (
            ROOT / "benchmark/swebench-act-context/protocol.sha256"
        ).read_text(encoding="ascii").strip()
        self.assertEqual(PACKETS.validate_frozen_lock(expected_digest), expected_digest)
        protocol = {"protocol_id": "rna-act-context-swebench-v1"}
        population = {"instances": []}
        with (
            unittest.mock.patch.object(PACKETS, "validate_frozen_lock") as validate,
            unittest.mock.patch.object(
                PACKETS.PROTOCOL,
                "load_json_object",
                side_effect=(protocol, population),
            ),
            unittest.mock.patch.object(PACKETS.PROTOCOL, "validate_protocol"),
            unittest.mock.patch.object(PACKETS.PROTOCOL, "validate_population"),
            unittest.mock.patch.object(PACKETS, "sha256_file", side_effect=(
                PACKETS.PROTOCOL.EXPECTED_PROTOCOL_SHA256,
                PACKETS.PROTOCOL.EXPECTED_POPULATION_SHA256,
            )),
        ):
            self.assertEqual(PACKETS.validate_frozen_inputs(expected_digest), (protocol, population))
        validate.assert_called_once_with(expected_digest)

    def test_command_protocol_rejects_non_exact_search_argv(self) -> None:
        source = json.loads(
            (ROOT / "benchmark/swebench-act-context/packet-vector.json").read_text()
        )
        vector = {"metadata": source["metadata"], "records": source["records"]}
        trace = [
            {
                "name": "fresh-reopen-readiness",
                "argv": ["rna", "--business-context", "disabled", "lsp-readiness", "--repo", "/repo", "--json"],
            },
            {"name": "semantic-search", "argv": ["rna", "search", "wrong query"]},
        ]
        with self.assertRaises(PACKETS.PacketError):
            PACKETS.validate_command_protocol(
                trace,
                Path("/evidence"),
                vector,
                {"locus_queries": [], "traversal": [], "candidate_bodies": []},
                {"patch": "", "problem_statement": "exact issue"},
            )

    @unittest.mock.patch.object(PACKETS, "packet_tokens", return_value=1)
    def test_offline_verifier_rejects_packet_manifest_and_trace_tamper(
        self, _packet_tokens: unittest.mock.Mock
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _packet_manifest(root)
            self.assertEqual(PACKETS.verify_output(root)["status"], "ready")

            packet_b = root / "packet-B.bin"
            packet_b.write_bytes(packet_b.read_bytes() + b"tamper")
            with self.assertRaisesRegex(PACKETS.PacketError, "packet B"):
                PACKETS.verify_output(root)

            _packet_manifest(root)
            (root / "command-evidence/0002-semantic-search.stdout").unlink()
            with self.assertRaises((PACKETS.PacketError, OSError)):
                PACKETS.verify_output(root)

            _packet_manifest(root)
            manifest = json.loads((root / "manifest.json").read_text())
            manifest["command_trace"] = [{"argv": ["rna", "scan"]}]
            projected = dict(manifest)
            projected.pop("content_digest")
            manifest["content_digest"] = PACKETS.sha256(PACKETS.canonical_json(projected))
            (root / "manifest.json").write_bytes(PACKETS.canonical_json(manifest) + b"\n")
            with self.assertRaisesRegex(PACKETS.PacketError, "command"):
                PACKETS.verify_output(root)

    def test_no_spend_guards_reject_forbidden_commands_and_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            recorder = PACKETS.CommandRecorder(Path(temporary) / "evidence", {})
            with self.assertRaisesRegex(PACKETS.PacketError, "forbidden enrichment"):
                recorder.run("bad", ["rna", "scan", "--full"], Path(temporary))
            self.assertEqual(recorder.results, [])
        with unittest.mock.patch.dict(os.environ, {"ANTHROPIC_API_KEY": "redacted"}, clear=False):
            with self.assertRaisesRegex(PACKETS.PacketError, "rejects ANTHROPIC_API_KEY"):
                PACKETS.materialize_command_environment(
                    Path("/does/not/matter/rna"),
                    Path("/does/not/matter/output"),
                    {},
                )


if __name__ == "__main__":
    unittest.main()
