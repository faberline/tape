//! Suite `operator_ha`: the operator's control plane under voluntary
//! disruption (#3053 AC1-AC4), on the operator layer of k8s/overlays/gke-test,
//! where each of the two replicas and tape-0 has a node of its own on
//! pool-tape-server.
//!
//! - AC1: more than one replica is Ready, on distinct nodes, and exactly one
//!   holds the Lease; the Lease and each replica's own `tape_operator_leader`
//!   gauge agree on which one.
//! - AC4: the metrics Service answers and has every replica as an endpoint,
//!   so a failed handover shows as a follower publishing `_leader 0`.
//! - AC2: deleting the leader hands the Lease to another replica, and that
//!   replica applies: a spec change reaches the child StatefulSet.
//! - AC3: draining the leader's node completes, and reconciliation goes on
//!   across the whole window. A heartbeat runs alongside the drain and goes
//!   on past its return until a sample lands under a new holder: the drain
//!   returns on eviction, and the orphaned Lease cannot be taken for its 15s
//!   duration, so the drain alone never contains the handover it causes.
//!
//! The monitoring component's render (the rest of AC4) needs no cluster; it
//! is checked offline in observability_assets.rs.

mod drain;
mod leader;
mod raft;
mod watch;

use super::{
    assert_pods_run, assert_replay, assert_version, endpoint, evidence, kubectl, kubectl_json,
    required_env, try_kubectl, wait_for, wait_healthy, Expected,
};
use drain::prove_drain_completes;
use leader::{
    assert_distinct_server_nodes, prove_lease_handover, wait_for_holder, wait_for_one_leader,
    wait_for_ready_replicas, wait_for_service_scrapeable,
};
use raft::{
    append_when_settled, assert_replica_replay, wait_for_raft_leader, wait_for_ready_tape_replicas,
    wait_for_replicas_caught_up, wait_for_statefulset_rollout, wait_for_survivor_leader,
};
use serde_json::json;
use std::io::Read;
use std::time::Duration;
use std::time::Instant;
use watch::{leadership_lines, parse_json_log_events, terminated_from_watch};

const SUITE: &str = "operator_ha";
const TAPE: &str = "tape";
const OPERATOR_LABEL: &str = "app.kubernetes.io/name=tape-operator";
const SCALE_TO_ONE: &str = r#"{"spec":{"replicas":1}}"#;
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
/// The Lease is named for the manager and lives in the operator's own
/// namespace; its holderIdentity is the holder's pod name.
const LEASE: &str = "tape-operator";
const METRICS_SERVICE: &str = "tape-operator-metrics";
const METRICS_PORT: u16 = 9090;
const POOL_LABEL: &str = "faberline.dev/pool";
const SERVER_POOL: &str = "tape-server";

/// A replacement replica may wait for pool-tape-server to add a node.
const REPLICAS_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const LEASE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// tape-0 may also wait for pool-tape-server to add a node.
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(90);
/// The metrics Service may wait out one replica being replaced.
const SERVICE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// A warm standby takes the Lease within a few renew periods; slower than
/// this and the second replica is not earning its keep.
const HANDOVER_LIMIT: Duration = Duration::from_secs(90);
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(180);
const DRAIN_TIMEOUT: &str = "240s";
const NEW_HOLDER_TIMEOUT: Duration = Duration::from_secs(180);
// The first append can run while the last pod is still scheduled and its volume provisioned.
const APPEND_SETTLE_TIMEOUT: Duration = Duration::from_secs(4 * 60);
const WORST_HEARTBEAT: Duration = Duration::from_secs(75);
const MIN_HEARTBEAT_SAMPLES: usize = 3;
const GRACE_SECS: u64 = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn leader_election_survives_a_leader_delete_and_a_node_drain() {
    // The suite talks to the operator, and to tape only for its `/version`;
    // the operator runs the same binary from the same image.
    let tape_endpoint = endpoint();
    let expected = Expected::from_env();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    // AC1: more than one Ready replica, each on a node of its own.
    let pods = wait_for_ready_replicas(2);
    let image_ids = assert_pods_run(&expected, OPERATOR_LABEL, "operator", pods.len() as u64);
    let nodes = assert_distinct_server_nodes(&pods);
    evidence(
        SUITE,
        "AC1 replicas ready on distinct nodes",
        json!({
            "tape_endpoint": tape_endpoint,
            "image": expected.image,
            "version": expected.version,
            "pods": pods.iter().map(|p| &p.name).collect::<Vec<_>>(),
            "nodes": nodes,
            "operator_image_ids": image_ids,
        }),
    );

    // AC1: exactly one leader, by the Lease and by every replica's gauge.
    let holder = wait_for_holder(None);
    let gauges = wait_for_one_leader(&http, &holder).await;
    evidence(
        SUITE,
        "AC1 exactly one leader",
        json!({ "lease_holder": holder, "tape_operator_leader": gauges }),
    );

    // AC4: the Service resolves, and every replica is its own target.
    let service = wait_for_service_scrapeable(&http, pods.len()).await;
    evidence(SUITE, "AC4 metrics Service scrapeable", service);

    wait_for(
        &format!("StatefulSet/{TAPE} from the operator"),
        LEASE_TIMEOUT,
        || try_kubectl(&["get", "statefulset", TAPE]).map(drop),
    );

    // The tape the operator rendered is the build under test.
    wait_healthy(&http, &tape_endpoint, SERVER_TIMEOUT).await;
    let version = assert_version(&http, &tape_endpoint, &expected).await;
    evidence(
        SUITE,
        "server reports the expected build",
        json!({ "endpoint": tape_endpoint, "git_sha": expected.git_sha, "version": version }),
    );

    // AC2: delete the leader; the Lease moves and the new holder applies.
    let handover = prove_lease_handover(&pods, &holder);
    evidence(SUITE, "AC2 leader delete hands over", handover);

    // The drain needs both replicas back, or the PDB refuses the eviction.
    let pods = wait_for_ready_replicas(2);
    let holder = wait_for_holder(None);

    // AC3: drain the leader's node with a live reconcile heartbeat.
    let drain = prove_drain_completes(&pods, &holder);
    evidence(SUITE, "AC3 drain keeps reconciling", drain);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn leader_sigterm_hands_off_and_exits_within_drain_delay() {
    let tape_endpoint = endpoint();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    // 1 operator + 3 tape pods need 4 nodes; the pool allows 4. Two operators would need 5. The suite runs with --test-threads=1 in name order, so the 2-replica case has already run, and the namespace is deleted after the run: do not scale back.
    #[rustfmt::skip]
    kubectl(&["patch", "deployment", "tape-operator", "--type=merge", "-p", SCALE_TO_ONE]);
    let operators = wait_for_ready_replicas(1);
    evidence(
        SUITE,
        "scaled operator to 1 replica",
        json!({ "operators": operators.iter().map(|p| p.name.clone()).collect::<Vec<_>>() }),
    );

    // Patch Tape CR tape to replicasPerShard: 3, voterCount: 3, graceSecs: GRACE_SECS
    let patch = json!({
        "spec": {
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS
        }
    })
    .to_string();
    kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch]);

    // Wait for tape-0..2 Ready
    // The StatefulSet rolls tape-0 after tape-1 and tape-2 are Ready; wait for it, or the leader delete leaves no quorum (#163).
    wait_for_statefulset_rollout(3);
    let pods = wait_for_ready_tape_replicas(3);
    evidence(
        SUITE,
        "scaled tape to 3 replicas",
        json!({
            "pods": pods,
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS
        }),
    );

    // Find raft leader via GET /raftz through pod proxy
    let leader_pod = wait_for_raft_leader(&pods);
    evidence(
        SUITE,
        "found raft leader via /raftz proxy",
        json!({ "leader": leader_pod }),
    );

    // Append a few events through the Service before deletion
    const TOPIC: &str = "ha.sigterm";
    for id in ["sigterm-1", "sigterm-2"] {
        append_when_settled(&http, &tape_endpoint, TOPIC, id, &pods).await;
    }
    evidence(
        SUITE,
        "appended events before deletion",
        json!({ "topic": TOPIC, "ids": ["sigterm-1", "sigterm-2"] }),
    );

    // Wait until all replicas hold the same log, so the delete does not leave
    // a replica that never got the appends (run 44305850703)
    let logs = wait_for_replicas_caught_up(&pods);
    evidence(
        SUITE,
        "all replicas hold the same raft log",
        json!({
            "replicas": logs
                .iter()
                .map(|(pod, last, commit)| json!({ "pod": pod, "last_index": last, "commit_index": commit }))
                .collect::<Vec<_>>()
        }),
    );

    // Re-check the leader, so the pod we delete is the leader at delete time
    let first_leader = leader_pod;
    let leader_pod = wait_for_raft_leader(&pods);
    evidence(
        SUITE,
        "raft leader re-checked before delete",
        json!({ "first": first_leader, "now": leader_pod, "changed": first_leader != leader_pod }),
    );

    // Record deletion time and delete the leader pod
    let leader_uid: String = kubectl_json(&["get", "pod", &leader_pod])["metadata"]["uid"]
        .as_str()
        .expect("leader pod uid")
        .to_owned();

    // Stream logs from the leader pod in background through its graceful shutdown
    let ns = required_env("NAMESPACE");
    let target_pod = leader_pod.clone();
    let log_stream = std::thread::spawn(move || {
        let output = std::process::Command::new("kubectl")
            .args(["-n", &ns, "logs", "-f", &target_pod, "-c", "server"])
            .output();
        output
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    });

    // Watch the leader pod from before the delete, so its terminated state is
    // seen even when the pod object is removed right after the container ends.
    let watch_ns = required_env("NAMESPACE");
    let mut watch = std::process::Command::new("kubectl")
        .args([
            "-n",
            &watch_ns,
            "get",
            "pod",
            &leader_pod,
            "-w",
            "-o",
            "json",
            "--output-watch-events",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start kubectl pod watch");
    let mut watch_out = watch.stdout.take().expect("pod watch stdout");
    let watch_reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = watch_out.read_to_string(&mut out);
        out
    });

    let deletion_time = Instant::now();
    let deletion_epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    kubectl(&["delete", "pod", &leader_pod, "--wait=false"]);
    evidence(
        SUITE,
        "deleted raft leader pod",
        json!({
            "deleted_pod": leader_pod,
            "leader_uid": leader_uid,
            "deletion_epoch_secs": deletion_epoch_secs,
        }),
    );

    // A survivor takes over within the drain delay (graceSecs); manual run saw 5.56s.
    let survivors: Vec<String> = pods.iter().filter(|p| *p != &leader_pod).cloned().collect();
    let new_leader = wait_for_survivor_leader(&survivors, Duration::from_secs(GRACE_SECS));
    let handoff_elapsed = deletion_time.elapsed();
    assert!(
        handoff_elapsed < Duration::from_secs(GRACE_SECS),
        "a survivor must report is_leader within graceSecs ({GRACE_SECS}s); took {handoff_elapsed:?}"
    );
    evidence(
        SUITE,
        "survivor became raft leader",
        json!({
            "new_leader": new_leader,
            "handoff_secs": handoff_elapsed.as_secs_f64(),
        }),
    );

    // Wait until the old pod object is gone, then stop the watch. The watch
    // holds the leader's terminated state; a replacement pod has another uid.
    let gone_deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < gone_deadline
        && try_kubectl(&["get", "pod", &leader_pod, "-o", "jsonpath={.metadata.uid}"])
            .is_ok_and(|uid| uid == leader_uid)
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = watch.kill();
    let _ = watch.wait();
    let watch_stream = watch_reader.join().expect("pod watch thread joined");
    let (exit_code, finished_epoch) = terminated_from_watch(&watch_stream, &leader_uid)
        .unwrap_or_else(|| {
            panic!("no terminated server status for the leader in the pod watch:\n{watch_stream}")
        });
    let finished_epoch = finished_epoch.unwrap_or_else(|| {
        panic!("finishedAt missing or unparseable for the leader in the pod watch:\n{watch_stream}")
    });

    // Collect leader shutdown logs
    let mut logs = log_stream.join().expect("log stream thread joined");
    if logs.trim().is_empty() || !logs.contains("raft_shutdown") {
        let fallback = try_kubectl(&["logs", &leader_pod, "-c", "server", "--previous"])
            .or_else(|_| try_kubectl(&["logs", &leader_pod, "-c", "server"]))
            .unwrap_or_default();
        if fallback.contains("raft_shutdown") {
            logs = fallback;
        }
    }
    let log_events = parse_json_log_events(&logs);

    let raft_shutdown = log_events
        .iter()
        .find(|e| e.event == "raft_shutdown")
        .unwrap_or_else(|| panic!("no raft_shutdown event in leader's log:\n{logs}"));
    assert!(
        raft_shutdown.raw.contains("Transferred"),
        "raft_shutdown must have handoff Transferred: {:?}\nleadership lines of the leader pod:\n{}",
        raft_shutdown.raw,
        leadership_lines(&log_events)
    );

    assert!(
        log_events.iter().any(|e| e.event == "http_drained"),
        "http_drained must be present in leader's log:\n{logs}"
    );
    assert!(
        !log_events
            .iter()
            .any(|e| e.event == "http_drain_incomplete"),
        "http_drain_incomplete must be absent from leader's log:\n{logs}"
    );
    assert!(
        !log_events.iter().any(|e| e.event == "http_drain_aborted"),
        "http_drain_aborted must be absent from leader's log:\n{logs}"
    );

    let shutdown_secs = finished_epoch.saturating_sub(deletion_epoch_secs);
    assert_eq!(exit_code, 0, "container exitCode must be 0");
    assert!(
        shutdown_secs < GRACE_SECS,
        "finishedAt minus deletion time ({shutdown_secs}s) must be less than graceSecs ({GRACE_SECS}s)"
    );

    evidence(
        SUITE,
        "verified leader graceful shutdown logs and status",
        json!({
            "exit_code": exit_code,
            "shutdown_secs": shutdown_secs,
            "raft_shutdown": raft_shutdown.raw,
            "http_drained": true,
            "http_drain_incomplete": false,
            "http_drain_aborted": false,
        }),
    );

    // After the pod rejoins, an append through the Service succeeds and
    // every replica's replay returns the identical event list
    let all_rejoined = wait_for_ready_tape_replicas(3);
    let rejoined_leader = wait_for_raft_leader(&all_rejoined);
    evidence(
        SUITE,
        "raft leader after the rejoin",
        json!({ "leader": rejoined_leader, "replicas": all_rejoined }),
    );
    append_when_settled(&http, &tape_endpoint, TOPIC, "sigterm-3", &all_rejoined).await;

    let expected_events = ["sigterm-1", "sigterm-2", "sigterm-3"];
    assert_replay(&http, &tape_endpoint, TOPIC, &expected_events).await;

    for pod in &all_rejoined {
        assert_replica_replay(pod, TOPIC, &expected_events);
    }
    evidence(
        SUITE,
        "every replica replayed identical events after rejoin",
        json!({
            "topic": TOPIC,
            "replicas": all_rejoined,
            "events": expected_events,
        }),
    );
}
#[test]
fn append_settle_timeout_covers_a_node_pool_scale_up() {
    assert_eq!(APPEND_SETTLE_TIMEOUT, Duration::from_secs(4 * 60));
}
#[test]
fn sigterm_test_appends_only_through_append_when_settled() {
    let src = include_str!("operator_ha.rs");
    let start = src
        .find(&["async fn leader_sigterm", "_hands_off"].concat())
        .expect("test fn present");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("end of fn");
    let body = &body[..end];
    assert!(
        !body.contains(&["append(", "&http"].concat()),
        "plain append call left in the sigterm test"
    );
    assert_eq!(
        body.matches("append_when_settled(").count(),
        2,
        "the loop and sigterm-3 must both use append_when_settled"
    );
}
#[test]
fn sigterm_test_waits_for_the_statefulset_rollout_before_it_picks_the_leader() {
    let src = include_str!("operator_ha.rs");
    let start = src
        .find(&["async fn leader_sigterm", "_hands_off"].concat())
        .expect("test fn present");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("end of fn");
    let body = &body[..end];
    let rollout = body
        .find(&["wait_for_statefulset_", "rollout(3)"].concat())
        .expect("rollout wait in the sigterm test");
    let pick = body
        .find(&["wait_for_ready_tape_", "replicas(3)"].concat())
        .expect("replica wait in the sigterm test");
    assert!(
        rollout < pick,
        "the rollout wait must come before the replica wait"
    );
}
