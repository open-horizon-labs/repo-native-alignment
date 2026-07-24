from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import live_identity
import provider_usage


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def ref(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": sha(data)}


class IdentityFixture:
    def __init__(self, root: Path):
        self.root = root
        self.repo = root / "index"
        self.cache = self.repo / ".oh/.cache"
        self.cache.mkdir(parents=True)
        (self.cache / "graph.bin").write_bytes(b"cache")
        (self.repo / ".gitignore").write_text(".oh/.cache/\n")
        (self.repo / "tracked.py").write_text("value = 1\n")
        subprocess.run(["git", "-C", str(self.repo), "init", "-q"], check=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(self.repo),
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repository.git",
            ],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.name", "Fixture"],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(self.repo),
                "config",
                "user.email",
                "fixture@example.invalid",
            ],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "add", ".gitignore", "tracked.py"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "fixture"], check=True
        )
        self.commit = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.decode().strip()
        self.tree = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD^{tree}"],
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.decode().strip()
        self.inventory = live_identity.cache_inventory_sha256(self.cache)
        git_binary = shutil.which("git")
        if git_binary is None:
            raise RuntimeError("test requires Git")
        self.git_binary = Path(git_binary).resolve(strict=True)

        self.launcher = root / "launcher"
        self.launcher.write_bytes(b"launcher")
        self.binary = root / "binary"
        self.binary.write_bytes(b"binary")
        self.environment = root / "canonical-environment.json"
        self.environment.write_bytes(canonical({"PATH": "/usr/bin:/bin"}))
        self.archive = root / "cache.tar.gz"
        self.archive.write_bytes(b"archive")
        self.manifest = root / "cache.manifest.json"
        self.manifest.write_bytes(
            canonical(
                {
                    "schema_version": "fixture-manifest-v1",
                    "operational_cache_inventory_sha256": self.inventory,
                }
            )
        )
        self.verification = root / "cache.verification.json"
        self.verification.write_bytes(
            canonical(
                {
                    "schema_version": "fixture-verification-v1",
                    "verified": True,
                    "operational_cache_inventory_sha256": self.inventory,
                }
            )
        )
        self.readiness = root / "readiness.json"
        self.readiness.write_bytes(
            canonical(
                {
                    "status": "READY",
                    "readiness": {
                        "ready": True,
                        "compatibility_violations": [],
                        "report_digest": "a" * 64,
                    },
                }
            )
        )
        self.state = root / "evidence/state.json"
        self.identity_path = root / "identity.json"
        self.identity = {
            "schema_version": "issue827-runtime-identity-v1",
            "case_id": "owner__repository-1",
            "base_commit": self.commit,
            "base_tree": self.tree,
            "root": "fixture-root",
            "index_checkout": str(self.repo),
            "expected_repository_identity": "owner/repository",
            "live_repository_identity": "owner/repository",
            "producer_commit": "b" * 40,
            "launcher_path": str(self.launcher),
            "launcher_sha256": sha(self.launcher.read_bytes()),
            "binary_path": str(self.binary),
            "binary_sha256": sha(self.binary.read_bytes()),
            "canonical_environment": ref(self.environment),
            "canonical_environment_sha256": sha(self.environment.read_bytes()),
            "cache_archive": ref(self.archive),
            "cache_archive_sha256": sha(self.archive.read_bytes()),
            "cache_manifest": ref(self.manifest),
            "cache_manifest_sha256": sha(self.manifest.read_bytes()),
            "operational_cache_inventory_sha256": self.inventory,
            "cache_verification_receipt": ref(self.verification),
            "readiness_report": ref(self.readiness),
            "cache_bindings_verified": True,
            "fresh_reopen_ready": True,
            "readiness_sentinel": live_identity.READY,
        }
        self.identity_path.write_bytes(canonical(self.identity))
        self.config = {
            "schema_version": "issue827-supervisor-config-v4",
            "state": str(self.state),
            "identity_receipt": str(self.identity_path),
            "expected_identity_sha256": sha(self.identity_path.read_bytes()),
            "expected_identity_schema": "issue827-runtime-identity-v1",
            "repo": str(self.repo),
            "launcher": str(self.launcher),
            "binary": str(self.binary),
            "trusted_rna_environment": str(self.environment),
            "root": "fixture-root",
            "expected_repository_identity": "owner/repository",
            "expected_base_commit": self.commit,
            "expected_base_tree": self.tree,
            "expected_producer_commit": "b" * 40,
            "expected_cache_manifest_sha256": sha(self.manifest.read_bytes()),
            "expected_cache_archive_sha256": sha(self.archive.read_bytes()),
            "expected_cache_inventory_sha256": self.inventory,
            "expected_launcher_sha256": sha(self.launcher.read_bytes()),
            "expected_binary_sha256": sha(self.binary.read_bytes()),
            "expected_canonical_environment_sha256": sha(
                self.environment.read_bytes()
            ),
            "git_binary": str(self.git_binary),
            "git_binary_sha256": sha(self.git_binary.read_bytes()),
        }

    def verifier(self) -> live_identity.LiveIdentityVerifier:
        return live_identity.LiveIdentityVerifier(self.config, self.state)


class ProjectionAuthorizationTests(unittest.TestCase):
    def test_only_final_projection_bytes_authorize_ids_and_bind_hash(self):
        raw = (
            b"`pkg.py:visible:function`\n"
            b"### Strict semantic qualification\n"
            b"`pkg.py:raw-only:function`\n"
        )
        final = raw.split(b"### Strict semantic qualification", 1)[0]
        authorization = live_identity.derive_projection_authorization(final)
        self.assertEqual(
            authorization["stable_code_ids"], ["pkg.py:visible:function"]
        )
        self.assertNotIn(
            "pkg.py:raw-only:function", authorization["stable_code_ids"]
        )
        self.assertEqual(authorization["projection_sha256"], sha(final))
        changed = live_identity.derive_projection_authorization(final + b"\n")
        self.assertNotEqual(
            authorization["projection_sha256"], changed["projection_sha256"]
        )
        self.assertNotEqual(sha(canonical(authorization)), sha(canonical(changed)))


class RegistrationUsageContractTests(unittest.TestCase):
    def test_registration_matches_nullable_reasoning_contract(self):
        registration = json.loads((HERE / "registration.template.json").read_bytes())
        usage = registration["usage"]
        self.assertEqual(
            usage["required_provider_fields"],
            list(provider_usage.REQUIRED_TOKEN_FIELDS),
        )
        self.assertEqual(usage["optional_provider_fields"], ["reasoning_tokens"])
        self.assertTrue(
            usage["unavailable_optional_fields_recorded_as_null_and_unobserved"]
        )
        self.assertTrue(usage["whole_invocation_model_usage_authoritative"])
        self.assertTrue(usage["top_level_usage_retained_separately"])
        self.assertTrue(usage["reasoning_tokens_excluded_from_provider_total"])


class LiveIdentityTests(unittest.TestCase):
    def test_only_exact_issue827_supervisor_schema_is_accepted(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = IdentityFixture(Path(tmp))
            fixture.verifier().verify("query:before")

        for legacy in ("rna-supervisor-config-v3", "rna-supervisor-config-v4"):
            with self.subTest(schema=legacy), tempfile.TemporaryDirectory() as tmp:
                fixture = IdentityFixture(Path(tmp))
                fixture.config["schema_version"] = legacy
                with self.assertRaisesRegex(
                    live_identity.LiveIdentityError, "supervisor_config_schema"
                ):
                    fixture.verifier().verify("query:before")
                self.assertTrue(json.loads(fixture.state.read_bytes())["fatal"])

    def test_before_and_after_revalidate_complete_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = IdentityFixture(Path(tmp))
            before = fixture.verifier().verify("query:before")
            after = fixture.verifier().verify("query:after")
            self.assertEqual(
                before["live_state_sha256"], after["live_state_sha256"]
            )
            self.assertEqual(before["base_commit"], fixture.commit)
            self.assertEqual(
                before["operational_cache_inventory_sha256"], fixture.inventory
            )
            self.assertEqual(
                before["readiness_report_digest"], "a" * 64
            )
            self.assertEqual(before["cache_archive_sha256"], sha(b"archive"))
            self.assertEqual(
                before["git_binary_sha256"],
                sha(fixture.git_binary.read_bytes()),
            )

    def test_absolute_pinned_git_works_when_path_has_no_git(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = IdentityFixture(Path(tmp))
            with mock.patch.dict(
                os.environ,
                {"PATH": str(Path(tmp) / "path-without-git")},
                clear=False,
            ):
                receipt = fixture.verifier().verify("query:before")
            self.assertEqual(
                receipt["git_binary_path"], str(fixture.git_binary)
            )

    def test_git_identity_missing_relative_symlink_or_digest_drift_fails_closed(
        self,
    ):
        def missing(fixture: IdentityFixture) -> None:
            fixture.config.pop("git_binary")

        def relative(fixture: IdentityFixture) -> None:
            fixture.config["git_binary"] = "git"

        def symlink(fixture: IdentityFixture) -> None:
            link = fixture.root / "git-link"
            link.symlink_to(fixture.git_binary)
            fixture.config["git_binary"] = str(link)

        def digest(fixture: IdentityFixture) -> None:
            fixture.config["git_binary_sha256"] = "0" * 64

        def replaced(fixture: IdentityFixture) -> None:
            copied = fixture.root / "git-copy"
            shutil.copyfile(fixture.git_binary, copied)
            fixture.config["git_binary"] = str(copied)
            fixture.config["git_binary_sha256"] = sha(copied.read_bytes())
            copied.write_bytes(copied.read_bytes() + b"replaced")

        for label, mutate in {
            "missing": missing,
            "relative": relative,
            "symlink": symlink,
            "digest": digest,
            "replaced": replaced,
        }.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                fixture = IdentityFixture(Path(tmp))
                mutate(fixture)
                with self.assertRaises(live_identity.LiveIdentityError):
                    fixture.verifier().verify(f"{label}:before")
                self.assertTrue(
                    json.loads(fixture.state.read_bytes())["fatal"]
                )

    def test_untracked_or_ignored_material_outside_cache_fails_closed(self):
        def untracked(fixture: IdentityFixture) -> None:
            (fixture.repo / "outside.tmp").write_bytes(b"outside")

        def ignored(fixture: IdentityFixture) -> None:
            (fixture.repo / ".git/info/exclude").write_text("outside.tmp\n")
            (fixture.repo / "outside.tmp").write_bytes(b"outside")

        for label, mutate in {
            "untracked": untracked,
            "ignored": ignored,
        }.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                fixture = IdentityFixture(Path(tmp))
                mutate(fixture)
                with self.assertRaisesRegex(
                    live_identity.LiveIdentityError,
                    "live_checkout_material_outside_cache",
                ):
                    fixture.verifier().verify(f"{label}:before")

    def test_cache_drift_is_fatal_even_after_bytes_are_restored(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = IdentityFixture(Path(tmp))
            fixture.verifier().verify("traverse:before")
            (fixture.cache / "graph.bin").write_bytes(b"tampered")
            with self.assertRaisesRegex(
                live_identity.LiveIdentityError, "live_cache_inventory"
            ):
                fixture.verifier().verify("traverse:after")
            state = json.loads(fixture.state.read_bytes())
            self.assertTrue(state["fatal"])
            self.assertEqual(state["fatal_entry_point"], "traverse:after")
            (fixture.cache / "graph.bin").write_bytes(b"cache")
            with self.assertRaisesRegex(
                live_identity.LiveIdentityError, "episode_already_fatal"
            ):
                fixture.verifier().verify("later")

    def test_checkout_and_each_bound_artifact_drift_fail_closed(self):
        mutators = {
            "checkout": lambda fixture: (
                fixture.repo / "tracked.py"
            ).write_text("value = 2\n"),
            "archive": lambda fixture: fixture.archive.write_bytes(b"changed"),
            "manifest": lambda fixture: fixture.manifest.write_bytes(b"{}"),
            "verification": lambda fixture: fixture.verification.write_bytes(b"{}"),
            "readiness": lambda fixture: fixture.readiness.write_bytes(b"{}"),
            "launcher": lambda fixture: fixture.launcher.write_bytes(b"changed"),
            "binary": lambda fixture: fixture.binary.write_bytes(b"changed"),
            "canonical_environment": lambda fixture: fixture.environment.write_bytes(
                b"{}\n"
            ),
        }
        for label, mutate in mutators.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                fixture = IdentityFixture(Path(tmp))
                mutate(fixture)
                with self.assertRaises(live_identity.LiveIdentityError):
                    fixture.verifier().verify(f"{label}:before")
                self.assertTrue(json.loads(fixture.state.read_bytes())["fatal"])

    def test_preexisting_fatal_state_cannot_be_cleared(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = IdentityFixture(Path(tmp))
            fixture.state.parent.mkdir(parents=True)
            original = {
                "schema_version": "issue827-rna-supervisor-state-v1",
                "fatal": True,
                "fatal_reason": "prior_failure",
            }
            fixture.state.write_bytes(canonical(original))
            with self.assertRaisesRegex(
                live_identity.LiveIdentityError, "prior_failure"
            ):
                fixture.verifier().verify("query:before")
            self.assertEqual(json.loads(fixture.state.read_bytes()), original)

    def test_malformed_or_nonabsolute_identity_paths_are_latched(self):
        identity_mutators = {
            "index_checkout_type": lambda value: value.update(
                {"index_checkout": {}}
            ),
            "index_checkout_relative": lambda value: value.update(
                {"index_checkout": "index"}
            ),
            "launcher_empty": lambda value: value.update({"launcher_path": ""}),
            "binary_relative": lambda value: value.update(
                {"binary_path": "binary"}
            ),
            "environment_ref_relative": lambda value: value[
                "canonical_environment"
            ].update({"path": "canonical-environment.json"}),
            "manifest_ref_relative": lambda value: value[
                "cache_manifest"
            ].update({"path": "cache.manifest.json"}),
        }
        for label, mutate in identity_mutators.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                fixture = IdentityFixture(Path(tmp))
                mutate(fixture.identity)
                fixture.identity_path.write_bytes(canonical(fixture.identity))
                fixture.config["expected_identity_sha256"] = sha(
                    fixture.identity_path.read_bytes()
                )
                with self.assertRaises(live_identity.LiveIdentityError):
                    fixture.verifier().verify(f"{label}:before")
                self.assertTrue(json.loads(fixture.state.read_bytes())["fatal"])

        config_mutators = {
            "identity_receipt_relative": lambda value: value.update(
                {"identity_receipt": "identity.json"}
            ),
            "repo_empty": lambda value: value.update({"repo": ""}),
            "launcher_relative": lambda value: value.update(
                {"launcher": "launcher"}
            ),
            "binary_type": lambda value: value.update({"binary": {}}),
        }
        for label, mutate in config_mutators.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                fixture = IdentityFixture(Path(tmp))
                mutate(fixture.config)
                with self.assertRaises(live_identity.LiveIdentityError):
                    fixture.verifier().verify(f"{label}:before")
                self.assertTrue(json.loads(fixture.state.read_bytes())["fatal"])


class ProviderUsageTests(unittest.TestCase):
    def usage(self, **changes: int) -> dict[str, int]:
        value = {
            "input_tokens": 10,
            "cache_creation_input_tokens": 20,
            "cache_read_input_tokens": 30,
            "output_tokens": 4,
        }
        value.update(changes)
        return value

    def exact_claude_216_result(self) -> dict:
        """Synthetic values in the exact successful Claude Code 2.1.216 shape."""
        return {
            "num_turns": 7,
            # Top-level usage is a distinct CLI scope and is intentionally not
            # equal to the whole-invocation multi-model aggregate.
            "usage": {
                "input_tokens": 2,
                "cache_creation_input_tokens": 30,
                "cache_read_input_tokens": 300,
                "output_tokens": 10,
                "server_tool_use": {"web_search_requests": 0},
                "iterations": [],
            },
            "modelUsage": {
                "claude-haiku-4-5-20251001": {
                    "inputTokens": 3,
                    "outputTokens": 1,
                    "cacheReadInputTokens": 0,
                    "cacheCreationInputTokens": 0,
                    "costUSD": 0.001,
                    "contextWindow": 200000,
                },
                "claude-sonnet-5": {
                    "inputTokens": 10,
                    "outputTokens": 30,
                    "cacheReadInputTokens": 100,
                    "cacheCreationInputTokens": 20,
                    "costUSD": 0.1,
                    "contextWindow": 1000000,
                },
            },
        }

    def test_exact_claude_216_shape_uses_model_usage_authoritatively(self):
        raw = self.exact_claude_216_result()
        receipt = provider_usage.parse_claude_usage(
            raw,
            provider_responses=7,
            provider_requests=7,
        )
        self.assertTrue(receipt["valid"])
        self.assertEqual(receipt["source"], "whole_invocation_model_usage")
        self.assertEqual(receipt["input_tokens"], 13)
        self.assertEqual(receipt["cache_creation_input_tokens"], 20)
        self.assertEqual(receipt["cache_read_input_tokens"], 100)
        self.assertEqual(receipt["output_tokens"], 31)
        self.assertEqual(receipt["provider_total_tokens"], 164)
        self.assertIsNone(receipt["reasoning_tokens"])
        self.assertFalse(receipt["reasoning_tokens_observed"])
        self.assertEqual(receipt["unobserved_fields"], ["reasoning_tokens"])
        self.assertEqual(receipt["top_level_usage"]["provider_total_tokens"], 342)
        self.assertIsNone(receipt["top_level_usage"]["reasoning_tokens"])
        self.assertEqual(receipt["cli_turns"], 7)
        self.assertEqual(receipt["provider_responses"], 7)
        self.assertEqual(receipt["provider_requests"], 7)
        json.dumps(receipt)

    def test_cli_turns_and_provider_requests_are_distinct_nullable_counts(self):
        raw = self.exact_claude_216_result()
        receipt = provider_usage.parse_claude_usage(
            raw,
            provider_requests=2,
        )
        self.assertEqual(receipt["cli_turns"], 7)
        self.assertEqual(receipt["provider_requests"], 2)
        self.assertIsNone(receipt["provider_responses"])
        raw.pop("num_turns")
        no_cli_count = provider_usage.parse_claude_usage(
            raw
        )
        self.assertIsNone(no_cli_count["cli_turns"])
        self.assertIsNone(no_cli_count["provider_requests"])

    def test_auxiliary_cli_usage_is_included_without_transcript_false_failure(self):
        raw = {
            "num_turns": 2,
            "usage": self.usage(
                input_tokens=20,
                cache_creation_input_tokens=0,
                cache_read_input_tokens=0,
                output_tokens=11,
            ),
            "modelUsage": {
                "claude-sonnet-5": {
                    "inputTokens": 30,
                    "cacheCreationInputTokens": 0,
                    "cacheReadInputTokens": 0,
                    "outputTokens": 19,
                }
            },
        }
        events = [
            {
                "usage": self.usage(
                    input_tokens=10,
                    cache_creation_input_tokens=0,
                    cache_read_input_tokens=0,
                    output_tokens=8,
                )
            },
            {
                "usage": self.usage(
                    input_tokens=10,
                    cache_creation_input_tokens=0,
                    cache_read_input_tokens=0,
                    output_tokens=3,
                )
            },
        ]
        receipt = provider_usage.parse_claude_usage(
            raw,
            events,
            provider_responses=2,
            provider_requests=3,
        )
        self.assertTrue(receipt["valid"])
        self.assertEqual(receipt["provider_total_tokens"], 49)
        self.assertEqual(receipt["model_events_usage"]["provider_total_tokens"], 31)
        self.assertEqual(receipt["auxiliary_cli_usage"]["provider_total_tokens"], 18)
        self.assertEqual(receipt["provider_responses"], 2)
        self.assertEqual(
            receipt["provider_responses_scope"], "agent_transcript_only"
        )
        self.assertEqual(receipt["provider_requests"], 3)

    def test_transcript_usage_cannot_exceed_whole_invocation(self):
        raw = {
            "num_turns": 1,
            "usage": self.usage(
                input_tokens=11,
                cache_creation_input_tokens=0,
                cache_read_input_tokens=0,
                output_tokens=1,
            ),
            "modelUsage": {
                "claude-sonnet-5": {
                    "inputTokens": 10,
                    "cacheCreationInputTokens": 0,
                    "cacheReadInputTokens": 0,
                    "outputTokens": 1,
                }
            },
        }
        with self.assertRaisesRegex(
            provider_usage.ProviderUsageError,
            "provider_usage_exceeds_whole_invocation:model_events:input_tokens",
        ):
            provider_usage.parse_claude_usage(
                raw,
                [{
                    "usage": self.usage(
                        input_tokens=11,
                        cache_creation_input_tokens=0,
                        cache_read_input_tokens=0,
                        output_tokens=1,
                    )
                }],
                provider_responses=1,
                provider_requests=1,
            )

    def test_missing_malformed_zero_and_inconsistent_usage_fail_closed(self):
        missing = self.exact_claude_216_result()
        del missing["modelUsage"]["claude-sonnet-5"]["outputTokens"]
        negative = self.exact_claude_216_result()
        negative["modelUsage"]["claude-sonnet-5"]["cacheReadInputTokens"] = -1
        boolean = self.exact_claude_216_result()
        boolean["modelUsage"]["claude-sonnet-5"]["outputTokens"] = True
        malformed_reasoning = self.exact_claude_216_result()
        malformed_reasoning["modelUsage"]["claude-sonnet-5"][
            "reasoningTokens"
        ] = "unknown"
        synthetic_zero = {
            "num_turns": 1,
            "usage": self.usage(
                input_tokens=0,
                cache_creation_input_tokens=0,
                cache_read_input_tokens=0,
                output_tokens=0,
            ),
            "modelUsage": {
                "claude-sonnet-5": {
                    "inputTokens": 0,
                    "cacheCreationInputTokens": 0,
                    "cacheReadInputTokens": 0,
                    "outputTokens": 0,
                }
            },
        }
        model_usage_missing = self.exact_claude_216_result()
        del model_usage_missing["modelUsage"]
        cases = {
            "missing": missing,
            "negative": negative,
            "boolean": boolean,
            "malformed_reasoning": malformed_reasoning,
            "synthetic_zero": synthetic_zero,
            "model_usage_missing": model_usage_missing,
        }
        for label, raw in cases.items():
            with self.subTest(label=label), self.assertRaises(
                provider_usage.ProviderUsageError
            ) as failure:
                provider_usage.parse_claude_usage(raw)
            self.assertFalse(failure.exception.receipt["valid"])
            self.assertIsNone(
                failure.exception.receipt["provider_total_tokens"]
            )

    def test_duplicate_aliases_or_event_counts_cannot_disagree(self):
        aliases = self.exact_claude_216_result()
        aliases["modelUsage"]["claude-sonnet-5"]["input_tokens"] = 11
        with self.assertRaisesRegex(
            provider_usage.ProviderUsageError, "aliases_inconsistent"
        ):
            provider_usage.parse_claude_usage(aliases)

        raw = self.exact_claude_216_result()
        aggregate = self.usage(
            input_tokens=13,
            cache_creation_input_tokens=20,
            cache_read_input_tokens=100,
            output_tokens=31,
        )
        with self.assertRaisesRegex(
            provider_usage.ProviderUsageError,
            "provider_responses_inconsistent_with_model_events",
        ):
            provider_usage.parse_claude_usage(
                raw,
                [{"usage": aggregate}],
                provider_responses=2,
            )

    def test_model_not_invoked_has_no_synthetic_usage(self):
        receipt = provider_usage.parse_claude_usage({}, model_invoked=False)
        self.assertTrue(receipt["valid"])
        self.assertIsNone(receipt["provider_total_tokens"])
        self.assertIsNone(receipt["cli_turns"])
        self.assertIsNone(receipt["provider_requests"])
        with self.assertRaisesRegex(
            provider_usage.ProviderUsageError,
            "provider_usage_present_without_model",
        ):
            provider_usage.parse_claude_usage(
                self.exact_claude_216_result(), model_invoked=False
            )
        for counts in (
            {"provider_responses": 1},
            {"provider_requests": 1},
        ):
            with self.subTest(counts=counts), self.assertRaises(
                provider_usage.ProviderUsageError
            ):
                provider_usage.parse_claude_usage(
                    {}, model_invoked=False, **counts
                )

    def test_invoked_provider_counts_cannot_contradict_usage(self):
        cases = (
            {"provider_responses": 0},
            {"provider_requests": 0},
            {"provider_responses": 2, "provider_requests": 1},
        )
        for counts in cases:
            with self.subTest(counts=counts), self.assertRaises(
                provider_usage.ProviderUsageError
            ):
                provider_usage.parse_claude_usage(
                    self.exact_claude_216_result(), **counts
                )

    def test_observed_reasoning_is_aggregated_without_synthesis(self):
        raw = self.exact_claude_216_result()
        raw["usage"]["reasoning_tokens"] = 2
        raw["modelUsage"]["claude-haiku-4-5-20251001"]["reasoningTokens"] = 1
        raw["modelUsage"]["claude-sonnet-5"]["reasoningTokens"] = 4
        receipt = provider_usage.parse_claude_usage(raw)
        self.assertEqual(receipt["reasoning_tokens"], 5)
        self.assertTrue(receipt["reasoning_tokens_observed"])
        self.assertEqual(receipt["unobserved_fields"], [])
        self.assertEqual(receipt["top_level_usage"]["reasoning_tokens"], 2)


if __name__ == "__main__":
    unittest.main()
