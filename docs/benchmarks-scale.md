# tape — benchmark and scale posture

Tape measures its own performance against its own baseline today. The one live
gate is the local regression budget below. The planned
[`durable-jetstream-competitive`](../ROADMAP.md#durable-jetstream-competitive)
outcome adds one bounded comparison: single-replica persistent Tape WAL versus
pinned NATS JetStream FileStore. It is not delivered and makes no win claim
until its authorized GKE evidence exists.

## Benchmark standard

| axis | standard |
|---|---|
| Engine | Tape release build on one host, HTTP/1.1 and h2c |
| Data size | 1,000 events / 128-byte payload |
| Metric | append, replay, and checkpoint latency against the local regression budget; durable append throughput across connection counts |
| Reliability | `verify_report` fails the test on any budget miss or any external peer-win claim |

## Local regression budget

`apps/tape/e2e/tape_perf_gate.rs` runs `tape::bench::run_benchmark(1_000,
128)` and asserts:

- `report.local_regression_passed` — append, replay, and checkpoint stay inside
  the local regression budget.
- `!report.external_peer_win_claim` — the report never claims a win over
  another broker from the local-only run.
- Durable append throughput rises with connection count, so a regression in
  the h2c path or the WAL group commit fails the gate rather than the number
  drifting silently.

Reproduce:

```bash
cargo test --release -p tape --test tape_perf_gate -- --nocapture
```

The `tape-bench` CLI exposes the same report for ad hoc runs and is the
calibration source the gate checks itself against:

```bash
cargo run -p tape --bin tape-bench -- run --events 1000 --format json
```

## Planned durable JetStream profile

The planned runner accepts `run --backend kind|gke --tape-image <digest>
--nats-image <digest> --profile durable-v1 --output <empty-dir>`. It pins one
amd64 NATS Server digest in version control and rejects mutable tags, a wrong
architecture, a different digest, or a non-empty output directory.

It will give Tape WAL and JetStream FileStore equal dedicated 2 vCPU, 8 GiB,
50 GiB `pd-balanced` resources, with a separate fixed tester node. The formal
matrix uses 128 B, 1 KiB, and 4 KiB payloads at 1, 16, and 64 clients. Each
cell warms up, records five interleaved 60-second publish samples, and replays
100,000 persisted messages.

Every cell must survive forced pod deletion and restart without message loss,
duplicates, or invalid messages. Pull acknowledgement correctness uses batches
of 100. The runner records acknowledgement timing but does not compare it.

Kind is the automatic preflight and must leave no cluster. GKE is an isolated
Terraform proof only after explicit authorization. It must preserve evidence,
destroy run-scoped resources, and prove provider cleanup. The unique
`tape-vs-jetstream-report.v1.json` records the image digests, NATS version,
resource and storage profile, workload seed, raw samples, recovery results,
verdict, diagnostics, artifact SHA-256, and `cleanup.json`.

Only a complete authorized GKE report may state the narrow result: correct
recovery in every formal workload, Tape throughput at least JetStream, and
Tape p99 no higher than JetStream. A failed cell is recorded as failed; the
runner does not rerun it to hide the result.

## Scale vocabulary

| class | meaning | current repo posture |
|---|---|---|
| Local regression | 1,000 events | release-mode gate, listed in `CONTRIBUTING.md` |
| Bounded stability | repeated restarts over minutes | `cargo test -p tape --test long_running_stability` |
| Multi-hour soak | `apps/tape/scripts/soak.sh` | manual, not a gate |
| Scale transition under load | replica-count change on GKE | not proven; see [ROADMAP.md](../ROADMAP.md#quotas-and-scale-transition) |

## History: retired peer-broker calibrations

These numbers were measured once and are not a product claim. The harnesses
(`tape_vs_nats_jetstream.rs`, `tape_vs_kafka.rs`, the competitor feature
baseline fixture, and the `rskafka` and `async-nats` dev-dependencies) were
removed on 2026-08-26.

- 2026-07-18, NATS JetStream: release Tape h2c and a local `nats-server -js`
  each replayed the same durable 20,000-event, 128-byte backlog over five
  validated full-replay samples. Tape p50 13,586 us against JetStream 27,384
  us, a 2.02x ratio; throughput 967,614 against 748,828 events/s; RSS 31.9
  MB against 51.9 MB; zero errors on both sides.
- 2026-07-18, Kafka KRaft: the same backlog against a pinned
  `apache/kafka:3.9.0` single-node broker in Docker with an `rskafka`
  consumer. Tape p50 13,575 us against Kafka 55,243 us, a 4.07x ratio. The
  earlier 20x and 110x figures compared in-process memory with a network
  client and were never valid evidence.
- Redpanda, Pulsar, and RabbitMQ Streams were never calibrated. RabbitMQ topic
  exchange was classified as not a replay baseline.

The Python scaling and durability proofs that lived under the retired
`external-contracts/` tree — a durable-throughput ceiling of 3.0 × N × the
WAL barrier rate with a 4.0 ratio floor, and twelve refused-probe durability
checks — had no Rust counterpart and were abandoned with that tree. The Rust
gates above are the whole of what is measured today.
