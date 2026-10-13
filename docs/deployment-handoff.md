# tape — test-environment deployment handoff

> One verified path for another team to stand up tape (topic replay journal)
> in a test environment without reading source. tape is the **durable
> replay/archive surface** — `relay` is the online broker and delivery
> surface; tape is for re-reading history after the fact (offset/time replay,
> consumer checkpoints, backfill/audit).

Covers: which image to run, the supported CLI surface, runbooks for binary /
Docker / Kubernetes (operator-driven), the environment variables, an
end-to-end smoke sequence, the backup/restore runbook, and the gates that
prove a build is release-ready.

---

## 1. Image / binary

| Path | Use | Runtime base |
|------|-----|--------------|
| `Dockerfile.release` | **Production-like.** Fetches the published release binary — no Rust toolchain, smallest attack surface. | distroless |
| `Dockerfile` | Dev/CI build from source (`cargo build --release -p tape`). | distroless |
| `Dockerfile.test` | **Internal only (#2576).** Same runtime stage as `Dockerfile.release`, but COPYs a locally built binary — the only way to get an *unreleased* commit into a cluster. | distroless |

```bash
tape dockerfile render --variant release --version 0.1.0 --out /tmp/tape-image
docker build -f /tmp/tape-image/Dockerfile.release -t tape:0.1.0 .
docker run --rm -p 7137:7137 tape:0.1.0 serve
```

`tape dockerfile render --variant source` renders the source-build Dockerfile
for local/CI images. Raft peer RPCs share the same h2c HTTP port as the
serving API (no separate peer port).

### Two published image lines

| Line | Tag | Produced by | Audience |
|------|-----|-------------|----------|
| release | `ghcr.io/faberline/tape:<semver>` + `latest` | `.github/workflows/tape-release.yml` | integrators — the only line to hand a user |
| dev/test | `ghcr.io/faberline/tape:sha-<git12>` | `.github/workflows/tape-test-image.yml` | acceptance harnesses only |

The dev/test line exists so that verifying a commit in a real cluster does
not require cutting a release first — that is how tape went 0.4.5 → 0.4.11
during the acceptance campaign, seven version bumps carrying no user-facing
change. Dispatch `tape-test-image` (optionally naming a `ref`); its run
summary prints the pushed tag already pinned by digest, ready to paste into
the acceptance harness's `TAPE_IMAGE`. Both lines land on the same GHCR
package, so `sha-*` tags inherit its public visibility and GKE pulls them
without a pull secret. `tape-test-image.yml` must never add `latest` or a
semver tag.

---

## 2. CLI surface (first-level commands)

`tape --help` (from `crates/tape/src/bin/tape/cli.rs`):

| Command | Purpose |
|---------|---------|
| `tape serve` | Run a serving node: HTTP API + (in HA mode) the raft group. |
| `tape append` / `tape replay` / `tape checkpoint` | Local file-backed journal admin (no server needed). |
| `tape spec` / `tape spec gen` | Print the machine-readable contract, or generate a typed ts/py/rust client. |
| `tape backup` | Pull a snapshot from a running node's `/admin/backup` and ship it to a destination sink (#1329). |
| `tape dockerfile` | Render source/release Dockerfiles. |
| `tape k8s` | Render Kubernetes layers: `crd render`, `operator render\|run`, `instance render`. |
| `tape llm` / `tape upgrade` / `tape issue` | Shared ecosystem CLI convention (agent docs, self-update, issue search/view/create). |

---

## 3. Runbooks

### 3a. Local binary (single node, embedded journal)

```bash
./scripts/dev-single.sh
# or directly:
tape serve --bind 127.0.0.1:7137 --store .tape/journal.json
```

### 3b. Docker

```bash
docker run --rm -p 7137:7137 tape:0.1.0 serve --bind 0.0.0.0:7137
```

### 3c. Local multi-node raft (auto-mode HA)

```bash
./scripts/dev-cluster.sh
```

Boots 3 `tape serve` processes on `:7137`/`:7138`/`:7139` with
`REPLICAS_PER_SHARD=3` so they form one raft group; append on any node
replicates to the others.

### 3d. Kubernetes (operator, CRD-driven)

```bash
tape k8s crd render --out /tmp/tape-k8s/crd.yaml
tape k8s operator render --namespace tape-system --out /tmp/tape-k8s
kubectl apply -f /tmp/tape-k8s/crd.yaml       # cluster-scoped API
kubectl apply -f /tmp/tape-k8s/operator.yaml  # control-plane namespace/RBAC/controller
```

Then create a `Tape` custom resource in the app namespace; the operator
reconciles the StatefulSet, Service, and PDB for that CR's namespace:

```bash
tape k8s instance render --profile staging --namespace journal --name tape --out /tmp/tape-k8s
kubectl apply -f /tmp/tape-k8s/tape.yaml
```

Profiles: `dev` (one pod, small disk, verbose logs), `staging` (prod-shaped
single node), `prod` (3-replica raft-HA group, auth required), `template`
(fill-in-the-blanks skeleton). HA (`replicasPerShard > 1`) instances must
keep the CR name `tape` — `tape serve` derives raft peer DNS as
`tape-<ordinal>.<peer-service>`.

> **Live proof (#1590):** the disposable Kind gate builds the real image,
> reconciles a Tape instance through the operator, appends to PVC-backed
> storage, replaces the pod, and verifies replay from the retained PVC. Raft
> repeated-leader-loss evidence remains the real h2c integration gate; the Kind
> proof intentionally covers the operator/PVC replacement boundary.

### 3e. CRD versioning & upgrade order

**Versioning Policy:**
`k8s/operator/crd.yaml` serves a single version, `v1alpha1` (`served: true`, `storage: true`), with no `conversion:` stanza (API server strategy defaults to `None`). Additive `spec` and `status` fields land inside `v1alpha1` across releases. Renaming, removing, or retyping an existing field requires a new CRD version and a conversion mechanism; no conversion webhook exists today, which is stated as a present limitation.

**Upgrade Order (CRD First):**
Always apply the updated CRD before deploying a newer operator or applying updated Custom Resources (following the `kubectl apply` sequence in `### 3d`).
*Why CRD first:* The Kubernetes API server silently prunes any field omitted from the stored CRD schema without error. For example, `spec.auth` (#2765) is a closed enum (`disabled|required`) defaulting to `required`. Applying a CR with `auth: disabled` against a pre-#2765 CRD silently prunes `auth` before storing the object. A post-#2765 operator reading the stored object gets the serde default `required`, finds no token source, and `AuthConfig::resolve` fails fast into a `CrashLoopBackOff`.

**Rollback Contract:**
Tape does not define a format-version constant on its durable path. A single-node `--data-dir` holds a `journal-<seq>.snap` snapshot (a JSON `TapeJournal`) plus a `journal.wal` of JSON `TapeCommand` frames; `--store` holds one whole-file JSON `TapeJournal`. Rollback behavior is asymmetric between binary versions and is further split by where a new field lands in the durable schema:

| Direction | Behavior & Hazard |
|---|---|
| Newer binary / older data | Safe by construction — `TapeJournal` fields carry `#[serde(default)]` attributes (e.g. `next_offsets`, `subscriptions`, `retention`) to load older state safely. |
| Older binary / top-level journal field | **Loads without error**, silently dropping unknown top-level fields. A `--store` file loses them on the next write, because every write rewrites the whole document; a `--data-dir` loses them when the next snapshot is written (every 1024 committed writes). Restoring a newer backup into an older binary exhibits the same loss. |
| Older binary / nested record (`deny_unknown_fields`) | **Fails to load and exits non-zero**; process does not start. `Subscription` (`crates/tape-shared-kernel/src/subscription.rs`) carries `#[serde(deny_unknown_fields)]` today. From a `--data-dir` snapshot the error is the bare serde message (`Error: unknown field ...`); from a `--store` file it carries `parse <path>` context. This is fail-loud, not silent data loss. |
| Older binary / WAL command it does not know | **Fails to load and exits non-zero**: a `journal.wal` frame holding a `TapeCommand` variant the older binary lacks does not decode, so recovery stops at startup. |
| Pre-WAL binary / migrated `--data-dir` | **Loads stale state without error.** The first WAL-era start seeds a snapshot from `journal.json` and never deletes or updates that file, so a pre-WAL binary reads `journal.json` as it stood at migration and every write since is missing. |

*Operator Action for Rollback:* Copy the data directory **before** starting an older binary, as top-level field loss happens on the next rewrite. If the older binary fails to start with a decode error (`unknown field ...`, `unknown variant ...`, or `parse ...`), this is the fail-loud case for a record or command the older binary does not know; inspect the error context rather than assuming storage corruption. Do not roll a `--data-dir` back across the WAL migration unless losing every write since that migration is acceptable. Leave the newer CRD installed when rolling back a binary if CRs use newer fields, and revert only the container image; do not downgrade the CRD schema. The top-level and nested-record cases were measured directly against a `--data-dir` snapshot and a `--store` file; the WAL-command and pre-WAL rows follow from the decode and migration code, not a run; no broader matrix across released versions exists, and replica (raft) state is not covered here.

### 3f. Capacity planning & PVC sizing

**Retention Fork (Evaluate First):**
Before applying any capacity formula, determine whether topic retention is configured.
* **No Retention Policy (Default):** the `TapeJournal` `retention` map is empty by default (`crates/tape-shared-kernel/src/tape_journal.rs`). The journal grows without bound, so `N` is total accumulated events over volume lifespan. A PVC sized from steady-state rate will eventually fill.
* **With Retention Policy:** `N` is the retained event window bounded by `min_offset` / `max_age_seconds` and floored by `protected_consumers` (`RetentionPolicy`, `crates/tape-shared-kernel/src/retention.rs`), which prevents pruning past the oldest protected consumer checkpoint.

**On-Disk Layout:**
At rest, a single-node `--data-dir` holds two files: `journal-<seq>.snap` (history up to the last snapshot boundary) and `journal.wal` (events appended after that boundary). At `DEFAULT_SNAPSHOT_THRESHOLD = 1024` frames (`crates/tape-storage/src/wal.rs`), the store writes a snapshot, truncates the WAL through it, and keeps exactly one snapshot via `prune(1)` (`WalStore::snapshot_and_truncate`, `crates/tape-storage/src/wal/store.rs`). History is stored **once**, not duplicated across snapshot and WAL; assuming double-counting will over-provision by 2×.

**Per-Event Overhead Rates:**

| tier | bytes per event |
|---|---|
| `journal.wal` | P + 128 |
| `journal-<seq>.snap` | P + 71 + d |

where `P` is raw payload content length in bytes (enclosing JSON string quotes are absorbed into the constants) and `d` is the number of decimal digits in the event's offset. A WAL frame holds the append command, which carries no offset, so its rate is flat; a snapshot entry carries the offset, so its rate rises one byte per digit (P + 72 at offset 0, P + 74 by offset 100). A snapshot file also carries about 105 bytes of fixed overhead.

**Sizing Formulas:**
```
bytes(N, P) ≈ S × (P + 71 + D) + (N − S) × (P + 128)
    where S = 1024 × floor(N / 1024)
          D = number of decimal digits in the largest offset, N − 1
```
* **Safe Upper Bound:** `N × (P + 128)` for any `N`.
* **Large-N Approximation:** `N × (P + 71 + D)`, which under-counts the WAL tail by less than `1024 × 57 = 58,368` bytes (57 KiB) regardless of `N`, because the WAL never holds more than 1024 events.

**Worked Example (Large-N Form):**
For `N = 10,000,000` retained events (largest offset 9,999,999, so `D = 7`) and raw payload size `P = 1024` bytes:

```
bytes(10_000_000, 1024) ≈ 10_000_000 × (1024 + 71 + 7)
                        = 10_000_000 × 1102
                        = 11,020,000,000 bytes   (≈ 11.02 GB)
```

Adding 20% headroom for snapshot transient writes and growth: `11,020,000,000 × 1.20 = 13,224,000,000` bytes (≈ 13.22 GB PVC provisioned).

**Measurement Basis & Operational Limits:**
Constants reflect four measured `tape serve --data-dir` runs against a fresh data directory, appending over one keep-alive connection (`P` of 128, 1024, and 4096 at `N=2000`, plus `P=1024` at `N=6000`). The WAL rate was exact in all four. The formula lands 1,005 bytes above the measured size in every run (it rounds every snapshot offset up to `D` digits, so it errs high), which is 0.22% at `P=128` and under 0.05% for the larger runs.
Unmeasured boundaries requiring headroom:
1. Transient disk peak while a new snapshot coexists with the old before `prune(1)`.
2. Multi-topic directory overhead (measured on single-topic runs).
3. Non-default snapshot thresholds (`WalStore::open_with_snapshot_threshold`).
4. Non-JSON string payloads.
5. A failed snapshot or truncate: the WAL keeps growing at `P + 128` per event until a later snapshot succeeds.
6. Replica (raft) data directories, which use a different store.

See `### Disk-full runbook (#2573)` for remediation if disk capacity is exhausted.

### 3g. Per-topic quotas (boundary)

While `### 3f` bounds whole-instance disk capacity, per-topic quotas would govern per-topic throughput, payload size, or event count limits. Tape has none today.

**Current State:**
Request admission in Tape is keyed by request class and caller, never by topic:
* **Boot wiring:** `AdmissionConfig::from_env("TAPE")?.controller("tape.read", "tape.write", "tape.admin")` (`crates/tape/src/bin/tape/serve.rs`) registers exactly three fixed admission classes. Admission is off unless at least one of `TAPE_ADMISSION_READ_CAPACITY`, `TAPE_ADMISSION_WRITE_CAPACITY`, or `TAPE_ADMISSION_ADMIN_CAPACITY` is set; `TAPE_ADMISSION_REFILL_SECS` and `TAPE_ADMISSION_MAX_KEYS` tune the shared token buckets.
* **Class selection:** The admission closure in `crates/tape/src/http/router.rs` routes `/admin/` to `tape.admin`, `GET` to `tape.read`, and every other method to `tape.write`. The bucket key is the request's `Authorization` header, or `anonymous` when there is none.

Every topic funnels into the same three classes; there is no per-topic capacity slot anywhere in this closure. With `TAPE_ADMISSION_WRITE_CAPACITY=2` and a one-hour refill, two tokenless appends to topic `a` succeeded and the next append to topic `b` was refused with `429 rate_limited`, while a read of `b` still answered `200`: one caller's appends to different topics draw on one bucket.

The other limits are no finer. The request body limit (`body_limit_bytes`) is one value for the whole instance. Retention (`### 3f`) is set per topic but bounds how many events are kept, not how fast they arrive or how large each one is.

**Boundary:**
Per-topic and per-subscription quotas are an open roadmap outcome (`quotas-and-scale-transition`, see [product/operations.md](product/operations.md) § Quotas and scale transition), not shipped behaviour. The admission classes cannot express them as they stand. `AdmissionController` lives in core's `service-http` crate, and supporting per-topic quotas there needs either (a) dynamically named classes at runtime (e.g. `"tape.write.{topic}"`, which requires `AdmissionConfig` to accept classes not known when the environment is parsed) or (b) a single class holding a per-key (per-topic) capacity map instead of one capacity. Both land in the shared library, not in this repository.

**Operator-side Workaround:**
Every existing control lever (the three admission classes, the body limit, and PVC size from `### 3f`) applies per Tape service instance, not per topic. The only way to give one topic a different effective quota than another today is to run a separate Tape CR (separate instance) per topic or topic group needing distinct limits, applying the sizing formulas in `### 3f` per instance. Because admission buckets per caller, giving each producer its own token also gives it its own write bucket, which bounds that producer across every topic it writes, not any one topic.

**Forward Pointer:**
When per-topic limits are built, the change starts in `service-http`'s `AdmissionConfig`/`AdmissionController` (a per-key or wildcard capacity map), owned as a shared-library update, and is then wired through the admission closure in `crates/tape/src/http/router.rs`.

---

## 4. Environment variables

`serve` reads flags and env (flag wins). Source: `crates/tape/src/bin/tape/serve.rs::ServeArgs`,
`crates/tape-access/src/authorization.rs`, core's `raft-runtime` (`src/cluster.rs`).

| Area | Env (≡ flag) | Default |
|------|--------------|---------|
| Bind | `TAPE_BIND` (`--bind`) | `127.0.0.1:7137` |
| Storage | `TAPE_STORE` (`--store`) | unset (in-memory) |
| Shutdown | `TAPE_GRACE_SECS` (`--grace-secs`) — the whole SIGTERM shutdown budget; set `terminationGracePeriodSeconds` to this plus 5 | `10` |
| Shutdown | `TAPE_DRAIN_DELAY_SECS` (`--drain-delay-secs`) — how long the node keeps serving after `/readyz` turns 503, capped by the grace budget | `5` |
| **Auth** | `TAPE_AUTH` (`--auth`) — `off`\|`required`; `TAPE_TOKEN_REGISTRY_FILE` (`--token-registry-file`) bearer-token registry JSON; `TAPE_TOKENS` legacy/dev inline fallback | `off` |
| **Raft HA** | `TAPE_DATA_DIR` (`--data-dir`), `TAPE_PEER_SERVICE` (`--peer-service`), `TAPE_PEERS`, plus the standard `POD_NAME`/`SHARD_COUNT`/`REPLICAS_PER_SHARD`/`VOTER_COUNT` downward-API quartet | peer-service `tape` |
| Peer mTLS | `TAPE_PEER_TLS_CERT`/`_KEY`/`_CA`, `TAPE_PEER_MTLS=on\|off` | unset (cleartext peer transport only when mTLS is off) |
| Backup | `TAPE_BACKUP_TOKEN` (`tape backup --token`) | unset |
| **ENOSPC recovery** | `TAPE_STORAGE_FULL_REPROBE_SECS` — how often a degraded node re-probes its store directory (#2573) | `30` |

> **Production:** set `TAPE_AUTH=required` + `TAPE_TOKEN_REGISTRY_FILE`, and
> for HA deployments set `REPLICAS_PER_SHARD>1` plus a durable `TAPE_DATA_DIR`
> volume. With `TAPE_PEER_MTLS=on`, complete peer material is validated before
> serving and the shared `raft-runtime` transport binds a dedicated required-mTLS
> raft listener. Public HTTP remains on the separate h2c + HTTP/1.1 port.

---

## 5. HTTP surface & probes

Registered in `crates/tape/src/http/router.rs` and `crates/tape-journal/src/interfaces/http.rs`
(standard probes via core's `service-http` shell; data-plane routes are
tape-specific):

| Path | Purpose | Auth |
|------|---------|------|
| `GET /healthz` | Liveness. | no |
| `GET /readyz` | Readiness — `503` from SIGTERM until the process exits. | no |
| `GET /metrics` | Prometheus text. | no |
| `GET /openapi.json`, `GET /docs` | OpenAPI 3 + Swagger UI. | no |
| `GET /admin/backup` | Stream a whole-journal snapshot. | per `TAPE_AUTH` (needs `admin` role on `*` when `--auth required`) |
| `POST /topics/{topic}/append` | Append one message envelope; returns the assigned offset after the durable write. | per `TAPE_AUTH` (write) |
| `POST /topics/{topic}/subscriptions` | Create a named subscription on the topic. | per `TAPE_AUTH` (write) |
| `GET /topics/{topic}/subscriptions` | List the topic's subscriptions. | per `TAPE_AUTH` (read) |
| `GET /topics/{topic}/subscriptions/{subscription}` | Show one subscription. | per `TAPE_AUTH` (read) |
| `DELETE /topics/{topic}/subscriptions/{subscription}` | Delete a subscription. | per `TAPE_AUTH` (write) |
| `POST /topics/{topic}/subscriptions/{subscription}/pull` | Pull a bounded window from the subscription cursor. | per `TAPE_AUTH` (read) |
| `POST /topics/{topic}/subscriptions/{subscription}/ack` | Advance the subscription cursor cumulatively. | per `TAPE_AUTH` (write) |
| `PUT /topics/{topic}/retention` | Set the topic retention policy. | per `TAPE_AUTH` (write) |
| `GET /topics/{topic}/retention` | Read the topic retention policy. | per `TAPE_AUTH` (read) |
| `GET /topics/{topic}/replay` | Legacy: replay by offset or timestamp. Leaves with the seek outcome in `ROADMAP.md`. | per `TAPE_AUTH` (read) |
| `GET /topics/{topic}/replay/stream` | Legacy: compact read-only h2c bulk replay. Leaves with the seek outcome. | per `TAPE_AUTH` (read) |
| `PUT /topics/{topic}/consumers/{consumer}/checkpoint` | Legacy: advance a consumer cursor without a subscription. Leaves with the seek outcome. | per `TAPE_AUTH` (write) |
| `GET /topics/{topic}/consumers/{consumer}/checkpoint` | Legacy: read a consumer cursor. Leaves with the seek outcome. | per `TAPE_AUTH` (read) |

The served set equals `tape spec --format routes`; `cargo test -p tape --test it --
spec_route_parity::` refuses any drift between the router, the spec inventory,
and `clients/openapi.json`.

---

## 6. Smoke sequence (end-to-end)

```bash
BASE=http://localhost:7137
curl -fs $BASE/healthz                                    # -> ok
curl -fs $BASE/readyz                                     # -> ok (200)

curl -fs -X POST $BASE/topics/orders.created/append \
  -H 'content-type: application/json' \
  -d '{"payload":{"order_id":"o-1"}}'

curl -fs "$BASE/topics/orders.created/replay?from_offset=0" | jq '.events'

curl -fs -X PUT $BASE/topics/orders.created/consumers/c1/checkpoint \
  -H 'content-type: application/json' -d '{"offset":0}'

curl -fs $BASE/metrics | head -5                          # Prometheus text
```

---

## 7. Backup / restore runbook (#1329)

```bash
# operator-driven or ad hoc, against a running node:
tape backup --url http://localhost:7137 --dest file:///tmp/tape-backups \
  --token "$TAPE_BACKUP_TOKEN" --retention-secs 604800
```

`--dest` accepts `file://` (always supported), `s3://` (feature `backup`,
`service-backup/s3`), or `gs://` (feature `backup`, always linked —
`service-backup/gcs` is unconditional). `gs://` authenticates via
workload-identity ADC in-cluster (GKE-proven) and Vat's
`STORAGE_EMULATOR_HOST` locally. The bearer token needs `admin` on `*`; omit
it entirely when the node runs `--auth off`. `--retention-secs` prunes older
backup objects at the destination after a successful put; omit to keep
everything. In-cluster, the operator's backup CronJob passes
`http://<name>.<namespace>.svc.cluster.local:7137` as `--url`.

### Cold restore runbook (#2465, #2468)

`bootstrapSeedUri` is **bootstrap-if-empty**, not a live restore endpoint: on
every boot the server first checks whether its durable `TAPE_DATA_DIR`
already has raft state. An empty data directory seeds from the URI as
before; a non-empty one (including a routine pod restart onto its own
already-bootstrapped PVC) **skips the seed and boots from its existing
state** instead of refusing, so the field is safe to leave declared on the
CR (fixed in #2468 — it used to be one-shot: any restart while the field was
still set crash-looped on the non-empty-PVC refusal).

1. Set `spec.bootstrapSeedUri` on a **fresh** instance (empty PVCs — a new
   deployment, or one whose PVCs were explicitly wiped) to the backup object
   URI.
2. Wait for the CR to report Ready, then verify the expected data replayed
   (e.g. `curl $BASE/topics/<topic>/replay?from_offset=0`).
3. Nothing further is required — future pod replacements restart onto the
   same populated PVC, detect existing state, and skip the seed
   automatically (`decision="skipped_existing_state"` in the pod log).
4. Recommended hygiene, not a requirement: remove the field once the
   bootstrap has converged.
   ```bash
   kubectl patch tape/<name> --type=json \
     -p '[{"op":"remove","path":"/spec/bootstrapSeedUri"}]'
   ```
   This guards against a narrower follow-on hazard: if the PVC is ever
   deleted and reprovisioned (not a routine restart) while the field is
   still set, the replacement pod's data dir really is empty again and WILL
   re-seed from the (possibly now-stale) backup object. Removing the field
   after a successful bootstrap makes that recreate-then-restart sequence
   fail loudly (no seed configured) instead of silently reseeding old data.

### Disk-full runbook (#2573)

A tape node that hits `ENOSPC` on its journal persist path latches **degraded
read-only mode**: mutating requests (append, checkpoint advance, subscription
create/delete/ack, retention set) answer `507` with error kind
`storage_full`, and **reads keep serving**. It does not crash, and it is not
`NotReady` — a full disk is precisely when an operator most needs to read what
is already journalled.

**Do not start by restarting the pod.** The node recovers itself: it re-probes
its store directory every `TAPE_STORAGE_FULL_REPROBE_SECS` (default `30`) and
clears the flag on the first successful write. Restarting only helps as a way
to reset the flag after you have already made room, and it costs you the
replay window during the restart.

1. Confirm the diagnosis:
   ```bash
   curl -s $BASE/metrics | grep tape_storage_
   ```
   `tape_storage_degraded 1` = currently refusing writes.
2. Read `tape_storage_full_errors_total` next. A **rising counter with the
   gauge back at 0** is the important case: the volume is flapping in and out
   of full, not recovered. The gauge alone cannot show that — a node that
   fills and re-probes clean between two scrapes reads healthy at both. The
   `TapeStorageDegraded` alert uses `max_over_time(...[5m])` for the same
   reason.
3. Make room. Either shrink the journal through retention:
   ```bash
   curl -X PUT $BASE/topics/<topic>/retention -d '{"max_events": <n>}'
   ```
   (note this **also** returns `507` while degraded — a retention change
   rewrites the whole journal through a temp file, so it needs room for a
   second copy before the old one is unlinked; free space at the volume level
   first), or expand the PVC on a resizable StorageClass:
   ```bash
   kubectl patch pvc data-<name>-0 -p \
     '{"spec":{"resources":{"requests":{"storage":"<bigger>"}}}}'
   ```
4. Wait one re-probe interval and re-check `/metrics`. The pod log carries
   `storage re-probe write succeeded; leaving degraded read-only mode`.
5. If the gauge stays `1` after the volume demonstrably has room, read the pod
   log for the re-probe warning — the store directory can be unwritable for
   reasons other than capacity (read-only remount, permissions, a failed CSI
   attach).

---

## 8. Release-readiness gates

A build is **not** production-ready until these pass (run from repo root
unless noted):

```bash
cargo build -p tape
cargo test -p tape

TAPE_SOAK_AUTOSTART=1 TAPE_SOAK_DURATION_SECS=60 bash scripts/soak.sh
```

> Shared service hardening is runnable here: topic authz, projected-secret
> rotation, bounded request admission, redacted management audit, and the
> bounded replay/checkpoint soak and shared raft-runtime peer mTLS. Retention/
> backfill and compaction are Tape domain work; multi-hour production soak
> remains separate evidence and is not claimed by this gate.

---

*Generated as the tape docs+scripts+traits polish handoff (#1331, epic
#1324). Coordinates (env names, ports, paths) are sourced from
`crates/tape/src/bin/tape/serve.rs`, `crates/tape/src/http/router.rs`, `crates/tape-journal/src/interfaces/http.rs`,
`crates/tape-access/src/authorization.rs`, core's `raft-runtime` (`src/cluster.rs`), `k8s/`,
`Dockerfile.release`.*
