//! Replicas, the Lease holder and the leader metric of the operator_ha suite.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::{kubectl, kubectl_json, try_kubectl, wait_for};
use super::{
    HANDOVER_LIMIT, HANDOVER_TIMEOUT, LEASE, LEASE_TIMEOUT, METRICS_PORT, METRICS_SERVICE,
    OPERATOR_LABEL, POOL_LABEL, RECONCILE_TIMEOUT, REPLICAS_TIMEOUT, SERVER_POOL, SERVICE_TIMEOUT,
    TAPE,
};

#[derive(Debug)]
pub(super) struct OperatorPod {
    pub(super) name: String,
    pub(super) node: String,
    ip: String,
}

/// Waits until at least `want` operator replicas are Ready and none is
/// terminating, and returns them.
pub(super) fn wait_for_ready_replicas(want: usize) -> Vec<OperatorPod> {
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

pub(super) fn is_ready(pod: &Value) -> bool {
    pod["status"]["conditions"].as_array().is_some_and(|all| {
        all.iter()
            .any(|c| c["type"] == "Ready" && c["status"] == "True")
    })
}

/// Co-located replicas would both go in one drain, and the PDB would block
/// it. Each node must also be on pool-tape-server: the drain touches only
/// that pool.
pub(super) fn assert_distinct_server_nodes(pods: &[OperatorPod]) -> Vec<String> {
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

pub(super) fn assert_server_node(node: &str) {
    let node_json = kubectl_json(&["get", "node", node]);
    assert_eq!(
        node_json["metadata"]["labels"][POOL_LABEL], SERVER_POOL,
        "node {node} is not on pool-{SERVER_POOL}"
    );
}

pub(super) fn lease_holder() -> Option<String> {
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
pub(super) fn wait_for_holder(not: Option<&str>) -> String {
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
pub(super) fn sample(body: &str, name: &str) -> Option<String> {
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
pub(super) async fn wait_for_one_leader(http: &reqwest::Client, holder: &str) -> Value {
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
/// an empty body. Bounded retry: a pool-tape-server node can still be
/// re-registering after its scale-up, and a replica on it goes away between
/// the gauge check and this scrape. Each attempt waits for every replica to
/// be Ready again, then needs a scrape through the Service and an endpoint
/// for each Ready replica. Returns the evidence.
pub(super) async fn wait_for_service_scrapeable(http: &reqwest::Client, replicas: usize) -> Value {
    let url = format!("http://{METRICS_SERVICE}:{METRICS_PORT}/metrics");
    let deadline = Instant::now() + SERVICE_TIMEOUT;
    let mut attempts = 0;
    loop {
        attempts += 1;
        let pods = wait_for_ready_replicas(replicas);
        let last = match service_scrape(http, &url, &pods).await {
            Ok(addresses) => {
                return json!({
                    "service": METRICS_SERVICE,
                    "endpoints": addresses,
                    "attempts": attempts,
                });
            }
            Err(last) => last,
        };
        assert!(
            Instant::now() < deadline,
            "AC4: Service/{METRICS_SERVICE}: not scrapeable with an endpoint for each of \
             the {replicas} replicas within {SERVICE_TIMEOUT:?} ({attempts} attempts); \
             last saw: {last}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// One AC4 attempt against the replicas `pods`. Returns the endpoint
/// addresses.
async fn service_scrape(
    http: &reqwest::Client,
    url: &str,
    pods: &[OperatorPod],
) -> Result<Vec<String>, String> {
    let body = scrape(http, url)
        .await
        .map_err(|err| format!("scrape through the Service: {err}"))?;
    if sample(&body, "tape_operator_reconcile_total").is_none() {
        return Err("the Service scrape has no tape_operator_reconcile_total".to_owned());
    }
    let endpoints = try_kubectl(&["get", "endpoints", METRICS_SERVICE, "-o", "json"])
        .map(|out| serde_json::from_str::<Value>(&out).unwrap())?;
    let addresses: Vec<String> = endpoints["subsets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["addresses"].as_array().into_iter().flatten())
        .filter_map(|a| a["ip"].as_str().map(str::to_owned))
        .collect();
    let missing: Vec<&str> = pods
        .iter()
        .filter(|pod| !addresses.contains(&pod.ip))
        .map(|pod| pod.name.as_str())
        .collect();
    if missing.is_empty() {
        Ok(addresses)
    } else {
        Err(format!(
            "endpoints {addresses:?} lack {missing:?}; each replica must be its own scrape target"
        ))
    }
}

/// One heartbeat: change the spec, then wait for the change to reach the
/// child StatefulSet. A status write proves a replica watches; only the
/// child proves one applies, and applying is what a standby takes over.
pub(super) fn reconcile_once(level: &str) -> Result<Duration, String> {
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

pub(super) fn prove_lease_handover(pods: &[OperatorPod], before: &str) -> Value {
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
