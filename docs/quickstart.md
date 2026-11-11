# Quickstart

This page takes you from an install to a published and pulled message, and then
to a 3-replica group. Every command here is checked by
`crates/tape/tests/it/quickstart_docs.rs`.

## Install

Pick one option.

(a) The `tape` binary, with the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/faberline/tape/main/install.sh | sh
tape --version
```

The script installs into `~/.local/bin` by default. Set `TAPE_INSTALL` to choose
another directory, and `TAPE_VERSION` to pin a release. For a private repository,
set `GH_TOKEN` to a GitHub token.

(b) The container image:

```bash
docker pull ghcr.io/faberline/tape:latest
docker run --rm -p 7137:7137 ghcr.io/faberline/tape:latest serve
```

## Run one node

```bash
tape serve --bind 127.0.0.1:7137 --store /tmp/tape/journal.json
```

Auth is off by default for `serve`, so the calls below need no token. When the
node is up, `GET /healthz` answers:

```bash
curl -sS http://127.0.0.1:7137/healthz
```

## Publish and pull a message

Run these four calls against the node above. They match the primary workflow in
[`README.md`](../README.md): the `orders` topic and the `billing` subscription.

Create the subscription:

```bash
curl -sS -X POST http://127.0.0.1:7137/topics/orders/subscriptions \
  -H 'content-type: application/json' -d '{"name":"billing"}'
```

Publish a message. The response carries its durable offset:

```bash
curl -sS -X POST http://127.0.0.1:7137/topics/orders/append \
  -H 'content-type: application/json' \
  -d '{"key":"order-1","payload":{"total":42}}'
```

Pull from the subscription. The pull does not move the cursor:

```bash
curl -sS -X POST http://127.0.0.1:7137/topics/orders/subscriptions/billing/pull \
  -H 'content-type: application/json' -d '{"limit":10}'
```

Acknowledge the batch. The ack call sets the cursor to `next_offset`:

```bash
curl -sS -X POST http://127.0.0.1:7137/topics/orders/subscriptions/billing/ack \
  -H 'content-type: application/json' -d '{"offset":1}'
```

## Run a 3-replica group

The operator path runs a group of three replicas in a Kubernetes cluster. Render
the cluster-scoped CRD and the operator, then apply them:

```bash
tape k8s crd render | kubectl apply -f -
tape k8s operator render | kubectl apply -f -
```

Render a `Tape` custom resource for your app namespace, then apply it:

```bash
tape k8s instance render --profile prod --namespace tape-app --out ./tape.yaml
kubectl apply -f ./tape.yaml
```

The `prod` profile sets `replicasPerShard: 3` and `voterCount: 3`, and `auth: required`.
Pulls then need a bearer token from the token registry. See
[`operations/environment-and-http.md`](operations/environment-and-http.md).

The custom resource kind is `Tape`, in API `tape.dev/v1alpha1`. The example CR for
the end-to-end suites is `k8s/overlays/gke-test/tape-cr.yaml`; it runs one replica.

Check the group. The CR must exist, and each pod answers `GET /raftz`:

```bash
kubectl get tape
```

Then run a command against the group. `tape connect` port-forwards to the Service
and sets `TAPE_URL`:

```bash
tape connect --namespace tape-app --cr tape -- <command>
```

## Next

- [Operations](operations/README.md): deploying, running and checking tape.
- Runbooks: [cold restore](runbooks/cold-restore.md),
  [disk full](runbooks/disk-full.md),
  [node drain and PDB](runbooks/node-drain-and-pdb.md).
- [Status](../STATUS.md): what is supported today.
