"""Tests for the checkout ref of .github/workflows/tape-test-image.yml (standard library only)."""
import unittest
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/tape-test-image.yml"
EXPECTED_REF = "ref: ${{ inputs.ref || github.sha }}"


def read_workflow() -> str:
    return WORKFLOW.read_text(encoding="utf-8")


class TapeTestImageRefTest(unittest.TestCase):
    def test_every_checkout_ref_uses_the_run_commit(self):
        lines = [line.strip() for line in read_workflow().splitlines()]
        ref_lines = [line for line in lines if line.startswith("ref: ${{")]
        self.assertEqual(len(ref_lines), 2, ref_lines)
        for line in ref_lines:
            self.assertEqual(line, EXPECTED_REF)

    def test_no_checkout_uses_the_moving_branch(self):
        self.assertNotIn("github.ref", read_workflow())

    def test_ref_input_is_kept_for_dispatch_and_call(self):
        text = read_workflow()
        self.assertIn("inputs.ref", text)
        self.assertIn("workflow_dispatch:", text)
        self.assertIn("workflow_call:", text)


if __name__ == "__main__":
    unittest.main()
