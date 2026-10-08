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
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionError {
    #[error("subscription {name} already exists for topic {topic}")]
    AlreadyExists { topic: String, name: String },
    #[error("subscription {name} does not exist for topic {topic}")]
    NotFound { topic: String, name: String },
    #[error("pull batch limit {limit} exceeds maximum {max}")]
    PullBatchTooLarge { limit: usize, max: usize },
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionAckError {
    #[error(transparent)]
    Subscription(#[from] SubscriptionError),
    #[error(transparent)]
    Checkpoint(#[from] TapeError),
}
