//! Suite `operator_partition`: a 3-replica Tape group loses the raft leader's
//! network link to its two peers. The two other pods keep accepting appends,
//! the cut-off pod acknowledges none, and after the link returns no
//! acknowledged append is lost and all three pods replay the same ids.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    assert_pods_run, endpoint, evidence, kubectl, kubectl_json, required_env, try_kubectl,
    wait_for, wait_healthy, Expected,
};

const SUITE: &str = "operator_partition";
const TAPE: &str = "tape";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const TOPIC: &str = "partition.cut";
const POLICY: &str = "tape-e2e-partition";
const GRACE_SECS: u64 = 10;
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ELECTION_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const HEAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const APPEND_SETTLE_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// Removes the NetworkPolicy when the test ends, also on a failure, so a
/// failed run never leaves the cut in place.
struct PolicyGuard;

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        let _ = try_kubectl(&["delete", "networkpolicy", POLICY, "--ignore-not-found"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn operator_partition_keeps_acked_appends() {
    let base = endpoint();
    let expected = Expected::from_env();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    // 1. Three replicas, all Ready and on the candidate image.
    let patch = json!({
        "spec": {
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS
        }
    })
    .to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);
    let pods = wait_ready_pods(3, SERVER_TIMEOUT);
    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    assert_pods_run(&expected, SERVER_LABEL, "server", 3);
    evidence(SUITE, "three replicas running", json!({ "pods": pods }));

    // 2. The raft leader is cut off; the other two form the majority.
    let leader = wait_for_raft_leader(&pods);
    let majority: Vec<String> = pods.iter().filter(|pod| **pod != leader).cloned().collect();
    evidence(
        SUITE,
        "leader found",
        json!({ "leader": leader, "majority": majority }),
    );

    // 3. Before the cut: ten acknowledged appends.
    let mut acked: Vec<String> = Vec::new();
    for n in 1..=10 {
        let id = format!("part-{n:04}");
        append_retry(&http, &base, TOPIC, &id).await;
        acked.push(id);
    }
    evidence(SUITE, "before the cut", json!({ "acked": acked.len() }));

    // 4. Apply the cut: a NetworkPolicy on the leader pod.
    let policy_file = std::env::temp_dir().join(format!("{POLICY}.json"));
    std::fs::write(&policy_file, partition_policy(&leader).to_string())
        .unwrap_or_else(|err| panic!("write {}: {err}", policy_file.display()));
    let policy_path = policy_file.to_str().expect("utf-8 temp path");
    kubectl(&["create", "-f", policy_path]);
    let _guard = PolicyGuard;
    evidence(
        SUITE,
        "cut applied",
        json!({ "leader": leader, "policy": POLICY }),
    );

    // 5. The cut-off pod must not acknowledge an append.
    for attempt in 1..=3 {
        let sent = http
            .post(format!("{}/topics/{TOPIC}/append", pod_url(&leader)))
            .timeout(Duration::from_secs(5))
            .json(&json!({ "payload": { "id": "part-cut" } }))
            .send()
            .await;
        if let Ok(response) = sent {
            assert!(
                !response.status().is_success(),
                "the cut-off pod {leader} acknowledged an append: the NetworkPolicy is not enforced on this cluster"
            );
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    evidence(
        SUITE,
        "cut-off pod refuses appends",
        json!({ "leader": leader, "attempts": 3 }),
    );

    // 6. The majority elects a leader and keeps accepting appends.
    let new_leader = wait_for("a leader among the majority", ELECTION_TIMEOUT, || {
        for pod in &majority {
            if let Ok(raftz) = raftz_of(pod) {
                if raftz["is_leader"].as_bool() == Some(true) {
                    return Ok(pod.clone());
                }
            }
        }
        Err("no leader among the majority in /raftz".to_owned())
    });
    for n in 11..=30 {
        let id = format!("part-{n:04}");
        append_retry(&http, &pod_url(&majority[0]), TOPIC, &id).await;
        acked.push(id);
    }
    assert_eq!(
        acked.len(),
        30,
        "the majority acknowledged {} appends in total, not 30",
        acked.len()
    );
    evidence(
        SUITE,
        "majority keeps accepting",
        json!({ "new_leader": new_leader, "acked": acked.len() }),
    );

    // 7. Heal the cut; all three pods must replay the same acknowledged ids.
    kubectl(&["delete", "networkpolicy", POLICY]);
    let deadline = tokio::time::Instant::now() + HEAL_TIMEOUT;
    loop {
        match heal_check(&http, &pods, &acked).await {
            Ok(()) => break,
            Err(last) if tokio::time::Instant::now() >= deadline => {
                panic!("all pods agree after the heal: not reached within {HEAL_TIMEOUT:?}; last saw: {last}")
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    evidence(
        SUITE,
        "all pods agree after the heal",
        json!({ "pods": pods, "acked": acked.len() }),
    );

    // 8. The group accepts writes after the heal.
    append_retry(&http, &base, TOPIC, "part-after").await;
    acked.push("part-after".to_owned());
    let got = replay_ids(&http, &base, TOPIC).await;
    assert_eq!(
        got.last().map(String::as_str),
        Some("part-after"),
        "replay after the heal ends with part-after: {got:?}"
    );
    check_no_loss(&got, &acked).unwrap_or_else(|err| panic!("{err}"));
    evidence(
        SUITE,
        "append after the heal",
        json!({ "acked": acked.len(), "replayed": got.len() }),
    );
}

/// The base URL of one server pod, through the headless Service.
fn pod_url(pod: &str) -> String {
    format!("http://{pod}.tape-headless:7137")
}

/// The NetworkPolicy that cuts `pod` off: it selects only that pod, accepts
/// ingress only from the test Job (pod label `app: tape-e2e`), and sends
/// egress only to the test Job and to DNS. Raft traffic is on port 7137
/// without mTLS, so the cut is by peer identity, not by port.
fn partition_policy(pod: &str) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": { "name": POLICY },
        "spec": {
            "podSelector": {
                "matchLabels": { "statefulset.kubernetes.io/pod-name": pod }
            },
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [
                { "from": [ { "podSelector": { "matchLabels": { "app": "tape-e2e" } } } ] }
            ],
            "egress": [
                { "to": [ { "podSelector": { "matchLabels": { "app": "tape-e2e" } } } ] },
                {
                    "to": [ { "namespaceSelector": {} } ],
                    "ports": [
                        { "protocol": "UDP", "port": 53 },
                        { "protocol": "TCP", "port": 53 }
                    ]
                }
            ]
        }
    })
}

/// One probe of the heal: every pod replays the same ids, and those ids
/// match the acknowledged ones.
async fn heal_check(
    http: &reqwest::Client,
    pods: &[String],
    acked: &[String],
) -> Result<(), String> {
    let mut first: Option<Vec<String>> = None;
    for pod in pods {
        let ids = try_replay_ids(http, &pod_url(pod), TOPIC)
            .await
            .map_err(|err| format!("{pod}: {err}"))?;
        match &first {
            None => first = Some(ids),
            Some(want) if *want != ids => {
                return Err(format!(
                    "{pod} replays {} ids, not the same as the others",
                    ids.len()
                ))
            }
            Some(_) => {}
        }
    }
    let replayed = first.unwrap_or_default();
    check_no_loss(&replayed, acked)
}

/// Appends `{"id": id}` to `topic` and returns once the server acknowledges
/// it. Retries only the transient `raft_unavailable` 503 and connect errors,
/// for at most `APPEND_SETTLE_TIMEOUT`. Any other error panics at once.
async fn append_retry(http: &reqwest::Client, base: &str, topic: &str, id: &str) {
    let deadline = tokio::time::Instant::now() + APPEND_SETTLE_TIMEOUT;
    loop {
        let last = match http
            .post(format!("{base}/topics/{topic}/append"))
            .json(&json!({ "payload": { "id": id } }))
            .send()
            .await
        {
            Err(err) => err.to_string(),
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
                format!("{status} {body}")
            }
        };
        assert!(
            tokio::time::Instant::now() < deadline,
            "append {id}: still failing after {APPEND_SETTLE_TIMEOUT:?}; last saw: {last}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// True only for the 503 that the server returns while a raft proposal cannot
/// reach a leader yet. Every other error is not transient.
fn is_transient_raft_error(status: u16, body: &Value) -> bool {
    status == 503 && body["error"] == "raft_unavailable"
}

/// Replays `topic` from offset 0 and panics on any error.
async fn replay_ids(http: &reqwest::Client, base: &str, topic: &str) -> Vec<String> {
    try_replay_ids(http, base, topic)
        .await
        .unwrap_or_else(|err| panic!("replay {topic}: {err}"))
}

/// Replays `topic` from offset 0 and returns its payload ids, in order.
async fn try_replay_ids(
    http: &reqwest::Client,
    base: &str,
    topic: &str,
) -> Result<Vec<String>, String> {
    let response = http
        .get(format!("{base}/topics/{topic}/replay?from_offset=0"))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("{status} {body}"));
    }
    Ok(body["events"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|e| e["payload"]["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
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

/// The raft state of one pod, read through the API server's pod proxy.
fn raftz_of(pod: &str) -> Result<Value, String> {
    let ns = required_env("NAMESPACE");
    let raw = format!("/api/v1/namespaces/{ns}/pods/{pod}:7137/proxy/raftz");
    let out = try_kubectl(&["get", "--raw", &raw])?;
    serde_json::from_str(&out).map_err(|err| format!("not JSON: {err}"))
}

fn wait_for_raft_leader(pods: &[String]) -> String {
    wait_for("raft leader via /raftz proxy", ELECTION_TIMEOUT, || {
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
    use super::{check_no_loss, partition_policy};

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn partition_check_no_loss_accepts_an_exact_match() {
        assert_eq!(
            check_no_loss(&ids(&["a", "b", "c"]), &ids(&["a", "b", "c"])),
            Ok(())
        );
    }

    #[test]
    fn partition_check_no_loss_collapses_a_retried_append() {
        assert_eq!(
            check_no_loss(&ids(&["a", "b", "b", "c"]), &ids(&["a", "b", "c"])),
            Ok(())
        );
    }

    #[test]
    fn partition_check_no_loss_reports_a_gap() {
        let err = check_no_loss(&ids(&["a", "c"]), &ids(&["a", "b", "c"]))
            .expect_err("a replay with a missing id must fail");
        assert!(
            err.contains("\"b\""),
            "message must name the missing id: {err}"
        );
    }

    #[test]
    fn partition_check_no_loss_reports_reordering() {
        assert!(check_no_loss(&ids(&["b", "a"]), &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn partition_check_no_loss_reports_an_extra_id() {
        let err = check_no_loss(&ids(&["a", "b", "z"]), &ids(&["a", "b"]))
            .expect_err("a replay with an unacknowledged id must fail");
        assert!(
            err.contains("\"z\""),
            "message must name the extra id: {err}"
        );
    }

    #[test]
    fn partition_policy_selects_only_the_leader_and_allows_only_the_job() {
        let policy = partition_policy("tape-1");
        assert_eq!(
            policy["spec"]["podSelector"]["matchLabels"]["statefulset.kubernetes.io/pod-name"],
            "tape-1"
        );
        assert_eq!(
            policy["spec"]["ingress"][0]["from"][0]["podSelector"]["matchLabels"]["app"],
            "tape-e2e"
        );
        assert!(
            policy["spec"]["ingress"][0].get("ports").is_none(),
            "the cut must not be by port: raft traffic is on 7137 without mTLS"
        );
    }
}
