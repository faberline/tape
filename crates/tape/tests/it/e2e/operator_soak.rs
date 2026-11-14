//! Suite `operator_soak`: the cluster form of `scripts/soak.sh`. It appends a
//! fixed set of events, then replays them for the soak window, and passes only
//! when every acked append is read back and the server RSS grows no more than
//! `TAPE_SOAK_RSS_GROWTH_PCT` (10) percent over the steady window.
//! The window comes from `E2E_SOAK_HOURS` (0.05 to 5, default 4). RSS is read
//! from `/proc/1/status` of the server container (`pods/exec`, no metrics API).
//! The suite is dispatch only: it is not in `all` and not in `ci.yml`.

use std::time::Duration;

use serde_json::json;

use super::{append, assert_replay, endpoint, evidence, kubectl, required_env, wait_healthy};

const SUITE: &str = "operator_soak";
const POD: &str = "tape-0";
const TOPIC: &str = "soak-orders";
const EVENTS: usize = 64;
// Same value as TAPE_SOAK_RSS_GROWTH_PCT in scripts/soak.sh.
const RSS_GROWTH_PCT: f64 = 10.0;
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ROUND_PAUSE: Duration = Duration::from_secs(5);

/// Reads VmRSS in kB from the text of `/proc/<pid>/status`.
fn parse_vm_rss_kb(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let value = line.trim_start_matches("VmRSS:").trim();
    value.strip_suffix("kB")?.trim().parse().ok()
}

/// Relative RSS growth in percent from `start_kb` to `end_kb`.
fn rss_growth_pct(start_kb: u64, end_kb: u64) -> f64 {
    assert!(start_kb > 0, "RSS at the start of the window is 0 kB");
    (end_kb as f64 - start_kb as f64) * 100.0 / start_kb as f64
}

/// The soak window for `E2E_SOAK_HOURS`; refuses values outside 0.05 to 5.
fn soak_window(hours: &str) -> Duration {
    let value: f64 = hours
        .parse()
        .unwrap_or_else(|_| panic!("E2E_SOAK_HOURS must be a number from 0.05 to 5, got {hours}"));
    assert!(
        (0.05..=5.0).contains(&value),
        "E2E_SOAK_HOURS must be a number from 0.05 to 5, got {hours}"
    );
    Duration::from_secs_f64(value * 3600.0)
}

/// VmRSS of the server process (PID 1 of the container), in kB.
fn sample_rss_kb() -> u64 {
    let status = kubectl(&["exec", POD, "-c", "server", "--", "cat", "/proc/1/status"]);
    parse_vm_rss_kb(&status).unwrap_or_else(|| panic!("no VmRSS in /proc/1/status: {status}"))
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
    let ids: Vec<String> = (1..=EVENTS).map(|i| format!("s-{i:04}")).collect();
    let want: Vec<&str> = ids.iter().map(String::as_str).collect();

    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    evidence(SUITE, "server healthy", json!({ "pod": POD }));

    for id in &ids {
        append(&http, &base, TOPIC, id).await;
    }
    assert_replay(&http, &base, TOPIC, &want).await;
    evidence(SUITE, "seeded", json!({ "events": EVENTS }));

    // Warm-up is a fifth of the window; the steady window is the rest.
    let warmup = window / 5;
    let steady = window - warmup;
    let warm_until = std::time::Instant::now() + warmup;
    while std::time::Instant::now() < warm_until {
        assert_replay(&http, &base, TOPIC, &want).await;
        tokio::time::sleep(ROUND_PAUSE).await;
    }
    let start_kb = sample_rss_kb();
    evidence(
        SUITE,
        "rss start",
        json!({ "rss_kb": start_kb, "pod": POD }),
    );

    let steady_until = std::time::Instant::now() + steady;
    let mut rounds: u64 = 0;
    while std::time::Instant::now() < steady_until {
        assert_replay(&http, &base, TOPIC, &want).await;
        rounds += 1;
        tokio::time::sleep(ROUND_PAUSE).await;
    }

    let end_kb = sample_rss_kb();
    let growth = rss_growth_pct(start_kb, end_kb);
    evidence(
        SUITE,
        "rss end",
        json!({
            "rss_kb": end_kb,
            "growth_pct": growth,
            "bound_pct": RSS_GROWTH_PCT,
            "rounds": rounds
        }),
    );

    assert_replay(&http, &base, TOPIC, &want).await;
    assert!(
        growth < RSS_GROWTH_PCT,
        "RSS grew {growth:.1}% ({start_kb} kB to {end_kb} kB), bound {RSS_GROWTH_PCT}%"
    );
    evidence(SUITE, "pass", json!({ "growth_pct": growth }));
}

#[test]
fn parse_vm_rss_kb_reads_the_status_line() {
    assert_eq!(
        parse_vm_rss_kb("Name:\ttape\nVmRSS:\t  12345 kB\nThreads:\t4\n"),
        Some(12345)
    );
    assert_eq!(parse_vm_rss_kb("Name:\ttape\n"), None);
    assert_eq!(parse_vm_rss_kb("VmRSS:\tabc kB\n"), None);
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
    assert!(std::panic::catch_unwind(|| soak_window("5.1")).is_err());
    assert!(std::panic::catch_unwind(|| soak_window("0")).is_err());
    assert!(std::panic::catch_unwind(|| soak_window("abc")).is_err());
}
