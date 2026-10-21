//! Pure rendering: a [`Tape`] spec → the child Kubernetes objects that
//! realize it. No cluster, no I/O — each object is a self-contained
//! `serde_json::Value` carrying `apiVersion`, `kind`, full `metadata` (labels
//! and owner reference), and `spec`. This is the operator's source of truth and
//! its primary test surface.
//!
//! tape is always a durable StatefulSet (per-pod journal and raft-state PVC),
//! so there is no Deployment branch — single-node is just
//! `replicasPerShard: 1` (no raft env consumed: `replica_mode()` flips HA only
//! when `REPLICAS_PER_SHARD > 1`). The shared [`service_k8s::render`] toolkit
//! supplies the identity, the downward-API StatefulSet (the env
//! `raft_runtime::cluster::ClusterTopology::from_env` consumes), and the
//! Service/PDB/ServiceAccount shapes; tape adds its runtime env, health
//! probes, security hardening, disk tier, and the opt-in token-registry
//! Secret wiring on top.

mod backup;
mod observability;
mod placement;
mod statefulset;

use serde_json::{json, Value};

use super::crd::{AuthMode, Tape};
use backup::{backup_cron_job, backup_service_account};
use observability::{prometheus_rule, service_monitor};
use placement::apply_pod_placement;
use service_k8s::render::{self, RenderCtx};
use service_k8s::service::PruneTarget;
use statefulset::statefulset;

const APP: &str = "tape";
const MANAGER: &str = "tape-operator";
const API_VERSION: &str = "tape.dev/v1alpha1";
const KIND: &str = "Tape";
/// Public HTTP/1.1 + h2c data/probe port. Raft peers use `RAFT_PORT` when
/// their shared mTLS transport is configured.
const CLIENT_PORT: i32 = 7137;
const RAFT_PORT: i32 = 7138;
const COMPONENT: &str = "server";
/// Seconds the kubelet waits beyond `graceSecs` before it SIGKILLs a pod.
/// `graceSecs` is tape's whole shutdown budget, so this slack is what lets
/// the sequence finish and the process exit on its own.
pub const TERMINATION_SLACK_SECS: u64 = 5;
/// Component label for the scheduled-backup CronJob (#2574), kept distinct
/// from `server` so its pods are never selected by the serving Services nor
/// counted against the PDB.
const BACKUP_COMPONENT: &str = "backup";
const TOKEN_REGISTRY_VOLUME: &str = "tape-token-registry";
const TOKEN_REGISTRY_KEY: &str = "token-registry.json";
const TOKEN_REGISTRY_MOUNT_DIR: &str = "/var/run/secrets/tape";
const TOKEN_REGISTRY_FILE: &str = "/var/run/secrets/tape/token-registry.json";

/// Resolve the instance name (defaults to `tape` only when metadata is
/// absent, which never happens for a real CR).
fn instance(tape: &Tape) -> String {
    tape.metadata
        .name
        .clone()
        .unwrap_or_else(|| APP.to_string())
}

/// The shared name of every backup-scoped child: the CronJob, its
/// ServiceAccount, and the CronJob's [`prunes`] target.
///
/// One spelling on purpose. [`prunes`] must name the exact object
/// [`backup_cron_job`] rendered, and a prune that misses by one character is
/// silent — the controller GETs a name that does not exist, finds nothing to
/// delete, and reports success while the real CronJob keeps firing on its old
/// schedule. Deriving both from here makes that class of drift a compile-time
/// impossibility rather than something a test has to notice.
fn backup_child(name: &str) -> String {
    format!("{name}-backup")
}

/// Resolve the namespace (defaults to `default` for unit construction).
fn namespace(tape: &Tape) -> String {
    tape.metadata
        .namespace
        .clone()
        .unwrap_or_else(|| "default".to_string())
}

/// The owner reference that ties a child to its `Tape` CR (cascading GC).
/// Omitted when the CR has no `uid` (only in unit construction).
fn owner_ref(tape: &Tape) -> Option<Value> {
    let uid = tape.metadata.uid.clone()?;
    let name = tape.metadata.name.clone()?;
    Some(render::owner_ref(API_VERSION, KIND, &name, &uid))
}

/// tape's render identity for the shared [`service_k8s::render`] helpers.
fn ctx<'a>(tape: &Tape, name: &'a str, ns: &'a str) -> RenderCtx<'a> {
    RenderCtx {
        app: APP,
        manager: MANAGER,
        api_version: API_VERSION,
        kind: KIND,
        name,
        ns,
        owner: owner_ref(tape),
    }
}

/// Which shared projection source (if any) supplies the token registry file.
/// Both are inactive unless the CR enables required bearer auth.
///
/// There is no precedence branch, and its absence is the point (#2765). The
/// two fields are mutually exclusive by CRD schema, so a spec naming both
/// never came from an API server; rendering *neither* is the safe answer,
/// because it leaves `TAPE_AUTH=required` with no registry file and the pod
/// fails startup naming the problem — instead of quietly serving whichever
/// registry a precedence rule happened to pick while the operator reads the
/// other one.
fn token_registry_source(tape: &Tape) -> Option<render::TokenRegistrySource<'_>> {
    if tape.spec.auth != AuthMode::Required {
        return None;
    }
    match (
        tape.spec.tokens_secret.as_deref(),
        tape.spec.tokens_secret_provider_class.as_deref(),
    ) {
        (Some(name), None) => Some(render::TokenRegistrySource::Secret {
            name,
            key: TOKEN_REGISTRY_KEY,
        }),
        (None, Some(provider_class)) => Some(render::TokenRegistrySource::Csi {
            provider_class,
            driver: tape.spec.tokens_secret_csi_driver.as_deref(),
        }),
        _ => None,
    }
}

/// Render every child object for `tape`, in dependency order (identity first,
/// then the workload + its Services + PDB, then the opt-in observability pair
/// and the optional backup CronJob).
pub fn render(tape: &Tape) -> Vec<Value> {
    let name = instance(tape);
    let ns = namespace(tape);
    let cx = ctx(tape, &name, &ns);
    let headless = format!("{name}-headless");

    let mut objects = Vec::new();
    if tape.spec.service_account_name.is_none() {
        objects.push(render::service_account(&cx, COMPONENT));
    }
    objects.push(statefulset(tape, &cx, &headless));
    objects.push(render::headless_service_with_ports(
        &cx,
        &headless,
        COMPONENT,
        vec![
            json!({ "name": "http", "port": CLIENT_PORT, "targetPort": "http", "protocol": "TCP" }),
            json!({ "name": "raft", "port": RAFT_PORT, "targetPort": "raft", "protocol": "TCP" }),
        ],
    ));
    objects.push(render::client_service(&cx, &name, COMPONENT, CLIENT_PORT));
    // Keep a raft quorum during voluntary disruptions: at most one tape
    // pod may be unavailable at a time.
    objects.push(render::pdb(&cx, &name, COMPONENT, 1));
    objects.push(backup_service_account(&cx));
    if tape.spec.observability {
        objects.push(service_monitor(&cx));
        objects.push(prometheus_rule(&cx));
    }
    if let Some(cron) = backup_cron_job(tape, &cx) {
        objects.push(cron);
    }
    objects
}

/// Children a previous spec rendered that this one no longer wants (#3054).
///
/// The inverse of the `spec.backup` branch in [`render`] above, naming its
/// target through the same [`instance`] / [`backup_child`] helpers the render
/// path resolves its own name with rather than re-spelling it. Removing a
/// backup schedule from the CR must actually stop the CronJob from existing —
/// Server-Side Apply only reconciles fields on objects it is still given, it
/// never deletes an object that stopped being rendered, so without this the
/// CronJob keeps firing on its old schedule forever.
///
/// # Why the observability pair is not pruned
///
/// The `spec.observability` ServiceMonitor/PrometheusRule are the other two
/// conditional children, and naming them here is actively wrong for two
/// independent reasons.
///
/// The disqualifying one is that a `PruneTarget` costs a GET on every requeue,
/// and both are `monitoring.coreos.com/v1` kinds. On a cluster without the
/// Prometheus Operator CRDs that API group is not served at all, so the
/// apiserver answers the GET with a plain-text `404 page not found` rather than
/// a structured `NotFound`; `get_opt` keys off `reason == "NotFound"`, does not
/// recognise it, and propagates it — failing the *entire* reconcile, including
/// the apply and the status write, on a 15s retry loop that never converges.
/// `spec.observability` is default-off precisely so a vanilla cluster stays
/// installable (see its doc comment on `TapeSpec`); pruning on the `false`
/// branch would reach for that API group in exactly the vanilla case the
/// default exists to protect, inverting the invariant. Caught on a live
/// cluster, which is the only place a missing API group is real. Restoring these two
/// targets is blocked on `get_opt` treating a bare 404 as absence (#3079).
///
/// The independent one is that they would not earn it anyway: lumen's
/// `prunes()` draws the line at children whose *presence changes runtime
/// behaviour*, and a stale ServiceMonitor only scrapes a metric nobody reads,
/// where a stale CronJob keeps writing backups to a destination the spec has
/// stopped naming.
///
/// [`backup_service_account`] is likewise never named, for a third reason: it
/// is rendered *unconditionally* (`render`, above) as a stable per-instance
/// identity for Workload Identity annotations, precisely so that identity
/// survives toggling `spec.backup` on and off. Pruning it would break that
/// guarantee in the other direction.
///
/// R4/AC6: this only *names* candidates; it does not itself verify ownership.
/// The controller (`core/crates/service-k8s/src/controller.rs:198` `prune_object`)
/// GETs the live object and only deletes it when a controller `ownerReference`
/// UID matches this CR's UID, warning and skipping otherwise — so a
/// same-named object this CR does not own is never touched. Tape does not
/// reimplement that check here.
pub fn prunes(tape: &Tape) -> Vec<PruneTarget> {
    let mut targets = Vec::new();
    if tape.spec.backup.is_none() {
        targets.push(PruneTarget {
            api_version: "batch/v1",
            kind: "CronJob",
            name: backup_child(&instance(tape)),
        });
    }
    targets
}
