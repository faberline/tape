#!/usr/bin/env python3
"""Deterministic validator for runbook copy qualification.

Inspects product-head document bytes as data against the frozen base.
Uses frozen Git trees and complete NUL-delimited inventory with change status,
old/new paths, and mode/type/blob identities.
Zero execution of runbook shell commands. Zero PR self-approval.
"""
import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Dict, List, Optional, Tuple


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


@dataclass
class DiffEntry:
    old_mode: str
    new_mode: str
    old_sha: str
    new_sha: str
    status: str
    src_path: str
    dst_path: Optional[str] = None


class FileLoader:
    """Abstract interface for loading file bytes and metadata."""
    def inventory(self) -> Dict[str, DiffEntry]:
        raise NotImplementedError

    def read_bytes(self, path: str, side: str) -> bytes:
        raise NotImplementedError

    def file_exists(self, path: str, side: str) -> bool:
        raise NotImplementedError

    def get_mode_and_type(self, path: str, side: str) -> Tuple[str, str]:
        raise NotImplementedError


class GitCheckoutLoader(FileLoader):
    """Loads file data and change lists from real Git checkouts and frozen trees."""
    def __init__(self, base_dir: Path, head_dir: Path, base_ref: str, head_ref: str):
        self.base_dir = Path(base_dir).resolve()
        self.head_dir = Path(head_dir).resolve()
        self.base_ref = base_ref
        self.head_ref = head_ref
        self._init_repos()

    def _init_repos(self):
        # Verify repositories exist
        for d, name in ((self.base_dir, "base"), (self.head_dir, "head")):
            res = subprocess.run(
                ["git", "-C", str(d), "rev-parse", "--is-inside-work-tree"],
                capture_output=True,
                text=True,
            )
            if res.returncode != 0:
                raise ValidationError(f"{name} directory is not a valid git repository: {d}")

        # Resolve exact commit SHAs
        try:
            self.base_sha = subprocess.check_output(
                ["git", "-C", str(self.base_dir), "rev-parse", "--verify", f"{self.base_ref}^{{commit}}"],
                stderr=subprocess.PIPE,
                text=True,
            ).strip()
        except subprocess.CalledProcessError as exc:
            raise ValidationError(f"cannot resolve base commit {self.base_ref} in {self.base_dir}: {exc.stderr.strip()}")

        try:
            self.head_sha = subprocess.check_output(
                ["git", "-C", str(self.head_dir), "rev-parse", "--verify", f"{self.head_ref}^{{commit}}"],
                stderr=subprocess.PIPE,
                text=True,
            ).strip()
        except subprocess.CalledProcessError as exc:
            raise ValidationError(f"cannot resolve head commit {self.head_ref} in {self.head_dir}: {exc.stderr.strip()}")

        if not re.fullmatch(r"[0-9a-f]{40}", self.base_sha):
            raise ValidationError(f"invalid base SHA format: {self.base_sha}")
        if not re.fullmatch(r"[0-9a-f]{40}", self.head_sha):
            raise ValidationError(f"invalid head SHA format: {self.head_sha}")

        print(f"Verified base commit: {self.base_sha} ({self.base_dir})")
        print(f"Verified head commit: {self.head_sha} ({self.head_dir})")

        # Set up git environment for cross-directory object access
        self.env = dict(os.environ)
        if self.base_dir != self.head_dir:
            base_objects = subprocess.check_output(
                ["git", "-C", str(self.base_dir), "rev-parse", "--git-path", "objects"],
                text=True,
            ).strip()
            abs_base_objects = str((self.base_dir / base_objects).resolve())
            self.env["GIT_ALTERNATE_OBJECT_DIRECTORIES"] = abs_base_objects

            # Verify head repo can resolve base_sha, fetch if necessary
            check = subprocess.run(
                ["git", "-C", str(self.head_dir), "cat-file", "-e", f"{self.base_sha}^{{commit}}"],
                env=self.env,
                capture_output=True,
            )
            if check.returncode != 0:
                fetch = subprocess.run(
                    ["git", "-C", str(self.head_dir), "fetch", "--quiet", str(self.base_dir), self.base_sha],
                    capture_output=True,
                    text=True,
                )
                if fetch.returncode != 0:
                    raise ValidationError(f"failed to fetch base objects into head repository: {fetch.stderr.strip()}")

    def inventory(self) -> Dict[str, DiffEntry]:
        cmd = [
            "git", "-C", str(self.head_dir),
            "diff-tree", "-r", "-z", "--raw", "-M", "-C",
            self.base_sha, self.head_sha
        ]
        try:
            raw = subprocess.check_output(cmd, env=self.env)
        except subprocess.CalledProcessError as exc:
            raise ValidationError(f"git diff-tree failed: {exc}")

        entries: Dict[str, DiffEntry] = {}
        if not raw:
            return entries

        parts = raw.split(b"\x00")
        i = 0
        while i < len(parts) - 1:
            meta = parts[i].decode("latin1").strip()
            if not meta:
                i += 1
                continue
            if not meta.startswith(":"):
                raise ValidationError(f"malformed diff-tree output: {meta}")
            tokens = meta[1:].split()
            if len(tokens) != 5:
                raise ValidationError(f"invalid diff-tree tokens count: {meta}")
            old_mode, new_mode, old_sha, new_sha, status = tokens
            src_path = parts[i + 1].decode("utf-8")
            if status.startswith("R") or status.startswith("C"):
                dst_path = parts[i + 2].decode("utf-8")
                entry = DiffEntry(old_mode, new_mode, old_sha, new_sha, status, src_path, dst_path)
                entries[dst_path] = entry
                i += 3
            else:
                entry = DiffEntry(old_mode, new_mode, old_sha, new_sha, status, src_path)
                entries[src_path] = entry
                i += 2
        return entries

    def read_bytes(self, path: str, side: str) -> bytes:
        ref = self.base_sha if side == "base" else self.head_sha
        repo = self.base_dir if side == "base" else self.head_dir
        cmd = ["git", "-C", str(repo), "cat-file", "blob", f"{ref}:{path}"]
        try:
            return subprocess.check_output(cmd, env=self.env)
        except subprocess.CalledProcessError as exc:
            raise ValidationError(f"failed to read blob for {path} at {side} ({ref}): {exc}")

    def file_exists(self, path: str, side: str) -> bool:
        ref = self.base_sha if side == "base" else self.head_sha
        repo = self.base_dir if side == "base" else self.head_dir
        cmd = ["git", "-C", str(repo), "cat-file", "-e", f"{ref}:{path}"]
        res = subprocess.run(cmd, env=self.env, capture_output=True)
        return res.returncode == 0

    def get_mode_and_type(self, path: str, side: str) -> Tuple[str, str]:
        ref = self.base_sha if side == "base" else self.head_sha
        repo = self.base_dir if side == "base" else self.head_dir
        cmd = ["git", "-C", str(repo), "ls-tree", "-z", ref, path]
        try:
            out = subprocess.check_output(cmd, env=self.env)
        except subprocess.CalledProcessError as exc:
            raise ValidationError(f"ls-tree failed for {path} at {side}: {exc}")
        if not out:
            raise ValidationError(f"path {path} not found in ls-tree at {side} ({ref})")
        parts = out.split(b"\x00")[0].decode("latin1").split()
        if len(parts) < 3:
            raise ValidationError(f"invalid ls-tree output for {path}: {out}")
        return parts[0], parts[1]


class MemoryLoader(FileLoader):
    """In-memory loader for fixture tests."""
    def __init__(
        self,
        base_files: Dict[str, bytes],
        head_files: Dict[str, bytes],
        modes: Optional[Dict[Tuple[str, str], str]] = None,
        types: Optional[Dict[Tuple[str, str], str]] = None,
        diff_entries: Optional[Dict[str, DiffEntry]] = None,
    ):
        self.base_files = base_files
        self.head_files = head_files
        self.modes = modes or {}
        self.types = types or {}
        self.diff_entries = diff_entries

    def inventory(self) -> Dict[str, DiffEntry]:
        if self.diff_entries is not None:
            return self.diff_entries
        entries: Dict[str, DiffEntry] = {}
        all_paths = sorted(set(self.base_files) | set(self.head_files))
        for p in all_paths:
            b = self.base_files.get(p)
            h = self.head_files.get(p)
            bm = self.modes.get((p, "base"), "100644")
            hm = self.modes.get((p, "head"), "100644")
            if b is None and h is not None:
                entries[p] = DiffEntry("000000", hm, "0" * 40, "1" * 40, "A", p)
            elif b is not None and h is None:
                entries[p] = DiffEntry(bm, "000000", "1" * 40, "0" * 40, "D", p)
            elif b != h or bm != hm:
                entries[p] = DiffEntry(bm, hm, "1" * 40, "2" * 40, "M", p)
        return entries

    def read_bytes(self, path: str, side: str) -> bytes:
        files = self.base_files if side == "base" else self.head_files
        if path not in files:
            raise ValidationError(f"missing {path} on {side}")
        return files[path]

    def file_exists(self, path: str, side: str) -> bool:
        files = self.base_files if side == "base" else self.head_files
        return path in files

    def get_mode_and_type(self, path: str, side: str) -> Tuple[str, str]:
        mode = self.modes.get((path, side), "100644")
        obj_type = self.types.get((path, side), "blob")
        return mode, obj_type


def load_utf8_bytes(loader: FileLoader, path: str, side: str) -> Tuple[bytes, str]:
    raw = loader.read_bytes(path, side)
    if b"\r" in raw:
        raise ValidationError(f"CRLF or CR detected in {path} at {side} (forbidden)")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ValidationError(f"invalid UTF-8 in {path} at {side}: {exc}")
    return raw, text


def validate_docs_copy(loader: FileLoader) -> dict:
    assertions_passed = []

    # 1. Complete PR file inventory contains exactly the three listed document paths
    inv = loader.inventory()
    if not inv:
        raise ValidationError("assertion 1 failed: empty changed files inventory")

    for path, entry in inv.items():
        if entry.status.startswith("R"):
            raise ValidationError(f"assertion 1 failed: forbidden rename detected: {entry.src_path} -> {entry.dst_path}")
        if entry.status.startswith("C"):
            raise ValidationError(f"assertion 1 failed: forbidden copy detected: {entry.src_path} -> {entry.dst_path}")
        if entry.status == "T":
            raise ValidationError(f"assertion 1 failed: forbidden type change detected: {entry.src_path}")
        if entry.old_mode == "120000" or entry.new_mode == "120000":
            raise ValidationError(f"assertion 1 failed: forbidden symlink detected: {entry.src_path}")
        if entry.status == "M" and entry.old_mode != entry.new_mode:
            raise ValidationError(f"assertion 1 failed: forbidden mode change detected: {entry.src_path} ({entry.old_mode} -> {entry.new_mode})")
        if entry.status in ("A", "M") and entry.new_mode != "100644":
            raise ValidationError(f"assertion 1 failed: invalid file mode {entry.new_mode} for {entry.src_path} (expected 100644)")

    changed = sorted(inv.keys())
    unexpected = set(changed) - set(EXPECTED_CHANGES)
    if unexpected:
        raise ValidationError(f"assertion 1 failed: unexpected changed paths: {sorted(unexpected)}")
    missing_expected = set(EXPECTED_CHANGES) - set(changed)
    if missing_expected:
        raise ValidationError(f"assertion 1 failed: missing expected changed paths: {sorted(missing_expected)}")
    if len(changed) != len(EXPECTED_CHANGES):
        raise ValidationError(f"assertion 1 failed: duplicate or invalid inventory length: {len(changed)}")

    for p in EXPECTED_CHANGES:
        mode, obj_type = loader.get_mode_and_type(p, "head")
        if mode == "120000":
            raise ValidationError(f"assertion 1 failed: {p} is a symlink at head (forbidden)")
        if mode != "100644":
            raise ValidationError(f"assertion 1 failed: {p} has invalid file mode {mode} at head (expected 100644)")
        if obj_type != "blob":
            raise ValidationError(f"assertion 1 failed: {p} has invalid type {obj_type} at head (expected blob)")

    assertions_passed.append("1. File inventory contains exactly the three expected document paths with valid modes")

    # 8. Check forbidden files, prefixes, extensions, and basenames
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

    base_handoff_raw, base_handoff = load_utf8_bytes(loader, handoff_path, "base")
    head_handoff_raw, head_handoff = load_utf8_bytes(loader, handoff_path, "head")
    if base_handoff_raw != head_handoff_raw:
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

    if base_handoff.count(sec7_start) > 1 or base_handoff.count(cold_sub) > 1 or base_handoff.count(disk_sub) > 1:
        raise ValidationError("assertion 5 failed: duplicate headings in base handoff")

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
    cold_head_raw, cold_head = load_utf8_bytes(loader, "docs/runbooks/cold-restore.md", "head")
    disk_head_raw, disk_head = load_utf8_bytes(loader, "docs/runbooks/disk-full.md", "head")

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
    base_ops_raw, base_ops = load_utf8_bytes(loader, "docs/operations/README.md", "base")
    head_ops_raw, head_ops = load_utf8_bytes(loader, "docs/operations/README.md", "head")

    new_cold_row = "| [Cold restore runbook](../runbooks/cold-restore.md) | Ad-hoc backups, destination storage schemes, and cold restore via bootstrap-if-empty |\n"
    new_disk_row = "| [Disk-full runbook](../runbooks/disk-full.md) | Degraded read-only mode on ENOSPC, metric diagnosis, and volume recovery |\n"
    combined_new_rows = new_cold_row + new_disk_row
    node_drain_prefix = "| [Node drain and PodDisruptionBudget](../runbooks/node-drain-and-pdb.md) |"

    if combined_new_rows not in head_ops:
        raise ValidationError("assertion 7 failed: expected new runbook rows missing or altered in operations table")

    if head_ops.count(combined_new_rows) != 1:
        raise ValidationError("assertion 7 failed: expected new runbook rows appear multiple times")

    if combined_new_rows + node_drain_prefix not in head_ops:
        raise ValidationError("assertion 7 failed: new runbook rows not placed immediately preceding node-drain runbook")

    # Exact whole-document transform check:
    # After removing EXACTLY the two intended new rows, remaining document must be byte-identical to base!
    reconstructed_ops = head_ops.replace(combined_new_rows, "", 1)
    if reconstructed_ops != base_ops:
        raise ValidationError("assertion 7 failed: docs/operations/README.md contains unintended changes outside the two approved runbook rows")

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
    parser.add_argument("--base-dir", type=Path, help="Base directory for checkout mode")
    parser.add_argument("--head-dir", type=Path, help="Head directory for checkout mode")
    parser.add_argument("--repo-dir", type=Path, default=Path.cwd(), help="Repository root directory")
    args = parser.parse_args()

    base_dir = (args.base_dir or args.repo_dir).resolve()
    head_dir = (args.head_dir or args.repo_dir).resolve()
    base_ref = args.base_ref or "origin/main"
    head_ref = args.head_ref or "HEAD"

    loader = GitCheckoutLoader(base_dir, head_dir, base_ref, head_ref)
    source_summary = f"git: base={base_dir}@{loader.base_sha} head={head_dir}@{loader.head_sha}"

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
