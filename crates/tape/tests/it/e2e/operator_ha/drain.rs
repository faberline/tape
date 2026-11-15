//! The heartbeat and the node drain of the operator_ha suite (AC3).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::try_kubectl;
use super::leader::{assert_server_node, lease_holder, reconcile_once, OperatorPod};
use super::{DRAIN_TIMEOUT, MIN_HEARTBEAT_SAMPLES, NEW_HOLDER_TIMEOUT, WORST_HEARTBEAT};

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

pub(super) fn prove_drain_completes(pods: &[OperatorPod], leader: &str) -> Value {
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
