//! The journal's mutation vocabulary: one [`TapeCommand`] per write, applied
//! by [`apply_command`] to produce one [`TapeOutcome`]. Every serving path —
//! raft propose, WAL group commit, and the legacy whole-file store — applies
//! commands through this one function, so they share one mutation semantics.

use serde::{Deserialize, Serialize};

use super::error::{SubscriptionAckError, SubscriptionError, TapeError};
use super::event::{ConsumerCheckpoint, TapeEvent};
use super::retention::{RetentionOutcome, RetentionPolicy};
use super::subscription::{SeekOutcome, Subscription};
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
        #[serde(default)]
        expected_offset: Option<u64>,
        #[serde(default)]
        epoch: Option<u64>,
        updated_at_ms: u64,
    },
    RetentionPut {
        topic: String,
        policy: RetentionPolicy,
        now_ms: u64,
    },
    SubscriptionSeek {
        topic: String,
        name: String,
        offset: u64,
        updated_at_ms: u64,
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
    SubscriptionSought(Result<SeekOutcome, SubscriptionAckError>),
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
            expected_offset,
            epoch,
            updated_at_ms,
        } => TapeOutcome::SubscriptionAcked(journal.ack_subscription_epoch_at(
            &topic,
            &name,
            offset,
            expected_offset,
            epoch,
            updated_at_ms,
        )),
        TapeCommand::RetentionPut {
            topic,
            policy,
            now_ms,
        } => TapeOutcome::RetentionUpdated(journal.put_retention(topic, policy, now_ms)),
        TapeCommand::SubscriptionSeek {
            topic,
            name,
            offset,
            updated_at_ms,
        } => TapeOutcome::SubscriptionSought(journal.seek_subscription_at(
            &topic,
            &name,
            offset,
            updated_at_ms,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_command_without_expected_offset_decodes() {
        // An old raft log entry has no `expected_offset` field.
        let old = r#"{"SubscriptionAck":{"topic":"t","name":"n","offset":1,"updated_at_ms":0}}"#;
        let command: TapeCommand = serde_json::from_str(old).unwrap();
        assert!(matches!(
            command,
            TapeCommand::SubscriptionAck {
                expected_offset: None,
                ..
            }
        ));
    }

    #[test]
    fn ack_command_without_epoch_decodes() {
        // An old raft log entry has no `epoch` field.
        let old = r#"{"SubscriptionAck":{"topic":"t","name":"n","offset":1,"updated_at_ms":0}}"#;
        let command: TapeCommand = serde_json::from_str(old).unwrap();
        assert!(matches!(
            command,
            TapeCommand::SubscriptionAck { epoch: None, .. }
        ));
    }

    #[test]
    fn seek_command_bumps_the_epoch_and_a_stale_epoch_ack_is_refused() {
        let mut journal = TapeJournal::default();
        for _ in 0..3 {
            apply_command(
                &mut journal,
                TapeCommand::Append {
                    topic: "orders".into(),
                    key: None,
                    payload: serde_json::json!({}),
                    timestamp_ms: 100,
                    applied_at_ms: 100,
                },
            );
        }
        apply_command(
            &mut journal,
            TapeCommand::SubscriptionCreate {
                topic: "orders".into(),
                name: "a".into(),
            },
        );

        let seek = apply_command(
            &mut journal,
            TapeCommand::SubscriptionSeek {
                topic: "orders".into(),
                name: "a".into(),
                offset: 1,
                updated_at_ms: 5,
            },
        );
        match seek {
            TapeOutcome::SubscriptionSought(Ok(outcome)) => {
                assert_eq!(outcome.epoch, 1);
                assert_eq!(outcome.cursor, 1);
            }
            other => panic!("unexpected seek outcome: {other:?}"),
        }

        let stale = apply_command(
            &mut journal,
            TapeCommand::SubscriptionAck {
                topic: "orders".into(),
                name: "a".into(),
                offset: 3,
                expected_offset: None,
                epoch: Some(0),
                updated_at_ms: 6,
            },
        );
        assert!(matches!(
            stale,
            TapeOutcome::SubscriptionAcked(Err(SubscriptionAckError::Checkpoint(
                TapeError::StaleEpoch {
                    epoch: 0,
                    current_epoch: 1
                }
            )))
        ));

        let fresh = apply_command(
            &mut journal,
            TapeCommand::SubscriptionAck {
                topic: "orders".into(),
                name: "a".into(),
                offset: 3,
                expected_offset: None,
                epoch: Some(1),
                updated_at_ms: 6,
            },
        );
        match fresh {
            TapeOutcome::SubscriptionAcked(Ok(checkpoint)) => assert_eq!(checkpoint.offset, 3),
            other => panic!("unexpected ack outcome: {other:?}"),
        }
    }

    #[test]
    fn seek_command_round_trips_json() {
        let command = TapeCommand::SubscriptionSeek {
            topic: "t".into(),
            name: "n".into(),
            offset: 7,
            updated_at_ms: 9,
        };
        let json = serde_json::to_string(&command).unwrap();
        let back: TapeCommand = serde_json::from_str(&json).unwrap();
        match back {
            TapeCommand::SubscriptionSeek {
                topic,
                name,
                offset,
                updated_at_ms,
            } => {
                assert_eq!(topic, "t");
                assert_eq!(name, "n");
                assert_eq!(offset, 7);
                assert_eq!(updated_at_ms, 9);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
