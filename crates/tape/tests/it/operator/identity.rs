use super::*;

// ---- #2581: serviceAccountName --------------------------------------------

/// #2581 E1 — A `Tape` CR without `serviceAccountName` renders the exact same
/// object set it renders today (no diff).
#[test]
fn service_account_name_unset_renders_existing_shape() {
    let tape = Tape::new("tape", spec(3));
    let objs = render(&tape).unwrap();

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
    let objs = render(&tape).unwrap();

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
    let objs_unset = render(&tape_unset).unwrap();

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
    let objs_set = render(&tape_set).unwrap();

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
    let objs = render(&tape).unwrap();
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
    let objs = render(&tape).unwrap();
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
    let objs1 = render(&tape1).unwrap();
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
    let objs2 = render(&Tape::new("tape", spec2)).unwrap();
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
    let objs3 = render(&Tape::new("tape", spec3)).unwrap();
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
    let objs4 = render(&Tape::new("tape", spec4)).unwrap();
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

/// #3058 E1 (extended by #243): CRD spec validation rules array contains
/// exactly four rules in stable order: the one-token-source rule first, the
/// backup-admin-token rule second, then the two peer TLS source rules.
#[test]
fn crd_has_four_spec_validation_rules_in_order() {
    let yaml = crd_yaml();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("CRD parses as YAML");
    let rules = doc["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
        ["x-kubernetes-validations"]
        .as_sequence()
        .expect("spec carries validation rules");
    assert_eq!(
        rules.len(),
        4,
        "spec schema must carry exactly four validation rules"
    );
    assert_eq!(
        rules[0]["rule"],
        "!(has(self.tokensSecret) && has(self.tokensSecretProviderClass))"
    );
    assert_eq!(
        rules[1]["rule"],
        "!(has(self.backup) && self.auth == 'required' && !has(self.backup.adminTokenSecret))"
    );
    assert_eq!(
        rules[2]["rule"],
        "!(has(self.peerTls) && has(self.peerTls.secretName) && has(self.peerTls.secretProviderClass))"
    );
    assert_eq!(
        rules[3]["rule"],
        "!has(self.peerTls) || (has(self.peerTls.secretName) != has(self.peerTls.secretProviderClass))"
    );
}

/// #3058 E2 & E3: Backup CEL rule text must match presence-only contract and
/// message must name field.
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

/// #3058 E5: The stale phrase 'the CR is accepted either way' must not appear
/// in the backup render or the generated CRD.
#[test]
fn crd_description_has_no_stale_accepted_either_way_phrase() {
    let render_src = include_str!("../../../../tape-operator/src/render/backup.rs");
    assert!(
        !render_src.contains("the CR is accepted either way"),
        "render/backup.rs must not contain the stale 'the CR is accepted either way' phrase"
    );
    let yaml = crd_yaml();
    assert!(
        !yaml.contains("the CR is accepted either way"),
        "the generated CRD must not contain the stale 'the CR is accepted either way' phrase"
    );
}
