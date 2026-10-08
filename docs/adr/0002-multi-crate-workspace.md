# ADR 0002: Multi-crate workspace

- **Status:** accepted
- **Date:** 2026-10-08
- **Supersedes:** the "one package, no workspace" and "three contexts in one
  crate" decisions of [ADR 0001](0001-standard-layout-and-ddd.md)

## Context

ADR 0001 laid tape out as one package holding three contexts. That made the
context boundaries a checker rule only: any module could still name any other,
and every build compiled and linked every dependency, so the serving binary
pulled in kube even though only the operator uses it. faberline/core and the
organisation's other Rust repositories are virtual workspaces with one crate
per context, which the rust-arch contract checks through its multi-crate
branch (A2, B1).

The journal context also held two pieces of infrastructure that do not use
each other: the single-node storage (WAL and legacy file store) and the raft
replication. Both implement ports the journal model defines.

## Decisions

- **Virtual workspace.** The root `Cargo.toml` holds only `[workspace]`,
  `[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]` and
  the profiles. Members live under `crates/<package>` and inherit everything
  they can. The release version stays a single column-0 `version = "x.y.z"`
  line in the root manifest, which the release tooling reads.
- **One crate per context, named `tape-<context>`.**
  - `tape-storage` and `tape-replication` split the journal's infrastructure
    along the two ports, so neither links the other's dependencies.
  - `tape-access`, `tape-journal` (application and interfaces) and
    `tape-operator` keep the contexts ADR 0001 named.
- **The journal model is the shared kernel.** `TapeJournal`, `TapeCommand`,
  the snapshot format and the `CommitLog` and `Replicator` ports move to
  `tape-shared-kernel`. Storage and replication must name them, and a
  cross-context reference to another context's domain layer is a B4 error; a
  shared kernel is the form the contract allows for a model several contexts
  build on. This also removes ADR 0001's B3 exception, because the HTTP
  interface now names kernel types, not another layer's.
- **The clock stays out of the kernel.** The wall-clock convenience methods on
  `TapeJournal` would be an orphan inherent impl outside the kernel and a B2
  error inside it. They become the `JournalClock` extension trait in
  `tape-journal`'s application layer, next to `now_ms`.
- **Two assembly crates.** `tape` holds `AppState`, the router, the backup
  runner, the `tape` binary and the integration tests. `tape-bench` holds the
  bench harness, its binary and the release-mode performance gate, so the
  perf-only dependencies stay out of `tape`.
- **Feature names do not change.** `operator`, `backup`, `otel`,
  `self-update` and `issue` stay features of `tape`; `operator` now enables
  the optional `tape-operator` dependency. Build scripts, the Dockerfile and
  CI keep their `-p tape --features ...` commands.
- **A test seam crosses a crate boundary.** `JournalService::
  set_inject_storage_full` was `#[cfg(test)]`; its caller is now in another
  crate, so it is an always-compiled `#[doc(hidden)] pub` method, like
  `WalStore::inject_next_sync_failure_with_kind`.

## Consequences

- The compiler enforces the context map: a crate can use only what its
  manifest lists.
- The default `tape` build does not compile or link kube.
- `cargo test --workspace` is the full unit and integration run; `-p tape`
  alone runs only the assembly crate's tests.
- The utoipa B2 exception names the kernel's `Cargo.toml` as well as its
  sources, because the checker also reads kernel manifests.
