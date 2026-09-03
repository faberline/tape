#!/usr/bin/env python3
"""Make a redacted Tape GKE receipt bound to one immutable candidate."""
from __future__ import annotations
import argparse, hashlib, json
from pathlib import Path

def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--candidate-manifest", required=True, type=Path)
    p.add_argument("--result", required=True, choices=["passed"])
    p.add_argument("--output", required=True, type=Path)
    a = p.parse_args()
    raw = a.candidate_manifest.read_bytes()
    m = json.loads(raw)
    receipt = {"schema":"tape.gke-release-receipt/v1","complete":True,"candidate":{
        "version":m["version"],"commit":m["commit"],"run_id":m["run_id"],"run_attempt":m["run_attempt"],
        "manifest_sha256":hashlib.sha256(raw).hexdigest(),"root_digest":m["image"]["root_digest"],
        "amd64_digest":m["image"]["amd64_digest"],"arm64_digest":m["image"]["arm64_digest"]},
        "result":a.result,"redaction":{"kubeconfig_retained":False,"token_retained":False,"command_output_retained":False}}
    data = (json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n").encode()
    a.output.write_bytes(data)
    a.output.with_suffix(a.output.suffix + ".sha256").write_text(hashlib.sha256(data).hexdigest() + "  " + a.output.name + "\n")
if __name__ == "__main__": main()
