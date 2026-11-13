#!/usr/bin/env python3
"""Check that the Rust toolchain is pinned to PINNED everywhere CI uses it.

Checks:
1. rust-toolchain.toml at the repo root: channel == PINNED and components
   include rustfmt and clippy.
2. Every step in .github/workflows/*.yml that uses dtolnay/rust-toolchain
   has its own `toolchain: PINNED` input (a line inside the same step).
"""
import argparse
import re
import sys
import tomllib
from pathlib import Path

PINNED = "1.99.1"
REQUIRED_COMPONENTS = ("rustfmt", "clippy")
ACTION = "dtolnay/rust-toolchain@"


def _indent(line):
    return len(line) - len(line.lstrip(" "))


def _check_toml(root):
    errors = []
    path = root / "rust-toolchain.toml"
    if not path.is_file():
        return ["rust-toolchain.toml: missing"]
    try:
        data = tomllib.loads(path.read_text())
    except tomllib.TOMLDecodeError as exc:
        return [f"rust-toolchain.toml: invalid TOML: {exc}"]
    toolchain = data.get("toolchain", {})
    channel = toolchain.get("channel")
    if channel != PINNED:
        errors.append(
            f"rust-toolchain.toml: channel must be {PINNED!r}, found {channel!r}"
        )
    components = toolchain.get("components", [])
    for name in REQUIRED_COMPONENTS:
        if name not in components:
            errors.append(f"rust-toolchain.toml: components must include {name}")
    return errors


def _step_dash_indent(lines, i):
    """Indent of the `- ` that starts the step containing line i."""
    indent_i = _indent(lines[i])
    for j in range(i, -1, -1):
        stripped = lines[j].lstrip(" ")
        if stripped.startswith("- ") and _indent(lines[j]) <= indent_i:
            return _indent(lines[j])
    return indent_i


def _step_end(lines, i, dash):
    """First line index after i that is not inside the step (dash column)."""
    j = i + 1
    while j < len(lines):
        line = lines[j]
        if line.strip() and _indent(line) <= dash:
            break
        j += 1
    return j


def _toolchain_value(stripped):
    raw = stripped[len("toolchain:"):]
    raw = re.sub(r"\s+#.*$", "", raw).strip()
    return raw.strip("'\"")


def _check_workflow(root, path):
    errors = []
    rel = path.relative_to(root)
    lines = path.read_text().splitlines()
    for i, line in enumerate(lines):
        if ACTION not in line:
            continue
        dash = _step_dash_indent(lines, i)
        end = _step_end(lines, i, dash)
        values = [
            (k, _toolchain_value(lines[k].strip()))
            for k in range(i, end)
            if lines[k].strip().startswith("toolchain:")
        ]
        if not values:
            errors.append(
                f"{rel}:{i + 1}: {ACTION.rstrip('@')} step has no toolchain: {PINNED}"
            )
            continue
        for k, value in values:
            if value != PINNED:
                errors.append(
                    f"{rel}:{k + 1}: toolchain must be {PINNED}, found {value!r}"
                )
    return errors


def check(root):
    """Return a list of error strings; empty when everything is pinned."""
    root = Path(root)
    errors = _check_toml(root)
    workflows = root / ".github" / "workflows"
    if workflows.is_dir():
        for path in sorted(workflows.glob("*.yml")):
            errors.extend(_check_workflow(root, path))
    return errors


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dir", default=".", help="repository root")
    args = parser.parse_args(argv)
    errors = check(Path(args.dir))
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("toolchain pin ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
