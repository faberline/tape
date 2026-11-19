#!/usr/bin/env python3
"""Check that every faberline/aw gke-test.yml call and aw-ref input names one commit.

Scans .github/workflows/*.yml for:
- `faberline/aw/.github/workflows/gke-test.yml@<ref>` (the `uses:` line), and
- `aw-ref: <ref>` inputs.

Every ref must be a 40-hex commit, and all of them must be the same commit.
Standard library only; needs no network.
"""
import argparse
import re
import sys
from pathlib import Path

USES_RE = re.compile(r"faberline/aw/\.github/workflows/gke-test\.yml@(\S+)")
AW_REF_RE = re.compile(r"^\s*aw-ref:\s*(\S+)")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def check(root):
    root = Path(root)
    errors = []
    found = []  # (name, line number, value), in sorted file order then line order
    for path in sorted((root / ".github" / "workflows").glob("*.yml")):
        for number, line in enumerate(path.read_text().splitlines(), start=1):
            match = USES_RE.search(line) or AW_REF_RE.search(line)
            if match:
                found.append((path.name, number, match.group(1)))

    if not found:
        return ["no faberline/aw gke-test.yml reference found in .github/workflows"]

    sha_matches = []
    for name, number, value in found:
        if SHA_RE.match(value):
            sha_matches.append((name, number, value))
        else:
            errors.append(
                f"{name}:{number}: aw ref must be a 40-hex commit, found '{value}'"
            )

    if sha_matches:
        first_name, first_number, first_value = sha_matches[0]
        for name, number, value in sha_matches[1:]:
            if value != first_value:
                errors.append(
                    f"{name}:{number}: aw ref {value} differs from {first_value} "
                    f"({first_name}:{first_number})"
                )
    return errors


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dir", default=".", help="repository root")
    args = parser.parse_args(argv)
    errors = check(args.dir)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("aw gke-test pin ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
