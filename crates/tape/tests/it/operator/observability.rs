use super::*;

/// The hand-maintained observability component the operator pair mirrors.
const STATIC_COMPONENT: &str =
    include_str!("../../../../../k8s/components/observability/prometheusrule.yaml");

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

    let off = kinds(&render(&Tape::new("tape", spec(3))).unwrap());
    assert!(
        !off.contains(&"ServiceMonitor".to_string())
            && !off.contains(&"PrometheusRule".to_string()),
        "a CR that never asked for observability must not require the \
         Prometheus Operator CRDs, got {off:?}"
    );

    let mut watched = spec(3);
    watched.observability = true;
    let objects = render(&Tape::new("tape", watched)).unwrap();
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
    let objects = render(&Tape::new("tape", watched)).unwrap();
    let rendered = of_kind(&objects, "PrometheusRule");

    assert_eq!(
        contract(rendered),
        contract(&file),
        "the operator-rendered PrometheusRule has drifted from \
         k8s/components/observability/prometheusrule.yaml — keep the two in step \
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
    /// The helper returns every comparison as `"<op> <literal>"`, not a bare
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
        let mut filtered = Vec::new();
        let mut depth = 0usize;
        for ch in expr.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ if depth == 0 => filtered.push(ch),
                _ => {}
            }
        }

        let mut out = Vec::new();
        let mut i = 0usize;
        while i < filtered.len() {
            let next = filtered.get(i + 1).copied();
            let op = match (filtered[i], next) {
                ('>', Some('=')) => Some(">="),
                ('<', Some('=')) => Some("<="),
                ('=', Some('=')) => Some("=="),
                ('!', Some('=')) => Some("!="),
                ('>', _) => Some(">"),
                ('<', _) => Some("<"),
                _ => None,
            };

            let Some(op) = op else {
                i += 1;
                continue;
            };
            let mut j = i + op.len();
            while j < filtered.len() && filtered[j].is_whitespace() {
                j += 1;
            }
            let mut literal = String::new();
            while j < filtered.len() && (filtered[j].is_ascii_digit() || filtered[j] == '.') {
                literal.push(filtered[j]);
                j += 1;
            }
            if !literal.is_empty() {
                out.push(format!("{op} {literal}"));
            }
            i = j;
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
    let objects = render(&Tape::new("tape", watched)).unwrap();

    let static_exprs = exprs(&file);
    let rendered_exprs = exprs(of_kind(&objects, "PrometheusRule"));

    // #2578: pin the real firing threshold of every alert, not only the
    // cross-check between the two files — a helper that reads nothing from
    // either side would otherwise pass that comparison as `[] == []`.
    let expected_thresholds: BTreeMap<&str, Vec<String>> = BTreeMap::from([
        ("TapeAppendLatencyHigh", vec!["> 500".to_string()]),
        ("TapeReplayLatencyHigh", vec!["> 2000".to_string()]),
        ("TapePodRestarting", vec!["> 2".to_string()]),
        ("TapeStorageDegraded", vec!["> 0".to_string()]),
        ("TapeAuthRegistryReloadFailing", vec!["> 0".to_string()]),
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
    let objects = render(&Tape::new("tape", watched)).unwrap();

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
