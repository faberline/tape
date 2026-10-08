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

use serde_json::{json, Value};

use super::crd::{AuthMode, Tape};
use backup::{backup_cron_job, backup_service_account};
use observability::{prometheus_rule, service_monitor};
use service_k8s::render::{self, RenderCtx, ServiceStatefulSet, WorkloadVolumeClaim};
use service_k8s::service::PruneTarget;
use service_k8s::stateful::{
    resource_request_or_default, DEFAULT_CPU_REQUEST, DEFAULT_MEMORY_REQUEST,
};

const APP: &str = "tape";
const MANAGER: &str = "tape-operator";
const API_VERSION: &str = "tape.dev/v1alpha1";
const KIND: &str = "Tape";
/// Public HTTP/1.1 + h2c data/probe port. Raft peers use `RAFT_PORT` when
/// their shared mTLS transport is configured.
const CLIENT_PORT: i32 = 7137;
const RAFT_PORT: i32 = 7138;
const COMPONENT: &str = "server";
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
/// default exists to protect, inverting the invariant. Caught by the Kind gate,
/// which is the only place a missing API group is real. Restoring these two
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

/// Container resources for tape's workload: requests for both CPU and memory,
/// plus a memory limit matching the request (#3051).
///
/// This is `render::requested_resources` plus the memory limit. Requests alone
/// make the pod Burstable, and the kubelet then relieves node memory pressure
/// by picking victims on QoS and usage — so a tape that grows without bound
/// gets its neighbours evicted while it keeps running. Bounding memory makes
/// the OOMKill land on the container that caused it.
///
/// Memory only, no CPU limit: tape is scheduled one pod per node and should be
/// free to use idle CPU, and throttling the persist path would make the very
/// latency this alert watches worse.
///
/// Do not raise the limit in response to an OOMKill. The growth it bounds is
/// the whole-journal rewrite in `AppState::persist`, and #3052 (WAL + group
/// commit) is what removes it.
fn container_resources(cpu: &str, memory: &str) -> Value {
    // Same defaulting as `render::requested_resources`, reused rather than
    // re-spelled: a whitespace-only `spec.cluster.resources.cpu` must fall back
    // to the shared baseline, not render as an unparseable quantity that makes
    // the API server reject the StatefulSet on every reconcile.
    let cpu = resource_request_or_default(cpu, DEFAULT_CPU_REQUEST);
    let memory = resource_request_or_default(memory, DEFAULT_MEMORY_REQUEST);
    json!({
        "requests": { "cpu": cpu, "memory": memory },
        "limits": { "memory": memory },
    })
}

/// The durable serving StatefulSet: the toolkit's downward-API base
/// (`replicas = replicasPerShard` — `shard_count` PINNED to 1, tape is a
/// single raft group; the raft-runtime env quartet + `TAPE_PEER_SERVICE`; the
/// `/data` PVC) hardened with tape's probes, security contexts, and writable
/// `/tmp`.
fn statefulset(tape: &Tape, cx: &RenderCtx, headless: &str) -> Value {
    let s = &tape.spec;
    // Empty values are resolved by core/crates/service-k8s to the shared request-only
    // data-plane baseline (1 CPU / 4Gi); tape owns no resource fallback.
    let cpu = s.cluster.resources.cpu.as_str();
    let memory = s.cluster.resources.memory.as_str();

    // Per-pod durable disk tier: ordered journal plus shared Raft hard state,
    // commit watermark, log, and snapshots on one ReadWriteOnce PVC.
    let mut pvc = json!({
        "metadata": { "name": "data", "labels": cx.labels(COMPONENT) },
        "spec": {
            "accessModes": ["ReadWriteOnce"],
            "resources": { "requests": { "storage": s.storage } },
        },
    });
    if let Some(sc) = &s.storage_class {
        pvc["spec"]["storageClassName"] = json!(sc);
    }

    // tape runtime env layered on top of the downward-API quartet +
    // TAPE_PEER_SERVICE the helper injects: bind-all on the serve port, the
    // /data disk tier, the drain window, and the resolved auth mode.
    //
    // TAPE_AUTH is unconditional and comes from the mode, never from whether a
    // registry source happens to be set (#2765). Deriving it from the source
    // meant `auth: required` with no `tokensSecret` rendered a pod with no
    // TAPE_AUTH at all -- an open data plane produced by a CR that explicitly
    // asked for authentication. Now that same CR starts with
    // TAPE_AUTH=required and no registry file, so it fails startup loudly.
    let mut extra_env = vec![
        json!({ "name": "TAPE_BIND", "value": format!("0.0.0.0:{CLIENT_PORT}") }),
        json!({ "name": "TAPE_RAFT_PORT", "value": RAFT_PORT.to_string() }),
        json!({ "name": "TAPE_DATA_DIR", "value": "/data" }),
        json!({ "name": "TAPE_GRACE_SECS", "value": s.grace_secs.to_string() }),
        json!({ "name": "TAPE_LOG_FORMAT", "value": "json" }),
        json!({ "name": "TAPE_AUTH", "value": s.auth.as_env() }),
    ];
    if let Some(level) = &s.log_level {
        extra_env.push(json!({ "name": "RUST_LOG", "value": level }));
    }
    if let Some(limit) = s.body_limit_bytes {
        extra_env.push(json!({ "name": "TAPE_BODY_LIMIT_BYTES", "value": limit.to_string() }));
    }
    if token_registry_source(tape).is_some() {
        extra_env.push(json!({ "name": "TAPE_TOKEN_REGISTRY_FILE", "value": TOKEN_REGISTRY_FILE }));
    }
    if let Some(seed_uri) = &s.bootstrap_seed_uri {
        extra_env.push(json!({ "name": "TAPE_BOOTSTRAP_SEED_URI", "value": seed_uri }));
    }
    if let Some(topics) = &s.topics {
        if !topics.is_empty() {
            // Compact JSON representation of topic/subscription declarations for the serve path
            let topics_json = serde_json::to_string(topics).expect("topics serialize as JSON");
            extra_env.push(json!({ "name": "TAPE_PROVISION_TOPICS", "value": topics_json }));
        }
    }

    let mut volumes = vec![json!({ "name": "tmp", "emptyDir": {} })];
    let mut volume_mounts = vec![json!({ "name": "tmp", "mountPath": "/tmp" })];
    if let Some(source) = token_registry_source(tape) {
        let projection = render::TokenRegistryProjection {
            volume_name: TOKEN_REGISTRY_VOLUME,
            mount_path: TOKEN_REGISTRY_MOUNT_DIR,
            source,
        };
        volumes.push(render::token_registry_volume(&projection));
        volume_mounts.push(render::token_registry_mount(&projection));
    }

    render::service_statefulset(ServiceStatefulSet {
        cx,
        name: cx.name,
        component: COMPONENT,
        image: s.cluster.image.as_str(),
        image_pull_policy: s
            .cluster
            .image_pull_policy
            .as_deref()
            .unwrap_or("IfNotPresent"),
        command: vec!["tape".into(), "serve".into()],
        args: vec![],
        ports: vec![
            json!({ "name": "http", "containerPort": CLIENT_PORT, "protocol": "TCP" }),
            json!({ "name": "raft", "containerPort": RAFT_PORT, "protocol": "TCP" }),
        ],
        headless_service: headless,
        // tape is a single raft group: shardCount is part of the shared CRD
        // shape but the render pins it to 1 (replicasPerShard is the scale
        // knob; serve's replica_mode() flips HA when it exceeds 1).
        shard_count: 1,
        replicas_per_shard: s.cluster.replicas_per_shard,
        voter_count: s.cluster.voter_count,
        headless_env_key: "TAPE_PEER_SERVICE",
        service_account_name: Some(s.service_account_name.as_deref().unwrap_or(cx.name)),
        env: extra_env,
        env_from: vec![],
        resources: container_resources(cpu, memory),
        pod_annotations: Some(json!({
            "prometheus.io/scrape": "true",
            "prometheus.io/port": CLIENT_PORT.to_string(),
            "prometheus.io/path": "/metrics",
        })),
        pod_security_context: Some(render::restricted_pod_security_context()),
        container_security_context: Some(render::restricted_container_security_context()),
        termination_grace_period_seconds: Some(s.grace_secs),
        readiness_probe: Some(json!({
            "httpGet": { "path": "/readyz", "port": "http" },
            "initialDelaySeconds": 2, "periodSeconds": 5, "timeoutSeconds": 3, "failureThreshold": 60,
        })),
        liveness_probe: Some(json!({
            "httpGet": { "path": "/healthz", "port": "http" },
            "initialDelaySeconds": 5, "periodSeconds": 15, "timeoutSeconds": 5, "failureThreshold": 3,
        })),
        startup_probe: Some(json!({
            "httpGet": { "path": "/healthz", "port": "http" },
            "periodSeconds": 5, "timeoutSeconds": 3, "failureThreshold": 120,
        })),
        lifecycle: None,
        volumes,
        volume_mounts,
        affinity: Some(render::dedicated_node_affinity(cx.selector(COMPONENT))),
        node_selector: None,
        tolerations: vec![],
        topology_spread_constraints: vec![],
        revision_history_limit: Some(5),
        update_strategy: Some(json!({ "type": "RollingUpdate" })),
        volume_claim: Some(WorkloadVolumeClaim {
            name: "data".to_owned(),
            template: pvc,
            mount_path: "/data",
            read_only: false,
        }),
    })
}
