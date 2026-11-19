"""Tests for scripts/ci/check_toolchain_pin.py (standard library only)."""
import tempfile
import unittest
from pathlib import Path

from check_toolchain_pin import check

GOOD_TOML = (
    "[toolchain]\n"
    'channel = "1.99.1"\n'
    'components = ["rustfmt", "clippy"]\n'
    'profile = "minimal"\n'
)

GOOD_STEP = (
    "jobs:\n"
    "  build:\n"
    "    steps:\n"
    "      - uses: dtolnay/rust-toolchain@abc # stable\n"
    "        with:\n"
    "          toolchain: 1.99.1\n"
)


def write_tree(root, toml=GOOD_TOML, workflow=GOOD_STEP):
    if toml is not None:
        (root / "rust-toolchain.toml").write_text(toml)
    wf_dir = root / ".github" / "workflows"
    wf_dir.mkdir(parents=True)
    if workflow is not None:
        (wf_dir / "x.yml").write_text(workflow)


class CheckToolchainPinTest(unittest.TestCase):
    def test_good_tree_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root)
            self.assertEqual(check(root), [])

    def test_stable_toolchain_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, workflow=GOOD_STEP.replace("1.99.1", "stable"))
            errors = check(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("x.yml", errors[0])
            self.assertIn("stable", errors[0])

    def test_missing_toolchain_input_fails(self):
        workflow = (
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: dtolnay/rust-toolchain@abc # stable\n"
            "        with:\n"
            "          targets: x86_64-unknown-linux-musl\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, workflow=workflow)
            errors = check(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("x.yml", errors[0])

    def test_step_without_with_fails(self):
        workflow = (
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: dtolnay/rust-toolchain@abc # stable\n"
            "      - run: echo hi\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, workflow=workflow)
            errors = check(root)
            self.assertEqual(len(errors), 1)

    def test_toolchain_of_next_step_is_not_counted(self):
        workflow = (
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: dtolnay/rust-toolchain@abc # stable\n"
            "      - uses: dtolnay/rust-toolchain@abc # stable\n"
            "        with:\n"
            "          toolchain: 1.99.1\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, workflow=workflow)
            errors = check(root)
            self.assertEqual(len(errors), 1)

    def test_missing_toml_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, toml=None)
            errors = check(root)
            self.assertTrue(any("rust-toolchain.toml" in e for e in errors))

    def test_wrong_channel_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, toml=GOOD_TOML.replace('"1.99.1"', '"stable"'))
            errors = check(root)
            self.assertTrue(any("1.99.1" in e for e in errors))

    def test_missing_component_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, toml=GOOD_TOML.replace('["rustfmt", "clippy"]', '["rustfmt"]'))
            errors = check(root)
            self.assertTrue(any("clippy" in e for e in errors))

    def test_repository_is_pinned(self):
        # Negative test: fails on main until the pin lands.
        self.assertEqual(check(Path(__file__).resolve().parents[2]), [])


if __name__ == "__main__":
    unittest.main()
