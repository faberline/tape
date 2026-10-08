# storage

storage makes a write durable on a single node. It implements the shared
kernel's `CommitLog` port twice: the group-commit write-ahead log under
`--data-dir`, and the legacy whole-file store under `--store`. Durable files,
framed logs, snapshots and fsync come from core's `storage-durable`; this
context holds tape's log format and recovery.

**Form:** infrastructure only · **Depends on:** — (shared kernel only) · **Source:** [`crates/tape-storage`](../../crates/tape-storage/src/lib.rs)

## Model

- **WAL** — `wal::WalStore`: an append-only log of JSON-encoded
  `TapeCommand` frames at `<data-dir>/journal.wal`, plus snapshots at
  `<data-dir>/journal-<seq>.snap`. It logs commands, never journal state.
- **Group commit** — `wal::CommitCoordinator` batches concurrent commits:
  every command in a batch is appended, one `fsync` covers the batch, and only
  then are the commands applied to the journal in order. The journal lock is
  never held across the fsync.
- **Legacy store** — `file_log::FileLog`: applies under the journal lock and
  rewrites the whole journal file. With no path it applies without
  durability, which tests, ephemeral runs and raft replicas use.
- **Legacy migration** — `wal::migrate_legacy_journal_file` seeds a WAL
  snapshot from the `journal.json` a pre-WAL `--data-dir` holds, once, and
  never deletes the old file, so a downgrade still finds it.

## Invariants

- Recovery loads the newest snapshot and replays every later frame through the
  shared kernel's `apply_command`, the same function the raft path applies, so
  the two cannot drift. A torn tail frame is truncated before it is read.
- A failed batch applies none of its commands. Any durability failure poisons
  the `WalStore`, so every later commit fails until it is reopened from disk;
  a partly written batch can never be followed by a frame reusing its
  sequence numbers.
- A write failure keeps both its `ErrorKind` and its `errno`
  (`DurabilityFailure`), so the journal service can tell ENOSPC and EIO, which
  latch degraded mode, from other failures.

## Exceptions and debts

- No checker exceptions.
- **Debts:** `WalStore::inject_next_sync_failure_with_kind` is an always
  compiled, `#[doc(hidden)]` test seam, because the integration tests that
  call it are in another crate.
