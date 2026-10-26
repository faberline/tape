"""Static disabled-pilot controls. Parses YAML data, never runs product or gh."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[3]
PIN = "cc157b0eb296d1ac4a428db7ed0c023d42b87ac3"
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


def check_disabled(policy, review_source, merge_source, supply, provenance):
    base = {key: value for key, value in policy.items() if key != "claude_final_review"}
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
        assert job["if"] == "${{ false }}"
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
    assert p["gates"] == {".github/workflows/gke-test.yml": {"id": 389097869, "definition_sha": "052a2e83dcac71da8b6292661d4200c985b3c1d3"}}
    assert p["merge_producer"] is None and p["merge_method"] is None
    fixture = ROOT / "scripts/issue-ci/tests/fixtures/claude-native-2.1.310/manifest.json"
    manifest = json.loads(fixture.read_text())
    assert supply["manifest_sha256"] == hashlib.sha256(fixture.read_bytes()).hexdigest()
    assert p["cli_sha256"] == supply["binary_sha256"] == manifest["platforms"]["linux-x64"]["checksum"]
    assert supply["status"] == "manifest_signature_verified_binary_verification_pending"
    assert supply["enabled"] is False
    verification = provenance["cli_observation"]["independent_signature_verification"]
    assert verification["gpg_verify_exit_code"] == 0
    assert verification["validsig_fingerprint"] == "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE"
    assert verification["binary_downloaded"] is False and verification["binary_hash_checked"] is False
    assert provenance["aw_candidate"] == PIN and provenance["rollout_status"] == "disabled_review_only_candidate"


class DisabledWiringTests(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads((ROOT / ".github/issue-ci-required-gates.json").read_text())
        self.review = (ROOT / ".github/workflows/issue-ci.yml").read_text()
        self.merge = (ROOT / ".github/workflows/claude-guarded-merge.yml").read_text()
        self.supply = json.loads((ROOT / ".github/claude-review-cli-supply.json").read_text())
        self.provenance = json.loads((ROOT / ".github/claude-review-pilot-provenance.json").read_text())

    def check(self, **changes):
        values = dict(policy=self.policy, review_source=self.review, merge_source=self.merge,
                      supply=self.supply, provenance=self.provenance)
        values.update(changes)
        check_disabled(**values)

    def test_actual_staged_candidate_is_disabled_and_preserves_existing_handoff(self):
        self.check()

    def test_enabling_any_job_is_rejected_then_restored_candidate_passes(self):
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace("${{ false }}", "${{ true }}", 1))
        with self.assertRaises(AssertionError): self.check(merge_source=self.merge.replace("${{ false }}", "${{ true }}", 1))
        self.check()

    def test_permission_expansion_or_mutable_aw_pin_is_rejected(self):
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace("issues: read", "issues: write"))
        with self.assertRaises(AssertionError): self.check(review_source=self.review.replace(PIN, "main"))
        with self.assertRaises(AssertionError): self.check(merge_source=self.merge.replace("contents: read", "contents: write"))

    def test_removing_real_gate_or_coverage_blocker_is_rejected(self):
        for mutation in (lambda p: p["workflows"][0]["jobs"].pop(), lambda p: p.update(qualification_blockers=[]), lambda p: p.update(lifecycle_mode="full")):
            value = copy.deepcopy(self.policy); mutation(value)
            with self.assertRaises(AssertionError): self.check(policy=value)

    def test_runtime_gke_and_docs_workflows_unchanged_against_observed_blobs(self):
        self.assertEqual(git_blob((ROOT / ".github/workflows/gke-test.yml").read_bytes()), self.provenance["gke"]["blob"])
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
