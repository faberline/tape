//! Raft leadership and replay of the tape servers in the operator_ha suite.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::{required_env, try_kubectl, wait_for};
use super::leader::is_ready;
use super::{APPEND_SETTLE_TIMEOUT, LEASE_TIMEOUT, SERVER_LABEL, SERVER_TIMEOUT};

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
