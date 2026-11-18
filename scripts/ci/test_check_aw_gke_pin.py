"""Tests for scripts/ci/check_aw_gke_pin.py (standard library only)."""
import tempfile
import unittest
from pathlib import Path

from check_aw_gke_pin import check, main

A = "8e6189b1a244da71a0c09f71ce869e8715d04f0c"
B = "edb3802d18d39d205bd0ada8431ecc97c6938c14"


def wf(uses, ref):
    return (
        "jobs:\n"
        "  x:\n"
        f"    uses: faberline/aw/.github/workflows/gke-test.yml@{uses}\n"
        "    with:\n"
        f"      aw-ref: {ref}\n"
    )


def write_tree(root, files):
    wf_dir = root / ".github" / "workflows"
    wf_dir.mkdir(parents=True)
    for name, text in files.items():
        (wf_dir / name).write_text(text)


class CheckAwGkePinTest(unittest.TestCase):
    def test_same_commit_everywhere_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, {"ci.yml": wf(A, A), "rc.yml": wf(A, A)})
            self.assertEqual(check(root), [])

    def test_stale_candidate_pin_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(
                root,
                {"ci.yml": wf(A, A), "tape-release-candidate.yml": wf(B, B)},
            )
            errors = check(root)
            self.assertEqual(len(errors), 2, errors)
            for error in errors:
                self.assertIn("tape-release-candidate.yml", error)
                self.assertIn("differs from", error)

    def test_aw_ref_alone_differs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, {"x.yml": wf(A, B)})
            errors = check(root)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("x.yml:5:", errors[0])
            self.assertIn("differs from", errors[0])

    def test_branch_ref_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, {"x.yml": wf("main", A)})
            errors = check(root)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("40-hex", errors[0])
            self.assertIn("'main'", errors[0])

    def test_short_sha_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, {"x.yml": wf("8e6189b", "8e6189b")})
            errors = check(root)
            self.assertEqual(len(errors), 2, errors)
            for error in errors:
                self.assertIn("40-hex", error)

    def test_no_reference_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, {"x.yml": "jobs: {}\n"})
            self.assertEqual(
                check(root),
                ["no faberline/aw gke-test.yml reference found in .github/workflows"],
            )

    def test_comment_after_value_is_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            text = (
                "jobs:\n"
                "  x:\n"
                f"    uses: faberline/aw/.github/workflows/gke-test.yml@{A}\n"
                "    with:\n"
                f"      aw-ref: {A} # note\n"
            )
            write_tree(root, {"x.yml": text})
            self.assertEqual(check(root), [])

    def test_main_returns_one_on_error(self):
        with tempfile.TemporaryDirectory() as bad, tempfile.TemporaryDirectory() as good:
            write_tree(Path(bad), {"x.yml": wf(A, B)})
            write_tree(Path(good), {"x.yml": wf(A, A)})
            self.assertEqual(main(["--dir", bad]), 1)
            self.assertEqual(main(["--dir", good]), 0)


if __name__ == "__main__":
    unittest.main()
