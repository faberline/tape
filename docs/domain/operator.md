# operator

operator runs tape on Kubernetes: a `Tape` custom resource
(`tape.dev/v1alpha1`) and a reconcile loop that renders one tape raft group
from it. The reconcile machinery, leases, conditions and manifest helpers come
from core's `service-k8s`; this context holds tape's resource shape and what it
renders. `tape` links it only with the `operator` feature, so the default build has no kube dependency.

**Form:** infrastructure only · **Depends on:** — · **Source:** [`crates/tape-operator`](../../crates/tape-operator/src/lib.rs)

## Model

- **Tape resource** — `Tape` with `TapeSpec` (the shared
  `service_k8s::ClusterSpec`, storage, auth mode and token secret, bootstrap
  seed URI, topics to provision with their subscriptions, observability,
  backup) and `TapeStatus` (phase, replica counts, and
  `service_k8s::Condition`s). `crd_yaml()` prints the
  CustomResourceDefinition.
- **Rendered topology** — `render` builds the ServiceAccount, the headless and
  client Services, the PodDisruptionBudget, and the StatefulSet whose pods
  learn their raft identity through the downward API, plus a ServiceMonitor
  and PrometheusRule when `spec.observability` is set. `prunes` lists what to
  delete when a setting is turned off.
- **Reconcile loop** — `reconcile::run` hands `service_k8s::run` the render
  and status projection; core drives the watch and the leader-gated
  server-side apply.

```text
Tape (tape.dev/v1alpha1)  --reconcile-->  ServiceAccount, StatefulSet,
                                          headless + client Service,
                                          PodDisruptionBudget
```

## Invariants

- One `Tape` resource is one raft group; tape has no sharding.
- The service image enables the `operator` feature, because the same image runs
  the checked-in operator Deployment (`tape k8s operator`).
- The CRD schema embeds core types (`service_backup` policies,
  `service_k8s::Condition`) as-is, so a core release that changes them changes
  tape's CRD.
