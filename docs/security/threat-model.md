# Threat model

This page names what tape protects, where its trust boundaries are, and the
STRIDE threats at each one, with the control in place and the known gap. Every
fact comes from the docs and manifests named in the section it belongs to. A
cell that says `none` has no control or no known gap in those sources.

## Assets

- Topic data on the WAL: the PVC `data-tape-0` and the pod volumes.
- Backups: the output of `GET /admin/backup` and the copies in the backup bucket.
- Certificates: the peer mTLS material (`TAPE_PEER_TLS_CERT`, `TAPE_PEER_TLS_KEY`, `TAPE_PEER_TLS_CA`) and the public-ingress TLS secret `tape-public-tls`.
- Bearer tokens: the registry file named by `TAPE_TOKEN_REGISTRY_FILE`.

## Boundary: public API

Client requests reach tape on port 7137. TLS ends at the ingress or the service
mesh, and the port itself has no TLS code (`docs/operations/public-api-tls.md`).

| STRIDE | Threat | Control | Known gap |
|---|---|---|---|
| Spoofing | A caller presents a forged or stolen bearer token. | With `TAPE_AUTH=required` and `TAPE_TOKEN_REGISTRY_FILE`, tape checks each token against the registry. A failed registry reload raises the alert `TapeAuthRegistryReloadFailing`. | The default is `TAPE_AUTH=off`. Tokens travel in clear inside the cluster. |
| Tampering | A request or response is changed between the ingress and tape. | none | The public port has no TLS code, so traffic between the ingress and tape is plaintext. |
| Repudiation | A caller denies a backup request it made. | `GET /admin/backup` is audited with the token redacted (`management-audit` in `STATUS.md`). | Only backup is audited. Data-plane writes leave no audit record. |
| Information disclosure | A caller outside the client namespaces reaches the API. | The NetworkPolicy `tape-server-ingress` admits port 7137 only from namespaces labeled `tape.cclab.dev/client-access=true`. The public-ingress policy admits it only from the namespace labeled `tape.cclab.dev/ingress-controller=true`. Tokens are never logged. | Inside the cluster the port carries plaintext. A cluster without NetworkPolicy enforcement ignores the policy. |
| Denial of service | A caller floods the API with appends or reads. | Append is classified as write admission in the shared bounded-admission mechanism (`docs/product/operations.md`). | none |
| Elevation of privilege | A caller with a read token performs a write or an admin action. | Per-topic roles `read` < `write` < `admin` are checked per request (`docs/domain/access.md`). `GET /admin/backup` needs `admin` on `*` when auth is required. | With `TAPE_AUTH=off` no role is checked. |

## Boundary: peer raft port

Raft traffic runs between tape pods on port 7138 with `TAPE_PEER_MTLS=on`. In
plaintext HA the peer routes share port 7137
(`k8s/components/network-policy/networkpolicy.yaml`).

| STRIDE | Threat | Control | Known gap |
|---|---|---|---|
| Spoofing | A process that is not a tape member joins the raft group. | With `TAPE_PEER_MTLS=on`, peer material is validated before start (`docs/operations/environment-and-http.md`). | With `TAPE_PEER_MTLS` off or unset, peer transport is cleartext. |
| Tampering | A raft message is changed in transit. | mTLS on the peer channel (`TAPE_PEER_MTLS=on`). | With `TAPE_PEER_MTLS` off or unset, peer traffic is plaintext. |
| Repudiation | A peer action cannot be traced back to its sender. | none | Only backup is audited. Peer traffic leaves no audit record. |
| Information disclosure | A caller outside the group reads peer traffic. | mTLS on the peer channel. The NetworkPolicy admits port 7138 only from tape server pods (`app: tape`). | Certificate rotation is manual. |
| Denial of service | A caller floods the peer port and stalls the group. | none | none |
| Elevation of privilege | A client namespace gains access to the peer port. | The NetworkPolicy admits port 7138 only from tape server pods. The public-ingress component does not open port 7138. | none |

## Boundary: operator and Kubernetes API

The operator reconciles the `Tape` CR (`k8s/operator/cluster/crd.yaml`). Its
ClusterRole (`k8s/operator/cluster/clusterrole.yaml`) lets it create, update and delete the
objects it renders, and write Leases and Events.

| STRIDE | Threat | Control | Known gap |
|---|---|---|---|
| Spoofing | A workload impersonates the operator against the Kubernetes API. | The operator acts under its own ServiceAccount, bound by a ClusterRoleBinding (`k8s/operator/cluster/clusterrolebinding.yaml`). | none |
| Tampering | A rendered workload is changed outside the operator. | none | none |
| Repudiation | An operator action on a Tape object cannot be traced. | The operator writes Kubernetes Events on the objects it manages (`events` create and patch). | none |
| Information disclosure | The operator reads Kubernetes objects beyond what it needs. | none | none |
| Denial of service | The operator is stopped or starved and the group is not reconciled. | none | none |
| Elevation of privilege | The operator's grant lets it change workloads outside the Tape namespace. | The grant names its resources and verbs (`k8s/operator/cluster/clusterrole.yaml`). | The grant is a ClusterRole, so it is cluster-wide. |

## Boundary: backup bucket

Backup objects go from tape to a destination (`file://`, `s3://` or `gs://`).
A restore reads one back through `spec.bootstrapSeedUri`
(`docs/runbooks/cold-restore.md`).

| STRIDE | Threat | Control | Known gap |
|---|---|---|---|
| Spoofing | A process writes a backup object or reads one it should not. | `gs://` authenticates with workload-identity ADC in-cluster (`docs/runbooks/cold-restore.md`). | none |
| Tampering | A backup object is changed at rest in the bucket. | none | none |
| Repudiation | A backup run cannot be traced back to its caller. | The backup audit record is redacted (`docs/product/operations.md`). | Only backup is audited. |
| Information disclosure | The backup audit record leaks a bearer token. | The backup audit record is redacted (`docs/product/operations.md`). | none |
| Denial of service | Backups fill the bucket or block the node. | none | none |
| Elevation of privilege | A caller without admin takes a backup. | `GET /admin/backup` needs `admin` on `*` when auth is required (`docs/runbooks/cold-restore.md`). | With `--auth off` no token is checked. |

## Known gaps

- The public port has no TLS code; TLS ends at the ingress or the mesh.
- Certificate rotation is manual.
- Only backup is audited; data-plane writes leave no audit record (`STATUS.md`, `management-audit`).
