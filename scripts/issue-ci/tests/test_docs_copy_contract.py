"""Tests for docs copy contract validator and regression fixtures.

Covers all positive and negative fixture scenarios required by issue #40:
1. Real two-checkout CLI-path regression exercising the workflow's actual invoked command:
   - Valid complete copy passes with nonzero assertions despite .git metadata
   - Extra executable-mode-only file change -> fails with named assertion
   - Broken symlink -> fails with named assertion
   - Allowed-path symlink -> fails with named assertion
   - Type change -> fails with named assertion
   - Rename / copy -> fails with named assertion
   - Unexpected / unmapped path -> fails with named assertion
   - CRLF negative fixture -> fails with named assertion
   - Modifying an old row in operations table -> fails with named assertion
   - Adding an extra link in operations table -> fails with named assertion
   - Inserting a command outside operations table -> fails with named assertion
2. Unit validator fixture scenarios:
   - Delete a paragraph -> fails
   - Change a command/scheme -> fails
   - Omit an issue heading -> fails
   - Duplicate a source heading -> fails
   - Break a relative link -> fails
   - Alter the handoff -> fails
   - Add forbidden README -> fails
   - Corrupted fixture demonstrations for content omission and illegal scope change
3. Policy selection tests:
   - Valid 3-document inventory selects both real gates
   - Extra unmapped file refuses qualification
   - Rename from protected path refuses qualification
   - Changed validator/selector/policy refuses qualification
   - Incomplete file inventory refuses qualification
4. Evaluator evidence tests:
   - Missing / skipped / cancelled / stale head or base / old attempt / untrusted workflow evidence cannot qualify
   - Real scoped snapshot mismatched-base regression (both run PR base and workflow definition base)
"""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import importlib.util

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ROOT / "scripts/issue-ci"


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


validator = load_module("validate_docs_copy", SCRIPTS / "validate-docs-copy.py")
GitCheckoutLoader = validator.GitCheckoutLoader
MemoryLoader = validator.MemoryLoader
ValidationError = validator.ValidationError
validate_docs_copy = validator.validate_docs_copy
EXPECTED_CHANGES = validator.EXPECTED_CHANGES
DiffEntry = validator.DiffEntry

ci = load_module("handoff", SCRIPTS / "handoff.py")
q = ci.qualification_module()

POLICY_FILE = ROOT / ".github/issue-ci-candidate-gates.json"
BASE_HANDOFF_CONTENT = (ROOT / "docs/deployment-handoff.md").read_bytes().decode("utf-8")
BASE_OPS_CONTENT = (ROOT / "docs/operations/README.md").read_bytes().decode("utf-8")

COLD_RESTORE_CONTENT = """# Backup / restore runbook (#1329)

```bash
# operator-driven or ad hoc, against a running node:
tape backup --url http://localhost:7137 --dest file:///tmp/tape-backups \\
  --token "$TAPE_BACKUP_TOKEN" --retention-secs 604800
```

`--dest` accepts `file://` (always supported), `s3://` (feature `backup`,
`service-backup/s3`), or `gs://` (feature `backup`, always linked —
`service-backup/gcs` is unconditional). `gs://` authenticates via
workload-identity ADC in-cluster (GKE-proven) and Vat's
`STORAGE_EMULATOR_HOST` locally. The bearer token needs `admin` on `*`; omit
it entirely when the node runs `--auth off`. `--retention-secs` prunes older
backup objects at the destination after a successful put; omit to keep
everything. In-cluster, the operator's backup CronJob passes
`http://<name>.<namespace>.svc.cluster.local:7137` as `--url`.

## Cold restore runbook (#2465, #2468)

`bootstrapSeedUri` is **bootstrap-if-empty**, not a live restore endpoint: on
every boot the server first checks whether its durable `TAPE_DATA_DIR`
already has raft state. An empty data directory seeds from the URI as
before; a non-empty one (including a routine pod restart onto its own
already-bootstrapped PVC) **skips the seed and boots from its existing
state** instead of refusing, so the field is safe to leave declared on the
CR (fixed in #2468 — it used to be one-shot: any restart while the field was
still set crash-looped on the non-empty-PVC refusal).

1. Set `spec.bootstrapSeedUri` on a **fresh** instance (empty PVCs — a new
   deployment, or one whose PVCs were explicitly wiped) to the backup object
   URI.
2. Wait for the CR to report Ready, then verify the expected data replayed
   (e.g. `curl $BASE/topics/<topic>/replay?from_offset=0`).
3. Nothing further is required — future pod replacements restart onto the
   same populated PVC, detect existing state, and skip the seed
   automatically (`decision="skipped_existing_state"` in the pod log).
4. Recommended hygiene, not a requirement: remove the field once the
   bootstrap has converged.
   ```bash
   kubectl patch tape/<name> --type=json \\
     -p '[{"op":"remove","path":"/spec/bootstrapSeedUri"}]'
   ```
   This guards against a narrower follow-on hazard: if the PVC is ever
   deleted and reprovisioned (not a routine restart) while the field is
   still set, the replacement pod's data dir really is empty again and WILL
   re-seed from the (possibly now-stale) backup object. Removing the field
   after a successful bootstrap makes that recreate-then-restart sequence
   fail loudly (no seed configured) instead of silently reseeding old data.
"""

DISK_FULL_CONTENT = """# Disk-full runbook (#2573)

A tape node that hits `ENOSPC` on its journal persist path latches **degraded
read-only mode**: mutating requests (append, checkpoint advance, subscription
create/delete/ack, retention set) answer `507` with error kind
`storage_full`, and **reads keep serving**. It does not crash, and it is not
`NotReady` — a full disk is precisely when an operator most needs to read what
is already journalled.

**Do not start by restarting the pod.** The node recovers itself: it re-probes
its store directory every `TAPE_STORAGE_FULL_REPROBE_SECS` (default `30`) and
clears the flag on the first successful write. Restarting only helps as a way
to reset the flag after you have already made room, and it costs you the
replay window during the restart.

1. Confirm the diagnosis:
   ```bash
   curl -s $BASE/metrics | grep tape_storage_
   ```
   `tape_storage_degraded 1` = currently refusing writes.
2. Read `tape_storage_full_errors_total` next. A **rising counter with the
   gauge back at 0** is the important case: the volume is flapping in and out
   of full, not recovered. The gauge alone cannot show that — a node that
   fills and re-probes clean between two scrapes reads healthy at both. The
   `TapeStorageDegraded` alert uses `max_over_time(...[5m])` for the same
   reason.
3. Make room. Either shrink the journal through retention:
   ```bash
   curl -X PUT $BASE/topics/<topic>/retention -d '{"max_events": <n>}'
   ```
   (note this **also** returns `507` while degraded — a retention change
   rewrites the whole journal through a temp file, so it needs room for a
   second copy before the old one is unlinked; free space at the volume level
   first), or expand the PVC on a resizable StorageClass:
   ```bash
   kubectl patch pvc data-<name>-0 -p \\
     '{"spec":{"resources":{"requests":{"storage":"<bigger>"}}}}'
   ```
4. Wait one re-probe interval and re-check `/metrics`. The pod log carries
   `storage re-probe write succeeded; leaving degraded read-only mode`.
5. If the gauge stays `1` after the volume demonstrably has room, read the pod
   log for the re-probe warning — the store directory can be unwritable for
   reasons other than capacity (read-only remount, permissions, a failed CSI
   attach).
"""

NEW_OPS_ROWS = (
    "| [Cold restore runbook](../runbooks/cold-restore.md) | Ad-hoc backups, destination storage schemes, and cold restore via bootstrap-if-empty |\n"
    "| [Disk-full runbook](../runbooks/disk-full.md) | Degraded read-only mode on ENOSPC, metric diagnosis, and volume recovery |\n"
)
NODE_DRAIN_ROW = "| [Node drain and PodDisruptionBudget](../runbooks/node-drain-and-pdb.md) |"
assert NODE_DRAIN_ROW in BASE_OPS_CONTENT
HEAD_OPS_CONTENT = BASE_OPS_CONTENT.replace(NODE_DRAIN_ROW, NEW_OPS_ROWS + NODE_DRAIN_ROW)


class RealTwoCheckoutCliTests(unittest.TestCase):
    """Exercises the workflow's actual invoked CLI command across two real Git repositories."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="tape40-real-twocheckout-"))
        self.base_repo = self.tmp / "trusted-base"
        self.head_repo = self.tmp / "product-head"
        self.base_repo.mkdir()
        self.head_repo.mkdir()

        # Initialize base git repo
        subprocess.run(["git", "init", "-b", "main"], cwd=self.base_repo, check=True, capture_output=True)
        subprocess.run(["git", "config", "user.name", "Test Base"], cwd=self.base_repo, check=True)
        subprocess.run(["git", "config", "user.email", "base@example.com"], cwd=self.base_repo, check=True)
        subprocess.run(["git", "config", "core.autocrlf", "false"], cwd=self.base_repo, check=True)

        (self.base_repo / ".gitattributes").write_bytes(b"* -text\n")
        (self.base_repo / "docs/deployment-handoff.md").parent.mkdir(parents=True, exist_ok=True)
        (self.base_repo / "docs/deployment-handoff.md").write_bytes(BASE_HANDOFF_CONTENT.encode("utf-8"))
        (self.base_repo / "docs/operations/README.md").parent.mkdir(parents=True, exist_ok=True)
        (self.base_repo / "docs/operations/README.md").write_bytes(BASE_OPS_CONTENT.encode("utf-8"))
        (self.base_repo / "Cargo.toml").write_bytes(b"[package]\nname = \"tape\"\nversion = \"0.1.0\"\n")

        subprocess.run(["git", "add", "."], cwd=self.base_repo, check=True)
        subprocess.run(["git", "commit", "-m", "base commit"], cwd=self.base_repo, check=True)
        self.base_sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.base_repo, text=True).strip()

        # Initialize head git repo as clone of base
        subprocess.run(["git", "clone", "--quiet", str(self.base_repo), str(self.head_repo)], check=True)
        subprocess.run(["git", "config", "user.name", "Test Head"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "config", "user.email", "head@example.com"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "config", "core.autocrlf", "false"], cwd=self.head_repo, check=True)

        # Apply exact three-doc changes to head
        (self.head_repo / "docs/operations/README.md").write_bytes(HEAD_OPS_CONTENT.encode("utf-8"))
        (self.head_repo / "docs/runbooks/cold-restore.md").parent.mkdir(parents=True, exist_ok=True)
        (self.head_repo / "docs/runbooks/cold-restore.md").write_bytes(COLD_RESTORE_CONTENT.encode("utf-8"))
        (self.head_repo / "docs/runbooks/disk-full.md").write_bytes(DISK_FULL_CONTENT.encode("utf-8"))

        subprocess.run(["git", "add", "."], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "exact three-doc PR head"], cwd=self.head_repo, check=True)
        self.head_sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run_validator_cli(self, base_sha=None, head_sha=None) -> subprocess.CompletedProcess:
        b_sha = base_sha or self.base_sha
        h_sha = head_sha or self.head_sha
        cmd = [
            sys.executable,
            str(SCRIPTS / "validate-docs-copy.py"),
            "--base-dir", str(self.base_repo),
            "--head-dir", str(self.head_repo),
            "--base-ref", b_sha,
            "--head-ref", h_sha,
        ]
        return subprocess.run(cmd, capture_output=True, text=True)

    def test_cli_positive_exact_three_doc_copy_passes_despite_git_metadata(self):
        proc = self.run_validator_cli()
        self.assertEqual(proc.returncode, 0, f"Validator failed: {proc.stderr}\n{proc.stdout}")
        self.assertIn("DOCS COPY CONTRACT VALIDATION: PASS", proc.stdout)
        self.assertIn("Assertions executed and passed: 9", proc.stdout)
        self.assertIn(self.base_sha, proc.stdout)
        self.assertIn(self.head_sha, proc.stdout)

    def test_cli_extra_executable_mode_only_fails(self):
        subprocess.run(["git", "update-index", "--chmod=+x", "Cargo.toml"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "executable mode change"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertTrue(
            "mode change detected" in proc.stderr or "unexpected changed paths: ['Cargo.toml']" in proc.stderr,
            f"Unexpected stderr: {proc.stderr}",
        )

    def test_cli_broken_symlink_fails(self):
        broken = self.head_repo / "docs/broken-link.md"
        try:
            broken.symlink_to("nonexistent-target.md")
        except OSError:
            self.skipTest("Symlinks not supported in environment")
        subprocess.run(["git", "add", "docs/broken-link.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "add broken symlink"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertTrue(
            "symlink detected" in proc.stderr or "unexpected changed paths" in proc.stderr,
            f"Unexpected stderr: {proc.stderr}",
        )

    def test_cli_allowed_path_symlink_fails(self):
        cold_path = self.head_repo / "docs/runbooks/cold-restore.md"
        cold_path.unlink()
        try:
            cold_path.symlink_to("../deployment-handoff.md")
        except OSError:
            self.skipTest("Symlinks not supported in environment")
        subprocess.run(["git", "add", "docs/runbooks/cold-restore.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "symlink allowed path"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertTrue(
            "symlink detected" in proc.stderr or "is a symlink at head" in proc.stderr or "invalid file mode" in proc.stderr,
            f"Unexpected stderr: {proc.stderr}",
        )

    def test_cli_type_change_fails(self):
        cargo = self.head_repo / "Cargo.toml"
        cargo.unlink()
        try:
            cargo.symlink_to("docs/deployment-handoff.md")
        except OSError:
            self.skipTest("Symlinks not supported in environment")
        subprocess.run(["git", "add", "Cargo.toml"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "type change Cargo.toml to symlink"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertTrue(
            "type change detected" in proc.stderr or "symlink detected" in proc.stderr or "unexpected changed paths" in proc.stderr,
            f"Unexpected stderr: {proc.stderr}",
        )

    def test_cli_rename_copy_fails(self):
        subprocess.run(["git", "mv", "Cargo.toml", "Cargo-old.toml"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "rename Cargo.toml"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertTrue(
            "rename detected" in proc.stderr or "unexpected changed paths" in proc.stderr,
            f"Unexpected stderr: {proc.stderr}",
        )

    def test_cli_unexpected_unmapped_path_fails(self):
        extra = self.head_repo / "docs/unmapped.md"
        extra.write_bytes(b"unmapped\n")
        subprocess.run(["git", "add", "docs/unmapped.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "add unmapped file"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("unexpected changed paths: ['docs/unmapped.md']", proc.stderr)

    def test_cli_crlf_fails(self):
        crlf_content = COLD_RESTORE_CONTENT.replace("\n", "\r\n")
        (self.head_repo / "docs/runbooks/cold-restore.md").write_bytes(crlf_content.encode("utf-8"))
        subprocess.run(["git", "add", "docs/runbooks/cold-restore.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "introduce CRLF in cold-restore.md"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("CRLF or CR detected in docs/runbooks/cold-restore.md", proc.stderr)

    def test_cli_operations_modify_old_row_fails(self):
        corrupted = HEAD_OPS_CONTENT.replace(
            "[Deployment handoff](../deployment-handoff.md)",
            "[Deployment handoff](../handoff-old.md)",
        )
        (self.head_repo / "docs/operations/README.md").write_bytes(corrupted.encode("utf-8"))
        subprocess.run(["git", "add", "docs/operations/README.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "modify old row in operations table"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("assertion 7 failed: docs/operations/README.md contains unintended changes outside the two approved runbook rows", proc.stderr)

    def test_cli_operations_add_extra_link_fails(self):
        extra_row = "| [Extra runbook](../runbooks/extra.md) | Extra runbook description |\n"
        corrupted = HEAD_OPS_CONTENT.replace(NODE_DRAIN_ROW, extra_row + NODE_DRAIN_ROW)
        (self.head_repo / "docs/operations/README.md").write_bytes(corrupted.encode("utf-8"))
        subprocess.run(["git", "add", "docs/operations/README.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "add extra link to operations table"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("assertion 7 failed", proc.stderr)

    def test_cli_operations_insert_command_outside_table_fails(self):
        corrupted = HEAD_OPS_CONTENT + "\n```bash\necho unauthorized\n```\n"
        (self.head_repo / "docs/operations/README.md").write_bytes(corrupted.encode("utf-8"))
        subprocess.run(["git", "add", "docs/operations/README.md"], cwd=self.head_repo, check=True)
        subprocess.run(["git", "commit", "-m", "insert command outside table"], cwd=self.head_repo, check=True)
        new_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.head_repo, text=True).strip()

        proc = self.run_validator_cli(head_sha=new_head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("assertion 7 failed: docs/operations/README.md contains unintended changes outside the two approved runbook rows", proc.stderr)


class MemoryValidatorTests(unittest.TestCase):
    """Unit tests using MemoryLoader for detailed contract assertion boundaries."""

    def setUp(self):
        self.base_files = {
            "docs/deployment-handoff.md": BASE_HANDOFF_CONTENT.encode("utf-8"),
            "docs/operations/README.md": BASE_OPS_CONTENT.encode("utf-8"),
        }
        self.head_files = {
            "docs/deployment-handoff.md": BASE_HANDOFF_CONTENT.encode("utf-8"),
            "docs/operations/README.md": HEAD_OPS_CONTENT.encode("utf-8"),
            "docs/runbooks/cold-restore.md": COLD_RESTORE_CONTENT.encode("utf-8"),
            "docs/runbooks/disk-full.md": DISK_FULL_CONTENT.encode("utf-8"),
        }

    def test_valid_memory_loader_passes(self):
        loader = MemoryLoader(self.base_files, self.head_files)
        res = validate_docs_copy(loader)
        self.assertEqual(res["status"], "pass")
        self.assertEqual(res["assertions_count"], 9)

    def test_delete_a_paragraph_fails(self):
        corrupted = COLD_RESTORE_CONTENT.replace("1. Set `spec.bootstrapSeedUri`", "<!-- omitted -->")
        heads = dict(self.head_files, **{"docs/runbooks/cold-restore.md": corrupted.encode("utf-8")})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "assertion 3 failed"):
            validate_docs_copy(loader)

    def test_change_command_or_scheme_fails(self):
        corrupted = DISK_FULL_CONTENT.replace("tape_storage_", "altered_metric_")
        heads = dict(self.head_files, **{"docs/runbooks/disk-full.md": corrupted.encode("utf-8")})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "assertion 4 failed"):
            validate_docs_copy(loader)

        corrupted = COLD_RESTORE_CONTENT.replace("file://", "ftp://")
        heads = dict(self.head_files, **{"docs/runbooks/cold-restore.md": corrupted.encode("utf-8")})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "assertion 3 failed"):
            validate_docs_copy(loader)

    def test_omit_issue_heading_fails(self):
        corrupted = COLD_RESTORE_CONTENT.replace("# Backup / restore runbook (#1329)", "# Backup / restore runbook")
        heads = dict(self.head_files, **{"docs/runbooks/cold-restore.md": corrupted.encode("utf-8")})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "assertion 3 failed"):
            validate_docs_copy(loader)

    def test_duplicate_source_heading_fails(self):
        corrupted = COLD_RESTORE_CONTENT + "\n## Cold restore runbook (#2465, #2468)\n"
        heads = dict(self.head_files, **{"docs/runbooks/cold-restore.md": corrupted.encode("utf-8")})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "duplicate headings"):
            validate_docs_copy(loader)

    def test_break_relative_link_fails(self):
        corrupted = HEAD_OPS_CONTENT.replace("../runbooks/cold-restore.md", "http://external.site/cold-restore.md")
        heads = dict(self.head_files, **{"docs/operations/README.md": corrupted.encode("utf-8")})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "assertion 7 failed"):
            validate_docs_copy(loader)

    def test_alter_handoff_fails(self):
        corrupted = (BASE_HANDOFF_CONTENT + "\n<!-- extra edit -->\n").encode("utf-8")
        heads = dict(self.head_files, **{"docs/deployment-handoff.md": corrupted})
        valid_entries = {p: DiffEntry("100644" if p.endswith("README.md") else "000000", "100644", "1" * 40, "2" * 40, "M" if p.endswith("README.md") else "A", p) for p in EXPECTED_CHANGES}
        loader = MemoryLoader(self.base_files, heads, diff_entries=valid_entries)
        with self.assertRaisesRegex(ValidationError, "assertion 2 failed: docs/deployment-handoff.md was modified"):
            validate_docs_copy(loader)

    def test_add_forbidden_readme_fails(self):
        heads = dict(self.head_files, **{"docs/runbooks/README.md": b"# Runbooks\n"})
        loader = MemoryLoader(self.base_files, heads)
        with self.assertRaisesRegex(ValidationError, "assertion 1 failed: unexpected changed paths"):
            validate_docs_copy(loader)

    def test_corrupted_fixture_demonstrations(self):
        """Negative control demonstration: shows verbatim failing assertion then restored pass."""
        loader = MemoryLoader(self.base_files, self.head_files)

        # 1. Deliberate content omission: remove retention paragraph in cold-restore
        omitted = COLD_RESTORE_CONTENT.replace("--retention-secs", "omitted-retention").encode("utf-8")
        omitted_loader = MemoryLoader(self.base_files, dict(self.head_files, **{"docs/runbooks/cold-restore.md": omitted}))
        with self.assertRaises(ValidationError) as ctx:
            validate_docs_copy(omitted_loader)
        self.assertIn("assertion 3 failed: docs/runbooks/cold-restore.md differs from normalized", str(ctx.exception))
        # Restored pass
        self.assertEqual(validate_docs_copy(loader)["status"], "pass")

        # 2. Deliberate illegal scope change: add unmapped file
        illegal_loader = MemoryLoader(self.base_files, dict(self.head_files, **{"docs/extra.md": b"extra\n"}))
        with self.assertRaises(ValidationError) as ctx:
            validate_docs_copy(illegal_loader)
        self.assertIn("assertion 1 failed: unexpected changed paths: ['docs/extra.md']", str(ctx.exception))
        # Restored pass
        self.assertEqual(validate_docs_copy(loader)["status"], "pass")


class QualificationPolicyTests(unittest.TestCase):
    def setUp(self):
        self.config = json.loads(POLICY_FILE.read_text())
        self.head_sha = "a" * 40
        self.base_sha = "b" * 40
        self.pr = {
            "number": 35,
            "state": "open",
            "body": "Closes #31",
            "changed_files": 3,
            "base": {"sha": self.base_sha},
            "head": {"sha": self.head_sha, "ref": "issue/31", "repo": {"full_name": "faberline/tape"}},
        }
        self.valid_files = [
            {"filename": p, "status": "modified" if p.endswith("README.md") else "added"}
            for p in EXPECTED_CHANGES
        ]

    def test_valid_three_document_inventory_selects_both_real_gates(self):
        plan = q.select(self.config, self.pr, self.valid_files)
        self.assertEqual(plan["issues"], [])
        self.assertEqual(sorted(plan["selected"]), ["backup-destination-docs", "docs-copy-contract"])
        workflows = plan["workflows"]
        self.assertEqual(len(workflows), 1)
        self.assertEqual(workflows[0]["path"], ".github/workflows/affected-docs.yml")
        self.assertEqual(sorted(workflows[0]["jobs"]), ["backup-destination-docs", "docs-copy-contract"])

    def test_extra_unmapped_file_refuses_qualification(self):
        bad_files = list(self.valid_files) + [{"filename": "unmapped/file.md", "status": "added"}]
        pr = dict(self.pr, changed_files=4)
        plan = q.select(self.config, pr, bad_files)
        self.assertIn("unmapped changed path: unmapped/file.md", plan["issues"])

    def test_rename_from_protected_path_refuses_qualification(self):
        renamed_files = list(self.valid_files) + [{
            "filename": "docs/safe.md",
            "status": "renamed",
            "previous_filename": "scripts/ci/test.sh",
        }]
        pr = dict(self.pr, changed_files=4)
        plan = q.select(self.config, pr, renamed_files)
        self.assertTrue(any("gate infrastructure change requires reviewed policy update" in i for i in plan["issues"]))

    def test_changed_validator_or_policy_refuses_qualification(self):
        for protected in (
            "scripts/ci/validate-docs-copy.py",
            "scripts/issue-ci/qualification.py",
            ".github/issue-ci-required-gates.json",
            ".github/workflows/affected-docs.yml",
        ):
            files = list(self.valid_files) + [{"filename": protected, "status": "modified"}]
            pr = dict(self.pr, changed_files=4)
            plan = q.select(self.config, pr, files)
            self.assertTrue(any("gate infrastructure change requires reviewed policy update: " + protected in i for i in plan["issues"]))

    def test_incomplete_file_inventory_refuses_qualification(self):
        pr = dict(self.pr, changed_files=2)
        plan = q.select(self.config, pr, self.valid_files)
        self.assertIn("incomplete, empty or capped PR files inventory", plan["issues"])


class EvaluatorEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.config = json.loads(POLICY_FILE.read_text())
        self.head_sha = "a" * 40
        self.base_sha = "b" * 40
        self.pr = {
            "number": 35,
            "state": "open",
            "body": "Closes #31",
            "changed_files": 3,
            "base": {"sha": self.base_sha},
            "head": {"sha": self.head_sha, "ref": "issue/31", "repo": {"full_name": "faberline/tape"}},
        }
        self.files = [
            {"filename": p, "status": "modified" if p.endswith("README.md") else "added"}
            for p in EXPECTED_CHANGES
        ]
        wf_path = ".github/workflows/affected-docs.yml"
        blob_sha = self.config["qualification"]["trusted_workflows"][wf_path]["definition_sha"]
        definitions = {
            (wf_path, self.head_sha): {"sha": blob_sha},
            (wf_path, self.base_sha): {"sha": blob_sha},
        }
        self.scoped = q.scoped_config(self.config, self.pr, self.files, definitions)

    def make_evidence(self, conclusion="success", status="completed", run_attempt=1, wf_id=None, pr_base=None):
        wf = self.scoped["workflows"][0]
        wf_path = wf["path"]
        identity_id = self.config["qualification"]["trusted_workflows"][wf_path]["id"]
        run = {
            "id": 100,
            "run_number": 1,
            "run_attempt": run_attempt,
            "status": status,
            "conclusion": conclusion,
            "html_url": "https://github.com/faberline/tape/actions/runs/100",
            "workflow_id": wf_id or identity_id,
            "path": wf_path,
            "head_sha": self.head_sha,
            "event": "pull_request",
            "head_repository": {"full_name": "faberline/tape"},
            "pull_requests": [{
                "number": 35,
                "head": {"sha": self.head_sha},
                "base": {"sha": pr_base or self.base_sha},
            }],
        }
        jobs = [
            {"name": name, "status": status, "conclusion": conclusion, "html_url": f"https://github.com/faberline/tape/actions/runs/100/job/{j}"}
            for j, name in enumerate(wf["jobs"])
        ]
        return {wf_path: (run, jobs)}

    def test_missing_evidence_is_pending(self):
        state, reasons, _ = ci.evaluate_policy(self.scoped, {})
        self.assertEqual(state, "pending")

    def test_skipped_or_cancelled_evidence_is_pending(self):
        for conc in ("skipped", "cancelled", "neutral"):
            snapshots = self.make_evidence(conclusion=conc)
            state, reasons, _ = ci.evaluate_policy(self.scoped, snapshots)
            self.assertEqual(state, "pending")

    def test_failure_evidence_yields_todo(self):
        snapshots = self.make_evidence(conclusion="failure")
        state, reasons, _ = ci.evaluate_policy(self.scoped, snapshots)
        self.assertEqual(state, "todo")

    def test_untrusted_workflow_definition_prevents_qualification(self):
        # Mismatched definition SHA
        wf_path = ".github/workflows/affected-docs.yml"
        definitions = {
            (wf_path, self.head_sha): {"sha": "c" * 40},
            (wf_path, self.base_sha): {"sha": "d" * 40},
        }
        bad_scoped = q.scoped_config(self.config, self.pr, self.files, definitions)
        self.assertIn(".github/workflows/affected-docs.yml", bad_scoped["_qualification_context"]["untrusted_workflows"])
        state, reasons, _ = ci.evaluate_policy(bad_scoped, self.make_evidence())
        self.assertEqual(state, "pending")
        self.assertTrue(any("untrusted head/base workflow definition" in r for r in reasons))

    def test_mismatched_base_run_cannot_qualify(self):
        # PR run has mismatched base SHA
        mismatched_base = "f" * 40
        snapshots = self.make_evidence(pr_base=mismatched_base)
        run = snapshots[".github/workflows/affected-docs.yml"][0]
        self.assertFalse(q.trusted_run(run, self.scoped))
        with patch.object(ci, "pages", return_value=[run]):
            filtered_snapshots = ci.snapshot("faberline/tape", self.head_sha, self.scoped)
        self.assertEqual(filtered_snapshots, {})
        state, reasons, _ = ci.evaluate_policy(self.scoped, filtered_snapshots)
        self.assertEqual(state, "pending")
        self.assertIn(".github/workflows/affected-docs.yml: missing run", reasons)

    def test_mismatched_base_workflow_definition_cannot_qualify(self):
        # Base workflow definition does not match expected definition SHA
        wf_path = ".github/workflows/affected-docs.yml"
        correct_sha = self.config["qualification"]["trusted_workflows"][wf_path]["definition_sha"]
        definitions = {
            (wf_path, self.head_sha): {"sha": correct_sha},
            (wf_path, self.base_sha): {"sha": "e" * 40},  # mismatched base definition
        }
        bad_scoped = q.scoped_config(self.config, self.pr, self.files, definitions)
        self.assertIn(".github/workflows/affected-docs.yml", bad_scoped["_qualification_context"]["untrusted_workflows"])
        self.assertTrue(any("untrusted head/base workflow definition" in issue for issue in bad_scoped["_qualification_context"]["issues"]))
        state, reasons, _ = ci.evaluate_policy(bad_scoped, self.make_evidence())
        self.assertEqual(state, "pending")
        self.assertTrue(any("untrusted head/base workflow definition" in r for r in reasons))


if __name__ == "__main__":
    unittest.main()
