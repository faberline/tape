//! The journal use cases every serving path shares: reads against the
//! node-local journal, and mutations that either propose through the raft
//! group or commit through the single-node durable log.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::error::JournalError;
use super::metrics::TapeMetrics;
use tape_shared_kernel::{
    encode_snapshot, replay_wire, CommitLog, ConsumerCheckpoint, PullSubscriptionBatch, Replicator,
    RetentionPolicy, Subscription, SubscriptionError, TapeEvent, TapeJournal,
};

mod exposition;
mod mutations;

/// Media type of the compact replay stream body.
pub const REPLAY_CONTENT_TYPE: &str = replay_wire::CONTENT_TYPE;

/// The journal (behind a `std::sync::Mutex` — an in-memory `BTreeMap` core
/// with no async internal awaits), the per-op request metrics, the drain flag
/// `/readyz` reports, the single-node [`CommitLog`] every non-replicated
/// mutation commits through, and the optional [`Replicator`] (#1327) that
/// replaces it in HA (`REPLICAS_PER_SHARD > 1`) mode.
#[derive(Clone)]
pub struct JournalService {
    journal: Arc<Mutex<TapeJournal>>,
    metrics: Arc<TapeMetrics>,
    draining: Arc<AtomicBool>,
    log: Arc<dyn CommitLog>,
    replicator: Option<Arc<dyn Replicator>>,
    /// #2573: ENOSPC fault injection, armed via
    /// [`JournalService::set_inject_storage_full`]. Scoped to THIS service
    /// (not a process-global flag) so parallel `cargo test` threads sharing
    /// the same test binary never cross-contaminate. `Arc` because the
    /// service is `Clone` and the router hands a clone to every handler:
    /// arming the flag on the service a test holds has to be visible to the
    /// one the request runs against.
    inject_storage_full: Arc<AtomicBool>,
}

impl JournalService {
    /// Serve `journal`, committing single-node mutations through `log`.
    /// `log` must apply into the same `journal` handle.
    pub fn new(journal: Arc<Mutex<TapeJournal>>, log: Arc<dyn CommitLog>) -> Self {
        Self {
            journal,
            metrics: Arc::new(TapeMetrics::new()),
            draining: Arc::new(AtomicBool::new(false)),
            log,
            replicator: None,
            inject_storage_full: Arc::new(AtomicBool::new(false)),
        }
    }

    /// #3052: swap the single-node durable backend (e.g. onto the WAL
    /// group-commit path for `tape serve --data-dir`).
    pub fn set_log(&mut self, log: Arc<dyn CommitLog>) {
        self.log = log;
    }

    /// Attach the raft group (auto-mode HA serve path, #1327). Once set,
    /// every mutation proposes through it instead of the commit log.
    pub fn set_replicator(&mut self, replicator: Arc<dyn Replicator>) {
        self.replicator = Some(replicator);
    }

    /// The shared journal handle, for wiring a raft group or a WAL
    /// coordinator onto the SAME journal this service reads from.
    pub fn journal_handle(&self) -> Arc<Mutex<TapeJournal>> {
        Arc::clone(&self.journal)
    }

    /// The per-op request metrics `/metrics` renders.
    pub fn metrics(&self) -> Arc<TapeMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Flip readiness to draining so `/readyz` returns 503. Called on
    /// SIGTERM via `service_http::shutdown_with_drain`.
    pub fn start_drain(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// #2573: arm/disarm the next [`JournalService::apply_mutation`] call on
    /// THIS service to fail with a synthetic `io::ErrorKind::StorageFull`
    /// instead of touching the real store — the fault-injection seam that
    /// exercises the REAL production path (`apply_mutation` ->
    /// [`TapeMetrics::mark_storage_degraded`] ->
    /// [`JournalService::storage_writable`] -> the `507`/`storage_full`
    /// envelope) end to end without needing a genuinely full disk.
    ///
    /// A degraded mode that cannot be exercised in CI is one that will be
    /// wrong the first time it runs for real, so the seam is deliberate rather
    /// than a testing convenience. Deliberately NOT `#[cfg(test)]`, like
    /// `WalStore::inject_next_sync_failure_with_kind`: the tests that arm it
    /// live in the `tape` assembly crate, which links this crate as an
    /// ordinary dependency. It only fires when a caller holding the service
    /// arms it, and no request path can.
    #[doc(hidden)]
    pub fn set_inject_storage_full(&self, on: bool) {
        self.inject_storage_full.store(on, Ordering::SeqCst);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TapeJournal> {
        self.journal.lock().expect("journal mutex poisoned")
    }

    // ---- reads: node-local, and served while degraded ----

    pub fn replay(
        &self,
        topic: &str,
        from_offset: Option<u64>,
        from_timestamp_ms: Option<u64>,
        limit: Option<usize>,
    ) -> Vec<TapeEvent> {
        self.lock()
            .replay(topic, from_offset, from_timestamp_ms, limit)
    }

    /// The same window as [`Self::replay`], encoded as one compact
    /// [`REPLAY_CONTENT_TYPE`] body under the journal lock so no event is
    /// cloned.
    pub fn replay_stream(
        &self,
        topic: &str,
        from_offset: Option<u64>,
        from_timestamp_ms: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, JournalError> {
        let journal = self.lock();
        let events = journal.replay_refs(topic, from_offset, from_timestamp_ms, limit);
        replay_wire::encode(&events).map_err(|error| JournalError::Internal(error.to_string()))
    }

    pub fn checkpoint(&self, topic: &str, consumer: &str) -> Option<ConsumerCheckpoint> {
        self.lock().checkpoint(topic, consumer).cloned()
    }

    pub fn subscriptions(&self, topic: &str) -> Vec<Subscription> {
        self.lock().subscriptions(topic)
    }

    pub fn subscription(&self, topic: &str, name: &str) -> Result<Subscription, JournalError> {
        self.lock()
            .subscription(topic, name)
            .cloned()
            .ok_or_else(|| {
                SubscriptionError::NotFound {
                    topic: topic.to_string(),
                    name: name.to_string(),
                }
                .into()
            })
    }

    /// A side-effect-free bounded window from the subscription's cursor.
    pub fn pull(
        &self,
        topic: &str,
        name: &str,
        limit: Option<usize>,
    ) -> Result<PullSubscriptionBatch, JournalError> {
        self.lock()
            .pull_subscription(topic, name, limit)
            .map_err(JournalError::from)
    }

    pub fn retention(&self, topic: &str) -> Option<RetentionPolicy> {
        self.lock().retention(topic).cloned()
    }

    /// A consistent snapshot of the whole journal for backup runners (#1329):
    /// `(applied_index, bytes)`, where the bytes are EXACTLY the raft state
    /// machine's snapshot (the whole journal + the applied index; 0 on a
    /// raft-less single node).
    pub fn backup_snapshot(&self) -> Result<(u64, Vec<u8>), JournalError> {
        match &self.replicator {
            Some(replicator) => {
                let applied = replicator.applied_index();
                replicator
                    .snapshot_bytes()
                    .map(|bytes| (applied, bytes))
                    .map_err(|error| JournalError::Internal(error.0))
            }
            None => {
                let journal = self.lock().clone();
                encode_snapshot(journal, 0)
                    .map(|bytes| (0, bytes))
                    .map_err(|error| JournalError::Internal(error.to_string()))
            }
        }
    }
}
