//! Suite `operator_soak`: the cluster form of `scripts/soak.sh`. The Tape
//! group of k8s/overlays/gke-test appends a fixed set of events, then replays
//! them for the soak window (`E2E_SOAK_HOURS`, from 0.05 to 5 hours). The suite
//! passes only when every acked append is read back and the memory of the
//! server container grows by less than `RSS_GROWTH_PCT` between the start of
//! the steady window and its end. Memory is the working set that the metrics
//! API reports for the container; the server image has no shell to read `/proc`.
//! The bound is `TAPE_SOAK_RSS_GROWTH_PCT` of `scripts/soak.sh` (10 percent).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{append, assert_replay, endpoint, evidence, kubectl, required_env, wait_healthy};

const SUITE: &str = "operator_soak";
const POD: &str = "tape-0";
const CONTAINER: &str = "server";
const TOPIC: &str = "soak-orders";
const EVENTS: usize = 64;
// Same value as TAPE_SOAK_RSS_GROWTH_PCT in scripts/soak.sh.
const RSS_GROWTH_PCT: f64 = 10.0;
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ROUND_PAUSE: Duration = Duration::from_secs(5);

/// A Kubernetes memory quantity as KiB: `Ki`, `Mi` or `Gi`, or plain bytes.
fn parse_memory_kib(quantity: &str) -> Option<u64> {
    let quantity = quantity.trim();
    if let Some(number) = quantity.strip_suffix("Ki") {
        return number.parse::<u64>().ok();
    }
    if let Some(number) = quantity.strip_suffix("Mi") {
        return number.parse::<u64>().ok().map(|mib| mib * 1024);
    }
    if let Some(number) = quantity.strip_suffix("Gi") {
        return number.parse::<u64>().ok().map(|gib| gib * 1024 * 1024);
    }
    quantity.parse::<u64>().ok().map(|bytes| bytes / 1024)
}

/// Growth of the memory from the start sample to the end sample, in percent.
fn rss_growth_pct(start_kib: u64, end_kib: u64) -> f64 {
    assert!(
        start_kib > 0,
        "start memory is 0 KiB; the sample is not a memory reading"
    );
    (end_kib as f64 - start_kib as f64) * 100.0 / start_kib as f64
}

/// The soak window from `E2E_SOAK_HOURS`; a value outside 0.05 to 5 fails.
fn soak_window(hours: &str) -> Duration {
    let value: f64 = hours.trim().parse().unwrap_or(f64::NAN);
    assert!(
        (0.05..=5.0).contains(&value),
        "E2E_SOAK_HOURS must be a number from 0.05 to 5, got {hours}"
    );
    Duration::from_secs_f64(value * 3600.0)
}

/// The memory of the server container in pod `tape-0`, in KiB, from the
/// metrics API (`metrics.k8s.io`) of the run's namespace.
fn sample_memory_kib() -> u64 {
    let ns = required_env("NAMESPACE");
    let raw = kubectl(&[
        "get",
        "--raw",
        &format!("/apis/metrics.k8s.io/v1beta1/namespaces/{ns}/pods/{POD}"),
    ]);
    let metrics: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("pod metrics are not JSON ({e}): {raw}"));
    let quantity = metrics["containers"]
        .as_array()
        .and_then(|containers| containers.iter().find(|c| c["name"] == CONTAINER))
        .and_then(|c| c["usage"]["memory"].as_str())
        .unwrap_or_else(|| panic!("no memory usage for {CONTAINER} in {raw}"));
    parse_memory_kib(quantity).unwrap_or_else(|| panic!("unreadable memory quantity {quantity}"))
}

/// One replay of every acked id, then a pause; a lost append panics here.
async fn replay_round(http: &reqwest::Client, base: &str, want: &[&str], rounds: &mut u64) {
    assert_replay(http, base, TOPIC, want).await;
    *rounds += 1;
    tokio::time::sleep(ROUND_PAUSE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn operator_soak_has_no_loss_and_bounded_rss() {
    let base = endpoint();
    let window = soak_window(&required_env("E2E_SOAK_HOURS"));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    evidence(SUITE, "server healthy", json!({}));

    let ids: Vec<String> = (1..=EVENTS).map(|i| format!("s-{i:04}")).collect();
    let want: Vec<&str> = ids.iter().map(String::as_str).collect();
    for id in &ids {
        append(&http, &base, TOPIC, id).await;
    }
    evidence(SUITE, "seeded", json!({ "events": EVENTS }));
    assert_replay(&http, &base, TOPIC, &want).await;

    // Warm-up: the first fifth of the window, not measured.
    let mut rounds: u64 = 0;
    let warmup = window / 5;
    let seeded_at = Instant::now();
    while seeded_at.elapsed() < warmup {
        replay_round(&http, &base, &want, &mut rounds).await;
    }

    let start_kib = sample_memory_kib();
    evidence(
        SUITE,
        "rss start",
        json!({ "rss_kib": start_kib, "pod": POD }),
    );

    // Steady window: the rest of the window, replaying the bounded stream.
    let steady = window - warmup;
    let steady_at = Instant::now();
    while steady_at.elapsed() < steady {
        replay_round(&http, &base, &want, &mut rounds).await;
    }

    let end_kib = sample_memory_kib();
    let growth = rss_growth_pct(start_kib, end_kib);
    evidence(
        SUITE,
        "rss end",
        json!({
            "rss_kib": end_kib,
            "growth_pct": growth,
            "bound_pct": RSS_GROWTH_PCT,
            "rounds": rounds,
        }),
    );

    assert_replay(&http, &base, TOPIC, &want).await;
    assert!(
        growth < RSS_GROWTH_PCT,
        "RSS grew {growth:.1}% ({start_kib} KiB to {end_kib} KiB), bound {RSS_GROWTH_PCT}%"
    );
    evidence(SUITE, "pass", json!({}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_kib_reads_the_metrics_quantity() {
        assert_eq!(parse_memory_kib("34716Ki"), Some(34716));
        assert_eq!(parse_memory_kib("34Mi"), Some(34816));
        assert_eq!(parse_memory_kib("1Gi"), Some(1048576));
        assert_eq!(parse_memory_kib("2048"), Some(2));
        assert_eq!(parse_memory_kib("abc"), None);
        assert_eq!(parse_memory_kib("12Xi"), None);
    }

    #[test]
    fn rss_growth_pct_is_relative_to_the_start() {
        assert!((rss_growth_pct(1000, 1100) - 10.0).abs() < 1e-9);
        assert!((rss_growth_pct(1000, 900) - (-10.0)).abs() < 1e-9);
    }

    #[test]
    fn soak_window_accepts_the_range_and_refuses_outside_it() {
        assert_eq!(soak_window("0.05"), Duration::from_secs(180));
        assert_eq!(soak_window("4"), Duration::from_secs(14400));
        for bad in ["5.1", "0", "abc"] {
            assert!(
                std::panic::catch_unwind(|| soak_window(bad)).is_err(),
                "{bad} must be refused"
            );
        }
    }
}
