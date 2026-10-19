//! Suite `operator_ha`: the operator's control plane under voluntary
//! disruption. It replaces scripts/kind-operator-ha.sh (#3053 AC1-AC4) on the
//! operator layer of k8s/overlays/gke-test, where each of the two replicas
//! and tape-0 has a node of its own on pool-tape-server.
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    assert_pods_run, endpoint, evidence, kubectl, kubectl_json, try_kubectl, wait_for, Expected,
};

const SUITE: &str = "operator_ha";
const TAPE: &str = "tape";
const OPERATOR_LABEL: &str = "app.kubernetes.io/name=tape-operator";
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
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(90);
/// A warm standby takes the Lease within a few renew periods; slower than
/// this and the second replica is not earning its keep.
const HANDOVER_LIMIT: Duration = Duration::from_secs(90);
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(180);
const DRAIN_TIMEOUT: &str = "240s";
const NEW_HOLDER_TIMEOUT: Duration = Duration::from_secs(180);
const WORST_HEARTBEAT: Duration = Duration::from_secs(75);
const MIN_HEARTBEAT_SAMPLES: usize = 3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn leader_election_survives_a_leader_delete_and_a_node_drain() {
    // The suite talks to the operator, not to tape; the endpoint is still
    // required, so a run outside the test Job fails instead of skipping.
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
            // tape reports no git sha at runtime yet, so this is the
            // workflow's input, tied to the image only through its tag.
            "git_sha": expected.git_sha,
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
    let addresses = assert_service_scrapeable(&http, pods.len()).await;
    evidence(
        SUITE,
        "AC4 metrics Service scrapeable",
        json!({ "service": METRICS_SERVICE, "endpoints": addresses }),
    );

    wait_for(
        &format!("StatefulSet/{TAPE} from the operator"),
        LEASE_TIMEOUT,
        || try_kubectl(&["get", "statefulset", TAPE]).map(drop),
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

#[derive(Debug)]
struct OperatorPod {
    name: String,
    node: String,
    ip: String,
}

/// Waits until at least `want` operator replicas are Ready and none is
/// terminating, and returns them.
fn wait_for_ready_replicas(want: usize) -> Vec<OperatorPod> {
    wait_for(
        &format!("{want} Ready operator replicas"),
        REPLICAS_TIMEOUT,
        || {
            let pods: Value = try_kubectl(&["get", "pods", "-l", OPERATOR_LABEL, "-o", "json"])
                .map(|out| serde_json::from_str(&out).unwrap())?;
            let all = pods["items"].as_array().cloned().unwrap_or_default();
            let ready: Vec<OperatorPod> = all
                .iter()
                .filter(|p| p["metadata"]["deletionTimestamp"].is_null() && is_ready(p))
                .map(|p| OperatorPod {
                    name: p["metadata"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    node: p["spec"]["nodeName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    ip: p["status"]["podIP"].as_str().unwrap_or_default().to_owned(),
                })
                .collect();
            if ready.len() >= want && ready.len() == all.len() {
                Ok(ready)
            } else {
                Err(format!("{} Ready of {} pods", ready.len(), all.len()))
            }
        },
    )
}

fn is_ready(pod: &Value) -> bool {
    pod["status"]["conditions"].as_array().is_some_and(|all| {
        all.iter()
            .any(|c| c["type"] == "Ready" && c["status"] == "True")
    })
}

/// Co-located replicas would both go in one drain, and the PDB would block
/// it. Each node must also be on pool-tape-server: the drain touches only
/// that pool.
fn assert_distinct_server_nodes(pods: &[OperatorPod]) -> Vec<String> {
    let mut nodes: Vec<String> = pods.iter().map(|p| p.node.clone()).collect();
    nodes.sort();
    nodes.dedup();
    assert_eq!(
        nodes.len(),
        pods.len(),
        "operator replicas share a node: {pods:?}"
    );
    for node in &nodes {
        assert_server_node(node);
    }
    nodes
}

fn assert_server_node(node: &str) {
    let node_json = kubectl_json(&["get", "node", node]);
    assert_eq!(
        node_json["metadata"]["labels"][POOL_LABEL], SERVER_POOL,
        "node {node} is not on pool-{SERVER_POOL}"
    );
}

fn lease_holder() -> Option<String> {
    try_kubectl(&[
        "get",
        "lease",
        LEASE,
        "-o",
        "jsonpath={.spec.holderIdentity}",
    ])
    .ok()
    .map(|out| out.trim().to_owned())
    .filter(|holder| !holder.is_empty())
}

/// Waits for a Lease holder other than `not`, and returns it.
fn wait_for_holder(not: Option<&str>) -> String {
    wait_for("a Lease holder", LEASE_TIMEOUT, || match lease_holder() {
        Some(holder) if Some(holder.as_str()) != not => Ok(holder),
        other => Err(format!("holder {other:?}")),
    })
}

/// Scrapes one replica directly, by pod IP.
async fn scrape(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = http.get(url).send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("{url}: {status}"))
    }
}

/// The last sample of an unlabelled or labelled series `name`.
fn sample(body: &str, name: &str) -> Option<String> {
    body.lines()
        .filter(|line| {
            line.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('{'))
        })
        .filter_map(|line| line.split_whitespace().next_back())
        .next_back()
        .map(str::to_owned)
}

/// Two sources must agree: the Lease, and each replica's own gauge. The
/// Lease alone would pass with a replica that believes it leads without
/// holding it. Returns each replica's gauge.
async fn wait_for_one_leader(http: &reqwest::Client, holder: &str) -> Value {
    let deadline = Instant::now() + LEASE_TIMEOUT;
    loop {
        let pods = wait_for_ready_replicas(2);
        let mut gauges = serde_json::Map::new();
        let mut last = None;
        for pod in &pods {
            let url = format!("http://{}:{METRICS_PORT}/metrics", pod.ip);
            match scrape(http, &url).await {
                Ok(body) => match sample(&body, "tape_operator_leader") {
                    Some(value) => {
                        gauges.insert(pod.name.clone(), json!(value));
                    }
                    None => last = Some(format!("{} has no tape_operator_leader", pod.name)),
                },
                Err(err) => last = Some(err),
            }
        }
        let leaders: Vec<&String> = gauges
            .iter()
            .filter(|(_, v)| v.as_str() == Some("1"))
            .map(|(k, _)| k)
            .collect();
        if last.is_none() && leaders.len() == 1 && leaders[0] == holder {
            return Value::Object(gauges);
        }
        let seen = last.unwrap_or_else(|| format!("gauges {gauges:?}, Lease holder {holder}"));
        assert!(
            Instant::now() < deadline,
            "exactly one replica reporting tape_operator_leader 1, the Lease holder: \
             not reached within {LEASE_TIMEOUT:?}; last saw: {seen}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// A Service whose selector matches nothing gives a connection error, not
/// an empty body. Returns the endpoint addresses.
async fn assert_service_scrapeable(http: &reqwest::Client, replicas: usize) -> Vec<String> {
    let url = format!("http://{METRICS_SERVICE}:{METRICS_PORT}/metrics");
    let body = scrape(http, &url)
        .await
        .unwrap_or_else(|err| panic!("AC4: scrape through the Service: {err}"));
    assert!(
        sample(&body, "tape_operator_reconcile_total").is_some(),
        "AC4: the Service scrape has no tape_operator_reconcile_total"
    );
    let endpoints = kubectl_json(&["get", "endpoints", METRICS_SERVICE]);
    let addresses: Vec<String> = endpoints["subsets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["addresses"].as_array().into_iter().flatten())
        .filter_map(|a| a["ip"].as_str().map(str::to_owned))
        .collect();
    assert!(
        addresses.len() >= replicas,
        "AC4: Service/{METRICS_SERVICE} has endpoints {addresses:?}; each of the \
         {replicas} replicas must be its own scrape target"
    );
    addresses
}

/// One heartbeat: change the spec, then wait for the change to reach the
/// child StatefulSet. A status write proves a replica watches; only the
/// child proves one applies, and applying is what a standby takes over.
fn reconcile_once(level: &str) -> Result<Duration, String> {
    let start = Instant::now();
    let patch = json!({ "spec": { "logLevel": level } }).to_string();
    try_kubectl(&["patch", "tape", TAPE, "--type=merge", "-p", &patch])?;
    let mut last = String::new();
    while start.elapsed() < RECONCILE_TIMEOUT {
        match try_kubectl(&["get", "statefulset", TAPE, "-o", "json"]) {
            Ok(out) => {
                let statefulset: Value = serde_json::from_str(&out).unwrap();
                let got = statefulset["spec"]["template"]["spec"]["containers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|c| c["env"].as_array().into_iter().flatten())
                    .find(|e| e["name"] == "RUST_LOG")
                    .and_then(|e| e["value"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                if got == level {
                    return Ok(start.elapsed());
                }
                last = format!("RUST_LOG {got:?}");
            }
            Err(err) => last = err,
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err(format!(
        "logLevel {level} did not reach StatefulSet/{TAPE} within {RECONCILE_TIMEOUT:?}; last saw {last}"
    ))
}

fn prove_lease_handover(pods: &[OperatorPod], before: &str) -> Value {
    // Recorded before the delete: after it, the Deployment adds a
    // replacement, and "the other pod" is no longer one pod. A warm standby
    // taking over in seconds is what two replicas buy; a fresh replacement
    // winning the race is not.
    let standby = pods
        .iter()
        .map(|p| p.name.clone())
        .find(|name| name != before);
    let start = Instant::now();
    kubectl(&["delete", "pod", before, "--wait=false"]);
    let after = wait_for(
        &format!("a Lease holder other than {before}"),
        HANDOVER_TIMEOUT,
        || match lease_holder() {
            Some(holder) if holder != before => Ok(holder),
            other => Err(format!("holder {other:?}")),
        },
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed <= HANDOVER_LIMIT,
        "AC2: {elapsed:?} to take the Lease from {before}; the standby exists to make this fast"
    );
    let applied = reconcile_once("debug")
        .unwrap_or_else(|err| panic!("AC2: new leader {after} did not apply a spec change: {err}"));
    json!({
        "leader_before": before,
        "standby": standby,
        "leader_after": after,
        "standby_took_over": standby.as_deref() == Some(after.as_str()),
        "handover_secs": elapsed.as_secs(),
        "apply_secs": applied.as_secs(),
    })
}

#[derive(Debug, Clone)]
struct Sample {
    n: u64,
    holder: Option<String>,
    result: Result<u64, String>,
}

/// The heartbeat runs on its own thread while the main thread drains, and
/// records every sample with the Lease holder it saw when the sample began.
struct Heartbeat {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<Sample>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let stop = Arc::clone(&stop);
            let samples = Arc::clone(&samples);
            std::thread::spawn(move || {
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    n += 1;
                    let level = if n % 2 == 0 { "debug" } else { "info" };
                    let holder = lease_holder();
                    let result = reconcile_once(level).map(|d| d.as_secs());
                    samples.lock().unwrap().push(Sample { n, holder, result });
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
        };
        Self {
            stop,
            samples,
            thread: Some(thread),
        }
    }

    fn samples(&self) -> Vec<Sample> {
        self.samples.lock().unwrap().clone()
    }

    fn stop(mut self) -> Vec<Sample> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("heartbeat thread panicked");
        }
        self.samples()
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Uncordons the drained node on every exit, so a failed run leaves the pool
/// usable until teardown.
struct Cordoned(String);

impl Drop for Cordoned {
    fn drop(&mut self) {
        if let Err(err) = try_kubectl(&["uncordon", &self.0]) {
            eprintln!("uncordon {}: {err}", self.0);
        }
    }
}

fn prove_drain_completes(pods: &[OperatorPod], leader: &str) -> Value {
    let node = pods
        .iter()
        .find(|p| p.name == leader)
        .map(|p| p.node.clone())
        .unwrap_or_else(|| panic!("Lease holder {leader} is not a Ready replica: {pods:?}"));
    // The drain evicts every pod on the node; it must be one of tape's.
    assert_server_node(&node);
    let heartbeat = Heartbeat::start();
    let cordoned = Cordoned(node.clone());
    let drain_start = Instant::now();
    // The timeout is the assertion: with 2 replicas on distinct nodes and
    // maxUnavailable: 1 the drain completes. A wrong PDB or co-located
    // replicas make the eviction API refuse until it runs out.
    let drained = try_kubectl(&[
        "drain",
        &node,
        "--ignore-daemonsets",
        "--delete-emptydir-data",
        "--force",
        &format!("--timeout={DRAIN_TIMEOUT}"),
    ]);
    let drain_secs = drain_start.elapsed().as_secs();
    if let Err(err) = drained {
        let samples = heartbeat.stop();
        panic!("AC3: drain of {node} did not complete: {err}; heartbeat {samples:?}");
    }

    // Keep the heartbeat going until a sample began under a new holder. The
    // handover evidence and the continuity evidence come from the same
    // samples, or they are claims about two different windows.
    let returned = Instant::now();
    let new_holder = loop {
        let samples = heartbeat.samples();
        if let Some(holder) = samples
            .iter()
            .filter_map(|s| s.holder.as_deref())
            .find(|h| *h != leader)
        {
            break holder.to_owned();
        }
        if returned.elapsed() > NEW_HOLDER_TIMEOUT {
            let samples = heartbeat.stop();
            panic!(
                "AC3: no surviving replica reconciled as a new Lease holder within \
                 {NEW_HOLDER_TIMEOUT:?} of the drain returning (was {leader}, Lease now \
                 {:?}); heartbeat {samples:?}",
                lease_holder()
            );
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    let after_drain_secs = returned.elapsed().as_secs();
    let samples = heartbeat.stop();

    let failures: Vec<&Sample> = samples.iter().filter(|s| s.result.is_err()).collect();
    assert!(
        failures.is_empty(),
        "AC3: heartbeat samples that never converged: {failures:?}"
    );
    assert!(
        samples.len() >= MIN_HEARTBEAT_SAMPLES,
        "AC3: only {} heartbeat samples; the window is too short to mean anything: {samples:?}",
        samples.len()
    );
    let worst = samples
        .iter()
        .filter_map(|s| s.result.as_ref().ok().copied())
        .max()
        .unwrap_or_default();
    assert!(
        worst <= WORST_HEARTBEAT.as_secs(),
        "AC3: worst reconcile latency {worst}s exceeds {WORST_HEARTBEAT:?}: {samples:?}"
    );
    // Reconciliation is leader-gated, so continuity under an unmoved holder
    // only says the surviving replica served every sample by itself.
    let mut holders: Vec<&str> = samples.iter().filter_map(|s| s.holder.as_deref()).collect();
    holders.dedup();
    let mut distinct = holders.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() >= 2,
        "AC3: the Lease holder never changed during the drain ({distinct:?}); this run did \
         not exercise a handover"
    );

    drop(cordoned);
    json!({
        "node": node,
        "leader_before": leader,
        "leader_after": new_holder,
        "drain_secs": drain_secs,
        "new_holder_secs_after_drain": after_drain_secs,
        "samples": samples.len(),
        "last_sample": samples.last().map(|s| s.n),
        "worst_reconcile_secs": worst,
        "holders_in_order": holders,
    })
}

#[test]
fn sample_reads_the_last_value_of_a_series() {
    let body = "# HELP tape_operator_leader x\n\
                tape_operator_leader 0\n\
                tape_operator_leader_changes_total 4\n\
                tape_operator_reconcile_total{result=\"ok\"} 7\n\
                tape_operator_leader 1\n";
    assert_eq!(sample(body, "tape_operator_leader").as_deref(), Some("1"));
    assert_eq!(
        sample(body, "tape_operator_reconcile_total").as_deref(),
        Some("7")
    );
    assert_eq!(sample(body, "tape_operator_missing"), None);
}
