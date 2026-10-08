# tape docs

Project-level documentation for tape. Usage starts at the root
[`README.md`](../README.md); contributor rules live in
[`CONTRIBUTING.md`](../CONTRIBUTING.md).

| Document | What it answers |
|----------|-----------------|
| [architecture.md](architecture.md) | How the workspace is cut into crates, bounded contexts and layers, what may depend on what, and which rule breaks are allowed and why. |
| [glossary.md](glossary.md) | The terms tape's code, API and docs use, and what each one means here. |
| [domain/](domain/) | One page per context: what it models, its ports, its invariants, and its recorded exceptions. |
| [adr/](adr/) | Architecture decisions and the reasons behind them. |
| [operations/](operations/README.md) | Deploying, running, benchmarking and checking tape, with links to the runbooks. |
| [product/](product/README.md) | What tape promises per capability area. |

The architecture contract itself is [`ddd.toml`](../ddd.toml) at the repository
root. It is checked by the workspace's rust-arch contract
(`scripts/meta/rust_arch_contract.py` in the workspace repo); see
[operations](operations/README.md#architecture-checker) for the command.

## Domain pages

| Context | What it owns |
|---------|--------------|
| [journal](domain/journal.md) | topics, events, consumer checkpoints, pull subscriptions, retention, and the service and HTTP interface over them |
| [storage](domain/storage.md) | the single-node commit logs: the group-commit WAL and the legacy whole-file store |
| [replication](domain/replication.md) | the raft group that replicates every write, and peer TLS |
| [access](domain/access.md) | bearer authentication and per-topic authorization |
| [operator](domain/operator.md) | the `Tape` custom resource and its reconcile loop |
