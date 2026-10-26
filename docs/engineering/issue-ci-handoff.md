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

## Disabled Claude review-only integration candidate

### Goal

Prepare review after the existing required tests succeed, preserving the current
failure-only handoff. The new `claude-preflight` and `claude-final-review` jobs
are appended to this repository's existing trusted `Issue CI handoff` workflow,
which wakes after `gke-test`. They are **unconditionally disabled**. No secret is
added to the PR-triggered GKE/docs/infrastructure workflows. The original
review-only change preserved their bytes; the separate offline dependency-pin
candidate below changes only declared refs and the AW scripts identity input.
Gate names and coverage blockers remain unchanged. No 99% threshold is added;
that remains a future five-milestone objective requiring real measurement.

### How

`.github/issue-ci-required-gates.json` now contains a staged `claude_final_review`
extension. The legacy handoff ignores this extension; its existing controller
bytes and policy fields are unchanged. The review producer reuses the real
Issue CI workflow ID `394214906`, with exact job `claude-final-review` and the
new local workflow blob. The required GKE ID is `389097869`, definition blob
`052a2e83dcac71da8b6292661d4200c985b3c1d3`; all eight exact jobs are retained.
The AW action pin is `cc157b0eb296d1ac4a428db7ed0c023d42b87ac3`, published as the
exact immutable head of [AW draft #86](https://github.com/faberline/aw/pull/86). It contains a hard merge hold
and does not require merge workflow registration for review-only operation.

`.github/claude-review-pilot-provenance.json` distinguishes existing default-
branch blobs/IDs from new candidate bytes. Observations come from normal read-only
GitHub metadata at main `1c556e237f9c1d3566b0f0d8dcee4591d4b44992`, not an old
disabled tree. GKE run `40011714270` has six successes and two operator failures;
it is historical failure evidence, not a qualifying run for this new candidate.
The docs workflow ID `395044816` and infrastructure ID `395010308` are observed
facts; neither is substituted for required GKE jobs by this change.

CLI supply is a separate, credential-free step from trusted default-branch
`github.workflow_sha`, with checkout credentials not persisted. The provisioning
helper never executes the downloaded binary or an installer shell script. It
allows only fixed version `2.1.310`, official HTTPS origin `downloads.claude.ai`,
the versioned manifest/signature/binary paths and release-key path; redirects,
ambient proxies, arbitrary URL/version arguments and credentialed provisioning
are refused. Before staging a binary it requires independently pinned manifest
and binary SHA256 hashes, a valid manifest signature rooted in exact fingerprint
`31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE`, matching signed linux-x64 checksum and
matching downloaded bytes. GPG uses an isolated temporary keyring and receives
no credentials. These checks follow the official
[release integrity instructions](https://code.claude.com/docs/en/setup#binary-integrity-and-code-signing).

Initial normal-context requests did not obtain the official manifest: the web
reader reported it unavailable and a public HTTPS fetch returned URLError, with
cause unknown. At that initial checkpoint no Linux binary or signing key had
been downloaded, signature checked or CLI executed, and the disabled supply and
policy hashes were null. Those failures did not establish access denial or a
verified release. The subsequent authenticated-manifest result below supersedes
that initial state; binary download availability and bytes remain unverified.

One subsequent bounded ordinary HTTPS retry used the same exact URL, a
15-second timeout and a 256 KiB limit. It failed in DNS resolution with
`URLError.reason` type `gaierror`, errno `8`; no HTTP response was obtained.
That identifies this retry's transport failure, not an access denial or a
retrospective explanation of the earlier unknown failure. No TLS bypass,
network/proxy setting change, binary download or further retry was performed.
The sanitized result is recorded separately in provenance.

An alternative **for independent supply review only** is Anthropic's documented
[npm distribution](https://code.claude.com/docs/en/setup#install-with-npm), pinned
to `@anthropic-ai/claude-code@2.1.310`. Before considering it, fetch official
registry metadata without credentials, pin `dist.integrity` and every required
platform/transitive package, inspect tarballs with install scripts disabled,
review the actual native-payload source, then bind the resulting Linux binary
hash to the review policy. Node.js 22 or later is required by the documented npm
package. Its installer may still fetch a native binary from the currently
unresolved host; npm is not yet proven to resolve this transport blocker.
Registry SRI is a different trust mechanism from the native release's GPG
signature. No equivalence or source switch is assumed: registry metadata and
tarballs have not been fetched, npm has not been executed, and the selected
native/GPG supply and all disabled flags remain unchanged.

A later normal `require_escalated` approval permitted one bounded read-only GET
per official manifest/signature/public-key resource. All three returned HTTP 200.
This proves availability in that approved execution context, not the cause of
the earlier DNS failures and not a general network or credential-access change.
The unmodified public bytes are preserved under
`scripts/issue-ci/tests/fixtures/claude-native-2.1.310/`; their observed SHA256
digests and exact URLs are in `.github/claude-review-native-integrity-candidate.json`.
The 2.1.310 manifest names binary `claude` for linux-x64, size 291638640 bytes,
with checksum `b73676430af12f97316a798b8371f81d640f90a58bdcb8f3ce56a835bb47df56`.
Manifest SHA256 is `895e85ce24c3df3fdb77ac1006bdb2b66f1c42dd557000e65ced1e4aad847e74`.

These are **observed candidate values, not trusted supply pins**. No installed
GPG, GPGV or Sequoia verifier was found in the checked standard locations; no
tool was installed. The public-key fingerprint and detached signature have not
been independently verified at that retrieval checkpoint. No Linux binary was
downloaded or executed.

The exact preserved public bytes were subsequently verified independently using
an existing cloud `/usr/bin/gpg` and an isolated temporary keyring. All three
artifact sizes and SHA256 values matched. Public-key import succeeded; signature
verification exited 0 with GOODSIG and VALIDSIG fingerprint
`31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE`, matching the official documented trust
anchor. This is independently reported cryptographic verification of the
manifest, not local Mac verification and not verification of downloaded binary
bytes. The local Mac verifier remains unavailable and no tools were installed.

The authenticated manifest hash and expected linux-x64 binary checksum are now
staged in the disabled supply file and review policy. The supply's status is
`manifest_signature_verified_binary_verification_pending`, with `enabled=false`.
The binary has not been downloaded, hashed or executed. Even changing only
`enabled` to true remains a refusal while this pending status is present.
Independently review the supply helper and this pin/provenance update, verify
actual binary bytes against the signed expected checksum, and obtain separate
deployment authorization before any activation. No draft PR was updated with
this unreviewed follow-up.

After a separate safe-download review, one normally approved official linux-x64
binary GET completed with the exact signed-manifest size of 291638640 bytes and
SHA256 `b73676430af12f97316a798b8371f81d640f90a58bdcb8f3ce56a835bb47df56`.
The request prohibited redirects, required the exact Content-Length and identity
encoding, used a 15-second socket timeout and 180-second process deadline, and
streamed at most the declared size into a unique mode-0600 temporary data file.
A second local read confirmed size, hash, regular-file type and permissions.
The binary was neither installed nor executed. The non-executable evidence is
in `.github/claude-review-binary-integrity-candidate.json`.

This completes artifact integrity evidence, not credentialed runner or semantic
review evidence. The selected supply and jobs remain disabled. The existing
`manifest_signature_verified_binary_verification_pending` supply hold label is
retained until an independently reviewed promotion; the separate current binary
record reports the completed hash check explicitly. Publishing this evidence,
promoting supply to `reviewed_pinned`, and enabling review jobs require separate
review/authorization. Merge remains hard disabled.

Only the final shared review action step references the existing same-name OAuth
Actions secret. Its presence/value has not been inspected. The new jobs override
the handoff's broader workflow permissions to contents/Actions/PR/issues read.
No repository permission, credential, environment or cloud resource is created.

### Acceptance

Offline tests check unchanged handoff prefix, complete retained gate/blocker
policy, unconditional job disabling, read-only scopes, fixed action pin, default-
branch provisioning, isolated secret scope, signed-supply validation and
corrupted/missing hash/signature negative controls. Synthetic bytes and mocked
GPG prove helper behavior; they do not prove a real signed release or runner.
The local infrastructure suite remains the verification command above.

Before separately authorized deployment, independently review shared AW and this
consumer diff, publish the reviewed immutable AW revision, verify real official
CLI supply and hashes, update the actual default-branch producer blob, inspect
machine/account managed CLI configuration, and establish real current-head test
and review evidence. Existing mutable GKE reusable/action dependencies remain
visible in provenance for source review; this candidate does not silently claim
their complete dependency trust. Do not remove true coverage blockers to activate.

### Never

`.github/workflows/claude-guarded-merge.yml` is a separately disabled pending
workflow. It has no write scope, no OAuth secret, no real merge call and no
invented workflow ID in policy. Shared AW also refuses `merge` before any API
call. Durable cross-run intent/result reconciliation, mutual exclusion and an
approved provider base/protection contract are still required. An ordinary
expected-head PUT is not atomic exact-base enforcement. No automatic merge is
authorized by a successful review.

The branch metadata currently reports main `protected=false`, branch protection
disabled and branch status-check enforcement off. This does **not** establish
absence of all rulesets: rulesets were not queried. Effective token permissions,
shared-action access, secret availability, organization-managed CLI policy and
merge-queue contract remain unverified. No protection bypass, new credential,
permission expansion or new infrastructure is proposed to resolve those gaps.

## Offline test-workflow dependency pin candidate

### Goal

Freeze existing action/reusable-workflow source references while preserving test
commands, job names, required gates, permissions and disabled Claude/merge jobs.
This is a separate unpublished candidate, not qualification or rollout evidence.

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
dispatch, change credentials/permissions/protections, enable Claude/merge jobs,
clear qualification blockers or claim live readiness from these offline tests.
