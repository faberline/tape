# Stability promise for 1.0.0

From `tape@1.0.0`, a release in the `1.x` line makes no breaking change to the five surfaces below. A breaking change waits for `2.0.0`. This page is the 1.0.0 promise. [Version compatibility](compatibility.md) holds the per-release rules and the tests that check them.

## API

The HTTP API in `clients/openapi.json`. Rule: additive only. Routes, methods, optional request fields and response fields may be added. A route, method or schema field is never removed, renamed or retyped, and no optional field becomes required.

Check: `clients/openapi.baseline.json` is the floor. The test `compatibility_docs::openapi_keeps_every_baseline_route_method_and_field` compares the live spec with it.

## Wire format

The JSON bodies of the HTTP API (the append, replay and checkpoint envelopes, and the shared error envelope), and the raft peer protocol messages between pods.

Rule: an old client works against a new server of the same major version. A field is added, never changed in meaning.

Raft peers: a `1.x` pod meets another `1.x` pod only during a rolling update between adjacent releases (`RollingUpdate`, one pod at a time), as the "Raft peer protocol" section of [compatibility.md](compatibility.md) says. No test runs two versions in one group. This is stated, not tested.

## On-disk format

The WAL and the journal in the data directory.

Rule: a `1.x` binary reads every data directory that an earlier `1.x` binary wrote. No `1.x` release needs a manual migration. An older binary must not run on a newer data directory; copy the directory first.

`tape@1.0.0` adds a format-version constant (issue #102). `1.x` never lowers it.

## CRD

The custom resource `Tape` in the group `tape.dev`, served by `k8s/operator/crd.yaml`.

Rule: `v1alpha1` serves `1.0.0`. Additive `spec` and `status` fields land inside the served version. Renaming, removing or retyping a field needs a new CRD version and a conversion mechanism.

Graduation: `v1alpha1` graduates to `v1` no earlier than a `1.x` minor release. In that release `v1alpha1` stays served together with `v1`, so no `1.x` release removes a served version. Only `2.0.0` may stop serving a version.

## CLI flags

Every flag and subcommand of `tape` (see [images-and-cli.md](images-and-cli.md) and `tape --help`).

Rule: a flag, a subcommand or an environment variable (`TAPE_*`) is not removed, renamed or given another meaning in `1.x`. A new flag may be added. A flag may be deprecated: it still works, prints a warning, and is removed no earlier than `2.0.0`.

## What is not promised

Internal Rust crate APIs (`crates/*`), metric names that [environment-and-http.md](environment-and-http.md) does not list, log text, the benchmark numbers, and anything marked experimental or `Limited` in [STATUS.md](../../STATUS.md).

## How a breaking change ships

Only in `2.0.0`, with a migration note in the release notes.
