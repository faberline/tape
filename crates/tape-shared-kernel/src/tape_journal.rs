//! The journal aggregate: every topic's events, consumer checkpoints, pull
//! subscriptions, and retention policies. Every method here is deterministic;
//! the caller supplies wall-clock time, so raft replicas apply identical state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{SubscriptionAckError, SubscriptionError, TapeError};
use super::event::{ConsumerCheckpoint, TapeEvent};
use super::retention::{RetentionOutcome, RetentionPolicy};
use super::subscription::{
    PullSubscriptionBatch, Subscription, DEFAULT_PULL_BATCH, MAX_PULL_BATCH,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TapeJournal {
    topics: BTreeMap<String, Vec<TapeEvent>>,
    #[serde(default)]
    next_offsets: BTreeMap<String, u64>,
    checkpoints: BTreeMap<String, ConsumerCheckpoint>,
    #[serde(default)]
    subscriptions: BTreeMap<String, Subscription>,
    #[serde(default)]
    retention: BTreeMap<String, RetentionPolicy>,
}

impl TapeJournal {
    /// Deterministic append + retention transition for Raft apply. Event time
    /// and policy-evaluation time are separate so historical backfill does not
    /// rewind the retention clock.
    pub fn append_at(
        &mut self,
        topic: impl Into<String>,
        key: Option<String>,
        payload: Value,
        timestamp_ms: u64,
        applied_at_ms: u64,
    ) -> TapeEvent {
        let topic = topic.into();
        let recovered_next = self
            .topics
            .get(&topic)
            .and_then(|events| events.last())
            .map(|event| event.offset + 1)
            .unwrap_or(0);
        let next = self
            .next_offsets
            .entry(topic.clone())
            .or_insert(recovered_next);
        let event = TapeEvent {
            topic: topic.clone(),
            offset: *next,
            timestamp_ms,
            key,
            payload,
        };
        *next = next.saturating_add(1);
        self.topics
            .entry(topic.clone())
            .or_default()
            .push(event.clone());
        self.enforce_retention(&topic, applied_at_ms);
        event
    }

    /// Replay events oldest-first from `from_offset`/`from_timestamp_ms`.
    /// #2484: an omitted `limit` is bounded to [`MAX_PULL_BATCH`] rather than
    /// returning the whole matching set, matching the pull-subscription cap
    /// so a single limit-less replay can't return an unbounded window; page
    /// with `from_offset`/`limit` to read past the first `MAX_PULL_BATCH`
    /// events.
    pub fn replay(
        &self,
        topic: &str,
        from_offset: Option<u64>,
        from_timestamp_ms: Option<u64>,
        limit: Option<usize>,
    ) -> Vec<TapeEvent> {
        self.replay_refs(topic, from_offset, from_timestamp_ms, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Borrowing form of [`Self::replay`] — same `limit`-omitted bound to
    /// [`MAX_PULL_BATCH`] (#2484).
    pub fn replay_refs(
        &self,
        topic: &str,
        from_offset: Option<u64>,
        from_timestamp_ms: Option<u64>,
        limit: Option<usize>,
    ) -> Vec<&TapeEvent> {
        let from_offset = from_offset.unwrap_or(0);
        let events = self
            .topics
            .get(topic)
            .into_iter()
            .flatten()
            .filter(|event| {
                event.offset >= from_offset
                    && from_timestamp_ms
                        .map(|timestamp| event.timestamp_ms >= timestamp)
                        .unwrap_or(true)
            });
        match limit {
            Some(limit) => events.take(limit).collect(),
            None => events.take(MAX_PULL_BATCH).collect(),
        }
    }

    /// Same validation/ordering as a wall-clock checkpoint put, parameterized on
    /// the timestamp so raft replicas apply an identical `updated_at_ms`
    /// instead of each computing `now_ms()` independently (#1327).
    pub fn put_checkpoint_at(
        &mut self,
        topic: impl Into<String>,
        consumer: impl Into<String>,
        offset: u64,
        updated_at_ms: u64,
    ) -> Result<ConsumerCheckpoint, TapeError> {
        let topic = topic.into();
        let consumer = consumer.into();
        let end_offset = self.end_offset(&topic);
        if offset > end_offset {
            return Err(TapeError::CheckpointBeyondEnd { offset, end_offset });
        }
        let key = checkpoint_key(&topic, &consumer);
        if let Some(existing) = self.checkpoints.get(&key) {
            if offset < existing.offset {
                return Err(TapeError::StaleCheckpoint {
                    current_offset: existing.offset,
                    new_offset: offset,
                });
            }
        }
        let checkpoint = ConsumerCheckpoint {
            topic,
            consumer,
            offset,
            updated_at_ms,
        };
        self.checkpoints.insert(key, checkpoint.clone());
        Ok(checkpoint)
    }

    pub fn checkpoint(&self, topic: &str, consumer: &str) -> Option<&ConsumerCheckpoint> {
        self.checkpoints.get(&checkpoint_key(topic, consumer))
    }

    /// Create a topic-scoped subscription without moving a pull checkpoint.
    pub fn create_subscription(
        &mut self,
        topic: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Subscription, SubscriptionError> {
        let topic = topic.into();
        let name = name.into();
        let key = subscription_key(&topic, &name);
        if self.subscriptions.contains_key(&key) {
            return Err(SubscriptionError::AlreadyExists { topic, name });
        }
        let subscription = Subscription { topic, name };
        self.subscriptions.insert(key, subscription.clone());
        Ok(subscription)
    }

    pub fn subscriptions(&self, topic: &str) -> Vec<Subscription> {
        self.subscriptions
            .values()
            .filter(|subscription| subscription.topic == topic)
            .cloned()
            .collect()
    }

    pub fn subscription(&self, topic: &str, name: &str) -> Option<&Subscription> {
        self.subscriptions.get(&subscription_key(topic, name))
    }

    /// Delete subscription metadata only; a matching pull checkpoint remains
    /// available through the existing checkpoint interface.
    pub fn delete_subscription(
        &mut self,
        topic: &str,
        name: &str,
    ) -> Result<Subscription, SubscriptionError> {
        self.subscriptions
            .remove(&subscription_key(topic, name))
            .ok_or_else(|| SubscriptionError::NotFound {
                topic: topic.to_string(),
                name: name.to_string(),
            })
    }

    /// Read a bounded, caller-driven window from a pull subscription cursor.
    /// Pulling is deliberately side-effect free: a caller must explicitly ack
    /// after processing to advance the durable checkpoint.
    pub fn pull_subscription(
        &self,
        topic: &str,
        name: &str,
        limit: Option<usize>,
    ) -> Result<PullSubscriptionBatch, SubscriptionError> {
        self.require_pull_subscription(topic, name)?;
        let limit = limit.unwrap_or(DEFAULT_PULL_BATCH);
        if limit > MAX_PULL_BATCH {
            return Err(SubscriptionError::PullBatchTooLarge {
                limit,
                max: MAX_PULL_BATCH,
            });
        }
        let cursor = self
            .checkpoint(topic, name)
            .map(|checkpoint| checkpoint.offset)
            .unwrap_or(0);
        let events = self.replay(topic, Some(cursor), None, Some(limit));
        let next_offset = events
            .last()
            .map(|event| event.offset + 1)
            .unwrap_or(cursor);
        Ok(PullSubscriptionBatch {
            topic: topic.to_string(),
            subscription: name.to_string(),
            cursor,
            limit,
            next_offset,
            events,
        })
    }

    /// Acknowledge a completed pull window by advancing its existing durable
    /// topic/name checkpoint. The checkpoint's stale and beyond-end guards are
    /// reused as-is. Per-message ack ids and in-flight leases arrive with the
    /// `subscription-ack-and-competing-subscribers` outcome in `ROADMAP.md`
    /// and supersede this cumulative cursor rather than sit beside it.
    pub fn ack_subscription_at(
        &mut self,
        topic: &str,
        name: &str,
        offset: u64,
        updated_at_ms: u64,
    ) -> Result<ConsumerCheckpoint, SubscriptionAckError> {
        self.require_pull_subscription(topic, name)?;
        Ok(self.put_checkpoint_at(topic, name, offset, updated_at_ms)?)
    }

    fn require_pull_subscription(
        &self,
        topic: &str,
        name: &str,
    ) -> Result<&Subscription, SubscriptionError> {
        let subscription =
            self.subscription(topic, name)
                .ok_or_else(|| SubscriptionError::NotFound {
                    topic: topic.to_string(),
                    name: name.to_string(),
                })?;
        Ok(subscription)
    }

    /// Return all subscriptions across all topics. Used by the metrics endpoint
    /// to compute subscription lag gauges (#2485).
    pub fn all_subscriptions(&self) -> Vec<&Subscription> {
        self.subscriptions.values().collect()
    }

    pub fn end_offset(&self, topic: &str) -> u64 {
        self.next_offsets.get(topic).copied().unwrap_or_else(|| {
            self.topics
                .get(topic)
                .and_then(|events| events.last())
                .map(|event| event.offset + 1)
                .unwrap_or(0)
        })
    }

    pub fn retention(&self, topic: &str) -> Option<&RetentionPolicy> {
        self.retention.get(topic)
    }

    pub fn put_retention(
        &mut self,
        topic: impl Into<String>,
        policy: RetentionPolicy,
        now_ms: u64,
    ) -> RetentionOutcome {
        let topic = topic.into();
        self.retention.insert(topic.clone(), policy.clone());
        let removed = self.enforce_retention(&topic, now_ms);
        RetentionOutcome {
            earliest_offset: self
                .topics
                .get(&topic)
                .and_then(|events| events.first())
                .map(|event| event.offset)
                .unwrap_or_else(|| self.end_offset(&topic)),
            end_offset: self.end_offset(&topic),
            topic,
            policy,
            removed,
        }
    }

    fn enforce_retention(&mut self, topic: &str, now_ms: u64) -> usize {
        let Some(policy) = self.retention.get(topic).cloned() else {
            return 0;
        };
        let end = self.end_offset(topic);
        let events = self.topics.entry(topic.to_string()).or_default();
        let age_boundary = policy.max_age_seconds.map(|seconds| {
            let cutoff = now_ms.saturating_sub(seconds.saturating_mul(1_000));
            events
                .iter()
                .find(|event| event.timestamp_ms >= cutoff)
                .map(|event| event.offset)
                .unwrap_or(end)
        });
        let mut boundary = policy
            .min_offset
            .into_iter()
            .chain(age_boundary)
            .max()
            .unwrap_or(0)
            .min(end);
        if let Some(protected) = policy
            .protected_consumers
            .iter()
            .filter_map(|consumer| self.checkpoints.get(&checkpoint_key(topic, consumer)))
            .map(|checkpoint| checkpoint.offset)
            .min()
        {
            boundary = boundary.min(protected);
        }
        let before = events.len();
        events.retain(|event| event.offset >= boundary);
        before - events.len()
    }
}

fn checkpoint_key(topic: &str, consumer: &str) -> String {
    format!("{topic}\u{1f}{consumer}")
}

fn subscription_key(topic: &str, name: &str) -> String {
    format!("{topic}\u{1f}{name}")
}

#[cfg(test)]
mod tests;
