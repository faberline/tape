//! SIGTERM on a raft leader runs the graceful shutdown sequence: the leader
//! hands leadership to a caught-up voter within `--grace-secs`, exits 0, and
//! the survivors keep every committed event. Three real `tape` processes on
//! the h2c peer topology, where the raft routes share the public listener,
//! so the listener must stay open until the handoff is done. Closing it then
//! must not run to the deadline: the long-lived peer connections are told to
//! close, so the leader exits well within its grace.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::raft_failover::{
    append, free_addr, spawn_node_with, wait_healthy, wait_leader, wait_replayed, Node,
    REQUEST_BUDGET,
};

const GRACE_SECS: u64 = 5;
/// The kubelet-side slack the manifests add on top of `--grace-secs`.
const TERMINATION_SLACK: Duration = Duration::from_secs(5);

fn sigterm(pid: u32) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("run kill -TERM");
    assert!(status.success(), "kill -TERM {pid} failed to run");
}

/// Wait for `node` to exit on its own and return whether it exited 0, and
/// how long that took.
fn wait_exit(node: &mut Node, budget: Duration) -> (bool, Duration) {
    let started = Instant::now();
    let deadline = started + budget;
    loop {
        if let Some(status) = node.child.try_wait().expect("poll tape child") {
            return (status.success(), started.elapsed());
        }
        assert!(
            Instant::now() < deadline,
            "tape did not exit within {budget:?} of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_leader_hands_off_leadership_and_exits_cleanly() {
    let binds: Vec<String> = (0..3).map(|_| free_addr()).collect();
    let peers_csv = binds.join(",");
    let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let logs = tempfile::tempdir().unwrap();
    let log_path = |i: usize| logs.path().join(format!("tape-{i}.log"));

    let mut nodes: Vec<Node> = (0..3usize)
        .map(|i| {
            let log = std::fs::File::create(log_path(i)).unwrap();
            spawn_node_with(i as u32, &binds[i], dirs[i].path(), &peers_csv, |command| {
                command
                    .env("TAPE_GRACE_SECS", GRACE_SECS.to_string())
                    .env("TAPE_DRAIN_DELAY_SECS", "0")
                    .env("TAPE_LOG_FORMAT", "json")
                    .env("RUST_LOG", "info")
                    .stdout(Stdio::from(log));
            })
        })
        .collect();

    let client = reqwest::Client::builder()
        .timeout(REQUEST_BUDGET)
        .build()
        .unwrap();
    for node in &nodes {
        wait_healthy(&client, &node.base_url(), "startup health").await;
    }
    let base_urls: Vec<String> = nodes.iter().map(Node::base_url).collect();
    let base_refs: Vec<&str> = base_urls.iter().map(String::as_str).collect();
    let leader_url = wait_leader(&client, &base_refs, "initial election")
        .await
        .to_string();

    for n in [1, 2] {
        let resp = append(&client, &leader_url, n).await;
        assert!(
            resp.status().is_success(),
            "append {n} failed before SIGTERM"
        );
    }

    let leader_idx = base_urls
        .iter()
        .position(|url| *url == leader_url)
        .expect("leader is one of our nodes");
    sigterm(nodes[leader_idx].child.id());
    let budget = Duration::from_secs(GRACE_SECS) + TERMINATION_SLACK;
    let (success, elapsed) = wait_exit(&mut nodes[leader_idx], budget);
    assert!(success, "a leader that shut down within its grace exits 0");
    assert!(
        elapsed < Duration::from_secs(GRACE_SECS - 1),
        "the leader exits well before its {GRACE_SECS}s grace, not at it: {elapsed:?}"
    );

    let log = std::fs::read_to_string(log_path(leader_idx)).unwrap();
    assert!(
        log.contains("\"http_drained\""),
        "the public HTTP server reports a clean drain:\n{log}"
    );
    for cut in ["\"http_drain_incomplete\"", "\"http_drain_aborted\""] {
        assert!(
            !log.contains(cut),
            "the public HTTP drain is not cut at the deadline ({cut}):\n{log}"
        );
    }
    let report = log
        .lines()
        .find(|line| line.contains("\"raft_shutdown\""))
        .unwrap_or_else(|| panic!("no raft_shutdown event in the leader's log:\n{log}"));
    assert!(
        report.contains("Transferred"),
        "the leader hands leadership to a caught-up voter: {report}"
    );
    assert!(
        report.contains("\"peer_listener_close_safe\":true"),
        "a completed shutdown marks the peer listener safe to close: {report}"
    );

    let survivor_urls: Vec<String> = base_urls
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != leader_idx)
        .map(|(_, url)| url.clone())
        .collect();
    let survivor_refs: Vec<&str> = survivor_urls.iter().map(String::as_str).collect();
    let new_leader = wait_leader(&client, &survivor_refs, "post-handoff leader")
        .await
        .to_string();
    let resp = append(&client, &new_leader, 3).await;
    assert!(resp.status().is_success(), "append 3 failed after handoff");
    for base in &survivor_urls {
        wait_replayed(&client, base, &[1, 2, 3], "post-handoff convergence").await;
    }
}
