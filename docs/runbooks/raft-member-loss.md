# Raft member loss and quorum loss

A tape group of 3 voters needs 2 replicas for quorum. This page covers one replica lost, two replicas lost, and a replaced PVC. The alert that starts here is `TapeRaftLeaderAbsent`, which fires after 2 minutes with no known leader.

## Check the group

Each replica answers `GET /raftz` with JSON that includes `is_leader` and `applied_index`. Read it on every pod:

```sh
kubectl -n <ns> get pods -l app.kubernetes.io/instance=tape,app.kubernetes.io/component=server
kubectl -n <ns> get tape
tape connect --namespace <ns> --cr tape -- sh -c 'curl -s "$TAPE_URL/raftz"'
```

`tape connect` goes through the Service, so it reaches one replica. To read each replica, forward to the pod and query it: `kubectl -n <ns> port-forward pod/<pod> 7137:7137`, then `curl -s http://127.0.0.1:7137/raftz`.

Exactly one replica must report `is_leader: true`. The metric `tape_raft_leader_known` is `0` when no leader is known.

## One of three replicas lost

The two survivors keep quorum. If the lost replica was the leader, they elect a new one, and they keep committing. No action is needed to keep serving.

Check with `/raftz` that one survivor is the leader. Then append a value and confirm that the `applied_index` of both survivors advances.

When the lost pod comes back, it rejoins and catches up. If it is far behind, it catches up by install-snapshot. Verify that its `applied_index` reaches the leader's.

Do not delete the PVC of the lost pod to speed this up.

## Two of three replicas lost (quorum loss)

Writes cannot commit: append and checkpoint-put both stop. Reads of data that is already committed may still be served.

Do not restart the remaining replica to reset the election. This makes quorum loss worse, as the `TapeRaftLeaderAbsent` annotation says.

Do not delete any PVC. First bring the lost pods back. Run `kubectl -n <ns> describe pod <pod>` on each one for the reason: node loss, eviction, image pull, or PVC attach. When one lost pod returns, quorum is back and a leader is elected.

If both lost pods have lost their PVC data for good, the data of this group is not recoverable from the survivors alone. Restore it from a backup with [cold-restore.md](cold-restore.md).

Membership is fixed at process start (see the `raft-replication` row in [STATUS.md](../../STATUS.md)). Do not try to change `voterCount` to regain quorum.

## A replaced PVC

A pod that starts on an empty PVC rejoins as a fresh member. When the other two replicas are healthy, it catches up by install-snapshot from the leader.

When the CR sets `bootstrapSeedUri`, an empty replica PVC is first seeded from that snapshot. This is bootstrap-if-empty: a data directory that is not empty skips the seed. See [cold-restore.md](cold-restore.md).

Replace one PVC at a time. Wait for `applied_index` to match the leader's before you replace the next one.

## See also

- [README.md](README.md) (the alert index)
- [cold-restore.md](cold-restore.md)
- [node-drain-and-pdb.md](node-drain-and-pdb.md)
- [../operations/README.md](../operations/README.md)
