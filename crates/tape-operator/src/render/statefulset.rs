//! The durable serving StatefulSet and its container resources.

use serde_json::{json, Value};
use service_k8s::render::stateful_instance::StatefulInstanceError;
use service_k8s::render::{self, RenderCtx, ServiceStatefulSet, WorkloadVolumeClaim};
use service_k8s::stateful::{
    resource_request_or_default, DEFAULT_CPU_REQUEST, DEFAULT_MEMORY_REQUEST,
};

use super::super::crd::Tape;
use super::placement::{placement_node_selector, placement_tolerations, set_image_pull_secrets};
use super::{
    token_registry_source, CLIENT_PORT, COMPONENT, PEER_TLS_MOUNT_DIR, PEER_TLS_VOLUME, RAFT_PORT,
    TERMINATION_SLACK_SECS, TOKEN_REGISTRY_FILE, TOKEN_REGISTRY_MOUNT_DIR, TOKEN_REGISTRY_VOLUME,
};

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
pub(super) fn statefulset(
    tape: &Tape,
    cx: &RenderCtx,
    headless: &str,
) -> Result<Value, StatefulInstanceError> {
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
    // /data disk tier, the shutdown budget, and the resolved auth mode.
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
        json!({ "name": "TAPE_LOG_FORMAT", "value": s.log_format.as_env() }),
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
    if s.peer_tls.is_some() {
        extra_env.push(json!({ "name": "TAPE_PEER_MTLS", "value": "on" }));
        extra_env.push(json!({ "name": "TAPE_PEER_TLS_CERT", "value": format!("{PEER_TLS_MOUNT_DIR}/tls.crt") }));
        extra_env.push(json!({ "name": "TAPE_PEER_TLS_KEY", "value": format!("{PEER_TLS_MOUNT_DIR}/tls.key") }));
        extra_env.push(
            json!({ "name": "TAPE_PEER_TLS_CA", "value": format!("{PEER_TLS_MOUNT_DIR}/ca.crt") }),
        );
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
    if let Some(peer_tls) = &s.peer_tls {
        volumes.push(json!({
            "name": PEER_TLS_VOLUME,
            "secret": { "secretName": peer_tls.secret_name, "defaultMode": 288 }
        }));
        volume_mounts.push(json!({
            "name": PEER_TLS_VOLUME,
            "mountPath": PEER_TLS_MOUNT_DIR,
            "readOnly": true
        }));
    }

    let mut sts = render::service_statefulset(ServiceStatefulSet {
        cx,
        name: cx.name(),
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
        service_account_name: Some(s.service_account_name.as_deref().unwrap_or(cx.name())),
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
        termination_grace_period_seconds: Some(s.grace_secs + TERMINATION_SLACK_SECS),
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
        node_selector: placement_node_selector(tape),
        tolerations: placement_tolerations(tape),
        topology_spread_constraints: vec![],
        revision_history_limit: Some(5),
        update_strategy: Some(json!({ "type": "RollingUpdate" })),
        volume_claim: Some(WorkloadVolumeClaim {
            name: "data".to_owned(),
            template: pvc,
            mount_path: "/data",
            read_only: false,
        }),
    })?;
    set_image_pull_secrets(&mut sts["spec"]["template"]["spec"], tape);
    Ok(sts)
}

#[cfg(test)]
mod tests {
    use service_k8s::ClusterSpec;

    use crate::{AuthMode, LogFormat, Tape, TapePeerTls, TapeSpec};

    fn spec(peer_tls: Option<TapePeerTls>) -> TapeSpec {
        TapeSpec {
            cluster: ClusterSpec {
                image: "tape:test".into(),
                image_pull_policy: None,
                shard_count: 1,
                replicas_per_shard: 3,
                voter_count: 3,
                resources: Default::default(),
            },
            storage: "10Gi".into(),
            storage_class: None,
            grace_secs: 10,
            log_level: None,
            log_format: LogFormat::default(),
            auth: AuthMode::default(),
            tokens_secret: None,
            tokens_secret_provider_class: None,
            tokens_secret_csi_driver: None,
            bootstrap_seed_uri: None,
            body_limit_bytes: None,
            topics: None,
            observability: false,
            backup: None,
            peer_tls,
            service_account_name: None,
            image_pull_secrets: Vec::new(),
            placement: None,
        }
    }

    fn statefulset_of(peer_tls: Option<TapePeerTls>) -> serde_json::Value {
        let objs = crate::render::render(&Tape::new("tape", spec(peer_tls))).unwrap();
        objs.into_iter()
            .find(|o| o["kind"] == "StatefulSet")
            .expect("render output has a StatefulSet")
    }

    fn env_value(sts: &serde_json::Value, name: &str) -> Option<String> {
        sts["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .map(|e| e["value"].as_str().unwrap().to_owned())
    }

    #[test]
    fn peer_tls_renders_env_volume_and_read_only_mount() {
        let sts = statefulset_of(Some(TapePeerTls {
            secret_name: "tape-peer".into(),
        }));

        assert_eq!(env_value(&sts, "TAPE_PEER_MTLS").as_deref(), Some("on"));
        assert_eq!(
            env_value(&sts, "TAPE_PEER_TLS_CERT").as_deref(),
            Some("/var/run/secrets/tape-peer-tls/tls.crt")
        );
        assert_eq!(
            env_value(&sts, "TAPE_PEER_TLS_KEY").as_deref(),
            Some("/var/run/secrets/tape-peer-tls/tls.key")
        );
        assert_eq!(
            env_value(&sts, "TAPE_PEER_TLS_CA").as_deref(),
            Some("/var/run/secrets/tape-peer-tls/ca.crt")
        );

        let pod = &sts["spec"]["template"]["spec"];
        let vol = pod["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "tape-peer-tls")
            .expect("peer TLS volume");
        assert_eq!(vol["secret"]["secretName"], "tape-peer");
        assert_eq!(vol["secret"]["defaultMode"], 288, "0o440");

        let mount = pod["containers"][0]["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "tape-peer-tls")
            .expect("peer TLS mount");
        assert_eq!(mount["mountPath"], "/var/run/secrets/tape-peer-tls");
        assert_eq!(mount["readOnly"], true);
    }

    #[test]
    fn no_peer_tls_renders_no_peer_wiring() {
        let sts = statefulset_of(None);

        assert_eq!(env_value(&sts, "TAPE_PEER_MTLS"), None);
        for name in [
            "TAPE_PEER_TLS_CERT",
            "TAPE_PEER_TLS_KEY",
            "TAPE_PEER_TLS_CA",
        ] {
            assert_eq!(env_value(&sts, name), None, "{name} must not render");
        }

        let pod = &sts["spec"]["template"]["spec"];
        let has_volume = pod["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "tape-peer-tls");
        assert!(!has_volume, "no peer TLS volume without spec.peerTls");

        let has_mount = pod["containers"][0]["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "tape-peer-tls");
        assert!(!has_mount, "no peer TLS mount without spec.peerTls");
    }
}
