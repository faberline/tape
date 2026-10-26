"""Synthetic supply bytes and mocked GPG only; no download or CLI execution."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("claude_supply", ROOT / "scripts/issue-ci/provision-claude.py")
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class SupplyTests(unittest.TestCase):
    def fixture(self):
        supply = json.loads((ROOT / ".github/claude-review-cli-supply.json").read_text())
        binary = b"synthetic fixture bytes; never executable product code"
        digest = hashlib.sha256(binary).hexdigest()
        manifest = json.dumps(dict(version=p.VERSION, platforms={"linux-x64": dict(checksum=digest)})).encode()
        supply.update(enabled=True, status="reviewed_pinned", binary_sha256=digest,
                      manifest_sha256=hashlib.sha256(manifest).hexdigest())
        values = {supply["manifest_path"]: manifest, supply["signature_path"]: b"fixture signature",
                  supply["signing_key_path"]: b"fixture public key", supply["binary_path"]: binary}
        return supply, values

    def test_checked_in_disabled_supply_never_reads_network_or_writes(self):
        supply = json.loads((ROOT / ".github/claude-review-cli-supply.json").read_text())
        with tempfile.TemporaryDirectory() as directory, patch.object(p, "fetch", side_effect=AssertionError("no network")) as network:
            target = Path(directory) / "claude"
            with self.assertRaisesRegex(p.Refused, "supply_not_enabled"):
                p.provision(supply, target, reader=network)
            network.assert_not_called()
            self.assertFalse(target.exists())

    def test_synthetic_positive_and_corruption_negative_then_restored_positive(self):
        supply, values = self.fixture()
        with tempfile.TemporaryDirectory() as directory:
            verify = lambda *args: None
            first = Path(directory) / "first"
            self.assertEqual(p.provision(supply, first, lambda path, limit: values[path], verify), str(first))
            self.assertEqual(first.read_bytes(), values[supply["binary_path"]])
            corrupted = dict(values); corrupted[supply["binary_path"]] = b"corrupt"
            second = Path(directory) / "second"
            with self.assertRaisesRegex(p.Refused, "binary_hash_mismatch"):
                p.provision(supply, second, lambda path, limit: corrupted[path], verify)
            self.assertFalse(second.exists())
            p.provision(supply, second, lambda path, limit: values[path], verify)
            self.assertEqual(first.read_bytes(), second.read_bytes())

    def test_manifest_tamper_missing_pins_mutable_version_and_foreign_host_refuse(self):
        supply, values = self.fixture()
        mutations = [dict(origin="https://foreign.invalid"), dict(cli_version="latest"), dict(platform="darwin-arm64"),
                     dict(manifest_sha256=None), dict(binary_sha256=None), dict(signing_fingerprint="A" * 40)]
        for mutation in mutations:
            with self.subTest(mutation=mutation), self.assertRaises(p.Refused): p.validate(dict(supply, **mutation))
        with tempfile.TemporaryDirectory() as directory:
            values[supply["manifest_path"]] = b"forged manifest"
            with self.assertRaisesRegex(p.Refused, "manifest_hash_mismatch"):
                p.provision(supply, Path(directory) / "claude", lambda path, limit: values[path], lambda *args: None)

    def test_signature_requires_exact_expected_primary_or_signing_fingerprint(self):
        for fingerprint, code, passed in ((p.FINGERPRINT, 0, True), ("A" * 40, 0, False), (p.FINGERPRINT, 1, False)):
            status = ("[GNUPG:] VALIDSIG " + fingerprint + " 2026-10-01 1790812800 0 4 0 1 10 00 " + fingerprint + "\n").encode()
            calls = []
            def execute(command, **kwargs):
                calls.append((command, kwargs))
                return SimpleNamespace(returncode=0 if "--import" in command else code,
                                       stdout=b"" if "--import" in command else status)
            with tempfile.TemporaryDirectory() as directory:
                if passed:
                    p.signature_valid(directory, b"manifest", b"sig", b"key", execute)
                else:
                    with self.assertRaises(p.Refused): p.signature_valid(directory, b"manifest", b"sig", b"key", execute)
            self.assertTrue(all(call[0][0] == "/usr/bin/gpg" for call in calls))
            self.assertTrue(all(set(call[1]["env"]) == {"HOME", "PATH"} for call in calls))

    def test_signature_failure_never_fetches_or_writes_binary(self):
        supply, values = self.fixture(); reads = []
        def reader(path, limit):
            reads.append(path); return values[path]
        def failed(*args): raise p.Refused("manifest_signature_refused")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(p.Refused, "manifest_signature_refused"):
                p.provision(supply, Path(directory) / "cli", reader, failed)
        self.assertNotIn(supply["binary_path"], reads)

    def test_redirect_duplicate_json_and_arbitrary_source_rejected(self):
        with self.assertRaises(p.Refused): p.NoRedirect().redirect_request()
        with self.assertRaises(p.Refused): p.decode('{"version":1,"version":1}')
        with self.assertRaises(p.Refused): p.fetch("/latest", 20)

    def test_credentialed_main_stops_before_supply_or_network(self):
        for credential in ("GH_TOKEN", "GITHUB_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"):
            with patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux", credential: "synthetic-only"}, clear=True), patch.object(p.platform, "machine", return_value="x86_64"), patch.object(p.sys, "argv", ["provision-claude.py"]), patch.object(p.Path, "read_bytes", side_effect=AssertionError("no file read")):
                with self.assertRaisesRegex(p.Refused, "credentialed_provisioning_refused"): p.main()


if __name__ == "__main__":
    unittest.main()
