//! The opt-in `spec.observability` children: the ServiceMonitor and the
//! PrometheusRule with tape's SLO alerts and their runbooks.

use serde_json::{json, Value};
use service_k8s::render::RenderCtx;

use super::COMPONENT;

/// Prometheus label-selector fragment scoping a *self-scraped tape* series to
/// this instance, the operator-path replacement for the static component's
/// `{app="tape",role="server"}`.
///
/// Those two labels are not intrinsic to the metric: they exist on the series
/// only because the component's ServiceMonitor grafts the Service's `app` /
/// `role` labels on via `targetLabels`. The operator labels its children with
/// the `app.kubernetes.io/*` recommended set instead, so the same trick needs
/// the same names — [`service_monitor`] grafts
/// `app.kubernetes.io/{instance,component}`, Prometheus sanitizes those to
/// `app_kubernetes_io_{instance,component}`, and the exprs select on the
/// sanitized form. Lifting the component's exprs verbatim would render, apply,
/// and evaluate cleanly while matching nothing — a permanently silent alert,
/// which is the precise failure #2575 exists to prevent.
fn series_selector(cx: &RenderCtx<'_>) -> String {
    format!(
        "namespace=\"{}\",app_kubernetes_io_instance=\"{}\",app_kubernetes_io_component=\"{}\"",
        cx.ns, cx.name, COMPONENT
    )
}

/// Static labels stamped on every alert this rule fires.
///
/// `sum(...)` without a `by` clause discards every series label, so the two
/// latency alerts would otherwise reach Alertmanager with nothing but their
/// name — the static component re-adds `app`/`role` for exactly that reason.
/// Only `[a-zA-Z_][a-zA-Z0-9_]*` is a legal Prometheus label name, so the
/// operator's dotted/slashed label keys appear here in their sanitized form,
/// matching what the un-aggregated series carry.
fn alert_labels(cx: &RenderCtx<'_>, severity: &str) -> Value {
    json!({
        "severity": severity,
        "namespace": cx.ns,
        "app_kubernetes_io_instance": cx.name,
    })
}

/// Selector label kube-prometheus-stack's default `serviceMonitorSelector` /
/// `ruleSelector` matches on (`release: <helm release name>`). Both static
/// observability objects already carry it; without it the stack's default
/// install silently ignores the rendered pair, so it is reproduced verbatim
/// rather than left to the CR author.
const PROMETHEUS_RELEASE_LABEL: &str = "prometheus";

/// Scrape config for this instance's `/metrics` (#2575), the operator-rendered
/// twin of `k8s/components/observability/servicemonitor.yaml`.
///
/// The selector matches this instance's *serving* Services — both the headless
/// and the client Service carry the component labels, so, exactly as with the
/// static component's `{app: tape, role: server}` selector against
/// `k8s/base/service.yaml`'s two Services, each pod is discovered twice. It is
/// deliberately not "fixed" here: the latency alerts divide two equally
/// doubled aggregates and are immune, and changing it would diverge the
/// operator path from the deployed static one for no alerting benefit.
pub(super) fn service_monitor(cx: &RenderCtx<'_>) -> Value {
    let mut meta = cx.meta(cx.name, COMPONENT);
    meta["labels"]["release"] = json!(PROMETHEUS_RELEASE_LABEL);
    json!({
        "apiVersion": "monitoring.coreos.com/v1",
        "kind": "ServiceMonitor",
        "metadata": meta,
        "spec": {
            "selector": { "matchLabels": cx.selector(COMPONENT) },
            // Graft the instance identity onto every scraped series; the alert
            // exprs below select on the sanitized form (see [`series_selector`]).
            "targetLabels": ["app.kubernetes.io/instance", "app.kubernetes.io/component"],
            "endpoints": [{
                "port": "http",
                "path": "/metrics",
                "interval": "15s",
                "scrapeTimeout": "10s",
                "honorLabels": true,
            }],
        },
    })
}

/// tape's four SLO alerts (#2575), the operator-rendered twin of
/// `k8s/components/observability/prometheusrule.yaml`.
///
/// Every alert reads a series tape actually publishes today.
/// `TapeAppendLatencyHigh` / `TapeReplayLatencyHigh` divide the
/// `tape_{append,replay}_latency_ms_sum` / `_count` pair `crates/tape-journal/src/application/metrics.rs`
/// records per request, `clamp_min(..., 1)` guarding the idle-window
/// zero-denominator. `TapeSubscriptionLagGrowing` reads the
/// `tape_subscription_lag{topic,subscription}` gauge `crates/tape-journal/src/application/service/exposition.rs` publishes
/// per subscription (#2485). `TapePodRestarting` is the one kube-state-metrics
/// series in the set, so it scopes by pod name and container instead — and the
/// container name is the one substantive difference from the static file,
/// which filters `container="tape"`: the shared StatefulSet helper names the
/// container after the *component*, so on this path it is `server`.
///
/// Thresholds, `for` windows, severities, summaries, and both #2485 runbooks
/// are reproduced from the static component verbatim; `crates/tape/tests/it/operator.rs`
/// holds the two documents to that.
pub(super) fn prometheus_rule(cx: &RenderCtx<'_>) -> Value {
    let s = series_selector(cx);
    let mut meta = cx.meta(cx.name, COMPONENT);
    meta["labels"]["release"] = json!(PROMETHEUS_RELEASE_LABEL);
    json!({
        "apiVersion": "monitoring.coreos.com/v1",
        "kind": "PrometheusRule",
        "metadata": meta,
        "spec": {
            "groups": [{
                "name": "tape.slo",
                "interval": "30s",
                "rules": [
                    {
                        "alert": "TapeAppendLatencyHigh",
                        "expr": format!(
                            "sum(rate(tape_append_latency_ms_sum{{{s}}}[5m])) \
                             / clamp_min(sum(rate(tape_append_latency_ms_count{{{s}}}[5m])), 1) > 500"
                        ),
                        "for": "10m",
                        "labels": alert_labels(cx, "warning"),
                        "annotations": {
                            "summary": "tape append average latency above 500ms",
                        },
                    },
                    {
                        "alert": "TapeReplayLatencyHigh",
                        "expr": format!(
                            "sum(rate(tape_replay_latency_ms_sum{{{s}}}[5m])) \
                             / clamp_min(sum(rate(tape_replay_latency_ms_count{{{s}}}[5m])), 1) > 2000"
                        ),
                        "for": "10m",
                        "labels": alert_labels(cx, "warning"),
                        "annotations": {
                            "summary": "tape replay average latency above 2s",
                        },
                    },
                    {
                        "alert": "TapePodRestarting",
                        "expr": format!(
                            "increase(kube_pod_container_status_restarts_total{{namespace=\"{}\",pod=~\"^{}-[0-9]+$\",container=\"{}\"}}[15m]) > 2",
                            cx.ns, cx.name, COMPONENT
                        ),
                        "for": "5m",
                        "labels": alert_labels(cx, "warning"),
                        "annotations": {
                            "summary": "tape pod restarting repeatedly",
                            "runbook": POD_RESTARTING_RUNBOOK,
                        },
                    },
                    {
                        // #2573. `max_over_time` on purpose: the node
                        // re-probes every 30s and clears the gauge itself, so
                        // a volume that keeps filling and draining can read 0
                        // at every scrape while rejecting writes between them.
                        // The window trades a ~5m tail after real recovery for
                        // not missing that. `critical`, not `warning`: unlike
                        // the latency alerts this one means writes are already
                        // being refused.
                        "alert": "TapeStorageDegraded",
                        "expr": format!("max_over_time(tape_storage_degraded{{{s}}}[5m]) > 0"),
                        "for": "2m",
                        "labels": alert_labels(cx, "critical"),
                        "annotations": {
                            "summary": "tape node in ENOSPC degraded read-only mode",
                            "runbook": STORAGE_DEGRADED_RUNBOOK,
                        },
                    },
                    {
                        "alert": "TapeSubscriptionLagGrowing",
                        "expr": format!("increase(tape_subscription_lag{{{s}}}[15m]) > 0"),
                        "for": "15m",
                        "labels": alert_labels(cx, "warning"),
                        "annotations": {
                            "summary": "tape subscription lag growing over 15m",
                            "runbook": SUBSCRIPTION_LAG_RUNBOOK,
                        },
                    },
                    {
                        // #3051: Memory headroom alert fires before the limit is
                        // reached. Both series come from cAdvisor and carry identical
                        // labels, so they divide directly. Guard divide-by-zero by
                        // filtering the denominator: if no limit is set, the container
                        // is unbounded and the gauge is 0, so the division yields +Inf
                        // and the alert fires permanently.
                        "alert": "TapeMemoryHeadroomLow",
                        "expr": format!(
                            "max by (pod) (\n\
                             container_memory_working_set_bytes{{namespace=\"{}\",pod=~\"^{}-[0-9]+$\",container=\"{}\"}}\n\
                             / (container_spec_memory_limit_bytes{{namespace=\"{}\",pod=~\"^{}-[0-9]+$\",container=\"{}\"}} > 0)\n\
                             ) > 0.85",
                            cx.ns, cx.name, COMPONENT,
                            cx.ns, cx.name, COMPONENT
                        ),
                        "for": "10m",
                        "labels": alert_labels(cx, "warning"),
                        "annotations": {
                            "summary": "tape container memory headroom below 15%",
                            "runbook": MEMORY_HEADROOM_RUNBOOK,
                        },
                    },
                ],
            }],
        },
    })
}

/// #2485's seed-failure triage, verbatim from the static component. A restart
/// loop on a CR carrying `spec.bootstrapSeedUri` is ambiguous until the log
/// decision field is read, so the runbook's job is to make the three outcomes
/// distinguishable rather than to describe the symptom.
const POD_RESTARTING_RUNBOOK: &str = "#2485: Differentiate seed-failure restarts by checking if `spec.bootstrapSeedUri` is set on the CR and examining the pod log for structured decision fields: (1) `decision=\"seeded\"` = seed succeeded, look for other causes (probe failures, image pull, resource limits); (2) `decision=\"skipped_existing_state\"` = seed was skipped (PVC already had data), pod is healthy; (3) NEITHER line present before a crash = seed fetch/decode failed (bad URI, IAM, corrupt object) — restore logs show which. #2468: the one-shot seed cleared bit is separate. See docs/deployment-handoff.md Cold restore runbook.";

/// #2485's consumer-liveness triage, verbatim from the static component.
/// Growing lag is only sometimes a fault — the runbook exists to separate a
/// dead consumer from a fast producer.
const SUBSCRIPTION_LAG_RUNBOOK: &str = "#2485: Check if the consumer bound to the subscription is alive and actively pulling. If the consumer has stopped or stalled, verify it has not crashed or exhausted resources. If the consumer is healthy, check the append rate to the topic — a rapid append rate combined with a slow consumer will naturally grow lag. No action needed if this is expected; adjust retention policy if needed to protect the subscription's checkpoint from expiry.";

/// #2573's ENOSPC triage, verbatim from the static component. The alert on its
/// own tells an operator writes are being refused but not that the node will
/// recover itself — the runbook's job is to stop a reflexive pod restart and
/// point at the two things that actually decide the outcome: capacity, and the
/// flap counter that distinguishes "recovered" from "recovering every 30s".
const STORAGE_DEGRADED_RUNBOOK: &str = "#2573: The node hit ENOSPC on its journal persist path and latched degraded read-only mode — mutating requests answer 507 `storage_full`, reads keep serving. Check `tape_storage_full_errors_total`: a rising counter with the gauge back at 0 means the volume is flapping in and out of full, not that it recovered. Remedy is capacity — free objects via retention or expand the PVC on a resizable StorageClass. No restart is needed: the pod re-probes the store directory every `TAPE_STORAGE_FULL_REPROBE_SECS` (default 30s) and clears the flag itself. If the gauge stays 1 after the volume has room, read the pod log for the re-probe warning — the store directory can be unwritable for reasons other than capacity (read-only remount, permissions).";

/// #3051 / #3052: Memory headroom runbook.
///
/// The one thing this text has to stop is the reflex fix. Raising the limit
/// clears the alert and restores exactly the silent growth the limit was added
/// to expose, so the runbook names the two causes that are actionable now
/// (unbounded retention, a stalled consumer) and points the structural one at
/// #3052 rather than at the person holding the pager.
///
/// It states no amplification ratio. The measured figure is from a macOS/APFS
/// host and has not been re-taken on Linux; an unverified multiplier in a
/// runbook is read as a fact about the cluster in front of you.
const MEMORY_HEADROOM_RUNBOOK: &str = "#3051: The container's memory working set is within 15% of its limit, and the limit equals the request, so the next step is an OOMKill of this pod. Do NOT raise the limit as the remedy — it defers the wall and hides the growth again; the limit exists to make this growth visible. Act on the two causes you can fix now: (1) retention — `tape retention list` (or the CR's `spec.retention`); a topic with no entry is NEVER pruned and grows without bound, so give every active topic a byte or count bound; (2) a stalled consumer — check `tape_subscription_lag` and whether the bound consumer is still pulling, since a checkpoint that stops advancing pins events that retention would otherwise drop. If neither applies, the pod is holding more memory than its journal justifies: this is the whole-journal-rewrite persist path (`AppState::persist` serializes the entire journal on every mutation), which #3052 (WAL + group commit) removes. Capacity sizing is #2552 — do not derive a new limit from this alert alone.";
