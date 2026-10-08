//! Per-topic retention: the policy a caller sets and what applying it removed.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RetentionPolicy {
    /// Explicit lower bound: events below this offset are eligible for removal.
    #[serde(default)]
    pub min_offset: Option<u64>,
    /// Events older than this wall-clock window are eligible for removal.
    #[serde(default)]
    pub max_age_seconds: Option<u64>,
    /// Never prune beyond the oldest named consumer checkpoint.
    #[serde(default)]
    pub protected_consumers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RetentionOutcome {
    pub topic: String,
    pub policy: RetentionPolicy,
    pub earliest_offset: u64,
    pub end_offset: u64,
    pub removed: usize,
}
