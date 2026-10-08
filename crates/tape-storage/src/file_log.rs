//! The legacy whole-file journal store (`--store <file>`): every mutation is
//! applied under the journal lock and the WHOLE journal is rewritten. With no
//! path configured it applies without any durability (tests, ephemeral runs,
//! and raft replicas, where raft-runtime owns durability).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tape_shared_kernel::{
    apply_command, BoxFuture, CommitLog, TapeCommand, TapeJournal, TapeOutcome,
};

#[cfg(test)]
mod tests;

pub struct FileLog {
    journal: Arc<Mutex<TapeJournal>>,
    store: Option<PathBuf>,
}

impl FileLog {
    pub fn new(journal: Arc<Mutex<TapeJournal>>, store: Option<PathBuf>) -> Self {
        Self { journal, store }
    }

    /// Persist the journal to `--store`, when configured, mirroring the CLI's
    /// `save_journal`.
    ///
    /// Durability (#2572): the write goes through
    /// [`storage_durable::atomic_write`] — temp file, fsync, rename, parent
    /// directory fsync — so a crash, eviction, or ENOSPC mid-write leaves the
    /// *previous* journal intact rather than a truncated one. A plain
    /// `fs::write` truncates before writing and never fsyncs, which made a
    /// failed write destructive and a successful one unproven.
    ///
    /// [`storage_durable::FsyncPolicy::Always`] is deliberate: in single-node
    /// mode this file is tape's only durability guarantee. The journal is
    /// re-serialized in full on every mutation, so the fsync is not the term
    /// that dominates this write.
    ///
    /// Failures are reported to the caller, which owns the degraded-mode
    /// latch (`JournalService::apply_mutation`). This function preserves the
    /// failure's [`std::io::ErrorKind`] end to end (#2572) -- that is what
    /// makes the caller's discrimination possible at all.
    fn persist(&self, journal: &TapeJournal) -> std::io::Result<()> {
        let Some(path) = &self.store else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(journal)?;
        let result =
            storage_durable::atomic_write(path, &bytes, storage_durable::FsyncPolicy::Always)
                .map_err(flatten_atomic_write_error);
        if let Err(error) = &result {
            // #2572 preserved the ErrorKind through `atomic_write`'s context
            // chain precisely so this discrimination is possible: a full disk
            // is a durable condition retrying cannot clear, every other I/O
            // failure may well be transient.
            if error.kind() == std::io::ErrorKind::StorageFull {
                tracing::error!(
                    error = %error,
                    path = %path.display(),
                    "journal persist hit ENOSPC; JournalService::apply_mutation will latch \
                     degraded read-only mode"
                );
            }
        }
        result
    }
}

impl CommitLog for FileLog {
    fn commit(&self, command: TapeCommand) -> BoxFuture<'_, std::io::Result<TapeOutcome>> {
        Box::pin(async move {
            let mut journal = self.journal.lock().expect("journal mutex poisoned");
            let outcome = apply_command(&mut journal, command);
            self.persist(&journal).map(|()| outcome)
        })
    }
}

/// Collapse `storage_durable::atomic_write`'s `anyhow::Error` back into an
/// `io::Error` without losing either half of it (#2572).
///
/// The **kind** is preserved by downcasting through the context chain, so a
/// full disk still reports [`std::io::ErrorKind::StorageFull`] rather than
/// `Other` — that is what lets a caller distinguish "the disk is full" from
/// "the write failed", which #2573's degraded read-only mode depends on.
///
/// The **message** uses anyhow's alternate form so the operator-facing 500
/// keeps the whole chain (`commit durable replace … -> …: No space left on
/// device`) instead of only the outermost context.
fn flatten_atomic_write_error(error: anyhow::Error) -> std::io::Error {
    match error.downcast_ref::<std::io::Error>() {
        Some(source) => std::io::Error::new(source.kind(), format!("{error:#}")),
        None => std::io::Error::other(format!("{error:#}")),
    }
}
