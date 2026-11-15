//! Suite `operator_nodeloss`: a 3-replica Tape group loses the node of its raft
//! leader. The Node object is deleted and the pod on it is force-deleted (not
//! drained). A survivor becomes leader, the lost pod is rescheduled and Ready
//! again, and every acked append is read back. Dispatch only, never in `all`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    assert_replay, endpoint, evidence, kubectl, kubectl_json, required_env, try_kubectl, wait_for,
    wait_healthy,
};

const SUITE: &str = "operator_nodeloss";
const TAPE: &str = "tape";
const SCALE_TO_ONE: &str = r#"{"spec":{"replicas":1}}"#;
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const GRACE_SECS: u64 = 10;
const LEASE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
// The first append can run while the last pod is still scheduled and its volume provisioned.
const APPEND_SETTLE_TIMEOUT: Duration = Duration::from_secs(4 * 60);
/// The stated bound for a survivor to lead after the leader's node is lost.
const NEW_LEADER_TIMEOUT: Duration = Duration::from_secs(120);
/// The lost pod may wait for the pool to add a node.
const RECOVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn operator_nodeloss_keeps_acked_appends() {
    const TOPIC: &str = "nodeloss.acked";
    let tape_endpoint = endpoint();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    // a. 1 operator + 3 tape pods need 4 nodes, the pool maximum: scale the operator to one replica.
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
    wait_healthy(&http, &tape_endpoint, SERVER_TIMEOUT).await;
    evidence(
        SUITE,
        "scaled tape to 3 replicas",
        json!({ "pods": pods, "replicasPerShard": 3, "voterCount": 3, "graceSecs": GRACE_SECS }),
    );

    // b. The raft leader and the node it runs on.
    let leader = wait_for_raft_leader(&pods);
    let leader_node = node_of(&kubectl_json(&["get", "pod", &leader]))
        .unwrap_or_else(|| panic!("leader {leader} has no node"));
    evidence(
        SUITE,
        "leader and node",
        json!({ "leader": leader, "node": leader_node }),
    );

    // c. Acked appends before the loss.
    let before = ["nl-0001", "nl-0002", "nl-0003", "nl-0004", "nl-0005"];
    for id in before {
        append_when_settled(&http, &tape_endpoint, TOPIC, id, &pods).await;
    }
    assert_replay(&http, &tape_endpoint, TOPIC, &before).await;

    // d. Lose the node: the Node object goes (its VM is not touched here), then the pod on it.
    kubectl(&["delete", "node", &leader_node, "--wait=false"]);
    kubectl(&[
        "delete",
        "pod",
        &leader,
        "--force",
        "--grace-period=0",
        "--wait=false",
    ]);
    evidence(
        SUITE,
        "node deleted",
        json!({ "node": leader_node, "pod": leader }),
    );

    // e. A survivor leads within the bound, and it is not the lost pod.
    let lost_at = Instant::now();
    let survivors: Vec<String> = pods.iter().filter(|p| *p != &leader).cloned().collect();
    let new_leader = wait_for_survivor_leader(&survivors, NEW_LEADER_TIMEOUT);
    check_leader_moved(&leader, &new_leader).unwrap_or_else(|err| panic!("{err}"));
    evidence(
        SUITE,
        "new leader",
        json!({ "new_leader": new_leader, "elapsed_secs": lost_at.elapsed().as_secs_f64() }),
    );

    // f. Writes still succeed with two replicas.
    let during = ["nl-0006", "nl-0007", "nl-0008"];
    for id in during {
        append_when_settled(&http, &tape_endpoint, TOPIC, id, &survivors).await;
    }

    // g. The lost pod is rescheduled, Ready, on another node.
    let rescheduled_on = wait_for("lost pod Ready again", RECOVER_TIMEOUT, || {
        let pod: Value = try_kubectl(&["get", "pod", &leader, "-o", "json"])
            .map(|out| serde_json::from_str(&out).unwrap())?;
        let node = node_of(&pod);
        match node {
            Some(n)
                if is_ready(&pod)
                    && pod["metadata"]["deletionTimestamp"].is_null()
                    && n != leader_node =>
            {
                Ok(n)
            }
            _ => Err(format!("on {:?}, ready {}", node_of(&pod), is_ready(&pod))),
        }
    });
    let all = wait_for_ready_tape_replicas(3);
    wait_healthy(&http, &tape_endpoint, SERVER_TIMEOUT).await;
    evidence(
        SUITE,
        "pod rescheduled",
        json!({ "pod": leader, "node": rescheduled_on, "replicas": all }),
    );

    // h. Every acked append reads back, in order.
    let every = [
        "nl-0001", "nl-0002", "nl-0003", "nl-0004", "nl-0005", "nl-0006", "nl-0007", "nl-0008",
    ];
    assert_replay(&http, &tape_endpoint, TOPIC, &every).await;

    // i. The recovered group accepts a write.
    append_when_settled(&http, &tape_endpoint, TOPIC, "nl-after", &all).await;
    evidence(
        SUITE,
        "recovered",
        json!({ "topic": TOPIC, "appended": "nl-after" }),
    );
}

/// The node a pod is scheduled on, or `None` while it is unscheduled.
fn node_of(pod: &Value) -> Option<String> {
    pod["spec"]["nodeName"]
        .as_str()
        .filter(|node| !node.is_empty())
        .map(str::to_owned)
}

/// The leader must have moved off the lost node's pod.
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
                let mut detail = format!("{} Ready of {} pods", ready.len(), all.len());
                for p in &all {
                    if let Some(conds) = p["status"]["conditions"].as_array() {
                        if let Some(unsched) = conds.iter().find(|c| {
                            c["type"] == "PodScheduled"
                                && c["status"] == "False"
                                && c["reason"] == "Unschedulable"
                        }) {
                            let name = p["metadata"]["name"].as_str().unwrap_or_default();
                            let msg = unsched["message"].as_str().unwrap_or_default();
                            detail.push_str(&format!("; {name} unschedulable: {msg}"));
                        }
                    }
                }
                Err(detail)
            }
        },
    )
}

fn raftz_of(pod: &str) -> Result<Value, String> {
    let ns = required_env("NAMESPACE");
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
    use serde_json::json;

    use super::{check_leader_moved, node_of};

    #[test]
    fn node_of_reads_the_node_name() {
        assert_eq!(
            node_of(&json!({"spec":{"nodeName":"gke-a"}})),
            Some("gke-a".to_owned())
        );
    }

    #[test]
    fn node_of_is_none_when_unscheduled() {
        assert_eq!(node_of(&json!({"spec":{}})), None);
        assert_eq!(node_of(&json!({"spec":{"nodeName":""}})), None);
    }

    #[test]
    fn check_leader_moved_accepts_a_new_leader() {
        assert_eq!(check_leader_moved("tape-0", "tape-1"), Ok(()));
    }

    #[test]
    fn check_leader_moved_rejects_the_same_leader() {
        let err = check_leader_moved("tape-0", "tape-0").unwrap_err();
        assert!(err.contains("tape-0"), "{err}");
    }

    #[test]
    fn workflow_lists_the_suite_and_keeps_it_out_of_all() {
        let workflow = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.github/workflows/gke-test.yml"
        ));
        let options = workflow
            .lines()
            .find(|l| l.starts_with("        options:"))
            .expect("suite options line");
        assert!(options.contains("operator_nodeloss"), "{options}");
        let all = workflow
            .lines()
            .find(|l| l.starts_with("            all)"))
            .expect("all case line");
        assert!(!all.contains("operator_nodeloss"), "{all}");

        let job = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/gke-test-job/operator_nodeloss/job.yaml"
        ));
        assert!(job.contains("e2e::operator_nodeloss::"), "job.yaml filter");

        let overlay = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/overlays/gke-test/kustomization.yaml"
        ));
        assert!(overlay.contains("e2e-node-loss.yaml"), "overlay resource");
    }
}
