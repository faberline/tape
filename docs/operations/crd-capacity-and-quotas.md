# CRD upgrade order, capacity and quotas

Copied from the retiring deployment handoff page (sections 3e to 3g). Text unchanged, except one relative link.

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
Per-topic and per-subscription quotas are an open roadmap outcome (`quotas-and-scale-transition`, see [product/operations.md](../product/operations.md) § Quotas and scale transition), not shipped behaviour. The admission classes cannot express them as they stand. `AdmissionController` lives in core's `service-http` crate, and supporting per-topic quotas there needs either (a) dynamically named classes at runtime (e.g. `"tape.write.{topic}"`, which requires `AdmissionConfig` to accept classes not known when the environment is parsed) or (b) a single class holding a per-key (per-topic) capacity map instead of one capacity. Both land in the shared library, not in this repository.

**Operator-side Workaround:**
Every existing control lever (the three admission classes, the body limit, and PVC size from `### 3f`) applies per Tape service instance, not per topic. The only way to give one topic a different effective quota than another today is to run a separate Tape CR (separate instance) per topic or topic group needing distinct limits, applying the sizing formulas in `### 3f` per instance. Because admission buckets per caller, giving each producer its own token also gives it its own write bucket, which bounds that producer across every topic it writes, not any one topic.

**Forward Pointer:**
When per-topic limits are built, the change starts in `service-http`'s `AdmissionConfig`/`AdmissionController` (a per-key or wildcard capacity map), owned as a shared-library update, and is then wired through the admission closure in `crates/tape/src/http/router.rs`.

