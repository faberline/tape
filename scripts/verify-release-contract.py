#!/usr/bin/env python3
"""Static guard for Tape's build-once candidate and no-rebuild promotion."""
from __future__ import annotations

import argparse
import hashlib
import re
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
CANDIDATE = ROOT / ".github/workflows/tape-release-candidate.yml"
PROMOTION = ROOT / ".github/workflows/tape-release.yml"
SHA = re.compile(r"@[0-9a-f]{40}(?:\s|#|$)")

def fail(message: str) -> None:
    raise SystemExit(f"release contract refused: {message}")

def check_text(candidate: str, promotion: str) -> None:
    for label, text in (("candidate", candidate), ("promotion", promotion)):
        if not all(SHA.search(line) for line in text.splitlines() if "uses:" in line):
            fail(f"{label} has a mutable action reference")
    if "workflow_dispatch:" not in promotion or "push:" in promotion:
        fail("promotion must be manual-only")
    if re.search(r"docker\s+(?:build\s|buildx\s+build)", promotion):
        fail("promotion contains an image build")
    if "candidate_run_id" not in promotion or "run_attempt" not in promotion:
        fail("promotion does not bind the candidate run and attempt")
    for token in ("gke_receipt_b64", "gke_receipt_sha256", "gke_receipt_sidecar_b64", "gke_receipt_sidecar_sha256"):
        if token not in promotion:
            fail(f"promotion lacks {token}")
    if "cclab.tape.candidate-manifest.v1" not in candidate:
        fail("candidate schema changed")
    if "tape.gke-release-receipt/v1" not in (ROOT / "apps/tape/scripts/make-gke-release-receipt.py").read_text():
        fail("GKE receipt schema changed")
    for gate in ("cargo test --locked -p tape", "tape_perf_gate", "project_docs_contract.py", "raft-implementor-build.sh"):
        if gate not in candidate:
            fail(f"candidate gate absent: {gate}")

def self_test() -> None:
    candidate = CANDIDATE.read_text()
    promotion = PROMOTION.read_text()
    check_text(candidate, promotion)
    mutations = {
        "promotion-build": promotion + "\n      - run: docker build .\n",
        "tag-first": promotion.replace("workflow_dispatch:", "push:\n    tags: [\"tape@*\"]\n  workflow_dispatch:", 1),
        "mutable-action": promotion.replace("@3d3c42e5aac5ba805825da76410c181273ba90b1", "@v4", 1),
        "missing-run": promotion.replace("candidate_run_id", "candidate_removed"),
    }
    for name, mutated in mutations.items():
        try:
            check_text(candidate, mutated)
        except SystemExit:
            continue
        fail(f"negative control passed: {name}")
    print("release contract self-test passed: positive plus 4 negative mutations")

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    else:
        check_text(CANDIDATE.read_text(), PROMOTION.read_text())
        print("release contract passed")

if __name__ == "__main__":
    main()
