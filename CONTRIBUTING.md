# Contributing to tape

## Brief

How to change `tape`. What it promises and the work roots it owns live in
[README.md](README.md); the per-surface support state is in
[STATUS.md](STATUS.md); repository-wide authoring and verification rules live
in the root [CONTRIBUTING.md](https://github.com/faberline/workspace/blob/main/CONTRIBUTING.md).

Use `product-deliver` for authorized work. QA owns the red e2e case and its
registration. Dev owns the red unit test and scoped implementation. A fresh
`tape-qa` runs the declared complete gate. The controller owns Git, tracker,
and acceptance. Legacy AW use is explicit-only.

Integration tests are modules of one test binary, `crates/tape/tests/it/main.rs`
(`[[test]] name = "it"`). A new case is a new `crates/tape/tests/it/<case>.rs` plus its
`mod <case>;` line in `main.rs`, added in the same `e2e` phase. Modules behind
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
| Local kind acceptance (manual) | `bash scripts/kind-e2e.sh` |

Run the document contract check after editing `README.md`, `STATUS.md`,
`ROADMAP.md`, or `clients/README.md`; it resolves every gate above to a
declared target and refuses a bare test-name filter.
