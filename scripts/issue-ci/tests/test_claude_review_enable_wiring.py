"""Offline enable-candidate controls. Parses YAML data, never runs product or gh."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[3]
PIN = "e06e407972685df3d5b50adeeca1d8ae913b4cea"
PREFLIGHT_IF = "${{ github.event.workflow_run.event == 'pull_request' && github.event.workflow_run.conclusion == 'success' && github.event.workflow_run.head_repository.full_name == github.repository }}"
REVIEW_IF = "${{ needs.claude-preflight.result == 'success' && needs.claude-preflight.outputs.eligible == 'true' }}"
PARSER = r'''
require 'psych'; require 'json'
def value(node)
  raise 'tag refused' unless node.tag.nil?
  case node
  when Psych::Nodes::Mapping
    result = {}
    node.children.each_slice(2) do |key, item|
      raise 'key refused' unless key.is_a?(Psych::Nodes::Scalar) && key.tag.nil?
      raise 'duplicate key' if result.key?(key.value)
      result[key.value] = value(item)
    end
    result
  when Psych::Nodes::Sequence
    node.children.map { |item| value(item) }
  when Psych::Nodes::Scalar
    node.value
  else
    raise 'unsupported node'
  end
end
stream = Psych.parse_stream(STDIN.read)
raise 'one document required' unless stream.children.length == 1
puts JSON.generate(value(stream.children.first.root))
'''


def yaml_data(source):
    # Psych AST only: no YAML constructors, aliases, tags or duplicate keys.
    return json.loads(subprocess.check_output(["ruby", "-rpsych", "-rjson", "-e", PARSER],
                      input=source, text=True, stderr=subprocess.DEVNULL))


def git_blob(raw):
    return hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()


def check_wiring(policy, review_source, merge_source, supply, provenance, binary_evidence, enable_candidate):
    base = {key: value for key, value in policy.items() if key not in ("claude_final_review", "claude_findings")}
    assert hashlib.sha256(json.dumps(base, sort_keys=True, separators=(",", ":")).encode()).hexdigest() == provenance["baseline_policy_canonical_sha256"]
    assert base["lifecycle_mode"] == "failure-only" and len(base["qualification_blockers"]) == 4
    prefix = provenance["existing_issue_ci_prefix"]
    assert hashlib.sha256(review_source.encode()[:prefix["length"]]).hexdigest() == prefix["sha256"]
    review, merge = yaml_data(review_source), yaml_data(merge_source)
    assert review["on"] == {"workflow_run": {"workflows": ["gke-test"], "types": ["completed"]}}
    assert set(review["jobs"]) == {"handoff", "claude-preflight", "claude-final-review"}
    assert review["jobs"]["claude-preflight"]["needs"] == "handoff"
    assert review["jobs"]["claude-final-review"]["needs"] == "claude-preflight"
    for name in ("claude-preflight", "claude-final-review"):
        job = review["jobs"][name]
        assert job["if"] == (PREFLIGHT_IF if name == "claude-preflight" else REVIEW_IF)
        assert job["permissions"] == {"contents": "read", "actions": "read", "pull-requests": "read", "issues": "read"}
        for step in job["steps"]:
            if "uses" in step and step["uses"].startswith("faberline/aw/"):
                assert step["uses"] == "faberline/aw/.github/actions/claude-review@" + PIN
    steps = review["jobs"]["claude-final-review"]["steps"]
    assert steps[0]["with"]["ref"] == "${{ github.workflow_sha }}"
    assert steps[0]["with"]["persist-credentials"] == "false"
    assert "head.sha" not in json.dumps(steps[0])
    credentialed = [step for step in steps if "CLAUDE_CODE_OAUTH_TOKEN" in step.get("env", {})]
    assert len(credentialed) == 1 and credentialed[0]["with"]["operation"] == "review"
    assert "secrets." not in merge_source and set(merge["permissions"].values()) == {"read"}
    for job in merge["jobs"].values():
        assert job["if"] == "${{ false }}" and "permissions" not in job
    p = policy["claude_final_review"]
    assert p["producer"] == {"path": ".github/workflows/issue-ci.yml", "id": 394214906,
                             "definition_sha": git_blob(review_source.encode()), "job": "claude-final-review"}
    gate_blob = provenance.get("dependency_pin_candidate", {}).get("candidate_workflow_blobs", {}).get(".github/workflows/gke-test.yml", provenance["gke"]["blob"])
    assert p["gates"] == {".github/workflows/gke-test.yml": {"id": 389097869, "definition_sha": gate_blob}}
    assert p["credential_profile"] == "personal-claude-max-oauth"
    assert p["merge_producer"] is None and p["merge_method"] is None
    contract = json.loads((ROOT / ".github/claude-review-test-contract.json").read_text())
    assert contract["held_test_root"] in p["protected_paths"]
    assert set(contract["held_build_and_target_paths"]) <= set(p["protected_paths"])
    assert "crates/tape/tests/" in p["protected_paths"] and "Cargo.toml" in p["protected_paths"]
    product = "crates/tape/src/service.rs"
    assert not any(product == root.rstrip("/") or product.startswith(root.rstrip("/") + "/") for root in p["protected_paths"])
    fixture = ROOT / "scripts/issue-ci/tests/fixtures/claude-native-2.1.310/manifest.json"
    manifest = json.loads(fixture.read_text())
    assert supply["manifest_sha256"] == hashlib.sha256(fixture.read_bytes()).hexdigest()
    assert p["cli_sha256"] == supply["binary_sha256"] == manifest["platforms"]["linux-x64"]["checksum"]
    assert supply["status"] == "reviewed_pinned"
    assert supply["enabled"] is True
    assert binary_evidence["observed_sha256"] == supply["binary_sha256"]
    assert binary_evidence["observed_size"] == manifest["platforms"]["linux-x64"]["size"] == 291638640
    assert binary_evidence["binary_executed"] is False and binary_evidence["binary_installed"] is False
    assert enable_candidate["status"] == "offline_skill_protocol_and_github_findings_candidate_not_deployed"
    assert enable_candidate["publication_authorized"] is False and enable_candidate["live_deployment_activated"] is False
    assert enable_candidate["producer_candidate_blob"] == git_blob(review_source.encode())
    assert enable_candidate["parallel_hourly_claude_review_required_until_ci_operational"] is True
    assert enable_candidate["cron_change_performed"] is False
    assert enable_candidate["ci_review_operational_proof"] == dict(exact_head_claude_job_conclusion=None,
        authenticated_review_receipt=None, guard_negative_evidence=None)
    verification = provenance["cli_observation"]["independent_signature_verification"]
    assert verification["gpg_verify_exit_code"] == 0
    assert verification["validsig_fingerprint"] == "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE"
    assert verification["binary_downloaded"] is False and verification["binary_hash_checked"] is False
    assert provenance["aw_candidate"] == PIN and provenance["rollout_status"] == "offline_skill_protocol_and_github_findings_candidate_not_deployed"


class OfflineEnableWiringTests(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads((ROOT / ".github/issue-ci-required-gates.json").read_text())
        self.review = (ROOT / ".github/workflows/issue-ci.yml").read_text()
        self.merge = (ROOT / ".github/workflows/claude-guarded-merge.yml").read_text()
        self.supply = json.loads((ROOT / ".github/claude-review-cli-supply.json").read_text())
        self.provenance = json.loads((ROOT / ".github/claude-review-pilot-provenance.json").read_text())
        self.binary_evidence = json.loads((ROOT / ".github/claude-review-binary-integrity-candidate.json").read_text())
        self.enable_candidate = json.loads((ROOT / ".github/claude-review-enable-candidate.json").read_text())

    def check(self, **changes):
        values = dict(policy=self.policy, review_source=self.review, merge_source=self.merge,
                      supply=self.supply, provenance=self.provenance, binary_evidence=self.binary_evidence, enable_candidate=self.enable_candidate)
        values.update(changes)
        check_wiring(**values)

    def test_actual_offline_candidate_is_guarded_and_preserves_existing_handoff(self):
        self.check()

    def test_unconditional_or_incomplete_review_conditions_are_rejected_then_restored_passes(self):
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace(PREFLIGHT_IF, "${{ true }}", 1))
        with self.assertRaises(AssertionError): self.check(merge_source=self.merge.replace("${{ false }}", "${{ true }}", 1))
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace(REVIEW_IF, "${{ true }}", 1))
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace(" && github.event.workflow_run.head_repository.full_name == github.repository", ""))
        self.check()

    def test_permission_expansion_or_mutable_aw_pin_is_rejected(self):
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace("issues: read", "issues: write"))
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace(PIN, "main"))
        with self.assertRaises(AssertionError): self.check(merge_source=self.merge.replace("contents: read", "contents: write"))

    def test_removing_real_gate_or_coverage_blocker_is_rejected(self):
        for mutation in (lambda p: p["workflows"][0]["jobs"].pop(), lambda p: p.update(qualification_blockers=[]), lambda p: p.update(lifecycle_mode="full")):
            value = copy.deepcopy(self.policy); mutation(value)
            with self.assertRaises(AssertionError): self.check(policy=value)

    def test_missing_or_mismatched_integrity_evidence_is_rejected(self):
        for mutation in (lambda v: v.update(observed_sha256="f" * 64),
                         lambda v: v.update(observed_size=0),
                         lambda v: v.update(binary_executed=True)):
            value = copy.deepcopy(self.binary_evidence); mutation(value)
            with self.assertRaises(AssertionError): self.check(binary_evidence=value)

    def test_weakened_test_harness_or_target_boundary_is_rejected(self):
        for path in ("crates/tape/tests/", "Cargo.toml", "crates/tape/Cargo.toml", "crates/tape/build.rs", ".cargo/"):
            value = copy.deepcopy(self.policy)
            value["claude_final_review"]["protected_paths"].remove(path)
            with self.subTest(path=path), self.assertRaises(AssertionError): self.check(policy=value)

    def test_unknown_or_changed_credential_profile_is_rejected(self):
        for profile in (None, "team-enterprise-oauth", "unknown"):
            value = copy.deepcopy(self.policy)
            value["claude_final_review"]["credential_profile"] = profile
            with self.subTest(profile=profile), self.assertRaises(AssertionError): self.check(policy=value)

    def test_stale_producer_blob_or_false_deployment_claim_is_rejected(self):
        value = copy.deepcopy(self.policy)
        value["claude_final_review"]["producer"]["definition_sha"] = "f" * 40
        with self.assertRaises(AssertionError): self.check(policy=value)
        candidate = dict(self.enable_candidate, live_deployment_activated=True)
        with self.assertRaises(AssertionError): self.check(enable_candidate=candidate)
        candidate = dict(self.enable_candidate, parallel_hourly_claude_review_required_until_ci_operational=False)
        with self.assertRaises(AssertionError): self.check(enable_candidate=candidate)

    def test_runtime_gke_matches_declared_candidate_and_docs_remain_observed_bytes(self):
        candidate = self.provenance.get("dependency_pin_candidate", {}).get("candidate_workflow_blobs", {})
        self.assertEqual(git_blob((ROOT / ".github/workflows/gke-test.yml").read_bytes()), candidate.get(".github/workflows/gke-test.yml", self.provenance["gke"]["blob"]))
        self.assertEqual(self.provenance["gke"]["blob"], "052a2e83dcac71da8b6292661d4200c985b3c1d3")
        for entry in self.provenance["other"]:
            self.assertEqual(git_blob((ROOT / entry["path"]).read_bytes()), entry["blob"])
        expected = self.policy["workflows"][0]["jobs"]
        observed = [job["name"] for job in self.provenance["gke"]["jobs"]]
        self.assertEqual(sorted(expected), sorted(observed))
        self.assertEqual(sum(job["conclusion"] == "failure" for job in self.provenance["gke"]["jobs"]), 2)

    def test_yaml_ambiguity_and_constructor_syntax_refused(self):
        for source in ("a: one\na: two\n", "a: &x one\nb: *x\n", "a: !ruby/object:Object {}\n"):
            with self.assertRaises(subprocess.CalledProcessError): yaml_data(source)


if __name__ == "__main__":
    unittest.main()
