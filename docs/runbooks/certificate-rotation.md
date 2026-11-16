# Certificate rotation runbook

## What rotates

The raft peer mTLS files are `TAPE_PEER_TLS_CERT`, `TAPE_PEER_TLS_KEY` and `TAPE_PEER_TLS_CA` (see [environment and HTTP surface](../operations/environment-and-http.md), row "Peer mTLS").

A running tape looks at the three files every 30 seconds (`peer_tls::RELOAD_INTERVAL`). It reloads them when their bytes change. New connections use the new material. Open HTTP/2 sessions finish on the old one. No restart is needed.

A reload that fails keeps the old material. The log then shows `raft peer TLS reload failed; keeping the last good material`.

## Public API certificate

The public port has no TLS code (see [public API TLS](../operations/public-api-tls.md)). Rotate the public certificate at the ingress or the service mesh. Tape does not reload it, and no tape restart is needed.

## Rotate the peer certificate (same CA)

Set the shell variables first:

```bash
NS=<namespace>
SECRET=<peer TLS secret name>
BASE=<tape HTTP endpoint, as in the smoke sequence>
```

1. Check that the group is healthy. Every replica is `Running`, and a leader is shown:

   ```bash
   kubectl -n $NS get pods
   curl -s $BASE/raftz
   ```

2. Find the container name of a replica. Use it in step 4 in place of `<container>`:

   ```bash
   kubectl -n $NS get pods
   kubectl -n $NS get pod <pod> -o jsonpath='{.spec.containers[*].name}'
   ```

3. Write the new `tls.crt` and `tls.key` into the Secret. Keep the same CA:

   ```bash
   kubectl -n $NS create secret generic $SECRET --from-file=cert.pem=<new cert> --from-file=key.pem=<new key> --from-file=ca.pem=<CA file> --dry-run=client -o yaml | kubectl -n $NS apply -f -
   ```

   Wait about 60 seconds for the kubelet to refresh the mounted files, then about 30 seconds for the tape poll.

4. Confirm that every replica reloaded. Each one logs a new `generation`:

   ```bash
   kubectl -n $NS logs <pod> -c <container> | grep "reloaded raft peer TLS material"
   ```

   Run it for every replica.

5. Confirm that replication still works. A leader is shown:

   ```bash
   curl -s $BASE/raftz
   ```

## Rotate the CA

Trust both CAs first, then move to the new one.

- Step A: replace `ca.pem` in the Secret with a bundle of the old and the new CA. Wait until every replica logs the reload (step 4 above).
- Step B: replace `cert.pem` and `key.pem` with a certificate and key signed by the new CA. Wait until every replica logs the reload.
- Step C: replace `ca.pem` with the new CA only. Wait until every replica logs the reload. Then confirm with `curl -s $BASE/raftz`.

## If a reload fails

1. Read the log of the replica for the warning line `raft peer TLS reload failed`.
2. The replica keeps serving on the old material. Nothing is lost.
3. Fix the Secret (step 3 above). The next poll, within 30 seconds, retries by itself.
4. Do not restart the pod.
