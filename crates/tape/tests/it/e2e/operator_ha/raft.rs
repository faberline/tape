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

/// One entry per pod with its last `/raftz` result, for a timeout message.
fn raftz_report(last: &[(String, Result<Value, String>)]) -> String {
    if last.is_empty() {
        return "no survivors".to_owned();
    }
    last.iter()
        .map(|(pod, result)| match result {
            Ok(v) => format!("{pod}: {v}"),
            Err(e) => format!("{pod}: error: {e}"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Ok only when every replica reports the same `last_index` and `commit_index`.
/// A replica that never got the log (run 44305850703) keeps a lower index, so
/// the leader must not be deleted before it catches up.
pub(super) fn replicas_caught_up(
    reports: &[(String, Result<Value, String>)],
) -> Result<(), String> {
    if reports.is_empty() {
        return Err("no replicas".to_owned());
    }
    let mut indexes: Vec<(&str, u64, u64)> = Vec::with_capacity(reports.len());
    for (pod, result) in reports {
        let body = result
            .as_ref()
            .map_err(|e| format!("{pod}: /raftz error: {e}"))?;
        match (body["last_index"].as_u64(), body["commit_index"].as_u64()) {
            (Some(last), Some(commit)) => indexes.push((pod.as_str(), last, commit)),
            _ => return Err(format!("{pod}: /raftz has no last_index or commit_index")),
        }
    }
    let (_, last0, commit0) = indexes[0];
    if indexes
        .iter()
        .all(|&(_, last, commit)| last == last0 && commit == commit0)
    {
        return Ok(());
    }
    let summary = indexes
        .iter()
        .map(|(pod, last, commit)| format!("{pod} last_index {last} commit_index {commit}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("replicas not caught up: {summary}"))
}

/// Waits until all replicas hold the same raft log and returns each pod's
/// `(pod, last_index, commit_index)`. A timeout names the state of every pod.
pub(super) fn wait_for_replicas_caught_up(pods: &[String]) -> Vec<(String, u64, u64)> {
    wait_for("all replicas hold the same raft log", LEASE_TIMEOUT, || {
        let reports: Vec<(String, Result<Value, String>)> = pods
            .iter()
            .map(|pod| (pod.clone(), raftz_of(pod)))
            .collect();
        replicas_caught_up(&reports)
            .map_err(|msg| format!("{msg}; last /raftz: {}", raftz_report(&reports)))?;
        Ok(reports
            .iter()
            .map(|(pod, result)| {
                let body = result.as_ref().expect("checked by replicas_caught_up");
                (
                    pod.clone(),
                    body["last_index"].as_u64().unwrap_or_default(),
                    body["commit_index"].as_u64().unwrap_or_default(),
                )
            })
            .collect())
    })
}

fn survivor_timeout_message(timeout: Duration, last: &[(String, Result<Value, String>)]) -> String {
    format!(
        "no survivor reported is_leader within {timeout:?}; last /raftz: {}",
        raftz_report(last)
    )
}

pub(super) fn wait_for_survivor_leader(survivors: &[String], timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let mut last: Vec<(String, Result<Value, String>)> = Vec::new();
        for pod in survivors {
            let raftz = raftz_of(pod);
            if let Ok(body) = &raftz {
                if body["is_leader"].as_bool().unwrap_or(false) {
                    return pod.clone();
                }
            }
            last.push((pod.clone(), raftz));
        }
        if Instant::now() >= deadline {
            dump_server_logs(survivors);
            panic!("{}", survivor_timeout_message(timeout, &last));
        }
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

#[test]
fn raftz_report_lists_each_survivor() {
    let last = vec![
        (
            "tape-0".to_owned(),
            Ok(json!({ "is_leader": false, "term": 3 })),
        ),
        ("tape-2".to_owned(), Err("not JSON: x".to_owned())),
    ];
    assert_eq!(
        raftz_report(&last),
        r#"tape-0: {"is_leader":false,"term":3}; tape-2: error: not JSON: x"#
    );
}

#[test]
fn raftz_report_of_no_survivors_says_so() {
    assert_eq!(raftz_report(&[]), "no survivors");
}

fn caught_up_report(pod: &str, last: u64, commit: u64) -> (String, Result<Value, String>) {
    (
        pod.to_owned(),
        Ok(json!({ "last_index": last, "commit_index": commit })),
    )
}

#[test]
fn replicas_caught_up_accepts_equal_logs() {
    let reports = vec![
        caught_up_report("tape-0", 6, 6),
        caught_up_report("tape-1", 6, 6),
        caught_up_report("tape-2", 6, 6),
    ];
    assert_eq!(replicas_caught_up(&reports), Ok(()));
}

#[test]
fn replicas_caught_up_rejects_an_empty_replica() {
    // The state of run 44305850703: tape-0 never got the log.
    let reports = vec![
        caught_up_report("tape-0", 0, 0),
        caught_up_report("tape-1", 6, 6),
        caught_up_report("tape-2", 6, 6),
    ];
    let err = replicas_caught_up(&reports).expect_err("an empty replica is not caught up");
    assert!(err.contains("replicas not caught up"), "{err}");
    assert!(err.contains("tape-0 last_index 0 commit_index 0"), "{err}");
    assert!(err.contains("tape-2 last_index 6 commit_index 6"), "{err}");
}

#[test]
fn replicas_caught_up_rejects_a_commit_lag() {
    let reports = vec![
        caught_up_report("tape-0", 6, 6),
        caught_up_report("tape-1", 6, 6),
        caught_up_report("tape-2", 6, 5),
    ];
    let err = replicas_caught_up(&reports).expect_err("a commit lag is not caught up");
    assert!(err.contains("commit_index 5"), "{err}");
}

#[test]
fn replicas_caught_up_names_a_pod_with_a_raftz_error() {
    let reports = vec![
        ("tape-0".to_owned(), Err("timeout".to_owned())),
        caught_up_report("tape-1", 6, 6),
    ];
    let err = replicas_caught_up(&reports).expect_err("a /raftz error is not caught up");
    assert!(err.contains("tape-0: /raftz error: timeout"), "{err}");
}

#[test]
fn replicas_caught_up_rejects_a_report_without_indexes() {
    let reports = vec![
        ("tape-0".to_owned(), Ok(json!({ "role": "Leader" }))),
        caught_up_report("tape-1", 6, 6),
    ];
    let err = replicas_caught_up(&reports).expect_err("a report without indexes is not caught up");
    assert!(err.contains("has no last_index or commit_index"), "{err}");
}

#[test]
fn replicas_caught_up_rejects_no_replicas() {
    assert_eq!(replicas_caught_up(&[]), Err("no replicas".to_owned()));
}

#[test]
fn operator_ha_waits_for_catch_up_before_the_leader_delete() {
    let src = include_str!("../operator_ha.rs");
    let start = src
        .find(concat!(
            "async fn ",
            "leader_sigterm_hands_off_and_exits_within_drain_delay"
        ))
        .expect("the leader_sigterm test is defined");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("the test ends") + 3;
    let func = &body[..end];
    let appended = func
        .find(concat!("appended events", " before deletion"))
        .expect("the append evidence is recorded");
    let catch_up = func
        .find(concat!("wait_for_replicas_caught_up", "(&pods)"))
        .expect("the test waits for the replicas to catch up");
    let delete = func
        .find("kubectl(&[\"delete\", \"pod\", &leader_pod")
        .expect("the test deletes the leader");
    assert!(
        appended < catch_up && catch_up < delete,
        "catch-up must sit between the appends and the leader delete: {func}"
    );
}

#[test]
fn survivor_timeout_message_names_the_bound_and_the_last_raftz() {
    let last = vec![("tape-0".to_owned(), Err("timeout".to_owned()))];
    assert_eq!(
        survivor_timeout_message(Duration::from_secs(10), &last),
        "no survivor reported is_leader within 10s; last /raftz: tape-0: error: timeout"
    );
}

#[test]
fn wait_for_survivor_leader_dumps_server_logs_when_it_times_out() {
    let src = include_str!("raft.rs");
    let start = src
        .find(concat!("pub(super) fn ", "wait_for_survivor_leader"))
        .expect("wait_for_survivor_leader is defined");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("wait_for_survivor_leader ends") + 3;
    let func = &body[..end];
    assert!(
        func.contains(concat!("dump_server_logs", "(survivors)")),
        "{func}"
    );
    assert!(
        func.contains(concat!("survivor_timeout_message", "(")),
        "{func}"
    );
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
