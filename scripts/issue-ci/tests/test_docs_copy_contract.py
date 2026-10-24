"""Tests for docs copy contract validator and regression fixtures.

Covers all positive and negative fixture scenarios required by issue #40:
1. Valid complete copy passes with nonzero assertions.
2. Negative fixtures:
   - Delete a paragraph -> fails
   - Change a command/scheme -> fails
   - Omit an issue heading -> fails
   - Duplicate a source heading -> fails
   - Break a relative link -> fails
   - Alter the handoff -> fails
   - Add forbidden README -> fails
   - Change file mode -> fails
   - Add Rust/K8s path -> fails
3. Policy selection tests:
   - Valid 3-document inventory selects both real gates
   - Extra unmapped file refuses qualification
   - Rename from protected path refuses qualification
   - Changed validator/selector/policy refuses qualification
   - Incomplete file inventory refuses qualification
4. Evaluator evidence tests:
   - Missing / skipped / cancelled / stale head or base / old attempt / untrusted workflow evidence cannot qualify
5. Corrupted fixture demonstrations for content omission and illegal scope change.
"""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import importlib.util

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ROOT / "scripts/issue-ci"

def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

validator = load_module("validate_docs_copy", SCRIPTS / "validate-docs-copy.py")
DiskLoader = validator.DiskLoader
ValidationError = validator.ValidationError
validate_docs_copy = validator.validate_docs_copy
EXPECTED_CHANGES = validator.EXPECTED_CHANGES

ci = load_module("handoff", SCRIPTS / "handoff.py")
q = ci.qualification_module()

POLICY_FILE = ROOT / ".github/issue-ci-candidate-gates.json"
BASE_HANDOFF_CONTENT = (ROOT / "docs/deployment-handoff.md").read_text()
BASE_OPS_CONTENT = (ROOT / "docs/operations/README.md").read_text()

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

HEAD_OPS_CONTENT = """# Operations

These documents are for someone running or diagnosing a live cluster, before
the product docs link to them.

| Document | What it answers |
|----------|-----------------|
| [Deployment handoff](../deployment-handoff.md) | Images, serve flags, environment, HTTP surface, smoke sequence, backup and restore runbooks |
| [Cold restore runbook](../runbooks/cold-restore.md) | Ad-hoc backups, destination storage schemes, and cold restore via bootstrap-if-empty |
| [Disk-full runbook](../runbooks/disk-full.md) | Degraded read-only mode on ENOSPC, metric diagnosis, and volume recovery |
| [Node drain and PodDisruptionBudget](../runbooks/node-drain-and-pdb.md) | Unblocking a drain on the direct-install singleton and verifying recovery |
| [Benchmark history](../benchmarks-scale.md) | The local performance gate and the retired peer-broker calibrations |
"""


class DocsCopyValidatorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="docs-contract-test-"))
        self.base_dir = self.tmp / "base"
        self.head_dir = self.tmp / "head"
        self.base_dir.mkdir(parents=True)
        self.head_dir.mkdir(parents=True)

        # Setup valid base
        (self.base_dir / "docs/deployment-handoff.md").parent.mkdir(parents=True, exist_ok=True)
        (self.base_dir / "docs/deployment-handoff.md").write_text(BASE_HANDOFF_CONTENT)
        (self.base_dir / "docs/operations/README.md").parent.mkdir(parents=True, exist_ok=True)
        (self.base_dir / "docs/operations/README.md").write_text(BASE_OPS_CONTENT)

        # Setup valid head
        (self.head_dir / "docs/deployment-handoff.md").parent.mkdir(parents=True, exist_ok=True)
        (self.head_dir / "docs/deployment-handoff.md").write_text(BASE_HANDOFF_CONTENT)
        (self.head_dir / "docs/operations/README.md").parent.mkdir(parents=True, exist_ok=True)
        (self.head_dir / "docs/operations/README.md").write_text(HEAD_OPS_CONTENT)
        (self.head_dir / "docs/runbooks/cold-restore.md").parent.mkdir(parents=True, exist_ok=True)
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(COLD_RESTORE_CONTENT)
        (self.head_dir / "docs/runbooks/disk-full.md").write_text(DISK_FULL_CONTENT)

        self.valid_changed = list(EXPECTED_CHANGES)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_valid_complete_copy_passes_with_nonzero_assertions(self):
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        result = validate_docs_copy(loader)
        self.assertEqual(result["status"], "pass")
        self.assertGreaterEqual(result["assertions_count"], 7)
        self.assertEqual(len(result["assertions"]), result["assertions_count"])

    def test_delete_a_paragraph_fails(self):
        # Delete paragraph in cold-restore.md
        corrupted = COLD_RESTORE_CONTENT.replace("1. Set `spec.bootstrapSeedUri`", "<!-- omitted -->")
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(corrupted)
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "assertion 3 failed"):
            validate_docs_copy(loader)

    def test_change_command_or_scheme_fails(self):
        # Alter command in disk-full
        corrupted = DISK_FULL_CONTENT.replace("tape_storage_", "altered_metric_")
        (self.head_dir / "docs/runbooks/disk-full.md").write_text(corrupted)
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "assertion 4 failed"):
            validate_docs_copy(loader)

        # Alter scheme in cold-restore
        (self.head_dir / "docs/runbooks/disk-full.md").write_text(DISK_FULL_CONTENT)
        corrupted = COLD_RESTORE_CONTENT.replace("file://", "ftp://")
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(corrupted)
        with self.assertRaisesRegex(ValidationError, "assertion 3 failed"):
            validate_docs_copy(loader)

    def test_omit_issue_heading_fails(self):
        # Omit (#1329) from header
        corrupted = COLD_RESTORE_CONTENT.replace("# Backup / restore runbook (#1329)", "# Backup / restore runbook")
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(corrupted)
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "assertion 3 failed"):
            validate_docs_copy(loader)

    def test_duplicate_source_heading_fails(self):
        # Add duplicate heading in cold-restore
        corrupted = COLD_RESTORE_CONTENT + "\n## Cold restore runbook (#2465, #2468)\n"
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(corrupted)
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "duplicate headings"):
            validate_docs_copy(loader)

        # Restore head and test duplicate heading in source handoff
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(COLD_RESTORE_CONTENT)
        corrupted_source = BASE_HANDOFF_CONTENT.replace(
            "## 7. Backup / restore runbook (#1329)\n",
            "## 7. Backup / restore runbook (#1329)\n\n## 7. Backup / restore runbook (#1329)\n",
        )
        (self.base_dir / "docs/deployment-handoff.md").write_text(corrupted_source)
        (self.head_dir / "docs/deployment-handoff.md").write_text(corrupted_source)
        with self.assertRaisesRegex(ValidationError, "assertion 5 failed: duplicate headings"):
            validate_docs_copy(loader)

    def test_break_relative_link_fails(self):
        # Break link in operations table
        corrupted = HEAD_OPS_CONTENT.replace("../runbooks/cold-restore.md", "http://external.site/cold-restore.md")
        (self.head_dir / "docs/operations/README.md").write_text(corrupted)
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "assertion 7 failed"):
            validate_docs_copy(loader)

    def test_alter_handoff_fails(self):
        # Mutate handoff on head
        (self.head_dir / "docs/deployment-handoff.md").write_text(BASE_HANDOFF_CONTENT + "\n<!-- extra edit -->\n")
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "assertion 2 failed: docs/deployment-handoff.md was modified"):
            validate_docs_copy(loader)

    def test_add_forbidden_readme_fails(self):
        # Add forbidden docs/runbooks/README.md
        forbidden = self.head_dir / "docs/runbooks/README.md"
        forbidden.write_text("# Runbooks index\n")
        changed = list(self.valid_changed) + ["docs/runbooks/README.md"]
        loader = DiskLoader(self.base_dir, self.head_dir, changed)
        with self.assertRaisesRegex(ValidationError, "assertion 1 failed: unexpected changed paths"):
            validate_docs_copy(loader)

    def test_change_file_mode_fails(self):
        # Change file mode to executable
        target = self.head_dir / "docs/runbooks/cold-restore.md"
        target.chmod(0o755)
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)
        with self.assertRaisesRegex(ValidationError, "invalid file mode"):
            validate_docs_copy(loader)

    def test_add_rust_or_k8s_path_fails(self):
        for bad_path in ("crates/tape/src/lib.rs", "k8s/overlay/test.yaml", "Cargo.toml"):
            changed = list(self.valid_changed) + [bad_path]
            loader = DiskLoader(self.base_dir, self.head_dir, changed)
            with self.assertRaisesRegex(ValidationError, "assertion 1 failed: unexpected changed paths"):
                validate_docs_copy(loader)

    def test_corrupted_fixture_demonstrations(self):
        """Negative control demonstration: shows verbatim failing assertion then restored pass."""
        loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed)

        # 1. Deliberate content omission: remove retention paragraph in cold-restore
        omitted = COLD_RESTORE_CONTENT.replace("--retention-secs", "omitted-retention")
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(omitted)
        with self.assertRaises(ValidationError) as ctx:
            validate_docs_copy(loader)
        self.assertIn("assertion 3 failed: docs/runbooks/cold-restore.md differs from normalized", str(ctx.exception))
        # Restore and verify pass
        (self.head_dir / "docs/runbooks/cold-restore.md").write_text(COLD_RESTORE_CONTENT)
        self.assertEqual(validate_docs_copy(loader)["status"], "pass")

        # 2. Deliberate illegal scope change: add unmapped file
        illegal_loader = DiskLoader(self.base_dir, self.head_dir, self.valid_changed + ["docs/extra.md"])
        with self.assertRaises(ValidationError) as ctx:
            validate_docs_copy(illegal_loader)
        self.assertIn("assertion 1 failed: unexpected changed paths: ['docs/extra.md']", str(ctx.exception))
        # Restored passing result
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
        # Mismatched changed_files count
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

    def make_evidence(self, conclusion="success", status="completed", run_attempt=1, wf_id=None):
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
                "base": {"sha": self.base_sha},
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


if __name__ == "__main__":
    unittest.main()
