//! tape request observability: per-op request counts + latency, the ENOSPC
//! degraded-mode series, the raft leader-known gauge, and the auth registry
//! reload series, rendered into the Prometheus exposition served at
//! `/metrics`.
//!
//! Built on the shared `metrics-prometheus` primitives (`Latency` — a
//! lock-free `sum`/`count` counter pair — plus the text-format encoder
//! [`metrics_prometheus::render`]); this module owns only tape's metric names
//! and the route -> op mapping. Recording happens in the HTTP interface's
//! `track` middleware over the `/topics` data plane, so the journal hot path
//! never carries a metrics probe.

use std::time::{SystemTime, UNIX_EPOCH};

use metrics_prometheus::{Counter, Gauge, Latency, Sample};
use tape_access::RegistryReloadRecorder;

/// #2579: the `raft_leader_known` value meaning "no raft poll has published a
/// result" — this process is not in raft mode, or the poller has not run yet.
/// [`TapeMetrics::render`] omits the series while it holds this value.
const NOT_RAFT: u64 = u64::MAX;

/// Per-op request metrics for the tape data plane. One [`Latency`]
/// (count + latency-ms sum) per op family; `count` doubles as the request
/// counter the `tape_<op>_requests_total` sample exposes.
#[derive(Debug)]
pub struct TapeMetrics {
    pub append: Latency,
    pub replay: Latency,
    pub checkpoint_get: Latency,
    pub checkpoint_put: Latency,
    /// Everything else on the data plane.
    pub other: Latency,
    /// #2573: `1` while this node is in ENOSPC degraded read-only mode (the
    /// journal persist path hit `io::ErrorKind::StorageFull`), `0` otherwise.
    /// Sticky: stays `1` until the periodic re-probe
    /// (`TAPE_STORAGE_FULL_REPROBE_SECS`, see `crates/tape/src/bin/tape/serve.rs`) confirms
    /// the store directory accepts a write again, or the process restarts.
    /// [`super::JournalService::storage_writable`] reads this to fast-fail
    /// mutations without touching the durable path;
    /// `operator::render::prometheus_rule`'s `TapeStorageDegraded` alert reads
    /// the published `tape_storage_degraded` series.
    pub storage_degraded: Gauge,
    /// #2573: total genuine ENOSPC hits observed on the journal persist path,
    /// monotonic across the process lifetime (never reset when
    /// `storage_degraded` clears) — see [`TapeMetrics::mark_storage_degraded`].
    /// A node that flaps in and out of degraded mode is invisible in the gauge
    /// alone; this counter is what makes that visible.
    pub storage_full_errors_total: Counter,
    /// #2579: `NOT_RAFT` means no raft poll has published a result yet; `0`
    /// means the node is in raft mode and the last poll found no elected
    /// leader; `1` means the node is in raft mode and the last poll found a
    /// leader (this node or a peer).
    pub raft_leader_known: Gauge,
    /// #2580: total auth registry reload failures observed while the last
    /// known-good registry kept serving.
    pub auth_registry_reload_failures_total: Counter,
    /// #2580: unix time in seconds of the last successful auth registry
    /// reload.
    pub auth_registry_reload_success_unixtime: Gauge,
}

/// The gauge starts at the sentinel value, not at `0`.
///
/// A derived `Default` would leave the gauge at `0`, and `0` is the value
/// `TapeRaftLeaderAbsent` fires on — so a default constructed from a process
/// that never entered the raft branch would look like a leaderless raft group.
/// The sentinel must be set by every construction route, which is why the
/// derive was dropped rather than the sentinel being set in `new()` alone.
impl Default for TapeMetrics {
    fn default() -> Self {
        let metrics = Self {
            append: Default::default(),
            replay: Default::default(),
            checkpoint_get: Default::default(),
            checkpoint_put: Default::default(),
            other: Default::default(),
            storage_degraded: Default::default(),
            storage_full_errors_total: Default::default(),
            raft_leader_known: Default::default(),
            auth_registry_reload_failures_total: Default::default(),
            auth_registry_reload_success_unixtime: Default::default(),
        };
        metrics.raft_leader_known.set(NOT_RAFT);
        metrics
    }
}

impl TapeMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// #2573: flip into ENOSPC degraded read-only mode and count the hit.
    ///
    /// Called by [`super::JournalService::apply_mutation`] — in front of every
    /// single-node durable write path — when and only when the failure is an
    /// ENOSPC or EIO durability failure. Any other failure stays a plain
    /// `500`: retrying a transient I/O error can succeed, retrying
    /// against a full disk cannot, and conflating the two is what makes
    /// clients back off against a condition backoff never clears.
    pub fn mark_storage_degraded(&self) {
        self.storage_degraded.set(1);
        self.storage_full_errors_total.incr();
    }

    /// #2573: leave degraded read-only mode after a re-probe write succeeded.
    /// Deliberately does NOT touch `storage_full_errors_total` — the counter
    /// records that the node was once full, which stays true.
    pub fn clear_storage_degraded(&self) {
        self.storage_degraded.set(0);
    }

    /// #2573: `true` while this node is in ENOSPC degraded read-only mode.
    pub fn is_storage_degraded(&self) -> bool {
        self.storage_degraded.get() == 1
    }

    /// #2579: publish the latest raft-leader observation for scrape-time
    /// rendering.
    pub fn set_raft_leader_known(&self, known: bool) {
        self.raft_leader_known.set(if known { 1 } else { 0 });
    }

    /// #2580: record a successful auth registry reload by publishing the
    /// current unix-second timestamp.
    pub fn record_auth_registry_reload_success(&self) {
        self.auth_registry_reload_success_unixtime.set(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or_default(),
        );
    }

    /// #2580: record a failed auth registry reload while continuing to serve
    /// the previously loaded registry.
    pub fn record_auth_registry_reload_failure(&self) {
        self.auth_registry_reload_failures_total.incr();
    }

    /// #2579: read the latest raft-leader observation published by the poller.
    pub fn get_raft_leader_known(&self) -> u64 {
        self.raft_leader_known.get()
    }

    /// Map a matched axum route pattern to its op family. Unknown routes
    /// land in `other`.
    fn op(&self, route: &str) -> &Latency {
        if route.ends_with("/append") {
            &self.append
        } else if route.ends_with("/replay") || route.ends_with("/replay/stream") {
            &self.replay
        } else if route.ends_with("/checkpoint") {
            // GET vs PUT collapse to the same route pattern; `track` picks
            // the concrete field based on the request method instead.
            &self.other
        } else {
            &self.other
        }
    }

    /// Record one request against the route's op family, disambiguating the
    /// shared `/checkpoint` route pattern by HTTP method.
    pub fn observe_method(&self, method: &str, route: &str, latency_ms: u64) {
        if route.ends_with("/checkpoint") {
            match method {
                "GET" => self.checkpoint_get.observe(latency_ms),
                "PUT" => self.checkpoint_put.observe(latency_ms),
                _ => self.other.observe(latency_ms),
            }
        } else {
            self.op(route).observe(latency_ms);
        }
    }

    /// Render the Prometheus text exposition (0.0.4) for the recorded
    /// request metrics, through the shared `metrics_prometheus` encoder.
    pub fn render(&self) -> String {
        let mut samples = vec![
            Sample::new(
                "tape_append_requests_total",
                "counter",
                "Total append requests.",
                self.append.count.get(),
            ),
            Sample::new(
                "tape_append_latency_ms_sum",
                "counter",
                "Sum of append request latencies in milliseconds.",
                self.append.sum.get(),
            ),
            Sample::new(
                "tape_append_latency_ms_count",
                "counter",
                "Count of append latency observations.",
                self.append.count.get(),
            ),
            Sample::new(
                "tape_replay_requests_total",
                "counter",
                "Total replay requests.",
                self.replay.count.get(),
            ),
            Sample::new(
                "tape_replay_latency_ms_sum",
                "counter",
                "Sum of replay request latencies in milliseconds.",
                self.replay.sum.get(),
            ),
            Sample::new(
                "tape_replay_latency_ms_count",
                "counter",
                "Count of replay latency observations.",
                self.replay.count.get(),
            ),
            Sample::new(
                "tape_checkpoint_get_requests_total",
                "counter",
                "Total checkpoint read requests.",
                self.checkpoint_get.count.get(),
            ),
            Sample::new(
                "tape_checkpoint_get_latency_ms_sum",
                "counter",
                "Sum of checkpoint read request latencies in milliseconds.",
                self.checkpoint_get.sum.get(),
            ),
            Sample::new(
                "tape_checkpoint_get_latency_ms_count",
                "counter",
                "Count of checkpoint read latency observations.",
                self.checkpoint_get.count.get(),
            ),
            Sample::new(
                "tape_checkpoint_put_requests_total",
                "counter",
                "Total checkpoint advance requests.",
                self.checkpoint_put.count.get(),
            ),
            Sample::new(
                "tape_checkpoint_put_latency_ms_sum",
                "counter",
                "Sum of checkpoint advance request latencies in milliseconds.",
                self.checkpoint_put.sum.get(),
            ),
            Sample::new(
                "tape_checkpoint_put_latency_ms_count",
                "counter",
                "Count of checkpoint advance latency observations.",
                self.checkpoint_put.count.get(),
            ),
            Sample::new(
                "tape_other_requests_total",
                "counter",
                "Total other data-plane requests.",
                self.other.count.get(),
            ),
            Sample::new(
                "tape_other_latency_ms_sum",
                "counter",
                "Sum of other data-plane request latencies in milliseconds.",
                self.other.sum.get(),
            ),
            Sample::new(
                "tape_other_latency_ms_count",
                "counter",
                "Count of other data-plane latency observations.",
                self.other.count.get(),
            ),
            Sample::new(
                "tape_storage_degraded",
                "gauge",
                "1 while this node is in ENOSPC degraded read-only mode (the journal persist \
                 path hit disk-full); mutating requests are fast-failed with 507.",
                self.storage_degraded.get(),
            ),
            Sample::new(
                "tape_storage_full_errors_total",
                "counter",
                "Total genuine ENOSPC hits observed on the journal persist path.",
                self.storage_full_errors_total.get(),
            ),
            Sample::new(
                "tape_auth_registry_reload_failures_total",
                "counter",
                "Total auth registry reload failures observed while serving the last known-good \
                 registry.",
                self.auth_registry_reload_failures_total.get(),
            ),
            Sample::new(
                "tape_auth_registry_reload_success_unixtime",
                "gauge",
                "Unix time in seconds of the last successful auth registry reload.",
                self.auth_registry_reload_success_unixtime.get(),
            ),
        ];
        if self.raft_leader_known.get() != NOT_RAFT {
            samples.push(Sample::new(
                "tape_raft_leader_known",
                "gauge",
                "1 while the last raft poll saw a leader; 0 while the last raft poll found no \
                 elected leader.",
                self.raft_leader_known.get(),
            ));
        }
        metrics_prometheus::render(&samples)
    }
}

/// #2580: the auth verifier's reload outcomes land on the same `TapeMetrics`
/// `/metrics` renders — `AppState::with_auth` hands the verifier
/// `state.metrics()`.
impl RegistryReloadRecorder for TapeMetrics {
    fn reload_succeeded(&self) {
        self.record_auth_registry_reload_success();
    }

    fn reload_failed(&self) {
        self.record_auth_registry_reload_failure();
    }
}

#[cfg(test)]
mod tests;
