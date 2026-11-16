//! The tape server replicas: Raft leader, replay, the StatefulSet rollout and the append retry.

use super::super::{required_env, try_kubectl, wait_for};
use super::leader::is_ready;
use super::{APPEND_SETTLE_TIMEOUT, LEASE_TIMEOUT, SERVER_LABEL, SERVER_TIMEOUT, TAPE};
use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

pub(super) fn wait_for_ready_tape_replicas(want: usize) -> Vec<String> {
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

/// Ok only when the StatefulSet has finished its rollout to `want` replicas:
/// the controller saw the newest generation, the update revision is the
/// current one, and every replica is updated and Ready.
fn statefulset_rollout_complete(statefulset: &Value, want: u64) -> Result<(), String> {
    let count = |field: &str| -> Result<u64, String> {
        statefulset["status"][field]
            .as_u64()
            .ok_or_else(|| format!("status.{field} missing"))
    };
    let generation = statefulset["metadata"]["generation"]
        .as_u64()
        .ok_or_else(|| "metadata.generation missing".to_owned())?;
    let observed = count("observedGeneration")?;
    if observed != generation {
        return Err(format!(
            "observedGeneration {observed} != generation {generation}"
        ));
    }
    let current = statefulset["status"]["currentRevision"]
        .as_str()
        .unwrap_or_default();
    let update = statefulset["status"]["updateRevision"]
        .as_str()
        .unwrap_or_default();
    if current.is_empty() || update.is_empty() || current != update {
        return Err(format!(
            "updateRevision {update} != currentRevision {current}"
        ));
    }
    for field in ["replicas", "updatedReplicas", "readyReplicas"] {
        let got = count(field)?;
        if got != want {
            return Err(format!("{field} {got} != {want}"));
        }
    }
    Ok(())
}

pub(super) fn wait_for_statefulset_rollout(want: u64) {
    wait_for(
        &format!("statefulset {TAPE} rolled out to {want} replicas"),
        SERVER_TIMEOUT,
        || {
            let out = try_kubectl(&["get", "statefulset", TAPE, "-o", "json"])?;
            let parsed: Value =
                serde_json::from_str(&out).map_err(|err| format!("not JSON: {err}"))?;
            statefulset_rollout_complete(&parsed, want)
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
pub(super) async fn append_when_settled(
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

pub(super) fn wait_for_raft_leader(pods: &[String]) -> String {
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

pub(super) fn wait_for_survivor_leader(survivors: &[String], timeout: Duration) -> String {
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

pub(super) fn assert_replica_replay(pod: &str, topic: &str, want: &[&str]) {
    wait_for(
        &format!("replica {pod} replay catch-up for {topic}"),
        Duration::from_secs(30),
        || {
            let ns = required_env("NAMESPACE");
            let raw =
                format!("/api/v1/namespaces/{ns}/pods/{pod}:7137/proxy/topics/{topic}/replay");
            let out = try_kubectl(&["get", "--raw", &raw])?;
            let body: Value =
                serde_json::from_str(&out).map_err(|e| format!("replay not json: {e}"))?;
            let events = body["events"]
                .as_array()
                .ok_or_else(|| format!("no events array in {body}"))?;
            let ids: Vec<&str> = events
                .iter()
                .filter_map(|e| e["payload"]["id"].as_str())
                .collect();
            if ids == want {
                Ok(())
            } else {
                Err(format!("got {ids:?}, want {want:?}"))
            }
        },
    );
}

#[test]
fn raft_unavailable_503_is_transient() {
    let body = json!({
        "error": "raft_unavailable",
        "message": "raft: proposal routing timed out",
    });
    assert!(is_transient_raft_error(503, &body));
}

#[test]
fn other_503_is_not_transient() {
    assert!(!is_transient_raft_error(
        503,
        &json!({ "error": "overloaded" })
    ));
}

#[test]
fn non_503_is_not_transient() {
    assert!(!is_transient_raft_error(
        500,
        &json!({ "error": "raft_unavailable" })
    ));
}

fn rolled_out_statefulset() -> Value {
    json!({
        "metadata": {"generation": 4},
        "status": {
            "observedGeneration": 4,
            "currentRevision": "tape-b",
            "updateRevision": "tape-b",
            "replicas": 3,
            "updatedReplicas": 3,
            "readyReplicas": 3
        }
    })
}

#[test]
fn rollout_is_complete_when_revisions_match_and_all_replicas_are_ready() {
    assert_eq!(
        statefulset_rollout_complete(&rolled_out_statefulset(), 3),
        Ok(())
    );
}

#[test]
fn rollout_is_incomplete_while_the_update_revision_differs() {
    let mut value = rolled_out_statefulset();
    value["status"]["currentRevision"] = json!("tape-a");
    value["status"]["updatedReplicas"] = json!(2);
    let err = statefulset_rollout_complete(&value, 3).unwrap_err();
    assert!(err.contains("updateRevision"), "{err}");
}

#[test]
fn rollout_is_incomplete_when_the_controller_has_not_seen_the_latest_generation() {
    let mut value = rolled_out_statefulset();
    value["status"]["observedGeneration"] = json!(3);
    let err = statefulset_rollout_complete(&value, 3).unwrap_err();
    assert!(err.contains("observedGeneration"), "{err}");
}

#[test]
fn rollout_is_incomplete_when_a_replica_is_not_ready() {
    let mut value = rolled_out_statefulset();
    value["status"]["readyReplicas"] = json!(2);
    let err = statefulset_rollout_complete(&value, 3).unwrap_err();
    assert!(err.contains("readyReplicas"), "{err}");
}

#[test]
fn rollout_is_incomplete_when_a_count_is_missing() {
    let mut value = rolled_out_statefulset();
    value["status"]
        .as_object_mut()
        .unwrap()
        .remove("readyReplicas");
    let err = statefulset_rollout_complete(&value, 3).unwrap_err();
    assert!(err.contains("readyReplicas"), "{err}");
}
