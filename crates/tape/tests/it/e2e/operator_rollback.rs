//! Suite `operator_rollback`: a rolling rollback of a 3-replica Tape group
//! from the image under test to the previous release loses no acknowledged
//! message, and the longest gap between two acknowledged appends stays below
//! `MAX_GAP`. The Tape CR `tape` of k8s/overlays/gke-test starts on the image
//! under test, publishes continue while the CR's image moves back to the
//! previous image (the StatefulSet rolls pod by pod), and a replay from offset
//! 0 must return every acknowledged id, in order, with no gap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    append, container, endpoint, evidence, kubectl, kubectl_json, required_env, wait_for,
    wait_healthy, Expected,
};

const SUITE: &str = "operator_rollback";
const TAPE: &str = "tape";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const TOPIC: &str = "rollback.rolling";
const ID_PREFIX: &str = "rb-";
const MAX_GAP: Duration = Duration::from_secs(60);
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ROLLOUT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const GRACE_SECS: u64 = 10;
const BEFORE_COUNT: usize = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn rolling_rollback_keeps_the_data() {
    let base = endpoint();
    let expected = Expected::from_env();
    let previous = required_env("E2E_PREVIOUS_IMAGE");
    assert_ne!(
        previous, expected.image,
        "E2E_PREVIOUS_IMAGE must differ from the image under test"
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    // Start on the image under test with three replicas.
    let patch = json!({
        "spec": {
            "image": expected.image,
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS
        }
    })
    .to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);
    let pods = wait_ready_pods(3, SERVER_TIMEOUT);
    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    for pod in &pods {
        let spec = kubectl_json(&["get", "pod", pod.as_str()])["spec"].clone();
        assert_eq!(
            container(&spec, "server")["image"],
            expected.image.as_str(),
            "pod {pod} runs the image under test"
        );
    }
    evidence(SUITE, "candidate running", json!({ "pods": pods }));

    // Publish while the group runs the image under test.
    for n in 1..=BEFORE_COUNT {
        append(&http, &base, TOPIC, &format!("{ID_PREFIX}{n:04}")).await;
    }

    // Keep publishing through the rollback.
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = tokio::spawn(publish_until(
        http.clone(),
        base.clone(),
        TOPIC.to_owned(),
        stop.clone(),
    ));

    // Move the CR back to the previous image; the StatefulSet rolls the pods.
    let patch = json!({ "spec": { "image": previous } }).to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);

    wait_for(
        "rolling rollback to the previous image",
        ROLLOUT_TIMEOUT,
        || {
            let pods = server_pods();
            let mut not_ready = 0;
            let mut all_previous = pods.len() == 3;
            for pod in &pods {
                if !is_ready(pod) {
                    not_ready += 1;
                }
                if container(&pod["spec"], "server")["image"] != previous.as_str() {
                    all_previous = false;
                }
            }
            assert!(
                pods.len() != 3 || not_ready <= 1,
                "{not_ready} of 3 server pods are not Ready at once; a pod-by-pod rollout keeps a quorum"
            );
            if all_previous && not_ready == 0 {
                Ok(())
            } else {
                Err(format!(
                    "{} live pods, {not_ready} not Ready, previous image on {}",
                    pods.len(),
                    pods.iter()
                        .filter(|p| container(&p["spec"], "server")["image"] == previous.as_str())
                        .count()
                ))
            }
        },
    );

    // Stop the publisher; it returns each acknowledged id with its time.
    stop.store(true, Ordering::SeqCst);
    let acked = publisher.await.expect("publisher task");
    assert!(
        acked.len() >= 5,
        "only {} messages were acknowledged during the rollback; the rollback was not under load",
        acked.len()
    );

    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    let pods = server_pods();
    assert_eq!(pods.len(), 3, "three server pods after the rollback");
    for pod in &pods {
        assert_eq!(
            container(&pod["spec"], "server")["image"],
            previous.as_str(),
            "pod {} runs the previous image",
            pod["metadata"]["name"]
        );
    }

    // No write stalls for longer than the stated bound.
    let times: Vec<Instant> = acked.iter().map(|(_, at)| *at).collect();
    let gap = max_gap(&times);
    check_gap(gap, MAX_GAP).unwrap_or_else(|err| panic!("{err}"));
    evidence(
        SUITE,
        "downtime",
        json!({
            "max_gap_ms": gap.as_millis() as u64,
            "bound_ms": MAX_GAP.as_millis() as u64
        }),
    );

    // Every acknowledged id is replayed, in order, with no gap.
    let want: Vec<String> = (1..=BEFORE_COUNT)
        .map(|i| format!("{ID_PREFIX}{i:04}"))
        .chain(acked.iter().map(|(id, _)| id.clone()))
        .collect();
    let got = replay_ids(&http, &base, TOPIC).await;
    check_no_loss(&got, &want).unwrap_or_else(|err| panic!("{err}"));
    evidence(
        SUITE,
        "no loss",
        json!({ "before": BEFORE_COUNT, "during": acked.len(), "replayed": got.len() }),
    );

    // The group still accepts writes after the rollback.
    append(&http, &base, TOPIC, "rb-after").await;
    let got = replay_ids(&http, &base, TOPIC).await;
    assert_eq!(
        got.last().map(String::as_str),
        Some("rb-after"),
        "replay after the rollback ends with rb-after: {got:?}"
    );
}

/// The largest time between two consecutive acknowledgements; zero for fewer
/// than two.
fn max_gap(times: &[Instant]) -> Duration {
    times
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .max()
        .unwrap_or(Duration::ZERO)
}

/// Accepts a gap at or below `bound`; a longer gap is an error that names both.
fn check_gap(gap: Duration, bound: Duration) -> Result<(), String> {
    if gap <= bound {
        Ok(())
    } else {
        Err(format!(
            "longest gap between acknowledged appends was {gap:?}, above the bound {bound:?}"
        ))
    }
}

/// Checks a replay against the acknowledged ids. A retried append may commit
/// twice, so consecutive equal entries of `replayed` collapse into one; any
/// other difference (a gap, reordering, an extra id) is an error.
fn check_no_loss(replayed: &[String], acked: &[String]) -> Result<(), String> {
    let mut collapsed: Vec<&str> = Vec::new();
    for id in replayed {
        if collapsed.last() != Some(&id.as_str()) {
            collapsed.push(id.as_str());
        }
    }
    let acked_refs: Vec<&str> = acked.iter().map(String::as_str).collect();
    if collapsed == acked_refs {
        return Ok(());
    }
    let first = collapsed
        .iter()
        .zip(acked_refs.iter())
        .position(|(got, want)| got != want)
        .unwrap_or(collapsed.len().min(acked_refs.len()));
    let missing: Vec<&str> = acked_refs
        .iter()
        .copied()
        .filter(|id| !collapsed.contains(id))
        .collect();
    let extra: Vec<&str> = collapsed
        .iter()
        .copied()
        .filter(|id| !acked_refs.contains(id))
        .collect();
    Err(format!(
        "replay differs from the acknowledged ids at index {first}: missing {missing:?}, extra {extra:?}"
    ))
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

/// Appends `rb-0021`, `rb-0022`, ... until `stop` is set, and returns each
/// acknowledged id with the instant of its acknowledgement. A failed attempt
/// (connect error, timeout, 5xx) is retried with the same id, so an
/// acknowledged id is never skipped.
async fn publish_until(
    http: reqwest::Client,
    base: String,
    topic: String,
    stop: Arc<AtomicBool>,
) -> Vec<(String, Instant)> {
    let mut acked = Vec::new();
    let mut n = BEFORE_COUNT + 1;
    while !stop.load(Ordering::SeqCst) {
        let id = format!("{ID_PREFIX}{n:04}");
        loop {
            let sent = http
                .post(format!("{base}/topics/{topic}/append"))
                .timeout(Duration::from_secs(5))
                .json(&json!({ "payload": { "id": id } }))
                .send()
                .await;
            match sent {
                Ok(response) if response.status().is_success() => break,
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        acked.push((id, Instant::now()));
        n += 1;
    }
    acked
}

/// The live server pods: those without a deletionTimestamp.
fn server_pods() -> Vec<Value> {
    kubectl_json(&["get", "pods", "-l", SERVER_LABEL])["items"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|pod| pod["metadata"]["deletionTimestamp"].is_null())
                .cloned()
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

/// Waits until exactly `want` live server pods exist and all are Ready.
/// Returns their names, sorted.
fn wait_ready_pods(want: usize, timeout: Duration) -> Vec<String> {
    wait_for(&format!("{want} ready server pods"), timeout, || {
        let pods = server_pods();
        let mut names: Vec<String> = pods
            .iter()
            .filter(|pod| is_ready(pod))
            .filter_map(|pod| pod["metadata"]["name"].as_str().map(str::to_owned))
            .collect();
        if pods.len() == want && names.len() == want {
            names.sort();
            Ok(names)
        } else {
            Err(format!("{} live pods, {} ready", pods.len(), names.len()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{check_gap, check_no_loss, max_gap};
    use std::time::{Duration, Instant};

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn check_no_loss_accepts_an_exact_match() {
        assert_eq!(
            check_no_loss(&ids(&["a", "b", "c"]), &ids(&["a", "b", "c"])),
            Ok(())
        );
    }

    #[test]
    fn check_no_loss_collapses_a_retried_duplicate() {
        assert_eq!(
            check_no_loss(&ids(&["a", "b", "b", "c"]), &ids(&["a", "b", "c"])),
            Ok(())
        );
    }

    #[test]
    fn check_no_loss_rejects_a_gap() {
        let err = check_no_loss(&ids(&["a", "c"]), &ids(&["a", "b", "c"]))
            .expect_err("a replay with a missing id must fail");
        assert!(
            err.contains("\"b\""),
            "message must name the missing id: {err}"
        );
    }

    #[test]
    fn check_no_loss_rejects_reordering() {
        assert!(check_no_loss(&ids(&["b", "a"]), &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn check_no_loss_rejects_an_extra_id() {
        let err = check_no_loss(&ids(&["a", "b", "z"]), &ids(&["a", "b"]))
            .expect_err("a replay with an unacknowledged id must fail");
        assert!(
            err.contains("\"z\""),
            "message must name the extra id: {err}"
        );
    }

    #[test]
    fn max_gap_is_the_largest_step() {
        let t0 = Instant::now();
        let times = [
            t0,
            t0 + Duration::from_secs(1),
            t0 + Duration::from_secs(4),
            t0 + Duration::from_secs(5),
        ];
        assert_eq!(max_gap(&times), Duration::from_secs(3));
    }

    #[test]
    fn max_gap_of_fewer_than_two_is_zero() {
        assert_eq!(max_gap(&[]), Duration::ZERO);
        assert_eq!(max_gap(&[Instant::now()]), Duration::ZERO);
    }

    #[test]
    fn check_gap_accepts_the_bound() {
        assert_eq!(
            check_gap(Duration::from_secs(60), Duration::from_secs(60)),
            Ok(())
        );
    }

    #[test]
    fn check_gap_rejects_a_longer_gap() {
        let err = check_gap(Duration::from_secs(61), Duration::from_secs(60))
            .expect_err("a gap above the bound must fail");
        assert!(
            err.contains("above the bound"),
            "message must say the gap is above the bound: {err}"
        );
    }

    #[test]
    fn gke_test_workflow_lists_the_suite() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let workflow = std::fs::read_to_string(format!("{root}/.github/workflows/gke-test.yml"))
            .expect("read gke-test.yml");
        let options = workflow
            .lines()
            .find(|line| line.trim_start().starts_with("options:"))
            .expect("gke-test.yml has an options line");
        assert!(
            options.contains("operator_rollback"),
            "options line lacks operator_rollback: {options}"
        );
        let job = std::fs::read_to_string(format!(
            "{root}/k8s/gke-test-job/operator_rollback/job.yaml"
        ))
        .expect("read operator_rollback job.yaml");
        assert!(
            job.contains("e2e::operator_rollback::"),
            "job.yaml lacks the operator_rollback test filter"
        );
    }
}
