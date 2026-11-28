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
/// install down with it. The base ClusterRole no longer grants that group
/// either; the component patch does.
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
    assert_eq!(
        listed,
        ["namespace", "cluster", "namespaced"],
        "k8s/operator/kustomization.yaml resources changed; check them here too"
    );

    // Each base lists its own files; check the files it lists, not the old flat set.
    // (base, its kustomization.yaml, its files as (file name, text)).
    type BaseFiles = &'static [(&'static str, &'static str)];
    let bases: [(&str, &str, BaseFiles); 3] = [
        (
            "namespace",
            include_str!("../../../../k8s/operator/namespace/kustomization.yaml"),
            &[(
                "namespace.yaml",
                include_str!("../../../../k8s/operator/namespace/namespace.yaml"),
            )],
        ),
        (
            "cluster",
            include_str!("../../../../k8s/operator/cluster/kustomization.yaml"),
            &[
                (
                    "crd.yaml",
                    include_str!("../../../../k8s/operator/cluster/crd.yaml"),
                ),
                (
                    "clusterrole.yaml",
                    include_str!("../../../../k8s/operator/cluster/clusterrole.yaml"),
                ),
                (
                    "clusterrolebinding.yaml",
                    include_str!("../../../../k8s/operator/cluster/clusterrolebinding.yaml"),
                ),
            ],
        ),
        (
            "namespaced",
            include_str!("../../../../k8s/operator/namespaced/kustomization.yaml"),
            &[
                (
                    "serviceaccount.yaml",
                    include_str!("../../../../k8s/operator/namespaced/serviceaccount.yaml"),
                ),
                (
                    "deployment.yaml",
                    include_str!("../../../../k8s/operator/namespaced/deployment.yaml"),
                ),
                (
                    "service.yaml",
                    include_str!("../../../../k8s/operator/namespaced/service.yaml"),
                ),
                (
                    "pdb.yaml",
                    include_str!("../../../../k8s/operator/namespaced/pdb.yaml"),
                ),
            ],
        ),
    ];
    for (base, kustomization, files) in bases {
        let base_doc = yaml(kustomization);
        let names: Vec<&str> = base_doc["resources"]
            .as_sequence()
            .expect("operator base lists resources")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            names,
            files.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            "k8s/operator/{base}/kustomization.yaml resources changed; check them here too"
        );
        for &(name, text) in files {
            for (api, kind) in kinds(text) {
                assert!(
                    !api.starts_with("monitoring.coreos.com/"),
                    "k8s/operator/{base}/{name} installs {api} {kind}"
                );
            }
        }
    }
}

/// Parse every document of a manifest file, skipping empty ones.
fn documents(text: &str) -> Vec<Value> {
    use serde::Deserialize;

    serde_yaml::Deserializer::from_str(text)
        .map(|doc| Value::deserialize(doc).expect("operator manifest parses"))
        .filter(|doc| !doc.is_null())
        .collect()
}

/// Kind of each document, in file order.
fn kind_names(docs: &[Value]) -> Vec<String> {
    docs.iter()
        .map(|doc| doc["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The operator can limit its watch by env var, but the shipped manifest only
/// documents the var as a comment: the default stays cluster-wide.
#[test]
fn operator_deployment_documents_the_watch_namespaces_env_but_keeps_it_off() {
    let text = include_str!("../../../../k8s/operator/namespaced/deployment.yaml");
    assert!(
        text.contains("# - name: TAPE_OPERATOR_WATCH_NAMESPACES"),
        "deployment lacks the commented TAPE_OPERATOR_WATCH_NAMESPACES example"
    );
    let docs = documents(text);
    let deployment = docs
        .iter()
        .find(|doc| doc["kind"].as_str() == Some("Deployment"))
        .expect("namespaced deployment present");
    let env = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_sequence()
        .expect("container env list");
    assert!(
        env.iter()
            .all(|entry| entry["name"].as_str() != Some("TAPE_OPERATOR_WATCH_NAMESPACES")),
        "TAPE_OPERATOR_WATCH_NAMESPACES must stay commented out"
    );
}

/// The operator install is split by scope: the cluster-scoped identity applies
/// `cluster/`, the namespaced identity applies `namespaced/`, and `namespace/`
/// is opt-in (issue #232). A namespaced apply must not hold RBAC, a Namespace
/// or a CRD, because the least-privileged identity cannot create them.
#[test]
fn operator_install_splits_by_scope() {
    let cluster: Vec<Value> = [
        include_str!("../../../../k8s/operator/cluster/crd.yaml"),
        include_str!("../../../../k8s/operator/cluster/clusterrole.yaml"),
        include_str!("../../../../k8s/operator/cluster/clusterrolebinding.yaml"),
    ]
    .into_iter()
    .flat_map(documents)
    .collect();
    assert_eq!(
        kind_names(&cluster),
        [
            "CustomResourceDefinition",
            "ClusterRole",
            "ClusterRoleBinding"
        ],
        "cluster/ must hold only cluster-scoped objects"
    );

    let namespaced = documents(include_str!(
        "../../../../k8s/operator/namespaced/serviceaccount.yaml"
    ))
    .into_iter()
    .chain(documents(include_str!(
        "../../../../k8s/operator/namespaced/deployment.yaml"
    )))
    .chain(documents(include_str!(
        "../../../../k8s/operator/namespaced/service.yaml"
    )))
    .chain(documents(include_str!(
        "../../../../k8s/operator/namespaced/pdb.yaml"
    )))
    .collect::<Vec<Value>>();
    assert_eq!(
        kind_names(&namespaced),
        [
            "ServiceAccount",
            "Deployment",
            "Service",
            "PodDisruptionBudget"
        ],
        "namespaced/ must hold exactly the namespaced objects"
    );
    for doc in &namespaced {
        let kind = doc["kind"].as_str().unwrap_or_default();
        assert_eq!(
            doc["metadata"]["namespace"].as_str(),
            Some("tape-system"),
            "namespaced/ {kind} must live in tape-system"
        );
        assert!(
            !matches!(
                kind,
                "Namespace" | "ClusterRole" | "ClusterRoleBinding" | "CustomResourceDefinition"
            ),
            "namespaced/ must not hold a cluster-scoped {kind}"
        );
    }

    let namespace = documents(include_str!(
        "../../../../k8s/operator/namespace/namespace.yaml"
    ));
    assert_eq!(kind_names(&namespace), ["Namespace"]);
    assert_eq!(
        namespace[0]["metadata"]["name"].as_str(),
        Some("tape-system")
    );
}

/// #238 — the base ClusterRole no longer grants `monitoring.coreos.com`. The
/// opt-in component patch appends that rule, and `tape k8s operator render
/// --monitoring` reads the same patch text.
#[test]
fn base_clusterrole_has_no_monitoring_rule_and_component_patch_adds_it() {
    let role = yaml(include_str!(
        "../../../../k8s/operator/cluster/clusterrole.yaml"
    ));
    let rules = role["rules"].as_sequence().expect("ClusterRole has rules");
    let groups_of = |rule: &Value| -> Vec<String> {
        rule["apiGroups"]
            .as_sequence()
            .map(|gs| {
                gs.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    for rule in rules {
        assert!(
            !groups_of(rule).iter().any(|g| g == "monitoring.coreos.com"),
            "base ClusterRole grants monitoring.coreos.com: {rule:?}"
        );
    }
    // Removing the monitoring rule must not drop any other group.
    for group in [
        "tape.dev",
        "apps",
        "batch",
        "policy",
        "coordination.k8s.io",
        "events.k8s.io",
    ] {
        assert!(
            rules
                .iter()
                .any(|rule| groups_of(rule).iter().any(|g| g == group)),
            "base ClusterRole lost the {group} rule"
        );
    }

    let patch = yaml(include_str!(
        "../../../../k8s/components/operator-monitoring/clusterrole-patch.yaml"
    ));
    let ops = patch.as_sequence().expect("patch is a sequence");
    assert_eq!(ops.len(), 1, "one JSON6902 operation");
    assert_eq!(ops[0]["op"], "add");
    assert_eq!(ops[0]["path"], "/rules/-");
    assert_eq!(ops[0]["value"]["apiGroups"][0], "monitoring.coreos.com");
    assert_eq!(ops[0]["value"]["resources"][0], "servicemonitors");
    assert_eq!(ops[0]["value"]["resources"][1], "prometheusrules");
    let verbs: Vec<&str> = ops[0]["value"]["verbs"]
        .as_sequence()
        .expect("patch verbs")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        verbs,
        ["get", "list", "watch", "create", "update", "patch", "delete"]
    );

    let component = yaml(include_str!(
        "../../../../k8s/components/operator-monitoring/kustomization.yaml"
    ));
    let patches = component["patches"]
        .as_sequence()
        .expect("component lists patches");
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0]["path"], "clusterrole-patch.yaml");
    assert_eq!(patches[0]["target"]["kind"], "ClusterRole");
    assert_eq!(patches[0]["target"]["name"], "tape-operator");
}

/// #239: the opt-in scoped RBAC base lists its files, keeps the CRD out of the
/// base (kustomize refuses files outside a base), and grants the child objects
/// in one Role per watched namespace, with no monitoring rule.
#[test]
fn scoped_operator_base_lists_its_files_and_stays_crd_free() {
    let base = yaml(include_str!(
        "../../../../k8s/operator/scoped/kustomization.yaml"
    ));
    assert_eq!(base["kind"], "Kustomization");
    let listed: Vec<&str> = base["resources"]
        .as_sequence()
        .expect("k8s/operator/scoped lists resources")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        listed,
        [
            "../namespace",
            "../namespaced",
            "clusterrole-watch.yaml",
            "clusterrolebinding-watch.yaml",
            "role-lease.yaml",
            "rolebinding-lease.yaml",
            "role-children.yaml",
            "rolebinding-children.yaml",
        ]
    );

    let files = [
        include_str!("../../../../k8s/operator/scoped/clusterrole-watch.yaml"),
        include_str!("../../../../k8s/operator/scoped/clusterrolebinding-watch.yaml"),
        include_str!("../../../../k8s/operator/scoped/role-lease.yaml"),
        include_str!("../../../../k8s/operator/scoped/rolebinding-lease.yaml"),
        include_str!("../../../../k8s/operator/scoped/role-children.yaml"),
        include_str!("../../../../k8s/operator/scoped/rolebinding-children.yaml"),
    ];
    let mut all = Vec::new();
    for text in files {
        assert!(
            !text.contains("monitoring.coreos.com"),
            "scoped RBAC file grants monitoring.coreos.com"
        );
        all.extend(documents(text));
    }
    assert_eq!(
        kind_names(&all),
        [
            "ClusterRole",
            "ClusterRoleBinding",
            "Role",
            "RoleBinding",
            "Role",
            "RoleBinding",
        ]
    );

    let children = &documents(include_str!(
        "../../../../k8s/operator/scoped/role-children.yaml"
    ))[0];
    assert_eq!(children["metadata"]["namespace"], "tape");
    let granted: Vec<String> = children["rules"]
        .as_sequence()
        .expect("children Role has rules")
        .iter()
        .flat_map(|r| r["resources"].as_sequence().cloned().unwrap_or_default())
        .filter_map(|r| r.as_str().map(str::to_owned))
        .collect();
    for want in [
        "statefulsets",
        "cronjobs",
        "services",
        "serviceaccounts",
        "poddisruptionbudgets",
        "events",
    ] {
        assert!(
            granted.iter().any(|g| g == want),
            "children Role lacks {want}"
        );
    }
}
