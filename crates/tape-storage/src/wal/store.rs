//! The WAL writer, its snapshot store, and recovery.

use std::path::Path;
use std::sync::Mutex;

use storage_durable::{FramedLogReader, FramedLogWriter, FsyncPolicy, SnapshotFileStore};

use super::io_error::{flatten_io_error, json_err};
use super::{DEFAULT_SNAPSHOT_THRESHOLD, SNAPSHOT_EXTENSION, SNAPSHOT_PREFIX, WAL_FILE_NAME};
use tape_shared_kernel::{apply_command, TapeCommand, TapeJournal, TapeOutcome};

/// Single-node durable commit coordinator for one `--data-dir`'s journal.
///
/// Holds the open WAL writer and the snapshot store; does not hold the
/// [`TapeJournal`] itself -- callers pass the shared `Arc<Mutex<TapeJournal>>`
/// (or any `&Mutex<TapeJournal>`) into [`Self::commit`] each time, matching
/// how `TapeStateMachine` already shares one journal across call sites.
pub struct WalStore {
    wal: FramedLogWriter,
    snapshots: SnapshotFileStore,
    /// The seq the *next* appended frame will use.
    next_seq: u64,
    /// Committed frames since the last successful snapshot + truncate.
    frames_since_snapshot: u64,
    snapshot_threshold: u64,
    /// Set for the whole duration of a `commit`'s durable-write region (every
    /// `append` through `sync`), and cleared only once that region completes
    /// successfully. A durability failure anywhere in that region -- an
    /// `append` or the covering `sync` returning `Err` -- leaves this `true`
    /// and poisons the store: every subsequent `commit` fails immediately
    /// until the caller reopens from disk.
    ///
    /// This is not defensive extra caution; it is the fix for a real bug.
    /// `append`'s effects are not undoable once written (a partially
    /// appended batch cannot be "rolled back" out of the file), so a batch
    /// that fails mid-write may still have landed some or all of its frames
    /// on disk even though the batch was never acknowledged. If the *next*
    /// `commit` were allowed to proceed, it would reuse the same starting
    /// `next_seq` (never advanced because the failed batch's `?` returned
    /// before `next_seq` was updated), producing a second on-disk frame with
    /// the same seq as an already-landed one. `FramedLogReader::read_frames`
    /// filters by `seq > from_seq` only -- it does not deduplicate -- so a
    /// later replay would apply *both* frames: a duplicate append, or a
    /// duplicate ack. Poisoning the whole store closes that window instead
    /// of patching each failure site individually. Step 3 wires this state
    /// to the existing `TapeMetrics::mark_storage_degraded` sticky
    /// read-only/507 path.
    poisoned: bool,
    /// Fault-injection seam (WI #3052 AC7): when armed via
    /// [`Self::inject_next_sync_failure_with_kind`], the next [`Self::commit`]
    /// fails its sync with this [`std::io::ErrorKind`] instead of performing a
    /// real fsync -- e.g. `ErrorKind::StorageFull` to simulate ENOSPC
    /// originating INSIDE the WAL without needing a genuinely full disk.
    ///
    /// Deliberately NOT `#[cfg(test)]` (unlike the older
    /// [`Self::inject_next_sync_failure`] this replaces the body of below):
    /// an integration test under `crates/tape/tests/it/` links this crate as an
    /// ordinary, non-`cfg(test)` dependency, so a `#[cfg(test)]`-gated seam
    /// would not exist for `crates/tape/tests/it/durable_write_path.rs` to call at all.
    /// This is an honestly-named, always-present fault-injection hook, not a
    /// hidden backdoor: it only ever fires when a caller explicitly arms it,
    /// and arming requires holding a `&WalStore` in the first place.
    injected_sync_failure: Mutex<Option<std::io::ErrorKind>>,
}

impl WalStore {
    /// Open (or create) the WAL + snapshot store under `dir`, recovering a
    /// [`TapeJournal`] by replaying the newest snapshot plus every WAL frame
    /// after it. Returns the store positioned to append after the last
    /// replayed frame, and the recovered journal.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<(WalStore, TapeJournal)> {
        Self::open_with_snapshot_threshold(dir, DEFAULT_SNAPSHOT_THRESHOLD)
    }

    /// Same as [`Self::open`] with an explicit snapshot-trigger threshold --
    /// the seam a caller (step 3) configures the snapshot cadence through.
    pub fn open_with_snapshot_threshold(
        dir: impl AsRef<Path>,
        snapshot_threshold: u64,
    ) -> std::io::Result<(WalStore, TapeJournal)> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;

        let snapshots = SnapshotFileStore::new(
            dir,
            SNAPSHOT_PREFIX,
            SNAPSHOT_EXTENSION,
            FsyncPolicy::Always,
        )
        .map_err(flatten_io_error)?;
        // `load_latest` doesn't hand back the winning `seq`, which recovery
        // needs to bound the WAL replay -- so this reads the sorted listing
        // itself instead.
        let snapshot_files = snapshots.snapshots().map_err(flatten_io_error)?;
        let (mut journal, snapshot_seq) = match snapshot_files.last() {
            Some(latest) => {
                let bytes = std::fs::read(&latest.path)?;
                let journal: TapeJournal = serde_json::from_slice(&bytes).map_err(json_err)?;
                (journal, latest.seq)
            }
            None => (TapeJournal::default(), 0),
        };

        let wal_path = dir.join(WAL_FILE_NAME);
        // Opening the writer truncates a torn tail as a side effect (scans to
        // the last good frame boundary and `set_len`s past it) before we read
        // anything below -- see the module doc comment.
        let wal = FramedLogWriter::open(&wal_path, FsyncPolicy::Os).map_err(flatten_io_error)?;

        let frames =
            FramedLogReader::read_frames(&wal_path, snapshot_seq).map_err(flatten_io_error)?;
        let mut next_seq = snapshot_seq + 1;
        let mut frames_since_snapshot = 0u64;
        for frame in frames {
            let command: TapeCommand = serde_json::from_slice(&frame.payload).map_err(json_err)?;
            apply_command(&mut journal, command);
            next_seq = frame.seq + 1;
            frames_since_snapshot += 1;
        }

        Ok((
            WalStore {
                wal,
                snapshots,
                next_seq,
                frames_since_snapshot,
                snapshot_threshold,
                poisoned: false,
                injected_sync_failure: Mutex::new(None),
            },
            journal,
        ))
    }

    /// Group-commit one batch of pending commands: encode + append every
    /// command, one fsync barrier over the whole batch, then apply them in
    /// order under a single lock acquisition. Fails closed -- if any append
    /// or the sync errors, this returns `Err` and not one command in
    /// `commands` has been applied to `journal`. A durability failure (as
    /// opposed to the store already being poisoned from a prior one) also
    /// poisons the store for every subsequent call -- see the `poisoned`
    /// field doc comment for why that is required, not optional caution.
    pub fn commit(
        &mut self,
        commands: Vec<TapeCommand>,
        journal: &Mutex<TapeJournal>,
    ) -> std::io::Result<Vec<TapeOutcome>> {
        if commands.is_empty() {
            return Ok(Vec::new());
        }

        if self.poisoned {
            return Err(std::io::Error::other(
                "wal store poisoned by an earlier durability failure; \
                 no further commits until reopen",
            ));
        }

        // Set BEFORE entering the durable-write region so every early return
        // below (`?`, or the test-only injected sync failure) leaves the
        // store poisoned by construction -- there is no failure site that
        // has to remember to poison it by hand.
        self.poisoned = true;

        let base_seq = self.next_seq;
        for (i, command) in commands.iter().enumerate() {
            let seq = base_seq + i as u64;
            let payload = serde_json::to_vec(command).map_err(json_err)?;
            self.wal.append(seq, &payload).map_err(flatten_io_error)?;
        }

        if let Some(kind) = self
            .injected_sync_failure
            .lock()
            .expect("injected_sync_failure mutex poisoned")
            .take()
        {
            // Injected right where the real `sync()` call below would fail:
            // every frame in this batch has already been appended (as a real
            // `sync` failure would leave it), but nothing has been synced or
            // applied yet.
            return Err(std::io::Error::new(
                kind,
                "injected sync failure (fault-injection seam)",
            ));
        }

        // The single group-commit barrier: one fsync covers every frame just
        // appended above. The journal lock is not held here or above -- it is
        // only taken in the apply loop below, after this line has already
        // returned `Ok`.
        self.wal.sync().map_err(flatten_io_error)?;
        self.next_seq = base_seq + commands.len() as u64;
        // Only a fully successful durable-write region un-poisons the store.
        self.poisoned = false;

        // WAL-order-equals-apply-order: this loop walks `commands` -- the
        // exact same `Vec`, in the exact same order -- that the append loop
        // above just walked and synced. Frame `base_seq + i` on disk and the
        // i-th outcome applied here always correspond to the same command,
        // because nothing reorders `commands` between the two loops.
        let mut outcomes = Vec::with_capacity(commands.len());
        {
            let mut journal = journal.lock().expect("journal mutex poisoned");
            for command in commands {
                outcomes.push(apply_command(&mut journal, command));
            }
        }

        self.frames_since_snapshot += outcomes.len() as u64;
        if self.frames_since_snapshot >= self.snapshot_threshold {
            let last_seq = self.next_seq - 1;
            // Serializing the snapshot is inside the swallowed region for the
            // same reason `snapshot_and_truncate` itself is: the batch is
            // already synced and applied by now, so an encode failure here
            // must not be reported as a failed commit. A `?` on this line
            // would hand step 3 an `Err` for a mutation that in fact
            // succeeded -- and step 3 maps `Err` to 507 + sticky degraded,
            // which is exactly the shape that invites a client retry and a
            // duplicate append.
            let snapshot_result = {
                let journal = journal.lock().expect("journal mutex poisoned");
                serde_json::to_vec(&*journal).map_err(json_err)
            }
            .and_then(|bytes| self.snapshot_and_truncate(last_seq, &bytes));
            match snapshot_result {
                Ok(()) => self.frames_since_snapshot = 0,
                Err(error) => {
                    // The batch above is already durably committed and
                    // applied; a snapshot/truncate hiccup only means the WAL
                    // keeps growing until the next successful attempt, not
                    // that this commit failed.
                    tracing::warn!(
                        %error,
                        "wal: snapshot+truncate failed; WAL will keep growing until the next successful attempt (the batch itself is committed)"
                    );
                }
            }
        }

        Ok(outcomes)
    }

    /// Save a snapshot at `seq`, truncate the WAL through it, and keep only
    /// the newest snapshot file.
    ///
    /// This is deliberately a bare `serde_json::to_vec(&TapeJournal)`, NOT
    /// the same bytes as `GET /admin/backup` / `raft::snapshot_bytes`, which
    /// serialize `raft::JournalSnapshot { up_to, journal, completed_proposals
    /// }`. The two formats are intentionally different: this snapshot is a
    /// purely internal recovery artifact for `WalStore::open`'s own replay,
    /// with no raft applied-index or proposal-dedupe concerns, and it is
    /// never read by anything outside this module. The #3052 out-of-scope
    /// boundary ("snapshot/backup wire format is unchanged") and AC6 ("`GET
    /// /admin/backup` is byte-identical to the old path") both live entirely
    /// on the `raft::snapshot_bytes` / `/admin/backup` side, which this
    /// function never touches.
    ///
    /// A failure here does NOT poison the store the way [`Self::commit`]'s
    /// durable-write region does: by the time this runs, the batch that
    /// triggered it is already durably synced AND applied to `journal`. A
    /// snapshot/truncate failure only means the WAL keeps growing instead of
    /// being compacted -- it is a maintenance hiccup, not an unresolved
    /// durability gap, so `commit` logs and continues rather than poisoning.
    fn snapshot_and_truncate(&mut self, seq: u64, snapshot_bytes: &[u8]) -> std::io::Result<()> {
        self.snapshots
            .save(seq, snapshot_bytes)
            .map_err(flatten_io_error)?;
        self.wal.truncate_through(seq).map_err(flatten_io_error)?;
        self.snapshots.prune(1).map_err(flatten_io_error)?;
        Ok(())
    }

    /// Test-only fault injection: the next [`Self::commit`] call appends its
    /// frames normally (so they can land on disk unsynced, matching a real
    /// crash-during-sync) and then fails exactly where the real `sync()`
    /// call would run, before anything is applied to the journal (mirrors
    /// `JournalService::inject_storage_full`).
    #[cfg(test)]
    pub(super) fn inject_next_sync_failure(&self) {
        self.inject_next_sync_failure_with_kind(std::io::ErrorKind::Other);
    }

    /// Fault-injection seam for WI #3052 AC7: arm the next [`Self::commit`]
    /// call to fail its sync with `kind` -- e.g.
    /// `std::io::ErrorKind::StorageFull` to simulate ENOSPC originating
    /// INSIDE the WAL (as opposed to `JournalService::set_inject_storage_full`,
    /// which short-circuits BEFORE the durable backend is ever reached and
    /// so cannot exercise this path -- see
    /// `crates/tape/tests/it/durable_write_path.rs`). Frames from the armed batch
    /// still land on disk (matching a real crash-during-sync), and the
    /// failure poisons the store exactly as a genuine sync failure would --
    /// see the `poisoned` field doc comment.
    ///
    /// Call this on the `WalStore` BEFORE handing it to
    /// [`CommitCoordinator::spawn`] (which moves it onto the dedicated commit
    /// thread and does not expose it again): the injected kind is consumed
    /// by the very next `commit`, whichever caller reaches it first.
    pub fn inject_next_sync_failure_with_kind(&self, kind: std::io::ErrorKind) {
        *self
            .injected_sync_failure
            .lock()
            .expect("injected_sync_failure mutex poisoned") = Some(kind);
    }
}
