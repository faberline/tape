//! The async handle onto the dedicated group-commit thread.

use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use super::io_error::clone_io_error;
use super::WalStore;
use tape_shared_kernel::{BoxFuture, CommitLog, TapeCommand, TapeJournal, TapeOutcome};

/// How many pending [`TapeCommand`]s [`CommitCoordinator`]'s loop drains into
/// one [`WalStore::commit`] batch before it stops accepting more for that
/// round. A cap rather than "drain everything queued" bounds worst-case
/// batch latency and memory under a request storm; `WalStore::commit`'s
/// group-commit fsync amortization does not need an unbounded batch to pay
/// off.
const MAX_COMMIT_BATCH: usize = 256;

/// One caller's pending mutation, queued for the next group-commit batch.
struct CommitRequest {
    command: TapeCommand,
    reply: oneshot::Sender<std::io::Result<TapeOutcome>>,
}

/// Single-node durable commit coordinator: the async-facing handle onto a
/// dedicated OS thread that owns the [`WalStore`] and drives its group
/// commit.
///
/// # Why a dedicated `std::thread`, not a `tokio::task`
///
/// [`WalStore::commit`] fsyncs -- a blocking syscall. Spawning it as an
/// ordinary `tokio::task` (even via `spawn_blocking`, which still borrows
/// from a bounded blocking-pool) would let the durable write path compete
/// with -- and under sustained load, starve -- the async runtime's request
/// handling, defeating the whole point of WI #3052 (replacing a serialized
/// per-request fsync with amortized group commit, not smuggling the same
/// blocking cost back into the runtime that serves HTTP). A `std::thread`
/// dedicated to exactly one `--data-dir`'s WAL is the coordinator's entire
/// job for the life of the process: one thread, one `WalStore`, one
/// `Mutex<TapeJournal>`.
///
/// # Wiring
///
/// [`Self::submit`] sends a [`CommitRequest`] down an unbounded
/// [`mpsc::Sender`] and awaits its [`oneshot::Receiver`] for the reply -- both
/// sides are safe to use from async code on any tokio worker. The dedicated
/// thread's loop blocks on [`mpsc::Receiver::blocking_recv`] for the first
/// request of a round, then drains up to [`MAX_COMMIT_BATCH`] more with
/// non-blocking `try_recv` so a request storm amortizes over one
/// `WalStore::commit` call instead of committing one command at a time.
/// `oneshot::Sender::send` never blocks and can be called from any thread,
/// so replying from the dedicated thread back to whichever tokio worker is
/// awaiting `submit` requires no additional synchronization.
pub struct CommitCoordinator {
    tx: mpsc::Sender<CommitRequest>,
}

impl CommitCoordinator {
    /// Spawn the dedicated commit thread and return the handle callers
    /// `submit` through. `store` and `journal` are moved onto the new thread
    /// (`journal` stays an `Arc` so callers -- e.g. `JournalService` -- keep a
    /// handle to read from it directly; the coordinator thread is simply
    /// another `Arc` owner that also mutates it via `WalStore::commit`).
    pub fn spawn(mut store: WalStore, journal: Arc<Mutex<TapeJournal>>) -> Self {
        // Bounded at one batch's worth: a queue deeper than one
        // `WalStore::commit` batch cannot help throughput (the dedicated
        // thread only ever drains `MAX_COMMIT_BATCH` per round) and instead
        // just hides backpressure from callers. `submit`'s `.send().await`
        // naturally yields the calling tokio worker back to the runtime
        // while the channel is full, rather than busy-waiting.
        let (tx, mut rx) = mpsc::channel::<CommitRequest>(MAX_COMMIT_BATCH);
        std::thread::spawn(move || {
            // `blocking_recv` parks this dedicated OS thread (not a tokio
            // worker) until the first request of a round arrives.
            while let Some(first) = rx.blocking_recv() {
                let mut batch = vec![first];
                while batch.len() < MAX_COMMIT_BATCH {
                    match rx.try_recv() {
                        Ok(request) => batch.push(request),
                        Err(_) => break,
                    }
                }
                let (commands, replies): (Vec<TapeCommand>, Vec<_>) = batch
                    .into_iter()
                    .map(|request| (request.command, request.reply))
                    .unzip();
                match store.commit(commands, &journal) {
                    Ok(outcomes) => {
                        for (reply, outcome) in replies.into_iter().zip(outcomes) {
                            // A dropped receiver (the submitting task was
                            // cancelled) is not this coordinator's problem --
                            // the command is already durably committed and
                            // applied either way.
                            let _ = reply.send(Ok(outcome));
                        }
                    }
                    Err(error) => {
                        // `std::io::Error` is not `Clone`: rebuild one per
                        // waiter so every caller in the failed batch can still
                        // discriminate ENOSPC/EIO from an ordinary failure --
                        // the property `JournalService::apply_mutation`'s
                        // degraded-mode mapping depends on. `kind()` alone is
                        // not enough to carry that; see [`DurabilityFailure`].
                        for reply in replies {
                            let _ = reply.send(Err(clone_io_error(&error)));
                        }
                    }
                }
            }
            // The sender half (and every clone of it) has been dropped --
            // e.g. process shutdown -- so this dedicated thread exits.
        });
        CommitCoordinator { tx }
    }

    /// Submit one command and await its durably-committed [`TapeOutcome`].
    /// Safe to call from any tokio worker; the actual fsync runs on the
    /// dedicated commit thread, never on the calling task's worker.
    pub async fn submit(&self, command: TapeCommand) -> std::io::Result<TapeOutcome> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CommitRequest {
                command,
                reply: reply_tx,
            })
            .await
            .map_err(|_| {
                std::io::Error::other("wal commit coordinator thread is no longer running")
            })?;
        reply_rx.await.map_err(|_| {
            std::io::Error::other(
                "wal commit coordinator dropped this request's reply before answering",
            )
        })?
    }
}

impl CommitLog for CommitCoordinator {
    fn commit(&self, command: TapeCommand) -> BoxFuture<'_, std::io::Result<TapeOutcome>> {
        Box::pin(self.submit(command))
    }
}
