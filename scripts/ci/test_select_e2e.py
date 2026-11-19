"""Tests for select_e2e.py. Standard library only; run from scripts/ci."""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import select_e2e as sel

ROOT = Path(__file__).resolve().parents[2]
BOTH = [sel.RECOVERY, sel.HA]


def pr(*paths):
    suites, image, _ = sel.select("pull_request", list(paths))
    return suites, image


class SelectTests(unittest.TestCase):
    def test_docs_only_starts_nothing(self):
        self.assertEqual(pr("docs/README.md", "STATUS.md", "clients/openapi.json"), ([], False))

    def test_rust_code_only_starts_nothing(self):
        self.assertEqual(pr("crates/tape-journal/src/lib.rs", "Cargo.lock"), ([], False))

    def test_ha_suite_file_selects_ha_only(self):
        self.assertEqual(pr("crates/tape/tests/it/e2e/operator_ha/raft.rs"), ([sel.HA], True))
        self.assertEqual(pr("crates/tape/tests/it/e2e/operator_ha.rs"), ([sel.HA], True))
        self.assertEqual(pr("k8s/gke-test-job/operator_ha/job.yaml"), ([sel.HA], True))

    def test_recovery_suite_file_selects_recovery_only(self):
        self.assertEqual(pr("crates/tape/tests/it/e2e/operator_recovery.rs"), ([sel.RECOVERY], True))
        self.assertEqual(pr("k8s/gke-test-job/operator_recovery/job.yaml"), ([sel.RECOVERY], True))

    def test_shared_files_select_both(self):
        for path in [
            "crates/tape/tests/it/e2e.rs",
            ".github/workflows/ci.yml",
            ".github/workflows/gke-test.yml",
            "crates/tape-operator/src/lib.rs",
            "k8s/operator/deployment.yaml",
            "k8s/overlays/gke-test/kustomization.yaml",
        ]:
            self.assertEqual(pr(path), (BOTH, True), path)

    def test_test_image_build_selects_recovery_and_the_image_jobs(self):
        self.assertEqual(pr(".github/workflows/tape-test-image.yml"), ([sel.RECOVERY], True))
        self.assertEqual(pr("Dockerfile.test"), ([sel.RECOVERY], True))

    def test_image_suite_file_runs_the_image_jobs_without_gke(self):
        self.assertEqual(pr("crates/tape/tests/it/e2e/image.rs"), ([], True))

    def test_suites_that_ci_does_not_run_select_nothing(self):
        self.assertEqual(pr("crates/tape/tests/it/e2e/operator_soak.rs", "k8s/gke-test-job/operator_soak/job.yaml"), ([], False))

    def test_unknown_path_selects_every_suite(self):
        self.assertEqual(pr("brand_new_dir/file.txt"), (BOTH, True))

    def test_union_of_paths(self):
        self.assertEqual(
            pr("docs/a.md", "crates/tape/tests/it/e2e/operator_ha.rs", "crates/tape/tests/it/e2e/operator_recovery.rs"),
            (BOTH, True),
        )

    def test_no_changed_path_starts_nothing(self):
        self.assertEqual(pr(), ([], False))

    def test_push_and_dispatch_run_every_suite(self):
        for event in ("push", "workflow_dispatch", "merge_group"):
            suites, image, _ = sel.select(event, ["docs/a.md"])
            self.assertEqual((suites, image), (BOTH, True), event)

    def test_every_suite_has_a_test_job_folder(self):
        for suite in sel.SUITES:
            self.assertTrue((ROOT / "k8s" / "gke-test-job" / suite).is_dir(), suite)


class GitTests(unittest.TestCase):
    def test_a_rename_counts_as_the_old_and_the_new_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            def git(*args):
                return subprocess.run(
                    ["git", "-c", "user.email=t@example.com", "-c", "user.name=t", *args],
                    cwd=tmp, check=True, capture_output=True, text=True,
                ).stdout.strip()

            git("init", "-q")
            os.makedirs(os.path.join(tmp, "docs"))
            Path(tmp, "docs", "a.md").write_text("one\ntwo\nthree\nfour\nfive\n")
            git("add", "-A")
            git("commit", "-q", "-m", "base")
            base = git("rev-parse", "HEAD")
            os.makedirs(os.path.join(tmp, "crates", "tape-operator"))
            git("mv", "docs/a.md", "crates/tape-operator/a.md")
            git("commit", "-q", "-m", "move")
            head = git("rev-parse", "HEAD")
            cwd = os.getcwd()
            os.chdir(tmp)
            try:
                paths = sel.changed_paths(base, head)
            finally:
                os.chdir(cwd)
            self.assertEqual(sorted(paths), ["crates/tape-operator/a.md", "docs/a.md"])
            self.assertEqual(sel.select("pull_request", paths)[0], BOTH)


class OutputTests(unittest.TestCase):
    def test_main_writes_the_outputs_and_the_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            out, summary = Path(tmp, "out"), Path(tmp, "summary")
            env = {"GITHUB_OUTPUT": str(out), "GITHUB_STEP_SUMMARY": str(summary)}
            with mock.patch.dict(os.environ, env), mock.patch.object(
                sel, "changed_paths", return_value=["crates/tape/tests/it/e2e/operator_ha.rs"]
            ):
                sel.main(["select_e2e.py", "pull_request", "b", "h"])
            self.assertEqual(out.read_text(), 'suites=["operator_ha"]\ne2e=true\n')
            self.assertIn("crates/tape/tests/it/e2e/operator_ha.rs", summary.read_text())
            self.assertIn("operator_ha", summary.read_text())


class WorkflowContractTests(unittest.TestCase):
    def setUp(self):
        self.ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text()

    def test_lint_runs_the_script_and_exports_both_outputs(self):
        self.assertIn('python3 scripts/ci/select_e2e.py "$EVENT" "$BASE_SHA" "$GITHUB_SHA"', self.ci)
        self.assertIn("e2e: ${{ steps.select.outputs.e2e }}", self.ci)
        self.assertIn("suites: ${{ steps.select.outputs.suites }}", self.ci)

    def test_the_gke_job_is_skipped_when_no_suite_is_selected(self):
        self.assertIn("if: needs.e2e-resolve.outputs.suites != '[]'", self.ci)

    def test_resolve_uses_the_selection_and_not_the_old_input(self):
        self.assertIn("SELECTED_SUITES: ${{ needs.lint.outputs.suites }}", self.ci)
        self.assertNotIn("INPUT_SUITE", self.ci)

    def test_resolve_accepts_exactly_the_known_suites(self):
        for suite in sel.SUITES:
            self.assertIn(f'. == "{suite}"', self.ci)


if __name__ == "__main__":
    unittest.main()
