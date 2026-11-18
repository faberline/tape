//! Rejections the journal reports. They are serialized inside raft outcomes,
//! so their shape is part of the snapshot format.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum TapeError {
    #[error("checkpoint offset {new_offset} is behind existing offset {current_offset}")]
    StaleCheckpoint {
        current_offset: u64,
        new_offset: u64,
    },
    #[error("checkpoint offset {offset} is beyond topic end offset {end_offset}")]
    CheckpointBeyondEnd { offset: u64, end_offset: u64 },
    #[error("ack expected offset {expected_offset} but the current offset is {current_offset}")]
    ExpectedOffsetMismatch {
        expected_offset: u64,
        current_offset: u64,
    },
    #[error("ack epoch {epoch} is stale: the subscription epoch is {current_epoch}")]
    StaleEpoch { epoch: u64, current_epoch: u64 },
    #[error("offset {offset} is outside the retained range {earliest_offset}..={end_offset}")]
    OffsetOutOfRange {
        offset: u64,
        earliest_offset: u64,
        end_offset: u64,
    },
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionError {
    #[error("subscription {name} already exists for topic {topic}")]
    AlreadyExists { topic: String, name: String },
    #[error("subscription {name} does not exist for topic {topic}")]
    NotFound { topic: String, name: String },
    #[error("pull batch limit {limit} exceeds maximum {max}")]
    PullBatchTooLarge { limit: usize, max: usize },
    #[error("cursor {cursor} is older than the earliest retained offset {earliest_offset} (end offset {end_offset})")]
    CursorOutOfRetention {
        cursor: u64,
        earliest_offset: u64,
        end_offset: u64,
    },
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionAckError {
    #[error(transparent)]
    Subscription(#[from] SubscriptionError),
    #[error(transparent)]
    Checkpoint(#[from] TapeError),
}
