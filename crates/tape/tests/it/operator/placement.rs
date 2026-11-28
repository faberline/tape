use super::*;

// ---- spec.placement / spec.imagePullSecrets --------------------------------

fn pool_placement() -> TapePlacement {
    TapePlacement {
        node_selector: [("faberline.dev/pool".to_string(), "tape-server".to_string())].into(),
        tolerations: vec![Toleration {
            key: Some("faberline.dev/pool".into()),
            operator: Some("Equal".into()),
            value: Some("tape-server".into()),
            effect: Some("NoSchedule".into()),
            toleration_seconds: None,
        }],
    }
}

fn placed_spec() -> TapeSpec {
    let mut s = spec(3);
    s.placement = Some(pool_placement());
    s.image_pull_secrets = vec![LocalObjectReference {
        name: "ghcr-pull".into(),
    }];
    s.backup = Some(configured_backup());
    s
}

fn statefulset_pod_spec(objs: &[Value]) -> &Value {
    &of_kind(objs, "StatefulSet")["spec"]["template"]["spec"]
}

fn cron_job_pod_spec(objs: &[Value]) -> &Value {
    &of_kind(objs, "CronJob")["spec"]["jobTemplate"]["spec"]["template"]["spec"]
}

/// Unset placement and pull secrets render exactly the previous pod specs: no
/// nodeSelector, no tolerations, no imagePullSecrets on either workload.
#[test]
fn placement_unset_renders_no_scheduling_constraints() {
    let mut s = spec(3);
    s.backup = Some(configured_backup());
    let objs = render(&Tape::new("tape", s)).unwrap();
    for (kind, pod) in [
        ("StatefulSet", statefulset_pod_spec(&objs)),
        ("CronJob", cron_job_pod_spec(&objs)),
    ] {
        for field in ["nodeSelector", "tolerations", "imagePullSecrets"] {
            assert!(
                pod.get(field)
                    .is_none_or(|v| v.is_null() || v == &serde_json::json!([])),
                "{kind} pod spec carries `{field}` with no placement configured: {pod}"
            );
        }
    }
}

/// `spec.placement` and `spec.imagePullSecrets` reach every operator-rendered
/// pod: on a tainted, dedicated pool a pod missing either stays Pending or
/// fails to pull.
#[test]
fn placement_and_pull_secrets_reach_every_rendered_pod() {
    let objs = render(&Tape::new("tape", placed_spec())).unwrap();
    let expected_tolerations = serde_json::json!([{
        "key": "faberline.dev/pool",
        "operator": "Equal",
        "value": "tape-server",
        "effect": "NoSchedule",
    }]);
    for (kind, pod) in [
        ("StatefulSet", statefulset_pod_spec(&objs)),
        ("CronJob", cron_job_pod_spec(&objs)),
    ] {
        assert_eq!(
            pod["nodeSelector"],
            serde_json::json!({ "faberline.dev/pool": "tape-server" }),
            "{kind} nodeSelector"
        );
        assert_eq!(
            pod["tolerations"], expected_tolerations,
            "{kind} tolerations"
        );
        assert_eq!(
            pod["imagePullSecrets"],
            serde_json::json!([{ "name": "ghcr-pull" }]),
            "{kind} imagePullSecrets"
        );
    }
}

/// Placement is layered on top of the operator-owned one-pod-per-node
/// anti-affinity and never replaces it.
#[test]
fn placement_keeps_the_dedicated_node_anti_affinity() {
    let plain = render(&Tape::new("tape", spec(3))).unwrap();
    let placed = render(&Tape::new("tape", placed_spec())).unwrap();
    assert_eq!(
        statefulset_pod_spec(&placed)["affinity"],
        statefulset_pod_spec(&plain)["affinity"]
    );
    assert!(
        !statefulset_pod_spec(&placed)["affinity"]["podAntiAffinity"].is_null(),
        "the StatefulSet lost its podAntiAffinity"
    );
}

/// The CR shape an overlay writes deserializes into the typed fields, and the
/// committed CRD declares them so the API server does not prune them.
#[test]
fn placement_fields_round_trip_through_the_cr_and_the_crd() {
    let cr: Tape = serde_json::from_value(serde_json::json!({
        "apiVersion": "tape.dev/v1alpha1",
        "kind": "Tape",
        "metadata": { "name": "tape" },
        "spec": {
            "image": "tape:test",
            "imagePullSecrets": [{ "name": "ghcr-pull" }],
            "placement": {
                "nodeSelector": { "faberline.dev/pool": "tape-server" },
                "tolerations": [{
                    "key": "faberline.dev/pool",
                    "operator": "Equal",
                    "value": "tape-server",
                    "effect": "NoSchedule",
                }],
            },
        },
    }))
    .expect("placement CR deserializes");
    assert_eq!(cr.spec.placement, Some(pool_placement()));
    assert_eq!(cr.spec.image_pull_secrets[0].name, "ghcr-pull");

    let committed = include_str!("../../../../../k8s/operator/cluster/crd.yaml");
    for field in [
        "imagePullSecrets",
        "placement",
        "nodeSelector",
        "tolerations",
    ] {
        assert!(
            committed.contains(field),
            "committed CRD is missing `{field}` — the API server would prune it"
        );
    }
}
