//! The journal's mutation vocabulary: one [`TapeCommand`] per write, applied
//! by [`apply_command`] to produce one [`TapeOutcome`]. Every serving path —
//! raft propose, WAL group commit, and the legacy whole-file store — applies
//! commands through this one function, so they share one mutation semantics.

use serde::{Deserialize, Serialize};

use super::error::{SubscriptionAckError, SubscriptionError, TapeError};
use super::event::{ConsumerCheckpoint, TapeEvent};
use super::retention::{RetentionOutcome, RetentionPolicy};
use super::subscription::Subscription;
use super::tape_journal::TapeJournal;

/// A tape write replicated through raft -- the command bytes of one log
/// entry. Both time-dependent fields (`timestamp_ms` / `updated_at_ms`) are
/// resolved by the proposing caller BEFORE the command is encoded, never
/// inside [`apply_command`], so every replica computes the identical value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TapeCommand {
    Append {
        topic: String,
        key: Option<String>,
        payload: serde_json::Value,
        timestamp_ms: u64,
        #[serde(default)]
        applied_at_ms: u64,
    },
    CheckpointPut {
        topic: String,
        consumer: String,
        offset: u64,
        updated_at_ms: u64,
    },
    SubscriptionCreate {
        topic: String,
        name: String,
    },
    SubscriptionDelete {
        topic: String,
        name: String,
    },
    SubscriptionAck {
        topic: String,
        name: String,
        offset: u64,
        updated_at_ms: u64,
    },
    RetentionPut {
        topic: String,
        policy: RetentionPolicy,
        now_ms: u64,
    },
}

/// The local-only apply outcome, claimed from raft-runtime's `OutcomeWindow`
/// by the proposing caller. Never sent over the wire -- only [`TapeCommand`]
/// crosses the raft log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TapeOutcome {
    Appended(TapeEvent),
    Checkpoint(Result<ConsumerCheckpoint, TapeError>),
    SubscriptionCreated(Result<Subscription, SubscriptionError>),
    SubscriptionDeleted(Result<Subscription, SubscriptionError>),
    SubscriptionAcked(Result<ConsumerCheckpoint, SubscriptionAckError>),
    RetentionUpdated(RetentionOutcome),
}

/// Apply one command to the journal. Pure: the command carries every
/// timestamp the transition needs.
pub fn apply_command(journal: &mut TapeJournal, command: TapeCommand) -> TapeOutcome {
    match command {
        TapeCommand::Append {
            topic,
            key,
            payload,
            timestamp_ms,
            applied_at_ms,
        } => {
            let applied_at_ms = if applied_at_ms == 0 {
                timestamp_ms
            } else {
                applied_at_ms
            };
            let event = journal.append_at(topic, key, payload, timestamp_ms, applied_at_ms);
            TapeOutcome::Appended(event)
        }
        TapeCommand::CheckpointPut {
            topic,
            consumer,
            offset,
            updated_at_ms,
        } => {
            let result = journal.put_checkpoint_at(topic, consumer, offset, updated_at_ms);
            TapeOutcome::Checkpoint(result)
        }
        TapeCommand::SubscriptionCreate { topic, name } => {
            TapeOutcome::SubscriptionCreated(journal.create_subscription(topic, name))
        }
        TapeCommand::SubscriptionDelete { topic, name } => {
            TapeOutcome::SubscriptionDeleted(journal.delete_subscription(&topic, &name))
        }
        TapeCommand::SubscriptionAck {
            topic,
            name,
            offset,
            updated_at_ms,
        } => TapeOutcome::SubscriptionAcked(journal.ack_subscription_at(
            &topic,
            &name,
            offset,
            updated_at_ms,
        )),
        TapeCommand::RetentionPut {
            topic,
            policy,
            now_ms,
        } => TapeOutcome::RetentionUpdated(journal.put_retention(topic, policy, now_ms)),
    }
}
