//! Pull subscriptions: a named cursor on a topic and the bounded window one
//! pull returns.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::event::TapeEvent;

pub const DEFAULT_PULL_BATCH: usize = 100;
pub const MAX_PULL_BATCH: usize = 1_000;

pub const DEFAULT_ACK_DEADLINE_SECONDS: u64 = 10;
pub const MIN_ACK_DEADLINE_SECONDS: u64 = 10;
pub const MAX_ACK_DEADLINE_SECONDS: u64 = 600;

pub(crate) fn default_ack_deadline_seconds() -> u64 {
    DEFAULT_ACK_DEADLINE_SECONDS
}

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
    #[serde(
        rename = "ackDeadlineSeconds",
        alias = "ack_deadline_seconds",
        default = "default_ack_deadline_seconds"
    )]
    pub ack_deadline_seconds: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_subscription_shape_decodes_with_default_ack_deadline() {
        let legacy_json = r#"{"topic":"orders","name":"sub-1"}"#;
        let subscription: Subscription =
            serde_json::from_str(legacy_json).expect("decode legacy subscription");
        assert_eq!(subscription.topic, "orders");
        assert_eq!(subscription.name, "sub-1");
        assert_eq!(
            subscription.ack_deadline_seconds,
            DEFAULT_ACK_DEADLINE_SECONDS
        );
    }

    #[test]
    fn subscription_roundtrip_with_explicit_ack_deadline() {
        let subscription = Subscription {
            topic: "orders".to_string(),
            name: "sub-1".to_string(),
            ack_deadline_seconds: 300,
        };
        let encoded = serde_json::to_string(&subscription).expect("encode subscription");
        assert!(encoded.contains(r#""ackDeadlineSeconds":300"#));
        let decoded: Subscription = serde_json::from_str(&encoded).expect("decode subscription");
        assert_eq!(decoded, subscription);
    }
}
