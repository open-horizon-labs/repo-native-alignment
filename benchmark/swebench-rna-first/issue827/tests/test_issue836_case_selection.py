from __future__ import annotations

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import registration_contract
import select_cases


class Issue836CaseSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.registration = json.loads(
            (HERE / "registration.template.json").read_bytes()
        )
        cls.binding_directory = tempfile.TemporaryDirectory()
        cls.binding_repo = Path(cls.binding_directory.name)
        registration_path = (
            cls.binding_repo
            / select_cases.ISSUE836_V3_REGISTRATION_PATH
        )
        registration_path.parent.mkdir(parents=True)
        registration_path.write_bytes(
            select_cases.canonical(cls.registration)
        )
        subprocess.run(
            ["git", "init", "-q", str(cls.binding_repo)],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(cls.binding_repo), "add", "."],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(cls.binding_repo),
                "-c",
                "user.name=selector-test",
                "-c",
                "user.email=selector-test@example.invalid",
                "commit",
                "-qm",
                "v3 registration",
            ],
            check=True,
        )
        cls.binding_commit = subprocess.run(
            ["git", "-C", str(cls.binding_repo), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        cls.binding_sha256 = select_cases.sha256_bytes(
            registration_path.read_bytes()
        )

    @classmethod
    def tearDownClass(cls) -> None:
        cls.binding_directory.cleanup()

    def current_selection(self) -> dict:
        selection = json.loads(
            (HERE.parent / "issue836" / "selection.json").read_bytes()
        )
        supersession = json.loads(
            json.dumps(
                registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
            )
        )
        selection.update(
            {
                "schema_version": select_cases.CURRENT_SCHEMA,
                "registration_commit": self.binding_commit,
                "registration_sha256": self.binding_sha256,
                "prior_model_calls": 0,
                "case_replacement_after_model_start": False,
                "pre_model_v2_supersession": supersession,
                "prefix_lineage": json.loads(
                    json.dumps(
                        registration_contract.FROZEN_V3_PREFIX_LINEAGE
                    )
                ),
            }
        )
        selection["cases"][supersession["superseded_rank"] - 1] = {
            "rank": supersession["superseded_rank"],
            "instance_id": supersession["replacement_instance_id"],
            "ranking_sha256": supersession["replacement_ranking_sha256"],
            "repo": supersession["replacement_repo"],
            "base_commit": supersession["replacement_base_commit"],
            "base_tree": supersession["replacement_base_tree"],
            "problem_statement_sha256": supersession[
                "replacement_problem_statement_sha256"
            ],
            "arm_order": supersession["preserved_arm_order"],
            "cache_preparation": (
                "cold exact-tree in-place index with the registered exact-CI "
                "artifact; fresh-process readiness and strict hybrid/Metal "
                "query verification"
            ),
        }
        selection.pop("digest", None)
        selection["digest"] = select_cases.sha256_bytes(
            select_cases.canonical(selection)
        )
        return selection

    def current_identities(
        self,
        selection: dict,
        **kwargs: object,
    ) -> tuple[select_cases.ExpectedEpisodeIdentity, ...]:
        return select_cases.expected_episode_identities(
            self.registration,
            selection,
            registration_repository=self.binding_repo,
            **kwargs,
        )

    def reseal(self, selection: dict) -> None:
        selection.pop("digest", None)
        selection["digest"] = select_cases.sha256_bytes(
            select_cases.canonical(selection)
        )

    def test_deterministic_prefix_is_exactly_twenty_eligible_ranks(self) -> None:
        instance_ids = [f"case-{index:02d}" for index in range(30)]
        excluded = {"case-03", "case-17"}
        expected = sorted(
            (
                hashlib.sha256(
                    select_cases.EXPECTED_SEED.encode("utf-8")
                    + b"\0"
                    + instance_id.encode("utf-8")
                ).hexdigest(),
                instance_id,
            )
            for instance_id in instance_ids
            if instance_id not in excluded
        )[:20]
        selected = select_cases.deterministic_ranked_prefix(
            instance_ids,
            excluded,
            select_cases.EXPECTED_SEED,
            20,
        )
        self.assertEqual(selected, expected)
        self.assertEqual(len(selected), 20)

    def test_v3_replaces_only_rank_eight_with_deterministic_s2_rank_21(
        self,
    ) -> None:
        supersession = (
            registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION
        )
        eligible = [
            (f"{rank:064x}", f"case-{rank:02d}")
            for rank in range(1, 22)
        ]
        eligible[7] = (
            "02e8357bad6c501dec2de83e0a5d769241abb835a7c484fdba3301a415489515",
            supersession["excluded_instance_id"],
        )
        eligible[20] = (
            supersession["replacement_ranking_sha256"],
            supersession["replacement_instance_id"],
        )
        selected = select_cases.registered_ranked_cohort(
            self.registration,
            eligible,
        )
        self.assertEqual(len(selected), 20)
        self.assertEqual(selected[7], eligible[20])
        self.assertEqual(selected[:7], eligible[:7])
        self.assertEqual(selected[8:], eligible[8:20])

    def test_current_selection_has_exact_ranks_parity_and_forty_episodes(
        self,
    ) -> None:
        selection = self.current_selection()
        identities = self.current_identities(selection)
        self.assertEqual(
            [case["rank"] for case in selection["cases"]],
            list(range(1, 21)),
        )
        self.assertEqual(len(identities), 40)
        self.assertEqual(
            [identity.rank for identity in identities],
            [rank for rank in range(1, 21) for _ in range(2)],
        )
        self.assertEqual(
            sum(case["arm_order"][0] == "A" for case in selection["cases"]),
            10,
        )
        self.assertEqual(
            sum(case["arm_order"][0] == "T" for case in selection["cases"]),
            10,
        )
        self.assertTrue(
            all(
                case["arm_order"]
                == (["A", "T"] if case["rank"] % 2 == 1 else ["T", "A"])
                for case in selection["cases"]
            )
        )

    def test_current_registration_records_truthful_prefix_lineage(self) -> None:
        selector = self.registration["selector"]
        self.assertEqual(
            selector["prefix_lineage"],
            {
                "ranks_1_through_2": "pre_model_carry_forward_prefix",
                "ranks_3_through_7_and_9_through_20": (
                    "deterministic_extension"
                ),
                "rank_8": "pre_model_replacement_from_s2_rank_21",
                "outcomes_inspected_for_extension": False,
            },
        )
        self.assertFalse(
            selector["gold_or_outcomes_inspected_before_selection"]
        )
        self.assertFalse(
            selector["problem_statements_inspected_by_human_before_selection"]
        )
        self.assertEqual(
            selector["pre_model_v2_supersession"],
            registration_contract.FROZEN_V3_PRE_MODEL_SUPERSESSION,
        )
        supersession = selector["pre_model_v2_supersession"]
        self.assertEqual(supersession["superseded_rank"], 8)
        self.assertEqual(
            supersession["excluded_instance_id"],
            "sympy__sympy-24661",
        )
        self.assertEqual(
            supersession["replacement_instance_id"],
            "django__django-11163",
        )
        self.assertEqual(supersession["replacement_source_rank"], 21)
        self.assertEqual(supersession["preserved_arm_order"], ["T", "A"])
        self.assertEqual(
            supersession["replacement_base_commit"],
            "e6588aa4e793b7f56f4cadbfa155b581e0efc59a",
        )
        self.assertEqual(
            supersession["replacement_base_tree"],
            "4c221e3aba9030ab459871106740934193ce1118",
        )
        self.assertEqual(
            supersession["replacement_problem_statement_sha256"],
            "8dbd8cae38c3a82b681a79bdfe8ffaa78aa6f1fad9744a1ca197f90265e5c80b",
        )
        self.assertEqual(
            supersession["reason_code"],
            "old_shipped_rna_binary_mjs_descriptor_incompatibility",
        )

    def test_historical_issue830_pair_remains_a_valid_verifier_path(
        self,
    ) -> None:
        registration = json.loads(
            (HERE.parent / "issue830" / "registration.json").read_bytes()
        )
        selection = json.loads(
            (HERE.parent / "issue830" / "selection.json").read_bytes()
        )
        registration_contract.validate_registration(registration)
        identities = select_cases.expected_episode_identities(
            registration,
            selection,
        )
        self.assertEqual(
            identities,
            (
                select_cases.ExpectedEpisodeIdentity(
                    1,
                    "sympy__sympy-23534",
                    "A",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    1,
                    "sympy__sympy-23534",
                    "T",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    2,
                    "django__django-11179",
                    "T",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    2,
                    "django__django-11179",
                    "A",
                ),
            ),
        )

    def test_historical_issue836_v2_cohort_remains_a_valid_verifier_path(
        self,
    ) -> None:
        registration = json.loads(
            (HERE.parent / "issue836" / "registration.json").read_bytes()
        )
        selection = json.loads(
            (HERE.parent / "issue836" / "selection.json").read_bytes()
        )
        self.assertEqual(
            registration["schema_version"],
            registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA,
        )
        self.assertEqual(
            selection["schema_version"],
            select_cases.ISSUE836_V2_SCHEMA,
        )
        registration_contract.validate_registration(registration)
        identities = select_cases.expected_episode_identities(
            registration,
            selection,
        )
        self.assertEqual(len(identities), 40)
        self.assertEqual(
            identities[14:16],
            (
                select_cases.ExpectedEpisodeIdentity(
                    8,
                    "sympy__sympy-24661",
                    "T",
                ),
                select_cases.ExpectedEpisodeIdentity(
                    8,
                    "sympy__sympy-24661",
                    "A",
                ),
            ),
        )

    def test_selection_schema_rank_and_parity_drift_fail_closed(self) -> None:
        for label, mutate in (
            (
                "schema",
                lambda selection: selection.update(
                    {"schema_version": select_cases.LEGACY_SCHEMA}
                ),
            ),
            (
                "rank",
                lambda selection: selection["cases"][9].update({"rank": 9}),
            ),
            (
                "parity",
                lambda selection: selection["cases"][9].update(
                    {"arm_order": ["A", "T"]}
                ),
            ),
            (
                "supersession",
                lambda selection: selection[
                    "pre_model_v2_supersession"
                ].update({"replacement_source_rank": 22}),
            ),
            (
                "lineage",
                lambda selection: selection["prefix_lineage"].update(
                    {"rank_8": "deterministic_extension"}
                ),
            ),
            (
                "replacement",
                lambda selection: selection["cases"][7].update(
                    {"instance_id": "django__django-11179"}
                ),
            ),
            (
                "non-rank-8-instance",
                lambda selection: selection["cases"][0].update(
                    {"instance_id": "fabricated__case-00001"}
                ),
            ),
        ):
            with self.subTest(label=label):
                selection = self.current_selection()
                mutate(selection)
                self.reseal(selection)
                with self.assertRaises(select_cases.SelectionError):
                    self.current_identities(selection)

    def test_v3_rejects_absent_or_changed_frozen_v2_artifacts(self) -> None:
        registration_bytes = select_cases.FROZEN_V2_REGISTRATION_FILE.read_bytes()
        selection_bytes = select_cases.FROZEN_V2_SELECTION_FILE.read_bytes()
        for label, mutate in (
            ("absent-registration", lambda registration, selection: registration.unlink()),
            ("absent-selection", lambda registration, selection: selection.unlink()),
            (
                "changed-registration",
                lambda registration, selection: registration.write_bytes(
                    registration.read_bytes() + b" "
                ),
            ),
            (
                "changed-selection",
                lambda registration, selection: selection.write_bytes(
                    selection.read_bytes() + b" "
                ),
            ),
        ):
            with self.subTest(label=label), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                registration_path = root / "registration.json"
                selection_path = root / "selection.json"
                registration_path.write_bytes(registration_bytes)
                selection_path.write_bytes(selection_bytes)
                mutate(registration_path, selection_path)
                with self.assertRaises(select_cases.SelectionError):
                    self.current_identities(
                        self.current_selection(),
                        frozen_v2_registration_path=registration_path,
                        frozen_v2_selection_path=selection_path,
                    )

    def test_v3_selection_provenance_and_digest_fail_closed(self) -> None:
        for label, mutate, reseal in (
            (
                "forty-zero-registration-commit",
                lambda selection: selection.update(
                    {"registration_commit": "0" * 40}
                ),
                True,
            ),
            (
                "dataset",
                lambda selection: selection.update(
                    {"dataset_arrow_sha256": "f" * 64}
                ),
                True,
            ),
            (
                "exclusions-file",
                lambda selection: selection.update(
                    {"exclusions_sha256": "f" * 64}
                ),
                True,
            ),
            (
                "excluded-ids",
                lambda selection: selection.update(
                    {"excluded_ids_sha256": "f" * 64}
                ),
                True,
            ),
            (
                "digest",
                lambda selection: selection.update({"digest": "f" * 64}),
                False,
            ),
        ):
            with self.subTest(label=label):
                selection = self.current_selection()
                mutate(selection)
                if reseal:
                    self.reseal(selection)
                with self.assertRaises(select_cases.SelectionError):
                    self.current_identities(selection)

    def test_registration_commit_binding_supports_separate_issue_path(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            registration_path = (
                repo
                / select_cases.ISSUE836_V3_REGISTRATION_PATH
            )
            exclusions_path = (
                repo / "benchmark/swebench-rna-first/issue827/exclusions.json"
            )
            selector_path = (
                repo / "benchmark/swebench-rna-first/issue827/select_cases.py"
            )
            registration_path.parent.mkdir(parents=True)
            exclusions_path.parent.mkdir(parents=True)
            registration_bytes = select_cases.canonical(
                {
                    "schema_version": (
                        registration_contract.CURRENT_REGISTRATION_SCHEMA
                    ),
                    "issue": 836,
                }
            )
            registration_path.write_bytes(registration_bytes)
            v2_registration_path = (
                repo / select_cases.ISSUE836_V2_REGISTRATION_PATH
            )
            v2_registration_path.parent.mkdir(parents=True)
            v2_registration_path.write_bytes(registration_bytes)
            exclusions_path.write_bytes(b'{"excluded":[]}\n')
            selector_path.write_bytes(
                Path(select_cases.__file__).resolve().read_bytes()
            )
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(repo),
                    "-c",
                    "user.name=selector-test",
                    "-c",
                    "user.email=selector-test@example.invalid",
                    "commit",
                    "-qm",
                    "fixture",
                ],
                check=True,
            )
            commit = subprocess.run(
                ["git", "-C", str(repo), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            select_cases.require_commit_binding(
                repo,
                commit,
                registration_path,
                exclusions_path,
                select_cases.ISSUE836_V3_REGISTRATION_PATH,
            )
            with self.assertRaises(select_cases.SelectionError):
                select_cases.require_commit_binding(
                    repo,
                    commit,
                    registration_path,
                    exclusions_path,
                    select_cases.ISSUE836_V2_REGISTRATION_PATH,
                )

            registration_path.write_bytes(
                select_cases.canonical(
                    {
                        "schema_version": (
                            registration_contract.LEGACY_REGISTRATION_SCHEMA
                        ),
                        "issue": 827,
                    }
                )
            )
            with self.assertRaises(select_cases.SelectionError):
                select_cases.require_commit_binding(
                    repo,
                    commit,
                    registration_path,
                    exclusions_path,
                    select_cases.ISSUE836_V3_REGISTRATION_PATH,
                )

            registration_path.unlink()
            alias_target = repo / "registration-alias-target.json"
            alias_target.write_bytes(registration_bytes)
            registration_path.symlink_to(alias_target)
            with self.assertRaises(select_cases.SelectionError):
                select_cases.require_commit_binding(
                    repo,
                    commit,
                    registration_path,
                    exclusions_path,
                    select_cases.ISSUE836_V3_REGISTRATION_PATH,
                )

    def test_registration_repo_path_rejects_unsafe_or_ambiguous_paths(
        self,
    ) -> None:
        for schema, value in (
            (
                registration_contract.LEGACY_REGISTRATION_SCHEMA,
                select_cases.REGISTRATION_PATH,
            ),
            (
                registration_contract.ISSUE836_V2_REGISTRATION_SCHEMA,
                select_cases.ISSUE836_V2_REGISTRATION_PATH,
            ),
            (
                registration_contract.CURRENT_REGISTRATION_SCHEMA,
                select_cases.CURRENT_REGISTRATION_PATH,
            ),
        ):
            self.assertEqual(
                select_cases.registered_registration_repo_path(value, schema),
                value,
            )
        with self.assertRaises(select_cases.SelectionError):
            select_cases.registered_registration_repo_path(
                select_cases.ISSUE836_V2_REGISTRATION_PATH,
                registration_contract.CURRENT_REGISTRATION_SCHEMA,
            )
        for value in (
            "/benchmark/registration.json",
            "../registration.json",
            "benchmark/../registration.json",
            "./benchmark/registration.json",
            "benchmark//registration.json",
            "benchmark\\registration.json",
            select_cases.EXCLUSIONS_PATH,
            select_cases.SELECTOR_PATH,
            "benchmark/swebench-rna-first/issue999/registration.json",
        ):
            with self.subTest(value=value):
                with self.assertRaises(select_cases.SelectionError):
                    select_cases.registered_registration_repo_path(
                        value
                    )


if __name__ == "__main__":
    unittest.main()
