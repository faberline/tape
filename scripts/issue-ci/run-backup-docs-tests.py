#!/usr/bin/env python3
"""Run affected docs tests and verify all six named cases pass.

Libtest zero-match exit code is NOT evidence. This runner ensures:
1. Exact product HEAD verified against HEAD_SHA (no merge preview commit).
2. Exact six named cases executed and reported ok.
3. 6 passed, 0 failed, 0 ignored.
"""
import os
import re
import subprocess
import sys

EXPECTED_TESTS = [
    "backup_destination_docs::readme_backup_sections_list_every_supported_scheme",
    "backup_destination_docs::deployment_handoff_lists_every_supported_scheme",
    "backup_destination_docs::backup_help_lists_every_supported_scheme",
    "backup_destination_docs::file_and_gs_sinks_are_unconditional_per_supported_schemes",
    "backup_destination_docs::tape_llm_backup_sectioned_topic_conforms",
    "backup_destination_docs::tape_llm_backup_topic_renders_every_supported_scheme_via_render_sectioned",
]


def verify_test_output(output: str):
    passed_tests = []
    for test in EXPECTED_TESTS:
        pattern = rf"test {re.escape(test)}\s+\.\.\.\s+ok"
        if not re.search(pattern, output):
            raise RuntimeError(f"Missing or non-passing test: {test}")
        passed_tests.append(test)

    summary_pattern = r"test result:\s+ok\.\s+6 passed;\s+0 failed;\s+0 ignored"
    if not re.search(summary_pattern, output):
        raise RuntimeError("Summary mismatch: expected 6 passed, 0 failed, 0 ignored")

    return passed_tests


def main():
    head_sha = os.environ.get("HEAD_SHA")
    base_sha = os.environ.get("BASE_SHA")
    try:
        current_head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    except Exception as exc:
        print(f"Error determining git HEAD: {exc}", file=sys.stderr)
        return 1

    if head_sha and current_head != head_sha:
        print(
            f"Error: current git HEAD {current_head} does not match expected product head {head_sha}",
            file=sys.stderr,
        )
        return 1

    print(f"Product HEAD: {current_head} (expected: {head_sha or 'n/a'}), Frozen BASE: {base_sha or 'n/a'}")

    cmd = ["cargo", "test", "--locked", "-p", "tape", "--test", "it", "--", "backup_destination_docs::"]
    print(f"Running command: {' '.join(cmd)}")
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    sys.stdout.write(proc.stdout)
    if proc.returncode != 0:
        print(f"Error: cargo test exited with status {proc.returncode}", file=sys.stderr)
        return proc.returncode

    try:
        passed = verify_test_output(proc.stdout)
        print(f"Verification successful: all {len(passed)} expected backup_destination_docs tests passed.")
        return 0
    except RuntimeError as exc:
        print(f"Verification failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
