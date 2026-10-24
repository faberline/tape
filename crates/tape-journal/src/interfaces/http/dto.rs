//! Request and response bodies the data plane adds around the journal's own
//! wire value objects.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use tape_shared_kernel::{ConsumerCheckpoint, RetentionPolicy, Subscription, TapeEvent};

/// Request body for `POST /topics/{topic}/append`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AppendRequest {
    /// Opaque caller-supplied label carried on the event envelope and returned on replay.
    /// Tape's own logic never reads key — not for partitioning, ordering, routing, or
    /// deduplication. Append is at-least-once; supplying key does not make a retry safe.
    #[serde(default)]
    pub key: Option<String>,
    /// Event payload.
    pub payload: serde_json::Value,
    /// Override event timestamp for deterministic tests/backfill.
    #[serde(default)]
    pub timestamp_ms: Option<u64>,
}

/// Query params for `GET /topics/{topic}/replay`.
#[derive(Debug, Deserialize)]
pub struct ReplayQuery {
    #[serde(default)]
    pub from_offset: Option<u64>,
    #[serde(default)]
    pub from_timestamp_ms: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Response body for `GET /topics/{topic}/replay`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReplayResponse {
    pub events: Vec<TapeEvent>,
}

/// Response body for `GET /topics/{topic}/consumers/{consumer}/checkpoint`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckpointResponse {
    pub checkpoint: Option<ConsumerCheckpoint>,
}

/// Request body for `PUT /topics/{topic}/consumers/{consumer}/checkpoint`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CheckpointPutRequest {
    pub offset: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionCreateRequest {
    pub name: String,
    #[serde(rename = "ackDeadlineSeconds", default)]
    #[schema(value_type = Option<u64>, default = 10, minimum = 10, maximum = 600)]
    pub ack_deadline_seconds: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubscriptionListResponse {
    pub subscriptions: Vec<Subscription>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SubscriptionPullRequest {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SubscriptionAckRequest {
    pub offset: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionGetResponse {
    pub policy: Option<RetentionPolicy>,
}
