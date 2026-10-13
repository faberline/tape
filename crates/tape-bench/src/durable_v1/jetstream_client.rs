//! The real-protocol client used by the durable-v1 comparison harness.
//!
//! Benchmark-only: compiled only with the `jetstream-benchmark` feature of
//! `tape-bench`. [`run`] drives NATS JetStream FileStore; [`run_tape`] drives
//! the same public HTTP contract on the Tape side. Both emit one
//! [`BenchmarkRecord`] per cell, which `benchmarks/jetstream/parse_records.py`
//! validates.

mod nats;
mod tape_http;

pub use nats::run;
pub use tape_http::run_tape;

use super::DurableV1Profile;
use crate::report::percentile;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Pull and acknowledgement batch size on both sides.
pub const BATCH: usize = 100;

const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const READINESS_MAX_BACKOFF: Duration = Duration::from_secs(1);

/// The persisted replay backlog a phase writes: none for a measured `normal`
/// sample, the full fixed backlog for `finalize`.
pub(crate) fn replay_events_for_phase(config: &ClientConfig) -> usize {
    if config.phase == "normal" {
        0
    } else {
        config.replay_events
    }
}

/// One cell's client configuration, as the tester Job passes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    pub tape_url: String,
    pub nats_url: String,
    pub topic: String,
    pub payload_bytes: usize,
    pub clients: usize,
    pub sample_index: usize,
    pub operations_per_client: usize,
    pub replay_events: usize,
    pub sample_seconds: u64,
    pub profile: String,
    pub phase: String,
    pub run_id: String,
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl ClientConfig {
    /// Reject a configuration outside the frozen `durable-v1` profile.
    pub fn validate(&self, profile: &DurableV1Profile) -> Result<(), String> {
        profile.validate()?;
        if self.profile != "durable-v1" {
            return Err("profile must be durable-v1".into());
        }
        if !matches!(self.phase.as_str(), "normal" | "finalize" | "recovery") {
            return Err("phase must be normal, finalize, or recovery".into());
        }
        if self.topic.is_empty() {
            return Err("topic is required".into());
        }
        if !is_safe_identifier(&self.run_id) {
            return Err("run_id must be a non-empty safe identifier".into());
        }
        if !profile.payload_bytes.contains(&self.payload_bytes) {
            return Err("payload_bytes is not in durable-v1".into());
        }
        if !profile.clients.contains(&self.clients) {
            return Err("clients is not in durable-v1".into());
        }
        if self.sample_index >= profile.samples {
            return Err("sample_index is out of range".into());
        }
        if self.operations_per_client == 0 || self.replay_events != profile.replay_events {
            return Err("operation and replay counts do not match durable-v1".into());
        }
        if self.sample_seconds != profile.sample_seconds {
            return Err("sample_seconds must be exactly 60 for durable-v1".into());
        }
        Ok(())
    }

    /// [`ClientConfig::validate`], plus the endpoint the chosen target needs.
    pub fn validate_for_target(
        &self,
        target: &str,
        profile: &DurableV1Profile,
    ) -> Result<(), String> {
        self.validate(profile)?;
        match target {
            "tape" if self.tape_url.is_empty() => Err("Tape endpoint is required".into()),
            "jetstream" if self.nats_url.is_empty() => Err("NATS endpoint is required".into()),
            "tape" | "jetstream" => Ok(()),
            _ => Err("target must be tape or jetstream".into()),
        }
    }
}

/// Every `(payload_bytes, clients, sample_index)` cell a suite phase runs:
/// five samples per cell for `normal`, one for `recovery`.
pub fn matrix_cells(phase: &str) -> Result<Vec<(usize, usize, usize)>, String> {
    let profile = DurableV1Profile::default();
    if !matches!(phase, "normal" | "recovery") {
        return Err("phase must be normal or recovery".into());
    }
    let samples = if phase == "normal" {
        profile.samples
    } else {
        1
    };
    Ok(profile
        .payload_bytes
        .iter()
        .flat_map(|&payload| {
            profile.clients.iter().flat_map(move |&clients| {
                (0..samples).map(move |sample| (payload, clients, sample))
            })
        })
        .collect())
}

/// The persisted-backlog replay result of one cell.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReplayResult {
    pub expected: usize,
    pub received: usize,
    pub loss_count: usize,
    pub duplicate_count: usize,
    pub error_count: usize,
    pub passed: bool,
}

/// One `tape-vs-jetstream-record.v1` JSON line.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkRecord {
    pub schema: &'static str,
    pub target: &'static str,
    pub run_id: String,
    pub topic: String,
    pub cell: Cell,
    pub sample_index: usize,
    pub phase: &'static str,
    pub operation_count: usize,
    pub elapsed_ms: u128,
    pub throughput_ops_per_sec: f64,
    pub p99_ms: u128,
    /// Measured acknowledgement latency percentile (kept separate from publish latency).
    pub ack_p99_ms: u128,
    pub replay: ReplayResult,
    pub ack_batch_size: usize,
    pub ack_cumulative: bool,
    pub loss_count: usize,
    pub duplicate_count: usize,
    pub error_count: usize,
    pub verdict: &'static str,
    pub failure: Option<String>,
}

/// The payload size and client count that identify a matrix cell.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cell {
    pub payload_bytes: usize,
    pub clients: usize,
}

impl BenchmarkRecord {
    /// Fail closed on a record that cannot count as durable-v1 evidence.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "tape-vs-jetstream-record.v1"
            || !matches!(self.target, "tape" | "jetstream")
        {
            return Err("invalid benchmark record identity".into());
        }
        if !is_safe_identifier(&self.run_id) || self.topic.is_empty() {
            return Err("invalid benchmark sample identity".into());
        }
        if !matches!(self.phase, "normal" | "finalize" | "recovery") {
            return Err("invalid benchmark phase".into());
        }
        if self.ack_batch_size != BATCH {
            return Err("benchmark requires batch-100 ack".into());
        }
        if (self.target == "tape") != self.ack_cumulative {
            return Err("ack mode does not match benchmark target".into());
        }
        if self.verdict == "incomplete" {
            if self.failure.as_deref().unwrap_or("").is_empty() {
                return Err("incomplete benchmark record requires failure cause".into());
            }
            return Ok(());
        }
        if self.phase == "normal" && self.replay.expected != 0 {
            return Err("normal benchmark record must not contain finalized replay".into());
        }
        if self.phase == "finalize" && self.replay.expected != 100_000 {
            return Err("finalize benchmark record requires 100000 replay events".into());
        }
        if self.operation_count == 0 || !self.throughput_ops_per_sec.is_finite() {
            return Err("invalid measured operation data".into());
        }
        if self.replay.received != self.replay.expected
            || !self.replay.passed
            || self.error_count != 0
        {
            return Err("durable replay or operation errors".into());
        }
        Ok(())
    }
}

/// The JetStream subject one cell publishes to.
pub fn subject(config: &ClientConfig) -> String {
    format!(
        "tape.bench.{}.{}.{}",
        config.run_id, config.payload_bytes, config.clients
    )
}

/// The Tape subscription a `normal` sample uses for its cell.
pub fn tape_subscription(config: &ClientConfig) -> String {
    tape_subscription_for_phase(config, "normal")
}

/// The Tape subscription one phase of a cell uses.
pub fn tape_subscription_for_phase(config: &ClientConfig, phase: &str) -> String {
    format!(
        "bench-{}-{}-{}-{}",
        config.run_id, config.payload_bytes, config.clients, phase
    )
}

/// The offset of `replay-0`, the first event of the fixed replay backlog.
pub fn replay_start(events: &[TapeEvent]) -> Result<u64, String> {
    events
        .iter()
        .find(|e| e.key.as_deref() == Some("replay-0"))
        .map(|e| e.offset)
        .ok_or_else(|| "replay-0 is missing from backlog window".into())
}

/// Skip the measured prefix of a pulled page and validate the replay suffix.
/// Returns the prefix length and the next expected replay index.
pub fn scan_replay_page(
    events: &[TapeEvent],
    expected_index: usize,
) -> Result<(usize, usize), String> {
    let start = events
        .iter()
        .position(|e| e.key.as_deref() == Some("replay-0"));
    let Some(start) = start else {
        if events
            .iter()
            .any(|e| e.key.as_deref().is_some_and(|k| k.starts_with("replay-")))
        {
            return Err("replay page contains replay key before replay-0".into());
        }
        return Ok((events.len(), expected_index));
    };
    let next = validate_replay_identity(&events[start..], events[start].offset, expected_index)?;
    Ok((start, next))
}

pub(crate) fn readiness_backoff(attempt: u32) -> Duration {
    READINESS_INITIAL_BACKOFF
        .saturating_mul(2u32.saturating_pow(attempt.min(4)))
        .min(READINESS_MAX_BACKOFF)
}

/// The p99 of unsorted latency samples; 0 when no sample was taken (a phase
/// that issues no acknowledgement, for example).
pub(crate) fn p99(mut samples: Vec<u128>) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    percentile(&samples, 0.99)
}

/// A record for a cell that did not complete, carrying its failure cause.
pub fn incomplete_record(
    config: &ClientConfig,
    target: &'static str,
    cause: String,
) -> BenchmarkRecord {
    BenchmarkRecord {
        schema: "tape-vs-jetstream-record.v1",
        target,
        run_id: config.run_id.clone(),
        topic: config.topic.clone(),
        cell: Cell {
            payload_bytes: config.payload_bytes,
            clients: config.clients,
        },
        sample_index: config.sample_index,
        phase: match config.phase.as_str() {
            "recovery" => "recovery",
            "finalize" => "finalize",
            _ => "normal",
        },
        operation_count: 0,
        elapsed_ms: 0,
        throughput_ops_per_sec: 0.0,
        p99_ms: 0,
        ack_p99_ms: 0,
        replay: ReplayResult {
            expected: config.replay_events,
            received: 0,
            loss_count: config.replay_events,
            duplicate_count: 0,
            error_count: 1,
            passed: false,
        },
        ack_batch_size: BATCH,
        ack_cumulative: target == "tape",
        loss_count: config.replay_events,
        duplicate_count: 0,
        error_count: 1,
        verdict: "incomplete",
        failure: Some(cause),
    }
}

/// One event of a Tape subscription pull page.
#[derive(Clone, Debug, Deserialize)]
pub struct TapeEvent {
    pub offset: u64,
    pub key: Option<String>,
    pub payload: Value,
}

/// Require contiguous offsets and `replay-<n>` keys from `first_index` on.
/// Returns the next expected replay index.
pub fn validate_replay_identity(
    events: &[TapeEvent],
    first_offset: u64,
    first_index: usize,
) -> Result<usize, String> {
    for (position, event) in events.iter().enumerate() {
        let expected_offset = first_offset + position as u64;
        let expected_key = format!("replay-{}", first_index + position);
        if event.offset != expected_offset {
            return Err("replay offsets are not contiguous".into());
        }
        if event.key.as_deref() != Some(expected_key.as_str()) {
            return Err("replay event identity mismatch".into());
        }
    }
    Ok(first_index + events.len())
}

#[cfg(test)]
mod tests;
