//! raft-runtime-backed consensus for tape (#1327).
//!
//! `tape`'s journal is wired as a [`raft_runtime::RaftStateMachine`] so HA
//! append/checkpoint-put go through the shared driver (propose -> commit ->
//! sole applier) instead of a hand-rolled one. tape is a **single-group**
//! adopter (like relay, unlike keep's host-per-shard): one [`RaftHost`]
//! replicates every topic's appends and every consumer's checkpoints; the
//! command is [`TapeCommand`] (append or checkpoint-put).
//!
//! Replication scope (deliberate): the whole [`TapeJournal`] — append
//! AND checkpoint-put both propose through raft in replica/HA mode. Reads
//! (`replay` / `checkpoint_get`) stay node-local against the same shared
//! journal the state machine mutates.
//!
//! Restart honesty: `RaftStore` persists the commit watermark with hard state,
//! so the host cold-replays every resident committed entry into a fresh state
//! machine before accepting new proposals. Host snapshots restore the whole
//! journal before log tailing. Old `applied-*.idx`/`snapshot-*.json` files are
//! read only as a migration path; new runs do not duplicate generic commit
//! persistence or per-apply fsyncs inside Tape.

use serde::{Deserialize, Serialize};

#[cfg(test)]
use tape_shared_kernel::TapeJournal;
use tape_shared_kernel::{ProposalId, TapeCommand};

mod bootstrap;
mod host;
mod state_machine;
#[cfg(test)]
mod tests;

pub use bootstrap::{data_dir_has_existing_state, prepare_bootstrap_seed};
pub use host::TapeRaft;
pub use raft_runtime::{HostShutdownReport, LeadershipHandoff, ShutdownPhase};
pub use state_machine::TapeStateMachine;

/// How many applied entries between host snapshots (log compaction; arms
/// InstallSnapshot for a lagging/fresh replica).
pub const SNAPSHOT_EVERY: u64 = 1024;

/// One raft log entry: the command plus the identity that makes an ambiguous
/// transport retry apply exactly once.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TapeEnvelope {
    proposal_id: ProposalId,
    command: TapeCommand,
}
