use super::{prunes, render};
use crate::{Tape, TapeSpec};
use serde_json::{json, Value};

fn instance(overrides: Value) -> Tape {
    let mut spec = json!({
        "image": "unit.invalid/tape:fixture",
        "shardCount": 9,
        "replicasPerShard": 3,
        "voterCount": 3,
        "resources": { "cpu": "250m", "memory": "512Mi" }
    });
    spec.as_object_mut()
        .expect("spec object")
        .extend(overrides.as_object().expect("override object").clone());
    let spec: TapeSpec = serde_json::from_value(spec).expect("valid Tape spec");
    let mut tape = Tape::new("ledger", spec);
    tape.metadata.namespace = Some("test-ns".into());
    tape.metadata.uid = Some("owner-uid".into());
    tape
}

fn object<'a>(objects: &'a [Value], kind: &str, name: &str) -> &'a Value {
    objects
        .iter()
        .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("missing {kind}/{name}"))
}

fn container(statefulset: &Value) -> &Value {
    &statefulset["spec"]["template"]["spec"]["containers"][0]
}

fn env<'a>(container: &'a Value, name: &str) -> Option<&'a Value> {
    container["env"]
        .as_array()
        .expect("environment array")
        .iter()
        .find(|variable| variable["name"] == name)
}

#[test]
fn durable_workload_keeps_single_group_disk_and_shutdown_contract() {
    let tape = instance(json!({
        "storage": "4Gi",
        "storageClass": "dynamic-rwo",
        "graceSecs": 11
    }));
    let objects = render(&tape);
    let sts = object(&objects, "StatefulSet", "ledger");
    assert_eq!(sts["spec"]["replicas"], 3);
    assert_eq!(
        sts["spec"]["template"]["spec"]["terminationGracePeriodSeconds"],
        16
    );
    let claim = &sts["spec"]["volumeClaimTemplates"][0];
    assert_eq!(claim["spec"]["resources"]["requests"]["storage"], "4Gi");
    assert_eq!(claim["spec"]["storageClassName"], "dynamic-rwo");
    assert_eq!(claim["spec"]["accessModes"], json!(["ReadWriteOnce"]));
    let serving = container(sts);
    assert_eq!(env(serving, "TAPE_DATA_DIR").unwrap()["value"], "/data");
    assert_eq!(env(serving, "TAPE_GRACE_SECS").unwrap()["value"], "11");
    assert_eq!(
        env(serving, "TAPE_PEER_SERVICE").unwrap()["value"],
        "ledger-headless"
    );
    assert_eq!(serving["resources"]["limits"]["memory"], "512Mi");
    assert!(serving["resources"]["limits"].get("cpu").is_none());
}

#[test]
fn owned_children_keep_service_identity_selectors_and_ports() {
    let objects = render(&instance(json!({})));
    assert_eq!(objects.len(), 6);
    for child in &objects {
        assert_eq!(child["metadata"]["namespace"], "test-ns");
        let owners = child["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0]["apiVersion"], "tape.dev/v1alpha1");
        assert_eq!(owners[0]["kind"], "Tape");
        assert_eq!(owners[0]["name"], "ledger");
        assert_eq!(owners[0]["uid"], "owner-uid");
        assert_eq!(owners[0]["controller"], true);
    }
    let sts = object(&objects, "StatefulSet", "ledger");
    let pod_labels = &sts["spec"]["template"]["metadata"]["labels"];
    for name in ["ledger", "ledger-headless"] {
        let service = object(&objects, "Service", name);
        let selector = service["spec"]["selector"].as_object().unwrap();
        assert!(!selector.is_empty());
        for (key, value) in selector {
            assert_eq!(&pod_labels[key.as_str()], value);
        }
    }
    let client = object(&objects, "Service", "ledger");
    assert_eq!(client["spec"]["ports"][0]["port"], 7137);
    let peers = object(&objects, "Service", "ledger-headless");
    assert_eq!(peers["spec"]["clusterIP"], "None");
    assert_eq!(peers["spec"]["ports"][0]["port"], 7137);
    assert_eq!(peers["spec"]["ports"][1]["port"], 7138);
    assert_eq!(
        object(&objects, "PodDisruptionBudget", "ledger")["spec"]["maxUnavailable"],
        1
    );
}

#[test]
fn registry_projection_defaults_to_required_and_rejects_ambiguous_sources() {
    let valid = render(&instance(json!({ "tokensSecret": "registry" })));
    let sts = object(&valid, "StatefulSet", "ledger");
    let serving = container(sts);
    assert_eq!(env(serving, "TAPE_AUTH").unwrap()["value"], "required");
    assert_eq!(
        env(serving, "TAPE_TOKEN_REGISTRY_FILE").unwrap()["value"],
        "/var/run/secrets/tape/token-registry.json"
    );
    let pod = &sts["spec"]["template"]["spec"];
    let volume = pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == "tape-token-registry")
        .unwrap();
    assert_eq!(volume["secret"]["secretName"], "registry");
    let mount = serving["volumeMounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mount| mount["name"] == "tape-token-registry")
        .unwrap();
    assert_eq!(mount["mountPath"], "/var/run/secrets/tape");
    assert_eq!(mount["readOnly"], true);

    // A locally constructed invalid CR must fail closed as well as an API
    // server rejecting the mutually exclusive fields through its schema.
    let invalid = render(&instance(json!({
        "tokensSecret": "registry",
        "tokensSecretProviderClass": "external-registry"
    })));
    let sts = object(&invalid, "StatefulSet", "ledger");
    let serving = container(sts);
    assert_eq!(env(serving, "TAPE_AUTH").unwrap()["value"], "required");
    assert!(env(serving, "TAPE_TOKEN_REGISTRY_FILE").is_none());
    assert!(!sts["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|volume| volume["name"] == "tape-token-registry"));
}

#[test]
fn backup_toggle_keeps_identity_and_prunes_only_the_cronjob() {
    let tape = instance(json!({}));
    let plain = render(&tape);
    object(&plain, "ServiceAccount", "ledger-backup");
    assert!(!plain.iter().any(|child| child["kind"] == "CronJob"));
    let targets = prunes(&tape);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].api_version, "batch/v1");
    assert_eq!(targets[0].kind, "CronJob");
    assert_eq!(targets[0].name, "ledger-backup");

    let scheduled = instance(json!({
        "tokensSecret": "registry",
        "placement": { "nodeSelector": { "faberline.dev/pool": "tape-server" } },
        "imagePullSecrets": [{ "name": "registry-pull" }],
        "backup": {
            "schedule": "15 * * * *",
            "destination": "gs://unit-bucket/ledger",
            "retentionSecs": 3600,
            "adminTokenSecret": "backup-admin"
        }
    }));
    let objects = render(&scheduled);
    assert!(prunes(&scheduled).is_empty());
    object(&objects, "ServiceAccount", "ledger-backup");
    let cron = object(&objects, "CronJob", "ledger-backup");
    assert_eq!(cron["spec"]["schedule"], "15 * * * *");
    let backup_pod = &cron["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    assert_eq!(backup_pod["serviceAccountName"], "ledger-backup");
    assert_eq!(
        backup_pod["containers"][0]["args"],
        json!([
            "backup",
            "--url",
            "http://ledger.test-ns.svc.cluster.local:7137",
            "--dest",
            "gs://unit-bucket/ledger",
            "--retention-secs",
            "3600"
        ])
    );
    assert_eq!(
        backup_pod["nodeSelector"],
        json!({ "faberline.dev/pool": "tape-server" })
    );
    assert_eq!(
        backup_pod["imagePullSecrets"],
        json!([{ "name": "registry-pull" }])
    );
    assert_eq!(
        env(&backup_pod["containers"][0], "TAPE_BACKUP_TOKEN").unwrap()["valueFrom"]
            ["secretKeyRef"]["name"],
        "backup-admin"
    );
}
