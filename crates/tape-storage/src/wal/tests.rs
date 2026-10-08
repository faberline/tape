use std::sync::{Arc, Mutex};

use super::io_error::{clone_io_error, flatten_io_error};
use super::*;
use tape_shared_kernel::{
    durability_errno, should_enter_storage_degraded_mode, RetentionPolicy, TapeOutcome,
};

fn append_cmd(topic: &str, n: u64, applied_at_ms: u64) -> TapeCommand {
    TapeCommand::Append {
        topic: topic.to_string(),
        key: None,
        payload: serde_json::json!({ "n": n }),
        timestamp_ms: applied_at_ms,
        applied_at_ms,
    }
}

#[test]
fn round_trip_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, journal) = WalStore::open(dir.path()).unwrap();
    let journal = Mutex::new(journal);

    let outcomes = store
        .commit(
            vec![
                append_cmd("orders", 1, 100),
                append_cmd("orders", 2, 100),
                append_cmd("orders", 3, 100),
            ],
            &journal,
        )
        .unwrap();
    assert_eq!(outcomes.len(), 3);
    let before = journal.lock().unwrap().clone();
    assert_eq!(before.end_offset("orders"), 3);
    drop(store);

    let (store2, recovered) = WalStore::open(dir.path()).unwrap();
    drop(store2);
    assert_eq!(recovered, before);
}

#[test]
fn torn_tail_recovers_every_complete_frame_and_drops_only_the_torn_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, journal) = WalStore::open(dir.path()).unwrap();
    let journal_lock = Mutex::new(journal);
    store
        .commit(
            vec![
                append_cmd("orders", 1, 100),
                append_cmd("orders", 2, 100),
                append_cmd("orders", 3, 100),
            ],
            &journal_lock,
        )
        .unwrap();
    let complete_journal = journal_lock.into_inner().unwrap();
    drop(store);

    // Simulate a crash mid-write of a fourth frame: append a few stray
    // bytes past every complete, already-synced frame -- too short to be
    // a valid frame header, so `scan_good_end` must stop exactly at the
    // boundary of the last complete frame and truncate only this torn
    // tail, not any of the three good frames above it.
    let wal_path = dir.path().join(WAL_FILE_NAME);
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .unwrap();
        file.write_all(b"\x00\x01\x02").unwrap();
        file.sync_all().unwrap();
    }

    let (store2, recovered) = WalStore::open(dir.path()).unwrap();
    drop(store2);
    // Every complete record survives; nothing beyond the good boundary
    // was fabricated.
    assert_eq!(recovered, complete_journal);
    assert_eq!(recovered.end_offset("orders"), 3);
}

#[test]
fn snapshot_and_truncate_shrinks_wal_and_reopen_still_reconstructs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, journal) = WalStore::open_with_snapshot_threshold(dir.path(), 3).unwrap();
    let journal_lock = Mutex::new(journal);

    for n in 0..6u64 {
        store
            .commit(vec![append_cmd("orders", n, 100)], &journal_lock)
            .unwrap();
    }
    let complete_journal = journal_lock.into_inner().unwrap();
    drop(store);

    let wal_path = dir.path().join(WAL_FILE_NAME);
    let wal_len_after_snapshot = std::fs::metadata(&wal_path).unwrap().len();
    // Six single-command commits with a threshold of 3 crosses the
    // threshold twice; the WAL must never grow to hold all six frames.
    assert!(wal_len_after_snapshot < 6 * 64);

    let (store2, recovered) = WalStore::open_with_snapshot_threshold(dir.path(), 3).unwrap();
    drop(store2);
    assert_eq!(recovered, complete_journal);
    assert_eq!(recovered.end_offset("orders"), 6);
}

#[test]
fn retention_pruning_replays_the_pruned_journal_not_the_pre_pruned_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, journal) = WalStore::open(dir.path()).unwrap();
    let journal_lock = Mutex::new(journal);

    let mut commands: Vec<TapeCommand> = (0..5).map(|n| append_cmd("orders", n, 100)).collect();
    commands.push(TapeCommand::RetentionPut {
        topic: "orders".to_string(),
        policy: RetentionPolicy {
            min_offset: Some(3),
            max_age_seconds: None,
            protected_consumers: Vec::new(),
        },
        now_ms: 100,
    });
    store.commit(commands, &journal_lock).unwrap();

    let pruned = journal_lock.into_inner().unwrap();
    // enforce_retention ran as a side effect of the RetentionPut command
    // (and of every prior append): only offsets >= 3 remain resident,
    // even though 5 events were appended.
    assert_eq!(pruned.replay("orders", None, None, None).len(), 2);
    assert_eq!(pruned.end_offset("orders"), 5);
    drop(store);

    let (store2, recovered) = WalStore::open(dir.path()).unwrap();
    drop(store2);
    // Replaying commands (not post-state) reproduces the SAME pruned
    // journal -- this is the property that requires logging TapeCommand
    // rather than TapeJournal state.
    assert_eq!(recovered, pruned);
    assert_eq!(recovered.replay("orders", None, None, None).len(), 2);
}

/// A sync failure after frames have already landed on disk must: (1)
/// leave the in-memory journal completely untouched; (2) poison the
/// store so the orphaned batch's seq is never reused by a later commit
/// (the actual bug this pins down: a reused seq would duplicate a frame,
/// and `FramedLogReader::read_frames` does not deduplicate by seq); and
/// (3) on reopen, replay the orphaned batch AT MOST ONCE. An
/// unacknowledged batch may or may not survive a crash right at the sync
/// boundary -- the caller already received an error, so either outcome
/// is a legitimate answer to "did it happen" -- but it must never come
/// back twice, which is the property poisoning exists to guarantee.
#[test]
fn sync_failure_poisons_the_store_and_the_orphaned_batch_replays_at_most_once() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, journal) = WalStore::open(dir.path()).unwrap();
    let journal_lock = Mutex::new(journal);
    store.inject_next_sync_failure();

    // The append loop runs normally; only the sync (injected) fails.
    let result = store.commit(vec![append_cmd("orders", 1, 100)], &journal_lock);
    assert!(result.is_err());
    // (1) journal untouched -- apply never runs before a successful sync.
    assert_eq!(journal_lock.lock().unwrap().end_offset("orders"), 0);

    // (2) poisoned: a later commit must not be allowed to reuse the same
    // starting seq the failed batch already wrote frames at.
    let retry = store.commit(vec![append_cmd("orders", 2, 100)], &journal_lock);
    assert!(retry.is_err());
    assert_eq!(journal_lock.lock().unwrap().end_offset("orders"), 0);
    drop(store);

    // (3) reopen replays whatever of the orphaned batch actually landed
    // on disk exactly once -- not zero-or-two times. `FramedLogWriter`'s
    // buffered writer flushes its already-appended-but-unsynced bytes on
    // drop, so in this test the frame lands and IS replayed; the
    // property under test is that it is never replayed twice.
    let (store2, recovered) = WalStore::open(dir.path()).unwrap();
    drop(store2);
    assert_eq!(recovered.replay("orders", None, None, None).len(), 1);
    assert_eq!(recovered.end_offset("orders"), 1);
}

#[test]
fn empty_wal_no_snapshot_opens_into_an_empty_journal() {
    let dir = tempfile::tempdir().unwrap();
    let (store, journal) = WalStore::open(dir.path()).unwrap();
    drop(store);
    assert_eq!(journal, TapeJournal::default());
}

/// Pins the reason [`DurabilityFailure`] exists at all. The obvious way to
/// recover an errno from a flattened durability failure --
/// `error.raw_os_error()` -- returns `None`, because
/// `std::io::Error::new` erases it; and the obvious way to recognize EIO
/// by kind fails too, because EIO is `Uncategorized`, not `Other`. Both
/// naive forms are asserted here so that deleting the carrier as
/// "redundant" turns this test red instead of silently disabling
/// `JournalService::apply_mutation`'s EIO branch.
#[test]
fn flatten_io_error_carries_the_errno_a_rebuilt_io_error_would_lose() {
    const EIO: i32 = 5;
    let flattened = flatten_io_error(
        anyhow::Error::from(std::io::Error::from_raw_os_error(EIO))
            .context("syncing the write-ahead log"),
    );

    assert_eq!(durability_errno(&flattened), Some(EIO));
    assert_eq!(flattened.raw_os_error(), None);
    assert_ne!(flattened.kind(), std::io::ErrorKind::Other);
    assert!(flattened
        .to_string()
        .contains("syncing the write-ahead log"));

    // ENOSPC needs no carrier -- it has a stable `ErrorKind` -- but the
    // carrier must not break it, since that is the path #2573 already
    // depends on.
    let enospc = flatten_io_error(anyhow::Error::from(std::io::Error::from(
        std::io::ErrorKind::StorageFull,
    )));
    assert_eq!(enospc.kind(), std::io::ErrorKind::StorageFull);
}

/// The commit coordinator fans one failure out to every waiter in the
/// batch, and `std::io::Error` is not `Clone`. Both the kind and the errno
/// have to survive that fan-out or the last waiter is told something
/// different from the first.
#[test]
fn clone_io_error_preserves_both_kind_and_errno_across_the_fan_out() {
    const EIO: i32 = 5;
    let original = flatten_io_error(anyhow::Error::from(std::io::Error::from_raw_os_error(EIO)));
    let cloned = clone_io_error(&original);

    assert_eq!(cloned.kind(), original.kind());
    assert_eq!(durability_errno(&cloned), Some(EIO));
    assert_eq!(cloned.to_string(), original.to_string());

    // An error that never came from an OS call reports no errno rather
    // than a misleading zero.
    let plain = std::io::Error::other("coordinator thread is gone");
    assert_eq!(durability_errno(&plain), None);
    assert_eq!(durability_errno(&clone_io_error(&plain)), None);
}

#[test]
fn migrate_legacy_journal_seeds_a_snapshot_wal_open_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let mut legacy = TapeJournal::default();
    legacy.append_at("orders", None, serde_json::json!({ "n": 1 }), 100, 100);
    legacy.append_at("orders", None, serde_json::json!({ "n": 2 }), 100, 100);
    let legacy_path = dir.path().join(LEGACY_JOURNAL_FILE_NAME);
    std::fs::write(&legacy_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let migrated = migrate_legacy_journal_file(dir.path()).unwrap();
    assert!(migrated, "a fresh dir with only journal.json must migrate");

    // journal.json is never deleted -- it is the rollback path.
    assert!(legacy_path.exists());

    let (store, recovered) = WalStore::open(dir.path()).unwrap();
    drop(store);
    assert_eq!(recovered.end_offset("orders"), 2);
    assert_eq!(recovered.replay("orders", None, None, None).len(), 2);
}

#[test]
fn migrate_legacy_journal_is_a_noop_without_a_legacy_file() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!migrate_legacy_journal_file(dir.path()).unwrap());
}

#[test]
fn migrate_legacy_journal_is_a_noop_once_a_wal_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    // Open (and immediately drop) a WalStore to establish journal.wal.
    let (store, _journal) = WalStore::open(dir.path()).unwrap();
    drop(store);
    // Even with a legacy file also present, an existing WAL wins -- do
    // not re-migrate over live WAL state.
    std::fs::write(
        dir.path().join(LEGACY_JOURNAL_FILE_NAME),
        serde_json::to_vec(&TapeJournal::default()).unwrap(),
    )
    .unwrap();
    assert!(!migrate_legacy_journal_file(dir.path()).unwrap());
}

#[tokio::test]
async fn commit_coordinator_submit_round_trips_through_the_dedicated_thread() {
    let dir = tempfile::tempdir().unwrap();
    let (store, journal) = WalStore::open(dir.path()).unwrap();
    let journal = Arc::new(Mutex::new(journal));
    let coordinator = CommitCoordinator::spawn(store, Arc::clone(&journal));

    let outcome = coordinator
        .submit(append_cmd("orders", 1, 100))
        .await
        .unwrap();
    assert!(matches!(outcome, TapeOutcome::Appended(_)));
    assert_eq!(journal.lock().unwrap().end_offset("orders"), 1);
}

#[tokio::test]
async fn commit_coordinator_batches_concurrent_submissions_in_submission_order() {
    let dir = tempfile::tempdir().unwrap();
    let (store, journal) = WalStore::open(dir.path()).unwrap();
    let journal = Arc::new(Mutex::new(journal));
    let coordinator = Arc::new(CommitCoordinator::spawn(store, Arc::clone(&journal)));

    let mut handles = Vec::new();
    for n in 0..32u64 {
        let coordinator = Arc::clone(&coordinator);
        handles.push(tokio::spawn(async move {
            coordinator
                .submit(append_cmd("orders", n, 100))
                .await
                .unwrap()
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    assert_eq!(journal.lock().unwrap().end_offset("orders"), 32);
}

/// The degraded-mode latch fires on ENOSPC *or* EIO (WI #3052 R6). This pins
/// the EIO half of the predicate, which is the half with no stable
/// `ErrorKind` to ride on and which was silently unreachable before
/// [`DurabilityFailure`](tape_shared_kernel::DurabilityFailure) carried
/// the errno explicitly.
#[test]
fn eio_is_recognized_through_the_durability_error_the_wal_path_actually_produces() {
    const EIO: i32 = 5;
    let from_wal_path =
        flatten_io_error(anyhow::Error::from(std::io::Error::from_raw_os_error(EIO)));
    assert!(should_enter_storage_degraded_mode(&from_wal_path));

    // A plain failure must NOT latch degraded mode: it fails one request
    // closed and leaves the node writable.
    assert!(!should_enter_storage_degraded_mode(&std::io::Error::other(
        "transient"
    )));
    assert!(!should_enter_storage_degraded_mode(&std::io::Error::from(
        std::io::ErrorKind::PermissionDenied
    )));
}
