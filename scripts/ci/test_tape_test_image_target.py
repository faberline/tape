"""Tests for the `target` input of .github/workflows/tape-test-image.yml (standard library only)."""
import os
import re
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "tape-test-image.yml"


def read_workflow():
    return WORKFLOW.read_text(encoding="utf-8")


def resolve_step(text):
    """The text of the `Resolve Dockerfile target` step, up to the build step."""
    start = text.index("- name: Resolve Dockerfile target")
    end = text.index("- name: Build and push multi-arch test image", start)
    return text[start:end]


def resolve_script(step):
    """The shell body of the resolve step (the lines after `run: |`), dedented."""
    lines = step.splitlines()
    i = next(n for n, line in enumerate(lines) if line.strip() == "run: |")
    key_indent = len(lines[i]) - len(lines[i].lstrip())
    body = []
    for line in lines[i + 1:]:
        indent = len(line) - len(line.lstrip())
        if line.strip() and indent <= key_indent:
            break
        body.append(line)
    return textwrap.dedent("\n".join(body)) + "\n"


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

    def test_build_step_passes_resolved_target(self):
        text = read_workflow()
        start = text.index("name: Build and push multi-arch test image")
        end = text.index("- name: Record pushed digest", start)
        step = text[start:end].splitlines()
        self.assertIn("          target: ${{ steps.target.outputs.stage }}", step)
        self.assertFalse(any("inputs.target" in line for line in step), "build step reads the raw input")
        self.assertFalse(any("||" in line for line in step), "fallback in target expression")
        self.assertFalse(any("target: server" in line for line in step), "hard-coded target")

    def test_resolve_step_maps_none_to_empty_stage(self):
        step = resolve_step(read_workflow())
        self.assertIn("id: target", step)
        self.assertIn("TARGET: ${{ inputs.target }}", step)
        self.assertIn('if [ "${TARGET}" = "none" ]; then', step)
        self.assertIn('echo "stage=" >> "$GITHUB_OUTPUT"', step)
        self.assertIn('echo "stage=${TARGET}" >> "$GITHUB_OUTPUT"', step)
        self.assertEqual(step.count("inputs.target"), 1)

    def test_resolve_step_comes_before_build_step(self):
        text = read_workflow()
        self.assertLess(
            text.index("- name: Resolve Dockerfile target"),
            text.index("- name: Build and push multi-arch test image"),
        )

    def test_input_descriptions_name_none(self):
        lines = read_workflow().splitlines()
        dispatch = input_lines(trigger_block(lines, "  workflow_dispatch:", "  workflow_call:"), "target")
        call = input_lines(trigger_block(lines, "  workflow_call:", "    outputs:"), "target")
        for block in (dispatch, call):
            self.assertTrue(any(re.search(r"\bnone\b", line) for line in block), block)

    def test_resolve_script_output(self):
        script = resolve_script(resolve_step(read_workflow()))
        cases = {"none": "stage=\n", "server": "stage=server\n", "base": "stage=base\n"}
        for value, expected in cases.items():
            with tempfile.TemporaryDirectory() as tmp:
                out = os.path.join(tmp, "github_output")
                result = subprocess.run(
                    ["bash", "-c", script],
                    env={"TARGET": value, "GITHUB_OUTPUT": out, "PATH": os.environ["PATH"]},
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                with open(out, encoding="utf-8") as fh:
                    self.assertEqual(fh.read(), expected, f"TARGET={value}")

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
