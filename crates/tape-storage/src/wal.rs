//! Append-only, group-commit write-ahead log for `tape serve --data-dir`
//! (WI #3052), replacing the per-request whole-file journal rewrite in
//! the legacy whole-file store ([`super::file_log::FileLog`]).
//!
//! # What one frame encodes
//!
//! A frame holds one JSON-encoded [`TapeCommand`], never [`TapeJournal`]
//! state. [`TapeJournal::append_at`] calls
//! `enforce_retention` as a side effect of appending, which can *delete*
//! events; logging post-mutation state would silently lose that
//! deleted-event history. Logging the command and replaying it through the
//! shared [`apply_command`] reproduces retention enforcement
//! (and every other side effect) identically on every replay -- the exact
//! same function the Raft-replicated path applies through, so the two
//! cannot drift.
//!
//! # Layout
//!
//! Both paths live directly under the caller's `--data-dir`, with fixed
//! names chosen so neither can collide with the `.storage_full_probe` file
//! the ENOSPC re-probe loop writes there (`crates/tape/src/bin/tape/serve.rs`):
//!
//! - WAL: `<dir>/journal.wal`
//! - Snapshots: `<dir>/journal-<seq>.snap` via [`SnapshotFileStore`]
//!
//! Nothing here scans the directory for arbitrary segment files.
//!
//! # Group commit
//!
//! [`WalStore::commit`] encodes and appends every command in a batch, then
//! performs exactly **one** `fsync` covering the whole batch, and only then
//! takes the journal lock to apply the commands in order. The lock is never
//! held across the fsync. If any append or the sync fails, the batch fails
//! closed: no command in it is applied, and the caller gets the error back
//! with both its [`std::io::ErrorKind`] and its `errno` intact (see
//! [`DurabilityFailure`] -- the kind alone carries ENOSPC but not EIO), so
//! `JournalService::apply_mutation` can tell a durability failure that must latch
//! degraded read-only mode from an ordinary one. That single
//! failed batch is not the only thing at risk: a failure can still have
//! landed some of its frames on disk (an `append` cannot be undone), so a
//! later batch reusing the same starting seq would produce a duplicate
//! on-disk frame and replay it twice. `WalStore` closes that window by
//! poisoning itself on any durability failure -- see the `poisoned` field
//! doc comment on the struct -- so every subsequent `commit` fails until the
//! caller reopens from disk.
//!
//! # Recovery
//!
//! [`WalStore::open`] loads the newest snapshot (if any), decodes it into a
//! [`TapeJournal`], then replays every WAL frame after the snapshot's
//! sequence through `apply_command`. [`FramedLogWriter::open`] truncates a
//! torn tail (a partial frame from a crash mid-write) before this module
//! ever reads a byte, and [`FramedLogReader::read_frames`] stops cleanly at
//! the first unreadable frame -- AC5 ("recovers all prior records and drops
//! only the torn one") is a property of using those two calls correctly,
//! not logic this module reimplements.

use std::path::Path;

use storage_durable::{FsyncPolicy, SnapshotFileStore};

#[cfg(test)]
use tape_shared_kernel::TapeCommand;
use tape_shared_kernel::TapeJournal;

mod coordinator;
mod io_error;
mod store;
#[cfg(test)]
mod tests;

pub use coordinator::CommitCoordinator;
use io_error::{flatten_io_error, json_err};
pub use store::WalStore;

/// Fixed WAL filename under `--data-dir`. Chosen so it cannot collide with
/// `.storage_full_probe` (written by `spawn_storage_full_reprobe` in
/// `crates/tape/src/bin/tape/serve.rs`).
const WAL_FILE_NAME: &str = "journal.wal";

/// Snapshot file prefix/extension under `--data-dir`: `journal-<seq>.snap`.
const SNAPSHOT_PREFIX: &str = "journal";
const SNAPSHOT_EXTENSION: &str = "snap";

/// How many committed frames accumulate before [`WalStore::commit`] drives a
/// snapshot + WAL truncate. Mirrors the shape of `raft::SNAPSHOT_EVERY`.
/// [`WalStore::open_with_snapshot_threshold`] is the real configuration seam
/// step 3 wires up (e.g. from a CLI flag or env var); the unit tests below
/// are just its first consumer, using a small value so they don't need to
/// drive a thousand real fsyncs to exercise snapshot + truncate.
pub const DEFAULT_SNAPSHOT_THRESHOLD: u64 = 1024;

/// The legacy whole-file JSON journal name a pre-WI-#3052 `tape serve
/// --data-dir` wrote (`resolve_journal_store` in `crates/tape/src/bin/tape/serve.rs`
/// used to join this onto `--data-dir` before the WAL existed).
const LEGACY_JOURNAL_FILE_NAME: &str = "journal.json";

/// One-time upgrade path for a `--data-dir` that already has state from
/// before WI #3052: seed a WAL-store snapshot from the old whole-file
/// `journal.json` so [`WalStore::open`] (called right after this) recovers
/// the pre-existing journal instead of starting empty.
///
/// Migrates ONLY when it is unambiguous that this `dir` predates the WAL:
/// no `journal.wal` yet, no `journal-*.snap` yet, and a `journal.json` that
/// does exist. Any other combination is a no-op (`Ok(false)`) -- in
/// particular, a directory that already has a WAL or a snapshot is treated
/// as already migrated (or a from-scratch WAL deployment that happens to
/// share a `--data-dir` with an old file for unrelated reasons), never
/// re-migrated.
///
/// Deliberately never deletes `journal.json`: it is the rollback path if the
/// operator needs to downgrade back to a pre-#3052 build. This function is
/// purely additive -- it writes one new WAL-store snapshot file and touches
/// nothing else.
///
/// Returns `Ok(true)` when a migration snapshot was written, `Ok(false)`
/// when no migration was needed (including "nothing to migrate" and
/// "already migrated").
pub fn migrate_legacy_journal_file(dir: &Path) -> std::io::Result<bool> {
    let wal_path = dir.join(WAL_FILE_NAME);
    if wal_path.exists() {
        return Ok(false);
    }

    let legacy_path = dir.join(LEGACY_JOURNAL_FILE_NAME);
    if !legacy_path.exists() {
        return Ok(false);
    }

    let snapshots = SnapshotFileStore::new(
        dir,
        SNAPSHOT_PREFIX,
        SNAPSHOT_EXTENSION,
        FsyncPolicy::Always,
    )
    .map_err(flatten_io_error)?;
    if !snapshots.snapshots().map_err(flatten_io_error)?.is_empty() {
        return Ok(false);
    }

    let bytes = std::fs::read(&legacy_path)?;
    // Round-trip through `TapeJournal` (rather than copying raw bytes) so a
    // malformed legacy file surfaces as a decode error here, at startup,
    // instead of silently seeding a snapshot `WalStore::open` cannot parse
    // later.
    let journal: TapeJournal = serde_json::from_slice(&bytes).map_err(json_err)?;
    let snapshot_bytes = serde_json::to_vec(&journal).map_err(json_err)?;
    // Seed at seq 0: no WAL frames exist yet (checked above), so replay
    // after this snapshot starts from an empty WAL, exactly like a normal
    // fresh `WalStore::open` with one prior snapshot.
    snapshots
        .save(0, &snapshot_bytes)
        .map_err(flatten_io_error)?;
    Ok(true)
}
