# journal

journal is tape itself: append-only topics of JSON events, per-consumer
checkpoints, named pull subscriptions, and per-topic retention. A write is one
`TapeCommand`; it becomes durable through either a single-node commit log or a
raft group, and both apply it through the same function, so they cannot drift.

**Form:** application + interfaces over the shared kernel · **Depends on:** access · **Source:** [`crates/tape-journal`](../../crates/tape-journal/src/lib.rs) and the model in [`crates/tape-shared-kernel`](../../crates/tape-shared-kernel/src/lib.rs)

## Model

The model lives in `tape-shared-kernel`, because the storage and
replication contexts build on it too.


- **Topic** — a name keyed into the journal. A topic exists once something is
  appended to it or a policy names it; there is no create call.
- **Event** — `TapeEvent`: topic, offset, `timestamp_ms`, an optional key, and
  a JSON payload. Offsets in a topic start at 0 and increase by one per append.
- **End offset** — the offset the next append will get. It survives retention
  removing every event, because the journal keeps `next_offsets` per topic.
- **Consumer checkpoint** — `ConsumerCheckpoint`: the offset a named consumer
  has processed up to in a topic, and when it was set.
- **Subscription** — `Subscription`: a named cursor on a topic. A pull reads a
  `PullSubscriptionBatch` window starting at the subscription's checkpoint; an
  ack advances that checkpoint.
- **Retention policy** — `RetentionPolicy`: an explicit `min_offset`, a
  `max_age_seconds` window, and `protected_consumers` whose checkpoints bound
  how far retention may remove. `RetentionOutcome` reports what one application
  of it removed.
- **Journal** — `TapeJournal`, the aggregate holding all of the above. Every
  method is deterministic; the caller passes the time.
- **Command and outcome** — `TapeCommand` is one write (append, checkpoint
  put, subscription create/delete/ack, retention put). `apply_command` applies
  it to a journal and returns a `TapeOutcome`. Only the command crosses the
  raft log or the WAL; the outcome stays local.
- **Snapshot** — `JournalSnapshot`: the whole journal at a raft applied index
  (`up_to`), plus completed proposal ids so a retried proposal applies once.
  `encode_snapshot` writes it. The same bytes are a raft snapshot, the body of
  `GET /admin/backup`, and a bootstrap seed.
- **Replay wire** — `replay_wire`, the compact binary replay stream
  (`application/vnd.tape.replay.v1`) for draining a large backlog.
- **Durability failure** — `DurabilityFailure` keeps a write failure's `errno`
  through the `std::io::Error` rebuilds on the write path.
  `should_enter_storage_degraded_mode` is true for ENOSPC and EIO, which latch
  the server read-only.

## Ports

Defined in the shared kernel, implemented by the storage and replication
contexts, held by the application layer as trait objects:

- `CommitLog` — single-node durability: apply a command and make it durable
  before returning the outcome. Implemented in [storage](storage.md).
- `Replicator` — replicated durability: propose a command and read back this
  node's outcome; report the applied index and the snapshot at it.
  Implemented in [replication](replication.md).

## Use cases

`JournalService` (application) is the only entry point the interfaces use:

- **Reads** — `replay`, `replay_stream`, `checkpoint`, `subscriptions`,
  `subscription`, `pull`, `retention`, `backup_snapshot`, `render_metrics`.
  Reads are node-local, including on raft followers.
- **Mutations** — `append`, `put_checkpoint`, `create_subscription`,
  `delete_subscription`, `ack_subscription`, `put_retention`. Each reads the
  clock once (`now_ms`), builds the command, and sends it through the
  `Replicator` when one is set, otherwise through the `CommitLog`.
- **Clock-stamped journal calls** — `JournalClock` gives callers that hold a
  bare `TapeJournal` (the offline tools, the bench, tests) `append`,
  `put_checkpoint` and `ack_subscription` stamped with `now_ms`; the kernel
  itself only has the `*_at` forms that take the time.
- **Degraded mode** — after a durability failure that
  `should_enter_storage_degraded_mode` accepts, `storage_writable` fails every
  mutation until the periodic storage re-probe (started by `tape serve`)
  clears it; reads keep working.
- **Drain** — `start_drain` makes `/readyz` report not ready. `tape serve`
  calls it first on SIGTERM, before the raft handoff and the listener drains.

## Interfaces

- `interfaces::http` — the axum handlers for `/topics/...` and `/admin/backup`,
  their DTOs, error mapping, and the utoipa document served at
  `/openapi.json`. Every handler calls `tape_access::authorize` before
  touching the service (see [access](access.md#published-language) for the
  roles).
- `interfaces::spec` — the hand-written offline contract `tape spec` and
  `tape llm` print. `crates/tape/tests/it/spec_route_parity.rs` keeps its route list equal
  to the router's, and `clients/openapi.json` is its committed output.

## Invariants

- `apply_command` is pure: every timestamp is in the command. Replicas and WAL
  replay therefore reach the same state.
- The WAL logs commands, never state, because an append can trigger retention
  that removes events. Replaying commands reproduces that removal.
- A checkpoint never moves backwards (`StaleCheckpoint`) and never passes the
  end offset (`CheckpointBeyondEnd`). A subscription ack is a checkpoint put
  and keeps both guards.
- A pull has no side effect; only an ack moves the cursor. A pull limit above
  `MAX_PULL_BATCH` (1000) is rejected, and a replay without a limit returns at
  most `MAX_PULL_BATCH` events.
- Retention removes events below the larger of `min_offset` and the age
  boundary, but never past the lowest checkpoint of a protected consumer.
  Event time and evaluation time are separate, so backfilling old events does
  not rewind the retention clock.
- Deleting a subscription keeps its checkpoint.

## Exceptions and debts

- **Checker exceptions:**
  - B2 (`utoipa`), long-term: `TapeEvent`, `ConsumerCheckpoint`,
    `Subscription`, `PullSubscriptionBatch`, `RetentionPolicy` and
    `RetentionOutcome` derive `ToSchema` in the shared kernel, a compile-time
    description of the same serde shape. They are served as-is in the OpenAPI
    document.
- **Debts:**
  - `JournalService` keeps the whole journal in memory behind one mutex; a
    snapshot is the whole journal.
  - The domain error types are serialized inside raft outcomes, so their shape
    is part of the snapshot format.
