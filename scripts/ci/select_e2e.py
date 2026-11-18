#!/usr/bin/env python3
"""Choose the GKE e2e suites of a pull request from its changed paths.

Cost control (aw docs/standards/cost-control.md rule 2): a pull request runs
only the suites that its diff can affect. A push to main, a manual run and any
event that is not a pull request run every suite. A path that no rule names
selects every suite.
"""
import json
import os
import re
import subprocess
import sys

RECOVERY = "operator_recovery"
HA = "operator_ha"
SUITES = [RECOVERY, HA]

# (pattern, suites, image, reason). The first rule that matches a path wins.
# `image` true builds the server image and runs the arm64 image check even
# when `suites` is empty.
RULES = [
    (r"^\.github/workflows/(ci|gke-test)\.yml$", SUITES, True, "the CI or GKE workflow"),
    (r"^k8s/(operator|base|components)/", SUITES, True, "the operator or the shared manifests"),
    (r"^k8s/overlays/gke-test/", SUITES, True, "the gke-test overlay"),
    (r"^crates/tape-operator/", SUITES, True, "the operator"),
    (r"^crates/tape/tests/it/e2e\.rs$", SUITES, True, "the shared e2e helpers"),
    (r"^k8s/gke-test-job/operator_ha/", [HA], False, "the operator_ha test Job"),
    (r"^k8s/gke-test-job/operator_recovery/", [RECOVERY], False, "the operator_recovery test Job"),
    (r"^crates/tape/tests/it/e2e/operator_ha(\.rs|/)", [HA], False, "the operator_ha suite"),
    (r"^crates/tape/tests/it/e2e/operator_recovery\.rs$", [RECOVERY], False, "the operator_recovery suite"),
    (r"^crates/tape/tests/it/e2e/image\.rs$", [], True, "the image suite (arm64 runner, no GKE)"),
    (r"^(\.github/workflows/tape-test-image\.yml|Dockerfile\.test)$", [RECOVERY], True, "the test image build"),
    (r"^k8s/gke-test-job/", [], False, "a test Job that ci does not run"),
    (r"^k8s/overlays/(dev|staging|prod|template)/", [], False, "an overlay that gke-test does not use"),
    (r"^crates/tape/tests/it/e2e/", [], False, "a suite that ci does not run"),
    (r"^(docs|clients|scripts|acceptance|benchmarks|compat|\.cargo)/", [], False, "documents, clients and scripts"),
    (r"^\.github/workflows/tape-release(-candidate)?\.yml$", [], False, "the release workflows"),
    (r"^crates/", [], False, "Rust code: covered by the test jobs; main runs every suite"),
    (r"^[A-Za-z0-9_.-]+\.(md|toml|lock|txt)$", [], False, "a root document or config"),
    (r"^(Dockerfile|Dockerfile\.release|Dockerfile\.dockerignore|LICENSE|build\.sh|install\.sh)$", [], False, "a release or install file"),
]


def select(event, paths):
    """Return (suites, image, reasons). `reasons` maps a path to its reason."""
    if event != "pull_request":
        return list(SUITES), True, {"*": f"event {event} runs every suite"}
    chosen = set()
    image = False
    reasons = {}
    for path in paths:
        for pattern, suites, wants_image, reason in RULES:
            if re.search(pattern, path):
                break
        else:
            suites, wants_image, reason = SUITES, True, "no rule names this path: every suite"
        chosen.update(suites)
        image = image or wants_image
        reasons[path] = f"{reason}: {', '.join(suites) or 'no suite'}"
    return [s for s in SUITES if s in chosen], image or bool(chosen), reasons


def changed_paths(base, head):
    out = subprocess.run(
        ["git", "diff", "--no-renames", "--name-only", base, head],
        check=True, capture_output=True, text=True,
    ).stdout
    return [line for line in out.splitlines() if line]


def main(argv):
    event, base, head = argv[1], argv[2], argv[3]
    paths = changed_paths(base, head) if event == "pull_request" else []
    suites, image, reasons = select(event, paths)
    lines = [
        "### e2e selection",
        f"event: `{event}`; suites: `{json.dumps(suites)}`; e2e jobs: `{str(image).lower()}`",
        "",
    ] + [f"- `{path}`: {reason}" for path, reason in reasons.items()]
    print("\n".join(lines))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as f:
            f.write("\n".join(lines) + "\n")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as f:
            f.write(f"suites={json.dumps(suites)}\ne2e={str(image).lower()}\n")


if __name__ == "__main__":
    main(sys.argv)
