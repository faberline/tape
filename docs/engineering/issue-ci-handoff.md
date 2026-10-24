# CI lifecycle handoff and qualification contract (failure-only)

This change adopts the reviewed `aw issue-ci` qualification contract v1 in
explicit `lifecycle_mode: failure-only`. A failed required current-head gate can
return a linked issue from `doing` to `todo` with failure/run links. **Success
cannot mark `done`.** Missing, skipped or untrusted evidence remains pending;
nonempty `qualification_blockers` remain in force.

## Trusted implementation and policy

`scripts/issue-ci/handoff.py` and `scripts/issue-ci/qualification.py` are unchanged
copies from `faberline/aw@693e45bded9a4ec8167573e82b423c823c193ec0` at
`plugins/faberline/skills/issue-ci/scripts/`.
- `handoff.py` SHA-256: `8313b8c5bf88a42c4cd8b3131fc576360fb0d2b6d120aa30d1a820f4e5ada773`.
- `qualification.py` SHA-256: `acc235139940b4e5d8ea1e75b4101ac442ee14a1e77ea01207838a55fdc7eaed`.
- Pinned shared action: `faberline/aw/.github/actions/issue-ci@693e45bded9a4ec8167573e82b423c823c193ec0`.

The workflow checks out only the repository default branch with persisted
credentials disabled. It never checks out or executes PR code, reads a PR gate
manifest, or accepts artifacts as authorization. All writers share the
`issue-ci-handoff` concurrency group. Each wake reconciles all managed PRs.
Permissions are contents/actions/PR read and issues write only.

The workflow guard requires explicit failure-only mode and an explicit nonempty list of
qualification blockers. It uses the pinned controller's `load_config` and
validates the `qualification` v1 contract schema. All-green evidence remains
pending even if blockers are accidentally cleared. A separate reviewed rollout
decision is required to authorize any future done transition; this guard refuses full mode.

CI alone may return a submitted `doing` issue to `todo` after a required current-
head run/job failure. It does not touch an already `done` issue or an unclaimed
`todo` issue. Existing journal checkpoints can finish interrupted recovery
without undoing an AGY claim. Other labels are preserved. Missing/pending/
skipped/cancelled/neutral/stale evidence is never a pass. Reruns use the latest
attempt. AGY owns only `todo -> doing`; review after eventual complete CI
qualification never merges automatically. No review schedule is created here.

The controller requires a same-repository `issue/N` or `agy/issue-N` branch
and the exact body line `Closes #N`. Other branch conventions and forks are
refused. Receipts deduplicate handoffs per head/policy/coding epoch; unrelated
labels are preserved. The controller rechecks current head and complete run
snapshots before writes and lifecycle state before removing old labels.

## Affected docs qualification gates

Workflow: `.github/workflows/affected-docs.yml`
Blob SHA: `bbcb437325754ea56e7d5a91477804d11ecdafd6`

Two real cloud jobs are configured on GitHub runners:
1. `docs-copy-contract` (`kind: policy`): Deterministic validator (`scripts/issue-ci/validate-docs-copy.py`)
   verifies that PR changes are strictly bounded to the three runbook copy paths, byte-identical
   handoff, promoted section levels, verbatim scheme preservation, and correct operations navigation.
2. `backup-destination-docs` (`kind: unit`): Runs affected Rust test selector:
   `cargo test --locked -p tape --test it -- backup_destination_docs::`
   via `scripts/issue-ci/run-backup-docs-tests.py`, ensuring all six named cases pass.

Applicability rules map only:
- `docs/operations/README.md`
- `docs/runbooks/cold-restore.md`
- `docs/runbooks/disk-full.md`
Unknown or extra changed paths fail closed.
Exemptions for `functional_e2e` and `performance` are explicitly justified for these exact
documentation-copy paths (no executable, deployed, topology, storage, or workload change).

## Gate inventory

Manifest `.github/issue-ci-required-gates.json`:
1. `.github/workflows/gke-test.yml` (8 jobs):
   - build test-client image
   - build server image / build (x86_64-unknown-linux-musl)
   - build server image / build (aarch64-unknown-linux-musl)
   - build server image / publish ghcr test image
   - resolve image identity
   - image e2e (arm64)
   - GKE e2e (operator_recovery) / deploy + test on shared GKE
   - GKE e2e (operator_ha) / deploy + test on shared GKE
2. `.github/workflows/affected-docs.yml` (2 jobs):
   - docs-copy-contract
   - backup-destination-docs

## Unresolved coverage decisions (blockers retained)

- No general pull_request unit workflow exists on the default branch.
- Existing gke-test is path-filtered and does not establish affected functional and E2E coverage for every product change.
- No declared affected performance pull_request gate exists; manual/release workflows are not PR qualification.
- A reviewed coverage selection policy and real qualifying jobs are needed before clearing these blockers; no blanket docs-only zero-check bypass is approved.

## Local affected verification

Run `python3 -m unittest discover -s scripts/issue-ci/tests -v`.
Tests execute offline with metadata and document fixtures only: no local product
E2E/performance, no cloud dispatch, and no issue mutations.
