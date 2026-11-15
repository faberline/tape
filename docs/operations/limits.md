# Limits

The limits below are per Tape instance, not per topic.

| Limit | Value | Where it is set | Above the limit |
|---|---|---|---|
| Request body size (data plane) | 8 MiB = `8388608` bytes, default | env `TAPE_BODY_LIMIT_BYTES` / flag `--body-limit-bytes`, default in `crates/tape/src/bin/tape/serve.rs` (`ServeArgs::body_limit_bytes`); CRD field `body_limit_bytes` in `crates/tape-operator/src/crd.rs` | the request is refused with HTTP `413`; nothing is written |
| Pull batch, default | `100` events (`DEFAULT_PULL_BATCH`) | `crates/tape-shared-kernel/src/subscription.rs` | n/a: this is the size used when the request has no limit |
| Pull batch, maximum | `1000` events (`MAX_PULL_BATCH`) | `crates/tape-shared-kernel/src/subscription.rs` | pull with `limit` above 1000 is refused with HTTP `400`, error code `subscription_error`; the cursor does not move |
| Checkpoint / ack offset | an ack offset may not be below the current cursor and may not be above the topic's end offset | `crates/tape-shared-kernel/src/tape_journal.rs` (`ack_subscription_at`; errors `StaleCheckpoint`, `CheckpointBeyondEnd`) | the ack is refused with HTTP `409`, error code `conflict` (`TapeError` maps to `JournalError::Conflict` in `crates/tape-journal/src/application/error.rs`); the cursor does not move |
| Admission (rate) per caller | off unless one of `TAPE_ADMISSION_READ_CAPACITY`, `TAPE_ADMISSION_WRITE_CAPACITY`, `TAPE_ADMISSION_ADMIN_CAPACITY` is set; `TAPE_ADMISSION_REFILL_SECS`, `TAPE_ADMISSION_MAX_KEYS` tune it | `crates/tape/src/http/router.rs` (three classes `tape.read`, `tape.write`, `tape.admin`); see `crd-capacity-and-quotas.md` § 3g | HTTP `429` with code `rate_limited` |
| Snapshot threshold (not a client limit) | `1024` WAL frames (`DEFAULT_SNAPSHOT_THRESHOLD`) | `crates/tape-storage/src/wal.rs` | a snapshot is written and the WAL is truncated; no error |

## Not supported

Tape does not enforce these. The sources are [CRD capacity and quotas](crd-capacity-and-quotas.md) § 3g and § 3f, and the [environment and HTTP surface](environment-and-http.md).

- A per-topic quota (throughput, event size, event count). One body limit and the three admission classes apply to the whole instance.
- A per-subscription quota or rate.
- A limit on the number of topics or subscriptions.
- A limit on disk use. Retention (`max_age_seconds`, `min_offset`) bounds the retained events only when a policy is set. With no policy the journal grows until the volume is full. Then the node answers HTTP `507` with code `storage_full`; see the [disk-full runbook](../runbooks/disk-full.md).
