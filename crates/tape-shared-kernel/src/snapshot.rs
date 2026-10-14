//! The whole-journal snapshot format: what a raft snapshot stores, what
//! `GET /admin/backup` serves, and what a bootstrap seed restores.

use serde::{Deserialize, Serialize};

use super::command::TapeOutcome;
use super::tape_journal::TapeJournal;

/// Identity of one proposal, so a retried raft entry applies exactly once.
/// `node` is the raft node id that proposed it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProposalId {
    pub node: u64,
    pub session: u64,
    pub sequence: u64,
}

/// Whole-journal snapshot tagged with the raft applied index. It carries the
/// full state machine, not a live/un-acked subset like relay's: every event
/// still retained, plus next offsets, checkpoints, subscriptions, and
/// retention policies. Retention does trim history, but only while applying
/// a replicated command (`TapeJournal::append_at`, `put_retention`), so every
/// replica trims at the same index and a snapshot taken after it already
/// reflects the trim.
#[derive(Debug, Serialize, Deserialize)]
pub struct JournalSnapshot {
    pub up_to: u64,
    pub journal: TapeJournal,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_proposals: Vec<(ProposalId, TapeOutcome)>,
}

/// Serialize the snapshot shape for `journal` at `up_to`, with no completed
/// proposals. A raft-less single node serves this from `GET /admin/backup`
/// with `up_to` 0: the bytes a backup runner ships are the bytes a raft group
/// would snapshot.
pub fn encode_snapshot(journal: TapeJournal, up_to: u64) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&JournalSnapshot {
        up_to,
        journal,
        completed_proposals: Vec::new(),
    })
}
