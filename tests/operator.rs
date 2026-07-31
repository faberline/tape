// HANDWRITE-BEGIN gap="missing-generator:unit-test:c19f401a" tracker="pending-tracker" reason="Feature-gated (operator) render-shape tests: CRD flattens ClusterSpec + tape knobs; render() emits the downward-API StatefulSet with the exact env/probe contract serve reads, plus ServiceAccount/Services/PDB; auth Secret wiring is opt-in; status_patch phases Pending/Reconciling/Ready."
//! Operator-adoption render-shape tests (#1328). Compiled only with
//! `--features operator`.
//!
//! - R4: `TapeSpec` flattens `service_k8s::ClusterSpec` into the CRD schema.
//! - R5: `render` emits the downward-API StatefulSet with the exact env/probe
//!   contract tape's serve reads, plus ServiceAccount/Services/PDB.
//! - R6: the token-registry Secret wiring is opt-in (relay/lumen's pattern).
//! - R7: `status_patch` reports Pending/Reconciling/Ready.
#![cfg(feature = "operator")]

use std::collections::HashMap;

use serde_json::Value;
use service_k8s::{ClusterSpec, ConditionStatus, ManagedService, ReadyFacts};
use tape::operator::crd::LogFormat;
use tape::operator::render::{prunes, render};
use tape::operator::{crd_yaml, AuthMode, Tape, TapeBackupSpec, TapeSpec};

fn spec(replicas: u32) -> TapeSpec {
    TapeSpec {
        cluster: ClusterSpec {
            image: "tape:test".into(),
            image_pull_policy: None,
            shard_count: 1,
            replicas_per_shard: replicas,
            voter_count: replicas,
            resources: Default::default(),
        },
        storage: "10Gi".into(),
        storage_class: None,
        grace_secs: 10,
        log_level: None,
        log_format: LogFormat::default(),
        // Not a literal: the fixture must track the real default, so a future
        // flip of `AuthMode::default()` shows up in every render test at once
        // instead of leaving them asserting a shape no user ever gets (#2765).
        auth: AuthMode::default(),
        tokens_secret: None,
        tokens_secret_provider_class: None,
        tokens_secret_csi_driver: None,
        bootstrap_seed_uri: None,
        body_limit_bytes: None,
        topics: None,
        observability: false,
        backup: None,
        service_account_name: None,
    }
}

fn of_kind<'a>(objs: &'a [Value], kind: &str) -> &'a Value {
    objs.iter()
        .find(|o| o["kind"] == kind)
        .unwrap_or_else(|| panic!("render output has no {kind}"))
}

fn env_of(sts: &Value) -> Vec<(&str, &Value)> {
    sts["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array()
        .expect("container env array")
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e))
        .collect()
}

/// R4 — the flattened `ClusterSpec` fields sit directly under the CRD's
/// `spec` schema (no `cluster` wrapper), so a `Tape` CR declares them inline.
#[test]
fn crd_flattens_cluster_spec() {
    let yaml = crd_yaml();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("CRD parses as YAML");
    let props = &doc["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
        ["properties"];
    for field in [
        "image",
        "imagePullPolicy",
        "shardCount",
        "replicasPerShard",
        "voterCount",
        "resources",
    ] {
        assert!(
            props.get(field).is_some(),
            "CRD spec schema must carry flattened field `{field}`"
        );
    }
    // Flatten merges properties → there is no nested `cluster` wrapper.
    assert!(
        props.get("cluster").is_none(),
        "ClusterSpec must be flattened, not nested under `cluster`"
    );
    // tape's own knobs are present too.
    for field in [
        "storage",
        "storageClass",
        "graceSecs",
        "logLevel",
        "logFormat",
        "auth",
        "tokensSecret",
        "tokensSecretProviderClass",
        "tokensSecretCsiDriver",
        "bootstrapSeedUri",
    ] {
        assert!(props.get(field).is_some(), "missing tape knob `{field}`");
    }
    // R3 — Kubernetes structural-schema compatible.
    assert!(
        !yaml.contains("uint32"),
        "CRD must not carry format: uint32"
    );
    assert!(
        !yaml.contains("uint64"),
        "CRD must not carry format: uint64"
    );
    assert!(
        yaml.contains("minimum"),
        "normalized uints keep a minimum floor"
    );
    // #2582 — `logFormat` is a closed enum defaulting to json.
    assert_eq!(props["logFormat"]["default"], "json");
    assert_eq!(
        props["logFormat"]["enum"]
            .as_sequence()
            .expect("logFormat carries a closed enum"),
        &vec![
            serde_yaml::Value::from("pretty"),
            serde_yaml::Value::from("json"),
        ],
    );
    assert_eq!(
        props["logFormat"]["type"], "string",
        "logFormat enum type is string"
    );
    // #2765 — `auth` is a closed enum defaulting to the required state. As an
    // unconstrained string, every value except the exact literal `"required"`
    // rendered an open data plane, so `auth: requried` applied cleanly and the
    // only signal was the absence of an env var in a pod nobody inspects.
    assert_eq!(props["auth"]["default"], "required");
    assert_eq!(
        props["auth"]["enum"]
            .as_sequence()
            .expect("auth carries a closed enum"),
        &vec![
            serde_yaml::Value::from("disabled"),
            serde_yaml::Value::from("required"),
        ],
    );
    assert_eq!(
        props["auth"]["type"], "string",
        "the off-state is spelled `disabled`, so nothing in this schema is \
         YAML 1.1 boolean-like and the enum can never be coerced to a bool"
    );
    assert_eq!(
        include_str!("../k8s/operator/crd.yaml"),
        yaml,
        "checked-in CRD must be regenerated from the renderer"
    );
}

// <HANDWRITE gap="missing-generator:kubernetes-peer-port-test" tracker="#1805" reason="kubernetes-peer-port-test section in operator.rs is hand-written pending codegen support">
/// R5 — the rendered StatefulSet carries exactly the downward-API env tape's
/// serve reads, the right replica count (single group), tape's runtime env +
/// disk tier, the probe contract, and the sibling child objects.
#[test]
fn render_emits_expected_child_objects() {
    let tape = Tape::new("tape", spec(3));
    let objs = render(&tape);

    let sts = of_kind(&objs, "StatefulSet");
    // Single raft group: shardCount pinned to 1 → replicas == replicasPerShard.
    assert_eq!(sts["spec"]["replicas"], 3, "replicas = replicasPerShard");
    assert_eq!(sts["spec"]["serviceName"], "tape-headless");
    assert_eq!(sts["spec"]["podManagementPolicy"], "Parallel");

    let env = env_of(sts);
    let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
    // The exact contract serve reads: raft_runtime::cluster (quartet) +
    // --peer-service (TAPE_PEER_SERVICE) + public/raft binds + data-dir/grace.
    for k in [
        "POD_NAME",
        "SHARD_COUNT",
        "REPLICAS_PER_SHARD",
        "VOTER_COUNT",
        "TAPE_PEER_SERVICE",
        "TAPE_BIND",
        "TAPE_RAFT_PORT",
        "TAPE_DATA_DIR",
        "TAPE_GRACE_SECS",
    ] {
        assert!(keys.contains(&k), "missing env {k}");
    }
    let get = |k: &str| env.iter().find(|(n, _)| *n == k).unwrap().1;
    assert_eq!(
        get("POD_NAME")["valueFrom"]["fieldRef"]["fieldPath"],
        "metadata.name"
    );
    assert_eq!(get("SHARD_COUNT")["value"], "1", "tape is a single group");
    assert_eq!(get("REPLICAS_PER_SHARD")["value"], "3");
    assert_eq!(get("VOTER_COUNT")["value"], "3");
    assert_eq!(get("TAPE_PEER_SERVICE")["value"], "tape-headless");
    assert_eq!(get("TAPE_BIND")["value"], "0.0.0.0:7137");
    assert_eq!(get("TAPE_RAFT_PORT")["value"], "7138");
    assert_eq!(get("TAPE_DATA_DIR")["value"], "/data");

    // Probe contract on the serve port: /readyz readiness, /healthz liveness
    // + startup — what service-http's standard probe routes answer.
    let container = &sts["spec"]["template"]["spec"]["containers"][0];
    let pod = &sts["spec"]["template"]["spec"];
    assert_eq!(container["readinessProbe"]["httpGet"]["path"], "/readyz");
    assert_eq!(container["livenessProbe"]["httpGet"]["path"], "/healthz");
    assert_eq!(container["startupProbe"]["httpGet"]["path"], "/healthz");
    assert_eq!(container["ports"][0]["containerPort"], 7137);
    assert_eq!(container["ports"][1]["name"], "raft");
    assert_eq!(container["ports"][1]["containerPort"], 7138);
    assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(container["resources"]["requests"]["cpu"], "1");
    assert_eq!(container["resources"]["requests"]["memory"], "4Gi");
    // #3051: memory limit is present and equals the request for attributed OOMKill.
    assert!(container["resources"]["limits"].is_object());
    assert_eq!(container["resources"]["limits"]["memory"], "4Gi");
    // No CPU limit; Burstable QoS only bounds memory.
    assert!(container["resources"]["limits"].get("cpu").is_none());
    assert_eq!(sts["spec"]["revisionHistoryLimit"], 5);
    assert_eq!(sts["spec"]["updateStrategy"]["type"], "RollingUpdate");
    assert_eq!(
        sts["spec"]["template"]["metadata"]["annotations"]["prometheus.io/path"],
        "/metrics"
    );
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        pod["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(
        pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"][0]
            ["topologyKey"],
        "kubernetes.io/hostname"
    );

    // Durable disk tier: the /data PVC carries the CR's storage size.
    assert_eq!(
        sts["spec"]["volumeClaimTemplates"][0]["spec"]["resources"]["requests"]["storage"],
        "10Gi"
    );
    let mounts = container["volumeMounts"].as_array().unwrap();
    let data_mount = mounts
        .iter()
        .find(|mount| mount["name"] == "data")
        .expect("data PVC mount");
    assert_eq!(data_mount["mountPath"], "/data");
    assert!(pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|volume| volume["name"] == "tmp" && volume["emptyDir"] == serde_json::json!({})));

    // The rest of the child set is present.
    assert_eq!(of_kind(&objs, "ServiceAccount")["metadata"]["name"], "tape");
    let headless = objs
        .iter()
        .find(|o| o["kind"] == "Service" && o["spec"]["clusterIP"] == "None")
        .expect("headless service");
    assert_eq!(headless["metadata"]["name"], "tape-headless");
    assert_eq!(headless["spec"]["ports"][0]["port"], 7137);
    assert_eq!(headless["spec"]["ports"][1]["name"], "raft");
    assert_eq!(headless["spec"]["ports"][1]["port"], 7138);
    let client = objs
        .iter()
        .find(|o| o["kind"] == "Service" && o["spec"]["type"] == "ClusterIP")
        .expect("client service");
    assert_eq!(client["spec"]["ports"][0]["port"], 7137);
    assert_eq!(client["spec"]["ports"].as_array().unwrap().len(), 1);
    assert_eq!(
        of_kind(&objs, "PodDisruptionBudget")["spec"]["maxUnavailable"],
        1
    );
}
// </HANDWRITE>

/// R6 — TAPE_AUTH always states the resolved mode; TAPE_TOKEN_REGISTRY_FILE +
/// the volume render only when the CR also names a source.
///
/// The env var used to be derived from whether a source was set, which made
/// `auth: required` with no `tokensSecret` render a pod with no TAPE_AUTH at
/// all — an open data plane produced by a CR that explicitly asked for
/// authentication (#2765).
#[test]
fn token_registry_secret_wiring_is_opt_in() {
    // Default CR: authentication required, no registry file. This pod fails
    // startup naming the field to set, which is the whole point of the flip —
    // the previous default served an open API and said nothing.
    let plain = Tape::new("tape", spec(1));
    let objs = render(&plain);
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);
    let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        env.iter().find(|(n, _)| *n == "TAPE_AUTH").unwrap().1["value"],
        "required",
        "omitting spec.auth must render authentication required"
    );
    assert!(!keys.contains(&"TAPE_TOKEN_REGISTRY_FILE"));
    let vols = sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap();
    assert!(
        vols.iter().all(|v| v["name"] != "tape-token-registry"),
        "no token-registry volume without tokensSecret"
    );

    // The explicit opt-out. Spelled `disabled` in the CR, `off` on the wire —
    // both halves pinned here so neither spelling can drift into the other
    // (a bare `off` in the CRD is read by YAML 1.1 as the boolean `false`).
    let mut open = spec(1);
    open.auth = AuthMode::Off;
    let objs = render(&Tape::new("tape", open));
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);
    assert_eq!(
        env.iter().find(|(n, _)| *n == "TAPE_AUTH").unwrap().1["value"],
        "off",
        "the serving process's TAPE_AUTH keeps taking `off`, not `disabled`"
    );
    assert_eq!(AuthMode::Off.as_env(), "off");
    assert!(!env.iter().any(|(n, _)| *n == "TAPE_TOKEN_REGISTRY_FILE"));

    // auth: required + tokensSecret: env + read-only Secret mount.
    let mut secured = spec(3);
    secured.auth = AuthMode::Required;
    secured.tokens_secret = Some("tape-token-registry".into());
    let objs = render(&Tape::new("tape", secured));
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);
    let get = |k: &str| env.iter().find(|(n, _)| *n == k).unwrap().1;
    assert_eq!(get("TAPE_AUTH")["value"], "required");
    assert_eq!(
        get("TAPE_TOKEN_REGISTRY_FILE")["value"],
        "/var/run/secrets/tape/token-registry.json"
    );
    let vols = sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap();
    let vol = vols
        .iter()
        .find(|v| v["name"] == "tape-token-registry")
        .expect("token-registry volume");
    assert_eq!(vol["secret"]["secretName"], "tape-token-registry");
    let mounts = sts["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
        .as_array()
        .unwrap();
    let mount = mounts
        .iter()
        .find(|m| m["name"] == "tape-token-registry")
        .expect("token-registry mount");
    assert_eq!(mount["mountPath"], "/var/run/secrets/tape");
    assert_eq!(mount["readOnly"], true);

    // auth: required + CSI provider class: same process contract, but no
    // Kubernetes Secret object; the shared projection helper emits the CSI
    // volume exactly once.
    let mut csi = spec(3);
    csi.auth = AuthMode::Required;
    csi.tokens_secret_provider_class = Some("tape-registry-csi".into());
    let objs = render(&Tape::new("tape", csi));
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);
    let get = |k: &str| env.iter().find(|(n, _)| *n == k).unwrap().1;
    assert_eq!(get("TAPE_AUTH")["value"], "required");
    assert_eq!(
        get("TAPE_TOKEN_REGISTRY_FILE")["value"],
        "/var/run/secrets/tape/token-registry.json"
    );
    let vol = sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "tape-token-registry")
        .expect("CSI token-registry volume");
    assert_eq!(vol["csi"]["driver"], "secrets-store.csi.k8s.io");
    assert_eq!(
        vol["csi"]["volumeAttributes"]["secretProviderClass"],
        "tape-registry-csi"
    );

    // Naming both sources renders NEITHER — the replacement for the old
    // "tokensSecret wins" precedence (#2765). Such a spec is rejected at
    // `kubectl apply` by the CRD's CEL rule, so one that reaches the renderer
    // never came from an API server. Rendering nothing leaves
    // TAPE_AUTH=required with no registry file, so the pod fails startup
    // instead of quietly serving whichever registry the precedence picked
    // while the operator reads the other one.
    let mut both = spec(3);
    both.auth = AuthMode::Required;
    both.tokens_secret = Some("tape-registry-secret".into());
    both.tokens_secret_provider_class = Some("tape-registry-csi".into());
    let objs = render(&Tape::new("tape", both));
    let sts = of_kind(&objs, "StatefulSet");
    assert!(
        sts["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["name"] != "tape-token-registry"),
        "an ambiguous spec must render no registry at all, not an arbitrary one"
    );
    assert!(
        !env_of(sts)
            .iter()
            .any(|(n, _)| *n == "TAPE_TOKEN_REGISTRY_FILE"),
        "no registry file path without an unambiguous source"
    );
}

/// #2765 — the CRD rejects an ambiguous token source at `kubectl apply`, and
/// the rule is spelled with presence tests only.
///
/// The `!= null` shape a nullable field seems to want does not compile at the
/// API server ("found no matching overload for '_!=_' applied to '(string,
/// null)'"), producing a CRD that passes every local test and installs on no
/// cluster. Lumen shipped exactly that once. Asserting the rule's TEXT here is
/// the cheap half; `kubectl apply --dry-run=server` is the half that actually
/// proves it.
#[test]
fn crd_rejects_naming_both_token_sources() {
    let yaml = crd_yaml();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("CRD parses as YAML");
    let rules = doc["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
        ["x-kubernetes-validations"]
        .as_sequence()
        .expect("spec carries validation rules");
    let rule = rules
        .iter()
        .find(|r| {
            r["rule"]
                .as_str()
                .is_some_and(|s| s.contains("tokensSecretProviderClass"))
        })
        .expect("a rule covering the two token sources");
    assert_eq!(
        rule["rule"],
        "!(has(self.tokensSecret) && has(self.tokensSecretProviderClass))"
    );
    assert!(
        !rule["rule"].as_str().unwrap().contains("null"),
        "CEL rules on nullable fields take presence tests only; a `!= null` \
         guard fails compilation at the API server and installs on no cluster"
    );
    assert!(
        rule["message"]
            .as_str()
            .is_some_and(|m| m.contains("at most one")),
        "the rejection must name the remedy, not just the violation"
    );
}

/// #2765 — omitting `spec.auth` means authentication is required.
///
/// The direct analogue of lumen's `auth_defaults_to_required`: the other way
/// round, forgetting the field ships an open cluster and nothing says so.
#[test]
fn auth_defaults_to_required() {
    assert_eq!(AuthMode::default(), AuthMode::Required);
    let parsed: TapeSpec = serde_json::from_value(serde_json::json!({
        "image": "tape:test",
        "storage": "10Gi",
    }))
    .expect("a spec omitting auth parses");
    assert_eq!(parsed.auth, AuthMode::Required);

    // And a typo is not silently the open state — it is not a state at all.
    let typo: Result<TapeSpec, _> = serde_json::from_value(serde_json::json!({
        "image": "tape:test",
        "storage": "10Gi",
        "auth": "requried",
    }));
    assert!(
        typo.is_err(),
        "an unknown auth value must be rejected, never read as disabled"
    );
}

/// GKE's managed Secrets Store add-on registers a different CSI driver name
/// than the community default (#2456/#2457); an explicit
/// `tokensSecretCsiDriver` must render that name on the pod volume.
#[test]
fn tokens_secret_csi_driver_overrides_the_default_csi_driver_name() {
    let mut csi = spec(3);
    csi.auth = AuthMode::Required;
    csi.tokens_secret_provider_class = Some("tape-registry-csi".into());
    csi.tokens_secret_csi_driver = Some("secrets-store-gke.csi.k8s.io".into());
    let objs = render(&Tape::new("tape", csi));
    let sts = of_kind(&objs, "StatefulSet");
    let vol = sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "tape-token-registry")
        .expect("CSI token-registry volume");
    assert_eq!(vol["csi"]["driver"], "secrets-store-gke.csi.k8s.io");
    assert_eq!(
        vol["csi"]["volumeAttributes"]["secretProviderClass"],
        "tape-registry-csi"
    );
}

/// The optional cold-recovery seed stays absent by default and projects to the
/// exact serve environment only when the CR intentionally requests it.
#[test]
fn bootstrap_seed_uri_wiring_is_opt_in() {
    let plain = Tape::new("tape", spec(3));
    let plain_objects = render(&plain);
    let plain_env = env_of(of_kind(&plain_objects, "StatefulSet"));
    assert!(
        plain_env
            .iter()
            .all(|(name, _)| *name != "TAPE_BOOTSTRAP_SEED_URI"),
        "ordinary PVC restarts must not reapply a seed"
    );

    let mut seeded = spec(3);
    seeded.bootstrap_seed_uri = Some("s3://tape-backups/orders/snapshot-42.json".into());
    let seeded = Tape::new("tape", seeded);
    let seeded_objects = render(&seeded);
    let seeded_env = env_of(of_kind(&seeded_objects, "StatefulSet"));
    let seed = seeded_env
        .iter()
        .find(|(name, _)| *name == "TAPE_BOOTSTRAP_SEED_URI")
        .expect("bootstrap seed env when CR requests one")
        .1;
    assert_eq!(seed["value"], "s3://tape-backups/orders/snapshot-42.json");
}

/// #2574 — `spec.backup` is the declarative way to schedule `tape backup`.
///
/// Unset renders no CronJob, so adding the field changes nothing about the
/// workload for CRs that do not ask for a backup. Set, it renders a CronJob
/// that invokes the same CLI verb an operator would run by hand — with the
/// instance's own image and client Service URL, which is the whole reason to
/// render it rather than hand-author one alongside the CR.
#[test]
fn backup_cron_job_is_opt_in_and_invokes_the_backup_verb() {
    let plain = render(&Tape::new("tape", spec(3)));
    assert!(
        plain.iter().all(|o| o["kind"] != "CronJob"),
        "no backup policy must render no CronJob"
    );

    let mut configured = spec(3);
    configured.backup = Some(TapeBackupSpec {
        policy: service_backup::ScheduledBackupPolicy {
            schedule: "17 3 * * *".into(),
            destination: "gs://tape-backups/orders".into(),
            retention_secs: Some(604_800),
        },
        admin_token_secret: Some("tape-backup-token".into()),
    });
    let objects = render(&Tape::new("tape", configured));

    let cj = of_kind(&objects, "CronJob");
    assert_eq!(cj["metadata"]["name"], "tape-backup");
    assert_eq!(cj["spec"]["schedule"], "17 3 * * *");

    let pod = &cj["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    let container = &pod["containers"][0];
    assert_eq!(container["command"][0], "tape");
    assert_eq!(
        container["image"], "tape:test",
        "the runner uses the instance's image, so it cannot drift from it"
    );

    let args: Vec<&str> = container["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args[0], "backup");
    assert!(args.contains(&"--dest"));
    assert!(args.contains(&"gs://tape-backups/orders"));
    assert!(args.contains(&"--retention-secs"));
    assert!(args.contains(&"604800"));
    // The URL must address this instance's client Service on the client port,
    // not the headless Service and not the raft port.
    assert!(
        args.contains(&"http://tape.default.svc.cluster.local:7137"),
        "backup pulls from the instance's own client Service; args were {args:?}"
    );

    // Runs under the dedicated backup identity, not the serving one: only this
    // pod needs cloud object-store credentials.
    assert_eq!(pod["serviceAccountName"], "tape-backup");

    // The admin token is projected as the env var `tape backup --token`
    // already falls back to; `/admin/backup` needs `admin` on `*`.
    let token = container["env"]
        .as_array()
        .expect("env array")
        .iter()
        .find(|e| e["name"] == "TAPE_BACKUP_TOKEN")
        .expect("admin token env when the CR names a secret");
    assert_eq!(
        token["valueFrom"]["secretKeyRef"]["name"],
        "tape-backup-token"
    );
    assert_eq!(token["valueFrom"]["secretKeyRef"]["key"], "token");
}

/// #2574 — the `<name>-backup` ServiceAccount is rendered whether or not a
/// backup is currently scheduled (lumen's #808 pattern).
///
/// It is the binding target for cloud IAM — GKE Workload Identity annotates
/// it — so an identity whose lifecycle followed the schedule would drop that
/// binding every time the policy was toggled off. The workload ServiceAccount
/// must still come first in the render order: `of_kind` takes the first match,
/// and the pre-existing identity assertion resolves through it.
#[test]
fn backup_service_account_exists_independently_of_the_schedule() {
    let names = |objs: &[Value]| -> Vec<String> {
        objs.iter()
            .filter(|o| o["kind"] == "ServiceAccount")
            .map(|o| o["metadata"]["name"].as_str().unwrap().to_string())
            .collect()
    };

    assert_eq!(
        names(&render(&Tape::new("tape", spec(3)))),
        vec!["tape".to_string(), "tape-backup".to_string()],
        "the backup identity is rendered even with no schedule configured"
    );

    let mut configured = spec(3);
    configured.backup = Some(TapeBackupSpec {
        policy: service_backup::ScheduledBackupPolicy {
            schedule: "0 4 * * *".into(),
            destination: "gs://tape-backups/orders".into(),
            retention_secs: None,
        },
        admin_token_secret: None,
    });
    let objects = render(&Tape::new("tape", configured));
    assert_eq!(
        names(&objects),
        vec!["tape".to_string(), "tape-backup".to_string()],
        "configuring a schedule must not render a second copy of the identity"
    );

    let sa = objects
        .iter()
        .find(|o| o["kind"] == "ServiceAccount" && o["metadata"]["name"] == "tape-backup")
        .expect("backup ServiceAccount");
    assert_eq!(
        sa["metadata"]["labels"]["app.kubernetes.io/component"], "backup",
        "the backup identity must not be labelled as a serving pod"
    );
}

/// #2574 — `retentionSecs` and `adminTokenSecret` are both optional, and
/// omitting them must not emit an empty flag or an unresolvable env var.
#[test]
fn backup_cron_job_omits_unset_retention_and_token() {
    let mut minimal = spec(1);
    minimal.backup = Some(TapeBackupSpec {
        policy: service_backup::ScheduledBackupPolicy {
            schedule: "0 * * * *".into(),
            destination: "file:///var/backups/tape".into(),
            retention_secs: None,
        },
        admin_token_secret: None,
    });
    let objects = render(&Tape::new("tape", minimal));
    let container = &of_kind(&objects, "CronJob")["spec"]["jobTemplate"]["spec"]["template"]
        ["spec"]["containers"][0];

    let args: Vec<&str> = container["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        !args.contains(&"--retention-secs"),
        "omitted retention must keep every object, not pass an empty flag"
    );
    assert!(
        container["env"].as_array().expect("env array").is_empty(),
        "an auth:off instance needs no admin token"
    );
}

/// #2574 — the committed CRD carries the `backup` properties, and has not
/// drifted from the generator at all.
///
/// Asserting on the generator alone would pass while the checked-in
/// `k8s/operator/crd.yaml` still pruned `spec.backup` in-cluster — a
/// structural schema drops properties it does not declare. That is not
/// hypothetical: this test's byte-equality half caught the committed file
/// still missing `spec.topics` from #2557, which had never been regenerated.
/// The acceptance harness renders the CRD from the binary
/// (`acceptance/gcp/scripts/render-manifests.sh`), so the
/// stale file went unnoticed there.
#[test]
fn generated_crd_carries_the_backup_properties() {
    let yaml = crd_yaml();
    let committed = include_str!("../k8s/operator/crd.yaml");

    for field in [
        "backup",
        "schedule",
        "destination",
        "retentionSecs",
        "adminTokenSecret",
    ] {
        assert!(
            yaml.contains(field),
            "generated CRD schema is missing `{field}`"
        );
        assert!(
            committed.contains(field),
            "committed CRD is missing `{field}` — the API server would prune it"
        );
    }

    // Deliberately `assert!`, not `assert_eq!`: the two documents are ~8 KB
    // each and dumping both escaped into the failure output buries the one
    // line that matters.
    assert!(
        committed.trim_end() == yaml.trim_end(),
        "apps/tape/k8s/operator/crd.yaml has drifted from the generator — \
         regenerate it with `cargo run -p tape --bin tape --features operator \
         -- k8s crd render --out apps/tape/k8s/operator/crd.yaml`"
    );
}

/// The hand-maintained observability component the operator pair mirrors.
const STATIC_COMPONENT: &str = include_str!("../k8s/components/observability/prometheusrule.yaml");

/// `spec.observability` renders both objects, `spec.observability` unset
/// renders neither.
///
/// Default-off is load-bearing, not a taste call: `monitoring.coreos.com/v1`
/// is not a built-in API group, so a cluster without the Prometheus Operator
/// CRDs would reject the pair and take the whole reconcile down with it. A
/// vanilla cluster must install cleanly.
#[test]
fn observability_pair_is_opt_in() {
    let kinds = |objs: &[Value]| -> Vec<String> {
        objs.iter()
            .map(|o| o["kind"].as_str().unwrap().to_string())
            .collect()
    };

    let off = kinds(&render(&Tape::new("tape", spec(3))));
    assert!(
        !off.contains(&"ServiceMonitor".to_string())
            && !off.contains(&"PrometheusRule".to_string()),
        "a CR that never asked for observability must not require the \
         Prometheus Operator CRDs, got {off:?}"
    );

    let mut watched = spec(3);
    watched.observability = true;
    let objects = render(&Tape::new("tape", watched));
    let on = kinds(&objects);
    assert!(
        on.contains(&"ServiceMonitor".to_string()) && on.contains(&"PrometheusRule".to_string()),
        "spec.observability must render both halves, got {on:?}"
    );

    // kube-prometheus-stack's default `serviceMonitorSelector`/`ruleSelector`
    // is `release: <helm release>`; without it the stack silently ignores both
    // objects and every alert below is dead on arrival.
    for kind in ["ServiceMonitor", "PrometheusRule"] {
        assert_eq!(
            of_kind(&objects, kind)["metadata"]["labels"]["release"],
            "prometheus",
            "{kind} must carry the selector label the Prometheus Operator matches on"
        );
    }
}

/// #2575 — every alert the static component ships is reproduced by the
/// operator with its `for`, severity, summary, and runbook text intact.
///
/// The runbooks are the point: #2485's seed-failure triage is the only thing
/// that distinguishes a bad `bootstrapSeedUri` from an unrelated crash loop,
/// and losing it during the relocation would leave an alert that fires with no
/// way to act on it. Comparing the whole contract map (not a name list) also
/// catches a rule silently dropped or added on either side.
#[test]
fn prometheus_rule_reproduces_the_static_component_alert_contract() {
    use std::collections::BTreeMap;

    /// alert name -> the operator-independent half of its definition.
    fn contract(doc: &Value) -> BTreeMap<String, Value> {
        doc["spec"]["groups"][0]["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .map(|r| {
                let name = r["alert"].as_str().expect("alert name").to_string();
                let fields = serde_json::json!({
                    "for": r["for"],
                    "severity": r["labels"]["severity"],
                    "summary": r["annotations"]["summary"],
                    "runbook": r["annotations"]["runbook"],
                });
                (name, fields)
            })
            .collect()
    }

    let file: Value =
        serde_yaml::from_str(STATIC_COMPONENT).expect("static observability component parses");
    let mut watched = spec(3);
    watched.observability = true;
    let objects = render(&Tape::new("tape", watched));
    let rendered = of_kind(&objects, "PrometheusRule");

    assert_eq!(
        contract(rendered),
        contract(&file),
        "the operator-rendered PrometheusRule has drifted from \
         apps/tape/k8s/components/observability/prometheusrule.yaml — keep the two in step \
         (alert names, `for`, severity, summary, and runbook text)"
    );

    let group = |doc: &Value| {
        (
            doc["spec"]["groups"][0]["name"].clone(),
            doc["spec"]["groups"][0]["interval"].clone(),
        )
    };
    assert_eq!(group(rendered), group(&file), "group name/interval drifted");
}

/// #2575 — the exprs read the same metrics at the same thresholds as the
/// static component, and differ only where the operator's own naming forces
/// it.
///
/// Byte-equality is impossible here and asserting it would be wrong: the
/// static component selects `{app="tape",role="server"}`, labels that exist
/// only because its ServiceMonitor grafts them from the Service, and the
/// operator labels children with the `app.kubernetes.io/*` set instead. So
/// this pins what must not change — metric names and thresholds — and names
/// the two substitutions that must.
#[test]
fn prometheus_rule_exprs_keep_the_metrics_and_thresholds() {
    use std::collections::BTreeMap;

    /// Every `metric{` head in an expr.
    fn metrics(expr: &str) -> Vec<String> {
        let mut out: Vec<String> = expr
            .match_indices('{')
            .filter_map(|(i, _)| {
                let head = &expr[..i];
                let start = head
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map_or(0, |p| p + 1);
                (start < i).then(|| head[start..].to_string())
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every literal a comparison operator is tested against.
    ///
    /// The helper now returns every comparison as `"<op> <literal>"`, not a bare
    /// number: the operator is part of the emitted value so `> 2` and `>= 2`
    /// remain distinct, and the two-character forms (`>=`, `<=`, `==`, `!=`)
    /// are tried before `>` and `<` for the same reason. Returning only the
    /// literal would make a one-sided operator change between the two files look
    /// identical and let a real contract drift pass this guard.
    ///
    /// Label selectors are stripped first because a `{...}` span contains `=` and
    /// `=~`, so scanning the raw expr would read a selector as though it were a
    /// comparison and produce a bogus threshold. The old implementation was
    /// vacuous for #2578: it split on `'>'` only, so `== 0`, `>= 2`, and `<
    /// 0.1` each yielded nothing. Both files therefore returned the empty vector
    /// and the guard compared `[] == []` while the two files could disagree on
    /// the real firing threshold.
    ///
    /// Fractional literals count (#3051): `TapeMemoryHeadroomLow` fires at
    /// `> 0.85`, and stopping at the decimal point would reduce it to `0` —
    /// the same value as its divide-by-zero guard — so the two files could
    /// disagree on the actual headroom budget and this guard would pass.
    fn thresholds(expr: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = expr.chars().collect::<Vec<_>>();
        let mut filtered = Vec::new();
        let mut depth = 0usize;
        for ch in chars.drain(..) {
            match ch {
                '{' => depth += 1,
                '}' => {
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                _ if depth == 0 => filtered.push(ch),
                _ => {}
            }
        }

        let mut i = 0usize;
        while i < filtered.len() {
            let op = if i + 1 < filtered.len() && filtered[i] == '>' && filtered[i + 1] == '=' {
                Some(">=")
            } else if i + 1 < filtered.len() && filtered[i] == '<' && filtered[i + 1] == '=' {
                Some("<=")
            } else if i + 1 < filtered.len() && filtered[i] == '=' && filtered[i + 1] == '=' {
                Some("==")
            } else if i + 1 < filtered.len() && filtered[i] == '!' && filtered[i + 1] == '=' {
                Some("!=")
            } else if filtered[i] == '>' {
                Some(">")
            } else if filtered[i] == '<' {
                Some("<")
            } else {
                None
            };

            if let Some(op) = op {
                let op_len = op.len();
                let mut j = i + op_len;
                while j < filtered.len() && filtered[j].is_whitespace() {
                    j += 1;
                }
                let mut literal = String::new();
                while j < filtered.len() {
                    let ch = filtered[j];
                    if ch.is_ascii_digit() || ch == '.' {
                        literal.push(ch);
                        j += 1;
                    } else {
                        break;
                    }
                }
                if !literal.is_empty() {
                    out.push(format!("{op} {literal}"));
                }
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    fn exprs(doc: &Value) -> BTreeMap<String, String> {
        doc["spec"]["groups"][0]["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .map(|r| {
                (
                    r["alert"].as_str().unwrap().to_string(),
                    r["expr"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    let file: Value =
        serde_yaml::from_str(STATIC_COMPONENT).expect("static observability component parses");
    let mut watched = spec(3);
    watched.observability = true;
    let objects = render(&Tape::new("tape", watched));

    let static_exprs = exprs(&file);
    let rendered_exprs = exprs(of_kind(&objects, "PrometheusRule"));

    let expected_thresholds: BTreeMap<&str, Vec<String>> = BTreeMap::from([
        ("TapeAppendLatencyHigh", vec!["> 500".to_string()]),
        ("TapeReplayLatencyHigh", vec!["> 2000".to_string()]),
        ("TapePodRestarting", vec!["> 2".to_string()]),
        ("TapeStorageDegraded", vec!["> 0".to_string()]),
        ("TapeSubscriptionLagGrowing", vec!["> 0".to_string()]),
        (
            "TapeMemoryHeadroomLow",
            vec!["> 0".to_string(), "> 0.85".to_string()],
        ),
        ("TapeNoReadyServingPods", vec!["== 0".to_string()]),
        ("TapeRaftLeaderAbsent", vec!["== 0".to_string()]),
        ("TapeBackupCronJobFailed", vec![">= 2".to_string()]),
        ("TapePvcNearFull", vec!["< 0.1".to_string()]),
    ]);
    let rendered_thresholds: BTreeMap<&str, Vec<String>> = rendered_exprs
        .iter()
        .map(|(alert, expr)| (alert.as_str(), thresholds(expr)))
        .collect();
    assert_eq!(rendered_thresholds, expected_thresholds);

    for (alert, static_expr) in &static_exprs {
        let rendered = rendered_exprs
            .get(alert)
            .unwrap_or_else(|| panic!("operator dropped alert {alert}"));
        assert_eq!(
            metrics(rendered),
            metrics(static_expr),
            "{alert} reads different series than the static component"
        );
        assert_eq!(
            thresholds(rendered),
            thresholds(static_expr),
            "{alert} fires at a different threshold than the static component"
        );
    }

    // Substitution 1: the tape-scraped series are scoped by the operator's own
    // labels, never by `app`/`role` — those simply do not exist on this path,
    // so a verbatim copy would evaluate cleanly and match nothing forever.
    for (alert, expr) in &rendered_exprs {
        assert!(
            !expr.contains("app=\"") && !expr.contains("role=\""),
            "{alert} still selects on the static component's labels: {expr}"
        );
    }

    // Substitution 2: the shared StatefulSet helper names the container after
    // the component, so the kube-state-metrics alert filters `server`, not the
    // static component's `tape`.
    let restarting = &rendered_exprs["TapePodRestarting"];
    assert!(
        restarting.contains("container=\"server\""),
        "TapePodRestarting must filter the container the operator actually \
         creates: {restarting}"
    );
    assert!(
        static_exprs["TapePodRestarting"].contains("container=\"tape\""),
        "the static component's container name changed — recheck the substitution"
    );

    // #3051: Memory headroom alert uses cAdvisor series that exist in both the
    // operator-rendered and static paths, but they come from different scrapers.
    // The operator path labels with `app.kubernetes.io/*` and container="server",
    // the static path labels with `app="tape",role="server"` and container="tape".
    let rendered_memory = &rendered_exprs
        .get("TapeMemoryHeadroomLow")
        .expect("operator must render TapeMemoryHeadroomLow");
    let static_memory = &static_exprs
        .get("TapeMemoryHeadroomLow")
        .expect("static component must define TapeMemoryHeadroomLow");

    // Both use the same cAdvisor metrics: working set and spec limit.
    for metric in [
        "container_memory_working_set_bytes",
        "container_spec_memory_limit_bytes",
    ] {
        assert!(
            rendered_memory.contains(metric),
            "rendered TapeMemoryHeadroomLow must read metric {metric}"
        );
        assert!(
            static_memory.contains(metric),
            "static TapeMemoryHeadroomLow must read metric {metric}"
        );
    }

    // Container name differs as expected: operator uses component name "server",
    // static file uses the direct-install name "tape".
    assert!(
        rendered_memory.contains("container=\"server\""),
        "operator TapeMemoryHeadroomLow must filter container=server: {rendered_memory}"
    );
    assert!(
        static_memory.contains("container=\"tape\""),
        "static TapeMemoryHeadroomLow must filter container=tape"
    );
}

/// #2575 — every `app_kubernetes_io_*` label the exprs select on is one the
/// ServiceMonitor actually grafts onto the scraped series.
///
/// This is the joint the whole relocation turns on. Prometheus does not
/// publish a service's labels on its metrics by itself; `targetLabels` is what
/// puts them there, sanitized (`.`/`/` -> `_`). Add a selector without adding
/// the graft and the alert is silent — no error, no missing-object, just
/// nothing. So assert the two halves agree rather than trusting them to.
#[test]
fn service_monitor_grafts_every_label_the_exprs_select_on() {
    fn labels_used(expr: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = expr;
        while let Some(p) = rest.find("app_kubernetes_io_") {
            let tail = &rest[p..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len());
            out.push(tail[..end].to_string());
            rest = &tail[end..];
        }
        out
    }

    let mut watched = spec(3);
    watched.observability = true;
    let objects = render(&Tape::new("tape", watched));

    let sm = of_kind(&objects, "ServiceMonitor");
    let grafted: Vec<String> = sm["spec"]["targetLabels"]
        .as_array()
        .expect("targetLabels must be declared, or no k8s label reaches the series")
        .iter()
        .map(|l| l.as_str().unwrap().replace(['.', '/'], "_"))
        .collect();

    // The graft only works if the ServiceMonitor selects a Service that
    // carries those labels in the first place.
    let selector = &sm["spec"]["selector"]["matchLabels"];
    for label in sm["spec"]["targetLabels"].as_array().unwrap() {
        assert!(
            !selector[label.as_str().unwrap()].is_null(),
            "{label} is grafted onto the series but is not part of the Service selector"
        );
    }
    assert_eq!(sm["spec"]["endpoints"][0]["port"], "http");
    assert_eq!(sm["spec"]["endpoints"][0]["path"], "/metrics");

    let rule = of_kind(&objects, "PrometheusRule");
    for r in rule["spec"]["groups"][0]["rules"].as_array().unwrap() {
        let expr = r["expr"].as_str().unwrap();
        for label in labels_used(expr) {
            assert!(
                grafted.contains(&label),
                "{} selects on `{label}`, which the ServiceMonitor never grafts \
                 onto the series — the alert would never fire",
                r["alert"]
            );
        }
    }
}

/// R7 — readiness target + status phases (Pending / Reconciling / Ready).
#[test]
fn status_patch_reports_pending_reconciling_ready() {
    let tape = Tape::new("tape", spec(3));

    let targets = tape.readiness_targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].kind, "StatefulSet");
    assert_eq!(targets[0].name, "tape");

    let mut all_ready = HashMap::new();
    all_ready.insert("tape".to_string(), 3i64);
    let status = tape.status_patch(&ReadyFacts { ready: all_ready });
    assert_eq!(status["status"]["phase"], "Ready");
    assert_eq!(status["status"]["desiredReplicas"], 3);
    assert_eq!(status["status"]["readyReplicas"], 3);

    let mut partial = HashMap::new();
    partial.insert("tape".to_string(), 1i64);
    let status = tape.status_patch(&ReadyFacts { ready: partial });
    assert_eq!(status["status"]["phase"], "Reconciling");

    let status = tape.status_patch(&ReadyFacts {
        ready: HashMap::new(),
    });
    assert_eq!(status["status"]["phase"], "Pending");
}

/// The binary installs this process-level provider before CLI dispatch. The
/// shared helper is intentionally safe when an earlier TLS path installed it.
#[test]
fn rustls_provider_install_is_idempotent() {
    peer_tls::install_default_crypto_provider();
    peer_tls::install_default_crypto_provider();
}

// ---- #3054: prunes() ------------------------------------------------------

fn configured_backup() -> TapeBackupSpec {
    TapeBackupSpec {
        policy: service_backup::ScheduledBackupPolicy {
            schedule: "17 3 * * *".into(),
            destination: "gs://tape-backups/orders".into(),
            retention_secs: None,
        },
        admin_token_secret: None,
    }
}

/// `api_version` is part of the identity, not decoration: the controller GETs
/// each target by `(api_version, kind, name)`, so a wrong group/version makes
/// the delete address an object that does not exist and the prune silently
/// becomes a no-op. Assert all three.
fn prune_names(
    targets: &[service_k8s::service::PruneTarget],
) -> Vec<(&'static str, &'static str, String)> {
    targets
        .iter()
        .map(|t| (t.api_version, t.kind, t.name.clone()))
        .collect()
}

/// The four `(observability, backup)` permutations, so every test below states
/// its expectation against the whole spec space instead of one sample.
fn permutations() -> Vec<(bool, bool, Tape)> {
    let mut out = Vec::new();
    for observability in [false, true] {
        for backup in [false, true] {
            let mut s = spec(3);
            s.observability = observability;
            s.backup = backup.then(configured_backup);
            out.push((observability, backup, Tape::new("tape", s)));
        }
    }
    out
}

/// R1/AC1 — no backup schedule: prune exactly the backup CronJob, whatever
/// `spec.observability` says. The two are independent branches and the
/// observability one deliberately prunes nothing (see
/// [`prunes_never_names_a_kind_a_vanilla_cluster_does_not_serve`]).
#[test]
fn prunes_names_the_cron_job_when_no_backup_is_configured() {
    for (observability, backup, tape) in permutations() {
        if backup {
            continue;
        }
        assert_eq!(
            prune_names(&prunes(&tape)),
            vec![("batch/v1", "CronJob", "tape-backup".to_string())],
            "at observability={observability} with no backup schedule"
        );
    }
}

/// R1/AC1 — a backup schedule is configured, so the CronJob is rendered and
/// must not be pruned; nothing else is a prune candidate either.
#[test]
fn prunes_names_nothing_when_a_backup_schedule_is_configured() {
    for (observability, backup, tape) in permutations() {
        if !backup {
            continue;
        }
        assert!(
            prunes(&tape).is_empty(),
            "at observability={observability} with a backup schedule, the CronJob is \
             rendered and prunes() must name nothing"
        );
    }
}

/// The regression guard for the defect the Kind gate caught: a `PruneTarget`
/// costs a GET on **every** requeue, so naming a kind whose API group the
/// cluster does not serve fails the entire reconcile — the apply and the status
/// write included — not just the prune.
///
/// A missing *object* comes back as a structured `NotFound` that `get_opt` maps
/// to `Ok(None)`. A missing *API group* comes back as a plain-text
/// `404 page not found` that kube cannot parse into an `ErrorResponse`, so its
/// `reason == "NotFound"` arm never matches and the error propagates. The
/// operator then retries every 15s, forever, and the CR's status stays empty.
///
/// This is why the `spec.observability` ServiceMonitor/PrometheusRule pair is
/// absent from `prunes()`: both are `monitoring.coreos.com/v1`, and
/// `spec.observability` is default-off precisely so a cluster without the
/// Prometheus Operator CRDs stays installable — pruning on the `false` branch
/// would reach for that group in exactly the vanilla case the default protects.
/// Blocked on #3079; until then the allow-list below is the gate.
#[test]
fn prunes_never_names_a_kind_a_vanilla_cluster_does_not_serve() {
    // Groups every conformant Kubernetes apiserver serves without any CRD or
    // add-on installed. Anything outside this set is only present if something
    // installed it, which the operator cannot assume and must not GET.
    const BUILT_IN: &[&str] = &[
        "v1",
        "apps/v1",
        "batch/v1",
        "policy/v1",
        "networking.k8s.io/v1",
        "rbac.authorization.k8s.io/v1",
        "autoscaling/v2",
        "coordination.k8s.io/v1",
    ];
    for (observability, backup, tape) in permutations() {
        for target in prunes(&tape) {
            assert!(
                BUILT_IN.contains(&target.api_version),
                "prunes() names {}/{} in group `{}`, which a cluster without add-on \
                 CRDs does not serve; the GET returns an unparseable plain-text 404 \
                 and fails the whole reconcile loop (observability={observability} \
                 backup={backup})",
                target.kind,
                target.name,
                target.api_version,
            );
        }
    }
}

/// AC6 — the always-rendered `<name>-backup` ServiceAccount must never appear
/// in `prunes()` under any spec, since it deliberately outlives the schedule
/// toggle. The controller's own ownership re-check
/// (`libs/service-k8s/src/controller.rs:198` `prune_object`) is a separate,
/// already-exercised safety net this test does not reimplement.
#[test]
fn prunes_never_names_the_backup_service_account() {
    for (observability, backup, tape) in permutations() {
        assert!(
            prunes(&tape)
                .iter()
                .all(|t| !(t.kind == "ServiceAccount" && t.name == "tape-backup")),
            "the backup ServiceAccount is a stable identity rendered unconditionally; \
             prunes() must never name it (observability={observability} backup={backup})"
        );
    }
}

/// The load-bearing invariant behind `prunes()`, asserted across all four spec
/// permutations, in two halves that each catch a different failure.
///
/// - **Disjoint — holds for every target, unconditionally.** An identity that
///   `prunes()` names while `render()` still emits it makes the controller
///   delete an object it re-applies on the next pass, forever: a reconcile loop
///   that never converges and a CronJob that fires or does not depending on
///   where in the loop the clock lands.
/// - **Total — holds for the CronJob.** It is rendered or pruned, never
///   neither. Neither is the #3054 bug itself: SSA reconciles fields, not
///   object lifetimes, so an object that stops being rendered and is never
///   pruned simply stays. A future conditional branch added to `render()` and
///   not mirrored into `prunes()` fails here rather than in a cluster months
///   later — so a new entry belongs in `total` unless it has an explicit
///   exemption below.
///
/// The `spec.observability` pair is the one exemption, and it is asserted as an
/// exemption rather than omitted, so that deleting `prunes()`'s CronJob branch
/// and "fixing" this test by moving the CronJob into the same list cannot pass
/// quietly. Both are `monitoring.coreos.com/v1`; pruning them would GET an API
/// group a vanilla cluster does not serve and fail every reconcile — see
/// [`prunes_never_names_a_kind_a_vanilla_cluster_does_not_serve`]. Blocked on
/// #3079.
#[test]
fn prunes_is_the_exact_inverse_of_the_conditional_render_branches() {
    let total = [("batch/v1", "CronJob", "tape-backup")];
    let exempt = [
        ("monitoring.coreos.com/v1", "ServiceMonitor", "tape"),
        ("monitoring.coreos.com/v1", "PrometheusRule", "tape"),
    ];
    for (observability, backup, tape) in permutations() {
        let rendered: Vec<(String, String, String)> = render(&tape)
            .iter()
            .map(|o| {
                (
                    o["apiVersion"].as_str().unwrap().to_string(),
                    o["kind"].as_str().unwrap().to_string(),
                    o["metadata"]["name"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let pruned = prune_names(&prunes(&tape));
        let is_rendered = |api_version: &str, kind: &str, name: &str| {
            rendered.contains(&(api_version.to_string(), kind.to_string(), name.to_string()))
        };
        let is_pruned = |api_version: &str, kind: &str, name: &str| {
            pruned
                .iter()
                .any(|(a, k, n)| *a == api_version && *k == kind && n == name)
        };

        for (api_version, kind, name) in &pruned {
            assert!(
                !is_rendered(api_version, kind, name),
                "{kind}/{name} is both rendered and pruned at \
                 observability={observability} backup={backup}; the controller \
                 would delete and re-apply it on every pass"
            );
        }

        for (api_version, kind, name) in total {
            assert!(
                is_rendered(api_version, kind, name) ^ is_pruned(api_version, kind, name),
                "{kind}/{name} is rendered={} pruned={} at observability={observability} \
                 backup={backup}; it must be in exactly one of the two sets",
                is_rendered(api_version, kind, name),
                is_pruned(api_version, kind, name),
            );
        }

        for (api_version, kind, name) in exempt {
            assert!(
                !is_pruned(api_version, kind, name),
                "{kind}/{name} is pruned at observability={observability} backup={backup}; \
                 its API group is not served by a vanilla cluster, so the GET would fail \
                 every reconcile (#3079)"
            );
        }
    }
}

// ---- #3054: conditions() ---------------------------------------------------

fn condition<'a>(
    facts: &'a [service_k8s::ConditionFact],
    type_: &str,
) -> &'a service_k8s::ConditionFact {
    facts
        .iter()
        .find(|c| c.type_ == type_)
        .unwrap_or_else(|| panic!("expected a `{type_}` condition, got: {facts:?}"))
}

fn ready_facts(name: &str, count: i64) -> ReadyFacts {
    let mut ready = HashMap::new();
    ready.insert(name.to_string(), count);
    ReadyFacts { ready }
}

/// R2/AC3 — all five conditions are present, each with a non-empty `reason`
/// and `message`.
#[test]
fn conditions_reports_all_five_with_reason_and_message() {
    let tape = Tape::new("tape", spec(3));
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    assert_eq!(facts.len(), 5, "got: {facts:?}");
    for type_ in [
        "Ready",
        "Progressing",
        "StorageHealthy",
        "BackupConfigured",
        "BackupSucceeded",
    ] {
        let c = condition(&facts, type_);
        assert!(!c.reason.is_empty(), "{type_} must carry a reason");
        assert!(!c.message.is_empty(), "{type_} must carry a message");
    }
}

/// R3/AC5 — `conditions()` is a pure function of `(spec, ready facts,
/// context)`: identical inputs called twice produce identical output.
///
/// This is what makes `observed_conditions()` safe to diff against: the
/// projection helper only preserves an existing `lastTransitionTime` when the
/// newly computed fact equals the observed one, so a `conditions()` that
/// varied per call — a clock read, a hash iteration order, a counter — would
/// restamp every condition on every 30s requeue and make the transition time
/// meaningless. `ConditionFact` carries no timestamp field of its own; the
/// stamping is entirely [`service_k8s::service::project`]'s job.
#[test]
fn conditions_is_a_pure_function_of_spec_and_observed_facts() {
    let tape = Tape::new("tape", spec(3));
    let facts_a = tape.conditions(&ready_facts("tape", 2), &Value::Null);
    let facts_b = tape.conditions(&ready_facts("tape", 2), &Value::Null);
    assert_eq!(facts_a, facts_b);
}

/// R2 — a degraded-storage instance is out of the operator's observation
/// reach today, so `StorageHealthy` reports `Unknown`/`NotObserved` rather
/// than a `False` derived from a signal that does not mean "the disk is
/// full". #3054's AC4 wanted the `False`; the observation path it needs is
/// #3071. The reasoning is on the condition in `src/operator/reconcile.rs`.
#[test]
fn storage_healthy_reports_unknown_not_observed() {
    let tape = Tape::new("tape", spec(3));
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    let storage = condition(&facts, "StorageHealthy");
    assert_eq!(storage.status, ConditionStatus::Unknown);
    assert_eq!(storage.reason, "NotObserved");
}

/// R2 — `BackupConfigured` names the schedule and destination when set, and
/// reports `False`/`NotConfigured` when `spec.backup` is unset.
#[test]
fn backup_configured_reflects_spec_backup() {
    let tape = Tape::new("tape", spec(3));
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    let backup = condition(&facts, "BackupConfigured");
    assert_eq!(backup.status, ConditionStatus::False);
    assert_eq!(backup.reason, "NotConfigured");

    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    let backup = condition(&facts, "BackupConfigured");
    assert_eq!(backup.status, ConditionStatus::True);
    assert_eq!(backup.reason, "ScheduleConfigured");
    assert!(backup.message.contains("17 3 * * *"));
    assert!(backup.message.contains("gs://tape-backups/orders"));
}

/// AC5 anti-drift assertion: the `Ready` condition's status agrees with
/// `status_patch`'s `phase` for the same inputs, because both project from
/// the same `Observation` — this is what makes the shared struct load-bearing
/// rather than two independently-written computations that happen to agree
/// today.
#[test]
fn ready_condition_agrees_with_status_patch_phase() {
    let tape = Tape::new("tape", spec(3));
    for count in [0i64, 1, 3] {
        let facts = tape.conditions(&ready_facts("tape", count), &Value::Null);
        let ready = condition(&facts, "Ready");
        let patch = tape.status_patch(&ready_facts("tape", count));
        let phase = patch["status"]["phase"].as_str().unwrap();
        let expected_ready_status = phase == "Ready";
        assert_eq!(
            ready.status == ConditionStatus::True,
            expected_ready_status,
            "Ready condition ({:?}) disagrees with status_patch phase ({phase}) at count={count}",
            ready.status
        );
    }
}

// ---- #2581: serviceAccountName --------------------------------------------

/// #2581 E1 — A `Tape` CR without `serviceAccountName` renders the exact same
/// object set it renders today (no diff).
#[test]
fn service_account_name_unset_renders_existing_shape() {
    let tape = Tape::new("tape", spec(3));
    let objs = render(&tape);

    let kinds_names: Vec<(&str, &str)> = objs
        .iter()
        .map(|o| {
            (
                o["kind"].as_str().unwrap(),
                o["metadata"]["name"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds_names,
        vec![
            ("ServiceAccount", "tape"),
            ("StatefulSet", "tape"),
            ("Service", "tape-headless"),
            ("Service", "tape"),
            ("PodDisruptionBudget", "tape"),
            ("ServiceAccount", "tape-backup"),
        ]
    );

    let sts = of_kind(&objs, "StatefulSet");
    assert_eq!(
        sts["spec"]["template"]["spec"]["serviceAccountName"],
        "tape"
    );
}

/// #2581 E2 — A CR with `serviceAccountName: platform-sa` renders no workload
/// `ServiceAccount` named `<instance>` and a StatefulSet pointing to `platform-sa`.
#[test]
fn service_account_name_configured_omits_service_account_and_updates_statefulset() {
    let mut configured = spec(3);
    configured.service_account_name = Some("platform-sa".into());
    let tape = Tape::new("tape", configured);
    let objs = render(&tape);

    let has_workload_sa = objs
        .iter()
        .any(|o| o["kind"] == "ServiceAccount" && o["metadata"]["name"] == "tape");
    assert!(
        !has_workload_sa,
        "operator must not render workload ServiceAccount when serviceAccountName is set"
    );

    let sts = of_kind(&objs, "StatefulSet");
    assert_eq!(
        sts["spec"]["template"]["spec"]["serviceAccountName"],
        "platform-sa"
    );
}

/// #2581 E3 — `<name>-backup` is invariant: `<instance>-backup` ServiceAccount
/// is rendered in both cases, and the backup CronJob pod serviceAccountName is
/// `<instance>-backup`.
#[test]
fn service_account_name_does_not_affect_backup_identity() {
    let mut unset = spec(3);
    unset.backup = Some(configured_backup());
    let tape_unset = Tape::new("tape", unset);
    let objs_unset = render(&tape_unset);

    let sa_backup_unset = objs_unset
        .iter()
        .find(|o| o["kind"] == "ServiceAccount" && o["metadata"]["name"] == "tape-backup")
        .expect("tape-backup ServiceAccount must exist when serviceAccountName is unset");
    assert_eq!(sa_backup_unset["metadata"]["name"], "tape-backup");

    let cj_unset = of_kind(&objs_unset, "CronJob");
    assert_eq!(
        cj_unset["spec"]["jobTemplate"]["spec"]["template"]["spec"]["serviceAccountName"],
        "tape-backup"
    );

    let mut set = spec(3);
    set.service_account_name = Some("platform-sa".into());
    set.backup = Some(configured_backup());
    let tape_set = Tape::new("tape", set);
    let objs_set = render(&tape_set);

    let sa_backup_set = objs_set
        .iter()
        .find(|o| o["kind"] == "ServiceAccount" && o["metadata"]["name"] == "tape-backup")
        .expect("tape-backup ServiceAccount must exist when serviceAccountName is set");
    assert_eq!(sa_backup_set["metadata"]["name"], "tape-backup");

    let cj_set = of_kind(&objs_set, "CronJob");
    assert_eq!(
        cj_set["spec"]["jobTemplate"]["spec"]["template"]["spec"]["serviceAccountName"],
        "tape-backup"
    );
}

/// #2581 E4 — `prunes()` is unchanged when `serviceAccountName` is configured:
/// the workload ServiceAccount is never a prune target.
#[test]
fn service_account_name_does_not_affect_prunes() {
    let mut configured = spec(3);
    configured.service_account_name = Some("platform-sa".into());
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);

    let targets = prunes(&tape);
    assert!(
        targets.is_empty(),
        "prunes() must return empty vec when backup is configured, even with serviceAccountName set; got {targets:?}"
    );

    let mut no_backup = spec(3);
    no_backup.service_account_name = Some("platform-sa".into());
    let tape_no_backup = Tape::new("tape", no_backup);
    let targets_no_backup = prunes(&tape_no_backup);
    assert_eq!(
        prune_names(&targets_no_backup),
        vec![("batch/v1", "CronJob", "tape-backup".to_string())],
        "prunes() must only name backup CronJob when backup is None, never a ServiceAccount"
    );
}

/// #2582 E1 — A `Tape` CR omitting `logFormat` renders `TAPE_LOG_FORMAT=json`
/// at the exact same position in the env list.
#[test]
fn log_format_unset_renders_existing_shape() {
    let tape = Tape::new("tape", spec(3));
    let objs = render(&tape);
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);

    let format_entry = env
        .iter()
        .find(|(k, _)| *k == "TAPE_LOG_FORMAT")
        .map(|(_, v)| v["value"].as_str().unwrap());
    assert_eq!(format_entry, Some("json"));

    let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        vec![
            "POD_NAME",
            "POD_NAMESPACE",
            "SHARD_COUNT",
            "REPLICAS_PER_SHARD",
            "VOTER_COUNT",
            "TAPE_PEER_SERVICE",
            "TAPE_BIND",
            "TAPE_RAFT_PORT",
            "TAPE_DATA_DIR",
            "TAPE_GRACE_SECS",
            "TAPE_LOG_FORMAT",
            "TAPE_AUTH",
        ]
    );
}

/// #2582 E2 — A `Tape` CR with `logFormat: pretty` renders `TAPE_LOG_FORMAT=pretty`.
#[test]
fn log_format_configured_pretty_renders_pretty() {
    let mut configured = spec(3);
    configured.log_format = LogFormat::Pretty;
    let tape = Tape::new("tape", configured);
    let objs = render(&tape);
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);

    let format_entry = env
        .iter()
        .find(|(k, _)| *k == "TAPE_LOG_FORMAT")
        .map(|(_, v)| v["value"].as_str().unwrap());
    assert_eq!(format_entry, Some("pretty"));
}

/// #2582 E3 — `TAPE_LOG_FORMAT` and `RUST_LOG` are orthogonal: `TAPE_LOG_FORMAT`
/// is unconditional, while `RUST_LOG` is present only when `logLevel` is set.
#[test]
fn log_format_and_log_level_are_orthogonal() {
    // 1. Unset logFormat, unset logLevel -> no RUST_LOG
    let tape1 = Tape::new("tape", spec(3));
    let objs1 = render(&tape1);
    let env1 = env_of(of_kind(&objs1, "StatefulSet"));
    assert!(env1.iter().all(|(k, _)| *k != "RUST_LOG"));
    assert_eq!(
        env1.iter()
            .find(|(k, _)| *k == "TAPE_LOG_FORMAT")
            .unwrap()
            .1["value"],
        "json"
    );

    // 2. Unset logFormat, logLevel: debug -> RUST_LOG=debug
    let mut spec2 = spec(3);
    spec2.log_level = Some("debug".into());
    let objs2 = render(&Tape::new("tape", spec2));
    let env2 = env_of(of_kind(&objs2, "StatefulSet"));
    assert_eq!(
        env2.iter().find(|(k, _)| *k == "RUST_LOG").unwrap().1["value"],
        "debug"
    );
    assert_eq!(
        env2.iter()
            .find(|(k, _)| *k == "TAPE_LOG_FORMAT")
            .unwrap()
            .1["value"],
        "json"
    );

    // 3. logFormat: pretty, unset logLevel -> no RUST_LOG
    let mut spec3 = spec(3);
    spec3.log_format = LogFormat::Pretty;
    let objs3 = render(&Tape::new("tape", spec3));
    let env3 = env_of(of_kind(&objs3, "StatefulSet"));
    assert!(env3.iter().all(|(k, _)| *k != "RUST_LOG"));
    assert_eq!(
        env3.iter()
            .find(|(k, _)| *k == "TAPE_LOG_FORMAT")
            .unwrap()
            .1["value"],
        "pretty"
    );

    // 4. logFormat: pretty, logLevel: debug -> RUST_LOG=debug
    let mut spec4 = spec(3);
    spec4.log_format = LogFormat::Pretty;
    spec4.log_level = Some("debug".into());
    let objs4 = render(&Tape::new("tape", spec4));
    let env4 = env_of(of_kind(&objs4, "StatefulSet"));
    assert_eq!(
        env4.iter().find(|(k, _)| *k == "RUST_LOG").unwrap().1["value"],
        "debug"
    );
    assert_eq!(
        env4.iter()
            .find(|(k, _)| *k == "TAPE_LOG_FORMAT")
            .unwrap()
            .1["value"],
        "pretty"
    );
}

/// #2582 — Serde defaults logFormat to Json and rejects unknown values.
#[test]
fn log_format_serde_default_and_validation() {
    assert_eq!(LogFormat::default(), LogFormat::Json);
    let parsed: TapeSpec = serde_json::from_value(serde_json::json!({
        "image": "tape:test",
        "storage": "10Gi",
    }))
    .expect("a spec omitting logFormat parses");
    assert_eq!(parsed.log_format, LogFormat::Json);

    let pretty_parsed: TapeSpec = serde_json::from_value(serde_json::json!({
        "image": "tape:test",
        "storage": "10Gi",
        "logFormat": "pretty",
    }))
    .expect("a spec with logFormat: pretty parses");
    assert_eq!(pretty_parsed.log_format, LogFormat::Pretty);

    let typo: Result<TapeSpec, _> = serde_json::from_value(serde_json::json!({
        "image": "tape:test",
        "storage": "10Gi",
        "logFormat": "nonsense",
    }));
    assert!(typo.is_err(), "an unknown logFormat value must be rejected");
}

/// E1: CRD spec validation rules array contains exactly two rules in stable order:
/// the one-token-source rule first, and the backup-admin-token rule second.
#[test]
fn crd_has_two_spec_validation_rules_in_order() {
    let yaml = crd_yaml();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("CRD parses as YAML");
    let rules = doc["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
        ["x-kubernetes-validations"]
        .as_sequence()
        .expect("spec carries validation rules");
    assert_eq!(
        rules.len(),
        2,
        "spec schema must carry exactly two validation rules"
    );
    assert_eq!(
        rules[0]["rule"],
        "!(has(self.tokensSecret) && has(self.tokensSecretProviderClass))"
    );
    assert_eq!(
        rules[1]["rule"],
        "!(has(self.backup) && self.auth == 'required' && !has(self.backup.adminTokenSecret))"
    );
}

/// E2 & E3: Backup CEL rule text must match presence-only contract and message must name field.
#[test]
fn crd_rejects_backup_without_admin_token_under_auth_required() {
    let yaml = crd_yaml();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("CRD parses as YAML");
    let rules = doc["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
        ["x-kubernetes-validations"]
        .as_sequence()
        .expect("spec carries validation rules");
    let rule = rules
        .iter()
        .find(|r| {
            r["rule"]
                .as_str()
                .is_some_and(|s| s.contains("adminTokenSecret"))
        })
        .expect("a rule covering backup admin token requirement");
    assert_eq!(
        rule["rule"],
        "!(has(self.backup) && self.auth == 'required' && !has(self.backup.adminTokenSecret))"
    );
    assert!(
        !rule["rule"].as_str().unwrap().contains("null"),
        "CEL rules on nullable fields take presence tests only; a `!= null` \
         guard fails compilation at the API server and installs on no cluster"
    );
    assert!(
        rule["message"]
            .as_str()
            .is_some_and(|m| m.contains("spec.backup.adminTokenSecret")),
        "the rejection message must contain the literal 'spec.backup.adminTokenSecret'"
    );
}

/// E5: The stale phrase 'the CR is accepted either way' must not appear in render.rs or the generated CRD.
#[test]
fn crd_description_has_no_stale_accepted_either_way_phrase() {
    let render_src = include_str!("../src/operator/render.rs");
    assert!(
        !render_src.contains("the CR is accepted either way"),
        "render.rs must not contain the stale 'the CR is accepted either way' phrase"
    );
    let yaml = crd_yaml();
    assert!(
        !yaml.contains("the CR is accepted either way"),
        "the generated CRD must not contain the stale 'the CR is accepted either way' phrase"
    );
}

// ---- #3071: BackupSucceeded & context tests --------------------------------

use tape::operator::reconcile::{backup_run_fact, CronJobStatusFacts, TapeContext};

fn parse_k8s_time(s: &str) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::Time {
    serde_json::from_value(serde_json::json!(s)).expect("valid RFC3339 time string")
}

/// E5 row 1: `spec.backup` unset yields `Unknown`/`NotConfigured`.
#[test]
fn backup_succeeded_state_table_not_configured() {
    let fact = backup_run_fact(false, None);
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "NotConfigured");
}

/// E5 row 2: CronJob status missing yields `Unknown`/`NotObserved`.
#[test]
fn backup_succeeded_state_table_not_observed() {
    let fact = backup_run_fact(true, None);
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "NotObserved");
}

/// E5 row 3: `status.active` non-empty yields `Unknown`/`RunInProgress`, even
/// when `lastScheduleTime` is newer than `lastSuccessfulTime`.
#[test]
fn backup_succeeded_state_table_run_in_progress() {
    let facts = CronJobStatusFacts {
        active: true,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T09:00:00Z")),
    };
    let fact = backup_run_fact(true, Some(&facts));
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "RunInProgress");
}

/// Negative control for the row-3-over-row-5 ordering.
#[test]
fn backup_succeeded_state_table_run_in_progress_precedence_over_last_run_succeeded() {
    let facts_later = CronJobStatusFacts {
        active: true,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
    };
    let fact_later = backup_run_fact(true, Some(&facts_later));
    assert_eq!(fact_later.type_, "BackupSucceeded");
    assert_eq!(fact_later.status, ConditionStatus::Unknown);
    assert_eq!(fact_later.reason, "RunInProgress");

    let facts_equal = CronJobStatusFacts {
        active: true,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
    };
    let fact_equal = backup_run_fact(true, Some(&facts_equal));
    assert_eq!(fact_equal.type_, "BackupSucceeded");
    assert_eq!(fact_equal.status, ConditionStatus::Unknown);
    assert_eq!(fact_equal.reason, "RunInProgress");
}

/// E5 row 4: `status.lastScheduleTime` absent yields `Unknown`/`NotYetRun`.
#[test]
fn backup_succeeded_state_table_not_yet_run() {
    let facts = CronJobStatusFacts {
        active: false,
        last_schedule: None,
        last_successful: None,
    };
    let fact = backup_run_fact(true, Some(&facts));
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "NotYetRun");
}

/// E5 row 5: `lastSuccessfulTime` present and not before `lastScheduleTime` yields `True`/`LastRunSucceeded`.
#[test]
fn backup_succeeded_state_table_last_run_succeeded() {
    let facts = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
    };
    let fact = backup_run_fact(true, Some(&facts));
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::True);
    assert_eq!(fact.reason, "LastRunSucceeded");

    // Exact equal timestamp also counts as "not before" -> True
    let facts_equal = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
    };
    let fact_equal = backup_run_fact(true, Some(&facts_equal));
    assert_eq!(fact_equal.status, ConditionStatus::True);
    assert_eq!(fact_equal.reason, "LastRunSucceeded");
}

/// E5 row 6: scheduled, not active, no matching success yields `False`/`LastRunFailed`.
#[test]
fn backup_succeeded_state_table_last_run_failed() {
    let facts_older = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T09:00:00Z")),
    };
    let fact_older = backup_run_fact(true, Some(&facts_older));
    assert_eq!(fact_older.type_, "BackupSucceeded");
    assert_eq!(fact_older.status, ConditionStatus::False);
    assert_eq!(fact_older.reason, "LastRunFailed");

    let facts_no_succ = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: None,
    };
    let fact_no_succ = backup_run_fact(true, Some(&facts_no_succ));
    assert_eq!(fact_no_succ.status, ConditionStatus::False);
    assert_eq!(fact_no_succ.reason, "LastRunFailed");
}

/// E4: Null context yields `Unknown` status for `BackupSucceeded`, never `True` and never `False`.
#[test]
fn backup_succeeded_null_context_yields_unknown() {
    let plain = Tape::new("tape", spec(3));
    let plain_facts = plain.conditions(&ready_facts("tape", 3), &Value::Null);
    let plain_succ = condition(&plain_facts, "BackupSucceeded");
    assert_eq!(plain_succ.status, ConditionStatus::Unknown);
    assert_eq!(plain_succ.reason, "NotConfigured");

    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let cfg = Tape::new("tape", configured);
    let cfg_facts = cfg.conditions(&ready_facts("tape", 3), &Value::Null);
    let cfg_succ = condition(&cfg_facts, "BackupSucceeded");
    assert_eq!(cfg_succ.status, ConditionStatus::Unknown);
    assert_eq!(cfg_succ.reason, "NotObserved");
}

/// E6: Purity survives with non-null context — calling `conditions` twice with identical non-null context yields identical output.
#[test]
fn conditions_is_a_pure_function_with_non_null_context() {
    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);

    let ctx = serde_json::to_value(TapeContext {
        backup_cron_job: Some(CronJobStatusFacts {
            active: false,
            last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
            last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
        }),
    })
    .unwrap();

    let facts_a = tape.conditions(&ready_facts("tape", 3), &ctx);
    let facts_b = tape.conditions(&ready_facts("tape", 3), &ctx);
    assert_eq!(facts_a, facts_b);
}

/// E7: `Ready` condition and `status_patch` are independent of the context.
#[test]
fn ready_condition_and_status_patch_are_independent_of_context() {
    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);

    let ready_3 = ready_facts("tape", 3);
    let ctx_null = Value::Null;
    let ctx_succeeded = serde_json::to_value(TapeContext {
        backup_cron_job: Some(CronJobStatusFacts {
            active: false,
            last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
            last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
        }),
    })
    .unwrap();
    let ctx_failed = serde_json::to_value(TapeContext {
        backup_cron_job: Some(CronJobStatusFacts {
            active: false,
            last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
            last_successful: Some(parse_k8s_time("2026-07-31T09:00:00Z")),
        }),
    })
    .unwrap();

    let patch_null = tape.status_patch(&ready_3);
    let patch_succeeded = tape.status_patch_with_context(&ready_3, &ctx_succeeded);
    let patch_failed = tape.status_patch_with_context(&ready_3, &ctx_failed);
    assert_eq!(patch_null, patch_succeeded);
    assert_eq!(patch_null, patch_failed);

    let conds_null = tape.conditions(&ready_3, &ctx_null);
    let conds_succ = tape.conditions(&ready_3, &ctx_succeeded);
    let conds_fail = tape.conditions(&ready_3, &ctx_failed);

    let ready_cond_null = condition(&conds_null, "Ready");
    let ready_cond_succ = condition(&conds_succ, "Ready");
    let ready_cond_fail = condition(&conds_fail, "Ready");
    assert_eq!(ready_cond_null, ready_cond_succ);
    assert_eq!(ready_cond_null, ready_cond_fail);
}
// HANDWRITE-END
