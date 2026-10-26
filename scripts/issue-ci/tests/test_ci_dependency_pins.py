"""Offline boundary controls for reference-only CI dependency candidates."""
import copy
import hashlib
import json
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[3]
BASELINES = {".github/workflows/gke-test.yml": "052a2e83dcac71da8b6292661d4200c985b3c1d3",
             ".github/workflows/tape-test-image.yml": "9179c8ed14c3aeba04ef0ec052fa8508985d0c5a"}
EXPECTED = {"actions/checkout", "actions/upload-artifact", "actions/download-artifact", "dtolnay/rust-toolchain",
            "Swatinem/rust-cache", "docker/setup-qemu-action", "docker/setup-buildx-action", "docker/login-action", "docker/build-push-action"}


def blob(raw):
    return hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()


def check(sources, evidence, policy, provenance):
    assert evidence["status"] == "offline_unpublished_candidate_not_qualified"
    assert set(sources) == set(BASELINES) == set(evidence["workflows"])
    assert {item["repo"] for item in evidence["references"]} == EXPECTED
    image = sources[".github/workflows/tape-test-image.yml"]
    for item, replacement in zip(evidence["references"], evidence["replacements"]):
        assert item["source_url"] == "https://api.github.com/repos/" + item["repo"] + "/git/ref/" + item["observed_ref"]
        assert re.fullmatch("[0-9a-f]{40}", item["commit"])
        ref_object = item["ref_object"]
        if ref_object["type"] == "tag":
            assert item["tag_dereference"]["url"] == ref_object["url"]
            assert item["tag_dereference"]["object"]["type"] == "commit"
            assert item["tag_dereference"]["object"]["sha"] == item["commit"]
        else:
            assert ref_object["type"] == "commit" and ref_object["sha"] == item["commit"]
        before = item["repo"] + "@" + item["observed_ref"].split("/", 1)[1]
        after = item["repo"] + "@" + item["commit"]
        assert replacement == dict(before=before, after=after)
        image = re.sub(r"(?m)(^\s*-?\s*uses:\s*)" + re.escape(after) + r"(?=\s|$)", lambda m: m[1] + before, image)
    assert blob(image.encode()) == BASELINES[".github/workflows/tape-test-image.yml"]
    aw = evidence["aw_reusable"]["candidate_commit"]
    assert re.fullmatch("[0-9a-f]{40}", aw) and evidence["aw_reusable"]["published"] is False
    gke = sources[".github/workflows/gke-test.yml"]
    assert gke.count("      aw-ref: " + aw + "\n") == 1
    normalized = gke.replace("faberline/aw/.github/workflows/gke-test.yml@" + aw,
                             "faberline/aw/.github/workflows/gke-test.yml@main")
    normalized = normalized.replace("      aw-ref: " + aw + "\n", "")
    assert blob(normalized.encode()) == BASELINES[".github/workflows/gke-test.yml"]
    for path, source in sources.items():
        definition = evidence["workflows"][path]
        assert definition["baseline_blob"] == BASELINES[path] and blob(source.encode()) == definition["candidate_blob"]
        uses = re.findall(r"(?m)^\s*-?\s*uses:\s*(\S+)", source)
        assert all(item.startswith("./") or re.fullmatch(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}", item) for item in uses)
    assert policy["claude_final_review"]["gates"][".github/workflows/gke-test.yml"]["definition_sha"] == evidence["workflows"][".github/workflows/gke-test.yml"]["candidate_blob"]
    candidate = provenance["dependency_pin_candidate"]
    assert candidate["published"] is False and candidate["real_checks_executed"] is False
    assert candidate["if_published_gke_path_match"] is True
    assert provenance["gke"]["blob"] == BASELINES[".github/workflows/gke-test.yml"]


class DependencyPinTests(unittest.TestCase):
    def setUp(self):
        self.evidence = json.loads((ROOT / ".github/ci-dependency-pins-candidate.json").read_text())
        self.sources = {path: (ROOT / path).read_text() for path in BASELINES}
        self.policy = json.loads((ROOT / ".github/issue-ci-required-gates.json").read_text())
        self.provenance = json.loads((ROOT / ".github/claude-review-pilot-provenance.json").read_text())

    def test_actual_candidate_preserves_commands_jobs_and_permissions(self):
        check(self.sources, self.evidence, self.policy, self.provenance)

    def test_changed_command_is_rejected_then_original_passes(self):
        path = ".github/workflows/tape-test-image.yml"
        changed = dict(self.sources, **{path: self.sources[path].replace("cargo build", "cargo check")})
        evidence = copy.deepcopy(self.evidence)
        evidence["workflows"][path]["candidate_blob"] = blob(changed[path].encode())
        with self.assertRaises(AssertionError): check(changed, evidence, self.policy, self.provenance)
        check(self.sources, self.evidence, self.policy, self.provenance)

    def test_mismatched_aw_scripts_ref_or_invented_source_owner_rejected(self):
        path = ".github/workflows/gke-test.yml"
        changed = dict(self.sources, **{path: self.sources[path].replace("      aw-ref: " + self.evidence["aw_reusable"]["candidate_commit"], "      aw-ref: main")})
        with self.assertRaises(AssertionError): check(changed, self.evidence, self.policy, self.provenance)
        evidence = copy.deepcopy(self.evidence)
        evidence["references"][0]["source_url"] = "https://api.github.com/repos/foreign/action/git/ref/tags/v4"
        with self.assertRaises(AssertionError): check(self.sources, evidence, self.policy, self.provenance)


if __name__ == "__main__":
    unittest.main()
