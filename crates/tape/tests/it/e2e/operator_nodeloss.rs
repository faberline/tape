//! Suite `operator_nodeloss`: the node of the raft leader of a 3-replica
//! group is deleted (not drained). A survivor becomes leader, the lost pod is
//! rescheduled and Ready again, and every acknowledged append is read back.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    assert_replay, endpoint, evidence, kubectl, kubectl_json, try_kubectl, wait_for, wait_healthy,
};

const SUITE: &str = "operator_nodeloss";
const TAPE: &str = "tape";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const TOPIC: &str = "nodeloss.acked";
const SCALE_TO_ONE: &str = r#"{"spec":{"replicas":1}}"#;
const GRACE_SECS: u64 = 10;
const LEASE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const APPEND_SETTLE_TIMEOUT: Duration = Duration::from_secs(4 * 60);
const NEW_LEADER_TIMEOUT: Duration = Duration::from_secs(120);
const RECOVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn operator_nodeloss_keeps_acked_appends() {
    let base = endpoint();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    // a. One operator and three tape replicas: 4 nodes, the pool maximum.
    kubectl(&[
        "patch",
        "deployment",
        "tape-operator",
        "--type=merge",
        "-p",
        SCALE_TO_ONE,
    ]);
    let patch = json!({
        "spec": {
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS
        }
    })
    .to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);
    let pods = wait_for_ready_tape_replicas(3);
    wait_healthy(&http, &base, SERVER_TIMEOUT).await;

    // b. The raft leader and the node it runs on.
    let leader = wait_for_raft_leader(&pods);
    let node = node_of(&kubectl_json(&["get", "pod", leader.as_str()]))
        .unwrap_or_else(|| panic!("leader {leader} has no spec.nodeName"));
    evidence(
        SUITE,
        "leader and node",
        json!({ "leader": leader, "node": node, "pods": pods }),
    );

    // c. Five acknowledged appends before the loss.
    let acked: Vec<String> = (1..=5).map(|n| format!("nl-{n:04}")).collect();
    for id in &acked {
        append_when_settled(&http, &base, TOPIC, id, &pods).await;
    }
    let want: Vec<&str> = acked.iter().map(String::as_str).collect();
    assert_replay(&http, &base, TOPIC, &want).await;

    // d. Lose the node: delete the Node object, then force-delete the pod on it.
    kubectl(&["delete", "node", node.as_str(), "--wait=false"]);
    kubectl(&[
        "delete",
        "pod",
        leader.as_str(),
        "--force",
        "--grace-period=0",
        "--wait=false",
    ]);
    evidence(
        SUITE,
        "node deleted",
        json!({ "node": node, "pod": leader }),
    );

    // e. A survivor leads, within the bound.
    let survivors: Vec<String> = pods.iter().filter(|pod| **pod != leader).cloned().collect();
    let started = Instant::now();
    let new_leader = wait_for_survivor_leader(&survivors, NEW_LEADER_TIMEOUT);
    check_leader_moved(&leader, &new_leader).unwrap_or_else(|err| panic!("{err}"));
    evidence(
        SUITE,
        "new leader",
        json!({ "leader": new_leader, "elapsed_secs": started.elapsed().as_secs() }),
    );

    // f. The group still accepts writes with two replicas.
    for n in 6..=8 {
        append_when_settled(&http, &base, TOPIC, &format!("nl-{n:04}"), &pods).await;
    }

    // g. The lost pod is Ready again on another node.
    let new_node = wait_for(
        "lost pod Ready again on another node",
        RECOVER_TIMEOUT,
        || {
            let pod =
                try_kubectl(&["get", "pod", leader.as_str(), "-o", "json"]).and_then(|out| {
                    serde_json::from_str::<Value>(&out).map_err(|err| err.to_string())
                })?;
            let rescheduled = is_ready(&pod) && pod["metadata"]["deletionTimestamp"].is_null();
            match node_of(&pod) {
                Some(n) if rescheduled && n != node => Ok(n),
                other => Err(format!(
                    "{leader} not Ready on a node other than {node}: node {other:?}, ready {}",
                    is_ready(&pod)
                )),
            }
        },
    );
    let pods = wait_for_ready_tape_replicas(3);
    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    evidence(
        SUITE,
        "pod rescheduled",
        json!({ "pod": leader, "node": new_node, "pods": pods }),
    );

    // h. Every acknowledged id, in order.
    let mut want_all = want.clone();
    want_all.extend(["nl-0006", "nl-0007", "nl-0008"]);
    assert_replay(&http, &base, TOPIC, &want_all).await;

    // i. The group accepts writes after the loss.
    append_when_settled(&http, &base, TOPIC, "nl-after", &pods).await;
    evidence(SUITE, "recovered", json!({ "acked": want_all.len() + 1 }));
}

/// The node a pod is scheduled on, from `spec.nodeName`; `None` when absent
/// or empty (the pod is not scheduled yet).
fn node_of(pod: &Value) -> Option<String> {
    pod["spec"]["nodeName"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Checks that the leader changed when its node was lost. The lost pod
/// cannot lead, so the same name after the loss is an error.
fn check_leader_moved(old: &str, new: &str) -> Result<(), String> {
    if old != new {
        Ok(())
    } else {
        Err(format!(
            "the leader is still {old}; the lost node's pod cannot lead"
        ))
    }
}

fn is_ready(pod: &Value) -> bool {
    pod["status"]["conditions"].as_array().is_some_and(|all| {
        all.iter()
            .any(|c| c["type"] == "Ready" && c["status"] == "True")
    })
}

fn wait_for_ready_tape_replicas(want: usize) -> Vec<String> {
    wait_for(
        &format!("{want} Ready tape server replicas"),
        SERVER_TIMEOUT,
        || {
            let pods: Value = try_kubectl(&["get", "pods", "-l", SERVER_LABEL, "-o", "json"])
                .map(|out| serde_json::from_str(&out).unwrap())?;
            let all = pods["items"].as_array().cloned().unwrap_or_default();
            let mut ready: Vec<String> = all
                .iter()
                .filter(|p| p["metadata"]["deletionTimestamp"].is_null() && is_ready(p))
                .filter_map(|p| p["metadata"]["name"].as_str().map(str::to_owned))
                .collect();
            ready.sort();
            if ready.len() >= want {
                Ok(ready)
            } else {
                Err(format!("{} Ready of {} pods", ready.len(), all.len()))
            }
        },
    )
}

fn raftz_of(pod: &str) -> Result<Value, String> {
    let ns = super::required_env("NAMESPACE");
    let raw = format!("/api/v1/namespaces/{ns}/pods/{pod}:7137/proxy/raftz");
    let out = try_kubectl(&["get", "--raw", &raw])?;
    serde_json::from_str(&out).map_err(|err| format!("not JSON: {err}"))
}

/// True only for the 503 that the server returns while a raft proposal cannot
/// reach a leader yet. Every other error is not transient.
fn is_transient_raft_error(status: u16, body: &Value) -> bool {
    status == 503 && body["error"] == "raft_unavailable"
}

/// Appends `{"id": id}` like `super::append`, but retries only the transient
/// `raft_unavailable` 503, for at most `APPEND_SETTLE_TIMEOUT`. Any other
/// error panics at once. On the final failure it prints the server logs.
async fn append_when_settled(
    http: &reqwest::Client,
    base: &str,
    topic: &str,
    id: &str,
    pods: &[String],
) {
    let deadline = Instant::now() + APPEND_SETTLE_TIMEOUT;
    let mut last = String::from("no attempt");
    loop {
        if Instant::now() >= deadline {
            dump_server_logs(pods);
            panic!("append {id}: still failing after {APPEND_SETTLE_TIMEOUT:?}; last saw: {last}");
        }
        match http
            .post(format!("{base}/topics/{topic}/append"))
            .json(&json!({ "payload": { "id": id } }))
            .send()
            .await
        {
            Err(err) => last = err.to_string(),
            Ok(response) => {
                let status = response.status();
                let body: Value = response
                    .json()
                    .await
                    .unwrap_or_else(|err| panic!("append {id}: {err}"));
                if status.is_success() {
                    assert_eq!(
                        body["payload"]["id"], id,
                        "append {id} echoes the payload: {body}"
                    );
                    return;
                }
                if !is_transient_raft_error(status.as_u16(), &body) {
                    panic!("append {id}: {status} {body}");
                }
                last = format!("{status} {body}");
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Prints the last 200 lines of the server container of each pod. Runs only on
/// a failure path, so it never panics.
fn dump_server_logs(pods: &[String]) {
    for pod in pods {
        let out = try_kubectl(&["logs", pod, "-c", "server", "--tail=200"])
            .unwrap_or_else(|err| format!("kubectl logs failed: {err}"));
        eprintln!("--- logs {pod} ---\n{out}");
    }
}

fn wait_for_raft_leader(pods: &[String]) -> String {
    wait_for("raft leader via /raftz proxy", LEASE_TIMEOUT, || {
        let mut leader = None;
        for pod in pods {
            if let Ok(raftz) = raftz_of(pod) {
                if raftz["is_leader"].as_bool().unwrap_or(false) {
                    if leader.is_some() {
                        return Err("multiple leaders seen in /raftz".to_owned());
                    }
                    leader = Some(pod.clone());
                }
            }
        }
        leader.ok_or_else(|| "no leader in /raftz".to_owned())
    })
}

fn wait_for_survivor_leader(survivors: &[String], timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        for pod in survivors {
            if let Ok(raftz) = raftz_of(pod) {
                if raftz["is_leader"].as_bool().unwrap_or(false) {
                    return pod.clone();
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "no survivor reported is_leader within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::{check_leader_moved, node_of};
    use serde_json::json;

    #[test]
    fn node_of_reads_the_node_name() {
        assert_eq!(
            node_of(&json!({ "spec": { "nodeName": "gke-a" } })),
            Some("gke-a".to_owned())
        );
    }

    #[test]
    fn node_of_is_none_when_unscheduled() {
        assert_eq!(node_of(&json!({ "spec": {} })), None);
        assert_eq!(node_of(&json!({ "spec": { "nodeName": "" } })), None);
    }

    #[test]
    fn check_leader_moved_accepts_a_new_leader() {
        assert_eq!(check_leader_moved("tape-0", "tape-1"), Ok(()));
    }

    #[test]
    fn check_leader_moved_rejects_the_same_leader() {
        let err = check_leader_moved("tape-0", "tape-0")
            .expect_err("the same leader after its node is lost must fail");
        assert!(
            err.contains("tape-0"),
            "message must name the leader: {err}"
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
            .find(|line| line.trim_start().starts_with("options:"))
            .expect("the suite options line");
        assert!(
            options.contains("operator_nodeloss"),
            "options line: {options}"
        );
        let all = workflow
            .lines()
            .find(|line| line.trim_start().starts_with("all)"))
            .expect("the all) case line");
        assert!(
            !all.contains("operator_nodeloss"),
            "the suite must not be in all: {all}"
        );
        let job = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/gke-test-job/operator_nodeloss/job.yaml"
        ))
        .expect("read the suite's job.yaml");
        assert!(job.contains("e2e::operator_nodeloss::"));
        let overlay = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/overlays/gke-test/kustomization.yaml"
        ))
        .expect("read the gke-test overlay");
        assert!(overlay.contains("e2e-node-loss.yaml"));
    }
}
