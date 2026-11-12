//! Suite `operator_upgrade`: a rolling image upgrade of a 3-replica Tape
//! group loses no acknowledged message. The Tape CR `tape` of
//! k8s/overlays/gke-test starts on the previous released image, publishes
//! continue while the CR's image moves to the candidate image (the
//! StatefulSet rolls pod by pod), and a replay from offset 0 must return
//! every acknowledged id, in order, with no gap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    append, assert_pods_run, assert_version, container, endpoint, evidence, kubectl, kubectl_json,
    required_env, wait_for, wait_healthy, Expected,
};

const SUITE: &str = "operator_upgrade";
const TAPE: &str = "tape";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const TOPIC: &str = "upgrade.rolling";
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ROLLOUT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const GRACE_SECS: u64 = 10;
const BEFORE_COUNT: usize = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn rolling_image_upgrade_loses_no_data() {
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

    // Start on the previous release with three replicas.
    let patch = json!({
        "spec": {
            "image": previous,
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS
        }
    })
    .to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);
    let pods = wait_ready_pods(3, SERVER_TIMEOUT);
    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    let mut versions = Vec::new();
    for pod in &pods {
        let spec = kubectl_json(&["get", "pod", pod.as_str()])["spec"].clone();
        assert_eq!(
            container(&spec, "server")["image"],
            previous.as_str(),
            "pod {pod} runs the previous image"
        );
        let version = kubectl(&[
            "exec",
            pod.as_str(),
            "-c",
            "server",
            "--",
            "tape",
            "--version",
        ]);
        assert_ne!(
            version.trim(),
            format!("tape {}", expected.version),
            "pod {pod} already runs the candidate build"
        );
        versions.push(version.trim().to_owned());
    }
    evidence(
        SUITE,
        "previous release running",
        json!({ "pods": pods, "versions": versions }),
    );

    // Publish while the group runs the previous release.
    for n in 1..=BEFORE_COUNT {
        append(&http, &base, TOPIC, &format!("up-{n:04}")).await;
    }

    // Keep publishing through the upgrade.
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = tokio::spawn(publish_until(
        http.clone(),
        base.clone(),
        TOPIC.to_owned(),
        stop.clone(),
    ));

    // Move the CR to the candidate image; the StatefulSet rolls the pods.
    let patch = json!({ "spec": { "image": expected.image } }).to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);

    let mut uids: Vec<(String, String)> = Vec::new();
    wait_for(
        "rolling upgrade to the candidate image",
        ROLLOUT_TIMEOUT,
        || {
            let pods = server_pods();
            let mut not_ready = 0;
            let mut all_candidate = pods.len() == 3;
            for pod in &pods {
                let name = pod["metadata"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let uid = pod["metadata"]["uid"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if !uids.iter().any(|(_, seen)| *seen == uid) {
                    uids.push((name, uid));
                }
                if !is_ready(pod) {
                    not_ready += 1;
                }
                if container(&pod["spec"], "server")["image"] != expected.image.as_str() {
                    all_candidate = false;
                }
            }
            assert!(
            pods.len() != 3 || not_ready <= 1,
            "{not_ready} of 3 server pods are not Ready at once; a pod-by-pod rollout keeps a quorum"
        );
            if all_candidate && not_ready == 0 {
                Ok(uids.clone())
            } else {
                Err(format!(
                    "{} live pods, {not_ready} not Ready, candidate image on {}",
                    pods.len(),
                    pods.iter()
                        .filter(
                            |p| container(&p["spec"], "server")["image"] == expected.image.as_str()
                        )
                        .count()
                ))
            }
        },
    );
    evidence(SUITE, "rollout finished", json!({ "uid_order": uids }));

    // Stop the publisher; it returns the ids it saw acknowledged.
    stop.store(true, Ordering::SeqCst);
    let acked = publisher.await.expect("publisher task");
    assert!(
        acked.len() >= 5,
        "only {} messages were acknowledged during the upgrade; the upgrade was not under load",
        acked.len()
    );

    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    let image_ids = assert_pods_run(&expected, SERVER_LABEL, "server", 3);
    assert_version(&http, &base, &expected).await;
    evidence(
        SUITE,
        "candidate running",
        json!({ "image": expected.image, "image_ids": image_ids }),
    );

    // Every acknowledged id is replayed, in order, with no gap.
    let want: Vec<String> = (1..=BEFORE_COUNT)
        .map(|i| format!("up-{i:04}"))
        .chain(acked.iter().cloned())
        .collect();
    let got = replay_ids(&http, &base, TOPIC).await;
    check_no_loss(&got, &want).unwrap_or_else(|err| panic!("{err}"));
    evidence(
        SUITE,
        "no loss",
        json!({ "before": BEFORE_COUNT, "during": acked.len(), "replayed": got.len() }),
    );

    // The group still accepts writes after the upgrade.
    append(&http, &base, TOPIC, "up-after").await;
    let got = replay_ids(&http, &base, TOPIC).await;
    assert_eq!(
        got.last().map(String::as_str),
        Some("up-after"),
        "replay after the upgrade ends with up-after: {got:?}"
    );
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

/// Appends `up-0021`, `up-0022`, ... until `stop` is set, and returns the ids
/// that were acknowledged. A failed attempt (connect error, timeout, 5xx) is
/// retried with the same id, so an acknowledged id is never skipped.
async fn publish_until(
    http: reqwest::Client,
    base: String,
    topic: String,
    stop: Arc<AtomicBool>,
) -> Vec<String> {
    let mut acked = Vec::new();
    let mut n = BEFORE_COUNT + 1;
    while !stop.load(Ordering::SeqCst) {
        let id = format!("up-{n:04}");
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
        acked.push(id);
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
    use super::check_no_loss;

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
}
