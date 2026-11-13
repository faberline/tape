# Backup / restore runbook (#1329)

> Prerequisite: Set `BASE` in this shell to the target Tape HTTP endpoint (`BASE=http://localhost:7137` for the local endpoint), as defined in the [smoke sequence](../operations/smoke-and-release-gates.md#6-smoke-sequence-end-to-end).

```bash
# operator-driven or ad hoc, against a running node:
tape backup --url http://localhost:7137 --dest file:///tmp/tape-backups \
  --token "$TAPE_BACKUP_TOKEN" --retention-secs 604800
```

`--dest` accepts `file://` (always supported), `s3://` (feature `backup`,
`service-backup/s3`), or `gs://` (feature `backup`, always linked —
`service-backup/gcs` is unconditional). `gs://` authenticates via
workload-identity ADC in-cluster (GKE-proven) and Vat's
`STORAGE_EMULATOR_HOST` locally. The bearer token needs `admin` on `*`; omit
it entirely when the node runs `--auth off`. `--retention-secs` prunes older
backup objects at the destination after a successful put; omit to keep
everything. In-cluster, the operator's backup CronJob passes
`http://<name>.<namespace>.svc.cluster.local:7137` as `--url`.

## Cold restore runbook (#2465, #2468)

`bootstrapSeedUri` is **bootstrap-if-empty**, not a live restore endpoint: on
every boot the server first checks whether its durable `TAPE_DATA_DIR`
already has raft state. An empty data directory seeds from the URI as
before; a non-empty one (including a routine pod restart onto its own
already-bootstrapped PVC) **skips the seed and boots from its existing
state** instead of refusing, so the field is safe to leave declared on the
CR (fixed in #2468 — it used to be one-shot: any restart while the field was
still set crash-looped on the non-empty-PVC refusal).

1. Set `spec.bootstrapSeedUri` on a **fresh** instance (empty PVCs — a new
   deployment, or one whose PVCs were explicitly wiped) to the backup object
   URI.
2. Wait for the CR to report Ready, then verify the expected data replayed
   (e.g. `curl $BASE/topics/<topic>/replay?from_offset=0`).
3. Nothing further is required — future pod replacements restart onto the
   same populated PVC, detect existing state, and skip the seed
   automatically (`decision="skipped_existing_state"` in the pod log).
4. Recommended hygiene, not a requirement: remove the field once the
   bootstrap has converged.
   ```bash
   kubectl patch tape/<name> --type=json \
     -p '[{"op":"remove","path":"/spec/bootstrapSeedUri"}]'
   ```
   This guards against a narrower follow-on hazard: if the PVC is ever
   deleted and reprovisioned (not a routine restart) while the field is
   still set, the replacement pod's data dir really is empty again and WILL
   re-seed from the (possibly now-stale) backup object. Removing the field
   after a successful bootstrap makes that recreate-then-restart sequence
   fail loudly (no seed configured) instead of silently reseeding old data.

## GKE restore drill (e2e::operator_restore)

The GKE suite `operator_restore` runs these steps in the namespace `tape`.
`<bucket>` is the bucket that `backup_bucket` names in the dispatch. The
suite gets it from `E2E_BACKUP_BUCKET`. Each command runs as the case runs
it; the case keeps the saved CR in memory and writes the restore file under
`TMPDIR` (`/work` in the Job).

(a) Scale the group to three replicas and set a yearly backup schedule, so
no backup starts on its own:

```bash
kubectl -n tape patch tape tape --type=merge -p '{"spec":{"replicasPerShard":3,"voterCount":3,"graceSecs":10,"backup":{"schedule":"0 0 1 1 *","destination":"gs://<bucket>/restore-drill"}}}'
```

(b) Run one backup from the CronJob and read the object key it wrote. The
case polls the Job until `status.succeeded` is 1. To check by hand:

```bash
kubectl -n tape create job tape-backup-drill --from=cronjob/tape-backup
kubectl -n tape get job tape-backup-drill -o json
kubectl -n tape logs job/tape-backup-drill
```

The `object.key` in the log names the backup. The seed is
`gs://<bucket>/<key>`.

(c) Save the CR, then destroy the group and its PVCs:

```bash
kubectl -n tape get tape tape -o json > /work/tape-before.json
kubectl -n tape delete tape tape --wait=true
kubectl -n tape delete pvc -l app.kubernetes.io/instance=tape --wait=true
```

(d) Create the CR again from the saved JSON. Remove `metadata.resourceVersion`,
`metadata.uid`, `metadata.creationTimestamp`, `metadata.generation`,
`metadata.managedFields`, `metadata.finalizers`, the annotation
`kubectl.kubernetes.io/last-applied-configuration`, the top-level `status`
and `spec.backup`. Set `spec.bootstrapSeedUri` to `gs://<bucket>/<key>`. Set
`spec.serviceAccountName` to `tape-restore-drill`, the identity that the drill
bucket grants; the server pods read the seed with it. Save
the result as `/work/tape-restore.json`, then run:

```bash
kubectl -n tape create -f /work/tape-restore.json
```

(e) Replay the topic. Every offset 0 to 29 must be back, in order, and a
new write must still work:

```bash
curl "$BASE/topics/restore.drill/replay"
```
