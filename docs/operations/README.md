# Operations

How to deploy, run, benchmark and check tape. The deployment and runbook pages
predate this directory and keep their paths, because generated alert rules and
the product docs link to them.

| Document | What it answers |
|----------|-----------------|
| [Deployment handoff](../deployment-handoff.md) | Images, serve flags, environment, HTTP surface, smoke sequence, backup and restore runbooks |
| [Node drain and PodDisruptionBudget](../runbooks/node-drain-and-pdb.md) | Unblocking a drain on the direct-install singleton and verifying recovery |
| [Benchmark history](../benchmarks-scale.md) | The local performance gate and the retired peer-broker calibrations |

## Building and testing

```sh
cargo build --release                       # tape and tape-bench
cargo test                                  # every crate's unit tests and crates/tape/tests/it
cargo test --features operator,backup       # plus the operator and backup modules
cargo test --release --test tape_perf_gate  # the local performance gate
```

`crates/tape/tests/it` starts real `tape serve` processes, including three-process raft
groups. `raft_failover::concurrent_ingress_across_all_replicas_commits_without_raft_timeouts`
is timing-sensitive on a loaded machine; run it alone with
`cargo test --test it raft_failover -- --test-threads=1` when it fails.

## Using core

tape depends on faberline/core crates by git tag, one entry per crate, all at
the same tag:

```toml
raft-runtime = { git = "https://github.com/faberline/core", tag = "v0.4.14" }
```

To move to a new core release, change every tag together, refresh
`Cargo.lock`, and read that release's `docs/migration/` notes in core. Two tags
in one build mean two copies of every shared type.

## Architecture checker

The architecture contract is [`ddd.toml`](../../ddd.toml) at the repository
root; [architecture.md](../architecture.md) explains it. The checker lives in
the workspace repo:

```sh
uv run <workspace>/scripts/meta/rust_arch_contract.py check --repo tape
uv run <workspace>/scripts/meta/rust_arch_contract.py check --repo tape --base main
```

`--repo` takes a repo name under `--root` (default `~/faberlines`) or a path;
`--verbose` shows every finding. With `--base`, the ratchet fails on any new
exception or exception path, any `[policy]` change, and turning `enforce` off.

When the checker reports a finding, fix it by moving the code to the right
layer or context. If that is not possible, add an `[[exceptions]]` entry with
the rule, the subject the checker printed, the exact file paths, and a reason
that says why the break is permanent.
