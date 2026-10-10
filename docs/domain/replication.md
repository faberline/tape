# replication

replication runs tape as a raft group. It implements the shared kernel's
`Replicator` port over core's `raft-runtime`: one host replicates every
topic's writes, and every replica applies them through the same
`apply_command` as the single-node path. Peer TLS parsing and the reloadable
peer transport come from core's `peer-tls` and `raft-runtime`; this context
holds tape's state machine, its bootstrap seeding and its environment names.

**Form:** infrastructure only · **Depends on:** — (shared kernel only) · **Source:** [`crates/tape-replication`](../../crates/tape-replication/src/lib.rs)

## Model

- **State machine** — `raft::TapeStateMachine`: a
  `raft_runtime::RaftStateMachine` whose command is `TapeCommand` and whose
  snapshot is the shared kernel's `JournalSnapshot`, taken every
  `raft::SNAPSHOT_EVERY` (1024) applied entries.
- **Host** — `raft::TapeRaft`: the `Replicator`. It proposes a command and
  reads back this node's `TapeOutcome`, and reports the applied index and the
  snapshot at it. tape has one group and no shards. On SIGTERM `tape serve`
  calls `quiesce_proposals` and then `shutdown_within(deadline)`, which hands
  leadership to a caught-up voter and drains peer RPCs before it reports.
- **Bootstrap seed** — `raft::prepare_bootstrap_seed` restores an empty
  replica from one `/admin/backup` object before its first election.
  `raft::data_dir_has_existing_state` is the emptiness check it and its
  callers share; a fresh volume's `lost+found` does not count as state.
- **Peer TLS** — `peer_tls::from_env` reads the `TAPE_PEER_*` environment and
  `peer_tls::peer_transport` turns it into raft-runtime's peer transport.

## Invariants

- Every write proposes through the group; reads stay node-local against the
  journal the state machine mutates.
- A restarted host replays every committed entry it holds into a fresh state
  machine before it accepts new proposals; a snapshot restores the whole
  journal first.
- Completed proposal ids travel in the snapshot, so a retried proposal applies
  once.
- Bootstrap seeding refuses a data directory that already holds raft state.
- Shutdown closes the public listener only after the host has shut down:
  without peer mTLS the raft routes share that listener, so closing it first
  would cut the leadership handoff. The dedicated mTLS peer listener closes
  gracefully only when the host reports `peer_listener_close_safe`, and is
  aborted otherwise.

## Exceptions and debts

- No checker exceptions.
- **Debts:** older `applied-*.idx` and `snapshot-*.json` files are still read
  as a migration path.
