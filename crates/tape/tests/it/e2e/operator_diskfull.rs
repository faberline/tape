//! Suite `operator_diskfull`: the data volume of the pod `tape-0` is filled.
//! Appends then answer 507 `storage_full` while reads keep working; after the
//! space is freed the pod accepts appends again, and every id acknowledged
//! before the fill is still readable. Follows `docs/runbooks/disk-full.md`.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    append, assert_pods_run, assert_replay, endpoint, evidence, kubectl, kubectl_json, try_kubectl,
    wait_for, wait_healthy, Expected,
};

const SUITE: &str = "operator_diskfull";
const POD: &str = "tape-0";
const TOPIC: &str = "orders.diskfull";
const FILLER: &str = "/data/e2e-filler";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const FIRST_POD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const FULL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RECOVER_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn operator_diskfull_refuses_then_recovers() {
    let base = endpoint();
    let expected = Expected::from_env();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    // a. The pod is Ready and runs the image under test.
    wait_for("tape-0 Ready", FIRST_POD_TIMEOUT, || {
        let pod = kubectl_json(&["get", "pod", POD]);
        if is_ready(&pod) {
            Ok(())
        } else {
            Err(format!("{POD} is not Ready"))
        }
    });
    wait_healthy(&http, &base, RECONCILE_TIMEOUT).await;
    assert_pods_run(&expected, SERVER_LABEL, "server", 1);

    // b. Five acknowledged appends before the fill.
    let acked: Vec<String> = (1..=5).map(|n| format!("full-{n:04}")).collect();
    for id in &acked {
        append(&http, &base, TOPIC, id).await;
    }
    let want: Vec<&str> = acked.iter().map(String::as_str).collect();
    assert_replay(&http, &base, TOPIC, &want).await;

    // c. Fill the volume until dd hits ENOSPC.
    match try_kubectl(&[
        "exec",
        POD,
        "-c",
        "server",
        "--",
        "dd",
        "if=/dev/zero",
        "of=/data/e2e-filler",
        "bs=1M",
    ]) {
        Ok(out) => panic!("the volume did not fill: dd exited cleanly: {out}"),
        Err(msg) => assert!(
            msg.contains("No space left on device"),
            "dd must fail with ENOSPC: {msg}"
        ),
    }
    evidence(
        SUITE,
        "volume filled",
        json!({ "pod": POD, "filler": FILLER }),
    );

    // d. Appends are refused with 507 storage_full once the node is degraded.
    let deadline = tokio::time::Instant::now() + FULL_TIMEOUT;
    let mut refused = 0;
    let (refused_status, refused_body) = loop {
        refused += 1;
        let id = format!("full-refused-{refused:04}");
        let (status, body) = try_append(&http, &base, TOPIC, &id).await;
        if is_storage_full(status, &body) {
            break (status, body);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "appends still not refused with 507 storage_full after {FULL_TIMEOUT:?}; last saw: {status} {body}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    // e. While full, reads still serve and the degraded gauge is 1.
    assert_replay(&http, &base, TOPIC, &want).await;
    let metrics = metrics_text(&http, &base).await;
    assert_eq!(
        metric_value(&metrics, "tape_storage_degraded"),
        Some(1.0),
        "tape_storage_degraded while full"
    );
    evidence(
        SUITE,
        "refused while full",
        json!({ "status": refused_status, "body": refused_body, "refused_attempts": refused }),
    );

    // f. Free the space.
    kubectl(&["exec", POD, "-c", "server", "--", "rm", "-f", FILLER]);

    // g. The pod re-probes and accepts appends again; only 507 storage_full is retried.
    let deadline = tokio::time::Instant::now() + RECOVER_TIMEOUT;
    loop {
        let (status, body) = try_append(&http, &base, TOPIC, "full-after").await;
        if (200..300).contains(&status) {
            break;
        }
        assert!(
            is_storage_full(status, &body),
            "append after the space was freed: {status} {body}"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "appends not accepted again after {RECOVER_TIMEOUT:?}; last saw: {status} {body}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // h. The degraded gauge is back at 0.
    let metrics = metrics_text(&http, &base).await;
    assert_eq!(
        metric_value(&metrics, "tape_storage_degraded"),
        Some(0.0),
        "tape_storage_degraded after recovery"
    );

    // i. The replay holds the five acknowledged ids, in order, then full-after.
    let mut want_all = want.clone();
    want_all.push("full-after");
    let got = replay_ids(&http, &base, TOPIC).await;
    assert_eq!(
        got, want_all,
        "replay after recovery: the acknowledged ids, in order, with no refused id"
    );

    // j.
    evidence(
        SUITE,
        "recovered",
        json!({ "replayed": got.len(), "acked_before_fill": acked.len() }),
    );
}

/// True only for the 507 that the server returns while the node is latched in
/// degraded read-only mode because the volume is full.
fn is_storage_full(status: u16, body: &Value) -> bool {
    status == 507 && body["error"] == "storage_full"
}

/// The value of the sample `name` in Prometheus text, matching the whole
/// metric name. Comment lines and other series are skipped.
fn metric_value(metrics: &str, name: &str) -> Option<f64> {
    metrics.lines().find_map(|line| {
        if line.starts_with('#') {
            return None;
        }
        let mut parts = line.split_whitespace();
        if parts.next() == Some(name) {
            parts.next().and_then(|value| value.parse().ok())
        } else {
            None
        }
    })
}

/// `GET {base}/metrics` as text; a failure panics.
async fn metrics_text(http: &reqwest::Client, base: &str) -> String {
    http.get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap_or_else(|err| panic!("GET {base}/metrics: {err}"))
        .text()
        .await
        .unwrap_or_else(|err| panic!("GET {base}/metrics: {err}"))
}

/// Appends `{"id": id}` to `topic` and returns the status and the JSON body,
/// whatever the status is.
async fn try_append(http: &reqwest::Client, base: &str, topic: &str, id: &str) -> (u16, Value) {
    let response = http
        .post(format!("{base}/topics/{topic}/append"))
        .json(&json!({ "payload": { "id": id } }))
        .send()
        .await
        .unwrap_or_else(|err| panic!("append {id}: {err}"));
    let status = response.status().as_u16();
    let body: Value = response
        .json()
        .await
        .unwrap_or_else(|err| panic!("append {id}: {err}"));
    (status, body)
}

/// Replays `topic` from offset 0 and returns its payload ids, in order.
async fn replay_ids(http: &reqwest::Client, base: &str, topic: &str) -> Vec<String> {
    let response = http
        .get(format!("{base}/topics/{topic}/replay?from_offset=0"))
        .send()
        .await
        .unwrap_or_else(|err| panic!("replay {topic}: {err}"));
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .unwrap_or_else(|err| panic!("replay {topic}: {err}"));
    assert!(status.is_success(), "replay {topic}: {status} {body}");
    body["events"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|e| e["payload"]["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn is_ready(pod: &Value) -> bool {
    pod["status"]["conditions"].as_array().is_some_and(|all| {
        all.iter()
            .any(|c| c["type"] == "Ready" && c["status"] == "True")
    })
}

#[cfg(test)]
mod tests {
    use super::{is_storage_full, metric_value};
    use serde_json::json;

    #[test]
    fn is_storage_full_accepts_507_with_kind() {
        assert!(is_storage_full(507, &json!({ "error": "storage_full" })));
    }

    #[test]
    fn is_storage_full_rejects_other_statuses() {
        assert!(!is_storage_full(500, &json!({ "error": "storage_full" })));
        assert!(!is_storage_full(507, &json!({ "error": "internal" })));
    }

    #[test]
    fn metric_value_reads_a_gauge() {
        let text = "# TYPE tape_storage_degraded gauge\ntape_storage_degraded 1\n";
        assert_eq!(metric_value(text, "tape_storage_degraded"), Some(1.0));
    }

    #[test]
    fn metric_value_matches_the_whole_name() {
        assert_eq!(
            metric_value("tape_storage_degraded_total 7\n", "tape_storage_degraded"),
            None
        );
    }

    #[test]
    fn workflow_lists_the_suite_and_keeps_it_out_of_all() {
        let workflow = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.github/workflows/gke-test.yml"
        ))
        .expect("read gke-test.yml");
        let options = workflow
            .lines()
            .find(|line| line.contains("options: ["))
            .expect("the suite options line");
        assert!(
            options.contains("operator_diskfull"),
            "options line: {options}"
        );
        let all = workflow
            .lines()
            .find(|line| line.trim_start().starts_with("all)"))
            .expect("the all) case line");
        assert!(
            !all.contains("operator_diskfull"),
            "the suite must not be in all: {all}"
        );
        let job = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/gke-test-job/operator_diskfull/job.yaml"
        ))
        .expect("read the suite's job.yaml");
        assert!(job.contains("e2e::operator_diskfull::"));
    }
}
