//! Why a journal use case failed, classified by what a caller can do about
//! it. Each domain rejection keeps its rendered message; the HTTP interface
//! maps each variant to its status and error code.

use tape_shared_kernel::{SubscriptionAckError, SubscriptionError, TapeError};

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// #2573: the node latched sticky degraded read-only mode after a
    /// durability failure; mutations fast-fail until the re-probe clears it.
    #[error(
        "node is in degraded read-only mode: the journal store reported ENOSPC on its \
         durable write path; reads keep serving. Retry once the periodic re-probe \
         (TAPE_STORAGE_FULL_REPROBE_SECS, default 30s) clears it, or restart the pod \
         after freeing or expanding the volume"
    )]
    StorageDegraded,
    /// The single-node durable write failed (fsync/rename/write, or the WAL
    /// coordinator's error). The [`std::io::ErrorKind`] is preserved so a
    /// full disk stays distinguishable from every other failure.
    #[error(transparent)]
    Durability(std::io::Error),
    /// The replicated path could not produce an outcome: the proposal failed
    /// or its outcome aged out before this node read it back.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
    /// A stale or beyond-end checkpoint offset.
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    SubscriptionNotFound(String),
    #[error("{0}")]
    SubscriptionExists(String),
    #[error("{0}")]
    PullBatchTooLarge(String),
}

impl From<TapeError> for JournalError {
    fn from(error: TapeError) -> Self {
        Self::Conflict(error.to_string())
    }
}

impl From<SubscriptionError> for JournalError {
    fn from(error: SubscriptionError) -> Self {
        let message = error.to_string();
        match error {
            SubscriptionError::NotFound { .. } => Self::SubscriptionNotFound(message),
            SubscriptionError::AlreadyExists { .. } => Self::SubscriptionExists(message),
            SubscriptionError::PullBatchTooLarge { .. } => Self::PullBatchTooLarge(message),
        }
    }
}

impl From<SubscriptionAckError> for JournalError {
    fn from(error: SubscriptionAckError) -> Self {
        match error {
            SubscriptionAckError::Subscription(error) => error.into(),
            SubscriptionAckError::Checkpoint(error) => error.into(),
        }
    }
}
