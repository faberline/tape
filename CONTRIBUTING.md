# Contributing to tape

## Brief

How to change `tape`. What it promises and the work roots it owns live in
[README.md](README.md); the per-surface support state is in
[STATUS.md](STATUS.md); repository-wide authoring and verification rules live
in [AW standards](https://github.com/faberline/aw/tree/main/docs/standards).

Use the shared Faberline `issue-plan`, `issue-work`, and `issue-review`
skills from [AW](https://github.com/faberline/aw/tree/main/plugins/faberline/skills).
Each skill runs one pass; its `-loop` companion adds an explicit interval.
Installing or updating a skill does not start a loop.

Planning writes the bounded issue contract. Work submits one issue's PR and
runs affected unit/static checks. CI runs the required functional, integration
and performance checks. It alone sets `done` after all required current-input
gates pass, leaving the issue open for independent local review. A required
CI failure reports its evidence and returns the issue to `todo`. Review reads
the issue, diff and current CI; it runs no tests and never merges, closes or
changes lifecycle labels. The controller owns Git and final acceptance.

Use the repository's existing complete gates for product claims. Keep local
work limited to affected unit/static checks; functional, integration and
performance execution belongs in authorized CI. Preserve all feature matrices,
test selection checks, numeric limits and exact-head/base evidence.

Integration tests are modules of one test binary, `crates/tape/tests/it/main.rs`
(`[[test]] name = "it"`). A new case is a new `crates/tape/tests/it/<case>.rs` plus its
`mod <case>;` line in `main.rs`, added in the same PR. Modules behind
a feature carry `#[cfg(feature = "...")]` on that line and only run with the
feature enabled. Run one case with `cargo test -p tape --test it -- <case>::`.
`crates/tape-bench/tests/tape_perf_gate.rs` is the one standalone target; its `//! isolation:`
line says why.

## Verification

| Gate | Command |
|---|---|
| Integration tests (`crates/tape/tests/it`) plus every crate's colocated unit tests | `cargo test --workspace` |
| Operator and backup feature targets | `cargo test -p tape --features operator,backup` |
| Release-mode performance ceiling | `cargo test --release -p tape-bench --test tape_perf_gate` |
| Product document contract | `uv run --python 3.13 --no-project scripts/meta/project_docs_contract.py check . --format json` |
| Shared GKE test cluster: suites `operator_recovery` and `operator_ha` (runs on pull requests that touch the operator, `k8s/`, or the suites; dispatch by hand otherwise) | `gh workflow run gke-test.yml` |

Run the document contract check after editing `README.md`, `STATUS.md`,
`ROADMAP.md`, or `clients/README.md`; it resolves every gate above to a
declared target and refuses a bare test-name filter.
