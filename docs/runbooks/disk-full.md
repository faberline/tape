# Disk-full runbook (#2573)

A tape node that hits `ENOSPC` on its journal persist path latches **degraded
read-only mode**: mutating requests (append, checkpoint advance, subscription
create/delete/ack, retention set) answer `507` with error kind
`storage_full`, and **reads keep serving**. It does not crash, and it is not
`NotReady` — a full disk is precisely when an operator most needs to read what
is already journalled.

**Do not start by restarting the pod.** The node recovers itself: it re-probes
its store directory every `TAPE_STORAGE_FULL_REPROBE_SECS` (default `30`) and
clears the flag on the first successful write. Restarting only helps as a way
to reset the flag after you have already made room, and it costs you the
replay window during the restart.

1. Confirm the diagnosis:
   ```bash
   curl -s $BASE/metrics | grep tape_storage_
   ```
   `tape_storage_degraded 1` = currently refusing writes.
2. Read `tape_storage_full_errors_total` next. A **rising counter with the
   gauge back at 0** is the important case: the volume is flapping in and out
   of full, not recovered. The gauge alone cannot show that — a node that
   fills and re-probes clean between two scrapes reads healthy at both. The
   `TapeStorageDegraded` alert uses `max_over_time(...[5m])` for the same
   reason.
3. Make room. Either shrink the journal through retention:
   ```bash
   curl -X PUT $BASE/topics/<topic>/retention -d '{"max_events": <n>}'
   ```
   (note this **also** returns `507` while degraded — a retention change
   rewrites the whole journal through a temp file, so it needs room for a
   second copy before the old one is unlinked; free space at the volume level
   first), or expand the PVC on a resizable StorageClass:
   ```bash
   kubectl patch pvc data-<name>-0 -p \
     '{"spec":{"resources":{"requests":{"storage":"<bigger>"}}}}'
   ```
4. Wait one re-probe interval and re-check `/metrics`. The pod log carries
   `storage re-probe write succeeded; leaving degraded read-only mode`.
5. If the gauge stays `1` after the volume demonstrably has room, read the pod
   log for the re-probe warning — the store directory can be unwritable for
   reasons other than capacity (read-only remount, permissions, a failed CSI
   attach).
