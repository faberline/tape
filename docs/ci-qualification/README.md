# Ordinary PR unit and static checks

This candidate continues the human-requested common test-coverage work after
[Core #74](https://github.com/faberline/core/pull/74) and
[Lumen #185](https://github.com/faberline/lumen/pull/185).
Tape's default branch has path-filtered GKE, document-copy and infrastructure
checks. It has no general workspace unit/static pull-request workflow.

## Scope and source

The baseline is `caf41306974493c35d95b56e21fd73ded9f8dc0b`. The new workflow
runs on every ordinary pull request with read-only permissions and no persisted
checkout credential. It adds three jobs: inventory and isolated controls,
workspace formatting, and a complete package/feature unit and Clippy matrix.
Formatting runs separately, so a format failure cannot hide the unit matrix.

`scripts/pr-gates/plan.py` and its controls are copied byte for byte from
`faberline/core@140b30f550d974d088f9b2f018c2b2ca3ff423e2`. That source's review
does not constitute independent review of this Tape adoption. The new workflow
uses pinned actions and preserves the existing GKE assertions, Claude,
docs-copy and qualification policy. The narrow runbook-copy contract in issue
#40 and the operator-documentation validation in issue #44 remain separate work.

## Manifest-derived coverage

Fresh locked Cargo metadata is compared with every workspace member manifest.
At the baseline there are eight packages, eight library targets and two binary
targets. The matrix has 18 package/feature rows:

| Package | Unit targets | Rows |
|---|---:|---:|
| `tape` | 2 | 8 |
| `tape-access` | 1 | 1 |
| `tape-bench` | 2 | 4 |
| `tape-journal` | 1 | 1 |
| `tape-operator` | 1 | 1 |
| `tape-replication` | 1 | 1 |
| `tape-shared-kernel` | 1 | 1 |
| `tape-storage` | 1 | 1 |

Every package has a default row. Each feature-bearing package also has
no-default, one row per named feature, and all-features. These are manifest
profiles, not the feature power set. Existing contracts that need particular
combined features retain their own tests. Optional local dependencies remain
in the advisory reverse-dependency graph. Unknown paths and docs-only PRs
cannot reduce execution: every PR runs the complete matrix.

Every enabled library and binary unit target runs separately. At the baseline
this means 30 ordinary unit invocations across the 18 rows, plus 18 all-target
Clippy invocations. Release retries for ignored tests add invocations when
needed. These counts are the plan, not evidence that the product tests passed.
The checked-in JSON snapshots are inspection metadata, not test results. Each
online job regenerates the plan from its exact event head and base.

## Evidence rules

The planner rejects missing, partial or duplicate package inventories, disabled
unit harnesses, removed baseline packages/features/targets, and lost target
applicability. It refuses matrices over 256 rows instead of truncating them.

Each target must produce a complete, named, nonempty passing libtest roster.
Failed, filtered, measured, duplicate or incomplete results fail. An empty
target requires a separately reviewed applicability decision; this candidate
does not waive it. A target's failure does not suppress later targets. Clippy
still runs after a unit failure, unless the workflow is cancelled.

If debug reports ignored tests, the complete same target must also pass in
release mode with `--include-ignored`. Every ignored name must pass there.
Both executions must succeed, and the release roster must have zero failed,
ignored, measured or filtered results. This preserves authored release
requirements without accepting skipped work or changing performance bounds.

Clippy uses `--all-targets -- -D warnings` for every row. Workspace formatting
uses `cargo fmt --all -- --check`. No lint exemption or test threshold changes
are part of this candidate. Dependency caches exclude workspace source crates.
Four rows run at most at once. The 30-minute job timeout is a runner budget,
not a measured product-performance oracle.

## Preparation checkpoint, 2026-10-28

Local inspection used locked offline Cargo metadata at the named baseline.
The 53 isolated helper controls passed with Python 3.13. The helper requires
Python 3.11 or newer; the local default Python 3.9 could not import `tomllib`,
so it was not used for the passing controls. Workflow YAML parsed with three
jobs. Rust product tests and builds were not run during this preparation.
Normal online PR execution and independent adoption review remain required.

The first online run at `3f7c32b9d679b09a4a1e5e38c1bb35969bb0f5a7`
completed formatting, inventory and 13 matrix rows successfully. Five rows
failed the nonempty-target rule: the operator library had no unit tests, and
the benchmark binary had no unit tests in its four feature profiles. All
other existing targets passed. These failures did not justify an exception.

The repair adds four operator rendering tests for durable storage and shutdown,
child identity and selectors, fail-closed credential projection, and backup
creation and pruning. It adds five benchmark CLI tests per feature profile for
argument contracts, invalid input, the empty-sweep guard and feature-gated
commands. The tests use no cluster or network. Production behavior, existing
test assertions and every gate threshold stay unchanged. Their product test
and Clippy results must come from the next exact-head online run.

The local inspection commands are:

```sh
cargo metadata --locked --no-deps --offline --format-version 1 > /tmp/tape-metadata.json
python3.13 scripts/pr-gates/plan.py inventory --root . --metadata-json /tmp/tape-metadata.json
python3.13 scripts/pr-gates/plan.py matrix --root . --metadata-json /tmp/tape-metadata.json
python3.13 -m unittest discover -s scripts/pr-gates/tests -v
```

Product commands run online with verified full head/base identities:

```sh
cargo test --locked -p <package> --lib <feature-arguments>
cargo test --locked -p <package> --bin <binary> <feature-arguments>
cargo clippy --locked -p <package> --all-targets <feature-arguments> -- -D warnings
cargo fmt --all -- --check
```

## Qualification and cloud-test boundary

The new checks execute candidate-owned code. Their success does not establish
trusted qualification. The live failure-only policy and its coverage blockers
remain intact. Approval of immutable workflow/driver identities, applicable
functional/E2E and measured-performance mappings, and actual current-head
trusted evidence remain necessary before separate qualification adoption.

Historical GKE run
[40011714270](https://github.com/faberline/tape/actions/runs/40011714270)
failed before creating the test Job: the operator could not be scheduled.
Saved events and Compute audit records identify `SSD_TOTAL_GB`, regional limit
500 GB in `axiom-502607` / `asia-east1`. Those records date from 2026-10-23,
before the shared N4/16 GB migration. A quota read on 2026-10-28 reported
0/500 GB for that metric. It does not prove another quota's usage or a new product pass.
The old product PR's head and failed evidence remain unchanged by this work.

The current candidate's GKE run at `bc04493` reached a different, confirmed
blocker before deployment: the pinned AW pool manifest still required
`e2-standard-2`, while the actual Tape pools were already `n4-standard-2`.
The repair keeps the reusable workflow at
`f5b4c093ca2b5525e594c6ea154f0159af1fb9cc` and sets its `aw-ref` input to
`23ce1028e73e8c6342ac14b0bc650c19db5898a3`, the applied N4/16 GB revision.
The GKE scripts are identical between these revisions. This keeps every
execution assertion and the machine-type consistency check intact.

The test-only Tape CR also explicitly selects the cluster's `dynamic-rwo`
StorageClass. The default `standard-rwo` provisions PD Balanced, which N4
cannot attach. `dynamic-rwo` selects Hyperdisk Balanced on these nodes. Its
minimum data-volume size is
[4 GiB](https://docs.cloud.google.com/compute/docs/disks/hd-types/hyperdisk-balanced),
so the test fixture requests `4Gi` instead of `1Gi`. This data PVC is separate
from each node's unchanged 16 GB boot disk. No production default, test
assertion, resource limit, timeout or performance bound changes.

This candidate neither completes any product issue nor authorizes lifecycle
activation, automatic merge or release. The existing product tests, runbook
contracts and broader performance obligations remain with their original work.
