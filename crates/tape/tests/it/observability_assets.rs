//! Offline contract checks for the optional Prometheus Operator component.

use serde_yaml::Value;

fn yaml(path: &str) -> Value {
    serde_yaml::from_str(path).expect("observability manifest parses")
}

#[test]
fn servicemonitor_scrapes_metrics_and_preserves_service_labels() {
    let doc = yaml(include_str!(
        "../../../../k8s/components/observability/servicemonitor.yaml"
    ));
    assert_eq!(doc["kind"], "ServiceMonitor");
    assert_eq!(doc["spec"]["endpoints"][0]["path"], "/metrics");
    assert_eq!(doc["spec"]["targetLabels"][0], "app");
    assert_eq!(doc["spec"]["targetLabels"][1], "role");
}

#[test]
fn alert_rules_only_reference_existing_tape_latency_series() {
    let rule = include_str!("../../../../k8s/components/observability/prometheusrule.yaml");
    let doc = yaml(rule);
    assert_eq!(doc["kind"], "PrometheusRule");
    for metric in [
        "tape_append_latency_ms_sum",
        "tape_append_latency_ms_count",
        "tape_replay_latency_ms_sum",
        "tape_replay_latency_ms_count",
        // #2573 — the degraded-mode gauge `TapeMetrics::render` publishes.
        "tape_storage_degraded",
        // #2579 — the raft leader-known gauge `TapeMetrics::render` publishes.
        "tape_raft_leader_known",
        // #2580 — auth registry reload metrics `TapeMetrics::render` publishes.
        "tape_auth_registry_reload_failures_total",
        "tape_auth_registry_reload_success_unixtime",
    ] {
        assert!(rule.contains(metric), "missing real Tape metric {metric}");
    }
}

/// #3051 — memory headroom alert references cAdvisor series, not tape-scraped
/// metrics. cAdvisor series are published by kubelet and carried by kube-state-
/// metrics / Prometheus Operator's default scrape config; they are not `tape_*`
/// names so this is a separate pinned assertion.
#[test]
fn alert_rules_reference_cadvisor_memory_series() {
    let rule = include_str!("../../../../k8s/components/observability/prometheusrule.yaml");
    for metric in [
        "container_memory_working_set_bytes",
        "container_spec_memory_limit_bytes",
    ] {
        assert!(
            rule.contains(metric),
            "TapeMemoryHeadroomLow must reference cAdvisor series {metric}"
        );
    }
}

/// #2578 — the new alerts read kube-state-metrics and kubelet series, so the
/// existing `tape_*` inclusion test cannot cover them.
#[test]
fn alert_rules_reference_kube_state_and_kubelet_series() {
    let rule = include_str!("../../../../k8s/components/observability/prometheusrule.yaml");
    for metric in [
        "kube_statefulset_status_replicas_ready",
        "kube_job_status_failed",
        "kubelet_volume_stats_available_bytes",
        "kubelet_volume_stats_capacity_bytes",
    ] {
        assert!(
            rule.contains(metric),
            "the new #2578 alerts must reference {metric}"
        );
    }
}

/// #3053 AC4 — the operator's ServiceMonitor and alerts ship as the opt-in
/// `operator-monitoring` component, never in the base layer: an
/// `apply -k k8s/operator` that held a `monitoring.coreos.com` object would
/// fail on any cluster without prometheus-operator, and take the operator
/// install down with it. The ClusterRole may still grant that group's verbs;
/// only installed objects count.
#[test]
fn operator_monitoring_is_a_component_and_the_operator_layer_stays_crd_free() {
    use serde::Deserialize;

    fn kinds(text: &str) -> Vec<(String, String)> {
        serde_yaml::Deserializer::from_str(text)
            .map(|doc| Value::deserialize(doc).expect("operator manifest parses"))
            .filter(|doc| !doc.is_null())
            .map(|doc| {
                (
                    doc["apiVersion"].as_str().unwrap_or_default().to_owned(),
                    doc["kind"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    let component = yaml(include_str!(
        "../../../../k8s/components/operator-monitoring/kustomization.yaml"
    ));
    assert_eq!(component["kind"], "Component");
    let mut monitoring = kinds(include_str!(
        "../../../../k8s/components/operator-monitoring/servicemonitor.yaml"
    ));
    monitoring.extend(kinds(include_str!(
        "../../../../k8s/components/operator-monitoring/prometheusrule.yaml"
    )));
    for want in ["ServiceMonitor", "PrometheusRule"] {
        assert!(
            monitoring
                .iter()
                .any(|(api, kind)| api.starts_with("monitoring.coreos.com/") && kind == want),
            "operator-monitoring renders no {want}: {monitoring:?}"
        );
    }

    let layer = yaml(include_str!("../../../../k8s/operator/kustomization.yaml"));
    let listed: Vec<&str> = layer["resources"]
        .as_sequence()
        .expect("k8s/operator lists resources")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let files = [
        (
            "crd.yaml",
            include_str!("../../../../k8s/operator/crd.yaml"),
        ),
        (
            "rbac.yaml",
            include_str!("../../../../k8s/operator/rbac.yaml"),
        ),
        (
            "deployment.yaml",
            include_str!("../../../../k8s/operator/deployment.yaml"),
        ),
        (
            "service.yaml",
            include_str!("../../../../k8s/operator/service.yaml"),
        ),
        (
            "pdb.yaml",
            include_str!("../../../../k8s/operator/pdb.yaml"),
        ),
    ];
    assert_eq!(
        listed,
        files.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "k8s/operator/kustomization.yaml resources changed; check them here too"
    );
    for (name, text) in files {
        for (api, kind) in kinds(text) {
            assert!(
                !api.starts_with("monitoring.coreos.com/"),
                "k8s/operator/{name} installs {api} {kind}"
            );
        }
    }
}
