# Pull subscription wire format

## Status

This is a design note for #12 and #13. Nothing here is implemented. It fixes
four wire-format points: offsets, checkpoint compare-and-set, seek epochs and
commit retry. Each point names the present behaviour with its file and
function, a rule, an example and the test that will prove it later. Where
the present code has no answer, the note gives two options and a
recommendation. The desk decides each `Decision needed`.

## 1. Offsets

### Present behaviour

- Offsets are `u64`. They start at 0 and each event gets one, assigned by
  append (`TapeJournal::append_at`, `crates/tape-shared-kernel/src/tape_journal.rs`).
- `from_offset` is inclusive. `replay_refs`
  (`crates/tape-shared-kernel/src/tape_journal.rs`, line 89) keeps events
  with `event.offset >= from_offset`. When it is omitted, the start is 0.
- A cursor is the offset of the next event to read. It is exclusive of
  everything before it. `pull_subscription` (`tape_journal.rs`, line 208)
  reads from the checkpoint offset, or from 0 when the subscription has no
  checkpoint. `next_offset` is the last event offset plus 1, or the cursor
  when the window is empty (lines 227 to 230).
- `end_offset(topic)` is the next offset to be assigned. A checkpoint may
  equal it (an empty tail) but may not exceed it.

### Rule

- The offset of an event is inclusive in a replay. The cursor of a pull is
  the next offset to read. The initial cursor of a subscription with no
  checkpoint is 0.

### Example

Topic `orders` has events at offsets 0, 1 and 2.

- `POST /topics/orders/subscriptions/s1/pull` with `limit` 2 and no
  checkpoint returns `cursor` 0, the events 0 and 1, and `next_offset` 2.
- `POST /topics/orders/subscriptions/s1/ack` with `offset` 2, then a pull,
  returns `cursor` 2, the event 2, and `next_offset` 3.

### Proving test

- Existing: `pull_subscription_uses_checkpoint_cursor_and_never_implicitly_acks`
  in `crates/tape-shared-kernel/src/tape_journal/tests.rs`, and
  `a_backward_or_beyond_end_ack_keeps_the_cursor` in
  `crates/tape/tests/it/properties.rs`.
- To write later: a pull with no checkpoint starts at offset 0.

## 2. Checkpoint compare-and-set

### Present behaviour

- `put_checkpoint_at` (`tape_journal.rs`, line 122) is a monotonic write. It
  is not a compare-and-set. It refuses an `offset` above `end_offset` with
  `TapeError::CheckpointBeyondEnd`. It refuses an `offset` below the stored
  one with `TapeError::StaleCheckpoint` (`crates/tape-shared-kernel/src/error.rs`).
  An equal offset is accepted, so a repeated write is idempotent.
- Every `TapeError` becomes `JournalError::Conflict`
  (`crates/tape-journal/src/application/error.rs`), which the HTTP layer maps to
  409 (`crates/tape-journal/src/interfaces/http/errors.rs`, line 30). The
  checkpoint put and the subscription ack both return this 409. The OpenAPI
  text in `crates/tape-journal/src/interfaces/http/topics.rs` (line 155) reads
  "Stale or beyond-end checkpoint offset". The 409 message names the offsets,
  for example `checkpoint offset 1 is behind existing offset 2`. It is text,
  not a field.
- `ConsumerCheckpoint` (`crates/tape-shared-kernel/src/event.rs`, line 19) has
  the fields `topic`, `consumer`, `offset` and `updated_at_ms`. It has no
  version and no epoch.
- Existing tests: `errors_render_the_shared_envelope` in
  `crates/tape/tests/it/http_transport.rs`, and
  `a_backward_or_beyond_end_ack_keeps_the_cursor` in
  `crates/tape/tests/it/properties.rs`.

### Rule

- Today a checkpoint only moves forward. A write to an offset below the
  stored one is refused, and a write to an offset above the topic end is
  refused.

### Decision needed

A competing writer can move the cursor forward past events that a pull has
not yet returned. This is a real product choice.

- Option A: keep the monotonic rule. Add an optional `expected_offset` to
  the ack body. When it is present and differs from the stored cursor, answer
  409 and name the current offset in the message. When it is absent, keep
  today's behaviour.
- Option B: add a `version` to `ConsumerCheckpoint`. It is a u64 that starts
  at 0 with no checkpoint and rises by 1 on every accepted write. The ack
  sends `expected_version`. A mismatch answers 409 and names the current
  version. This changes the stored type, so the raft snapshot format changes
  (`error.rs` says the outcomes are part of the snapshot format).
- Recommendation: Option A. It is smaller and keeps the snapshot format.

Example for the recommended option: the cursor is 2. `POST .../ack` with
body `{"offset":3,"expected_offset":1}` answers 409. The same request with
`"expected_offset":2` answers 200 and a checkpoint at 3.

### Proving test

To write later: `ack_with_stale_expected_offset_is_409` in
`crates/tape/tests/it/http_transport.rs`.

## 3. Seek epochs

### Present behaviour

- No seek exists. `ROADMAP.md#seek-snapshot-and-retention` owns it. The
  cursor is cumulative and monotonic. The comment on `put_checkpoint_at`
  (`tape_journal.rs`, line 118) says there is "no admin rewind verb".

### Decision needed

- Option A: each subscription has an `epoch`, a u64 that starts at 0. A seek
  sets the cursor, which may move backwards, and adds 1 to `epoch`. Pull
  responses carry `epoch`. An ack must send the `epoch` it was pulled under.
  An ack with an older epoch answers 409 `stale_epoch` and changes nothing. So
  a response that was in flight during a seek is harmless, because its ack is
  refused.
- Option B: no epoch. A seek is refused with 409 while any pull window is
  unacked. This needs an ack deadline that does not exist yet, so it depends
  on `ROADMAP.md#subscription-ack-and-competing-subscribers`.
- Recommendation: Option A.

Example for Option A: the epoch is 0 and the cursor is 5. A puller pulls the
window 5 to 7 under epoch 0. `POST .../seek` with `{"offset":0}` answers
`{"epoch":1,"cursor":0}`. The puller's `POST .../ack` with
`{"offset":7,"epoch":0}` answers 409 `stale_epoch`. A new pull returns
`cursor` 0 and `epoch` 1.

### Proving test

To write later: `ack_after_seek_with_old_epoch_is_409` in a new file under
`crates/tape/tests/it/`, named by the seek issue.

## 4. Commit with an unknown result

### Present behaviour

- Single node: `JournalService::apply_mutation`
  (`crates/tape-journal/src/application/service/mutations.rs`, line 195)
  returns an io error only for a durability failure. A domain rejection is a
  normal outcome.
- Raft: `propose_and_claim` (`crates/tape-replication/src/raft/host.rs`,
  line 296) returns `None` when the node did not apply the proposal within
  `PROPOSAL_OUTCOME_WAIT` (5 seconds, line 25). The caller then does not know
  whether the write committed.
- Append has no deduplication. A retry gets a new offset
  (`docs/delivery-semantics.md`).
- A repeated ack is safe today. The same offset is accepted again, because of
  the monotonic rule in section 2.

### Rule

- Ack, checkpoint put and seek are idempotent by offset (and by epoch). On an
  unknown result the client retries the same request. A 409 whose message
  names an existing offset equal to or beyond the offset the client sent
  means the first attempt committed. The client treats it as success.
- Append is not idempotent. After an unknown result the client reads back
  with `GET /topics/{topic}/replay?from_offset=<end offset known before the
  append>` and looks for its own `key`. Only if the key is absent does it
  append again. This relies on a unique `key` that the client chooses.

### Decision needed

- Option A: keep this client-side rule. No server change.
- Option B: add a server-side idempotency key to append.
- Recommendation: Option A. Option B is a new product promise, and it
  conflicts with `docs/delivery-semantics.md`.

Example: an ack with `{"offset":3}` times out. The client retries. The
response is 200 (the first attempt was unknown), or 409 with a message that
names the existing offset 3. The client treats both as success.

### Proving test

To write later: `ack_retry_after_commit_is_success_or_stale_at_same_offset`
in `crates/tape/tests/it/http_transport.rs`.
