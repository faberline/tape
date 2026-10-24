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
qualification blockers. It uses the pinned controller's `load_config`. All-green evidence remains
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

## Staging principle: Live required gates vs Candidate qualification

Under the Defer44 staging principle, live required gates must not introduce narrowing
qualification in front of all existing PR failure reconciliation prior to complete
activation.

### 1. Live required gates (`.github/issue-ci-required-gates.json`)
- Evaluated by the live default-branch `issue-ci.yml` reconciler.
- Contains only the active GKE test workflow inventory:
  `.github/workflows/gke-test.yml` (8 jobs):
  - `build test-client image`
  - `build server image / build (x86_64-unknown-linux-musl)`
  - `build server image / build (aarch64-unknown-linux-musl)`
  - `build server image / publish ghcr test image`
  - `resolve image identity`
  - `image e2e (arm64)`
  - `GKE e2e (operator_recovery) / deploy + test on shared GKE`
  - `GKE e2e (operator_ha) / deploy + test on shared GKE`
- Contains NO live `qualification` block. This ensures that any non-docs PR experiencing a GKE failure
  is cleanly reconciled to `todo` without failing qualification selection or applicability checks.
- Retains `lifecycle_mode: failure-only` and all 4 `qualification_blockers`.

### 2. Candidate qualification contract (`.github/issue-ci-candidate-gates.json`)
- Defines the staged qualification contract v1 for affected docs runbook copy qualification.
- Validated offline via `scripts/issue-ci/validate-policy.py`.
- Includes trusted workflow identities:
  - `.github/workflows/gke-test.yml` (ID: 389097869)
  - `.github/workflows/affected-docs.yml` (provisional ID: 1, definition blob SHA pinned)
- Checks:
  - `docs-copy-contract` (`kind: policy`): executes `validate-docs-copy.py`
  - `backup-destination-docs` (`kind: unit`): executes `run-backup-docs-tests.py`
- Mandatory policy check: `always: ["docs-copy-contract"]`
- Protected paths:
  - `.github/actions/**`
  - `.github/issue-ci-candidate-gates.json`
  - `.github/issue-ci-required-gates.json`
  - `.github/workflows/**`
  - `scripts/ci/**`
  - `scripts/issue-ci/**`
- Rules:
  Maps strictly to the three runbook documentation paths:
  - `docs/operations/README.md`
  - `docs/runbooks/cold-restore.md`
  - `docs/runbooks/disk-full.md`
  Requires `backup-destination-docs`.
  Explicit exemptions for `functional_e2e` and `performance` are justified because these changes
  are exact content-preserving copies of human-read documentation with no runtime, dependency,
  deployment, topology, storage, workload or selector changes.

## Trusted base execution for affected docs

In `.github/workflows/affected-docs.yml`:
- The `docs-copy-contract` job checks out the trusted base branch at `trusted-base` (`ref: ${{ github.event.pull_request.base.sha }}`).
- The product PR head is checked out at `product-head` (`ref: ${{ github.event.pull_request.head.sha }}`).
- The validator `trusted-base/scripts/issue-ci/validate-docs-copy.py` executes from the trusted base,
  treating all markdown files in `product-head` strictly as DATA.
- The `backup-destination-docs` job executes on the cloud Ubuntu runner via `scripts/issue-ci/run-backup-docs-tests.py`,
  running `cargo test --locked -p tape --test it -- backup_destination_docs::` and verifying all 6 named cases pass.

## Infrastructure CI on GitHub runners

Workflow `.github/workflows/ci-infrastructure.yml`:
- Triggers on `pull_request` touching infrastructure files:
  - `.github/workflows/**`
  - `scripts/issue-ci/**`
  - `.github/issue-ci*.json`
  - `docs/engineering/issue-ci-handoff.md`
- Runs in an unprivileged GitHub runner (`ubuntu-latest`) with `contents: read`.
- Validates:
  1. `python3 scripts/issue-ci/check-policy.py .github/issue-ci-required-gates.json`
  2. `python3 scripts/issue-ci/validate-policy.py .github/issue-ci-candidate-gates.json`
  3. `python3 -m unittest discover -s scripts/issue-ci/tests -v`
- Ensures PRs modifying CI infrastructure generate authentic cloud CI check evidence.

## Activation prerequisites for candidate qualification

Before promoting `.github/issue-ci-candidate-gates.json` into live `.github/issue-ci-required-gates.json`:
1. **Land PR #41 on default branch**: Establishes the validator scripts, test suite, and candidate policy definitions on main.
2. **Register `.github/workflows/affected-docs.yml`**: Ensure the workflow is registered in GitHub Actions on the default branch, and query the real numeric workflow ID assigned by GitHub.
3. **Record immutable blob SHA**: Compute the git blob SHA of `.github/workflows/affected-docs.yml` as committed on the default branch.
4. **Update `issue-ci.yml`**: Add `"affected-docs"` to `workflow_run.workflows` in `.github/workflows/issue-ci.yml`.
5. **Promote candidate gates**: Replace the live configuration in `.github/issue-ci-required-gates.json` with the candidate qualification configuration updated with the verified numeric workflow ID and default-branch blob SHA.

## Unresolved coverage decisions (blockers retained)

- No general pull_request unit workflow exists on the default branch.
- Existing gke-test is path-filtered and does not establish affected functional and E2E coverage for every product change.
- No declared affected performance pull_request gate exists; manual/release workflows are not PR qualification.
- A reviewed coverage selection policy and real qualifying jobs are needed before clearing these blockers; no blanket docs-only zero-check bypass is approved.

## Local affected verification

Run `python3 -m unittest discover -s scripts/issue-ci/tests -v`.
Tests execute offline with metadata and document fixtures only: no local product
E2E/performance, no cloud dispatch, and no issue mutations.
