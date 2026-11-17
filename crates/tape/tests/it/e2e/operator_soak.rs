//! Suite `operator_soak`: the cluster form of `scripts/soak.sh`. The Tape CR
//! `tape` of k8s/overlays/gke-test runs one pod `tape-0`. The suite appends a
//! fixed set of 64 events, then replays them for the soak window, and fails
//! when an acked append is not read back or when the memory of the server
//! container grows by `RSS_GROWTH_PCT` or more over the steady window. The
//! window comes from `E2E_SOAK_HOURS` (0.05 to 5). Memory is the working set
//! that the metrics API reports for the server container: the server image
//! is distroless, so the suite runs no command in the pod.

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

    // Seed once. The steady window never appends: the stream stays bounded,
    // as in scripts/soak.sh.
    let ids: Vec<String> = (1..=EVENTS).map(|i| format!("s-{i:04}")).collect();
    let want: Vec<&str> = ids.iter().map(String::as_str).collect();
    for id in &want {
        append(&http, &base, TOPIC, id).await;
    }
    evidence(SUITE, "seeded", json!({ "events": EVENTS }));
    assert_replay(&http, &base, TOPIC, &want).await;

    // Warm-up: a fifth of the window, replaying, before the first sample.
    let seeded = Instant::now();
    let warmup = window / 5;
    let mut rounds: u64 = 0;
    replay_until(&http, &base, &want, seeded + warmup, &mut rounds).await;
    let start_kib = sample_memory_kib();
    evidence(
        SUITE,
        "rss start",
        json!({ "rss_kib": start_kib, "pod": POD }),
    );

    // Steady window: replay every acked id until the window has passed.
    let steady_until = Instant::now() + (window - warmup);
    replay_until(&http, &base, &want, steady_until, &mut rounds).await;

    let end_kib = sample_memory_kib();
    let growth = rss_growth_pct(start_kib, end_kib);
    evidence(
        SUITE,
        "rss end",
        json!({
            "rss_kib": end_kib,
            "growth_pct": growth,
            "bound_pct": RSS_GROWTH_PCT,
            "rounds": rounds
        }),
    );
    assert_replay(&http, &base, TOPIC, &want).await;
    assert!(
        growth < RSS_GROWTH_PCT,
        "RSS grew {growth:.1}% ({start_kib} KiB to {end_kib} KiB), bound {RSS_GROWTH_PCT}%"
    );
    evidence(SUITE, "pass", json!({}));
}

/// Replays every id in `want` in rounds of one check each, with a pause
/// between rounds, until `until` has passed. A failed replay panics, so a lost
/// acked append fails the test.
async fn replay_until(
    http: &reqwest::Client,
    base: &str,
    want: &[&str],
    until: Instant,
    rounds: &mut u64,
) {
    while Instant::now() < until {
        assert_replay(http, base, TOPIC, want).await;
        *rounds += 1;
        tokio::time::sleep(ROUND_PAUSE).await;
    }
}

/// Parses a Kubernetes memory quantity into KiB. `Ki`, `Mi` and `Gi` are
/// binary units; a number without a suffix is bytes. Anything else is `None`.
fn parse_memory_kib(quantity: &str) -> Option<u64> {
    let quantity = quantity.trim();
    for (suffix, factor) in [("Ki", 1), ("Mi", 1024), ("Gi", 1024 * 1024)] {
        if let Some(number) = quantity.strip_suffix(suffix) {
            return number.parse::<u64>().ok()?.checked_mul(factor);
        }
    }
    quantity.parse::<u64>().ok().map(|bytes| bytes / 1024)
}

/// Growth of the sample `end_kib` over `start_kib`, in percent of the start.
fn rss_growth_pct(start_kib: u64, end_kib: u64) -> f64 {
    assert!(start_kib > 0, "the start RSS sample is 0 KiB");
    (end_kib as f64 - start_kib as f64) * 100.0 / start_kib as f64
}

/// The soak window for `hours` (E2E_SOAK_HOURS). Panics unless `hours` is a
/// number from 0.05 to 5.
fn soak_window(hours: &str) -> Duration {
    let value: f64 = hours.trim().parse().unwrap_or(f64::NAN);
    assert!(
        (0.05..=5.0).contains(&value),
        "E2E_SOAK_HOURS must be a number from 0.05 to 5, got {hours}"
    );
    Duration::from_secs_f64(value * 3600.0)
}

/// The working-set memory of the server container of `POD`, in KiB, from the
/// metrics API (`metrics.k8s.io`) of the run's namespace.
fn sample_memory_kib() -> u64 {
    let namespace = required_env("NAMESPACE");
    let path = format!("/apis/metrics.k8s.io/v1beta1/namespaces/{namespace}/pods/{POD}");
    let text = kubectl(&["get", "--raw", path.as_str()]);
    let pod: Value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("metrics for {POD} are not JSON: {err}: {text}"));
    let quantity = pod["containers"]
        .as_array()
        .and_then(|containers| {
            containers
                .iter()
                .find(|c| c["name"].as_str() == Some(CONTAINER))
        })
        .and_then(|c| c["usage"]["memory"].as_str())
        .unwrap_or_else(|| panic!("no memory usage for container {CONTAINER} in {text}"));
    parse_memory_kib(quantity)
        .unwrap_or_else(|| panic!("cannot read memory quantity {quantity:?} in {text}"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{parse_memory_kib, rss_growth_pct, soak_window};

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
        assert!((rss_growth_pct(1000, 900) + 10.0).abs() < 1e-9);
    }

    #[test]
    fn soak_window_accepts_the_range_and_refuses_outside_it() {
        assert_eq!(soak_window("0.05"), Duration::from_secs(180));
        assert_eq!(soak_window("4"), Duration::from_secs(14400));
        assert!(std::panic::catch_unwind(|| soak_window("5.1")).is_err());
        assert!(std::panic::catch_unwind(|| soak_window("0")).is_err());
        assert!(std::panic::catch_unwind(|| soak_window("abc")).is_err());
    }
}
