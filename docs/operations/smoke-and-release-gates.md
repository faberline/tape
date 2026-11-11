# Smoke sequence and release gates

Copied from the retiring deployment handoff page (sections 6 and 8). Text unchanged.

## 6. Smoke sequence (end-to-end)

```bash
BASE=http://localhost:7137
curl -fs $BASE/healthz                                    # -> ok
curl -fs $BASE/readyz                                     # -> ok (200)

curl -fs -X POST $BASE/topics/orders.created/append \
  -H 'content-type: application/json' \
  -d '{"payload":{"order_id":"o-1"}}'

curl -fs "$BASE/topics/orders.created/replay?from_offset=0" | jq '.events'

curl -fs -X PUT $BASE/topics/orders.created/consumers/c1/checkpoint \
  -H 'content-type: application/json' -d '{"offset":0}'

curl -fs $BASE/metrics | head -5                          # Prometheus text
```


## 8. Release-readiness gates

A build is **not** production-ready until these pass (run from repo root
unless noted):

```bash
cargo build -p tape
cargo test -p tape

TAPE_SOAK_AUTOSTART=1 TAPE_SOAK_DURATION_SECS=60 bash scripts/soak.sh
```

> Shared service hardening is runnable here: topic authz, projected-secret
> rotation, bounded request admission, redacted management audit, and the
> bounded replay/checkpoint soak and shared raft-runtime peer mTLS. Retention/
> backfill and compaction are Tape domain work; multi-hour production soak
> remains separate evidence and is not claimed by this gate.

