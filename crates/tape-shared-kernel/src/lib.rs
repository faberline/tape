//! The tape journal: append-only topics, consumer checkpoints, pull
//! subscriptions, and retention, with every mutation expressed as one
//! [`TapeCommand`].

mod command;
mod durability;
mod error;
mod event;
mod ports;
pub mod replay_wire;
mod retention;
mod snapshot;
mod subscription;
mod tape_journal;

pub use command::{apply_command, TapeCommand, TapeOutcome};
pub use durability::{durability_errno, should_enter_storage_degraded_mode, DurabilityFailure};
pub use error::{SubscriptionAckError, SubscriptionError, TapeError};
pub use event::{ConsumerCheckpoint, TapeEvent};
pub use ports::{BoxFuture, CommitLog, ReplicationError, Replicator};
pub use retention::{RetentionOutcome, RetentionPolicy};
pub use snapshot::{encode_snapshot, JournalSnapshot, ProposalId};
pub use subscription::{PullSubscriptionBatch, Subscription, DEFAULT_PULL_BATCH, MAX_PULL_BATCH};
pub use tape_journal::TapeJournal;
