//! 3-replica declarative subscription provisioning convergence.
//!
//! Proves that with Raft enabled, subscriptions declared via `TAPE_PROVISION_TOPICS`
//! reach each replica's journal only through committed Raft proposals. Divergent
//! subscription stanzas across three replicas converge to the leader-committed
//! union on every replica, and follower restart re-provisions idempotently without error.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crate::raft_failover::{
    free_addr, spawn_node_with, wait_healthy, wait_leader, Node, REQUEST_BUDGET,
};

async fn wait_subscriptions_converged(
    client: &reqwest::Client,
    nodes: &[Node],
    topic: &str,
    expected: &BTreeSet<String>,
    deadline: Instant,
) {
    loop {
        let mut all_match = true;
        for node in nodes {
            let resp = client
                .get(format!("{}/topics/{topic}/subscriptions", node.base_url()))
                .send()
                .await;
            let Ok(resp) = resp else {
                all_match = false;
                break;
            };
            if !resp.status().is_success() {
                all_match = false;
                break;
            }
            let Ok(body) = resp.json::<serde_json::Value>().await else {
                all_match = false;
                break;
            };
            let Some(subs) = body["subscriptions"].as_array() else {
                all_match = false;
                break;
            };
            let actual: BTreeSet<String> = subs
                .iter()
                .filter_map(|s| s["name"].as_str().map(String::from))
                .collect();
            let raw_len = subs.iter().filter_map(|s| s["name"].as_str()).count();
            if actual != *expected || raw_len != expected.len() {
                all_match = false;
                break;
            }
        }
        if all_match {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for subscriptions on all replicas to converge to {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_replicas_with_divergent_subscription_stanzas_converge_to_union() {
    let binds: Vec<String> = (0..3).map(|_| free_addr()).collect();
    let peers_csv = binds.join(",");
    let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();

    let stanzas = [
        r#"[{"name":"orders","subscriptions":["sub-a","sub-shared"]}]"#,
        r#"[{"name":"orders","subscriptions":["sub-b","sub-shared"]}]"#,
        r#"[{"name":"orders","subscriptions":["sub-shared"]}]"#,
    ];

    let nodes: Vec<Node> = (0..3)
        .map(|i| {
            spawn_node_with(i as u32, &binds[i], dirs[i].path(), &peers_csv, |command| {
                command.env("TAPE_PROVISION_TOPICS", stanzas[i]);
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

    let bases: Vec<String> = nodes.iter().map(|n| n.base_url()).collect();
    let base_slices: Vec<&str> = bases.iter().map(|s| s.as_str()).collect();
    let _leader = wait_leader(&client, &base_slices, "election").await;

    let expected: BTreeSet<String> = ["sub-a", "sub-b", "sub-shared"]
        .into_iter()
        .map(String::from)
        .collect();

    let deadline = Instant::now() + Duration::from_secs(15);
    wait_subscriptions_converged(&client, &nodes, "orders", &expected, deadline).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follower_restart_with_same_stanza_preserves_union_idempotently() {
    let binds: Vec<String> = (0..3).map(|_| free_addr()).collect();
    let peers_csv = binds.join(",");
    let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();

    let stanzas = [
        r#"[{"name":"orders","subscriptions":["sub-a","sub-shared"]}]"#,
        r#"[{"name":"orders","subscriptions":["sub-b","sub-shared"]}]"#,
        r#"[{"name":"orders","subscriptions":["sub-shared"]}]"#,
    ];

    let mut nodes: Vec<Node> = (0..3)
        .map(|i| {
            spawn_node_with(i as u32, &binds[i], dirs[i].path(), &peers_csv, |command| {
                command.env("TAPE_PROVISION_TOPICS", stanzas[i]);
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

    let bases: Vec<String> = nodes.iter().map(|n| n.base_url()).collect();
    let base_slices: Vec<&str> = bases.iter().map(|s| s.as_str()).collect();
    let leader_url = wait_leader(&client, &base_slices, "initial election")
        .await
        .to_string();

    let expected: BTreeSet<String> = ["sub-a", "sub-b", "sub-shared"]
        .into_iter()
        .map(String::from)
        .collect();

    let deadline = Instant::now() + Duration::from_secs(15);
    wait_subscriptions_converged(&client, &nodes, "orders", &expected, deadline).await;

    // Identify a follower node to restart
    let follower_idx = (0..3)
        .find(|&i| nodes[i].base_url() != leader_url)
        .expect("must have at least one follower in 3-replica cluster");

    // Stop the follower process
    let _ = nodes[follower_idx].child.kill();
    let _ = nodes[follower_idx].child.wait();

    let log_file = tempfile::NamedTempFile::new().unwrap();
    let log_path = log_file.path().to_path_buf();

    // Restart the follower with the same data directory, bind, and stanza
    nodes[follower_idx] = spawn_node_with(
        follower_idx as u32,
        &binds[follower_idx],
        dirs[follower_idx].path(),
        &peers_csv,
        |command| {
            let stderr_out = std::fs::File::create(&log_path).unwrap();
            command
                .env("TAPE_PROVISION_TOPICS", stanzas[follower_idx])
                .env("RUST_LOG", "info")
                .stderr(std::process::Stdio::from(stderr_out));
        },
    );

    wait_healthy(
        &client,
        &nodes[follower_idx].base_url(),
        "restarted follower health",
    )
    .await;

    // Verify all three replicas still return the same converged set with no duplicates
    let post_restart_deadline = Instant::now() + Duration::from_secs(15);
    wait_subscriptions_converged(&client, &nodes, "orders", &expected, post_restart_deadline).await;

    // Give a brief moment for any background provisioning logging to flush
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the restarted node logged no failure for the already-existing subscriptions
    let logs = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        !logs.contains("subscription ensure failed"),
        "restarted follower logged a subscription ensure failure: {logs}"
    );
    assert!(
        logs.contains("already_exists"),
        "restarted follower should log idempotent already_exists decision: {logs}"
    );
}
