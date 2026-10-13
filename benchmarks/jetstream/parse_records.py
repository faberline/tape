#!/usr/bin/env python3
"""Fail-closed parser for tape-bench jetstream-suite JSON records."""
import json, sys

required = {(p, c, s) for p in (128, 1024, 4096) for c in (1, 16, 64) for s in range(5)}
rows = []
for line in open(sys.argv[1], encoding="utf-8"):
    try: row = json.loads(line)
    except json.JSONDecodeError: continue
    if row.get("target") not in {"tape", "jetstream"}: continue
    cell = row.get("cell", {})
    key = (cell.get("payload_bytes"), cell.get("clients"), row.get("sample_index"))
    phase = row.get("phase")
    normal_key = (cell.get("payload_bytes"), cell.get("clients"), row.get("sample_index"))
    if (phase == "normal" and normal_key not in required) or (phase == "recovery" and (cell.get("payload_bytes"), cell.get("clients")) not in {(p,c) for p in (128,1024,4096) for c in (1,16,64)}) or not isinstance(row.get("throughput_ops_per_sec"), (int, float)) or not isinstance(row.get("p99_ms"), (int, float)):
        raise SystemExit("unexpected or incomplete benchmark record")
    replay = row.get("replay")
    if not isinstance(replay, dict):
        raise SystemExit("malformed replay evidence")
    if row.get("ack_batch_size") != 100 or row.get("error_count") != 0:
        raise SystemExit("wrong ack mode or benchmark error")
    if phase not in {"normal", "finalize", "recovery"}:
        raise SystemExit("missing run phase")
    if phase == "normal":
        normal_replay = {
            "expected": 0,
            "received": 0,
            "loss_count": 0,
            "duplicate_count": 0,
            "error_count": 0,
            "passed": False,
        }
        if replay != normal_replay:
            raise SystemExit("normal records must not report replay success")
    if phase in {"finalize", "recovery"} and (replay.get("expected") != 100000 or replay.get("received") != 100000 or replay.get("loss_count") != 0 or replay.get("duplicate_count") != 0 or replay.get("error_count") != 0 or replay.get("passed") is not True):
        raise SystemExit("missing or failed recovery evidence")
    rows.append(row)
if any(not r.get("run_id") or not r.get("topic") for r in rows):
    raise SystemExit("missing durable run identity")
normal = [r for r in rows if r.get("phase") == "normal"]
recovery = [r for r in rows if r.get("phase") == "recovery"]
finalize = [r for r in rows if r.get("phase") == "finalize"]
if [r.get("phase") for r in rows] != (["normal"] * 90 + ["finalize"] * 18 + ["recovery"] * 18):
    raise SystemExit("phase order must be normal, finalize, recovery")
seen = {(r["target"], r["cell"]["payload_bytes"], r["cell"]["clients"], r["sample_index"]) for r in normal}
if len(normal) != 90 or len(seen) != 90:
    raise SystemExit("missing or duplicate target/cell/sample records")
expected_order = []
for p in (128, 1024, 4096):
    for c in (1, 16, 64):
        for s in range(5):
            expected_order.extend([("tape", p, c, s), ("jetstream", p, c, s)])
actual_order = [(r["target"], r["cell"]["payload_bytes"], r["cell"]["clients"], r["sample_index"]) for r in normal]
if actual_order != expected_order:
    raise SystemExit("normal records are not interleaved Tape/JetStream pairs")
identity = lambda rs: {(r["target"], r["cell"]["payload_bytes"], r["cell"]["clients"]) for r in rs}
if len(finalize) != 18 or len(identity(finalize)) != 18 or len(recovery) != 18 or len(identity(recovery)) != 18 or any(r.get("sample_index") != 4 for r in finalize + recovery) or len({r["target"] for r in recovery}) != 2:
    raise SystemExit("post-restart recovery evidence must contain 18 records for both targets")
for r in recovery + finalize:
    match = [n for n in normal if n["target"] == r["target"] and n["cell"]["payload_bytes"] == r["cell"]["payload_bytes"] and n["cell"]["clients"] == r["cell"]["clients"] and n["sample_index"] == 4]
    if len(match) != 1 or r["run_id"] != match[0]["run_id"] or r["topic"] != match[0]["topic"]:
        raise SystemExit("finalize/recovery identity does not match sample-4 durable instance")
for key in required:
    pair = [r for r in normal if (r["cell"]["payload_bytes"], r["cell"]["clients"], r["sample_index"]) == key]
    tape = next((r for r in pair if r["target"] == "tape"), None)
    nats = next((r for r in pair if r["target"] == "jetstream"), None)
    if not tape or not nats or tape["throughput_ops_per_sec"] < nats["throughput_ops_per_sec"] or tape["p99_ms"] > nats["p99_ms"]:
        raise SystemExit("durable-v1 comparison failed")
json.dump({"records": rows, "verdict": "PASSED"}, sys.stdout)
