//! Unit tests for [`super::TapeMetrics`].

use super::*;

#[test]
fn routes_map_to_op_families() {
    let m = TapeMetrics::new();
    m.observe_method("POST", "/topics/{topic}/append", 3);
    m.observe_method("GET", "/topics/{topic}/replay", 2);
    m.observe_method("GET", "/topics/{topic}/replay/stream", 4);
    m.observe_method("GET", "/topics/{topic}/consumers/{consumer}/checkpoint", 1);
    m.observe_method("PUT", "/topics/{topic}/consumers/{consumer}/checkpoint", 4);
    assert_eq!(m.append.count.get(), 1);
    assert_eq!(m.append.sum.get(), 3);
    assert_eq!(m.replay.count.get(), 2);
    assert_eq!(m.replay.sum.get(), 6);
    assert_eq!(m.checkpoint_get.count.get(), 1);
    assert_eq!(m.checkpoint_put.count.get(), 1);
    assert_eq!(m.checkpoint_put.sum.get(), 4);
    assert_eq!(m.other.count.get(), 0);
}

#[test]
fn render_exposes_tape_request_counters() {
    let m = TapeMetrics::new();
    m.observe_method("POST", "/topics/{topic}/append", 7);
    let out = m.render();
    assert!(out.contains("# TYPE tape_append_requests_total counter"));
    assert!(out.contains("tape_append_requests_total 1"));
    assert!(out.contains("tape_append_latency_ms_sum 7"));
    assert!(out.contains("tape_replay_requests_total 0"));
}

/// #2573 — the degraded gauge is sticky across `mark`, clears only on
/// `clear`, and the error counter is monotonic across a full flap cycle.
/// The counter surviving `clear_storage_degraded` is the point: a node
/// that fills up and re-probes clean every 30s looks healthy in the gauge
/// at every scrape, and only the counter shows it.
#[test]
fn storage_degraded_is_sticky_and_the_error_counter_is_monotonic() {
    let m = TapeMetrics::new();
    assert!(!m.is_storage_degraded());
    assert_eq!(m.storage_full_errors_total.get(), 0);

    m.mark_storage_degraded();
    assert!(m.is_storage_degraded());
    assert_eq!(m.storage_full_errors_total.get(), 1);

    // Sticky: a second hit while already degraded stays degraded and
    // counts again.
    m.mark_storage_degraded();
    assert!(m.is_storage_degraded());
    assert_eq!(m.storage_full_errors_total.get(), 2);

    m.clear_storage_degraded();
    assert!(!m.is_storage_degraded());
    assert_eq!(
        m.storage_full_errors_total.get(),
        2,
        "clearing the gauge must not rewrite history: the node WAS full twice"
    );
}

/// #2573 — both series are exposed so the `TapeStorageDegraded` alert and
/// the flap counter have something to select on.
#[test]
fn render_exposes_the_storage_degraded_series() {
    let m = TapeMetrics::new();
    let clean = m.render();
    assert!(clean.contains("# TYPE tape_storage_degraded gauge"));
    assert!(clean.contains("tape_storage_degraded 0"));
    assert!(clean.contains("# TYPE tape_storage_full_errors_total counter"));
    assert!(clean.contains("tape_storage_full_errors_total 0"));

    m.mark_storage_degraded();
    let degraded = m.render();
    assert!(degraded.contains("tape_storage_degraded 1"));
    assert!(degraded.contains("tape_storage_full_errors_total 1"));
}

/// #2580 — the alert reads these series, so pin that they render; an
/// alert-file test alone proves only that the alert mentions them.
#[test]
fn render_exposes_auth_registry_reload_series() {
    let m = TapeMetrics::new();
    let clean = m.render();
    assert!(clean.contains("# TYPE tape_auth_registry_reload_failures_total counter"));
    assert!(clean.contains("tape_auth_registry_reload_failures_total 0"));
    assert!(clean.contains("# TYPE tape_auth_registry_reload_success_unixtime gauge"));
    assert!(clean.contains("tape_auth_registry_reload_success_unixtime 0"));

    m.record_auth_registry_reload_failure();
    let after_fail = m.render();
    assert!(after_fail.contains("tape_auth_registry_reload_failures_total 1"));

    m.record_auth_registry_reload_success();
    let after_success = m.render();
    let line = after_success
        .lines()
        .find(|l| l.starts_with("tape_auth_registry_reload_success_unixtime "))
        .expect("the success gauge must render as its own sample line");
    let value: u64 = line
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .expect("the gauge sample must carry a parseable value");
    assert!(value > 0);
}

/// #2579 — a process that never entered the raft branch publishes no
/// leader-known series, so `TapeRaftLeaderAbsent` has nothing to fire on.
#[test]
fn fresh_metrics_render_omits_the_raft_leader_known_series() {
    let m = TapeMetrics::new();
    let out = m.render();
    assert!(!out.contains("tape_raft_leader_known"));
    assert!(!out.contains("# TYPE tape_raft_leader_known gauge"));
}

/// #2579 — once the poller publishes, both readings render, including the
/// `0` the alert fires on.
#[test]
fn raft_leader_known_gauge_tracks_known_and_unknown_states() {
    let m = TapeMetrics::new();

    m.set_raft_leader_known(true);
    let known = m.render();
    assert!(known.contains("# TYPE tape_raft_leader_known gauge"));
    assert!(known.contains("tape_raft_leader_known 1"));
    assert_eq!(m.get_raft_leader_known(), 1);

    m.set_raft_leader_known(false);
    let unknown = m.render();
    assert!(unknown.contains("# TYPE tape_raft_leader_known gauge"));
    assert!(unknown.contains("tape_raft_leader_known 0"));
    assert_eq!(m.get_raft_leader_known(), 0);
}
