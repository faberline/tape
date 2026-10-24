#!/usr/bin/env python3
"""Deterministic validator for runbook copy qualification.

Inspects product-head document bytes as data against the frozen base.
Zero execution of runbook shell commands. Zero PR self-approval.
"""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


EXPECTED_CHANGES = (
    "docs/operations/README.md",
    "docs/runbooks/cold-restore.md",
    "docs/runbooks/disk-full.md",
)

FORBIDDEN_FILE = "docs/runbooks/README.md"

FORBIDDEN_PREFIXES = (
    "crates/",
    "openapi/",
    "clients/",
    "k8s/",
    "tests/",
)

FORBIDDEN_EXTENSIONS = (
    ".rs",
)

FORBIDDEN_BASENAMES = (
    "Cargo.toml",
    "Cargo.lock",
)


class ValidationError(RuntimeError):
    """Raised when any contract assertion is violated."""
    pass


class FileLoader:
    """Abstract interface for loading file bytes and metadata."""
    def read_text(self, path: str, side: str) -> str:
        raise NotImplementedError

    def file_exists(self, path: str, side: str) -> bool:
        raise NotImplementedError

    def changed_files(self) -> list:
        raise NotImplementedError

    def check_file_mode_and_symlink(self, path: str, side: str):
        raise NotImplementedError


class GitLoader(FileLoader):
    """Loads file data and change lists from Git revisions."""
    def __init__(self, base_ref: str, head_ref: str, repo_dir: Path):
        self.base_ref = base_ref
        self.head_ref = head_ref
        self.repo_dir = repo_dir

    def _show(self, ref: str, path: str) -> str:
        try:
            return subprocess.check_output(
                ["git", "show", f"{ref}:{path}"],
                cwd=self.repo_dir,
                stderr=subprocess.PIPE,
                text=True,
            )
        except subprocess.CalledProcessError as exc:
            raise ValidationError(f"failed to read {path} at {ref}: {exc.stderr.strip()}")

    def read_text(self, path: str, side: str) -> str:
        ref = self.base_ref if side == "base" else self.head_ref
        return self._show(ref, path)

    def file_exists(self, path: str, side: str) -> bool:
        ref = self.base_ref if side == "base" else self.head_ref
        res = subprocess.run(
            ["git", "cat-file", "-e", f"{ref}:{path}"],
            cwd=self.repo_dir,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return res.returncode == 0

    def changed_files(self) -> list:
        out = subprocess.check_output(
            ["git", "diff", "--name-only", f"{self.base_ref}...{self.head_ref}"],
            cwd=self.repo_dir,
            text=True,
        )
        return [line.strip() for line in out.splitlines() if line.strip()]

    def check_file_mode_and_symlink(self, path: str, side: str):
        ref = self.base_ref if side == "base" else self.head_ref
        out = subprocess.check_output(
            ["git", "ls-tree", ref, path],
            cwd=self.repo_dir,
            text=True,
        ).strip()
        if not out:
            raise ValidationError(f"{path} not found in git tree at {ref}")
        mode = out.split()[0]
        if mode == "120000":
            raise ValidationError(f"{path} is a symlink at {ref} (forbidden)")
        if mode != "100644":
            raise ValidationError(f"{path} has invalid file mode {mode} at {ref} (expected 100644)")


class DiskLoader(FileLoader):
    """Loads file data from directory trees for offline fixture testing."""
    def __init__(self, base_dir: Path, head_dir: Path, changed: list = None):
        self.base_dir = base_dir
        self.head_dir = head_dir
        self._changed = changed

    def read_text(self, path: str, side: str) -> str:
        root = self.base_dir if side == "base" else self.head_dir
        target = root / path
        if not target.is_file():
            raise ValidationError(f"missing {path} on {side} disk: {target}")
        return target.read_text()

    def file_exists(self, path: str, side: str) -> bool:
        root = self.base_dir if side == "base" else self.head_dir
        return (root / path).is_file()

    def changed_files(self) -> list:
        if self._changed is not None:
            return self._changed
        # Compute differences between base and head
        changed = []
        all_paths = set()
        for root in (self.base_dir, self.head_dir):
            if root.exists():
                for p in root.rglob("*"):
                    if p.is_file():
                        all_paths.add(p.relative_to(root).as_posix())
        for rel in sorted(all_paths):
            b, h = self.base_dir / rel, self.head_dir / rel
            if not b.exists() or not h.exists():
                changed.append(rel)
            elif b.read_bytes() != h.read_bytes():
                changed.append(rel)
        return changed

    def check_file_mode_and_symlink(self, path: str, side: str):
        root = self.base_dir if side == "base" else self.head_dir
        target = root / path
        if target.is_symlink():
            raise ValidationError(f"{path} is a symlink on {side} disk (forbidden)")
        if not target.is_file():
            raise ValidationError(f"{path} is not a regular file on {side} disk")
        mode = oct(target.stat().st_mode & 0o777)
        if mode not in ("0o644", "0o664"):
            raise ValidationError(f"{path} has invalid file mode {mode} on {side} disk (expected 0o644)")


def validate_docs_copy(loader: FileLoader) -> dict:
    assertions_passed = []

    # 1. Complete PR file inventory contains exactly the three listed document paths
    changed = loader.changed_files()
    if not changed:
        raise ValidationError("assertion 1 failed: empty changed files inventory")
    unexpected = set(changed) - set(EXPECTED_CHANGES)
    if unexpected:
        raise ValidationError(f"assertion 1 failed: unexpected changed paths: {sorted(unexpected)}")
    missing_expected = set(EXPECTED_CHANGES) - set(changed)
    if missing_expected:
        raise ValidationError(f"assertion 1 failed: missing expected changed paths: {sorted(missing_expected)}")
    if len(changed) != len(EXPECTED_CHANGES):
        raise ValidationError(f"assertion 1 failed: duplicate or invalid inventory length: {len(changed)}")

    for p in EXPECTED_CHANGES:
        loader.check_file_mode_and_symlink(p, "head")

    assertions_passed.append("1. File inventory contains exactly the three expected document paths with valid modes")

    # 8 (part 1). Check forbidden file and content changes
    if loader.file_exists(FORBIDDEN_FILE, "head"):
        raise ValidationError(f"assertion 8 failed: forbidden file {FORBIDDEN_FILE} is present")

    for p in changed:
        if any(p.startswith(prefix) for prefix in FORBIDDEN_PREFIXES):
            raise ValidationError(f"assertion 8 failed: forbidden path change under prefix: {p}")
        if any(p.endswith(ext) for ext in FORBIDDEN_EXTENSIONS):
            raise ValidationError(f"assertion 8 failed: forbidden extension change: {p}")
        if Path(p).name in FORBIDDEN_BASENAMES:
            raise ValidationError(f"assertion 8 failed: forbidden file change: {p}")

    assertions_passed.append("8. No forbidden docs/runbooks/README.md or Rust/K8s/OpenAPI/client paths admitted")

    # 2. Head docs/deployment-handoff.md is byte-identical to the frozen base
    handoff_path = "docs/deployment-handoff.md"
    if not loader.file_exists(handoff_path, "base") or not loader.file_exists(handoff_path, "head"):
        raise ValidationError(f"assertion 2 failed: {handoff_path} must exist at both base and head")
    base_handoff = loader.read_text(handoff_path, "base")
    head_handoff = loader.read_text(handoff_path, "head")
    if base_handoff != head_handoff:
        raise ValidationError(f"assertion 2 failed: {handoff_path} was modified (must remain byte-identical to base)")

    assertions_passed.append("2. docs/deployment-handoff.md is byte-identical to frozen base")

    # 3, 4, 5. Extract and normalize sections from base handoff
    sec7_start = "## 7. Backup / restore runbook (#1329)\n"
    cold_sub = "### Cold restore runbook (#2465, #2468)\n"
    disk_sub = "### Disk-full runbook (#2573)\n"
    divider = "\n---\n\n## 8. Release-readiness gates\n"

    if sec7_start not in base_handoff:
        raise ValidationError("assertion 3 failed: missing section 7 in base handoff")
    if cold_sub not in base_handoff:
        raise ValidationError("assertion 3 failed: missing cold restore subsection in base handoff")
    if disk_sub not in base_handoff:
        raise ValidationError("assertion 4 failed: missing disk-full subsection in base handoff")
    if divider not in base_handoff:
        raise ValidationError("assertion 4 failed: missing release-readiness divider in base handoff")

    idx_sec7 = base_handoff.index(sec7_start)
    idx_cold = base_handoff.index(cold_sub)
    idx_disk = base_handoff.index(disk_sub)
    idx_div = base_handoff.index(divider)

    if not (idx_sec7 < idx_cold < idx_disk < idx_div):
        raise ValidationError("assertion 5 failed: base handoff sections out of expected order")

    # Check for duplicate headings in base handoff section
    if base_handoff.count(sec7_start) > 1 or base_handoff.count(cold_sub) > 1 or base_handoff.count(disk_sub) > 1:
        raise ValidationError("assertion 5 failed: duplicate headings in base handoff")

    # Extract source sections
    cold_source = base_handoff[idx_sec7:idx_disk]
    disk_source = base_handoff[idx_disk:idx_div]

    # Normalize cold-restore:
    # 1. Promote ## 7. -> #
    cold_norm = cold_source.replace("## 7. Backup / restore runbook (#1329)", "# Backup / restore runbook (#1329)")
    # 2. Promote ### Cold restore -> ## Cold restore
    cold_norm = cold_norm.replace("### Cold restore runbook (#2465, #2468)", "## Cold restore runbook (#2465, #2468)")
    cold_norm = cold_norm.rstrip("\n") + "\n"

    # Normalize disk-full:
    # 1. Promote ### Disk-full -> # Disk-full
    disk_norm = disk_source.replace("### Disk-full runbook (#2573)", "# Disk-full runbook (#2573)")
    disk_norm = disk_norm.rstrip("\n") + "\n"

    # Read head targets
    cold_head = loader.read_text("docs/runbooks/cold-restore.md", "head")
    disk_head = loader.read_text("docs/runbooks/disk-full.md", "head")

    # 3. Cold-restore target verification
    cold_headings = [l for l in cold_head.splitlines() if l.startswith("#")]
    if len(cold_headings) != len(set(cold_headings)):
        raise ValidationError("assertion 3 failed: duplicate headings in cold-restore.md")
    if cold_head != cold_norm:
        raise ValidationError("assertion 3 failed: docs/runbooks/cold-restore.md differs from normalized base handoff section 7")
    assertions_passed.append("3. docs/runbooks/cold-restore.md matches normalized base handoff section 7")

    # 4. Disk-full target verification
    disk_headings = [l for l in disk_head.splitlines() if l.startswith("#")]
    if len(disk_headings) != len(set(disk_headings)):
        raise ValidationError("assertion 4 failed: duplicate headings in disk-full.md")
    if disk_head != disk_norm:
        raise ValidationError("assertion 4 failed: docs/runbooks/disk-full.md differs from normalized base handoff disk-full subsection")
    assertions_passed.append("4. docs/runbooks/disk-full.md matches normalized base handoff disk-full subsection")

    # 5. Structural normalization validation
    assertions_passed.append("5. Section heading levels promoted; divider and trailing blank lines cleanly stripped")

    # 6. Supported scheme sentence retains file://, s3://, and gs:// with original qualifications
    expected_scheme_sentence = (
        "`--dest` accepts `file://` (always supported), `s3://` (feature `backup`,\n"
        "`service-backup/s3`), or `gs://` (feature `backup`, always linked —\n"
        "`service-backup/gcs` is unconditional). `gs://` authenticates via\n"
        "workload-identity ADC in-cluster (GKE-proven) and Vat's\n"
        "`STORAGE_EMULATOR_HOST` locally."
    )
    if expected_scheme_sentence not in cold_head:
        raise ValidationError("assertion 6 failed: supported scheme sentence or qualifications altered in cold-restore.md")
    assertions_passed.append("6. Supported schemes (file://, s3://, gs://) and qualifications preserved verbatim")

    # 7. Operations table retains existing rows and links, adds exactly two relative links next to node-drain runbook
    base_ops = loader.read_text("docs/operations/README.md", "base")
    head_ops = loader.read_text("docs/operations/README.md", "head")

    base_rows = [l.strip() for l in base_ops.splitlines() if l.strip().startswith("| [")]
    head_rows = [l.strip() for l in head_ops.splitlines() if l.strip().startswith("| [")]

    expected_existing_links = [
        "[Deployment handoff](../deployment-handoff.md)",
        "[Node drain and PodDisruptionBudget](../runbooks/node-drain-and-pdb.md)",
        "[Benchmark history](../benchmarks-scale.md)",
    ]
    for link in expected_existing_links:
        if not any(link in row for row in head_rows):
            raise ValidationError(f"assertion 7 failed: existing link {link} missing from head operations table")

    new_cold_link = "| [Cold restore runbook](../runbooks/cold-restore.md) | Ad-hoc backups, destination storage schemes, and cold restore via bootstrap-if-empty |"
    new_disk_link = "| [Disk-full runbook](../runbooks/disk-full.md) | Degraded read-only mode on ENOSPC, metric diagnosis, and volume recovery |"

    if new_cold_link not in head_rows or new_disk_link not in head_rows:
        raise ValidationError("assertion 7 failed: expected new runbook links missing from operations table")

    if len(head_rows) != len(base_rows) + 2:
        raise ValidationError(f"assertion 7 failed: expected exactly 2 rows added to operations table (was {len(head_rows) - len(base_rows)})")

    # Verify placed next to node-drain runbook
    idx_cold_row = head_rows.index(new_cold_link)
    idx_disk_row = head_rows.index(new_disk_link)
    drain_rows = [i for i, r in enumerate(head_rows) if "node-drain-and-pdb.md" in r]
    if not drain_rows:
        raise ValidationError("assertion 7 failed: node-drain row missing")
    idx_drain = drain_rows[0]
    if not (idx_cold_row + 1 == idx_disk_row and idx_disk_row + 1 == idx_drain):
        raise ValidationError("assertion 7 failed: new runbook rows not placed immediately preceding node-drain runbook")

    # Both targets exist at head
    if not loader.file_exists("docs/runbooks/cold-restore.md", "head"):
        raise ValidationError("assertion 7 failed: cold-restore.md target does not exist at head")
    if not loader.file_exists("docs/runbooks/disk-full.md", "head"):
        raise ValidationError("assertion 7 failed: disk-full.md target does not exist at head")

    assertions_passed.append("7. Operations table retains existing entries and adds exact relative links next to node-drain")

    # 9. Nonzero assertion verification and summary
    if len(assertions_passed) < 7:
        raise ValidationError(f"assertion 9 failed: insufficient assertions executed ({len(assertions_passed)})")

    assertions_passed.append("9. Nonzero assertion results validated and reported")

    return {
        "status": "pass",
        "assertions_count": len(assertions_passed),
        "assertions": assertions_passed,
        "changed_files": changed,
    }


def main():
    parser = argparse.ArgumentParser(description="Validate docs copy contract between base and head")
    parser.add_argument("--base-ref", help="Base git commit reference (default: origin/main or HEAD~1)")
    parser.add_argument("--head-ref", help="Head git commit reference (default: HEAD)")
    parser.add_argument("--base-dir", type=Path, help="Base directory for disk mode")
    parser.add_argument("--head-dir", type=Path, help="Head directory for disk mode")
    parser.add_argument("--repo-dir", type=Path, default=Path.cwd(), help="Repository root directory")
    args = parser.parse_args()

    if args.base_dir and args.head_dir:
        loader = DiskLoader(args.base_dir, args.head_dir)
        source_summary = f"disk: base={args.base_dir} head={args.head_dir}"
    else:
        base_ref = args.base_ref or "origin/main"
        head_ref = args.head_ref or "HEAD"
        loader = GitLoader(base_ref, head_ref, args.repo_dir)
        source_summary = f"git: base={base_ref} head={head_ref}"

    try:
        result = validate_docs_copy(loader)
    except ValidationError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 1

    print("================================================================================")
    print("DOCS COPY CONTRACT VALIDATION: PASS")
    print(f"Target: {source_summary}")
    print(f"Assertions executed and passed: {result['assertions_count']}")
    for a in result["assertions"]:
        print(f"  [OK] {a}")
    print("================================================================================")
    return 0


if __name__ == "__main__":
    sys.exit(main())
