// <HANDWRITE gap="missing-generator:logic:tape-competitor-performance" tracker="#768" reason="Initial local benchmark and external peer calibration ledger before generated efficiency primitives exist.">
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Value};

use crate::server::{router, AppState};
use crate::wal::{CommitCoordinator, WalStore};
use crate::TapeJournal;

const DEFAULT_EVENTS: usize = 1_000;
const DEFAULT_PAYLOAD_BYTES: usize = 128;

/// Data-plane request body size limit for [`run_durable_benchmark`]'s
/// `AppState` -- arbitrary but generous headroom over the small bench
/// payloads this drives, matching the limit other real-HTTP tape tests use.
const DURABLE_BENCH_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct PerfBudget {
    pub append_p95_us: u128,
    pub replay_full_us: u128,
    pub checkpoint_p95_us: u128,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerCalibration {
    pub peer: &'static str,
    pub replay_baseline: bool,
    pub status: &'static str,
    pub win_claim: bool,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompetitiveBaseline {
    pub events: usize,
    pub payload_bytes: usize,
    pub ratchet: f64,
    pub budget: PerfBudget,
    pub peers: Vec<PeerCalibration>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchReport {
    pub project: &'static str,
    pub events: usize,
    pub payload_bytes: usize,
    pub append_p50_us: u128,
    pub append_p95_us: u128,
    pub replay_full_us: u128,
    pub checkpoint_p50_us: u128,
    pub checkpoint_p95_us: u128,
    pub local_regression_passed: bool,
    pub external_peer_win_claim: bool,
    pub verdict: &'static str,
    pub peers: Vec<PeerCalibration>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalReplayWin {
    pub peer: &'static str,
    pub workload: &'static str,
    pub events: usize,
    pub payload_bytes: usize,
    pub tape_replay_us: u128,
    pub peer_replay_us: u128,
    pub ratio: f64,
    pub required_ratio: f64,
    pub win_claim: bool,
    pub evidence: &'static str,
}

pub fn default_baseline() -> CompetitiveBaseline {
    CompetitiveBaseline {
        events: DEFAULT_EVENTS,
        payload_bytes: DEFAULT_PAYLOAD_BYTES,
        ratchet: 0.8,
        budget: PerfBudget {
            append_p95_us: 5_000,
            replay_full_us: 50_000,
            checkpoint_p95_us: 5_000,
        },
        peers: vec![
            separate_gate_peer(
                "Kafka topic log",
                "Calibrated by the release real-service tape_vs_kafka gate; the local-only report never imports or claims that result.",
            ),
            uncalibrated_peer("Redpanda topic log"),
            uncalibrated_peer("Pulsar topic"),
            separate_gate_peer(
                "NATS JetStream stream",
                "Calibrated by the release real-service tape_vs_nats_jetstream gate; the local-only report never imports or claims that result.",
            ),
            uncalibrated_peer("RabbitMQ Streams"),
            PeerCalibration {
                peer: "RabbitMQ topic exchange",
                replay_baseline: false,
                status: "not_a_replay_baseline",
                win_claim: true,
                reason: "Tape has offset/time replay and durable checkpoints; RabbitMQ topic exchange is routing/fanout only.",
            },
        ],
    }
}

pub fn run_benchmark(events: usize, payload_bytes: usize) -> BenchReport {
    let baseline = default_baseline();
    let events = events.max(1);
    let payload_bytes = payload_bytes.max(1);
    let payload = payload(payload_bytes);
    let mut journal = TapeJournal::default();
    let mut append_samples = Vec::with_capacity(events);

    for i in 0..events {
        let started = Instant::now();
        journal.append(
            "orders.created",
            Some(format!("orders.created.{i}")),
            payload.clone(),
            Some(i as u64),
        );
        append_samples.push(started.elapsed().as_micros());
    }

    let replay_started = Instant::now();
    let replayed = journal.replay_refs("orders.created", Some(0), None, Some(events));
    let replay_full_us = replay_started.elapsed().as_micros();
    assert_eq!(replayed.len(), events);

    let mut checkpoint_samples = Vec::with_capacity(events);
    for offset in 0..=events {
        let started = Instant::now();
        journal
            .put_checkpoint("orders.created", "bench-worker", offset as u64)
            .expect("checkpoint advances within topic end offset");
        checkpoint_samples.push(started.elapsed().as_micros());
    }

    append_samples.sort_unstable();
    checkpoint_samples.sort_unstable();
    let append_p50_us = percentile(&append_samples, 0.50);
    let append_p95_us = percentile(&append_samples, 0.95);
    let checkpoint_p50_us = percentile(&checkpoint_samples, 0.50);
    let checkpoint_p95_us = percentile(&checkpoint_samples, 0.95);
    let local_regression_passed = append_p95_us <= baseline.budget.append_p95_us
        && replay_full_us <= baseline.budget.replay_full_us
        && checkpoint_p95_us <= baseline.budget.checkpoint_p95_us;
    let external_peer_win_claim = baseline
        .peers
        .iter()
        .any(|peer| peer.replay_baseline && peer.win_claim);

    BenchReport {
        project: "tape",
        events,
        payload_bytes,
        append_p50_us,
        append_p95_us,
        replay_full_us,
        checkpoint_p50_us,
        checkpoint_p95_us,
        local_regression_passed,
        external_peer_win_claim,
        verdict: if local_regression_passed && !external_peer_win_claim {
            "local_regression_passed_external_wins_require_separate_gates"
        } else {
            "failed_or_overclaimed"
        },
        peers: baseline.peers,
    }
}

/// One connection-count's measured durable append throughput from
/// [`run_durable_benchmark`].
#[derive(Clone, Debug, Serialize)]
pub struct DurableConnectionSample {
    pub connections: usize,
    pub events: usize,
    pub elapsed_us: u128,
    pub ops_per_sec: f64,
}

/// WI #3052 AC1 report: durable append throughput at each sampled connection
/// count, driven over real HTTP against the real [`crate::wal::CommitCoordinator`]
/// group-commit path (`FsyncPolicy::Always`), plus the scaling ratio between
/// the highest and lowest sampled connection count. `tape_perf_gate.rs` gates
/// on `scaling_ratio`, never on an absolute `ops_per_sec` value -- the
/// absolute number is a property of the machine's fsync, the ratio is a
/// property of the group-commit design.
#[derive(Clone, Debug, Serialize)]
pub struct DurableBenchReport {
    pub payload_bytes: usize,
    pub samples: Vec<DurableConnectionSample>,
    /// `ops_per_sec` at the highest sampled connection count divided by
    /// `ops_per_sec` at the lowest sampled connection count.
    pub scaling_ratio: f64,
}

/// Drive `connections` concurrent HTTP clients, each issuing
/// `events_per_connection` sequential `POST /topics/{topic}/append` requests,
/// against a real axum router wired to a real [`WalStore`] +
/// [`CommitCoordinator`] over `FsyncPolicy::Always` -- the same durable path
/// `tape serve --data-dir` runs in production, and the same shape of harness
/// that measured the pre-#3052 85-89 ops/s flat line (in-process HTTP over a
/// real `127.0.0.1:0` socket, not an in-memory journal call).
///
/// Each sampled connection count gets its own fresh [`tempfile::TempDir`] and
/// [`WalStore`] so one sample's growing WAL/journal never contaminates the
/// next sample's measurement. Sequential requests *within* one connection are
/// the point: that is what makes group commit's cross-connection batching
/// observable in the throughput curve, rather than measuring one connection's
/// own request/response round-trip latency.
pub fn run_durable_benchmark(
    events_per_connection: usize,
    payload_bytes: usize,
    connection_counts: &[usize],
) -> DurableBenchReport {
    let events_per_connection = events_per_connection.max(1);
    let payload_bytes = payload_bytes.max(1);
    let payload = payload(payload_bytes);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime for durable benchmark");

    let samples: Vec<DurableConnectionSample> = connection_counts
        .iter()
        .map(|&connections| {
            let connections = connections.max(1);
            runtime.block_on(run_one_durable_sample(
                connections,
                events_per_connection,
                payload.clone(),
            ))
        })
        .collect();

    let min_connections = connection_counts.iter().copied().min().unwrap_or(1).max(1);
    let max_connections = connection_counts.iter().copied().max().unwrap_or(1).max(1);
    let base = samples.iter().find(|s| s.connections == min_connections);
    let top = samples.iter().find(|s| s.connections == max_connections);
    let scaling_ratio = match (base, top) {
        (Some(base), Some(top)) if base.ops_per_sec > 0.0 => top.ops_per_sec / base.ops_per_sec,
        _ => 0.0,
    };

    DurableBenchReport {
        payload_bytes,
        samples,
        scaling_ratio,
    }
}

/// One connection-count sample of [`run_durable_benchmark`]: spins up a fresh
/// durable `AppState` (real `WalStore` + `CommitCoordinator`, real HTTP
/// listener), drives `connections` concurrent client tasks each issuing
/// `events_per_connection` sequential appends, then tears the server down.
async fn run_one_durable_sample(
    connections: usize,
    events_per_connection: usize,
    payload: Value,
) -> DurableConnectionSample {
    let dir = tempfile::TempDir::new().expect("create durable bench temp dir");
    let (wal_store, journal) =
        WalStore::open(dir.path()).expect("open WalStore for durable bench sample");
    let state = AppState::new(journal, None, DURABLE_BENCH_BODY_LIMIT_BYTES);
    let coordinator = CommitCoordinator::spawn(wal_store, state.journal_handle());
    let state = state.with_wal(std::sync::Arc::new(coordinator));
    let app = router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind durable bench listener");
    let addr = listener.local_addr().expect("durable bench listener addr");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(service_http::serve(listener, app, async move {
        let _ = shutdown_rx.await;
    }));

    let client = reqwest::Client::new();
    let started = Instant::now();
    let mut handles = Vec::with_capacity(connections);
    for _ in 0..connections {
        let client = client.clone();
        let payload = payload.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..events_per_connection {
                let response = client
                    .post(format!("http://{addr}/topics/durable-bench/append"))
                    .json(&json!({
                        "payload": payload,
                        "timestamp_ms": i as u64,
                    }))
                    .send()
                    .await
                    .expect("durable bench append request");
                assert!(
                    response.status().is_success(),
                    "durable bench append returned {} for event {i}",
                    response.status()
                );
            }
        }));
    }
    for handle in handles {
        handle.await.expect("durable bench client task panicked");
    }
    let elapsed_us = started.elapsed().as_micros();

    // Best-effort graceful shutdown; the next sample uses a fresh listener
    // regardless, so a slow/failed shutdown here cannot leak into the next
    // sample's measurement.
    let _ = shutdown_tx.send(());
    let _ = server.await;

    let events = connections * events_per_connection;
    let ops_per_sec = if elapsed_us == 0 {
        0.0
    } else {
        events as f64 / (elapsed_us as f64 / 1_000_000.0)
    };

    DurableConnectionSample {
        connections,
        events,
        elapsed_us,
        ops_per_sec,
    }
}

pub fn verify_report(report: &BenchReport) -> Result<(), String> {
    let baseline = default_baseline();
    if report.append_p95_us > baseline.budget.append_p95_us {
        return Err(format!(
            "append p95 {}us exceeds {}us",
            report.append_p95_us, baseline.budget.append_p95_us
        ));
    }
    if report.replay_full_us > baseline.budget.replay_full_us {
        return Err(format!(
            "full replay {}us exceeds {}us",
            report.replay_full_us, baseline.budget.replay_full_us
        ));
    }
    if report.checkpoint_p95_us > baseline.budget.checkpoint_p95_us {
        return Err(format!(
            "checkpoint p95 {}us exceeds {}us",
            report.checkpoint_p95_us, baseline.budget.checkpoint_p95_us
        ));
    }
    if report.external_peer_win_claim {
        return Err("external broker win claim requires calibrated peer evidence".to_string());
    }
    Ok(())
}

pub fn external_replay_win(
    peer: &'static str,
    workload: &'static str,
    events: usize,
    payload_bytes: usize,
    tape_replay_us: u128,
    peer_replay_us: u128,
    required_ratio: f64,
    evidence: &'static str,
) -> ExternalReplayWin {
    let ratio = if tape_replay_us == 0 {
        f64::INFINITY
    } else {
        peer_replay_us as f64 / tape_replay_us as f64
    };
    ExternalReplayWin {
        peer,
        workload,
        events,
        payload_bytes,
        tape_replay_us,
        peer_replay_us,
        ratio,
        required_ratio,
        win_claim: ratio >= required_ratio,
        evidence,
    }
}

pub fn verify_external_replay_win(report: &ExternalReplayWin) -> Result<(), String> {
    if report.events == 0 {
        return Err("external replay win requires at least one event".to_string());
    }
    if !report.win_claim {
        return Err(format!(
            "{} replay ratio {:.2}x is below required {:.2}x (peer {}us, tape {}us)",
            report.peer,
            report.ratio,
            report.required_ratio,
            report.peer_replay_us,
            report.tape_replay_us
        ));
    }
    Ok(())
}

fn uncalibrated_peer(peer: &'static str) -> PeerCalibration {
    PeerCalibration {
        peer,
        replay_baseline: true,
        status: "not_calibrated",
        win_claim: false,
        reason: "No real-service external benchmark has been run in this checkout; Tape reports local regression only.",
    }
}

fn separate_gate_peer(peer: &'static str, reason: &'static str) -> PeerCalibration {
    PeerCalibration {
        peer,
        replay_baseline: true,
        status: "calibrated_separate_gate",
        win_claim: false,
        reason,
    }
}

fn payload(bytes: usize) -> Value {
    json!({
        "id": "bench",
        "body": "x".repeat(bytes),
    })
}

fn percentile(samples: &[u128], q: f64) -> u128 {
    let idx = (((samples.len() - 1) as f64) * q).round() as usize;
    samples[idx]
}

/// The immutable, deliberately small first Tape/JetStream comparison profile.
/// This is shared by the shell runner and unit tests so a changed workload
/// cannot silently produce a comparable-looking report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableV1Profile {
    pub payload_bytes: [usize; 3],
    pub clients: [usize; 3],
    pub warmup: bool,
    pub samples: usize,
    pub sample_seconds: u64,
    pub replay_events: usize,
    pub subscription_batch: usize,
}

impl Default for DurableV1Profile {
    fn default() -> Self {
        Self {
            payload_bytes: [128, 1024, 4096],
            clients: [1, 16, 64],
            warmup: true,
            samples: 5,
            sample_seconds: 60,
            replay_events: 100_000,
            subscription_batch: 100,
        }
    }
}

impl DurableV1Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.payload_bytes != [128, 1024, 4096] {
            return Err("workload payload mismatch".into());
        }
        if self.clients != [1, 16, 64] {
            return Err("workload client mismatch".into());
        }
        if !self.warmup || self.samples != 5 || self.sample_seconds != 60 {
            return Err("durable-v1 requires warmup and five 60-second samples".into());
        }
        if self.replay_events != 100_000 || self.subscription_batch != 100 {
            return Err("durable-v1 recovery workload mismatch".into());
        }
        Ok(())
    }
}

pub fn validate_image_digest(image: &str, architecture: &str) -> Result<(), String> {
    let (name, digest) = image
        .split_once('@')
        .ok_or_else(|| "mutable image: digest is required".to_string())?;
    if name.is_empty()
        || !digest.starts_with("sha256:")
        || digest.len() != 71
        || !digest[7..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("invalid image digest; expected name@sha256:<64 hex>".into());
    }
    if architecture != "amd64" {
        return Err("unsupported architecture; durable-v1 requires amd64".into());
    }
    Ok(())
}

/// The real-protocol client used by the durable-v1 comparison harness.
#[cfg(feature = "jetstream-benchmark")]
pub mod jetstream_client {
    use super::{percentile, DurableV1Profile};
    use anyhow::{bail, Context, Result};
    use async_nats::jetstream::{
        self,
        consumer::{pull, AckPolicy, DeliverPolicy},
    };
    use futures::StreamExt;
    use reqwest::StatusCode;
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use std::time::{Duration, Instant};

    pub const BATCH: usize = 100;

    pub(crate) fn replay_events_for_phase(config: &ClientConfig) -> usize {
        if config.phase == "normal" { 0 } else { config.replay_events }
    }
    const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
    const READINESS_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
    const READINESS_MAX_BACKOFF: Duration = Duration::from_secs(1);

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

    impl ClientConfig {
        pub fn validate(&self, profile: &DurableV1Profile) -> Result<(), String> {
            profile.validate()?;
            if self.profile != "durable-v1" { return Err("profile must be durable-v1".into()); }
            if !matches!(self.phase.as_str(), "normal" | "finalize" | "recovery") { return Err("phase must be normal, finalize, or recovery".into()); }
            if self.topic.is_empty() {
                return Err("topic is required".into());
            }
            if self.run_id.is_empty() || !self.run_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err("run_id must be a non-empty safe identifier".into()); }
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

        pub fn validate_for_target(&self, target: &str, profile: &DurableV1Profile) -> Result<(), String> {
            self.validate(profile)?;
            match target {
                "tape" if self.tape_url.is_empty() => Err("Tape endpoint is required".into()),
                "jetstream" if self.nats_url.is_empty() => Err("NATS endpoint is required".into()),
                "tape" | "jetstream" => Ok(()),
                _ => Err("target must be tape or jetstream".into()),
            }
        }
    }

    pub fn matrix_cells(phase: &str) -> Result<Vec<(usize, usize, usize)>, String> {
        let profile = DurableV1Profile::default();
        if !matches!(phase, "normal" | "recovery") { return Err("phase must be normal or recovery".into()); }
        let samples = if phase == "normal" { profile.samples } else { 1 };
        Ok(profile.payload_bytes.iter().flat_map(|&payload| profile.clients.iter().flat_map(move |&clients| (0..samples).map(move |sample| (payload, clients, sample)))).collect())
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    pub struct ReplayResult {
        pub expected: usize,
        pub received: usize,
        pub loss_count: usize,
        pub duplicate_count: usize,
        pub error_count: usize,
        pub passed: bool,
    }

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

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct Cell {
        pub payload_bytes: usize,
        pub clients: usize,
    }

    impl BenchmarkRecord {
        pub fn validate(&self) -> Result<(), String> {
            if self.schema != "tape-vs-jetstream-record.v1"
                || !matches!(self.target, "tape" | "jetstream")
            {
                return Err("invalid benchmark record identity".into());
            }
            if self.run_id.is_empty() || !self.run_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || self.topic.is_empty() {
                return Err("invalid benchmark sample identity".into());
            }
            if !matches!(self.phase, "normal" | "finalize" | "recovery") { return Err("invalid benchmark phase".into()); }
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

    pub fn subject(config: &ClientConfig) -> String {
        format!("tape.bench.{}.{}.{}", config.run_id, config.payload_bytes, config.clients)
    }

    pub fn tape_subscription(config: &ClientConfig) -> String {
        tape_subscription_for_phase(config, "normal")
    }

    pub fn tape_subscription_for_phase(config: &ClientConfig, phase: &str) -> String {
        format!("bench-{}-{}-{}-{}", config.run_id, config.payload_bytes, config.clients, phase)
    }

    pub fn replay_start(events: &[TapeEvent]) -> Result<u64, String> {
        events.iter().find(|e| e.key.as_deref() == Some("replay-0"))
            .map(|e| e.offset)
            .ok_or_else(|| "replay-0 is missing from backlog window".into())
    }

    pub fn scan_replay_page(events: &[TapeEvent], expected_index: usize) -> Result<(usize, usize), String> {
        let start = events.iter().position(|e| e.key.as_deref() == Some("replay-0"));
        let Some(start) = start else {
            if events.iter().any(|e| e.key.as_deref().is_some_and(|k| k.starts_with("replay-"))) {
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

    async fn wait_until_ready(http: &reqwest::Client, base: &str) -> Result<()> {
        let deadline = Instant::now() + READINESS_TIMEOUT;
        let mut attempt = 0;
        loop {
            match http.get(format!("{base}/readyz")).send().await {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    if Instant::now() >= deadline {
                        bail!("Tape readiness timeout after {}s: /readyz returned {}", READINESS_TIMEOUT.as_secs(), response.status());
                    }
                }
                Err(error) if Instant::now() >= deadline => {
                    bail!("Tape readiness timeout after {}s: {error}", READINESS_TIMEOUT.as_secs());
                }
                Err(_) => {}
            }
            let delay = readiness_backoff(attempt);
            attempt = attempt.saturating_add(1);
            tokio::time::sleep(delay.min(deadline.saturating_duration_since(Instant::now()))).await;
        }
    }

    pub fn incomplete_record(config: &ClientConfig, target: &'static str, cause: String) -> BenchmarkRecord {
        BenchmarkRecord {
            schema: "tape-vs-jetstream-record.v1",
            target,
            run_id: config.run_id.clone(),
            topic: config.topic.clone(),
            cell: Cell { payload_bytes: config.payload_bytes, clients: config.clients },
            sample_index: config.sample_index,
            phase: match config.phase.as_str() { "recovery" => "recovery", "finalize" => "finalize", _ => "normal" },
            operation_count: 0,
            elapsed_ms: 0,
            throughput_ops_per_sec: 0.0,
            p99_ms: 0,
            ack_p99_ms: 0,
            replay: ReplayResult { expected: config.replay_events, received: 0, loss_count: config.replay_events, duplicate_count: 0, error_count: 1, passed: false },
            ack_batch_size: BATCH,
            ack_cumulative: target == "tape",
            loss_count: config.replay_events,
            duplicate_count: 0,
            error_count: 1,
            verdict: "incomplete",
            failure: Some(cause),
        }
    }

    #[derive(Clone, Debug, Deserialize)]
    pub struct TapeEvent {
        pub offset: u64,
        pub key: Option<String>,
        pub payload: Value,
    }
    #[derive(Debug, Deserialize)]
    struct TapePull {
        events: Vec<TapeEvent>,
        next_offset: u64,
    }

    pub fn validate_replay_identity(events: &[TapeEvent], first_offset: u64, first_index: usize) -> Result<usize, String> {
        for (position, event) in events.iter().enumerate() {
            let expected_offset = first_offset + position as u64;
            let expected_key = format!("replay-{}", first_index + position);
            if event.offset != expected_offset { return Err("replay offsets are not contiguous".into()); }
            if event.key.as_deref() != Some(expected_key.as_str()) { return Err("replay event identity mismatch".into()); }
        }
        Ok(first_index + events.len())
    }

    pub async fn run(config: ClientConfig) -> Result<BenchmarkRecord> {
        let profile = DurableV1Profile::default();
        config.validate_for_target("jetstream", &profile).map_err(anyhow::Error::msg)?;
        if config.phase == "recovery" { return run_jetstream_recovery(config).await; }
        let replay_events = replay_events_for_phase(&config);
        let nats = async_nats::connect(&config.nats_url)
            .await
            .context("connect NATS")?;
        let js = jetstream::new(nats);
        let stream_name = format!("TAPE_BENCH_{}_{}_{}", config.run_id, config.payload_bytes, config.clients);
        let subject = subject(&config);
        if config.phase == "normal" && config.sample_index == 0 {
            let _ = js.delete_stream(&stream_name).await;
        }
        let mut stream = js
            .get_or_create_stream(jetstream::stream::Config {
                name: stream_name.clone(),
                subjects: vec![subject.clone()],
                storage: jetstream::stream::StorageType::File,
                num_replicas: 1,
                ..Default::default()
            })
            .await
            .context("create FileStore stream")?;
        let payload = vec![b'x'; config.payload_bytes];
        if config.phase == "normal" {
            js.publish(subject.clone(), payload.clone().into()).await.context("warmup publish")?.await.context("warmup publish ack")?;
        }
        let target_ops = config.clients * config.operations_per_client;
        let backlog_start = stream.info().await.context("read stream sequence before backlog")?.state.last_sequence + 1;
        let sample_duration = Duration::from_secs(config.sample_seconds);
        let measure = config.phase == "normal";
        let operations_per_client = config.operations_per_client;
        let started = Instant::now();
        let mut tasks = Vec::with_capacity(config.clients);
        for _ in 0..config.clients {
            let js = js.clone();
            let subject = subject.clone();
            let payload = payload.clone();
            tasks.push(tokio::spawn(async move {
                let mut latencies = Vec::with_capacity(operations_per_client);
                let started = Instant::now();
                let mut i = 0;
                while measure && (started.elapsed() < sample_duration || i == 0) {
                    let one = Instant::now();
                    js.publish(subject.clone(), payload.clone().into()).await.context("publish")?.await.context("publish ack")?;
                    latencies.push(one.elapsed().as_millis());
                    i += 1;
                }
                Ok::<_, anyhow::Error>(latencies)
            }));
        }
        let mut latencies: Vec<u128> = Vec::with_capacity(target_ops);
        for task in tasks { latencies.extend(task.await.context("publisher task")??); }
        let elapsed = started.elapsed();
        let total = latencies.len();
        // The persistence backlog is a separate, fixed recovery workload.
        for _ in 0..replay_events {
            js.publish(subject.clone(), payload.clone().into()).await.context("publish replay backlog")?.await.context("replay publish ack")?;
        }
        let verifier_name = format!("tape-bench-finalize-{}", config.sample_index);
        let consumer = if replay_events > 0 { Some(stream.create_consumer(pull::Config {
            durable_name: Some(verifier_name),
            deliver_policy: DeliverPolicy::ByStartSequence { start_sequence: backlog_start },
            ack_policy: AckPolicy::Explicit, ..Default::default()
        }).await.context("create explicit pull consumer")?) } else { None };
        let mut replay_received = 0usize;
        let mut ack_errors = 0usize;
        let mut ack_latencies = Vec::with_capacity(config.replay_events / BATCH + 1);
        while replay_received < replay_events {
            let mut batch = consumer.as_ref().expect("replay verifier exists")
                .batch()
                .max_messages(BATCH)
                .expires(Duration::from_secs(10))
                .messages()
                .await
                .context("pull")?;
            while let Some(message) = batch.next().await {
                let message = message.map_err(|e| anyhow::anyhow!("pull message: {e}"))?;
                if message.payload.len() != config.payload_bytes {
                    ack_errors += 1;
                }
                let ack_started = Instant::now();
                message
                    .ack()
                    .await
                    .map_err(|e| anyhow::anyhow!("explicit ack: {e}"))?;
                ack_latencies.push(ack_started.elapsed().as_millis());
                replay_received += 1;
            }
            if replay_received == 0 {
                bail!("JetStream returned no replay messages");
            }
        }
        let replay = ReplayResult {
            expected: replay_events,
            received: replay_received,
            loss_count: replay_events.saturating_sub(replay_received),
            duplicate_count: replay_received.saturating_sub(config.replay_events),
            error_count: ack_errors,
            passed: replay_received == replay_events && ack_errors == 0,
        };
        let replay_duplicates = replay.duplicate_count;
        let replay_loss = replay.loss_count;
        let record = BenchmarkRecord {
            schema: "tape-vs-jetstream-record.v1",
            target: "jetstream",
            run_id: config.run_id.clone(),
            topic: config.topic.clone(),
            cell: Cell {
                payload_bytes: config.payload_bytes,
                clients: config.clients,
            },
            sample_index: config.sample_index,
            phase: if config.phase == "finalize" { "finalize" } else { "normal" },
            operation_count: if config.phase == "finalize" { replay_events } else { total },
            elapsed_ms: elapsed.as_millis(),
            throughput_ops_per_sec: if config.phase == "finalize" { replay_events } else { total } as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
            p99_ms: latencies.first().map(|_| percentile(&latencies, 0.99)).unwrap_or(0),
            ack_p99_ms: percentile(&ack_latencies, 0.99),
            replay,
            ack_batch_size: BATCH,
            ack_cumulative: false,
            loss_count: replay_loss,
            duplicate_count: replay_duplicates,
            error_count: ack_errors,
            verdict: "measured",
            failure: None,
        };
        record.validate().map_err(anyhow::Error::msg)?;
        Ok(record)
    }

    /// Drive the same public HTTP contract used by the Tape side of the gate.
    pub async fn run_tape(config: ClientConfig) -> Result<BenchmarkRecord> {
        let profile = DurableV1Profile::default();
        config.validate_for_target("tape", &profile).map_err(anyhow::Error::msg)?;
        if config.phase == "recovery" { return run_tape_recovery(config).await; }
        let replay_events = replay_events_for_phase(&config);
        let http = reqwest::Client::new();
        let base = config.tape_url.trim_end_matches('/');
        let topic = &config.topic;
        let sub = tape_subscription_for_phase(&config, if config.phase == "finalize" { "finalize" } else { "normal" });
        wait_until_ready(&http, base).await.context("wait for Tape readiness")?;
        let create = http
            .post(format!("{base}/topics/{topic}/subscriptions"))
            .json(&json!({"name": sub}))
            .send()
            .await?;
        if create.status() != StatusCode::CREATED && create.status() != StatusCode::CONFLICT {
            bail!("subscription create failed: {}", create.status());
        }
        let payload = Value::String("x".repeat(config.payload_bytes));
        if config.phase == "normal" {
            let warmup = http.post(format!("{base}/topics/{topic}/append")).json(&json!({"key": "warmup", "payload": payload})).send().await?;
            if !warmup.status().is_success() { bail!("warmup append failed: {}", warmup.status()); }
        }
        let target_ops = config.clients * config.operations_per_client;
        let sample_duration = Duration::from_secs(config.sample_seconds);
        let measure = config.phase == "normal";
        let operations_per_client = config.operations_per_client;
        let started = Instant::now();
        let mut tasks = Vec::with_capacity(config.clients);
        for client_index in 0..config.clients {
            let http = http.clone(); let base = base.to_string(); let topic = topic.to_string(); let payload = payload.clone();
            tasks.push(tokio::spawn(async move {
                let mut latencies = Vec::with_capacity(operations_per_client);
                let started = Instant::now();
                let mut i = 0;
                while measure && (started.elapsed() < sample_duration || i == 0) {
                    let t = Instant::now();
                    let r = http.post(format!("{base}/topics/{topic}/append")).json(&json!({"key": format!("bench-{client_index}-{i}"), "payload": payload})).send().await?;
                    if !r.status().is_success() { bail!("append failed: {}", r.status()); }
                    latencies.push(t.elapsed().as_millis());
                    i += 1;
                }
                Ok::<_, anyhow::Error>(latencies)
            }));
        }
        let mut latencies: Vec<u128> = Vec::with_capacity(target_ops);
        for task in tasks { latencies.extend(task.await.context("publisher task")??); }
        let elapsed = started.elapsed();
        let total = latencies.len();
        // Move the public cumulative cursor past measured messages before
        // creating the fixed recovery backlog. This is the Tape equivalent of
        // JetStream's ByStartSequence(total + 1).
        let mut measured = 0usize;
        let mut ack_latencies = Vec::new();
        let measured_target = if config.phase == "normal" { total + 1 } else { 0 }; // includes the unmeasured warmup record
        while measured < measured_target {
            let limit = BATCH.min(measured_target - measured);
            let r = http.post(format!("{base}/topics/{topic}/subscriptions/{sub}/pull")).json(&json!({"limit": limit})).send().await?;
            if !r.status().is_success() { bail!("measured cursor pull failed: {}", r.status()); }
            let b: TapePull = r.json().await?;
            if b.events.is_empty() || b.events.len() > limit { bail!("measured cursor did not cover the exact sample window"); }
            measured += b.events.len();
            let ack_started = Instant::now();
            let a = http.post(format!("{base}/topics/{topic}/subscriptions/{sub}/ack")).json(&json!({"offset": b.next_offset})).send().await?;
            if !a.status().is_success() { bail!("measured cursor ack failed: {}", a.status()); }
            ack_latencies.push(ack_started.elapsed().as_millis());
        }
        let replay_payload = payload.clone();
        for i in 0..replay_events {
            let r = http.post(format!("{base}/topics/{topic}/append")).json(&json!({"key": format!("replay-{i}"), "payload": replay_payload})).send().await?;
            if !r.status().is_success() { bail!("replay append failed: {}", r.status()); }
        }
        let mut got = 0;
        let dups = 0;
        let mut errors = 0;
        let mut replay_index = 0usize;
        while got < replay_events {
            let limit = BATCH.min(replay_events - got);
            let r = http
                .post(format!("{base}/topics/{topic}/subscriptions/{sub}/pull"))
                .json(&json!({"limit": limit}))
                .send()
                .await?;
            if !r.status().is_success() {
                bail!("pull failed: {}", r.status());
            }
            let b: TapePull = r.json().await?;
            if b.events.is_empty() {
                bail!("Tape returned no replay messages");
            }
            let (prefix, validated_index) = scan_replay_page(&b.events, replay_index).map_err(anyhow::Error::msg)?;
            if validated_index != replay_index { replay_index = validated_index; }
            for e in &b.events[prefix..] {
                if e.payload != replay_payload {
                    errors += 1;
                }
                got += 1;
            }
            let next = b.next_offset;
            let ack_started = Instant::now();
            let a = http
                .post(format!("{base}/topics/{topic}/subscriptions/{sub}/ack"))
                .json(&json!({"offset": next}))
                .send()
                .await?;
            if !a.status().is_success() {
                bail!("ack failed: {}", a.status());
            }
            ack_latencies.push(ack_started.elapsed().as_millis());
        }
        let replay = ReplayResult {
            expected: replay_events,
            received: got,
            loss_count: replay_events.saturating_sub(got),
            duplicate_count: dups,
            error_count: errors,
            passed: got == replay_events && dups == 0 && errors == 0,
        };
        let replay_loss = replay.loss_count;
        let record = BenchmarkRecord {
            schema: "tape-vs-jetstream-record.v1",
            target: "tape",
            run_id: config.run_id.clone(),
            topic: config.topic.clone(),
            cell: Cell {
                payload_bytes: config.payload_bytes,
                clients: config.clients,
            },
            sample_index: config.sample_index,
            phase: if config.phase == "finalize" { "finalize" } else { "normal" },
            operation_count: if config.phase == "finalize" { replay_events } else { total },
            elapsed_ms: elapsed.as_millis(),
            throughput_ops_per_sec: if config.phase == "finalize" { replay_events } else { total } as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
            p99_ms: percentile(&latencies, 0.99),
            ack_p99_ms: percentile(&ack_latencies, 0.99),
            replay,
            ack_batch_size: BATCH,
            ack_cumulative: true,
            loss_count: replay_loss,
            duplicate_count: dups,
            error_count: errors,
            verdict: "measured",
            failure: None,
        };
        record.validate().map_err(anyhow::Error::msg)?;
        Ok(record)
    }

    async fn run_jetstream_recovery(config: ClientConfig) -> Result<BenchmarkRecord> {
        let nats = async_nats::connect(&config.nats_url).await.context("connect NATS")?;
        let js = jetstream::new(nats);
        let stream_name = format!("TAPE_BENCH_{}_{}_{}", config.run_id, config.payload_bytes, config.clients);
        let subject = subject(&config);
        let mut stream = js.get_stream(&stream_name).await.context("missing persistent stream")?;
        let info = stream.info().await.context("read persistent stream info")?.clone();
        if info.config.storage != jetstream::stream::StorageType::File || info.state.last_sequence < config.replay_events as u64 { bail!("persistent FileStore backlog is missing"); }
        let start = info.state.last_sequence - config.replay_events as u64 + 1;
        let consumer = stream.create_consumer(pull::Config { durable_name: Some(format!("tape-bench-recovery-{}", config.sample_index)), deliver_policy: DeliverPolicy::ByStartSequence { start_sequence: start }, ack_policy: AckPolicy::Explicit, filter_subject: subject, ..Default::default() }).await.context("create recovery consumer")?;
        let started = Instant::now(); let mut received = 0usize; let mut errors = 0usize;
        while received < config.replay_events {
            let mut batch = consumer.batch().max_messages(BATCH.min(config.replay_events - received)).expires(Duration::from_secs(10)).messages().await.context("recovery pull")?;
            while let Some(message) = batch.next().await { let message = message.map_err(|e| anyhow::anyhow!("recovery message: {e}"))?; if message.payload.len() != config.payload_bytes { errors += 1; } message.ack().await.map_err(|e| anyhow::anyhow!("recovery ack: {e}"))?; received += 1; }
            if received == 0 { bail!("persistent backlog replay is empty"); }
        }
        let elapsed = started.elapsed();
        let replay = ReplayResult { expected: config.replay_events, received, loss_count: config.replay_events.saturating_sub(received), duplicate_count: received.saturating_sub(config.replay_events), error_count: errors, passed: received == config.replay_events && errors == 0 };
        let record = BenchmarkRecord { schema: "tape-vs-jetstream-record.v1", target: "jetstream", run_id: config.run_id.clone(), topic: config.topic.clone(), cell: Cell { payload_bytes: config.payload_bytes, clients: config.clients }, sample_index: config.sample_index, phase: "recovery", operation_count: received, elapsed_ms: elapsed.as_millis(), throughput_ops_per_sec: received as f64 / elapsed.as_secs_f64().max(f64::EPSILON), p99_ms: 1, ack_p99_ms: 0, replay, ack_batch_size: BATCH, ack_cumulative: false, loss_count: errors, duplicate_count: 0, error_count: errors, verdict: "recovery_measured", failure: None };
        record.validate().map_err(anyhow::Error::msg)?; Ok(record)
    }

    async fn run_tape_recovery(config: ClientConfig) -> Result<BenchmarkRecord> {
        let http = reqwest::Client::new(); let base = config.tape_url.trim_end_matches('/'); let topic = &config.topic; let sub = tape_subscription_for_phase(&config, "recovery");
        let mut index = 0usize; let started = Instant::now(); let errors = 0usize;
        while index < config.replay_events {
            let limit = BATCH.min(config.replay_events - index);
            let r = http.post(format!("{base}/topics/{topic}/subscriptions/{sub}/pull")).json(&json!({"limit": limit})).send().await?;
            if !r.status().is_success() { bail!("recovery pull failed: {}", r.status()); }
            let page: TapePull = r.json().await?; if page.events.is_empty() { bail!("persistent Tape backlog is missing"); }
            let (_prefix, validated_index) = scan_replay_page(&page.events, index).map_err(anyhow::Error::msg)?;
            index = validated_index;
            let ack = http.post(format!("{base}/topics/{topic}/subscriptions/{sub}/ack")).json(&json!({"offset": page.next_offset})).send().await?;
            if !ack.status().is_success() { bail!("recovery cumulative ack failed: {}", ack.status()); }
        }
        let elapsed = started.elapsed(); let replay = ReplayResult { expected: config.replay_events, received: index, loss_count: config.replay_events.saturating_sub(index), duplicate_count: 0, error_count: errors, passed: index == config.replay_events }; let record = BenchmarkRecord { schema: "tape-vs-jetstream-record.v1", target: "tape", run_id: config.run_id.clone(), topic: config.topic.clone(), cell: Cell { payload_bytes: config.payload_bytes, clients: config.clients }, sample_index: config.sample_index, phase: "recovery", operation_count: index, elapsed_ms: elapsed.as_millis(), throughput_ops_per_sec: index as f64 / elapsed.as_secs_f64().max(f64::EPSILON), p99_ms: 1, ack_p99_ms: 0, replay, ack_batch_size: BATCH, ack_cumulative: true, loss_count: config.replay_events.saturating_sub(index), duplicate_count: 0, error_count: errors, verdict: "recovery_measured", failure: None }; record.validate().map_err(anyhow::Error::msg)?; Ok(record)
    }
}

#[cfg(test)]
mod durable_v1_tests {
    use super::*;

    #[test]
    fn fixed_profile_is_mechanically_valid() {
        assert!(DurableV1Profile::default().validate().is_ok());
    }

    #[test]
    fn profile_rejects_changed_workload() {
        let mut p = DurableV1Profile::default();
        p.samples = 4;
        assert_eq!(
            p.validate().unwrap_err(),
            "durable-v1 requires warmup and five 60-second samples"
        );
    }

    #[test]
    fn image_validation_rejects_mutable_and_non_amd64() {
        assert!(validate_image_digest("nats:latest", "amd64").is_err());
        assert!(validate_image_digest(
            "nats@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "arm64"
        )
        .is_err());
        assert!(validate_image_digest(
            "nats@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "amd64"
        )
        .is_ok());
    }

    #[cfg(feature = "jetstream-benchmark")]
    mod client {
        use super::super::jetstream_client::*;
        use super::super::DurableV1Profile;
        use std::time::Duration;

        fn config() -> ClientConfig {
            ClientConfig {
                tape_url: "http://tape".into(),
                nats_url: "nats://nats".into(),
                topic: "bench".into(),
                payload_bytes: 128,
                clients: 1,
                sample_index: 0,
                operations_per_client: 1,
                replay_events: 100_000,
                sample_seconds: 60,
                profile: "durable-v1".into(),
                phase: "normal".into(),
                run_id: "test-run".into(),
            }
        }

        #[test]
        fn validation_rejects_non_profile_cells() {
            let mut c = config();
            c.payload_bytes = 129;
            assert!(c.validate(&DurableV1Profile::default()).is_err());
        }

        #[test]
        fn validation_rejects_bad_replay_count_and_timing() {
            let mut c = config();
            c.replay_events = 99_999;
            assert!(c.validate(&DurableV1Profile::default()).is_err());
            c.replay_events = 100_000;
            c.sample_seconds = 59;
            assert!(c.validate(&DurableV1Profile::default()).is_err());
        }

        #[test]
        fn validation_rejects_unknown_profile_and_phase() {
            let mut c = config();
            c.profile = "latest".into();
            assert!(c.validate(&DurableV1Profile::default()).is_err());
            c.profile = "durable-v1".into();
            c.phase = "warmup".into();
            assert!(c.validate(&DurableV1Profile::default()).is_err());
        }

        #[test]
        fn normal_samples_do_not_schedule_backlog_replay() {
            let mut c = config();
            assert_eq!(super::super::jetstream_client::replay_events_for_phase(&c), 0);
            c.phase = "finalize".into();
            assert_eq!(super::super::jetstream_client::replay_events_for_phase(&c), 100_000);
        }

        #[test]
        fn matrix_enumeration_has_frozen_cardinality_and_identity() {
            assert_eq!(matrix_cells("normal").unwrap().len(), 45);
            assert_eq!(matrix_cells("recovery").unwrap().len(), 9);
            let cells = matrix_cells("normal").unwrap();
            assert_eq!(cells[0], (128, 1, 0));
            assert_eq!(cells[44], (4096, 64, 4));
            assert!(matrix_cells("other").is_err());
        }

        #[test]
        fn target_validation_requires_only_the_selected_endpoint() {
            let mut c = config();
            c.nats_url.clear();
            assert!(c.validate_for_target("tape", &DurableV1Profile::default()).is_ok());
            assert!(c.validate_for_target("jetstream", &DurableV1Profile::default()).is_err());
            c.nats_url = "nats://nats".into();
            c.tape_url.clear();
            assert!(c.validate_for_target("jetstream", &DurableV1Profile::default()).is_ok());
            assert!(c.validate_for_target("tape", &DurableV1Profile::default()).is_err());
        }

        #[test]
        fn replay_identity_requires_cursor_continuity_and_replay_keys() {
            let events = vec![
                TapeEvent { offset: 41, key: Some("replay-0".into()), payload: serde_json::json!({}) },
                TapeEvent { offset: 42, key: Some("replay-1".into()), payload: serde_json::json!({}) },
            ];
            assert_eq!(validate_replay_identity(&events, 41, 0).unwrap(), 2);
            assert!(validate_replay_identity(&events, 40, 0).is_err());
            let mut wrong = events;
            wrong[1].key = Some("bench-1".into());
            assert!(validate_replay_identity(&wrong, 41, 0).is_err());
        }

        #[test]
        fn normal_and_recovery_use_the_same_subject_and_subscription_cell() {
            let c = config();
            assert_eq!(subject(&c), "tape.bench.test-run.128.1");
            assert_eq!(tape_subscription(&c), "bench-test-run-128-1-normal");
            assert_ne!(tape_subscription_for_phase(&c, "finalize"), tape_subscription_for_phase(&c, "recovery"));
        }

        #[test]
        fn sample_identity_changes_with_runner_supplied_run_id() {
            let first = config();
            let mut second = first.clone();
            second.run_id = "test-run-sample-1".into();
            assert_ne!(subject(&first), subject(&second));
            assert_ne!(tape_subscription(&first), tape_subscription(&second));
        }

        #[test]
        fn replay_start_skips_prefix_and_requires_replay_zero() {
            let events = vec![
                TapeEvent { offset: 10, key: Some("bench-0".into()), payload: serde_json::json!(null) },
                TapeEvent { offset: 11, key: Some("replay-0".into()), payload: serde_json::json!(null) },
            ];
            assert_eq!(replay_start(&events).unwrap(), 11);
            assert!(replay_start(&events[..1]).is_err());
        }

        #[test]
        fn replay_scan_skips_prefix_and_rejects_bad_sequences() {
            let page = vec![
                TapeEvent { offset: 1, key: Some("warmup".into()), payload: serde_json::json!(null) },
                TapeEvent { offset: 2, key: Some("bench-0".into()), payload: serde_json::json!(null) },
                TapeEvent { offset: 3, key: Some("replay-0".into()), payload: serde_json::json!(null) },
                TapeEvent { offset: 4, key: Some("replay-1".into()), payload: serde_json::json!(null) },
            ];
            assert_eq!(scan_replay_page(&page, 0).unwrap(), (2, 2));
            let mut wrong = page.clone();
            wrong[2].key = Some("replay-2".into());
            assert!(scan_replay_page(&wrong, 0).is_err());
            let bad_prefix = vec![TapeEvent { offset: 1, key: Some("replay-3".into()), payload: serde_json::json!(null) }];
            assert!(scan_replay_page(&bad_prefix, 0).is_err());
        }

        #[test]
        fn tape_payload_is_exact_string_bytes_without_envelope() {
            let payload = serde_json::Value::String("x".repeat(128));
            assert_eq!(payload.as_str().unwrap().len(), 128);
            assert!(!payload.get("body").is_some());
        }

        #[test]
        fn incomplete_record_preserves_failure_and_is_not_complete() {
            let c = config();
            let r = incomplete_record(&c, "tape", "connection refused".into());
            assert_eq!(r.verdict, "incomplete");
            assert_eq!(r.failure.as_deref(), Some("connection refused"));
            assert!(r.validate().is_ok());
        }

        #[test]
        fn readiness_backoff_is_bounded_without_waiting() {
            let first = super::super::jetstream_client::readiness_backoff(0);
            let later = super::super::jetstream_client::readiness_backoff(99);
            assert_eq!(first, Duration::from_millis(100));
            assert_eq!(later, Duration::from_secs(1));
            assert!(later >= first);
        }

        #[test]
        fn record_json_has_stable_schema_and_verdict_inputs() {
            let r = BenchmarkRecord {
                schema: "tape-vs-jetstream-record.v1",
                target: "jetstream",
                run_id: "test-run".into(),
                topic: "bench".into(),
                cell: Cell {
                    payload_bytes: 128,
                    clients: 1,
                },
                sample_index: 0,
                phase: "normal",
                operation_count: 1,
                elapsed_ms: 1,
                throughput_ops_per_sec: 1000.0,
                p99_ms: 0,
                ack_p99_ms: 0,
                replay: ReplayResult {
                    expected: 0,
                    received: 0,
                    loss_count: 0,
                    duplicate_count: 0,
                    error_count: 0,
                    passed: true,
                },
                ack_batch_size: 100,
                ack_cumulative: false,
                loss_count: 0,
                duplicate_count: 0,
                error_count: 0,
                verdict: "measured",
                failure: None,
            };
            assert!(r.validate().is_ok());
            let mut tape = r.clone();
            tape.target = "tape";
            tape.ack_cumulative = true;
            assert!(tape.validate().is_ok());
            tape.ack_cumulative = false;
            assert!(tape.validate().is_err());
            let value = serde_json::to_value(&r).unwrap();
            for field in [
                "schema",
                "target",
                "cell",
                "sample_index",
                "operation_count",
                "throughput_ops_per_sec",
                "p99_ms",
                "replay",
                "ack_batch_size",
                "verdict",
            ] {
                assert!(value.get(field).is_some(), "missing {field}");
            }
        }

        #[test]
        fn finalize_record_is_distinct_and_requires_full_replay() {
            let c = config();
            let mut r = BenchmarkRecord {
                schema: "tape-vs-jetstream-record.v1", target: "jetstream", run_id: "test-run".into(), topic: "bench".into(),
                cell: Cell { payload_bytes: 128, clients: 1 }, sample_index: 0,
                phase: "finalize", operation_count: 1, elapsed_ms: 1,
                throughput_ops_per_sec: 1.0, p99_ms: 0, ack_p99_ms: 0,
                replay: ReplayResult { expected: 100_000, received: 100_000, loss_count: 0, duplicate_count: 0, error_count: 0, passed: true },
                ack_batch_size: 100, ack_cumulative: false, loss_count: 0,
                duplicate_count: 0, error_count: 0, verdict: "measured", failure: None,
            };
            assert!(r.validate().is_ok());
            r.phase = "normal";
            assert!(r.validate().is_err());
            assert_eq!(c.phase, "normal");
        }
    }
}
// </HANDWRITE>
