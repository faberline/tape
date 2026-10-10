# Node drain and PodDisruptionBudget

The direct-install Tape data-plane StatefulSet has `maxUnavailable: 0` in its
PodDisruptionBudget. The eviction API will reject `kubectl drain`, which keeps
retrying forever. With exactly one durable member, an eviction would mean
data-plane downtime and potential data loss if the pod cannot rejoin before the
journal is cleaned or the PVC is recycled.

**Recognizing the issue:** when a GKE node auto-upgrade or a manual drain hangs,
you will see:

```
error when evicting pods/"tape-0" (will retry after 5s):
Cannot evict pod as it would violate the disruption budget.
```

**How to unblock the drain:**

The PDB gates only the eviction API. Delete the pod directly via the delete API,
which bypasses the PDB:

```bash
kubectl delete pod tape-0 -n <namespace>
```

This unblocks the drain immediately. The node continues draining, and Tape goes
through graceful shutdown and recovery.

**Alternative: temporarily widen the PDB.** Patch it, wait for the eviction to
actually land, then restore it. The wait is not optional — restoring
`maxUnavailable: 0` while the eviction is still in flight re-blocks the drain,
and you are back where you started with a PDB that now looks correct:

```bash
kubectl patch pdb tape -n <namespace> -p '{"spec":{"maxUnavailable":1}}'
kubectl wait --for=delete pod/tape-0 -n <namespace> --timeout=120s
kubectl patch pdb tape -n <namespace> -p '{"spec":{"maxUnavailable":0}}'
```

Prefer the direct delete. It is one step, it cannot leave the PDB widened if you
are interrupted, and it makes the outage an explicit act rather than a side
effect of a policy edit.

**During graceful shutdown:**

When the pod is deleted, the kubelet sends SIGTERM. Tape runs one shutdown
sequence inside a single deadline of `TAPE_GRACE_SECS` (ConfigMap `tape-config`,
default `30`), built from core's `server_lifecycle::ShutdownDeadline`
(`crates/tape/src/bin/tape/serve/shutdown.rs`):

1. Calls `start_drain()`, so `/readyz` returns 503. The kubelet's readiness
   probe fails and the endpoints controller withdraws the pod from the `tape`
   Service. New clients stop being routed here; this is the whole mechanism,
   there is no flag clients read.
2. Keeps serving for `TAPE_DRAIN_DELAY_SECS` (default `5`, capped by the
   deadline) while the endpoints move.
3. In replica mode, stops admitting raft proposals (writes answer 503), hands
   leadership to a caught-up voter and drains peer RPCs
   (`RaftHost::shutdown_within`). It logs one `event="raft_shutdown"` line with
   the handoff and whether every phase finished.
4. Closes the public listener and tells every open connection to close
   (GOAWAY on HTTP/2, `Connection: close` on HTTP/1.1), so in-flight requests
   finish and idle keep-alive or peer connections end at once instead of
   holding the drain open until the deadline. It logs `event="http_drained"`,
   or `event="http_drain_incomplete"` if connections were cut at the deadline,
   and exits. If the raft host could not finish within the deadline, the peer
   listener is aborted and the process exits non-zero.

**Nothing is flushed at shutdown, and nothing needs to be.** Every acked write
was already made durable at the time it was acked: the WAL
(`crates/tape-storage/src/wal`) fsyncs each group commit before it answers, and
in replica mode raft commits it to a quorum first. The grace budget buys
in-flight requests and the leadership handoff time to complete. It is not a
durability window, and cutting it short costs open requests and one election,
not data.

> **Keep `terminationGracePeriodSeconds` at `TAPE_GRACE_SECS` + 5.** These are
> two independent knobs on the direct-install path:
> `k8s/base/statefulset.yaml` hardcodes `terminationGracePeriodSeconds: 35` and
> `TAPE_GRACE_SECS` comes from the ConfigMap. The 5 s slack lets the process
> exit on its own; raise the ConfigMap value alone and the kubelet SIGKILLs the
> process partway through its shutdown. Raise both together. (On the operator
> path this cannot happen: `crates/tape-operator/src/render.rs` derives
> `terminationGracePeriodSeconds` from `spec.graceSecs` plus
> `TERMINATION_SLACK_SECS`.)

**After pod deletion (write outage begins):**

Once the old pod is gone and before the new one is ready, nothing answers the
Service, so both reads and writes fail. With one durable member this outage is
unavoidable; it is the cost you accepted when you unblocked the drain.

**Pod recovery (the replacement starts):**

1. Expect the replacement to be **Pending, not scheduled back onto the drained
   node** — `kubectl drain` cordons the node first (`SchedulingDisabled`), which
   is the point. Do not `kubectl uncordon` to make it schedule: that defeats the
   node upgrade or repair you were draining for.
2. Where it can land depends on the volume. A regional/zonal PD binds it to the
   same zone; a node-local PV (`local-path`, LVM, hostPath) pins it to the
   drained node, in which case Pending until that node returns is the correct and
   expected state. Check with:
   ```bash
   kubectl get pod tape-0 -n <namespace> -o wide
   kubectl describe pod tape-0 -n <namespace> | tail -20   # scheduler's reason
   ```
3. Whichever node it lands on, it rebinds the same PVC (`data-tape-0`).
   Kubernetes does not delete a PVC when its pod is deleted.
4. On startup Tape loads `/data/journal.json` (`TAPE_DATA_DIR=/data`) and serves
   from it. The direct-install StatefulSet is explicitly single-member
   (`VOTER_COUNT=1`), so there is no Raft state to replay and no cluster to
   rejoin — readiness *is* recovery.

**Verify recovery:**

```bash
kubectl rollout status statefulset/tape -n <namespace>
kubectl get endpoints tape -n <namespace>          # tape-0's IP must be listed
kubectl exec -n <namespace> tape-0 -- \
  wget -qO- http://127.0.0.1:7137/readyz
```

Then confirm the journal survived by replaying a topic you know had events
before the drain — an empty replay from a *ready* pod means the PVC did not come
back with it:

```bash
kubectl exec -n <namespace> tape-0 -- \
  wget -qO- 'http://127.0.0.1:7137/topics/<topic>/replay?from_offset=0'
```

**Optional: export the journal before the outage:**

If the release has the backup feature compiled in (`tape backup` is
feature-gated), export the journal state before deletion:

```bash
tape backup --url http://tape-0.tape-headless.<namespace>.svc.cluster.local:7137 \
  --dest file:///tmp/tape-backup --token "$TAPE_BACKUP_TOKEN"
```

(The per-pod DNS name goes through the headless Service `tape-headless`, which is
the StatefulSet's `serviceName`; `tape` is the load-balanced ClusterIP and does
not resolve per-pod.)

See [the deployment handoff](../deployment-handoff.md) § 7 for the full backup/restore
runbook.
