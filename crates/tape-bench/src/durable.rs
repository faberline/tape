//! WI #3052 AC1: durable append throughput over real HTTP against the WAL
//! group-commit path, sampled across connection counts.

use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Value};

use crate::report::payload;
use tape::http::{router, AppState};
use tape_storage::wal::{CommitCoordinator, WalStore};

/// Data-plane request body size limit for [`run_durable_benchmark`]'s
/// `AppState` -- arbitrary but generous headroom over the small bench
/// payloads this drives, matching the limit other real-HTTP tape tests use.
const DURABLE_BENCH_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

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
/// count, driven over real HTTP against the real [`CommitCoordinator`]
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
