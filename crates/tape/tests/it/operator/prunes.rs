use super::*;

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

/// The regression guard for the defect a live cluster caught: a `PruneTarget`
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
                BUILT_IN.contains(&target.api_version()),
                "prunes() names {}/{} in group `{}`, which a cluster without add-on \
                 CRDs does not serve; the GET returns an unparseable plain-text 404 \
                 and fails the whole reconcile loop (observability={observability} \
                 backup={backup})",
                target.kind(),
                target.name(),
                target.api_version(),
            );
        }
    }
}

/// AC6 — the always-rendered `<name>-backup` ServiceAccount must never appear
/// in `prunes()` under any spec, since it deliberately outlives the schedule
/// toggle. The controller's own ownership re-check
/// (`core/crates/service-k8s/src/controller.rs:198` `prune_object`) is a separate,
/// already-exercised safety net this test does not reimplement.
#[test]
fn prunes_never_names_the_backup_service_account() {
    for (observability, backup, tape) in permutations() {
        assert!(
            prunes(&tape)
                .iter()
                .all(|t| !(t.kind() == "ServiceAccount" && t.name() == "tape-backup")),
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
            .unwrap()
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
