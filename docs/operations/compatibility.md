# Version compatibility

This page says what a client, a data directory, a raft peer or a CRD of version N may meet in N+1. N and N+1 mean two adjacent tape releases (`tape@X.Y.Z`). Tape is pre-1.0: a minor release may change behaviour, but each rule below holds between adjacent releases.

## API

The HTTP API is additive-only inside a release line. A release may add routes, methods, request fields that are optional, and response fields. It must not remove or rename a route, a method or a schema field (property).

`clients/openapi.json` is the live contract. It is byte-equal to `tape spec --format openapi`, and a gate refuses drift between the two. `clients/openapi.baseline.json` is the floor: the copy of `clients/openapi.json` from the last release that promised it. The test `compatibility_docs::openapi_keeps_every_baseline_route_method_and_field` fails when the live contract loses anything the baseline has.

To drop something on purpose, the change must say so in the pull request and replace the baseline in the same change. To raise the floor after a release, run `cp clients/openapi.json clients/openapi.baseline.json`.

## Data directory and WAL

The rules below are copied from the "Rollback Contract" in [CRD upgrade order, capacity and quotas](crd-capacity-and-quotas.md), section 3e.

| Direction | Behaviour |
|---|---|
| Newer binary reads older data | Safe: `TapeJournal` fields carry `#[serde(default)]`. |
| Older binary reads newer data, top-level journal field | Loads without error and silently drops the unknown field. The next write loses it. |
| Older binary meets a nested record with `deny_unknown_fields` (`Subscription`) | Fails to load and exits non-zero. Fail-loud, with an `unknown field` error. |
| Older binary meets an unknown `TapeCommand` WAL variant | Fails to load and exits non-zero. Fail-loud, with an `unknown variant` error. |

There is no format-version constant on the durable path.

Rule: do not run an older binary on a newer data directory. Copy the directory first.

## Raft peer protocol

No version number is exchanged between raft peers, and no compatibility across versions is tested.

The only supported mixed-version window is the Kubernetes `RollingUpdate` of the StatefulSet (`crates/tape-operator/src/render/statefulset.rs`, `k8s/base/statefulset.yaml`) between two adjacent releases, one pod at a time. Do not leave a group mixed after the rollout. Do not skip a release. Read the release notes first.

This window is stated, not tested; no test runs two versions in one group.

## CRD

The CRD serves a single version, `v1alpha1`.

- Additive `spec` and `status` fields land inside `v1alpha1`.
- Renaming, removing or retyping a field needs a new CRD version and a conversion mechanism. None exists today.
- Apply the CRD before the operator and the custom resources. The versioning policy is in [CRD upgrade order, capacity and quotas](crd-capacity-and-quotas.md), section 3e.
