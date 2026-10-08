//! The journal driven as a raft-runtime state machine.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use raft_runtime::{Index, OutcomeWindow, ProposalCache, RaftStateMachine};

use super::TapeEnvelope;
use tape_shared_kernel::{
    apply_command, JournalSnapshot, ProposalId, TapeCommand, TapeJournal, TapeOutcome,
};

/// tape's [`TapeJournal`] driven as a [`RaftStateMachine`].
///
/// `apply` calls the SAME validated [`TapeJournal::append_at`] /
/// [`TapeJournal::put_checkpoint_at`] methods the single-node path uses --
/// append-ordering, retention, and stale-checkpoint semantics are unchanged --
/// and stashes the outcome in an [`OutcomeWindow`] keyed by raft index so the
/// proposing handler can return the real domain result (read-your-write). The
/// applied index is tracked in memory while shared Raft persistence owns new
/// restart recovery. The optional marker/sibling snapshot fields below exist
/// solely to adopt data produced by the pre-shared-persistence implementation.
pub struct TapeStateMachine {
    journal: Arc<Mutex<TapeJournal>>,
    pub(super) applied: AtomicU64,
    /// Legacy `applied-<node>.idx` migration source. New runs do not write it.
    marker: Option<PathBuf>,
    outcomes: Mutex<OutcomeWindow<TapeOutcome>>,
    completed: Mutex<ProposalCache<ProposalId, TapeOutcome>>,
}

/// The sibling snapshot file path for a given marker path
/// (`applied-<node>.idx` -> `snapshot-<node>.json`, same directory).
pub(super) fn snapshot_path_for(marker: &Path) -> PathBuf {
    let name = marker
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("applied.idx");
    let name = name.replacen("applied-", "snapshot-", 1);
    let name = name
        .strip_suffix(".idx")
        .map(|stem| format!("{stem}.json"))
        .unwrap_or_else(|| format!("{name}.json"));
    marker.with_file_name(name)
}

impl TapeStateMachine {
    /// Wrap `journal` as the group's state machine. `marker` names only the
    /// legacy migration files; current RaftStore log/snapshot recovery is the
    /// durable source for new data.
    pub fn new(journal: Arc<Mutex<TapeJournal>>, marker: Option<PathBuf>) -> Result<Arc<Self>> {
        let mut applied = 0u64;
        let mut recovered_completed = Vec::new();
        if let Some(path) = &marker {
            let snap_path = snapshot_path_for(path);
            match std::fs::read(&snap_path) {
                Ok(bytes) => {
                    let snap: JournalSnapshot =
                        serde_json::from_slice(&bytes).with_context(|| {
                            format!("corrupt journal snapshot {}", snap_path.display())
                        })?;
                    *journal.lock().expect("journal mutex poisoned") = snap.journal;
                    applied = snap.up_to;
                    // Installed below after `Self` is constructed.
                    recovered_completed = snap.completed_proposals;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("read journal snapshot"),
            }
            // The journal snapshot is authoritative. A marker without the
            // matching state must never advance the apply floor or committed
            // events would disappear after restart.
            match std::fs::read_to_string(path) {
                Ok(s) => {
                    let marker_floor = s
                        .trim()
                        .parse::<u64>()
                        .with_context(|| format!("corrupt applied marker {}", path.display()))?;
                    if marker_floor != applied {
                        tracing::warn!(
                            marker_floor,
                            snapshot_floor = applied,
                            "raft: applied marker disagrees with journal snapshot; replaying from snapshot floor"
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("read applied marker"),
            }
        }
        Ok(Arc::new(Self {
            journal,
            applied: AtomicU64::new(applied),
            marker,
            outcomes: Mutex::new(OutcomeWindow::default()),
            completed: Mutex::new({
                let mut cache = ProposalCache::default();
                cache.restore(recovered_completed);
                cache
            }),
        }))
    }

    /// Remove and return the apply outcome at `index` (the proposing handler
    /// claims it once; unclaimed outcomes age out).
    pub fn claim_outcome(&self, index: Index) -> Option<TapeOutcome> {
        self.outcomes.lock().expect("outcome window").claim(index)
    }

    /// Resolve a committed proposal by its stable id. Unlike the transient
    /// index window, this cache survives ambiguous transport retries and is
    /// snapshotted with the state machine.
    pub(super) fn proposal_outcome(&self, id: &ProposalId) -> Option<TapeOutcome> {
        self.completed.lock().expect("completed proposals").get(id)
    }

    /// The journal this state machine applies into.
    pub fn journal(&self) -> Arc<Mutex<TapeJournal>> {
        Arc::clone(&self.journal)
    }
}

impl RaftStateMachine for TapeStateMachine {
    fn apply(&self, index: Index, command: &[u8]) -> Result<()> {
        // Legacy migration floor: entries represented by an imported app
        // snapshot were already applied. New stores start at zero and replay
        // their shared persisted commit range normally.
        if index <= self.applied.load(Ordering::Acquire) && self.marker.is_some() {
            return Ok(());
        }
        let (proposal_id, decoded) = match serde_json::from_slice::<TapeEnvelope>(command) {
            Ok(envelope) => (Some(envelope.proposal_id), Ok(envelope.command)),
            Err(envelope_error) => (
                None,
                serde_json::from_slice::<TapeCommand>(command).map_err(|_| envelope_error),
            ),
        };
        let cached = proposal_id
            .as_ref()
            .and_then(|id| self.completed.lock().expect("completed proposals").get(id));
        let outcome = match (cached, decoded) {
            (Some(outcome), _) => Some(outcome),
            (None, Ok(command)) => {
                let mut journal = self.journal.lock().expect("journal mutex poisoned");
                Some(apply_command(&mut journal, command))
            }
            (None, Err(e)) => {
                tracing::warn!(index, error = %e, "raft: undecodable command (entry no-ops)");
                None
            }
        };
        if let Some(outcome) = outcome {
            if let Some(id) = proposal_id {
                self.completed
                    .lock()
                    .expect("completed proposals")
                    .insert(id, outcome.clone());
            }
            let mut window = self.outcomes.lock().expect("outcome window");
            window.insert(index, outcome);
            window.advance(index);
        }
        self.applied.store(index, Ordering::Release);
        Ok(())
    }

    fn snapshot(&self, writer: &mut dyn std::io::Write) -> Result<()> {
        let journal = self.journal.lock().expect("journal mutex poisoned").clone();
        let bytes = serde_json::to_vec(&JournalSnapshot {
            up_to: self.applied_index(),
            journal,
            completed_proposals: self
                .completed
                .lock()
                .expect("completed proposals")
                .snapshot(),
        })?;
        writer.write_all(&bytes)?;
        Ok(())
    }

    fn restore(&self, reader: &mut dyn std::io::Read) -> Result<()> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        let snap: JournalSnapshot = serde_json::from_slice(&bytes)?;
        *self.journal.lock().expect("journal mutex poisoned") = snap.journal;
        self.completed
            .lock()
            .expect("completed proposals")
            .restore(snap.completed_proposals);
        self.applied.store(snap.up_to, Ordering::Release);
        Ok(())
    }

    fn applied_index(&self) -> Index {
        self.applied.load(Ordering::Acquire)
    }
}
