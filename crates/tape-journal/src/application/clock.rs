//! Wall-clock entry points. The domain journal takes every timestamp as an
//! argument so raft replicas apply identical values; [`JournalClock`] reads
//! the clock once, at the edge, for callers that are not replicating. It is
//! an extension trait because the journal lives in the pure shared kernel,
//! which may not read the clock.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use tape_shared_kernel::{
    ConsumerCheckpoint, SubscriptionAckError, TapeError, TapeEvent, TapeJournal,
};

/// Milliseconds since the Unix epoch, or 0 if the clock reads before it.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// [`TapeJournal`] mutations stamped with the current wall-clock time.
pub trait JournalClock {
    /// Append with event time `timestamp_ms` (now when `None`), applied now.
    fn append(
        &mut self,
        topic: impl Into<String>,
        key: Option<String>,
        payload: Value,
        timestamp_ms: Option<u64>,
    ) -> TapeEvent;

    /// Advance a consumer checkpoint, stamped now.
    fn put_checkpoint(
        &mut self,
        topic: impl Into<String>,
        consumer: impl Into<String>,
        offset: u64,
    ) -> Result<ConsumerCheckpoint, TapeError>;

    /// Ack a pull subscription's cursor, stamped now.
    fn ack_subscription(
        &mut self,
        topic: &str,
        name: &str,
        offset: u64,
    ) -> Result<ConsumerCheckpoint, SubscriptionAckError>;
}

impl JournalClock for TapeJournal {
    fn append(
        &mut self,
        topic: impl Into<String>,
        key: Option<String>,
        payload: Value,
        timestamp_ms: Option<u64>,
    ) -> TapeEvent {
        let timestamp_ms = timestamp_ms.unwrap_or_else(now_ms);
        self.append_at(topic, key, payload, timestamp_ms, now_ms())
    }

    fn put_checkpoint(
        &mut self,
        topic: impl Into<String>,
        consumer: impl Into<String>,
        offset: u64,
    ) -> Result<ConsumerCheckpoint, TapeError> {
        self.put_checkpoint_at(topic, consumer, offset, now_ms())
    }

    fn ack_subscription(
        &mut self,
        topic: &str,
        name: &str,
        offset: u64,
    ) -> Result<ConsumerCheckpoint, SubscriptionAckError> {
        self.ack_subscription_at(topic, name, offset, now_ms())
    }
}
