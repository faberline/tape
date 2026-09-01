# Operations

What an operator gets from a tape deployment: backup and restore, grants and
admission, Kubernetes assets, probes and telemetry, and the one performance
gate. This area spans the README capabilities `backup-and-seed`,
`security-hardening`, `kubernetes-native-deployment`,
`operations-observability`, and `local-performance-ceiling`.

## Whole-journal backup and cold seed

- Problem: none open as shipped; the limits below belong to the rebaseline.
- Who: operators.
- Promise: `GET /admin/backup` streams a whole-journal snapshot and requires
  the admin role when auth is required. `tape backup` ships it to a
  `file://`, `s3://`, or `gs://` destination with retention pruning, and the
  README, the runbook, the CLI help, and the LLM topic list every accepted
  scheme. `--bootstrap-seed-uri` restores it into an empty data directory
  only, never over existing data. The backup audit record is redacted.
- Limits today: only the `file://` sink is proven end to end in this
  repository; the cold-restore runbook lives in the deployment handoff page
  rather than under runbooks.
- Non-goals: export subscriptions; a subscription snapshot (that is
  [retention-seek-and-snapshots.md](retention-seek-and-snapshots.md)).
- Neighbours: none; first section of the area.
- Status rows: `backup-to-sink`, `cold-seed-bootstrap`, `management-audit`.

## Grants and bounded admission

- Problem: none open as shipped; the limits below belong to two outcomes.
- Who: operators issuing tokens; every caller under `--auth required`.
- Promise: with `--auth required`, append needs a write grant on the topic,
  reads need a read grant, backup needs the admin role, probes stay
  tokenless, and `--auth off` keeps every route tokenless. The body limit is
  enforced per request, and append is classified as write admission for the
  shared bounded-admission mechanism.
- Limits today: grants are per topic, not per subscription (closed by
  [subscriptions.md](subscriptions.md) § Subscription ack and competing
  subscribers); the default router keeps admission disabled and there is no
  per-topic or per-subscription quota (closed by Quotas and scale transition
  below); tape itself holds the token registry (closed by
  § Kubernetes-delegated authentication below).
- Non-goals: identity federation; the token registry is the shared library's.
- Neighbours: none within the area.
- Status rows: `per-topic-authorization`, `flow-control-quotas`.

## Kubernetes operator and direct install

- Problem: none open as shipped; the limits below belong to three outcomes.
- Who: operators.
- Promise: a `Tape` custom resource renders and reconciles a StatefulSet,
  Services, PodDisruptionBudget, ConfigMap, backup CronJob, and observability
  pair with status conditions; auth defaults to required; stale objects are
  pruned. A direct-install base deploys a durable singleton. Topics and
  subscriptions are provisioned declaratively. The kind script and the GKE
  acceptance path exercise the assets end to end.
- Limits today: `shardCount` is fixed at 1 and a replica-count change
  restarts members; the kind and GKE runs are manual, read back through the
  legacy routes, and prove no scale transition.
- Non-goals: a regional GKE profile; managed-service billing.
- Neighbours: extended by
  [replication-and-availability.md](replication-and-availability.md)
  § Live replica membership and § Multi-shard topology.
- Status rows: `k8s-deployment-assets`, `k8s-operator`,
  `kind-cluster-acceptance`, `gke-zonal-acceptance`.

## Health, metrics, traces, and drain

- Problem: none open as shipped.
- Who: operators and their alerting.
- Promise: `/healthz`, `/readyz`, `/metrics`, `/openapi.json`, and `/docs`
  on the data-plane port; readiness flips to 503 on drain; request counters,
  latency sums, topic offset and subscription lag gauges; OTLP traces; a
  bounded stability run survives repeated restarts without losing history.
- Limits today: no oldest-unacked-age, delivery-attempt, or dead-letter
  counter, because there is no per-message delivery state (closed by the
  subscription outcome).
- Non-goals: a metrics surface other than Prometheus text exposition.
- Neighbours: none within the area.
- Status rows: `standard-operational-endpoints`, `otlp-tracing`,
  `bounded-stability-run`.

## Local performance ceiling

- Problem: none open as shipped.
- Who: operators sizing a node; the repository, as a regression gate.
- Promise: append, replay, and checkpoint stay inside the release-mode budget
  measured against tape's own baseline, durable append throughput rises with
  connection count, and tape never claims a win over another broker.
- Non-goals: any figure against Kafka, JetStream, or another broker.
- Neighbours: none within the area.
- Status rows: `local-performance-ceiling`.

## Cluster connect

- Problem: reaching a tape deployed on Kubernetes takes a hand-rolled
  `kubectl port-forward` plus a manual token copy; `lumen connect` gets both
  from the shared `cli_std::connect`, and tape has no such verb.
- Who: operators and agents driving a cluster-deployed tape from outside the
  cluster.
- Promise: `tape connect` opens the port-forward lifecycle and resolves the
  caller's credential through `cli_std::connect`, the same shape as
  `lumen connect`: always built rather than behind a feature flag, prints the
  local endpoint, and tears the forward down on exit.
- Non-goals: a general kubectl replacement; multi-cluster context
  management; any credential store of tape's own.
- Open: what credential connect hands the caller once the token-registry
  Secret retires under § Kubernetes-delegated authentication.
- Neighbours: composes with § Kubernetes-delegated authentication — the
  credential it resolves must be one the server's verification mode accepts.
- Outcome: `cluster-connect`. Tracking: not assigned.

## Kubernetes-delegated authentication

- Problem: tape verifies bearer tokens against a static role-map registry it
  must hold, mount, and reload; lumen holds no credentials at all —
  TokenReview authenticates and SubjectAccessReview authorizes.
- Who: operators issuing and rotating tokens today; platform teams that want
  one identity system across the cluster.
- Promise: with auth required in-cluster, tape authenticates each caller by
  TokenReview and authorizes by SubjectAccessReview, mapping topic read,
  write, and admin operations onto Kubernetes resource attributes the same
  way lumen's `service_auth::k8s` mode does. Tape stops holding any
  credential: the static token registry and its Secret retire. Probes stay
  tokenless, and `--auth off` keeps every route tokenless.
- Non-goals: an OAuth or OIDC surface of tape's own; identity federation; a
  parallel static-registry mode kept as fallback.
- Open: the auth story for a non-Kubernetes deployment once the registry
  retires (today that leaves only `--auth off`); how the
  subscription-scoped grants of [subscriptions.md](subscriptions.md)
  § Subscription ack and competing subscribers map onto SubjectAccessReview
  resource attributes.
- Neighbours: supersedes the token-registry half of § Grants and bounded
  admission; § Cluster connect must hand out a credential this mode accepts.
- Outcome: `kubernetes-delegated-authentication`. Tracking: not assigned.

## Ungated Kubernetes render

- Problem: every `tape k8s` verb sits behind `--features operator`, so the
  shipped serving binary answers with a rebuild-with-the-feature stub; lumen
  ships manifest rendering unconditionally and gates only the reconcile
  controller.
- Who: operators rendering the CRD and manifests from the release binary.
- Promise: `tape k8s crd` and the render verbs work in the default build,
  with `service-k8s` linked unconditionally as `default-features = false,
  features = ["render-only"]`, the same shape as lumen; only
  `tape k8s operator run` — the reconcile controller and its kube-rs
  client — stays behind the `operator` feature.
- Non-goals: shipping the reconcile controller or a kube-rs client in the
  default build.
- Open: none.
- Neighbours: extends § Kubernetes operator and direct install on the
  packaging side; changes no rendered object.
- Outcome: `ungated-kubernetes-render`. Tracking: not assigned.

## Quotas and scale transition

- Problem: nothing bounds a single tenant's topics, subscriptions, or bytes,
  and no run proves a replica-count change under load.
- Who: operators running more than one team on one instance.
- Promise: per-topic and per-subscription quotas as counts and bytes per
  window, refused with the shared error envelope; the default router enables
  bounded admission; the GCP acceptance script proves a replica-count
  transition under load with no committed loss, zonal.
- Non-goals: billing-style accounting; a regional profile.
- Open: the quota dimensions and defaults; whether quotas are set on the
  custom resource or through the API.
- Neighbours: extends Grants and bounded admission and Kubernetes operator
  and direct install; depends on
  [replication-and-availability.md](replication-and-availability.md)
  § Live replica membership for the transition it proves.
- Outcome: `quotas-and-scale-transition`. Tracking: not assigned.

## Non-goals in this area

- `peer-broker-benchmarks`: the earlier NATS JetStream and Kafka
  calibrations are history in `docs/benchmarks-scale.md`, not a claim.
- `export-subscriptions`: backup is disaster recovery only.
