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
  - `.github/workflows/gke-test.yml` (ID: 389097869, blob SHA: `052a2e83dcac71da8b6292661d4200c985b3c1d3`)
  - `.github/workflows/affected-docs.yml` (provisional ID: 1, blob SHA: `9782c830864c85559763ddb556ac32bfa3c9c835`)
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

## Trusted base execution and deterministic document validation

In `.github/workflows/affected-docs.yml`:
- The `docs-copy-contract` job checks out the trusted base branch at `trusted-base` (`ref: ${{ github.event.pull_request.base.sha }}`).
- The product PR head is checked out at `product-head` (`ref: ${{ github.event.pull_request.head.sha }}`).
- The validator `trusted-base/scripts/issue-ci/validate-docs-copy.py` executes from the trusted base,
  treating all markdown files in `product-head` strictly as DATA.
- **Frozen Git tree & NUL-delimited inventory**:
  - The validator uses `GitCheckoutLoader` across both checkouts, connecting them via `GIT_ALTERNATE_OBJECT_DIRECTORIES`.
  - It runs `git diff-tree -r -z --raw -M -C <base_sha> <head_sha>` to obtain complete change metadata.
  - Rejects forbidden renames, copies, type changes, symlinks (`120000`), mode changes (`old_mode != new_mode`), and unexpected paths.
  - Verifies exact `100644` modes for all allowed files.
  - Reads document bytes directly from git blob objects at the frozen commits (avoiding untracked filesystem diffs and text newline normalization).
  - Strictly rejects any CRLF (`\r\n`) or CR (`\r`) in document bytes.
- **Exact operations whole-document transform**:
  - Requires the two exact runbook rows:
    ```markdown
    | [Cold restore runbook](../runbooks/cold-restore.md) | Ad-hoc backups, destination storage schemes, and cold restore via bootstrap-if-empty |
    | [Disk-full runbook](../runbooks/disk-full.md) | Degraded read-only mode on ENOSPC, metric diagnosis, and volume recovery |
    ```
    to be added immediately preceding the node-drain row.
  - Reconstructed check: after removing EXACTLY these two rows, the remaining document must be 100% byte-identical to base `docs/operations/README.md`.
- **Mandatory prerequisite wrapper and §6 smoke sequence contract**:
  - Both `docs/runbooks/cold-restore.md` and `docs/runbooks/disk-full.md` must include exactly one mandatory prerequisite wrapper paragraph immediately following the document's H1 heading and one blank line (`\n\n`), followed by one blank line before the extracted runbook body:
    ```markdown
    > Prerequisite: Set `BASE` in this shell to the target Tape HTTP endpoint (`BASE=http://localhost:7137` for the local endpoint), as defined in the [deployment handoff smoke sequence](../deployment-handoff.md#6-smoke-sequence-end-to-end).
    ```
  - The relative source link `[deployment handoff smoke sequence](../deployment-handoff.md#6-smoke-sequence-end-to-end)` is strictly verified against the frozen base `docs/deployment-handoff.md`, asserting section 6 heading `## 6. Smoke sequence (end-to-end)` exists and defines `BASE=http://localhost:7137`.
  - The validator permits only that exact wrapper text, endpoint, link, count (1), and placement (immediately after H1).
  - The validator strips only this single inserted block before performing byte-preserving comparison against the normalized base handoff sections. Any missing, malformed, duplicate, or misplaced wrapper, extra blockquote command, or CRLF fails validation with explicit named assertions.
- **Exact Cargo PR head execution**:
  - The `backup-destination-docs` job explicitly checks out `ref: ${{ github.event.pull_request.head.sha }}` (not default merge commit).
  - Step verifies `git rev-parse HEAD` equals `github.event.pull_request.head.sha` and logs both head and base.
  - Runs `scripts/issue-ci/run-backup-docs-tests.py` on the Ubuntu runner, verifying all 6 named `backup_destination_docs::` tests pass (6 passed, 0 failed, 0 ignored).

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

## Offline Claude review-only enable audit candidate

### Goal

This unpublished branch is the complete configuration proposed for independent
review. It changes the local review jobs from unconditional false to guarded
conditions and promotes the fixed CLI supply to `reviewed_pinned`. It does not
publish, deploy, call Claude, dispatch CI or execute a binary. The existing
public draft remains the separately published disabled revision.

`.github/claude-review-enable-candidate.json` records the candidate identities
and remaining acceptance. Live deployment and publication authorization are
false. Merge remains hard disabled, with no write scope or workflow ID invented.

### How

The trusted `Issue CI handoff` tail now requires a successful `pull_request`
wake from the same repository. `claude-preflight` depends on the existing
handoff, and the model job requires both preflight success and its authenticated
eligible output. The shared action independently rechecks every required job,
exact current head/base and the wake identity; YAML conditions are not evidence
by themselves. The handoff prefix, failure-only mode, eight required GKE jobs
and all four genuine coverage blockers are retained. No immediate 99% threshold
is introduced; that remains a future five-milestone measured objective.

New review jobs have only contents/Actions/PR/issues read. Provisioning checks
out `github.workflow_sha`, not a PR head, with credentials not persisted. Only
its final immutable shared-action step receives the existing named OAuth secret.
No token value or validity call is part of this offline work. The reviewer has
no tools/MCP and runs in an isolated temporary home as defined by the pinned
shared implementation. Managed machine/account policy still needs inspection;
the settings flags cannot override administrator-managed hooks.

The review action stays pinned to AW
`cc157b0eb296d1ac4a428db7ed0c023d42b87ac3`, published in
[AW draft #86](https://github.com/faberline/aw/pull/86). The review producer reuses
real workflow ID `394214906` and job `claude-final-review`; its definition blob
is recomputed from the actual conditional workflow bytes. GKE keeps real ID
`389097869`, all exact job names, and candidate definition blob
`d1530edc69924ee42ff630a477ecef4735cd68d4`. Observed deployed blob
`052a2e83dcac71da8b6292661d4200c985b3c1d3` remains separate historical evidence.
Both the reusable workflow and its scripts input use the same unpublished AW
`f5b4c093ca2b5525e594c6ea154f0159af1fb9cc`; publish that separately reviewed source
before any consumer rollout. See the dependency section below for owner/ref
proofs and residual runtime supply limits.

CLI version `2.1.310` uses only fixed official `downloads.claude.ai` URLs. The
exact manifest/signature/public-key bytes are preserved in the fixtures. An
independent existing cloud `/usr/bin/gpg`, isolated temporary keyring and matched
sizes/hashes verified the signature with exit 0, GOODSIG and VALIDSIG fingerprint
`31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE`, matching the official
[release integrity instructions](https://code.claude.com/docs/en/setup#binary-integrity-and-code-signing).
No local verification tool was installed. Earlier normal-context DNS failures
and the successful normally approved read context remain distinguished in
provenance; neither was treated as a credential denial.

Manifest SHA256 is
`895e85ce24c3df3fdb77ac1006bdb2b66f1c42dd557000e65ced1e4aad847e74`.
One bounded official linux-x64 download measured exactly 291638640 bytes with
SHA256 `b73676430af12f97316a798b8371f81d640f90a58bdcb8f3ce56a835bb47df56`.
It prohibited redirects, required the exact Content-Length and identity encoding,
used a 15-second socket timeout and 180-second process deadline, and wrote only
mode-0600 temporary data. A second local read confirmed size/hash/type/mode.
The binary was neither installed nor executed. The integrity receipt is data,
not runner/model execution or deployment authority.

The selected supply's expected hashes and `enabled=true` / `reviewed_pinned`
values are local enable-proposal configuration. No script is invoked merely by
editing these files. Its helper still authenticates the manifest and verifies
newly downloaded binary bytes on a future authorized runner; it accepts no
arbitrary URL/version, redirect or credentialed provisioning process. Official
npm remains an unselected alternative; no npm package, script or installation
was fetched or executed for this candidate.

### Acceptance

The actual diff includes conditions, supply promotion, producer blob, immutable
source/script pins and negative controls, rather than instructions to do them.
Offline tests reject unconditional/incomplete job conditions, fork bypass,
permission expansion, mutable pins, stale producer blobs, removed gates or
blockers, mismatched integrity receipts and a false deployment claim. Source-only
validation does not prove an actual GitHub runner or OAuth/model response.

Still required: independent review of this complete enable/dependency diff,
publication permission covering its GKE path effects, publishing the reviewed AW
workflow/scripts commit, separately authorized infrastructure deployment, managed
CLI policy inspection and real current-head gates/review transport evidence.
Existing product PR heads must contain matching reviewed gate definitions at
head/base before the collector can accept them; an old green run is insufficient.
No actual functional/e2e/performance/model run occurred in this offline work.

The trusted gate boundary now holds `crates/tape/tests/`, Cargo manifests/lock,
workspace/crate target and build-hook configuration, `.cargo/` and toolchain
selection paths before model review. `.github/claude-review-test-contract.json`
records their role. A legitimate test/configuration change needs separate
independent test/gate-contract review; this is a manual route, not a verdict
that the change is wrong. Ordinary `crates/tape/src/` product changes remain
reviewable. Existing failure-only mode and genuine coverage blockers remain.

Keep the corresponding existing hourly Claude cloud review running alongside CI
review during rollout. Stop that corresponding schedule only after a real
exact-current-HEAD Claude job conclusion, an authenticated valid review receipt
and guard negative evidence establish that CI review works. Installing workflows,
ordinary CI success, static fixtures or disabled jobs are insufficient. This
candidate performs no schedule or cron change.

### Never

The separate `claude-guarded-merge.yml` jobs stay false and read-only; AW `merge`
refuses before any API call. A successful review cannot authorize a merge.
Cross-run durable intent/result reconciliation, mutual exclusion and an approved
provider base/protection contract remain separate prerequisites. Expected-head
PUT alone is not atomic exact-base enforcement.

No permission, protection, environment, credential or infrastructure change is
made here. The earlier branch metadata observation reports `protected=false`
and status-check enforcement off; it is not proof that all rulesets are absent.
Effective deployment permissions and managed-policy behavior require independent
inspection. Do not infer live readiness from an offline candidate or clear true
coverage blockers to activate it.

## Offline test-workflow dependency pin candidate

### Goal

Freeze existing action/reusable-workflow source references while preserving test
commands, job names, required gates and permissions. The dependency-only base
preserved disabled Claude/merge jobs; this combined enable-audit branch adds the
review conditions above while keeping merge false. This remains unpublished
and is not qualification or rollout evidence.

### How

`.github/ci-dependency-pins-candidate.json` records normal read-only GitHub refs
from action owner/maintainer repositories and the exact resolved commits. The
Swatinem v2 annotated tag is explicitly dereferenced to its commit; a tag-object
SHA is never substituted for an action commit. Referenced tag/branch names are
observation provenance, not runtime refs. Current major-tag resolution may be a
different action revision from a historical run; independent source review is
still required.

The AW GKE workflow candidate freezes checkout, Google auth/setup-gcloud and
artifact action refs. Its unchanged scripts and infrastructure derive from AW
main `59ed3084c991606fc41b55a3d59088307b6aef62`. Tape points both reusable `uses`
and its explicit `aw-ref` checkout input to the same local AW candidate commit.
This avoids leaving `scripts/gke-test` at the reusable workflow's default main.
The AW candidate is not yet published; remote action access is not established.

Tape's test-image workflow freezes nine existing owner/maintainer action refs.
The existing GKE workflow numeric ID and every required job remain intact.
The trusted-policy extension binds the **candidate** workflow blob, while the
previous deployed blob and run conclusions stay in observed provenance. This
is not a claim that candidate definitions already exist on the default branch.

### Acceptance

Offline tests reverse exactly the declared dependency substitutions and the
one frozen scripts-ref input, then require the original workflow Git blob IDs.
An unrelated command/content change, mutable ref, foreign source URL, mismatch
between reusable/scripts SHA or false provenance fails. YAML syntax and existing
failure-only/disabled-job guards remain checked. No real GKE or product test
has run for these new definitions.

Pinned `uses` sources are only one boundary. Hosted Ubuntu tools/images, apt
packages, Rust stable compiler selection inside the frozen toolchain action,
QEMU/Buildx image/tool downloads, gcloud/components and arbitrary transitive
action runtime downloads still need separate supply review and exact version/
digest policy. This candidate does not claim a hermetic or fully qualified run.

### Never

Do not publish this candidate under the earlier review-only draft permission:
changing `.github/workflows/gke-test.yml` **does** match Tape's automatic GKE PR
path filter. A future push/PR therefore requires separate authorization for
that ordinary GKE effect after independent infrastructure review. Do not manually
dispatch, change credentials/permissions/protections, deploy the proposed review
conditions, enable merge, clear qualification blockers or claim live readiness
from these offline tests.
