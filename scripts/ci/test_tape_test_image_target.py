"""Tests for the `target` input of .github/workflows/tape-test-image.yml (standard library only)."""
import re
import unittest
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "tape-test-image.yml"


def read_workflow():
    return WORKFLOW.read_text(encoding="utf-8")


def input_lines(lines, name):
    """The key line of input `name` and the lines indented under it; [] when absent."""
    key = f"      {name}:"
    if key not in lines:
        return []
    i = lines.index(key)
    out = [key]
    for line in lines[i + 1:]:
        if not line.startswith("        "):
            break
        out.append(line)
    return out


def trigger_block(lines, start, end):
    """Lines from `start` up to (not including) the next `end` line."""
    i = lines.index(start)
    j = lines.index(end, i + 1)
    return lines[i:j]


class TapeTestImageTargetTest(unittest.TestCase):
    def test_dispatch_target_input_defaults_to_server(self):
        lines = trigger_block(read_workflow().splitlines(), "  workflow_dispatch:", "  workflow_call:")
        target = input_lines(lines, "target")
        self.assertIn("        default: server", target)

    def test_call_target_input_defaults_to_server(self):
        lines = trigger_block(read_workflow().splitlines(), "  workflow_call:", "    outputs:")
        target = input_lines(lines, "target")
        self.assertIn("        default: server", target)
        self.assertIn("        type: string", target)

    def test_build_step_passes_target_input_unchanged(self):
        text = read_workflow()
        start = text.index("name: Build and push multi-arch test image")
        end = text.index("- name: Record pushed digest", start)
        step = text[start:end].splitlines()
        self.assertIn("          target: ${{ inputs.target }}", step)
        self.assertFalse(any("||" in line for line in step), "fallback in target expression")
        self.assertFalse(any("target: server" in line for line in step), "hard-coded target")

    def test_image_tag_unchanged(self):
        lines = read_workflow().splitlines()
        self.assertIn("          tags: ${{ steps.ver.outputs.tag }}", lines)
        self.assertIn('          tag="ghcr.io/${{ github.repository_owner }}/tape:sha-${git12}"', lines)
        code = [line for line in lines if line.strip() and not line.lstrip().startswith("#")]
        tags_lines = [line for line in code if line.strip().startswith("tags:")]
        self.assertEqual(len(tags_lines), 1)
        for line in code:
            self.assertNotIn(":latest", line)
            self.assertIsNone(re.search(r"tape:\d", line), line)


if __name__ == "__main__":
    unittest.main()
