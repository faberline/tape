//! The event envelope and the consumer checkpoint: the two records every topic
//! holds. Both are published as-is on the HTTP API and in snapshots.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TapeEvent {
    pub topic: String,
    pub offset: u64,
    pub timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConsumerCheckpoint {
    pub topic: String,
    pub consumer: String,
    pub offset: u64,
    pub updated_at_ms: u64,
}
