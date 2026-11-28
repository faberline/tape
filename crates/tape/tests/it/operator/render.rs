use super::*;

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
        "peerTls",
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
        include_str!("../../../../../k8s/operator/cluster/crd.yaml"),
        yaml,
        "checked-in CRD must be regenerated from the renderer"
    );
}

/// R5 — the rendered StatefulSet carries exactly the downward-API env tape's
/// serve reads, the right replica count (single group), tape's runtime env +
/// disk tier, the probe contract, and the sibling child objects.
#[test]
fn render_emits_expected_child_objects() {
    let tape = Tape::new("tape", spec(3));
    let objs = render(&tape).unwrap();

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
    // graceSecs is the whole shutdown budget; the kubelet waits a fixed
    // slack beyond it so the sequence finishes before SIGKILL.
    assert_eq!(get("TAPE_GRACE_SECS")["value"], "10");
    assert_eq!(
        sts["spec"]["template"]["spec"]["terminationGracePeriodSeconds"],
        10 + tape_operator::render::TERMINATION_SLACK_SECS
    );

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
    let objs = render(&plain).unwrap();
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
    let objs = render(&Tape::new("tape", open)).unwrap();
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
    let objs = render(&Tape::new("tape", secured)).unwrap();
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
    let objs = render(&Tape::new("tape", csi)).unwrap();
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
    let objs = render(&Tape::new("tape", both)).unwrap();
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
    let objs = render(&Tape::new("tape", csi)).unwrap();
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
    let plain_objects = render(&plain).unwrap();
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
    let seeded_objects = render(&seeded).unwrap();
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
    let plain = render(&Tape::new("tape", spec(3))).unwrap();
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
    let objects = render(&Tape::new("tape", configured)).unwrap();

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
        names(&render(&Tape::new("tape", spec(3))).unwrap()),
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
    let objects = render(&Tape::new("tape", configured)).unwrap();
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
    let objects = render(&Tape::new("tape", minimal)).unwrap();
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
/// `k8s/operator/cluster/crd.yaml` still pruned `spec.backup` in-cluster — a
/// structural schema drops properties it does not declare. That is not
/// hypothetical: this test's byte-equality half caught the committed file
/// still missing `spec.topics` from #2557, which had never been regenerated.
/// The acceptance harness renders the CRD from the binary
/// (`acceptance/gcp/scripts/render-manifests.sh`), so the
/// stale file went unnoticed there.
#[test]
fn generated_crd_carries_the_backup_properties() {
    let yaml = crd_yaml();
    let committed = include_str!("../../../../../k8s/operator/cluster/crd.yaml");

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
        "k8s/operator/cluster/crd.yaml has drifted from the generator — \
         regenerate it with `cargo run -p tape --bin tape --features operator \
         -- k8s crd render --out k8s/operator/cluster/crd.yaml`"
    );
}

/// Issue #242 — `spec.peerTls` turns on raft peer mTLS: the render sets the
/// `TAPE_PEER_*` env, a read-only Secret volume and its mount.
#[test]
fn peer_tls_secret_renders_env_volume_and_mount() {
    let mut s = spec(3);
    s.peer_tls = Some(TapePeerTls {
        secret_name: "tape-peer".into(),
    });
    let objs = render(&Tape::new("tape", s)).unwrap();
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);

    assert_eq!(
        env.iter().find(|(n, _)| *n == "TAPE_PEER_MTLS").unwrap().1["value"],
        "on"
    );
    for (name, file) in [
        ("TAPE_PEER_TLS_CERT", "tls.crt"),
        ("TAPE_PEER_TLS_KEY", "tls.key"),
        ("TAPE_PEER_TLS_CA", "ca.crt"),
    ] {
        assert_eq!(
            env.iter().find(|(n, _)| *n == name).unwrap().1["value"],
            format!("/var/run/secrets/tape-peer-tls/{file}").as_str(),
            "{name} must point at the mounted Secret key"
        );
    }

    let vols = sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap();
    let vol = vols
        .iter()
        .find(|v| v["name"] == "tape-peer-tls")
        .expect("peer TLS volume");
    assert_eq!(vol["secret"]["secretName"], "tape-peer");
    assert_eq!(vol["secret"]["defaultMode"], 288, "0o440");

    let mounts = sts["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
        .as_array()
        .unwrap();
    let mount = mounts
        .iter()
        .find(|m| m["name"] == "tape-peer-tls")
        .expect("peer TLS mount on the container");
    assert_eq!(mount["mountPath"], "/var/run/secrets/tape-peer-tls");
    assert_eq!(mount["readOnly"], true);
}

/// Issue #242 — without `spec.peerTls` the render adds no peer wiring.
#[test]
fn no_peer_tls_renders_no_peer_env() {
    let objs = render(&Tape::new("tape", spec(3))).unwrap();
    let sts = of_kind(&objs, "StatefulSet");
    let env = env_of(sts);

    assert!(
        !env.iter()
            .any(|(n, _)| n.starts_with("TAPE_PEER_TLS") || *n == "TAPE_PEER_MTLS"),
        "no peer TLS env without spec.peerTls"
    );
    let vols = sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap();
    assert!(
        vols.iter().all(|v| v["name"] != "tape-peer-tls"),
        "no peer TLS volume without spec.peerTls"
    );
    let mounts = sts["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
        .as_array()
        .unwrap();
    assert!(
        mounts.iter().all(|m| m["name"] != "tape-peer-tls"),
        "no peer TLS mount without spec.peerTls"
    );
}
