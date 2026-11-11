# Runbooks

Each alert that the tape rule files define has one row here: what it means, the first thing to check, and where the full procedure is.

| Alert | Meaning | First check | Runbook |
|---|---|---|---|
| `TapeAppendLatencyHigh` | Average append latency has been above 500 ms for 10 minutes. | Read the pod's append latency series on its `/metrics` endpoint. | see the runbook annotation in `k8s/components/observability/prometheusrule.yaml` |
| `TapeReplayLatencyHigh` | Average replay latency has been above 2 s for 10 minutes. | Read the pod's replay latency series on its `/metrics` endpoint. | see the runbook annotation in `k8s/components/observability/prometheusrule.yaml` |
| `TapePodRestarting` | A tape pod has restarted repeatedly. | Read the pod log for the `decision=` fields that show whether the seed ran. | [cold-restore.md](cold-restore.md) |
| `TapeStorageDegraded` | A node hit ENOSPC and is in degraded read-only mode. | Check `tape_storage_full_errors_total`; a rising counter means the volume is flapping. | [disk-full.md](disk-full.md) |
| `TapeAuthRegistryReloadFailing` | Token-registry reloads are being rejected, so the last good registry is still in use. | Read the `failure` field of the `service_auth.audit` warn event. | see the runbook annotation in `k8s/components/observability/prometheusrule.yaml` |
| `TapeRaftLeaderAbsent` | The raft group has no elected leader, so writes cannot commit. | Check `/raftz` on each replica. | [raft-member-loss.md](raft-member-loss.md) |
| `TapeSubscriptionLagGrowing` | Subscription lag has grown for 15 minutes. | Check that the consumer bound to the subscription is alive and pulling. | see the runbook annotation in `k8s/components/observability/prometheusrule.yaml` |
| `TapeMemoryHeadroomLow` | The container's memory is within 15% of its limit, which equals its request. | List retention with `tape retention list`; do not raise the limit. | see the runbook annotation in `k8s/components/observability/prometheusrule.yaml` |
| `TapeNoReadyServingPods` | The StatefulSet has zero ready replicas, so tape serves nothing. | Check the pods; `Pending` means scheduling or storage. | [node-drain-and-pdb.md](node-drain-and-pdb.md) |
| `TapeBackupCronJobFailed` | Two or more retained backup Jobs for this instance have failed. | Read the log of the newest failed Job. | [cold-restore.md](cold-restore.md) |
| `TapePvcNearFull` | The data PVC has less than 10% free. | Check that every active topic has a retention bound with `tape retention list`. | [disk-full.md](disk-full.md) |
| `TapeOperatorAbsent` | No tape operator is scraped in the namespace, so nothing reconciles Tape custom resources. | Run `kubectl -n tape-system get deploy tape-operator`. | see the runbook annotation in `k8s/components/operator-monitoring/prometheusrule.yaml` |
| `TapeOperatorReconcileErrorRate` | The tape operator fails more than 10% of its reconciles. | Run `kubectl get events -A --field-selector reason=ReconcileFailed`. | see the runbook annotation in `k8s/components/operator-monitoring/prometheusrule.yaml` |

The rule files are the source, and the test `runbook_index::every_alert_has_a_row_in_the_index` keeps this table complete.
