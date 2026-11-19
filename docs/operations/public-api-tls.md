# Public API TLS

Tape serves plain HTTP/1.1 and h2c on port 7137. The public port has no TLS code. TLS ends at the ingress or the service mesh. Peer traffic is separate: raft uses mTLS on port 7138 (`TAPE_PEER_MTLS=on`, see the `raft-peer-mtls` row in `STATUS.md`).

## Pattern

The client reaches the ingress over HTTPS. The ingress forwards plain HTTP to the Service `tape`, port 7137. The plaintext stays inside the cluster network.

The opt-in component is `k8s/components/public-ingress`. It renders a TLS Ingress and a NetworkPolicy. Replace these three things for your environment:

- the host `tape.example.com`;
- the secret `tape-public-tls`;
- `ingressClassName`, which the component does not set.

The Ingress is [ingress.yaml](../../k8s/components/public-ingress/ingress.yaml). The policy is [networkpolicy.yaml](../../k8s/components/public-ingress/networkpolicy.yaml). The policy admits port 7137 only from the namespace labeled `tape.cclab.dev/ingress-controller=true`. It does not open port 7138.

## Steps

```bash
kubectl label namespace <ingress-controller-namespace> tape.cclab.dev/ingress-controller=true
# In the environment's kustomization.yaml, add the component next to
# ../../components/network-policy:
#   components:
#     - ../../components/public-ingress
kubectl apply -k <overlay>
curl --fail https://tape.example.com/healthz
```

## Service mesh

With a service mesh, use the mesh mTLS to reach the pod and skip the Ingress. Keep the NetworkPolicy idea: admit port 7137 only from the mesh gateway namespace.

## Limits

- The page does not terminate TLS inside tape.
- A cluster without NetworkPolicy enforcement ignores the policy.
- Tape does not check the `X-Forwarded-Proto` header.
- Bearer tokens (`TAPE_AUTH`) travel in clear inside the cluster. Use this pattern only where that network is trusted.
