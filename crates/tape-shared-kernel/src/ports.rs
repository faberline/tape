//! The two ways a mutation becomes durable. Application code holds these as
//! trait objects; infrastructure supplies the WAL, the legacy whole-file
//! store, and the raft group.

use std::future::Future;
use std::pin::Pin;

use super::command::{TapeCommand, TapeOutcome};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Single-node durability: apply `command` to the journal and make it durable
/// before resolving. A failure carries the durable write path's
/// [`std::io::Error`] (see [`super::durability`]).
pub trait CommitLog: Send + Sync {
    fn commit(&self, command: TapeCommand) -> BoxFuture<'_, std::io::Result<TapeOutcome>>;
}

/// Replicated durability: propose `command` to the group and read back this
/// node's apply outcome. `Ok(None)` means the command committed but its
/// outcome aged out before this node could read it back.
pub trait Replicator: Send + Sync {
    fn propose(
        &self,
        command: TapeCommand,
    ) -> BoxFuture<'_, Result<Option<TapeOutcome>, ReplicationError>>;

    /// Highest log index this node's journal has applied.
    fn applied_index(&self) -> u64;

    /// The exact state-machine snapshot at [`Self::applied_index`].
    fn snapshot_bytes(&self) -> Result<Vec<u8>, ReplicationError>;
}

/// A replication failure, carried as its rendered message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ReplicationError(pub String);
