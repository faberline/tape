//! Pull subscriptions: a named cursor on a topic and the bounded window one
//! pull returns.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::event::TapeEvent;

pub const DEFAULT_PULL_BATCH: usize = 100;
pub const MAX_PULL_BATCH: usize = 1_000;

/// A named pull cursor owned by a topic. Today it is one cumulative offset the
/// caller advances by acking a pull window. Per-message ack, leases, competing
/// subscribers and push delivery are `ROADMAP.md` outcomes
/// (`subscription-ack-and-competing-subscribers`, `push-subscriptions`) that
/// replace this cursor; they are not exclusions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub topic: String,
    pub name: String,
}

/// One caller-driven pull window. `cursor` is the checkpoint used to read;
/// `next_offset` is advisory until an explicit
/// [`TapeJournal::ack_subscription_at`](super::TapeJournal::ack_subscription_at).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PullSubscriptionBatch {
    pub topic: String,
    pub subscription: String,
    pub cursor: u64,
    pub limit: usize,
    pub next_offset: u64,
    pub events: Vec<TapeEvent>,
}
