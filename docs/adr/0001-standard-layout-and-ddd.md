# ADR 0001: Standard layout and bounded contexts

- **Status:** accepted; superseded in part by [ADR 0002](0002-multi-crate-workspace.md) (crate layout)
- **Date:** 2026-10-07

## Context

tape was split out of the faberline monorepo into its own repository. It kept
the monorepo's shape: a one-member Cargo workspace, flat root modules
(`server`, `raft`, `wal`, `spec`, `auth`, `backup`, `bench`, `metrics`), two
large binary files, and integration tests under `e2e/` registered one
`[[test]]` each. The shared building blocks it uses already live in
faberline/core, which follows the organisation's standard Rust layout and
domain-driven design rules, checked by the workspace's rust-arch contract.

## Decisions

- **One package, no workspace.** A workspace with one member adds a level of
  indirection and nothing else (rule A2). Versions and metadata are written
  directly in `[package]` and `[dependencies]`.
- **Three contexts in one crate.** `journal` is tape's model and serving path;
  `access` is its adoption of `service-auth`; `operator` is its Kubernetes
  resource. Splitting them into crates would add publish and version overhead
  for no reuse: nothing outside tape uses them.
- **journal is fully layered.** The domain (`TapeJournal`, `TapeCommand`, the
  snapshot format, the durability ports) is pure. The application layer
  (`JournalService`) is the only thing the HTTP interface calls. WAL, legacy
  store and raft are infrastructure behind the `CommitLog` and `Replicator`
  ports.
- **access is application only, operator is infrastructure only.** access is
  configuration plus a policy function over `service-auth`; it has no model of
  its own. operator is a CRD and a reconcile loop over `service-k8s`; its
  shapes are wire formats, not a domain.
- **Wiring lives in `src/app`.** `AppState`, the router, the bench harness
  and the backup runner build infrastructure and hand it to the application
  layer, so they are assembly, not a layer.
- **Binaries are thin.** `src/bin/tape/main.rs` and
  `src/bin/tape-bench/main.rs` hold only `fn main`; each subcommand is a sibling
  module (rule B6).
- **One integration-test binary.** Every integration test is a module of
  `tests/it/main.rs`. `tape_perf_gate` stays standalone because it measures a
  quiet release build, and says so in its `//! isolation:` line.
- **Long-term exceptions for the wire types.** Six journal domain types are
  tape's JSON format and OpenAPI schemas. They derive `utoipa::ToSchema` in the
  domain (B2) and the HTTP interface names them directly (B3). A duplicate set
  of interface types would have to be kept identical to them by hand, which is
  the drift this rule exists to prevent.
- **No compatibility re-exports.** The old module paths (`tape::server`,
  `tape::raft`, `tape::wal`, ...) are removed, not aliased. tape is an
  application; nothing outside this repository imports its library.

## Consequences

- The rust-arch checker runs with `[migration] enforce = true`; any new
  error-level finding fails it.
- A new exception, or a new path in an existing one, needs a reason in
  `ddd.toml` and is caught by the checker's ratchet against `main`.
- Moving a file named in an exception needs a `[[renames]]` entry.
