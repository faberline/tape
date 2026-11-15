//! Suite `operator_diskfull`: the Tape CR `tape` of k8s/overlays/gke-test
//! runs one pod `tape-0`. The suite fills the pod's data volume, shows that
//! appends are refused with 507 `storage_full` while reads keep working,
//! frees the space, and shows that the pod takes appends again with every
//! earlier acked id still readable. It follows docs/runbooks/disk-full.md.
//! Dispatch only: never in `all`, never in ci.yml.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    append, assert_replay, endpoint, evidence, kubectl, try_kubectl, wait_for, wait_healthy,
    Expected,
};

const SUITE: &str = "operator_diskfull";
const POD: &str = "tape-0";
const TOPIC: &str = "orders.diskfull";
const FILLER: &str = "/data/e2e-filler";
const FILLED_IDS: [&str; 5] = [
    "full-0001",
    "full-0002",
    "full-0003",
    "full-0004",
    "full-0005",
];

const FIRST_POD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const FULL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RECOVER_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn operator_diskfull_refuses_then_recovers() {
    let base = endpoint();
    // Checks the identity variables the Job sets; the suite itself does not
    // compare versions.
    let _expected = Expected::from_env();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    wait_for("pod tape-0 Ready", FIRST_POD_TIMEOUT, || {
        let pod = try_kubectl(&["get", "pod", POD, "-o", "json"])?;
        let pod: Value = serde_json::from_str(&pod).map_err(|err| err.to_string())?;
        let ready = pod["status"]["conditions"].as_array().is_some_and(|all| {
            all.iter()
                .any(|c| c["type"] == "Ready" && c["status"] == "True")
        });
        if ready {
            Ok(())
        } else {
            Err("Ready is not True".to_owned())
        }
    });
    wait_healthy(&http, &base, RECONCILE_TIMEOUT).await;

    for id in FILLED_IDS {
        append(&http, &base, TOPIC, id).await;
    }
    assert_replay(&http, &base, TOPIC, &FILLED_IDS).await;

    // dd writes until the volume is full and then exits non-zero.
    let filled = try_kubectl(&[
        "exec",
        POD,
        "-c",
        "server",
        "--",
        "dd",
        "if=/dev/zero",
        &format!("of={FILLER}"),
        "bs=1M",
    ]);
    let dd_error = match filled {
        Err(msg) => msg,
        Ok(out) => panic!("the volume did not fill; dd exited 0: {out}"),
    };
    assert!(
        dd_error.contains("No space left on device"),
        "dd must stop on a full volume: {dd_error}"
    );
    evidence(
        SUITE,
        "volume filled",
        json!({ "pod": POD, "path": FILLER, "dd": dd_error }),
    );

    // Appends are refused with 507 once the pod has noticed the full volume.
    let deadline = tokio::time::Instant::now() + FULL_TIMEOUT;
    let mut attempts = 0u32;
    let (refused_status, refused_body) = loop {
        attempts += 1;
        let id = format!("full-refused-{attempts:04}");
        let (status, body) = try_append(&http, &base, TOPIC, &id).await;
        if is_storage_full(status, &body) {
            break (status, body);
        }
        assert!(
            !(200..300).contains(&status),
            "append {id} was accepted on a full volume: {status} {body}"
        );
        if tokio::time::Instant::now() >= deadline {
            panic!("appends not refused as storage_full within {FULL_TIMEOUT:?}; last saw {status} {body}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    // Reads keep serving while the volume is full.
    assert_replay(&http, &base, TOPIC, &FILLED_IDS).await;
    let degraded = metric_value(&metrics_text(&http, &base).await, "tape_storage_degraded");
    assert_eq!(
        degraded,
        Some(1.0),
        "tape_storage_degraded while the volume is full"
    );
    evidence(
        SUITE,
        "refused while full",
        json!({ "status": refused_status, "body": refused_body, "reads": "ok" }),
    );

    kubectl(&["exec", POD, "-c", "server", "--", "rm", "-f", FILLER]);

    // The pod re-probes the volume on a timer, so earlier tries can still be
    // refused with 507. Any other status is a failure.
    let deadline = tokio::time::Instant::now() + RECOVER_TIMEOUT;
    loop {
        let (status, body) = try_append(&http, &base, TOPIC, "full-after").await;
        if status == 200 {
            break;
        }
        assert!(
            is_storage_full(status, &body),
            "append full-after after freeing space: {status} {body}"
        );
        if tokio::time::Instant::now() >= deadline {
            panic!("appends not accepted within {RECOVER_TIMEOUT:?}; last saw {status} {body}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    assert_eq!(
        metric_value(&metrics_text(&http, &base).await, "tape_storage_degraded"),
        Some(0.0),
        "tape_storage_degraded after recovery"
    );
    let mut want: Vec<&str> = FILLED_IDS.to_vec();
    want.push("full-after");
    assert_replay(&http, &base, TOPIC, &want).await;
    evidence(SUITE, "recovered", json!({ "topic": TOPIC, "ids": want }));
}

/// The error body of a full volume: status 507 and kind `storage_full`.
fn is_storage_full(status: u16, body: &Value) -> bool {
    status == 507 && body["error"] == "storage_full"
}

/// The value of an unlabelled sample `name` in Prometheus text, if any.
fn metric_value(metrics: &str, name: &str) -> Option<f64> {
    metrics
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next() != Some(name) {
                return None;
            }
            parts.next()?.parse().ok()
        })
}

async fn metrics_text(http: &reqwest::Client, base: &str) -> String {
    http.get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap_or_else(|err| panic!("GET /metrics: {err}"))
        .text()
        .await
        .unwrap_or_else(|err| panic!("GET /metrics body: {err}"))
}

/// Appends `{"id": id}` and returns the status and the JSON body, whatever
/// the status is.
async fn try_append(http: &reqwest::Client, base: &str, topic: &str, id: &str) -> (u16, Value) {
    let response = http
        .post(format!("{base}/topics/{topic}/append"))
        .json(&json!({ "payload": { "id": id } }))
        .send()
        .await
        .unwrap_or_else(|err| panic!("append {id}: {err}"));
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn is_storage_full_accepts_507_with_kind() {
        let body = json!({ "error": "storage_full" });
        assert!(super::is_storage_full(507, &body));
    }

    #[test]
    fn is_storage_full_rejects_other_statuses() {
        assert!(!super::is_storage_full(
            500,
            &json!({ "error": "storage_full" })
        ));
        assert!(!super::is_storage_full(
            507,
            &json!({ "error": "internal" })
        ));
    }

    #[test]
    fn metric_value_reads_a_gauge() {
        let text = "# TYPE tape_storage_degraded gauge\ntape_storage_degraded 1\n";
        assert_eq!(
            super::metric_value(text, "tape_storage_degraded"),
            Some(1.0)
        );
    }

    #[test]
    fn metric_value_matches_the_whole_name() {
        let text = "tape_storage_degraded_total 7\n";
        assert_eq!(super::metric_value(text, "tape_storage_degraded"), None);
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
            .find(|line| line.trim_start().starts_with("options:"))
            .expect("the suite input has an options line");
        assert!(
            options.contains("operator_diskfull"),
            "options must list operator_diskfull: {options}"
        );
        let all = workflow
            .lines()
            .find(|line| line.trim_start().starts_with("all)"))
            .expect("the resolve step has an all) case");
        assert!(
            !all.contains("operator_diskfull"),
            "all must not run operator_diskfull: {all}"
        );
        let job = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/gke-test-job/operator_diskfull/job.yaml"
        ))
        .expect("read the suite's job.yaml");
        assert!(
            job.contains("e2e::operator_diskfull::"),
            "job.yaml must run the suite filter"
        );
    }
}
