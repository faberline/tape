# Architecture

tape is a Cargo workspace of eight crates under `crates/`. Six hold tape's
model and its bounded contexts, one crate per context; two are assembly crates
that wire them into binaries. This page says how the crates are cut, what may
depend on what, and which rule breaks are allowed and why. The machine-checked
form of everything here is [`ddd.toml`](../ddd.toml); the decisions behind it
are in [ADR 0001](adr/0001-standard-layout-and-ddd.md) and
[ADR 0002](adr/0002-multi-crate-workspace.md).

Everything that is not specific to tape — the HTTP service kit, bearer auth,
the raft host, peer TLS, durable files, backups, the Kubernetes operator kit,
the standard CLI commands — comes from
[faberline/core](https://github.com/faberline/core) crates pinned by git tag.
This repository keeps only tape's own logic.

## Crates

| Crate | Role | Internal dependencies | What it owns |
|-------|------|-----------------------|--------------|
| `tape-shared-kernel` | shared kernel (pure) | — | `TapeJournal`, `TapeCommand`, `apply_command`, the snapshot and replay-wire formats, and the `CommitLog` and `Replicator` ports |
| [`tape-storage`](domain/storage.md) | context `storage` (infrastructure) | shared kernel | the group-commit WAL and the legacy whole-file store, both `CommitLog`s |
| [`tape-replication`](domain/replication.md) | context `replication` (infrastructure) | shared kernel | the raft-runtime state machine and host (`Replicator`) and peer TLS |
| [`tape-access`](domain/access.md) | context `access` (application) | — | tape's adoption of `service-auth`: configuration and per-topic authorization |
| [`tape-journal`](domain/journal.md) | context `journal` (application, interfaces) | shared kernel, access | `JournalService`, `TapeMetrics`, the wall clock, the HTTP handlers and the offline spec |
| [`tape-operator`](domain/operator.md) | context `operator` (infrastructure) | — | the `Tape` custom resource and the reconcile loop |
| `tape` | assembly | all of the above; `tape-operator` behind feature `operator` | `AppState` and the router, the backup runner (feature `backup`), the `tape` binary, the integration tests |
| `tape-bench` | assembly | tape, journal, storage, shared kernel | the bench harness, the `tape-bench` binary, the release-mode performance gate |

The journal's HTTP handlers call `tape_access::authorize`; that is the only
dependency between contexts. Storage and replication reach the journal model
through the shared kernel, never through `tape-journal`.

## Source layout

```text
Cargo.toml                    virtual workspace: [workspace.package],
                              [workspace.dependencies], [workspace.lints]
crates/
  tape-shared-kernel/src/     lib.rs, tape_journal.rs, command.rs, ports.rs,
                              snapshot.rs, replay_wire.rs, ...
  tape-storage/src/           wal.rs + wal/, file_log.rs
  tape-replication/src/       raft.rs + raft/, peer_tls.rs
  tape-access/src/            authorization.rs
  tape-journal/src/           application.rs + application/,
                              interfaces.rs + interfaces/ (http, spec)
  tape-operator/src/          crd.rs, reconcile.rs, render.rs + render/
  tape/
    build.rs                  build stamp
    src/http.rs + http/       AppState and the router
    src/backup.rs             the backup runner (feature `backup`)
    src/bin/tape/main.rs      `tape`: entry point only
    src/bin/tape/*.rs         one module per subcommand
    tests/it/main.rs          the one integration-test binary; one module per file
  tape-bench/
    src/report.rs, durable.rs the bench harness
    src/bin/tape-bench/       `tape-bench`: main.rs (entry point only), cli.rs
    tests/tape_perf_gate.rs   standalone: needs a quiet release build
```

Every crate's `lib.rs` holds only `mod`, `pub mod` and `pub use` lines.
Modules use the `foo.rs` + `foo/` layout; `mod.rs` files are denied by
`[workspace.lints.clippy] mod_module_files = "deny"`, which every member
inherits with `[lints] workspace = true`. Versions, edition and license are
inherited from `[workspace.package]`, and every dependency version is written
once in `[workspace.dependencies]`.

## Layers

Inside one context a layer may use only the layers listed for it:

| Layer | May use |
|-------|---------|
| domain | — |
| application | domain |
| infrastructure | domain |
| interfaces | application |

Every layer may use the shared kernel. Another context may use a layered
context only through its application layer. The assembly crates belong to no
layer and may use any of them; they are where infrastructure is built and
handed to the application layer.

- **tape-shared-kernel** is pure: no I/O, no clock, no logging. Every
  time-dependent field of a [`TapeCommand`](domain/journal.md#model) is resolved
  by the caller before the command exists, so every replica applies it to the
  same state. It defines the two durability ports, `CommitLog` and
  `Replicator`.
- **tape-journal application** is `JournalService`: reads against the
  node-local journal, and mutations that go through the `Replicator` when one
  is set and through the `CommitLog` otherwise. It also owns `now_ms`, the one
  wall-clock read, the `JournalClock` extension trait that stamps
  `TapeJournal` mutations with it, and `TapeMetrics`.
- **tape-storage** and **tape-replication** implement the ports: `wal` (the
  group-commit write-ahead log), `file_log` (the legacy whole-file store), and
  `raft` (the raft-runtime state machine and host).
- **tape-journal interfaces** is `http`, the axum handlers, and `spec`, the
  hand-written offline contract `tape spec` and `tape llm` print.
- **tape** builds `AppState` (the service on its chosen backend, auth, the raft
  group) and composes the journal routes with `service-http`'s probe routes and
  raft-runtime's peer routes into one router.

## Dependency isolation

Each crate lists only the dependencies it uses, so the compiler enforces the
context boundaries and the default `tape` build does not link kube:
`tape-operator` (and with it `kube`, `k8s-openapi` and `service-k8s`) is an
optional dependency of `tape`, enabled by feature `operator`. utoipa is a
dependency of the shared kernel and `tape-journal` only.

## Shared kernel purity

The shared kernel may use only `std`, `serde`, `serde_json`, `thiserror` and the
other crates in `[policy.domain] allowed_crates`, and may not touch the file
system, network, environment, process, clock or standard streams. The checker
enforces this (rule B2), both in the source and in the kernel crate's
`Cargo.toml`.

## Exceptions

Each exception is recorded in `ddd.toml` with its files and reason.

| Rule | Subject | Files | Why |
|------|---------|-------|-----|
| B2 | `utoipa` | `crates/tape-shared-kernel/Cargo.toml`, `crates/tape-shared-kernel/src/{event,retention,subscription}.rs` | `ToSchema` is a compile-time description of the same serde shape. These types are served as-is in the OpenAPI document; an interfaces copy would duplicate the wire contract. |

## Size

Files under any crate's `src` over 400 lines are a warning and over 1000 lines
an error (`[policy] size_warn` / `size_error`). No file is allowlisted.

## Tests

- Unit tests live next to the code, in a `#[cfg(test)] mod tests;` child
  module, in the crate that owns the code. `cargo test --workspace` runs all
  of them.
- Integration tests are modules of the single `crates/tape/tests/it` binary
  (`[[test]] name = "it"`), so they share one link step. Feature-gated modules
  carry `#[cfg(feature = "...")]` on their `mod` line in `tests/it/main.rs`.
- A standalone `tests/<name>.rs` binary needs a reason in its leading `//!`
  block (`//! isolation: ...`). The only one is `tape-bench`'s
  `tape_perf_gate`, which times a release build and is `test = false`.
