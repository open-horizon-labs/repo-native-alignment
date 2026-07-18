#!/usr/bin/env python3
"""Focused tests for the deterministic capability-server fixture."""

import importlib.util
from pathlib import Path
import re
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
FIXTURE_ROOT = REPO_ROOT / "tests/fixtures/lsp_capability_repo"
SERVER_PATH = REPO_ROOT / "tests/fixtures/lsp_capability_server.py"
SPEC = importlib.util.spec_from_file_location("lsp_capability_server", SERVER_PATH)
SERVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SERVER)


def utf16_units(text):
    return len(text.encode("utf-16-le")) // 2


class DocumentLinkResultTest(unittest.TestCase):
    def test_range_matches_requested_readme_and_guide(self):
        cases = (
            ("README.md", r"\[the source\]\(src/app\.py\)"),
            ("docs/guide.md", r"\[the source\]\(\.\./src/app\.py\)"),
        )
        for relative_path, pattern in cases:
            with self.subTest(path=relative_path):
                path = FIXTURE_ROOT / relative_path
                response = SERVER.document_link_result(path.as_uri())
                self.assertEqual(len(response), 1)
                link_range = response[0]["range"]
                line = path.read_text(encoding="utf-8").splitlines()[
                    link_range["start"]["line"]
                ]
                match = re.search(pattern, line)
                self.assertIsNotNone(match)
                self.assertEqual(
                    link_range["start"]["character"], utf16_units(line[: match.start()])
                )
                self.assertEqual(
                    link_range["end"]["character"], utf16_units(line[: match.end()])
                )


if __name__ == "__main__":
    unittest.main()
