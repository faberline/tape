//! The journal's mutation vocabulary: one [`TapeCommand`] per write, applied
//! by [`apply_command`] to produce one [`TapeOutcome`]. Every serving path —
//! raft propose, WAL group commit, and the legacy whole-file store — applies
//! commands through this one function, so they share one mutation semantics.

use serde::{Deserialize, Serialize};

use super::error::{SubscriptionAckError, SubscriptionError, TapeError};
use super::event::{ConsumerCheckpoint, TapeEvent};
use super::retention::{RetentionOutcome, RetentionPolicy};
use super::subscription::{default_ack_deadline_seconds, Subscription};
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
        #[serde(
            default = "default_ack_deadline_seconds",
            rename = "ackDeadlineSeconds",
            alias = "ack_deadline_seconds"
        )]
        ack_deadline_seconds: u64,
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
        TapeCommand::SubscriptionCreate {
            topic,
            name,
            ack_deadline_seconds,
        } => TapeOutcome::SubscriptionCreated(journal.create_subscription_with_deadline(
            topic,
            name,
            ack_deadline_seconds,
        )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_ACK_DEADLINE_SECONDS;

    #[test]
    fn old_command_shape_decodes_with_default_ack_deadline() {
        let legacy_command_json = r#"{"SubscriptionCreate":{"topic":"orders","name":"sub-1"}}"#;
        let command: TapeCommand =
            serde_json::from_str(legacy_command_json).expect("decode legacy command");
        match command {
            TapeCommand::SubscriptionCreate {
                topic,
                name,
                ack_deadline_seconds,
            } => {
                assert_eq!(topic, "orders");
                assert_eq!(name, "sub-1");
                assert_eq!(ack_deadline_seconds, DEFAULT_ACK_DEADLINE_SECONDS);
            }
            _ => panic!("unexpected command variant"),
        }
    }
}
