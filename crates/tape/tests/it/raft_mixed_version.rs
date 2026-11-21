//! Mixed-version raft group (bar point 6): one replica from the previous
//! release, two from this build. The previous binary comes from
//! `TAPE_PREVIOUS_BIN`; CI fetches it from the image pinned in
//! `.github/previous-release-image`. The test never skips: without the
//! variable it fails.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::raft_failover::{
    append, free_addr, spawn_node_from, spawn_node_with, wait_healthy, wait_leader, wait_replayed,
    Node, REQUEST_BUDGET,
};

const EXIT_BUDGET: Duration = Duration::from_secs(15);

fn previous_binary() -> PathBuf {
    let raw = std::env::var("TAPE_PREVIOUS_BIN").unwrap_or_default();
    if raw.is_empty() {
        panic!(
            "TAPE_PREVIOUS_BIN is not set; CI takes the previous release binary from .github/previous-release-image with scripts/ci/fetch_previous_tape.sh"
        );
    }
    let path = PathBuf::from(raw);
    assert!(
        path.exists(),
        "TAPE_PREVIOUS_BIN points to a missing file: {}",
        path.display()
    );
    path
}

fn role(index: usize) -> &'static str {
    if index == 0 {
        "previous"
    } else {
        "current"
    }
}

fn sigterm(pid: u32) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("run kill -TERM");
    assert!(status.success(), "kill -TERM {pid} failed to run");
}

/// Wait for the SIGTERMed leader to exit, and require exit status 0.
fn wait_exit_ok(node: &mut Node, role: &str) {
    let deadline = Instant::now() + EXIT_BUDGET;
    loop {
        if let Some(status) = node.child.try_wait().expect("poll tape child") {
            assert!(
                status.success(),
                "leader ({role}) exited with {status} after SIGTERM"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "leader ({role}) did not exit within {EXIT_BUDGET:?} of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn previous_binary_is_not_the_current_binary() {
    let previous = previous_binary()
        .canonicalize()
        .expect("canonicalize previous binary");
    let current = Path::new(env!("CARGO_BIN_EXE_tape"))
        .canonicalize()
        .expect("canonicalize current binary");
    assert_ne!(
        previous, current,
        "TAPE_PREVIOUS_BIN must not be the current build"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_version_group_appends_reads_and_changes_leader() {
    let previous = previous_binary();
    let binds: Vec<String> = (0..3).map(|_| free_addr()).collect();
    let peers_csv = binds.join(",");
    let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();

    let mut nodes: Vec<Node> = vec![
        spawn_node_from(&previous, 0, &binds[0], dirs[0].path(), &peers_csv, |_| {}),
        spawn_node_with(1, &binds[1], dirs[1].path(), &peers_csv, |_| {}),
        spawn_node_with(2, &binds[2], dirs[2].path(), &peers_csv, |_| {}),
    ];

    let client = reqwest::Client::builder()
        .timeout(REQUEST_BUDGET)
        .build()
        .unwrap();

    for (i, node) in nodes.iter().enumerate() {
        let base = node.base_url();
        let phase = format!("mixed-version health (node {i}, {})", role(i));
        wait_healthy(&client, &base, &phase).await;
    }

    let base_urls: Vec<String> = nodes.iter().map(Node::base_url).collect();
    let base_refs: Vec<&str> = base_urls.iter().map(String::as_str).collect();
    let leader_url = wait_leader(&client, &base_refs, "mixed-version election")
        .await
        .to_string();

    // One append through each node: the previous build, then the two current.
    for (n, idx) in [(1i64, 0usize), (2, 1), (3, 2)] {
        let resp = append(&client, &base_urls[idx], n).await;
        let status = resp.status();
        assert!(
            status.is_success(),
            "mixed-version append {n} via node {idx} ({}) failed: {status}",
            role(idx)
        );
    }

    for (i, base) in base_urls.iter().enumerate() {
        let phase = format!("mixed-version read-back (node {i}, {})", role(i));
        wait_replayed(&client, base, &base_refs, &[1, 2, 3], &phase).await;
    }

    // Stop the current leader, whichever build it runs, with SIGTERM.
    let leader_idx = base_urls
        .iter()
        .position(|base| *base == leader_url)
        .expect("leader is one of our nodes");
    sigterm(nodes[leader_idx].child.id());
    wait_exit_ok(&mut nodes[leader_idx], role(leader_idx));

    let survivor_idx: Vec<usize> = (0..3).filter(|i| *i != leader_idx).collect();
    let survivor_urls: Vec<&str> = survivor_idx
        .iter()
        .map(|&i| base_urls[i].as_str())
        .collect();
    let new_leader = wait_leader(&client, &survivor_urls, "mixed-version re-election")
        .await
        .to_string();
    assert_ne!(new_leader, leader_url, "a survivor took over leadership");

    let new_leader_idx = base_urls
        .iter()
        .position(|base| *base == new_leader)
        .expect("new leader is one of our nodes");
    let resp = append(&client, &new_leader, 4).await;
    let status = resp.status();
    assert!(
        status.is_success(),
        "mixed-version append 4 via new leader (node {new_leader_idx}, {}) failed: {status}",
        role(new_leader_idx)
    );

    for &i in &survivor_idx {
        let phase = format!(
            "mixed-version read-back after leader change (node {i}, {})",
            role(i)
        );
        wait_replayed(&client, &base_urls[i], &base_refs, &[1, 2, 3, 4], &phase).await;
    }

    for &i in &survivor_idx {
        let _ = nodes[i].child.kill();
        let _ = nodes[i].child.wait();
    }
}
